use std::{cell::RefCell, convert::TryInto, process::Command, sync::atomic::Ordering};

use crate::config::{BindConfig, LayoutType};
use crate::shell::{
    PointerLayoutMoveGrab, PointerMoveSurfaceGrab, PointerResizeSurfaceGrab, PointerWidthResizeGrab,
    ResizeData, ResizeEdge, ResizeState, SurfaceData, WindowElement,
};
use crate::{AnvilState, focus::PointerFocusTarget, shell::FullscreenSurface};

#[cfg(feature = "udev")]
use crate::udev::UdevData;
#[cfg(feature = "udev")]
use smithay::backend::renderer::DebugFlags;

use smithay::{
    backend::input::{
        self, Axis, AxisSource, Device, DeviceCapability, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, TouchEvent,
    },
    desktop::{WindowSurfaceType, layer_map_for_output, space::SpaceElement},
    input::{
        keyboard::{FilterResult, Keysym, ModifiersState, keysyms as xkb},
        pointer::{AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, MotionEvent},
        touch::{DownEvent, UpEvent},
    },
    output::Scale,
    reexports::{
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1,
        wayland_server::protocol::wl_pointer,
    },
    utils::{Logical, Point, SERIAL_COUNTER as SCOUNTER, Serial, Transform},
    wayland::{
        input_method::InputMethodSeat,
        keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat,
        shell::wlr_layer::{KeyboardInteractivity, Layer as WlrLayer},
        tablet_manager::{TabletDescriptor, TabletSeatTrait},
    },
};

use smithay::backend::input::AbsolutePositionEvent;

#[cfg(any(feature = "winit", feature = "x11"))]
use smithay::output::Output;
use tracing::{debug, error, info, warn};

use crate::state::Backend;
#[cfg(feature = "udev")]
use smithay::{
    backend::{
        input::{
            GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent as _,
            GestureSwipeUpdateEvent as _, PointerMotionEvent, ProximityState,
            TabletToolButtonEvent, TabletToolEvent, TabletToolProximityEvent, TabletToolTipEvent,
            TabletToolTipState,
        },
        session::Session,
    },
    input::pointer::{
        GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
        GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
        GestureSwipeUpdateEvent, RelativeMotionEvent,
    },
    reexports::wayland_server::DisplayHandle,
    wayland::{
        pointer_constraints::{PointerConstraint, with_pointer_constraint},
        seat::WaylandFocus,
    },
};

impl<BackendData: Backend> AnvilState<BackendData> {
    // Allow in this method because of existing usage
    #[allow(clippy::uninlined_format_args)]
    fn process_common_key_action(&mut self, action: KeyAction) {
        match action {
            KeyAction::None => (),

            KeyAction::Quit => {
                info!("Quitting.");
                self.running.store(false, Ordering::SeqCst);
            }

            KeyAction::CloseWindow => {
                if let Some(keyboard) = self.seat.get_keyboard() {
                    if let Some(crate::focus::KeyboardFocusTarget::Window(window)) =
                        keyboard.current_focus()
                    {
                        // Queue close animation (snapshot captured during next render).
                        // send_close() is deferred until after the snapshot is captured
                        // to ensure the window texture is still available.
                        self.queue_close_animation(&WindowElement(window.clone()));
                    }
                }
            }

            KeyAction::ToggleFloating => {
                if let Some(keyboard) = self.seat.get_keyboard() {
                    if let Some(crate::focus::KeyboardFocusTarget::Window(window)) =
                        keyboard.current_focus()
                    {
                        self.toggle_window_floating(&WindowElement(window.clone()));
                    }
                }
            }

            KeyAction::Run(cmd) => {
                info!(cmd, "Starting program");

                if let Err(e) = Command::new(&cmd)
                    .envs(
                        self.socket_name
                            .clone()
                            .map(|v| ("WAYLAND_DISPLAY", v))
                            .into_iter()
                            .chain(
                                #[cfg(feature = "xwayland")]
                                self.xdisplay.map(|v| ("DISPLAY", format!(":{v}"))),
                                #[cfg(not(feature = "xwayland"))]
                                None,
                            ),
                    )
                    .spawn()
                {
                    error!(cmd, err = %e, "Failed to start program");
                }
            }

            KeyAction::TogglePreview => {
                self.show_window_preview = !self.show_window_preview;
            }

            KeyAction::ToggleDecorations => {
                for element in self.space.elements() {
                    #[allow(irrefutable_let_patterns)]
                    if let Some(toplevel) = element.0.toplevel() {
                        let mode_changed = toplevel.with_pending_state(|state| {
                            if let Some(current_mode) = state.decoration_mode {
                                let new_mode = if current_mode
                                    == zxdg_toplevel_decoration_v1::Mode::ClientSide
                                {
                                    zxdg_toplevel_decoration_v1::Mode::ServerSide
                                } else {
                                    zxdg_toplevel_decoration_v1::Mode::ClientSide
                                };
                                state.decoration_mode = Some(new_mode);
                                true
                            } else {
                                false
                            }
                        });

                        if mode_changed && toplevel.is_initial_configure_sent() {
                            toplevel.send_pending_configure();
                        }
                    }
                }
            }

            KeyAction::Screenshot => {
                info!("Screenshot requested");
                self.pending_screenshot = true;
            }

            KeyAction::Callback(idx) => {
                if let Some(ref lua_config) = self.lua_config {
                    if let Err(e) = lua_config.invoke_callback(idx) {
                        warn!("Lua callback {} failed: {}", idx, e);
                    }
                } else {
                    warn!("Lua callback {} triggered but no LuaConfig available", idx);
                }
            }

            KeyAction::FocusNext => self.focus_cycle(1),

            KeyAction::FocusPrev => self.focus_cycle(-1),

            KeyAction::WorkspaceNext | KeyAction::WorkspacePrev => {
                let Some(output) = self.focused_output() else {
                    return;
                };
                let current = self.active_workspace(&output);
                let target = if matches!(action, KeyAction::WorkspaceNext) {
                    current.saturating_add(1)
                } else {
                    current.saturating_sub(1)
                };
                self.switch_workspace(&output, target);
            }

            KeyAction::Workspace(n) => {
                if let Some(output) = self.focused_output() {
                    self.switch_workspace(&output, n as u32);
                }
            }

            KeyAction::ResizeWidthUp => self.resize_width(40.0),

            KeyAction::ResizeWidthDown => self.resize_width(-40.0),

            KeyAction::ToggleFullscreen => self.toggle_fullscreen(),

            KeyAction::ToggleMaximize => self.toggle_maximize(),

            _ => unreachable!(
                "Common key action handler encountered backend specific action {:?}",
                action
            ),
        }
    }

    fn keyboard_key_to_action<B: InputBackend>(&mut self, evt: B::KeyboardKeyEvent) -> KeyAction {
        let keycode = evt.key_code();
        let state = evt.state();
        let pressed = state == KeyState::Pressed;
        debug!(?keycode, ?state, "key");
        let serial = SCOUNTER.next_serial();
        let time = Event::time_msec(&evt);
        let keyboard = self.seat.get_keyboard().unwrap();

        for layer in self.layer_shell_state.layer_surfaces().rev() {
            let exclusive = layer.with_cached_state(|data| {
                data.keyboard_interactivity == KeyboardInteractivity::Exclusive
                    && (data.layer == WlrLayer::Top || data.layer == WlrLayer::Overlay)
            });
            if exclusive {
                let surface = self.space.outputs().find_map(|o| {
                    let map = layer_map_for_output(o);
                    map.layers().find(|l| l.layer_surface() == &layer).cloned()
                });
                if let Some(surface) = surface {
                    keyboard.set_focus(self, Some(surface.into()), serial);
                    keyboard.input::<(), _>(self, keycode, state, serial, time, |_, _, _| {
                        FilterResult::Forward
                    });
                    return KeyAction::None;
                };
            }
        }

        let inhibited = self
            .space
            .element_under(self.pointer.current_location())
            .and_then(|(window, _)| {
                let surface = window.wl_surface()?;
                self.seat.keyboard_shortcuts_inhibitor_for_surface(&surface)
            })
            .map(|inhibitor| inhibitor.is_active())
            .unwrap_or(false);

        let action = keyboard
            .input(
                self,
                keycode,
                state,
                serial,
                time,
                |this, modifiers, handle| {
                    let modified = handle.modified_sym();
                    let raw = handle.raw_latin_sym_or_raw_current_sym();

                    debug!(
                        ?state,
                        mods = ?modifiers,
                        keysym = ::xkbcommon::xkb::keysym_get_name(modified),
                        "keysym"
                    );

                    if !pressed && !this.suppressed_keys.contains(&keycode) {
                        return FilterResult::Forward;
                    }

                    if pressed {
                        if !inhibited {
                            let action = process_keyboard_shortcut(
                                &this.config.binds,
                                *modifiers,
                                modified,
                                raw,
                            );

                            if action.is_some() {
                                this.suppressed_keys.insert(keycode);
                            }

                            action
                                .map(FilterResult::Intercept)
                                .unwrap_or(FilterResult::Forward)
                        } else {
                            FilterResult::Forward
                        }
                    } else {
                        this.suppressed_keys.remove(&keycode);
                        FilterResult::Intercept(KeyAction::None)
                    }
                },
            )
            .unwrap_or(KeyAction::None);

        self.update_cursor_for_no_csd();
        action
    }

    fn on_pointer_button<B: InputBackend>(&mut self, evt: B::PointerButtonEvent) {
        let serial = SCOUNTER.next_serial();
        let button = evt.button_code();

        let state = wl_pointer::ButtonState::from(evt.state());

        if wl_pointer::ButtonState::Pressed == state {
            self.update_keyboard_focus(self.pointer.current_location(), serial);
        };

        if self.config.window.prefer_no_csd
            && wl_pointer::ButtonState::Pressed == state
            && button == 0x110
        {
            let resize_modifier = &self.config.window.resize_modifier;
            if !resize_modifier.is_empty() {
                let keyboard = self.seat.get_keyboard().unwrap();
                let modifiers = keyboard.modifier_state();
                let modifier_pressed = match resize_modifier.as_str() {
                    "Ctrl" => modifiers.ctrl,
                    "Alt" => modifiers.alt,
                    "Super" | "Logo" => modifiers.logo,
                    "Shift" => modifiers.shift,
                    _ => false,
                };

                if modifier_pressed {
                    let location = self.pointer.current_location();
                    if let Some((window, window_loc)) = self
                        .space
                        .element_under(location)
                        .map(|(w, p)| (w.clone(), p))
                    {
                        let geometry = window.geometry();
                        let window_size = geometry.size;

                        let rel_x = location.x - window_loc.x as f64;
                        let rel_y = location.y - window_loc.y as f64;

                        let edges = detect_resize_edges(rel_x, rel_y, window_size.w, window_size.h);

                        let layout_active =
                            self.config.layout.layout != LayoutType::Floating;

                        if layout_active {
                            // Tiling layout active: the modifier+mouse gestures
                            // are limited to adjusting the window's column width
                            // (left/right edges, the other windows move to make
                            // room) and reordering the window within the layout
                            // (anywhere else on the window).
                            let pointer = self.pointer.clone();
                            let start_data = smithay::input::pointer::GrabStartData {
                                focus: None,
                                button: 0x110,
                                location,
                            };

                            if edges.intersects(ResizeEdge::LEFT | ResizeEdge::RIGHT) {
                                let initial_width = window
                                    .decoration_state()
                                    .layout
                                    .width_override
                                    .unwrap_or(window_size.w as f64);
                                let grab = PointerWidthResizeGrab {
                                    start_data,
                                    window: window.clone(),
                                    initial_width,
                                };
                                use smithay::input::pointer::Focus;
                                pointer.set_grab(self, grab, serial, Focus::Clear);
                                pointer.frame(self);
                                return;
                            } else {
                                // Detach the window from the layout while dragging
                                // so it can follow the cursor; on release it is
                                // re-inserted at the slot under its final position.
                                {
                                    let mut st = window.decoration_state();
                                    let lws = &mut st.layout;
                                    lws.is_floating = true;
                                    lws.move_anim = None;
                                    lws.target = None;
                                }
                                let grab = PointerLayoutMoveGrab {
                                    start_data,
                                    window: window.clone(),
                                    initial_window_location: window_loc,
                                };
                                use smithay::input::pointer::Focus;
                                pointer.set_grab(self, grab, serial, Focus::Clear);
                                pointer.frame(self);
                                return;
                            }
                        }

                        if !edges.is_empty() {
                            let initial_window_location = window_loc;
                            let initial_window_size = window_size;

                            if let Some(surface) = window.wl_surface() {
                                smithay::wayland::compositor::with_states(
                                    &surface,
                                    move |states| {
                                        states
                                            .data_map
                                            .get::<RefCell<SurfaceData>>()
                                            .unwrap()
                                            .borrow_mut()
                                            .resize_state = ResizeState::Resizing(ResizeData {
                                            edges,
                                            initial_window_location,
                                            initial_window_size,
                                        });
                                    },
                                );
                            }

                            let pointer = self.pointer.clone();
                            let start_data = smithay::input::pointer::GrabStartData {
                                focus: None,
                                button: 0x110,
                                location,
                            };

                            let grab = PointerResizeSurfaceGrab {
                                start_data,
                                window: window.clone(),
                                edges,
                                initial_window_location,
                                initial_window_size,
                                last_window_size: initial_window_size,
                            };

                            use smithay::input::pointer::Focus;
                            pointer.set_grab(self, grab, serial, Focus::Clear);
                            pointer.frame(self);
                            return;
                        } else {
                            let initial_window_location = window_loc;

                            let pointer = self.pointer.clone();
                            let start_data = smithay::input::pointer::GrabStartData {
                                focus: None,
                                button: 0x110,
                                location,
                            };

                            let grab = PointerMoveSurfaceGrab {
                                start_data,
                                window: window.clone(),
                                initial_window_location,
                            };

                            use smithay::input::pointer::Focus;
                            pointer.set_grab(self, grab, serial, Focus::Clear);
                            pointer.frame(self);
                            return;
                        }
                    }
                }
            }
        }

        let pointer = self.pointer.clone();
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state: state.try_into().unwrap(),
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);
    }

    fn update_keyboard_focus(&mut self, location: Point<f64, Logical>, serial: Serial) {
        let keyboard = self.seat.get_keyboard().unwrap();
        let touch = self.seat.get_touch();
        let input_method = self.seat.input_method();
        // change the keyboard focus unless the pointer or keyboard is grabbed
        // We test for any matching surface type here but always use the root
        // (in case of a window the toplevel) surface for the focus.
        // So for example if a user clicks on a subsurface or popup the toplevel
        // will receive the keyboard focus. Directly assigning the focus to the
        // matching surface leads to issues with clients dismissing popups and
        // subsurface menus (for example firefox-wayland).
        // see here for a discussion about that issue:
        // https://gitlab.freedesktop.org/wayland/wayland/-/issues/294
        if !self.pointer.is_grabbed()
            && (!keyboard.is_grabbed() || input_method.keyboard_grabbed())
            && !touch.map(|touch| touch.is_grabbed()).unwrap_or(false)
        {
            let output = self.space.output_under(location).next().cloned();
            if let Some(output) = output.as_ref() {
                let output_geo = self.space.output_geometry(output).unwrap();
                if let Some(window) = output
                    .user_data()
                    .get::<FullscreenSurface>()
                    .and_then(|f| f.get())
                {
                    if let Some((_, _)) = window
                        .surface_under(location - output_geo.loc.to_f64(), WindowSurfaceType::ALL)
                    {
                        #[cfg(feature = "xwayland")]
                        if let Some(surface) = window.0.x11_surface() {
                            self.xwm.as_mut().unwrap().raise_window(surface).unwrap();
                        }
                        keyboard.set_focus(self, Some(window.into()), serial);
                        return;
                    }
                }

                let layers = layer_map_for_output(output);
                if let Some(layer) = layers
                    .layer_under(WlrLayer::Overlay, location - output_geo.loc.to_f64())
                    .or_else(|| {
                        layers.layer_under(WlrLayer::Top, location - output_geo.loc.to_f64())
                    })
                {
                    if layer.can_receive_keyboard_focus() {
                        if let Some((_, _)) = layer.surface_under(
                            location
                                - output_geo.loc.to_f64()
                                - layers.layer_geometry(layer).unwrap().loc.to_f64(),
                            WindowSurfaceType::ALL,
                        ) {
                            keyboard.set_focus(self, Some(layer.clone().into()), serial);
                            return;
                        }
                    }
                }
            }

            if let Some((window, _)) = self
                .space
                .element_under(location)
                .map(|(w, p)| (w.clone(), p))
            {
                // Ignore windows on inactive workspaces (invisible).
                if window.decoration_state().hidden {
                    return;
                }
                self.space.raise_element(&window, true);
                #[cfg(feature = "xwayland")]
                if let Some(surface) = window.0.x11_surface() {
                    self.xwm.as_mut().unwrap().raise_window(surface).unwrap();
                }
                keyboard.set_focus(self, Some(window.into()), serial);
                // Re-run the layout so scrollable/focus-following layouts (e.g. the
                // niri-style scrolling layout in the config) pan the view to the
                // focused window.
                self.arrange_layout();
                return;
            }

            if let Some(output) = output.as_ref() {
                let output_geo = self.space.output_geometry(output).unwrap();
                let layers = layer_map_for_output(output);
                if let Some(layer) = layers
                    .layer_under(WlrLayer::Bottom, location - output_geo.loc.to_f64())
                    .or_else(|| {
                        layers.layer_under(WlrLayer::Background, location - output_geo.loc.to_f64())
                    })
                {
                    if layer.can_receive_keyboard_focus() {
                        if let Some((_, _)) = layer.surface_under(
                            location
                                - output_geo.loc.to_f64()
                                - layers.layer_geometry(layer).unwrap().loc.to_f64(),
                            WindowSurfaceType::ALL,
                        ) {
                            keyboard.set_focus(self, Some(layer.clone().into()), serial);
                        }
                    }
                }
            };
        }
    }

    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(PointerFocusTarget, Point<f64, Logical>)> {
        let output = self.space.outputs().find(|o| {
            let geometry = self.space.output_geometry(o).unwrap();
            geometry.contains(pos.to_i32_round())
        })?;
        let output_geo = self.space.output_geometry(output).unwrap();
        let layers = layer_map_for_output(output);

        let mut under = None;
        if let Some((surface, loc)) = output
            .user_data()
            .get::<FullscreenSurface>()
            .and_then(|f| f.get())
            .and_then(|w| w.surface_under(pos - output_geo.loc.to_f64(), WindowSurfaceType::ALL))
        {
            under = Some((surface, loc + output_geo.loc));
        } else if let Some(focus) = layers
            .layer_under(WlrLayer::Overlay, pos - output_geo.loc.to_f64())
            .or_else(|| layers.layer_under(WlrLayer::Top, pos - output_geo.loc.to_f64()))
            .and_then(|layer| {
                let layer_loc = layers.layer_geometry(layer).unwrap().loc;
                layer
                    .surface_under(
                        pos - output_geo.loc.to_f64() - layer_loc.to_f64(),
                        WindowSurfaceType::ALL,
                    )
                    .map(|(surface, loc)| {
                        (
                            PointerFocusTarget::from(surface),
                            loc + layer_loc + output_geo.loc,
                        )
                    })
            })
        {
            under = Some(focus)
        } else if let Some(focus) = self.space.element_under(pos).and_then(|(window, loc)| {
            window
                .surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, surf_loc)| (surface, surf_loc + loc))
        }) {
            under = Some(focus);
        } else if let Some(focus) = layers
            .layer_under(WlrLayer::Bottom, pos - output_geo.loc.to_f64())
            .or_else(|| layers.layer_under(WlrLayer::Background, pos - output_geo.loc.to_f64()))
            .and_then(|layer| {
                let layer_loc = layers.layer_geometry(layer).unwrap().loc;
                layer
                    .surface_under(
                        pos - output_geo.loc.to_f64() - layer_loc.to_f64(),
                        WindowSurfaceType::ALL,
                    )
                    .map(|(surface, loc)| {
                        (
                            PointerFocusTarget::from(surface),
                            loc + layer_loc + output_geo.loc,
                        )
                    })
            })
        {
            under = Some(focus)
        };
        under.map(|(s, l)| (s, l.to_f64()))
    }

    fn on_pointer_axis<B: InputBackend>(&mut self, evt: B::PointerAxisEvent) {
        let horizontal_amount = evt.amount(input::Axis::Horizontal).unwrap_or_else(|| {
            evt.amount_v120(input::Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.
        });
        let vertical_amount = evt
            .amount(input::Axis::Vertical)
            .unwrap_or_else(|| evt.amount_v120(input::Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.);
        let horizontal_amount_discrete = evt.amount_v120(input::Axis::Horizontal);
        let vertical_amount_discrete = evt.amount_v120(input::Axis::Vertical);

        {
            let mut frame = AxisFrame::new(evt.time_msec()).source(evt.source());
            if horizontal_amount != 0.0 {
                frame = frame
                    .relative_direction(Axis::Horizontal, evt.relative_direction(Axis::Horizontal));
                frame = frame.value(Axis::Horizontal, horizontal_amount);
                if let Some(discrete) = horizontal_amount_discrete {
                    frame = frame.v120(Axis::Horizontal, discrete as i32);
                }
            }
            if vertical_amount != 0.0 {
                frame = frame
                    .relative_direction(Axis::Vertical, evt.relative_direction(Axis::Vertical));
                frame = frame.value(Axis::Vertical, vertical_amount);
                if let Some(discrete) = vertical_amount_discrete {
                    frame = frame.v120(Axis::Vertical, discrete as i32);
                }
            }
            if evt.source() == AxisSource::Finger {
                if evt.amount(Axis::Horizontal) == Some(0.0) {
                    frame = frame.stop(Axis::Horizontal);
                }
                if evt.amount(Axis::Vertical) == Some(0.0) {
                    frame = frame.stop(Axis::Vertical);
                }
            }
            let pointer = self.pointer.clone();
            pointer.axis(self, frame);
            pointer.frame(self);
        }
    }

    fn touch_location_transformed<B: InputBackend, E: AbsolutePositionEvent<B>>(
        &self,
        evt: &E,
    ) -> Option<Point<f64, Logical>> {
        let output = self
            .space
            .outputs()
            .find(|output| output.name().starts_with("eDP"))
            .or_else(|| self.space.outputs().next());

        let output = output?;
        let output_geometry = self.space.output_geometry(output)?;

        let transform = output.current_transform();
        let size = transform.invert().transform_size(output_geometry.size);
        Some(
            transform.transform_point_in(evt.position_transformed(size), &size.to_f64())
                + output_geometry.loc.to_f64(),
        )
    }

    fn on_touch_down<B: InputBackend>(&mut self, evt: B::TouchDownEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };

        let Some(touch_location) = self.touch_location_transformed(&evt) else {
            return;
        };

        let serial = SCOUNTER.next_serial();
        self.update_keyboard_focus(touch_location, serial);

        let under = self.surface_under(touch_location);
        handle.down(
            self,
            under,
            &DownEvent {
                slot: evt.slot(),
                location: touch_location,
                serial,
                time: evt.time_msec(),
            },
        );
    }

    fn on_touch_up<B: InputBackend>(&mut self, evt: B::TouchUpEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        let serial = SCOUNTER.next_serial();
        handle.up(
            self,
            &UpEvent {
                slot: evt.slot(),
                serial,
                time: evt.time_msec(),
            },
        )
    }

    fn on_touch_motion<B: InputBackend>(&mut self, evt: B::TouchMotionEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        let Some(touch_location) = self.touch_location_transformed(&evt) else {
            return;
        };

        let under = self.surface_under(touch_location);
        handle.motion(
            self,
            under,
            &smithay::input::touch::MotionEvent {
                slot: evt.slot(),
                location: touch_location,
                time: evt.time_msec(),
            },
        );
    }

    fn on_touch_frame<B: InputBackend>(&mut self, _evt: B::TouchFrameEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        handle.frame(self);
    }

    fn on_touch_cancel<B: InputBackend>(&mut self, _evt: B::TouchCancelEvent) {
        let Some(handle) = self.seat.get_touch() else {
            return;
        };
        handle.cancel(self);
    }

    fn on_device_added<B: InputBackend>(&mut self, device: B::Device) {
        let dh = &self.display_handle;
        if device.has_capability(DeviceCapability::TabletTool) {
            self.seat
                .tablet_seat()
                .add_tablet::<Self>(dh, &TabletDescriptor::from(&device));
        }
        if device.has_capability(DeviceCapability::Touch) && self.seat.get_touch().is_none() {
            self.seat.add_touch();
        }
    }

    fn on_device_removed<B: InputBackend>(&mut self, device: B::Device) {
        if device.has_capability(DeviceCapability::TabletTool) {
            let tablet_seat = self.seat.tablet_seat();

            tablet_seat.remove_tablet(&TabletDescriptor::from(&device));

            // If there are no tablets in seat we can remove all tools
            if tablet_seat.count_tablets() == 0 {
                tablet_seat.clear_tools();
            }
        }
    }

    fn update_cursor_for_no_csd(&mut self) {
        if !self.config.window.prefer_no_csd {
            return;
        }

        let resize_modifier = &self.config.window.resize_modifier;
        if resize_modifier.is_empty() {
            return;
        }

        let keyboard = self.seat.get_keyboard().unwrap();
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

        let location = self.pointer.current_location();
        if let Some((window, window_loc)) = self
            .space
            .element_under(location)
            .map(|(w, p)| (w.clone(), p))
        {
            let geometry = window.geometry();
            let window_size = geometry.size;

            let rel_x = location.x - window_loc.x as f64;
            let rel_y = location.y - window_loc.y as f64;

            let edges = detect_resize_edges(rel_x, rel_y, window_size.w, window_size.h);

            let layout_active = self.config.layout.layout != LayoutType::Floating;

            if layout_active {
                // Layout mode: only the left/right edges resize the column width;
                // everything else reorders the window within the layout.
                if edges.intersects(ResizeEdge::LEFT) {
                    self.cursor_status = CursorImageStatus::Named(CursorIcon::WResize);
                } else if edges.intersects(ResizeEdge::RIGHT) {
                    self.cursor_status = CursorImageStatus::Named(CursorIcon::EResize);
                } else {
                    self.cursor_status = CursorImageStatus::Named(CursorIcon::AllScroll);
                }
            } else if !edges.is_empty() {
                self.cursor_status = CursorImageStatus::Named(edges.cursor_icon());
            } else {
                // Center zone: show move cursor when modifier is held
                self.cursor_status = CursorImageStatus::Named(CursorIcon::AllScroll);
            }
        }
    }
}

/// Detect which resize edges are at the given position within a window,
/// using niri's 1/3 zone approach.
fn detect_resize_edges(
    rel_x: f64,
    rel_y: f64,
    window_width: i32,
    window_height: i32,
) -> ResizeEdge {
    let mut edges = ResizeEdge::empty();
    let w = window_width as f64;
    let h = window_height as f64;

    if rel_x < w / 3.0 {
        edges |= ResizeEdge::LEFT;
    } else if rel_x > 2.0 * w / 3.0 {
        edges |= ResizeEdge::RIGHT;
    }

    if rel_y < h / 3.0 {
        edges |= ResizeEdge::TOP;
    } else if rel_y > 2.0 * h / 3.0 {
        edges |= ResizeEdge::BOTTOM;
    }

    edges
}

#[cfg(any(feature = "winit", feature = "x11"))]
impl<BackendData: Backend> AnvilState<BackendData> {
    pub fn process_input_event_windowed<B: InputBackend>(
        &mut self,
        event: InputEvent<B>,
        output_name: &str,
    ) {
        match event {
            InputEvent::Keyboard { event } => match self.keyboard_key_to_action::<B>(event) {
                KeyAction::ScaleUp => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_scale = output.current_scale().fractional_scale();
                    let new_scale = current_scale + 0.25;
                    output.change_current_state(
                        None,
                        None,
                        Some(Scale::Fractional(new_scale)),
                        None,
                    );

                    crate::shell::fixup_positions(&mut self.space, self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                KeyAction::ScaleDown => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_scale = output.current_scale().fractional_scale();
                    let new_scale = f64::max(1.0, current_scale - 0.25);
                    output.change_current_state(
                        None,
                        None,
                        Some(Scale::Fractional(new_scale)),
                        None,
                    );

                    crate::shell::fixup_positions(&mut self.space, self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                KeyAction::RotateOutput => {
                    let output = self
                        .space
                        .outputs()
                        .find(|o| o.name() == output_name)
                        .unwrap()
                        .clone();

                    let current_transform = output.current_transform();
                    let new_transform = match current_transform {
                        Transform::Normal => Transform::_90,
                        Transform::_90 => Transform::_180,
                        Transform::_180 => Transform::_270,
                        Transform::_270 => Transform::Flipped,
                        Transform::Flipped => Transform::Flipped90,
                        Transform::Flipped90 => Transform::Flipped180,
                        Transform::Flipped180 => Transform::Flipped270,
                        Transform::Flipped270 => Transform::Normal,
                    };
                    tracing::info!(?current_transform, ?new_transform, output = ?output.name(), "changing output transform");
                    output.change_current_state(None, Some(new_transform), None, None);
                    crate::shell::fixup_positions(&mut self.space, self.pointer.current_location());
                    self.backend_data.reset_buffers(&output);
                }

                action => match action {
                    KeyAction::None
                    | KeyAction::Quit
                    | KeyAction::CloseWindow
                    | KeyAction::Run(_)
                    | KeyAction::TogglePreview
                    | KeyAction::ToggleDecorations
                    | KeyAction::ToggleFloating
                    | KeyAction::Screenshot
                    | KeyAction::FocusNext
                    | KeyAction::FocusPrev
                    | KeyAction::WorkspaceNext
                    | KeyAction::WorkspacePrev
                    | KeyAction::Workspace(_)
                    | KeyAction::ResizeWidthUp
                    | KeyAction::ResizeWidthDown
                    | KeyAction::ToggleFullscreen
                    | KeyAction::ToggleMaximize => self.process_common_key_action(action),

                    _ => tracing::warn!(
                        ?action,
                        output_name,
                        "Key action unsupported on on output backend.",
                    ),
                },
            },

            InputEvent::PointerMotionAbsolute { event } => {
                let output = self
                    .space
                    .outputs()
                    .find(|o| o.name() == output_name)
                    .unwrap()
                    .clone();
                self.on_pointer_move_absolute_windowed::<B>(event, &output)
            }
            InputEvent::PointerButton { event } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.on_pointer_axis::<B>(event),
            InputEvent::TouchDown { event } => self.on_touch_down::<B>(event),
            InputEvent::TouchUp { event } => self.on_touch_up::<B>(event),
            InputEvent::TouchMotion { event } => self.on_touch_motion::<B>(event),
            InputEvent::TouchFrame { event } => self.on_touch_frame::<B>(event),
            InputEvent::TouchCancel { event } => self.on_touch_cancel::<B>(event),
            InputEvent::DeviceAdded { device } => self.on_device_added::<B>(device),
            InputEvent::DeviceRemoved { device } => self.on_device_removed::<B>(device),
            _ => (), // other events are not handled in anvil (yet)
        }
    }

    fn on_pointer_move_absolute_windowed<B: InputBackend>(
        &mut self,
        evt: B::PointerMotionAbsoluteEvent,
        output: &Output,
    ) {
        let output_geo = self.space.output_geometry(output).unwrap();

        let pos = evt.position_transformed(output_geo.size) + output_geo.loc.to_f64();
        let serial = SCOUNTER.next_serial();

        let pointer = self.pointer.clone();
        let under = self.surface_under(pos);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);
    }

    pub fn release_all_keys(&mut self) {
        let keyboard = self.seat.get_keyboard().unwrap();
        for keycode in keyboard.pressed_keys() {
            keyboard.input(
                self,
                keycode,
                KeyState::Released,
                SCOUNTER.next_serial(),
                0,
                |_, _, _| FilterResult::Forward::<bool>,
            );
        }
    }
}

#[cfg(feature = "udev")]
impl AnvilState<UdevData> {
    pub fn process_input_event<B: InputBackend>(
        &mut self,
        dh: &DisplayHandle,
        event: InputEvent<B>,
    ) {
        match event {
            InputEvent::Keyboard { event, .. } => match self.keyboard_key_to_action::<B>(event) {
                #[cfg(feature = "udev")]
                KeyAction::VtSwitch(vt) => {
                    info!(to = vt, "Trying to switch vt");
                    if let Err(err) = self.backend_data.session.change_vt(vt) {
                        error!(vt, "Error switching vt: {}", err);
                    }
                }
                KeyAction::Screen(num) => {
                    let geometry = self
                        .space
                        .outputs()
                        .nth(num)
                        .map(|o| self.space.output_geometry(o).unwrap());

                    if let Some(geometry) = geometry {
                        let x = geometry.loc.x as f64 + geometry.size.w as f64 / 2.0;
                        let y = geometry.size.h as f64 / 2.0;
                        let location = (x, y).into();
                        let pointer = self.pointer.clone();
                        let under = self.surface_under(location);
                        pointer.motion(
                            self,
                            under,
                            &MotionEvent {
                                location,
                                serial: SCOUNTER.next_serial(),
                                time: self.clock.now().as_millis(),
                            },
                        );
                        pointer.frame(self);
                    }
                }
                KeyAction::ScaleUp => {
                    let pos = self.pointer.current_location().to_i32_round();
                    let output = self
                        .space
                        .outputs()
                        .find(|o| self.space.output_geometry(o).unwrap().contains(pos))
                        .cloned();

                    if let Some(output) = output {
                        let (output_location, scale) = (
                            self.space.output_geometry(&output).unwrap().loc,
                            output.current_scale().fractional_scale(),
                        );
                        let new_scale = scale + 0.25;
                        output.change_current_state(
                            None,
                            None,
                            Some(Scale::Fractional(new_scale)),
                            None,
                        );

                        let rescale = scale / new_scale;
                        let output_location = output_location.to_f64();
                        let mut pointer_output_location =
                            self.pointer.current_location() - output_location;
                        pointer_output_location.x *= rescale;
                        pointer_output_location.y *= rescale;
                        let pointer_location = output_location + pointer_output_location;

                        crate::shell::fixup_positions(&mut self.space, pointer_location);
                        let pointer = self.pointer.clone();
                        let under = self.surface_under(pointer_location);
                        pointer.motion(
                            self,
                            under,
                            &MotionEvent {
                                location: pointer_location,
                                serial: SCOUNTER.next_serial(),
                                time: self.clock.now().as_millis(),
                            },
                        );
                        pointer.frame(self);
                        self.backend_data.reset_buffers(&output);
                    }
                }
                KeyAction::ScaleDown => {
                    let pos = self.pointer.current_location().to_i32_round();
                    let output = self
                        .space
                        .outputs()
                        .find(|o| self.space.output_geometry(o).unwrap().contains(pos))
                        .cloned();

                    if let Some(output) = output {
                        let (output_location, scale) = (
                            self.space.output_geometry(&output).unwrap().loc,
                            output.current_scale().fractional_scale(),
                        );
                        let new_scale = f64::max(1.0, scale - 0.25);
                        output.change_current_state(
                            None,
                            None,
                            Some(Scale::Fractional(new_scale)),
                            None,
                        );

                        let rescale = scale / new_scale;
                        let output_location = output_location.to_f64();
                        let mut pointer_output_location =
                            self.pointer.current_location() - output_location;
                        pointer_output_location.x *= rescale;
                        pointer_output_location.y *= rescale;
                        let pointer_location = output_location + pointer_output_location;

                        crate::shell::fixup_positions(&mut self.space, pointer_location);
                        let pointer = self.pointer.clone();
                        let under = self.surface_under(pointer_location);
                        pointer.motion(
                            self,
                            under,
                            &MotionEvent {
                                location: pointer_location,
                                serial: SCOUNTER.next_serial(),
                                time: self.clock.now().as_millis(),
                            },
                        );
                        pointer.frame(self);
                        self.backend_data.reset_buffers(&output);
                    }
                }
                KeyAction::RotateOutput => {
                    let pos = self.pointer.current_location().to_i32_round();
                    let output = self
                        .space
                        .outputs()
                        .find(|o| self.space.output_geometry(o).unwrap().contains(pos))
                        .cloned();

                    if let Some(output) = output {
                        let current_transform = output.current_transform();
                        let new_transform = match current_transform {
                            Transform::Normal => Transform::_90,
                            Transform::_90 => Transform::_180,
                            Transform::_180 => Transform::_270,
                            Transform::_270 => Transform::Flipped,
                            Transform::Flipped => Transform::Flipped90,
                            Transform::Flipped90 => Transform::Flipped180,
                            Transform::Flipped180 => Transform::Flipped270,
                            Transform::Flipped270 => Transform::Normal,
                        };
                        output.change_current_state(None, Some(new_transform), None, None);
                        crate::shell::fixup_positions(
                            &mut self.space,
                            self.pointer.current_location(),
                        );
                        self.backend_data.reset_buffers(&output);
                    }
                }
                KeyAction::ToggleTint => {
                    let mut debug_flags = self.backend_data.debug_flags();
                    debug_flags.toggle(DebugFlags::TINT);
                    self.backend_data.set_debug_flags(debug_flags);
                }

                action => match action {
                    KeyAction::None
                    | KeyAction::Quit
                    | KeyAction::CloseWindow
                    | KeyAction::Run(_)
                    | KeyAction::TogglePreview
                    | KeyAction::ToggleDecorations
                    | KeyAction::ToggleFloating
                    | KeyAction::Screenshot
                    | KeyAction::FocusNext
                    | KeyAction::FocusPrev
                    | KeyAction::WorkspaceNext
                    | KeyAction::WorkspacePrev
                    | KeyAction::Workspace(_)
                    | KeyAction::ResizeWidthUp
                    | KeyAction::ResizeWidthDown
                    | KeyAction::ToggleFullscreen
                    | KeyAction::ToggleMaximize => self.process_common_key_action(action),

                    _ => unreachable!(),
                },
            },
            InputEvent::PointerMotion { event, .. } => self.on_pointer_move::<B>(dh, event),
            InputEvent::PointerMotionAbsolute { event, .. } => {
                self.on_pointer_move_absolute::<B>(dh, event)
            }
            InputEvent::PointerButton { event, .. } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event, .. } => self.on_pointer_axis::<B>(event),
            InputEvent::TabletToolAxis { event, .. } => self.on_tablet_tool_axis::<B>(event),
            InputEvent::TabletToolProximity { event, .. } => {
                self.on_tablet_tool_proximity::<B>(dh, event)
            }
            InputEvent::TabletToolTip { event, .. } => self.on_tablet_tool_tip::<B>(event),
            InputEvent::TabletToolButton { event, .. } => self.on_tablet_button::<B>(event),
            InputEvent::GestureSwipeBegin { event, .. } => self.on_gesture_swipe_begin::<B>(event),
            InputEvent::GestureSwipeUpdate { event, .. } => {
                self.on_gesture_swipe_update::<B>(event)
            }
            InputEvent::GestureSwipeEnd { event, .. } => self.on_gesture_swipe_end::<B>(event),
            InputEvent::GesturePinchBegin { event, .. } => self.on_gesture_pinch_begin::<B>(event),
            InputEvent::GesturePinchUpdate { event, .. } => {
                self.on_gesture_pinch_update::<B>(event)
            }
            InputEvent::GesturePinchEnd { event, .. } => self.on_gesture_pinch_end::<B>(event),
            InputEvent::GestureHoldBegin { event, .. } => self.on_gesture_hold_begin::<B>(event),
            InputEvent::GestureHoldEnd { event, .. } => self.on_gesture_hold_end::<B>(event),

            InputEvent::TouchDown { event } => self.on_touch_down::<B>(event),
            InputEvent::TouchUp { event } => self.on_touch_up::<B>(event),
            InputEvent::TouchMotion { event } => self.on_touch_motion::<B>(event),
            InputEvent::TouchFrame { event } => self.on_touch_frame::<B>(event),
            InputEvent::TouchCancel { event } => self.on_touch_cancel::<B>(event),

            InputEvent::DeviceAdded { device } => self.on_device_added::<B>(device),
            InputEvent::DeviceRemoved { device } => self.on_device_removed::<B>(device),
            _ => {
                // other events are not handled in anvil (yet)
            }
        }
    }

    fn on_pointer_move<B: InputBackend>(
        &mut self,
        _dh: &DisplayHandle,
        evt: B::PointerMotionEvent,
    ) {
        let mut pointer_location = self.pointer.current_location();
        let serial = SCOUNTER.next_serial();

        let pointer = self.pointer.clone();
        let under = self.surface_under(pointer_location);

        let mut pointer_locked = false;
        let mut pointer_confined = false;
        let mut confine_region = None;
        if let Some((surface, surface_loc)) = under
            .as_ref()
            .and_then(|(target, l)| Some((target.wl_surface()?, l)))
        {
            with_pointer_constraint(&surface, &pointer, |constraint| match constraint {
                Some(constraint) if constraint.is_active() => {
                    // Constraint does not apply if not within region
                    if !constraint.region().is_none_or(|x| {
                        x.contains((pointer_location - *surface_loc).to_i32_round())
                    }) {
                        return;
                    }
                    match &*constraint {
                        PointerConstraint::Locked(_locked) => {
                            pointer_locked = true;
                        }
                        PointerConstraint::Confined(confine) => {
                            pointer_confined = true;
                            confine_region = confine.region().cloned();
                        }
                    }
                }
                _ => {}
            });
        }

        pointer.relative_motion(
            self,
            under.clone(),
            &RelativeMotionEvent {
                delta: evt.delta(),
                delta_unaccel: evt.delta_unaccel(),
                utime: evt.time(),
            },
        );

        // If pointer is locked, only emit relative motion
        if pointer_locked {
            pointer.frame(self);
            return;
        }

        pointer_location += evt.delta();

        // clamp to screen limits
        // this event is never generated by winit
        pointer_location = self.clamp_coords(pointer_location);

        let new_under = self.surface_under(pointer_location);

        // If confined, don't move pointer if it would go outside surface or region
        if pointer_confined {
            if let Some((surface, surface_loc)) = &under {
                if new_under.as_ref().and_then(|(under, _)| under.wl_surface())
                    != surface.wl_surface()
                {
                    pointer.frame(self);
                    return;
                }
                if let Some(region) = confine_region {
                    if !region.contains((pointer_location - *surface_loc).to_i32_round()) {
                        pointer.frame(self);
                        return;
                    }
                }
            }
        }

        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pointer_location,
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);

        self.update_cursor_for_no_csd();

        // If pointer is now in a constraint region, activate it
        // TODO Anywhere else pointer is moved needs to do this
        if let Some((under, surface_location)) =
            new_under.and_then(|(target, loc)| Some((target.wl_surface()?.into_owned(), loc)))
        {
            with_pointer_constraint(&under, &pointer, |constraint| match constraint {
                Some(constraint) if !constraint.is_active() => {
                    let point = (pointer_location - surface_location).to_i32_round();
                    if constraint
                        .region()
                        .is_none_or(|region| region.contains(point))
                    {
                        constraint.activate();
                    }
                }
                _ => {}
            });
        }
    }

    fn on_pointer_move_absolute<B: InputBackend>(
        &mut self,
        _dh: &DisplayHandle,
        evt: B::PointerMotionAbsoluteEvent,
    ) {
        let serial = SCOUNTER.next_serial();

        let max_x = self.space.outputs().fold(0, |acc, o| {
            acc + self.space.output_geometry(o).unwrap().size.w
        });

        let max_h_output = self
            .space
            .outputs()
            .max_by_key(|o| self.space.output_geometry(o).unwrap().size.h)
            .unwrap();

        let max_y = self.space.output_geometry(max_h_output).unwrap().size.h;

        let mut pointer_location = (evt.x_transformed(max_x), evt.y_transformed(max_y)).into();

        // clamp to screen limits
        pointer_location = self.clamp_coords(pointer_location);

        let pointer = self.pointer.clone();
        let under = self.surface_under(pointer_location);

        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pointer_location,
                serial,
                time: evt.time_msec(),
            },
        );
        pointer.frame(self);

        self.update_cursor_for_no_csd();
    }

    fn on_tablet_tool_axis<B: InputBackend>(&mut self, evt: B::TabletToolAxisEvent) {
        let tablet_seat = self.seat.tablet_seat();

        if let Some(pointer_location) = self.touch_location_transformed(&evt) {
            let pointer = self.pointer.clone();
            let under = self.surface_under(pointer_location);
            let tablet = tablet_seat.get_tablet(&TabletDescriptor::from(&evt.device()));
            let tool = tablet_seat.get_tool(&evt.tool());

            pointer.motion(
                self,
                under.clone(),
                &MotionEvent {
                    location: pointer_location,
                    serial: SCOUNTER.next_serial(),
                    time: self.clock.now().as_millis(),
                },
            );

            if let (Some(tablet), Some(tool)) = (tablet, tool) {
                if evt.pressure_has_changed() {
                    tool.pressure(evt.pressure());
                }
                if evt.distance_has_changed() {
                    tool.distance(evt.distance());
                }
                if evt.tilt_has_changed() {
                    tool.tilt(evt.tilt());
                }
                if evt.slider_has_changed() {
                    tool.slider_position(evt.slider_position());
                }
                if evt.rotation_has_changed() {
                    tool.rotation(evt.rotation());
                }
                if evt.wheel_has_changed() {
                    tool.wheel(evt.wheel_delta(), evt.wheel_delta_discrete());
                }

                tool.motion(
                    pointer_location,
                    under.and_then(|(f, loc)| f.wl_surface().map(|s| (s.into_owned(), loc))),
                    &tablet,
                    SCOUNTER.next_serial(),
                    evt.time_msec(),
                );
            }

            pointer.frame(self);
        }
    }

    fn on_tablet_tool_proximity<B: InputBackend>(
        &mut self,
        dh: &DisplayHandle,
        evt: B::TabletToolProximityEvent,
    ) {
        let tablet_seat = self.seat.tablet_seat();

        if let Some(pointer_location) = self.touch_location_transformed(&evt) {
            let tool = evt.tool();
            tablet_seat.add_tool::<Self>(self, dh, &tool);

            let pointer = self.pointer.clone();
            let under = self.surface_under(pointer_location);
            let tablet = tablet_seat.get_tablet(&TabletDescriptor::from(&evt.device()));
            let tool = tablet_seat.get_tool(&tool);

            pointer.motion(
                self,
                under.clone(),
                &MotionEvent {
                    location: pointer_location,
                    serial: SCOUNTER.next_serial(),
                    time: evt.time_msec(),
                },
            );
            pointer.frame(self);

            if let (Some(under), Some(tablet), Some(tool)) = (
                under.and_then(|(f, loc)| f.wl_surface().map(|s| (s.into_owned(), loc))),
                tablet,
                tool,
            ) {
                match evt.state() {
                    ProximityState::In => tool.proximity_in(
                        pointer_location,
                        under,
                        &tablet,
                        SCOUNTER.next_serial(),
                        evt.time_msec(),
                    ),
                    ProximityState::Out => tool.proximity_out(evt.time_msec()),
                }
            }
        }
    }

    fn on_tablet_tool_tip<B: InputBackend>(&mut self, evt: B::TabletToolTipEvent) {
        let tool = self.seat.tablet_seat().get_tool(&evt.tool());

        if let Some(tool) = tool {
            match evt.tip_state() {
                TabletToolTipState::Down => {
                    let serial = SCOUNTER.next_serial();
                    tool.tip_down(serial, evt.time_msec());

                    // change the keyboard focus
                    self.update_keyboard_focus(self.pointer.current_location(), serial);
                }
                TabletToolTipState::Up => {
                    tool.tip_up(evt.time_msec());
                }
            }
        }
    }

    fn on_tablet_button<B: InputBackend>(&mut self, evt: B::TabletToolButtonEvent) {
        let tool = self.seat.tablet_seat().get_tool(&evt.tool());

        if let Some(tool) = tool {
            tool.button(
                evt.button(),
                evt.button_state(),
                SCOUNTER.next_serial(),
                evt.time_msec(),
            );
        }
    }

    fn on_gesture_swipe_begin<B: InputBackend>(&mut self, evt: B::GestureSwipeBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_begin(
            self,
            &GestureSwipeBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_swipe_update<B: InputBackend>(&mut self, evt: B::GestureSwipeUpdateEvent) {
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_update(
            self,
            &GestureSwipeUpdateEvent {
                time: evt.time_msec(),
                delta: evt.delta(),
            },
        );
    }

    fn on_gesture_swipe_end<B: InputBackend>(&mut self, evt: B::GestureSwipeEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_swipe_end(
            self,
            &GestureSwipeEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn on_gesture_pinch_begin<B: InputBackend>(&mut self, evt: B::GesturePinchBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_begin(
            self,
            &GesturePinchBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_pinch_update<B: InputBackend>(&mut self, evt: B::GesturePinchUpdateEvent) {
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_update(
            self,
            &GesturePinchUpdateEvent {
                time: evt.time_msec(),
                delta: evt.delta(),
                scale: evt.scale(),
                rotation: evt.rotation(),
            },
        );
    }

    fn on_gesture_pinch_end<B: InputBackend>(&mut self, evt: B::GesturePinchEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_pinch_end(
            self,
            &GesturePinchEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn on_gesture_hold_begin<B: InputBackend>(&mut self, evt: B::GestureHoldBeginEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_hold_begin(
            self,
            &GestureHoldBeginEvent {
                serial,
                time: evt.time_msec(),
                fingers: evt.fingers(),
            },
        );
    }

    fn on_gesture_hold_end<B: InputBackend>(&mut self, evt: B::GestureHoldEndEvent) {
        let serial = SCOUNTER.next_serial();
        let pointer = self.pointer.clone();
        pointer.gesture_hold_end(
            self,
            &GestureHoldEndEvent {
                serial,
                time: evt.time_msec(),
                cancelled: evt.cancelled(),
            },
        );
    }

    fn clamp_coords(&self, pos: Point<f64, Logical>) -> Point<f64, Logical> {
        if self.space.outputs().next().is_none() {
            return pos;
        }

        let (pos_x, pos_y) = pos.into();
        let max_x = self.space.outputs().fold(0, |acc, o| {
            acc + self.space.output_geometry(o).unwrap().size.w
        });
        let clamped_x = pos_x.clamp(0.0, max_x as f64);
        let max_y = self
            .space
            .outputs()
            .find(|o| {
                let geo = self.space.output_geometry(o).unwrap();
                geo.contains((clamped_x as i32, 0))
            })
            .map(|o| self.space.output_geometry(o).unwrap().size.h);

        if let Some(max_y) = max_y {
            let clamped_y = pos_y.clamp(0.0, max_y as f64);
            (clamped_x, clamped_y).into()
        } else {
            (clamped_x, pos_y).into()
        }
    }
}

/// Possible results of a keyboard action
#[allow(dead_code)] // some of these are only read if udev is enabled
#[derive(Debug)]
enum KeyAction {
    /// Quit the compositor
    Quit,
    /// Close the focused window
    CloseWindow,
    /// Trigger a vt-switch
    VtSwitch(i32),
    /// run a command
    Run(String),
    /// Switch the current screen
    Screen(usize),
    /// Focus the next window in the layout
    FocusNext,
    /// Focus the previous window in the layout
    FocusPrev,
    /// Switch to the next workspace (creating it if needed)
    WorkspaceNext,
    /// Switch to the previous workspace
    WorkspacePrev,
    /// Switch to a specific workspace
    Workspace(usize),
    /// Grow the focused window's column width
    ResizeWidthUp,
    /// Shrink the focused window's column width
    ResizeWidthDown,
    /// Toggle true fullscreen for the focused window
    ToggleFullscreen,
    /// Toggle windowed fullscreen (fills the work area)
    ToggleMaximize,
    ScaleUp,
    ScaleDown,
    TogglePreview,
    RotateOutput,
    ToggleTint,
    ToggleFloating,
    ToggleDecorations,
    /// Take a screenshot
    Screenshot,
    /// Call a custom Lua callback by index
    Callback(usize),
    /// Do nothing more
    None,
}

fn process_keyboard_shortcut(
    binds: &[BindConfig],
    modifiers: ModifiersState,
    modified: Keysym,
    raw: Option<Keysym>,
) -> Option<KeyAction> {
    if (xkb::KEY_XF86Switch_VT_1..=xkb::KEY_XF86Switch_VT_12).contains(&modified.raw()) {
        return Some(KeyAction::VtSwitch(
            (modified.raw() - xkb::KEY_XF86Switch_VT_1 + 1) as i32,
        ));
    }

    if modified == Keysym::Print || raw == Some(Keysym::Print) {
        return Some(KeyAction::Screenshot);
    }

    for bind in binds {
        let required_mods = |m: &str| match m {
            "Ctrl" => modifiers.ctrl,
            "Alt" => modifiers.alt,
            "Super" | "Logo" => modifiers.logo,
            "Shift" => modifiers.shift,
            "IsoLevel3Shift" => modifiers.iso_level3_shift,
            "IsoLevel5Shift" => modifiers.iso_level5_shift,
            _ => false,
        };

        let all_required_pressed = bind.modifiers.iter().all(|m| required_mods(m.as_str()));
        if !all_required_pressed {
            continue;
        }

        let bind_has_ctrl = bind.modifiers.iter().any(|m| m == "Ctrl");
        let bind_has_alt = bind.modifiers.iter().any(|m| m == "Alt");
        let bind_has_super = bind.modifiers.iter().any(|m| m == "Super" || m == "Logo");
        let bind_has_shift = bind.modifiers.iter().any(|m| m == "Shift");
        let bind_has_iso3 = bind.modifiers.iter().any(|m| m == "IsoLevel3Shift");
        let bind_has_iso5 = bind.modifiers.iter().any(|m| m == "IsoLevel5Shift");

        let no_extra_pressed = (!modifiers.ctrl || bind_has_ctrl)
            && (!modifiers.alt || bind_has_alt)
            && (!modifiers.logo || bind_has_super)
            && (!modifiers.shift || bind_has_shift)
            && (!modifiers.iso_level3_shift || bind_has_iso3)
            && (!modifiers.iso_level5_shift || bind_has_iso5);

        if !no_extra_pressed {
            continue;
        }

        let key_matches = match bind.key.as_str() {
            "BackSpace" => modified == Keysym::BackSpace || raw == Some(Keysym::BackSpace),
            "Return" => modified == Keysym::Return || raw == Some(Keysym::Return),
            "Tab" => modified == Keysym::Tab || raw == Some(Keysym::Tab),
            "Escape" => modified == Keysym::Escape || raw == Some(Keysym::Escape),
            "Space" => modified == Keysym::space || raw == Some(Keysym::space),
            "Print" => modified == Keysym::Print || raw == Some(Keysym::Print),
            "Insert" => modified == Keysym::Insert || raw == Some(Keysym::Insert),
            "Delete" => modified == Keysym::Delete || raw == Some(Keysym::Delete),
            "Home" => modified == Keysym::Home || raw == Some(Keysym::Home),
            "End" => modified == Keysym::End || raw == Some(Keysym::End),
            "Page_Up" => modified == Keysym::Page_Up || raw == Some(Keysym::Page_Up),
            "Page_Down" => modified == Keysym::Page_Down || raw == Some(Keysym::Page_Down),
            "Left" => modified == Keysym::Left || raw == Some(Keysym::Left),
            "Right" => modified == Keysym::Right || raw == Some(Keysym::Right),
            "Up" => modified == Keysym::Up || raw == Some(Keysym::Up),
            "Down" => modified == Keysym::Down || raw == Some(Keysym::Down),
            "F1" => modified == Keysym::F1 || raw == Some(Keysym::F1),
            "F2" => modified == Keysym::F2 || raw == Some(Keysym::F2),
            "F3" => modified == Keysym::F3 || raw == Some(Keysym::F3),
            "F4" => modified == Keysym::F4 || raw == Some(Keysym::F4),
            "F5" => modified == Keysym::F5 || raw == Some(Keysym::F5),
            "F6" => modified == Keysym::F6 || raw == Some(Keysym::F6),
            "F7" => modified == Keysym::F7 || raw == Some(Keysym::F7),
            "F8" => modified == Keysym::F8 || raw == Some(Keysym::F8),
            "F9" => modified == Keysym::F9 || raw == Some(Keysym::F9),
            "F10" => modified == Keysym::F10 || raw == Some(Keysym::F10),
            "F11" => modified == Keysym::F11 || raw == Some(Keysym::F11),
            "F12" => modified == Keysym::F12 || raw == Some(Keysym::F12),
            single_char if single_char.len() == 1 => {
                if let Some(c) = single_char.chars().next() {
                    let lower = c.to_lowercase().next().unwrap();
                    let lower_keysym = Keysym::from_char(lower);
                    let upper_keysym = Keysym::from_char(c);
                    modified == lower_keysym
                        || modified == upper_keysym
                        || raw == Some(lower_keysym)
                        || raw == Some(upper_keysym)
                } else {
                    false
                }
            }
            other => {
                let modified_name = xkbcommon::xkb::keysym_get_name(modified);
                let raw_matches = raw
                    .map(|r| {
                        let raw_name = xkbcommon::xkb::keysym_get_name(r);
                        raw_name == other
                    })
                    .unwrap_or(false);
                modified_name == other || raw_matches
            }
        };

        if key_matches {
            return Some(match &bind.action {
                crate::config::BindAction::Quit => KeyAction::Quit,
                crate::config::BindAction::CloseWindow => KeyAction::CloseWindow,
                crate::config::BindAction::Run(cmd) => KeyAction::Run(cmd.clone()),
                crate::config::BindAction::Screenshot => KeyAction::Screenshot,
                crate::config::BindAction::ToggleDecorations => KeyAction::ToggleDecorations,
                crate::config::BindAction::TogglePreview => KeyAction::TogglePreview,
                crate::config::BindAction::ScaleUp => KeyAction::ScaleUp,
                crate::config::BindAction::ScaleDown => KeyAction::ScaleDown,
                crate::config::BindAction::RotateOutput => KeyAction::RotateOutput,
                crate::config::BindAction::ToggleTint => KeyAction::ToggleTint,
                crate::config::BindAction::ToggleFloating => KeyAction::ToggleFloating,
crate::config::BindAction::VtSwitch(n) => KeyAction::VtSwitch(*n),
        crate::config::BindAction::Screen(n) => KeyAction::Screen(*n),
        crate::config::BindAction::FocusNext => KeyAction::FocusNext,
        crate::config::BindAction::FocusPrev => KeyAction::FocusPrev,
        crate::config::BindAction::WorkspaceNext => KeyAction::WorkspaceNext,
        crate::config::BindAction::WorkspacePrev => KeyAction::WorkspacePrev,
        crate::config::BindAction::Workspace(n) => KeyAction::Workspace(*n),
        crate::config::BindAction::ResizeWidthUp => KeyAction::ResizeWidthUp,
        crate::config::BindAction::ResizeWidthDown => KeyAction::ResizeWidthDown,
        crate::config::BindAction::ToggleFullscreen => KeyAction::ToggleFullscreen,
        crate::config::BindAction::ToggleMaximize => KeyAction::ToggleMaximize,
        crate::config::BindAction::Callback(idx) => KeyAction::Callback(*idx),
            });
        }
    }

    None
}
