use std::cell::RefCell;

use smithay::{
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy, Space, Window,
        WindowSurfaceType, find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
        space::SpaceElement,
    },
    input::{Seat, pointer::Focus},
    output::Output,
    reexports::{
        wayland_protocols::xdg::{decoration as xdg_decoration, shell::server::xdg_toplevel},
        wayland_server::{
            Resource,
            protocol::{wl_output, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Logical, Point, Rectangle, Serial},
    wayland::{
        compositor::{self, with_states, SurfaceAttributes},
        seat::WaylandFocus,
        shell::xdg::{
            Configure, PopupSurface, PositionerState, ToplevelCachedState, ToplevelSurface, XdgShellHandler,
            XdgShellState,
        },
    },
};
use tracing::{trace, warn};

use crate::{
    focus::KeyboardFocusTarget,
    shell::{TouchMoveSurfaceGrab, TouchResizeSurfaceGrab},
    state::{AnvilState, Backend},
};

use super::{
    FullscreenSurface, PointerMoveSurfaceGrab, PointerResizeSurfaceGrab, ResizeData, ResizeEdge, ResizeState,
    SurfaceData, WindowElement, fullscreen_output_geometry, place_new_window,
};

impl<BackendData: Backend> XdgShellHandler for AnvilState<BackendData> {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = WindowElement(Window::new_wayland_window(surface.clone()));
        let needs_center = place_new_window(&mut self.space, self.pointer.current_location(), &window, true);

        // Apply window config (including window-rule overrides)
        window.apply_config(&self.config);

        // Mark that window needs centering (hide until first commit positions it correctly)
        if needs_center {
            window.decoration_state().needs_center = true;
        }

        // Start open animation
        if self.config.animations.enable && self.config.animations.window_open.enable {
            use crate::animation::Animation;
            let open_config = &self.config.animations.window_open;
            let anim = Animation::ease(
                0.0,  // from: progress 0 (invisible)
                1.0,  // to: progress 1 (fully visible)
                open_config.duration_ms,
                open_config.curve.to_curve(),
            );
            window.decoration_state().open_animation = Some(anim);
        }

        // Note: prefer_no_csd can't be checked per-rule here since app_id/title
        // are not yet available. The global config is used at creation time.
        if self.config.window.prefer_no_csd {
            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::TiledTop);
                state.states.set(xdg_toplevel::State::TiledBottom);
                state.states.set(xdg_toplevel::State::TiledLeft);
                state.states.set(xdg_toplevel::State::TiledRight);
            });
        }

        // Add pre-commit hook to detect surface unmap (null buffer commit).
        // When a window commits a null buffer (BufferAssignment::Removed), we capture
        // its contents as a texture snapshot *before* the buffer is removed, so the
        // close animation will have valid content. The snapshot is stored in
        // WindowState.pending_close_snapshot and used later in toplevel_destroyed.
        compositor::add_pre_commit_hook::<Self, _>(surface.wl_surface(), |state, _dh, surface| {
            use smithay::wayland::compositor::BufferAssignment;

            // Check if this commit is removing the buffer (surface unmap)
            let got_unmapped = compositor::with_states(surface, |states| {
                let mut guard = states.cached_state.get::<SurfaceAttributes>();
                matches!(guard.pending().buffer.as_ref(), Some(BufferAssignment::Removed))
            });

            if got_unmapped {
                // Find the window in space
                let Some(window) = state
                    .space
                    .elements()
                    .find(|w| w.wl_surface().as_deref() == Some(surface))
                    .cloned()
                else {
                    return;
                };

                // Only capture if we don't already have a snapshot (avoid overwriting)
                if window.decoration_state().pending_close_snapshot.is_some() {
                    return;
                }

                // Check if close animation is enabled
                if !state.config.animations.enable || !state.config.animations.window_close.enable {
                    return;
                }

                // Capture the snapshot using the primary renderer
                let output = state.space.outputs_for_element(&window).first().cloned();
                let config = state.config.clone();
                let snapshot = state.backend_data.with_primary_renderer(|renderer| {
                    crate::state::AnvilState::<BackendData>::capture_close_snapshot(
                        &state.space,
                        &config,
                        &window,
                        renderer,
                        output.as_ref(),
                    )
                });

                // with_primary_renderer returns Option<Option<PendingCloseSnapshot>>
                // - outer None: no renderer available
                // - inner None: capture failed
                // - inner Some: snapshot captured successfully
                if let Some(Some(snapshot)) = snapshot {
                    tracing::debug!("pre-commit: captured close animation snapshot for unmap");
                    window.decoration_state().pending_close_snapshot = Some(snapshot);
                }
            }
        });

        compositor::add_post_commit_hook(surface.wl_surface(), |state: &mut Self, _, surface| {
            handle_toplevel_commit(&mut state.space, surface);
            // Re-apply window rules in case app_id/title changed
            if let Some(window) = state
                .space
                .elements()
                .find(|w| w.wl_surface().as_deref() == Some(surface))
            {
                window.apply_config(&state.config);
            }
        });
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        // Do not send a configure here, the initial configure
        // of a xdg_surface has to be sent during the commit if
        // the surface is not already configured

        self.unconstrain_popup(&surface);

        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            warn!("Failed to track popup: {}", err);
        }
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            let geometry = positioner.get_geometry();
            state.geometry = geometry;
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat: Seat<AnvilState<BackendData>> = Seat::from_resource(&seat).unwrap();
        self.move_request_xdg(&surface, &seat, serial)
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat: Seat<AnvilState<BackendData>> = Seat::from_resource(&seat).unwrap();

        let resize_modifier = self.config.window.resize_modifier.clone();
        if !resize_modifier.is_empty() {
            let keyboard = seat.get_keyboard().unwrap();
            let modifiers = keyboard.modifier_state();
            let modifier_pressed = match resize_modifier.as_str() {
                "Ctrl" => modifiers.ctrl,
                "Alt" => modifiers.alt,
                "Super" | "Logo" => modifiers.logo,
                "Shift" => modifiers.shift,
                _ => false,
            };
            if !modifier_pressed {
                return;
            }
        }

        if let Some(touch) = seat.get_touch() {
            if touch.has_grab(serial) {
                let start_data = touch.grab_start_data().unwrap();
                tracing::info!(?start_data);

                // If the client disconnects after requesting a move
                // we can just ignore the request
                let Some(window) = self.window_for_surface(surface.wl_surface()) else {
                    tracing::info!("no window");
                    return;
                };

                // If the focus was for a different surface, ignore the request.
                if start_data.focus.is_none()
                    || !start_data
                        .focus
                        .as_ref()
                        .unwrap()
                        .0
                        .same_client_as(&surface.wl_surface().id())
                {
                    tracing::info!("different surface");
                    return;
                }
                let geometry = window.geometry();
                let loc = self.space.element_location(&window).unwrap();
                let (initial_window_location, initial_window_size) = (loc, geometry.size);

                with_states(surface.wl_surface(), move |states| {
                    states
                        .data_map
                        .get::<RefCell<SurfaceData>>()
                        .unwrap()
                        .borrow_mut()
                        .resize_state = ResizeState::Resizing(ResizeData {
                        edges: edges.into(),
                        initial_window_location,
                        initial_window_size,
                    });
                });

                let grab = TouchResizeSurfaceGrab {
                    start_data,
                    window,
                    edges: edges.into(),
                    initial_window_location,
                    initial_window_size,
                    last_window_size: initial_window_size,
                };

                touch.set_grab(self, grab, serial);
                return;
            }
        }

        let pointer = seat.get_pointer().unwrap();

        // Check that this surface has a click grab.
        if !pointer.has_grab(serial) {
            return;
        }

        let start_data = pointer.grab_start_data().unwrap();

        let window = self.window_for_surface(surface.wl_surface()).unwrap();

        // If the focus was for a different surface, ignore the request.
        if start_data.focus.is_none()
            || !start_data
                .focus
                .as_ref()
                .unwrap()
                .0
                .same_client_as(&surface.wl_surface().id())
        {
            return;
        }

        let geometry = window.geometry();
        let loc = self.space.element_location(&window).unwrap();
        let (initial_window_location, initial_window_size) = (loc, geometry.size);

        with_states(surface.wl_surface(), move |states| {
            states
                .data_map
                .get::<RefCell<SurfaceData>>()
                .unwrap()
                .borrow_mut()
                .resize_state = ResizeState::Resizing(ResizeData {
                edges: edges.into(),
                initial_window_location,
                initial_window_size,
            });
        });

        let grab = PointerResizeSurfaceGrab {
            start_data,
            window,
            edges: edges.into(),
            initial_window_location,
            initial_window_size,
            last_window_size: initial_window_size,
        };

        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn ack_configure(&mut self, surface: WlSurface, configure: Configure) {
        if let Configure::Toplevel(configure) = configure {
            if let Some(serial) = with_states(&surface, |states| {
                if let Some(data) = states.data_map.get::<RefCell<SurfaceData>>() {
                    if let ResizeState::WaitingForFinalAck(_, serial) = data.borrow().resize_state {
                        return Some(serial);
                    }
                }

                None
            }) {
                // When the resize grab is released the surface
                // resize state will be set to WaitingForFinalAck
                // and the client will receive a configure request
                // without the resize state to inform the client
                // resizing has finished. Here we will wait for
                // the client to acknowledge the end of the
                // resizing. To check if the surface was resizing
                // before sending the configure we need to use
                // the current state as the received acknowledge
                // will no longer have the resize state set
                let is_resizing = with_states(&surface, |states| {
                    states
                        .cached_state
                        .get::<ToplevelCachedState>()
                        .current()
                        .last_acked
                        .as_ref()
                        .is_some_and(|c| c.state.states.contains(xdg_toplevel::State::Resizing))
                });

                if configure.serial >= serial && is_resizing {
                    with_states(&surface, |states| {
                        let mut data = states
                            .data_map
                            .get::<RefCell<SurfaceData>>()
                            .unwrap()
                            .borrow_mut();
                        if let ResizeState::WaitingForFinalAck(resize_data, _) = data.resize_state {
                            data.resize_state = ResizeState::WaitingForCommit(resize_data);
                        } else {
                            unreachable!()
                        }
                    });
                }
            }

            let window = self
                .space
                .elements()
                .find(|element| element.wl_surface().as_deref() == Some(&surface));
            if let Some(window) = window {
                use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
                let is_ssd = configure
                    .state
                    .decoration_mode
                    .map(|mode| mode == Mode::ServerSide)
                    .unwrap_or(false);
                window.set_ssd(is_ssd);
            }
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, mut wl_output: Option<wl_output::WlOutput>) {
        // NOTE: This is only one part of the solution. We can set the
        // location and configure size here, but the surface should be rendered fullscreen
        // independently from its buffer size
        let wl_surface = surface.wl_surface();

        let output_geometry = fullscreen_output_geometry(wl_surface, wl_output.as_ref(), &mut self.space);

        if let Some(geometry) = output_geometry {
            let output = wl_output
                .as_ref()
                .and_then(Output::from_resource)
                .unwrap_or_else(|| self.space.outputs().next().unwrap().clone());
            let client = match self.display_handle.get_client(wl_surface.id()) {
                Ok(client) => client,
                Err(_) => return,
            };
            for output in output.client_outputs(&client) {
                wl_output = Some(output);
            }
            let window = self
                .space
                .elements()
                .find(|window| window.wl_surface().map(|s| &*s == wl_surface).unwrap_or(false))
                .unwrap();

            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Fullscreen);
                state.size = Some(geometry.size);
                state.fullscreen_output = wl_output;
            });
            output.user_data().insert_if_missing(FullscreenSurface::default);
            output
                .user_data()
                .get::<FullscreenSurface>()
                .unwrap()
                .set(window.clone());
            trace!("Fullscreening: {:?}", window);
        }

        // The protocol demands us to always reply with a configure,
        // regardless of we fulfilled the request or not
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        } else {
            // Will be sent during initial configure
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let ret = surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Fullscreen);
            state.size = None;
            state.fullscreen_output.take()
        });
        if let Some(output) = ret {
            let output = Output::from_resource(&output).unwrap();
            if let Some(fullscreen) = output.user_data().get::<FullscreenSurface>() {
                trace!("Unfullscreening: {:?}", fullscreen.get());
                fullscreen.clear();
                self.backend_data.reset_buffers(&output);
            }
        }

        // The protocol demands us to always reply with a configure,
        // regardless of we fulfilled the request or not
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        } else {
            // Will be sent during initial configure
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        // NOTE: This should use layer-shell when it is implemented to
        // get the correct maximum size
        let window = self.window_for_surface(surface.wl_surface()).unwrap();
        let outputs_for_window = self.space.outputs_for_element(&window);
        let output = outputs_for_window
            .first()
            // The window hasn't been mapped yet, use the primary output instead
            .or_else(|| self.space.outputs().next())
            // Assumes that at least one output exists
            .expect("No outputs found");
        let geometry = self.space.output_geometry(output).unwrap();

        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Maximized);
            state.size = Some(geometry.size);
        });
        self.space.map_element(window, geometry.loc, true);

        // The protocol demands us to always reply with a configure,
        // regardless of we fulfilled the request or not
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        } else {
            // Will be sent during initial configure
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Maximized);
            state.size = None;
        });

        // The protocol demands us to always reply with a configure,
        // regardless of we fulfilled the request or not
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        } else {
            // Will be sent during initial configure
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat: Seat<AnvilState<BackendData>> = Seat::from_resource(&seat).unwrap();
        let kind = PopupKind::Xdg(surface);
        if let Some(root) = find_popup_root_surface(&kind).ok().and_then(|root| {
            self.space
                .elements()
                .find(|w| w.wl_surface().map(|s| *s == root).unwrap_or(false))
                .cloned()
                .map(KeyboardFocusTarget::from)
                .or_else(|| {
                    self.space
                        .outputs()
                        .find_map(|o| {
                            let map = layer_map_for_output(o);
                            map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL).cloned()
                        })
                        .map(KeyboardFocusTarget::LayerSurface)
                })
        }) {
            let ret = self.popups.grab_popup(root, kind, &seat, serial);

            if let Ok(mut grab) = ret {
                if let Some(keyboard) = seat.get_keyboard() {
                    if keyboard.is_grabbed()
                        && !(keyboard.has_grab(serial)
                            || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
                    {
                        grab.ungrab(PopupUngrabStrategy::All);
                        return;
                    }
                    keyboard.set_focus(self, grab.current_grab(), serial);
                    keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
                }
                if let Some(pointer) = seat.get_pointer() {
                    if pointer.is_grabbed()
                        && !(pointer.has_grab(serial)
                            || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
                    {
                        grab.ungrab(PopupUngrabStrategy::All);
                        return;
                    }
                    pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
                }
            }
        }
    }

    fn toplevel_destroyed(&mut self, toplevel: ToplevelSurface) {
        let _wl_surface = toplevel.wl_surface().clone();
        let window = match self.space.elements().find(|w| w.0.toplevel() == Some(&toplevel)) {
            Some(w) => w.clone(),
            None => {
                trace!("toplevel_destroyed: window not found in space");
                return;
            }
        };

        // Find the output this window is on
        let output = match self.space.outputs_for_element(&window).first().cloned() {
            Some(o) => o,
            None => {
                trace!("toplevel_destroyed: no output for window");
                return;
            }
        };

        // Try to use the pre-captured snapshot (captured in pre-commit hook
        // when BufferAssignment::Removed was detected, i.e. when the surface
        // unmapped). This snapshot has the window's last valid content.
        let snapshot = window.decoration_state().pending_close_snapshot.take();

        let config = self.config.clone();
        let started = if let Some(snapshot) = snapshot {
            tracing::info!("toplevel_destroyed: using pre-captured snapshot for close animation");
            AnvilState::<BackendData>::start_close_animation_from_snapshot(
                &mut self.closing_windows,
                &config,
                snapshot,
            )
        } else {
            // No pre-captured snapshot available. Try to capture one now as a fallback.
            // This may produce an empty snapshot if the surface buffer is already gone,
            // but it's worth trying for cases where the client doesn't unmap before destroying.
            tracing::debug!("toplevel_destroyed: no pre-captured snapshot, trying fallback capture");
            self.backend_data.with_primary_renderer(|renderer| {
                AnvilState::<BackendData>::start_close_animation_inner(
                    &mut self.closing_windows,
                    &self.space,
                    &config,
                    &window,
                    renderer,
                    &output,
                )
            }).unwrap_or(false)
        };

        if started {
            tracing::info!("toplevel_destroyed: close animation started");
            // Remove the window from the space — the ClosingWindow
            // will handle rendering from now on.
            self.space.unmap_elem(&window);
            // Queue redraw to show the animation
            self.backend_data.queue_redraw(&output);
        } else {
            tracing::debug!("toplevel_destroyed: close animation not started (disabled or failed)");
            // Animation not started — just remove the window
            self.space.unmap_elem(&window);
        }
    }
}

impl<BackendData: Backend> AnvilState<BackendData> {
    pub fn move_request_xdg(&mut self, surface: &ToplevelSurface, seat: &Seat<Self>, serial: Serial) {
        if let Some(touch) = seat.get_touch() {
            if touch.has_grab(serial) {
                let start_data = touch.grab_start_data().unwrap();

                // If the client disconnects after requesting a move
                // we can just ignore the request
                let Some(window) = self.window_for_surface(surface.wl_surface()) else {
                    return;
                };

                // If the focus was for a different surface, ignore the request.
                if start_data.focus.is_none()
                    || !start_data
                        .focus
                        .as_ref()
                        .unwrap()
                        .0
                        .same_client_as(&surface.wl_surface().id())
                {
                    return;
                }

                let mut initial_window_location = self.space.element_location(&window).unwrap();

                // If surface is maximized then unmaximize it
                let changed = surface.with_pending_state(|state| {
                    if state.states.unset(xdg_toplevel::State::Maximized) {
                        state.size = None;
                        true
                    } else {
                        false
                    }
                });
                if changed {
                    surface.send_configure();

                    // NOTE: In real compositor mouse location should be mapped to a new window size
                    // For example, you could:
                    // 1) transform mouse pointer position from compositor space to window space (location relative)
                    // 2) divide the x coordinate by width of the window to get the percentage
                    //   - 0.0 would be on the far left of the window
                    //   - 0.5 would be in middle of the window
                    //   - 1.0 would be on the far right of the window
                    // 3) multiply the percentage by new window width
                    // 4) by doing that, drag will look a lot more natural
                    //
                    // but for anvil needs setting location to pointer location is fine
                    initial_window_location = start_data.location.to_i32_round();
                }

                let grab = TouchMoveSurfaceGrab {
                    start_data,
                    window,
                    initial_window_location,
                };

                touch.set_grab(self, grab, serial);
                return;
            }
        }

        let pointer = seat.get_pointer().unwrap();

        // Check that this surface has a click grab.
        if !pointer.has_grab(serial) {
            return;
        }

        let start_data = pointer.grab_start_data().unwrap();

        // If the client disconnects after requesting a move
        // we can just ignore the request
        let Some(window) = self.window_for_surface(surface.wl_surface()) else {
            return;
        };

        // If the focus was for a different surface, ignore the request.
        if start_data.focus.is_none()
            || !start_data
                .focus
                .as_ref()
                .unwrap()
                .0
                .same_client_as(&surface.wl_surface().id())
        {
            return;
        }

        let mut initial_window_location = self.space.element_location(&window).unwrap();

        // If surface is maximized then unmaximize it
        let changed = surface.with_pending_state(|state| {
            if state.states.unset(xdg_toplevel::State::Maximized) {
                state.size = None;
                true
            } else {
                false
            }
        });
        if changed {
            surface.send_configure();

            // NOTE: In real compositor mouse location should be mapped to a new window size
            // For example, you could:
            // 1) transform mouse pointer position from compositor space to window space (location relative)
            // 2) divide the x coordinate by width of the window to get the percentage
            //   - 0.0 would be on the far left of the window
            //   - 0.5 would be in middle of the window
            //   - 1.0 would be on the far right of the window
            // 3) multiply the percentage by new window width
            // 4) by doing that, drag will look a lot more natural
            //
            // but for anvil needs setting location to pointer location is fine
            let pos = pointer.current_location();
            initial_window_location = (pos.x as i32, pos.y as i32).into();
        }

        let grab = PointerMoveSurfaceGrab {
            start_data,
            window,
            initial_window_location,
        };

        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self.window_for_surface(&root) else {
            return;
        };

        let mut outputs_for_window = self.space.outputs_for_element(&window);
        if outputs_for_window.is_empty() {
            return;
        }

        // Get a union of all outputs' geometries.
        let mut outputs_geo = self
            .space
            .output_geometry(&outputs_for_window.pop().unwrap())
            .unwrap();
        for output in outputs_for_window {
            outputs_geo = outputs_geo.merge(self.space.output_geometry(&output).unwrap());
        }

        let window_geo = self.space.element_geometry(&window).unwrap();

        // The target geometry for the positioner should be relative to its parent's geometry, so
        // we will compute that here.
        let mut target = outputs_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

/// Should be called on `WlSurface::commit` of xdg toplevel
fn handle_toplevel_commit(space: &mut Space<WindowElement>, surface: &WlSurface) -> Option<()> {
    let window = space
        .elements()
        .find(|w| w.wl_surface().as_deref() == Some(surface))
        .cloned()?;

    let mut window_loc = space.element_location(&window)?;
    let geometry = window.geometry();

    let needs_center = with_states(window.wl_surface().as_deref()?, |states| {
        let Some(data) = states.data_map.get::<RefCell<SurfaceData>>() else {
            return false;
        };
        let mut data = data.borrow_mut();
        let needs = data.needs_center && geometry.size.w > 0 && geometry.size.h > 0;
        if needs {
            data.needs_center = false;
        }
        needs
    });

    if needs_center {
        let outputs_for_window = space.outputs_for_element(&window);
        let output = outputs_for_window
            .first()
            .or_else(|| space.outputs().next())
            .cloned();
        let output_geometry = output
            .and_then(|o| {
                let geo = space.output_geometry(&o)?;
                let map = layer_map_for_output(&o);
                let zone = map.non_exclusive_zone();
                Some(Rectangle::new(geo.loc + zone.loc, zone.size))
            });

        if let Some(output_geometry) = output_geometry {
            let x = output_geometry.loc.x + (output_geometry.size.w - geometry.size.w) / 2 - geometry.loc.x;
            let y = output_geometry.loc.y + (output_geometry.size.h - geometry.size.h) / 2 - geometry.loc.y;
            space.relocate_element(&window, (x, y));

            // Window is now properly centered — safe to render
            let mut deco = window.decoration_state();
            deco.needs_center = false;
            // Restart open animation so it plays from the beginning
            // (it was running in the background while the window was hidden)
            if let Some(ref anim) = deco.open_animation {
                deco.open_animation = Some(anim.restarted(0.0, 1.0, 0.0));
            }

            return Some(());
        }
    }

    let new_loc: Point<Option<i32>, Logical> = with_states(window.wl_surface().as_deref()?, |states| {
        let data = states.data_map.get::<RefCell<SurfaceData>>()?.borrow_mut();

        if let ResizeState::Resizing(resize_data) = data.resize_state {
            let edges = resize_data.edges;
            let loc = resize_data.initial_window_location;
            let size = resize_data.initial_window_size;

            edges.intersects(ResizeEdge::TOP_LEFT).then(|| {
                let new_x = edges
                    .intersects(ResizeEdge::LEFT)
                    .then_some(loc.x + (size.w - geometry.size.w));

                let new_y = edges
                    .intersects(ResizeEdge::TOP)
                    .then_some(loc.y + (size.h - geometry.size.h));

                (new_x, new_y).into()
            })
        } else {
            None
        }
    })?;

    if let Some(new_x) = new_loc.x {
        window_loc.x = new_x;
    }
    if let Some(new_y) = new_loc.y {
        window_loc.y = new_y;
    }

    if new_loc.x.is_some() || new_loc.y.is_some() {
        space.relocate_element(&window, window_loc);
    }

    Some(())
}