#[cfg(feature = "xwayland")]
use std::os::unix::io::OwnedFd;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use tracing::{info, warn};

use smithay::{
    backend::{
        input::{Keycode, TabletToolDescriptor},
        renderer::element::{
            RenderElementStates, default_primary_scanout_output_compare,
            utils::select_dmabuf_feedback,
        },
    },
    delegate_dispatch2,
    desktop::{
        PopupKind, PopupManager, Space,
        space::SpaceElement,
        utils::{
            OutputPresentationFeedback, surface_presentation_feedback_flags_from_states,
            surface_primary_scanout_output, update_surface_primary_scanout_output,
            with_surfaces_surface_tree,
        },
    },
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DnDGrab, DndGrabHandler, DndTarget, GrabType, Source},
        keyboard::{LedState, XkbConfig},
        pointer::{CursorImageStatus, Focus, PointerHandle},
    },
    output::Output,
    reexports::{
        calloop::{Interest, LoopHandle, Mode, PostAction, generic::Generic},
        wayland_protocols::xdg::decoration::{
            self as xdg_decoration,
            zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
        },
        wayland_server::{
            Client, Display, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Clock, Logical, Monotonic, Point, Rectangle, Serial, Time},
    wayland::{
        background_effect::{self, BackgroundEffectState, ExtBackgroundEffectHandler},
        commit_timing::{CommitTimerBarrierStateUserData, CommitTimingManagerState},
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent, with_states,
        },
        dmabuf::DmabufFeedback,
        fifo::{FifoBarrierCachedState, FifoManagerState},
        fixes::FixesState,
        fractional_scale::{
            FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale,
        },
        image_capture_source::{
            ImageCaptureSource, ImageCaptureSourceHandler, ImageCaptureSourceState,
            OutputCaptureSourceHandler, OutputCaptureSourceState,
        },
        image_copy_capture::{
            BufferConstraints, Frame, ImageCopyCaptureHandler, ImageCopyCaptureState, Session,
            SessionRef,
        },
        input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface},
        keyboard_shortcuts_inhibit::{
            KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState,
            KeyboardShortcutsInhibitor,
        },
        output::{OutputHandler, OutputManagerState},
        pointer_constraints::{
            PointerConstraintsHandler, PointerConstraintsState, with_pointer_constraint,
        },
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        seat::WaylandFocus,
        security_context::{
            SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
            SecurityContextState,
        },
        selection::{
            SelectionHandler,
            data_device::{
                DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus,
            },
            primary_selection::{
                PrimarySelectionHandler, PrimarySelectionState, set_primary_focus,
            },
            wlr_data_control::{DataControlHandler, DataControlState},
        },
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{
                ToplevelSurface, XdgShellState,
                decoration::{XdgDecorationHandler, XdgDecorationState},
            },
        },
        shm::{ShmHandler, ShmState},
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        tablet_manager::{TabletManagerState, TabletSeatHandler},
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        virtual_keyboard::VirtualKeyboardManagerState,
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
        xdg_foreign::{XdgForeignHandler, XdgForeignState},
    },
};

#[cfg(feature = "xwayland")]
use crate::cursor::Cursor;
use crate::{
    focus::{KeyboardFocusTarget, PointerFocusTarget},
    ipc::{IpcRequest, IpcResponse, OutputInfo, WindowInfo},
    screencopy::{Screencopy, ScreencopyHandler, ScreencopyManagerState},
    shell::{WindowElement, WindowRenderElement},
};
#[cfg(feature = "xdp-gnome-screencast")]
use crate::dbus::gnome_shell_introspect::{IntrospectToState, StateToIntrospect, WindowProperties};
use smithay::backend::renderer::gles::GlesRenderer;
#[cfg(feature = "xwayland")]
use smithay::{
    utils::Size,
    wayland::selection::{SelectionSource, SelectionTarget},
    wayland::xwayland_keyboard_grab::{XWaylandKeyboardGrabHandler, XWaylandKeyboardGrabState},
    wayland::xwayland_shell,
    xwayland::{X11Wm, XWayland, XWaylandEvent},
};

#[derive(Debug, Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    pub security_context: Option<SecurityContext>,
}
impl ClientData for ClientState {
    /// Notification that a client was initialized
    fn initialized(&self, _client_id: ClientId) {}
    /// Notification that a client is disconnected
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

pub struct ConfigWatcher(pub notify::RecommendedWatcher);

impl std::fmt::Debug for ConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        return f.debug_struct("ConfigWatcher").finish()
    }
}

#[derive(Debug)]
pub struct AnvilState<BackendData: Backend + 'static> {
    pub backend_data: BackendData,
    pub socket_name: Option<String>,
    pub display_handle: DisplayHandle,
    pub running: Arc<AtomicBool>,
    pub handle: LoopHandle<'static, AnvilState<BackendData>>,

    // desktop
    pub space: Space<WindowElement>,
    pub popups: PopupManager,

    // smithay state
    pub compositor_state: CompositorState,
    pub data_device_state: DataDeviceState,
    pub layer_shell_state: WlrLayerShellState,
    pub output_manager_state: OutputManagerState,
    pub primary_selection_state: PrimarySelectionState,
    pub data_control_state: DataControlState,
    pub seat_state: SeatState<AnvilState<BackendData>>,
    pub keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    pub shm_state: ShmState,
    pub viewporter_state: ViewporterState,
    pub xdg_activation_state: XdgActivationState,
    pub xdg_decoration_state: XdgDecorationState,
    pub xdg_shell_state: XdgShellState,
    pub presentation_state: PresentationState,
    pub fractional_scale_manager_state: FractionalScaleManagerState,
    pub xdg_foreign_state: XdgForeignState,
    #[cfg(feature = "xwayland")]
    pub xwayland_shell_state: xwayland_shell::XWaylandShellState,
    pub single_pixel_buffer_state: SinglePixelBufferState,
    pub fifo_manager_state: FifoManagerState,
    pub commit_timing_manager_state: CommitTimingManagerState,
    pub image_capture_source_state: ImageCaptureSourceState,
    pub output_capture_source_state: OutputCaptureSourceState,
    pub image_copy_capture_state: ImageCopyCaptureState,
    pub screencopy_state: ScreencopyManagerState,
    pub background_effect_state: BackgroundEffectState,

    pub dnd_icon: Option<DndIcon>,

    pub pending_screenshot: bool,

    // input-related fields
    pub suppressed_keys: HashSet<Keycode>,
    pub cursor_status: CursorImageStatus,
    pub seat_name: String,
    pub seat: Seat<AnvilState<BackendData>>,
    pub clock: Clock<Monotonic>,
    pub pointer: PointerHandle<AnvilState<BackendData>>,
    pub cursor_position_hint: Option<(WlSurface, Point<f64, Logical>)>,

    #[cfg(feature = "xwayland")]
    pub xwm: Option<X11Wm>,
    #[cfg(feature = "xwayland")]
    pub xdisplay: Option<u32>,

    #[cfg(feature = "debug")]
    pub renderdoc: Option<renderdoc::RenderDoc<renderdoc::V141>>,

    pub show_window_preview: bool,

    pub config: crate::config::Config,

    /// Tiling layout engine.
    pub layout: crate::layout::Layout,

    /// Lua runtime state with callback functions for `BindAction::Callback`.
    pub lua_config: Option<Box<crate::config::LuaConfig>>,

    /// Windows currently playing their close animation.
    pub closing_windows: Vec<crate::shell::closing_window::ClosingWindow>,

    /// Active workspace index per output (keyed by output name).
    pub workspaces: std::collections::HashMap<String, u32>,

    #[cfg(feature = "xdp-gnome-screencast")]
    pub screencasting: crate::screencasting::Screencasting,
    #[cfg(feature = "xdp-gnome-screencast")]
    pub dbus: Option<crate::dbus::DBusServers>,
    #[cfg(feature = "xdp-gnome-screencast")]
    pub mutter_x11_interop_state:
        crate::protocols::mutter_x11_interop::MutterX11InteropManagerState,

    pub config_watcher: Option<ConfigWatcher>,
    pub config_reload_timer: Option<std::time::Instant>,
}

#[derive(Debug)]
pub struct DndIcon {
    pub surface: WlSurface,
    pub offset: Point<i32, Logical>,
}

impl<BackendData: Backend> DataDeviceHandler for AnvilState<BackendData> {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        return &mut self.data_device_state
    }
}

impl<BackendData: Backend> WaylandDndGrabHandler for AnvilState<BackendData> {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        self.dnd_icon = icon.map(|surface| return DndIcon {
            surface,
            offset: (0, 0).into(),
        });

        match type_ {
            GrabType::Pointer => {
                let Some(pointer) = seat.get_pointer() else {
                    return;
                };
                let Some(start_data) = pointer.grab_start_data() else {
                    return;
                };
                pointer.set_grab(
                    self,
                    DnDGrab::new_pointer(&self.display_handle, start_data, source, seat),
                    serial,
                    Focus::Keep,
                );
            }
            GrabType::Touch => {
                let Some(touch) = seat.get_touch() else {
                    return;
                };
                let Some(start_data) = touch.grab_start_data() else {
                    return;
                };
                touch.set_grab(
                    self,
                    DnDGrab::new_touch(&self.display_handle, start_data, source, seat),
                    serial,
                );
            }
        }
    }
}

impl<BackendData: Backend> DndGrabHandler for AnvilState<BackendData> {
    fn dropped(
        &mut self,
        _target: Option<DndTarget<'_, Self>>,
        _validated: bool,
        _seat: Seat<Self>,
        _location: Point<f64, Logical>,
    ) {
        self.dnd_icon = None;
    }
}

impl<BackendData: Backend> OutputHandler for AnvilState<BackendData> {}

impl<BackendData: Backend> SelectionHandler for AnvilState<BackendData> {
    type SelectionUserData = ();

    #[cfg(feature = "xwayland")]
    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        if let Some(xwm) = self.xwm.as_mut()
            && let Err(err) = xwm.new_selection(ty, source.map(|source| return source.mime_types())) {
                warn!(?err, ?ty, "Failed to set Xwayland selection");
            }
    }

    #[cfg(feature = "xwayland")]
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        if let Some(xwm) = self.xwm.as_mut()
            && let Err(err) = xwm.send_selection(ty, mime_type, fd) {
                warn!(?err, "Failed to send primary (X11 -> Wayland)");
            }
    }
}

impl<BackendData: Backend> PrimarySelectionHandler for AnvilState<BackendData> {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        return &mut self.primary_selection_state
    }
}

impl<BackendData: Backend> DataControlHandler for AnvilState<BackendData> {
    fn data_control_state(&mut self) -> &mut DataControlState {
        return &mut self.data_control_state
    }
}

impl<BackendData: Backend> ShmHandler for AnvilState<BackendData> {
    fn shm_state(&self) -> &ShmState {
        return &self.shm_state
    }
}

impl<BackendData: Backend> SeatHandler for AnvilState<BackendData> {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = PointerFocusTarget;
    type TouchFocus = PointerFocusTarget;

    fn seat_state(&mut self) -> &mut SeatState<AnvilState<BackendData>> {
        return &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, target: Option<&KeyboardFocusTarget>) {
        let dh = &self.display_handle;

        let wl_surface = target.and_then(WaylandFocus::wl_surface);

        let focus = wl_surface.and_then(|s| return dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, focus.clone());
        set_primary_focus(dh, seat, focus);

        self.update_border_focus(target);
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }

    fn led_state_changed(&mut self, _seat: &Seat<Self>, led_state: LedState) {
        self.backend_data.update_led_state(led_state)
    }
}

impl<BackendData: Backend> TabletSeatHandler for AnvilState<BackendData> {
    fn tablet_tool_image(&mut self, _tool: &TabletToolDescriptor, image: CursorImageStatus) {
        // TODO: tablet tools should have their own cursors
        self.cursor_status = image;
    }
}

impl<BackendData: Backend> InputMethodHandler for AnvilState<BackendData> {
    fn new_popup(&mut self, surface: PopupSurface) {
        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            warn!("Failed to track popup: {}", err);
        }
    }

    fn popup_repositioned(&mut self, _: PopupSurface) {}

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        if let Some(parent) = surface.get_parent().map(|parent| return parent.surface.clone()) {
            let _ = PopupManager::dismiss_popup(&parent, &PopupKind::from(surface));
        }
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, smithay::utils::Logical> {
        return self.space
            .elements()
            .find_map(|window| {
                return (window.wl_surface().as_deref() == Some(parent)).then(|| return window.geometry())
            })
            .unwrap_or_default()
    }
}

impl<BackendData: Backend> KeyboardShortcutsInhibitHandler for AnvilState<BackendData> {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        return &mut self.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // Just grant the wish for everyone
        inhibitor.activate();
    }
}

impl<BackendData: Backend> PointerConstraintsHandler for AnvilState<BackendData> {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // XXX region
        let Some(current_focus) = pointer.current_focus() else {
            return;
        };
        if current_focus.wl_surface().as_deref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    fn remove_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        if with_pointer_constraint(surface, pointer, |constraint| return constraint.is_none()) {
            if let Some((hint_surface, hint_location)) = &self.cursor_position_hint {
                let origin = self
                    .space
                    .elements()
                    .find_map(|window| {
                        return (window.wl_surface().as_deref() == Some(hint_surface))
                            .then(|| return window.geometry())
                    })
                    .unwrap_or_default()
                    .loc
                    .to_f64();

                pointer.set_location(origin + *hint_location);
            }
            self.cursor_position_hint = None;
        }
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        if with_pointer_constraint(surface, pointer, |constraint| {
            return constraint.is_some_and(|c| return c.is_active())
        }) {
            self.cursor_position_hint = Some((surface.clone(), location));
        }
    }
}

impl<BackendData: Backend> XdgActivationHandler for AnvilState<BackendData> {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        return &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        if let Some((serial, seat)) = data.serial {
            let Some(keyboard) = self.seat.get_keyboard() else {
                return false
            };
            return Seat::from_resource(&seat) == Some(self.seat.clone())
                && keyboard
                    .last_enter()
                    .map(|last_enter| return serial.is_no_older_than(&last_enter))
                    .unwrap_or(false)
        } else {
            return false
        }
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if token_data.timestamp.elapsed().as_secs() < 10 {
            // Just grant the wish
            let w = self
                .space
                .elements()
                .find(|window| return window.wl_surface().map(|s| return *s == surface).unwrap_or(false))
                .cloned();
            if let Some(window) = w {
                self.space.raise_element(&window, true);
                // Re-run the layout so the view can pan to the activated window.
                self.arrange_layout();
            }
        }
    }
}

impl<BackendData: Backend> XdgDecorationHandler for AnvilState<BackendData> {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        let mode = if self.config.window.prefer_no_csd {
            Mode::ServerSide
        } else {
            Mode::ClientSide
        };
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
        });
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        let mode = if self.config.window.prefer_no_csd {
            Mode::ServerSide
        } else {
            Mode::ClientSide
        };
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
        });

        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        use xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        let mode = if self.config.window.prefer_no_csd {
            Mode::ServerSide
        } else {
            Mode::ClientSide
        };
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
        });

        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}

impl<BackendData: Backend> FractionalScaleHandler for AnvilState<BackendData> {
    fn new_fractional_scale(
        &mut self,
        surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        // Here we can set the initial fractional scale
        //
        // First we look if the surface already has a primary scan-out output, if not
        // we test if the surface is a subsurface and try to use the primary scan-out output
        // of the root surface. If the root also has no primary scan-out output we just try
        // to use the first output of the toplevel.
        // If the surface is the root we also try to use the first output of the toplevel.
        //
        // If all the above tests do not lead to a output we just use the first output
        // of the space (which in case of anvil will also be the output a toplevel will
        // initially be placed on)
        #[allow(clippy::redundant_clone)]
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }

        with_states(&surface, |states| {
            let primary_scanout_output = surface_primary_scanout_output(&surface, states)
                .or_else(|| {
                    if root != surface {
                        return with_states(&root, |states| {
                            return surface_primary_scanout_output(&root, states).or_else(|| {
                                return self.window_for_surface(&root).and_then(|window| {
                                    return self.space.outputs_for_element(&window).first().cloned()
                                })
                            })
                        })
                    } else {
                        return self.window_for_surface(&root).and_then(|window| {
                            return self.space.outputs_for_element(&window).first().cloned()
                        })
                    }
                })
                .or_else(|| return self.space.outputs().next().cloned());
            if let Some(output) = primary_scanout_output {
                with_fractional_scale(states, |fractional_scale| {
                    fractional_scale.set_preferred_scale(output.current_scale().fractional_scale());
                });
            }
        });
    }
}

impl<BackendData: Backend + 'static> SecurityContextHandler for AnvilState<BackendData> {
    fn context_created(
        &mut self,
        source: SecurityContextListenerSource,
        security_context: SecurityContext,
    ) {
        self.handle
            .insert_source(source, move |client_stream, _, data| {
                let client_state = ClientState {
                    security_context: Some(security_context.clone()),
                    ..ClientState::default()
                };
                if let Err(err) = data
                    .display_handle
                    .insert_client(client_stream, Arc::new(client_state))
                {
                    warn!("Error adding wayland client: {}", err);
                };
            })
            .expect("Failed to init wayland socket source");
    }
}

#[cfg(feature = "xwayland")]
impl<BackendData: Backend + 'static> XWaylandKeyboardGrabHandler for AnvilState<BackendData> {
    fn keyboard_focus_for_xsurface(&self, surface: &WlSurface) -> Option<KeyboardFocusTarget> {
        let elem = self
            .space
            .elements()
            .find(|elem| return elem.wl_surface().as_deref() == Some(surface))?;
        return Some(KeyboardFocusTarget::Window(elem.0.clone()))
    }
}

impl<BackendData: Backend> XdgForeignHandler for AnvilState<BackendData> {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        return &mut self.xdg_foreign_state
    }
}

impl<BackendData: Backend + 'static> ExtBackgroundEffectHandler for AnvilState<BackendData> {
    fn capabilities(&self) -> background_effect::Capability {
        return background_effect::Capability::Blur
    }
}

impl<BackendData: Backend> ImageCaptureSourceHandler for AnvilState<BackendData> {
    fn source_destroyed(&mut self, _source: ImageCaptureSource) {
        // Anvil doesn't track sources
    }
}

impl<BackendData: Backend> OutputCaptureSourceHandler for AnvilState<BackendData> {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        return &mut self.output_capture_source_state
    }

    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        source.user_data().insert_if_missing(|| return output.downgrade());
    }
}

impl<BackendData: Backend> ImageCopyCaptureHandler for AnvilState<BackendData> {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        return &mut self.image_copy_capture_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        use smithay::output::WeakOutput;
        let weak_output = source.user_data().get::<WeakOutput>()?;
        let output = weak_output.upgrade()?;
        let mode = output.current_mode()?;

        return Some(BufferConstraints {
            size: mode
                .size
                .to_logical(1)
                .to_buffer(1, smithay::utils::Transform::Normal),
            shm: vec![
                smithay::reexports::wayland_server::protocol::wl_shm::Format::Argb8888,
                smithay::reexports::wayland_server::protocol::wl_shm::Format::Xrgb8888,
            ],
            #[cfg(any(feature = "udev", feature = "winit", feature = "x11"))]
            dma: None,
        })
    }

    fn new_session(&mut self, _session: Session) {
        // Anvil doesn't track sessions; they clean up on drop
    }

    fn frame(&mut self, _session: &SessionRef, frame: Frame) {
        self.pending_screenshot = true;
        frame.fail(smithay::wayland::image_copy_capture::CaptureFailureReason::Unknown);
    }
}

impl<BackendData: Backend> ScreencopyHandler for AnvilState<BackendData> {
    fn screencopy_state(&mut self) -> &mut ScreencopyManagerState {
        return &mut self.screencopy_state
    }

    fn frame(
        &mut self,
        _manager: &smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
        _screencopy: Screencopy,
    ) {
        self.pending_screenshot = true;
    }
}

delegate_dispatch2!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

crate::delegate_screencopy!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

#[cfg(feature = "xdp-gnome-screencast")]
impl<BackendData: Backend + 'static> crate::protocols::mutter_x11_interop::MutterX11InteropHandler
    for AnvilState<BackendData>
{
}
#[cfg(feature = "xdp-gnome-screencast")]
crate::delegate_mutter_x11_interop!(@<BackendData: Backend + 'static> AnvilState<BackendData>);

impl<BackendData: Backend + 'static> AnvilState<BackendData> {
    pub fn init(
        display: Display<AnvilState<BackendData>>,
        handle: LoopHandle<'static, AnvilState<BackendData>>,
        backend_data: BackendData,
        listen_on_socket: bool,
    ) -> AnvilState<BackendData> {
        let dh = display.handle();

        let clock = Clock::new();

        let mut config = crate::config::load_config();
        let lua_config = config.lua_config.take();
        for (k, v) in &config.env {
            unsafe {
                std::env::set_var(k, v);
            }
        }

        // init wayland clients
        let socket_name = if listen_on_socket {
            let source = ListeningSocketSource::new_auto().unwrap();
            let socket_name = source.socket_name().to_string_lossy().into_owned();
            handle
                .insert_source(source, |client_stream, _, data| {
                    if let Err(err) = data
                        .display_handle
                        .insert_client(client_stream, Arc::new(ClientState::default()))
                    {
                        warn!("Error adding wayland client: {}", err);
                    };
                })
                .expect("Failed to init wayland socket source");
            info!(name = socket_name, "Listening on wayland socket");
            Some(socket_name)
        } else {
            None
        };
        handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, data| {
                    profiling::scope!("dispatch_clients");
                    // Safety: we don't drop the display
                    unsafe {
                        display.get_mut().dispatch_clients(data).unwrap();
                    }
                    return Ok(PostAction::Continue)
                },
            )
            .expect("Failed to init wayland server source");

        // init globals
        let compositor_state = CompositorState::new::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let data_control_state =
            DataControlState::new::<Self, _>(&dh, Some(&primary_selection_state), |_| return true);
        let mut seat_state = SeatState::new();
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let xdg_activation_state = XdgActivationState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let presentation_state = PresentationState::new::<Self>(&dh, clock.id() as u32);
        let fractional_scale_manager_state = FractionalScaleManagerState::new::<Self>(&dh);
        let xdg_foreign_state = XdgForeignState::new::<Self>(&dh);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&dh);
        let fifo_manager_state = FifoManagerState::new::<Self>(&dh);
        let commit_timing_manager_state = CommitTimingManagerState::new::<Self>(&dh);
        TextInputManagerState::new::<Self>(&dh);
        InputMethodManagerState::new::<Self, _>(&dh, |_client| return true);
        VirtualKeyboardManagerState::new::<Self, _>(&dh, |_client| return true);
        // Expose global only if backend supports relative motion events
        if BackendData::HAS_RELATIVE_MOTION {
            RelativePointerManagerState::new::<Self>(&dh);
        }
        PointerConstraintsState::new::<Self>(&dh);
        if BackendData::HAS_GESTURES {
            PointerGesturesState::new::<Self>(&dh);
        }
        TabletManagerState::new::<Self>(&dh);
        SecurityContextState::new::<Self, _>(&dh, |client| {
            return client
                .get_data::<ClientState>()
                .is_none_or(|client_state| return client_state.security_context.is_none())
        });
        FixesState::new::<Self>(&dh);
        let background_effect_state = BackgroundEffectState::new::<Self>(&dh);

        // Image capture protocols (screencopy)
        let image_capture_source_state = ImageCaptureSourceState::new();
        let output_capture_source_state = OutputCaptureSourceState::new::<Self>(&dh);
        let image_copy_capture_state = ImageCopyCaptureState::new::<Self>(&dh);
        let screencopy_state = ScreencopyManagerState::new::<Self, _>(&dh, |_| return true);

        #[cfg(feature = "xdp-gnome-screencast")]
        let mutter_x11_interop_state =
            crate::protocols::mutter_x11_interop::MutterX11InteropManagerState::new::<Self, _>(
                &dh,
                move |_| return true,
            );

        // init input
        let seat_name = backend_data.seat_name();
        let mut seat = seat_state.new_wl_seat(&dh, seat_name.clone());

        let pointer = seat.add_pointer();
        seat.add_keyboard(XkbConfig::default(), 200, 25)
            .expect("Failed to initialize the keyboard");

        let keyboard_shortcuts_inhibit_state = KeyboardShortcutsInhibitState::new::<Self>(&dh);

        #[cfg(feature = "xwayland")]
        let xwayland_shell_state = xwayland_shell::XWaylandShellState::new::<Self>(&dh.clone());

        #[cfg(feature = "xwayland")]
        XWaylandKeyboardGrabState::new::<Self>(&dh.clone());

        return AnvilState {
            backend_data,
            display_handle: dh,
            socket_name,
            running: Arc::new(AtomicBool::new(true)),
            handle,
            space: Space::default(),
            popups: PopupManager::default(),
            compositor_state,
            data_device_state,
            layer_shell_state,
            output_manager_state,
            primary_selection_state,
            data_control_state,
            seat_state,
            keyboard_shortcuts_inhibit_state,
            shm_state,
            viewporter_state,
            xdg_activation_state,
            xdg_decoration_state,
            xdg_shell_state,
            presentation_state,
            fractional_scale_manager_state,
            xdg_foreign_state,
            single_pixel_buffer_state,
            fifo_manager_state,
            commit_timing_manager_state,
            image_capture_source_state,
            output_capture_source_state,
            image_copy_capture_state,
            screencopy_state,
            background_effect_state,
            dnd_icon: None,
            pending_screenshot: false,
            suppressed_keys: HashSet::new(),
            cursor_status: CursorImageStatus::default_named(),
            seat_name,
            seat,
            pointer,
            cursor_position_hint: None,
            clock,

            #[cfg(feature = "xwayland")]
            xwayland_shell_state,
            #[cfg(feature = "xwayland")]
            xwm: None,
            #[cfg(feature = "xwayland")]
            xdisplay: None,
            #[cfg(feature = "debug")]
            renderdoc: renderdoc::RenderDoc::new().ok(),
            show_window_preview: false,
            config,
            layout: crate::layout::Layout::default(),
            lua_config,
            closing_windows: Vec::new(),
            workspaces: std::collections::HashMap::new(),
            #[cfg(feature = "xdp-gnome-screencast")]
            screencasting: crate::screencasting::Screencasting::new_stub(),
            #[cfg(feature = "xdp-gnome-screencast")]
            dbus: None,
            #[cfg(feature = "xdp-gnome-screencast")]
            mutter_x11_interop_state,
            config_watcher: None,
            config_reload_timer: None,
        }
    }

    #[cfg(feature = "xwayland")]
    pub fn start_xwayland(&mut self) {
        use std::process::Stdio;

        use smithay::wayland::compositor::CompositorHandler;

        let (xwayland, client) = XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            std::iter::empty::<String>(),
            true,
            Stdio::null(),
            Stdio::null(),
            |_| (),
        )
        .expect("failed to start XWayland");

        let display_handle = self.display_handle.clone();
        let ret = self
            .handle
            .insert_source(xwayland, move |event, _, data| match event {
                XWaylandEvent::Ready {
                    x11_socket,
                    display_number,
                } => {
                    let xwayland_scale = std::env::var("ANVIL_XWAYLAND_SCALE")
                        .ok()
                        .and_then(|s| return s.parse::<f64>().ok())
                        .unwrap_or(1.);
                    data.client_compositor_state(&client)
                        .set_client_scale(xwayland_scale);
                    let mut wm = X11Wm::start_wm(
                        data.handle.clone(),
                        &display_handle,
                        x11_socket,
                        client.clone(),
                    )
                    .expect("Failed to attach X11 Window Manager");

                    let cursor_theme = data.config.cursor.theme.as_deref();
                    let cursor_size = data.config.cursor.size;
                    let cursor = Cursor::load_with_config(cursor_theme, cursor_size);
                    let image = cursor.get_image(1, Duration::ZERO);
                    wm.set_cursor(
                        &image.pixels_rgba,
                        Size::from((image.width as u16, image.height as u16)),
                        Point::from((image.xhot as u16, image.yhot as u16)),
                    )
                    .expect("Failed to set xwayland default cursor");
                    data.xwm = Some(wm);
                    data.xdisplay = Some(display_number);
                }
                XWaylandEvent::Error => {
                    warn!("XWayland crashed on startup");
                }
            });
        if let Err(e) = ret {
            tracing::error!(
                "Failed to insert the XWaylandSource into the event loop: {}",
                e
            );
        }
    }

    pub fn run_init_commands(&self) {
        use std::process::Command;

        let env_iter = self
            .socket_name
            .clone()
            .map(|v| return ("WAYLAND_DISPLAY", v))
            .into_iter()
            .chain(
                #[cfg(feature = "xwayland")]
                self.xdisplay.map(|v| return ("DISPLAY", format!(":{v}"))),
                #[cfg(not(feature = "xwayland"))]
                None::<(String, String)>,
            );

        let env_pairs: Vec<(String, String)> = env_iter.map(|(k, v)| return (k.to_string(), v)).collect();

        for cmd in &self.config.init_commands {
            info!(cmd, "Running init command");
            if let Err(e) = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .envs(env_pairs.iter().map(|(k, v)| return (k.as_str(), v.as_str())))
                .spawn()
            {
                warn!(cmd, err = %e, "Failed to run init command");
            }
        }

        for code in &self.config.init_shell_commands {
            info!(code, "Running init shell command");
            if let Err(e) = Command::new("sh")
                .arg("-c")
                .arg(code)
                .envs(env_pairs.iter().map(|(k, v)| return (k.as_str(), v.as_str())))
                .spawn()
            {
                warn!(code, err = %e, "Failed to run init shell command");
            }
        }
    }

    pub fn reload_config(&mut self) {
        let old_config = self.config.clone();
        self.config = crate::config::reload_config(&old_config);

        // Preserve the LuaConfig from the new config into AnvilState
        self.lua_config = self.config.lua_config.take();

        if self.config.cursor.theme != old_config.cursor.theme
            || self.config.cursor.size != old_config.cursor.size
        {
            let cursor_theme = self.config.cursor.theme.as_deref();
            let cursor_size = self.config.cursor.size;
            self.backend_data.reload_cursor(cursor_theme, cursor_size);
        }

        // Apply window config (including window-rule overrides) to all windows
        self.space.elements().for_each(|window| {
            window.apply_config(&self.config);
        });

        // Re-tile windows if the layout config changed.
        self.arrange_layout();
    }

    pub fn update_border_focus(&mut self, target: Option<&KeyboardFocusTarget>) {
        let focused_surface = target.and_then(|t| {
            if let crate::focus::KeyboardFocusTarget::Window(w) = t {
                return w.wl_surface()
            } else {
                return None
            }
        });

        let border_width = self.config.window.border.width;

        self.space.elements().for_each(|window| {
            let is_focused = focused_surface
                .as_ref()
                .is_some_and(|fs| return window.wl_surface().is_some_and(|ws| return fs == &ws));
            let geo = smithay::desktop::space::SpaceElement::geometry(&window.0);
            let mut ws = window.decoration_state();
            // Skip fully-hidden (inactive workspace) windows.
            if ws.hidden && ws.fade_anim.is_none() {
                return;
            }
            ws.border
                .set_active(is_focused, geo.size.w, geo.size.h, border_width);
        });
    }

    /// Check if there are any active animations (open, close, or layout).
    pub fn has_active_animations(&self) -> bool {
        if !self.closing_windows.is_empty() {
            return true;
        }
        // Check for open animations
        for window in self.space.elements() {
            let state = window.decoration_state();
            if state.open_animation.is_some() {
                return true;
            }
            if state
                .fade_anim
                .as_ref()
                .is_some_and(|anim| return !anim.is_done())
            {
                return true;
            }
            if state
                .layout
                .move_anim
                .as_ref()
                .is_some_and(|(_, anim)| return !anim.is_done())
            {
                return true;
            }
        }
        return false
    }

    /// Start close animation for a window.
    ///
    /// This captures the window's texture snapshot and starts the fade-out animation.
    /// Returns true if animation was started, false if disabled.
    pub fn start_close_animation(
        &mut self,
        window: &WindowElement,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        output: &Output,
    ) -> bool {
        return Self::start_close_animation_inner(
            &mut self.closing_windows,
            &self.space,
            &self.config,
            window,
            renderer,
            output,
        )
    }

    /// Inner implementation of start_close_animation that takes specific fields
    /// to avoid conflicting borrows on self.
    pub(crate) fn start_close_animation_inner(
        closing_windows: &mut Vec<crate::shell::closing_window::ClosingWindow>,
        space: &Space<crate::shell::WindowElement>,
        config: &crate::config::Config,
        window: &WindowElement,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        output: &Output,
    ) -> bool {
        let animations_config = &config.animations;
        if !animations_config.enable || !animations_config.window_close.enable {
            tracing::debug!("close animation disabled, skipping");
            return false;
        }

        // Capture the snapshot first
        let snapshot =
            match Self::capture_close_snapshot(space, config, window, renderer, Some(output)) {
                Some(s) => s,
                None => return false,
            };

        // Then start animation from snapshot
        return Self::start_close_animation_from_snapshot(closing_windows, config, snapshot)
    }

    /// Capture a window's contents as a texture snapshot for close animation.
    ///
    /// This should be called while the window surface still has a valid buffer
    /// (e.g. in a pre-commit hook when BufferAssignment::Removed is detected).
    /// Returns the PendingCloseSnapshot if capture succeeded.
    pub(crate) fn capture_close_snapshot(
        space: &Space<crate::shell::WindowElement>,
        _config: &crate::config::Config,
        window: &WindowElement,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        output: Option<&Output>,
    ) -> Option<crate::shell::ssd::PendingCloseSnapshot> {
        use crate::render_helpers::texture::TextureBuffer;
        use smithay::backend::allocator::Fourcc;
        use smithay::backend::renderer::element::{AsRenderElements, Element, RenderElement};
        use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
        use smithay::backend::renderer::{Bind, Frame, Offscreen, Renderer};

        // Get window geometry and position
        let win_geo = match space.element_geometry(window) {
            Some(geo) => geo,
            None => {
                tracing::warn!("close animation: window not found in space, skipping");
                return None;
            }
        };
        let geo_size = win_geo.size.to_f64();

        // Resolve output: use provided or find from space (clone to avoid lifetime issues)
        let output_cloned;
        let output = match output {
            Some(o) => o,
            None => {
                output_cloned = space.outputs_for_element(window).first().cloned();
                match output_cloned.as_ref() {
                    Some(o) => o,
                    None => {
                        tracing::warn!("close animation: no output for window");
                        return None;
                    }
                }
            }
        };

        let output_scale = output.current_scale().fractional_scale();
        let scale = smithay::utils::Scale::from(output_scale);

        // Render the window elements to a texture
        let location = win_geo.loc.to_physical_precise_round(output_scale);
        let output_geo = space.output_geometry(output)?;

        let window_elements: Vec<WindowRenderElement> =
            AsRenderElements::<GlesRenderer>::render_elements(
                window,
                renderer,
                location - output_geo.loc.to_physical_precise_round(output_scale),
                scale,
                1.0,
            );

        // Check if we actually captured any content
        if window_elements.is_empty() {
            tracing::debug!("close animation: no window elements to capture, skipping");
            return None;
        }

        // Compute encompassing geometry for the elements
        let encompassing_geo = window_elements
            .iter()
            .map(|e| return smithay::backend::renderer::element::Element::geometry(e, scale))
            .reduce(|a, b| return a.merge(b))
            .unwrap_or_else(|| {
                return smithay::utils::Rectangle::from_size(
                    (win_geo.size.w as i32, win_geo.size.h as i32).into(),
                )
            });

        // If the encompassing geometry has zero area, the window content is gone
        if encompassing_geo.size.w == 0 || encompassing_geo.size.h == 0 {
            tracing::debug!("close animation: window content is empty (zero size), skipping");
            return None;
        }

        let buffer_size = encompassing_geo.size;
        let transform = smithay::utils::Transform::Normal;

        // Render to offscreen texture
        let buffer_size_for_create = buffer_size.to_logical(1).to_buffer(1, transform);
        let texture: GlesTexture = match <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            buffer_size_for_create,
        ) {
            Ok(t) => t,
            Err(e) => {
                warn!(
                    "Failed to create offscreen texture for close animation: {:?}",
                    e
                );
                return None;
            }
        };

        // We need to clone the texture for the TextureBuffer since bind() borrows it
        let texture_clone = texture.clone();
        let mut texture_mut = texture;

        let mut target = match <GlesRenderer as Bind<GlesTexture>>::bind(renderer, &mut texture_mut)
        {
            Ok(t) => t,
            Err(e) => {
                warn!(
                    "Failed to bind offscreen texture for close animation: {:?}",
                    e
                );
                return None;
            }
        };

        let output_transform = transform.invert();
        let output_rect =
            smithay::utils::Rectangle::from_size(output_transform.transform_size(buffer_size));

        let mut frame = match renderer.render(&mut target, buffer_size, output_transform) {
            Ok(f) => f,
            Err(e) => {
                warn!("Failed to start render frame for close animation: {:?}", e);
                return None;
            }
        };

        if let Err(e) = frame.clear(
            smithay::backend::renderer::Color32F::TRANSPARENT,
            &[output_rect],
        ) {
            warn!("Failed to clear frame for close animation: {:?}", e);
            return None;
        }

        let encompassing_loc = encompassing_geo.loc;

        for element in &window_elements {
            let geo = Element::geometry(element, scale);
            let src = Element::src(element);
            // Offset the element geometry so it's relative to the texture's top-left
            // corner (encompassing_geo.loc) rather than the output origin.
            // Without this offset, elements positioned far from the output origin
            // would be clipped by the texture boundary.
            let dst = Rectangle::new(geo.loc - encompassing_loc, geo.size);
            if let Err(e) = RenderElement::<GlesRenderer>::draw(
                element,
                &mut frame,
                src,
                dst,
                &[dst],
                &[],
                None,
            ) {
                warn!("Failed to draw element for close animation: {:?}", e);
            }
        }

        let _sync_point = match frame.finish() {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to finish frame for close animation: {:?}", e);
                return None;
            }
        };

        // Drop target and texture_mut to release the borrow
        drop(target);
        drop(texture_mut);

        let buffer =
            TextureBuffer::from_texture(renderer, texture_clone, scale, transform, Vec::new());

        // Position of the window content relative to the output, in logical coords.
        // This is where the closing animation will render the snapshot.
        let pos = encompassing_geo.loc.to_f64().to_logical(scale);

        // Buffer offset is zero since the texture content starts at (0,0) in the
        // offscreen texture, and we've already accounted for the element positions
        // when rendering into the texture.
        let buffer_offset = Point::from((0., 0.));

        return Some(crate::shell::ssd::PendingCloseSnapshot {
            buffer,
            geo_size,
            pos,
            buffer_offset,
        })
    }

    /// Start close animation from a pre-captured snapshot.
    ///
    /// Called in `toplevel_destroyed` with the snapshot captured in the pre-commit hook.
    pub(crate) fn start_close_animation_from_snapshot(
        closing_windows: &mut Vec<crate::shell::closing_window::ClosingWindow>,
        config: &crate::config::Config,
        snapshot: crate::shell::ssd::PendingCloseSnapshot,
    ) -> bool {
        use crate::animation::Animation;
        use crate::shell::closing_window::ClosingWindow;

        let close_config = &config.animations.window_close;

        // Create the close animation
        let anim = Animation::ease(
            0.0, // from: progress 0 (visible)
            1.0, // to: progress 1 (gone)
            close_config.duration_ms,
            close_config.curve.to_curve(),
        );

        let end_scale = close_config.scale;
        let closing = ClosingWindow::new(
            snapshot.buffer,
            snapshot.geo_size,
            snapshot.pos,
            snapshot.buffer_offset,
            anim,
            end_scale,
        );
        closing_windows.push(closing);

        return true
    }

    /// Remove closing windows whose animations have finished.
    pub fn cleanup_finished_close_animations(&mut self) {
        self.closing_windows
            .retain(|closing| return closing.is_animating());
    }

    /// Request that a window be closed.
    ///
    /// This sends the Wayland close event to the client. When the client
    /// destroys the toplevel, `toplevel_destroyed` will capture a snapshot
    /// and start the close animation.
    pub fn queue_close_animation(&mut self, window: &WindowElement) {
        use smithay::desktop::WindowSurface;
        match window.0.underlying_surface() {
            WindowSurface::Wayland(w) => w.send_close(),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(w) => {
                let _ = w.close();
            }
        }
    }

    #[cfg(feature = "xdp-gnome-screencast")]
    pub fn on_introspect_msg(
        &mut self,
        to_introspect: &async_channel::Sender<StateToIntrospect>,
        msg: IntrospectToState,
    ) {
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

        let IntrospectToState::GetWindows = msg;

        let mut windows = HashMap::new();

        for (idx, window) in self.space.elements().enumerate() {
            if let Some(toplevel) = window.0.toplevel() {
                let (title, app_id) = with_states(toplevel.wl_surface(), |states| {
                    let role = states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .unwrap()
                        .lock()
                        .unwrap();
                    let title = role.title.clone().unwrap_or_default();
                    let app_id = role
                        .app_id
                        .as_ref()
                        .map(|id| format!("{id}.desktop"))
                        .unwrap_or_default();
                    return (title, app_id)
                });

                let id = (idx as u64) + 1;
                windows.insert(id, WindowProperties { title, app_id });
            }
        }

        let msg = StateToIntrospect::Windows(windows);
        if let Err(err) = to_introspect.send_blocking(msg) {
            warn!("error sending windows to introspect: {err:?}");
        }
    }

    // ── Workspaces ──────────────────────────────────────────────

    /// Active workspace index for the given output.
    pub fn active_workspace(&self, output: &Output) -> u32 {
        return self.workspaces.get(&output.name()).copied().unwrap_or(0)
    }

    /// The currently focused window element, if any.
    pub fn focused_window(&self) -> Option<WindowElement> {
        let keyboard = self.seat.get_keyboard()?;
        match keyboard.current_focus() {
            Some(crate::focus::KeyboardFocusTarget::Window(window)) => return self
                .space
                .elements()
                .find(|we| return we.0 == window)
                .cloned(),
            _ => return None,
        }
    }

    /// The output the focused window lives on, else the pointer output, else the
    /// first output.
    pub fn focused_output(&self) -> Option<Output> {
        if let Some(window) = self.focused_window()
            && let Some(geo) = self.space.element_geometry(&window) {
                for output in self.space.outputs() {
                    if self
                        .space
                        .output_geometry(output)
                        .map(|g| return g.intersection(geo).is_some())
                        .unwrap_or(false)
                    {
                        return Some(output.clone());
                    }
                }
            }
        return self.space
            .output_under(self.pointer.current_location())
            .next()
            .cloned()
            .or_else(|| return self.space.outputs().next().cloned())
    }

    /// Switch the active workspace of `output` to `target`, fading windows in/out.
    /// Workspaces extend infinitely downward (any `target` index is valid).
    pub fn switch_workspace(&mut self, output: &Output, target: u32) {
        let key = output.name();
        let old = self.active_workspace(output);
        if old == target {
            return;
        }
        self.workspaces.insert(key, target);

        let ws_anim = self.config.animations.workspace_switch;
        let animated = self.config.animations.enable && ws_anim.enable;
        let fade_of = |from: f64, to: f64| {
            if animated {
                return crate::animation::Animation::ease(from, to, ws_anim.duration_ms, ws_anim.curve.to_curve())
            } else {
                return crate::animation::Animation::new_off()
            }
        };

        let windows: Vec<WindowElement> =
            self.space.elements_for_output(output).cloned().collect();
        for window in windows {
            let mut st = window.decoration_state();
            if st.workspace == target {
                st.hidden = false;
                st.fade_anim = Some(fade_of(0.0, 1.0));
            } else {
                st.hidden = true;
                st.fade_anim = Some(fade_of(1.0, 0.0));
            }
        }

        // Raise the topmost visible window so the layout treats it as focused.
        let topmost = self
            .space
            .elements_for_output(output)
            .find(|w| {
                let st = w.decoration_state();
                return !st.hidden && st.workspace == target
            })
            .cloned();
        if let Some(window) = &topmost {
            self.space.raise_element(window, true);
            #[cfg(feature = "xwayland")]
            if let (Some(surface), Some(xwm)) = (window.0.x11_surface(), self.xwm.as_mut()) {
                let _ = xwm.raise_window(surface);
            }
        }

        self.arrange_layout();

        if let Some(keyboard) = self.seat.get_keyboard() {
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            keyboard.set_focus(self, topmost.map(|w| return w.into()), serial);
        }
    }

    /// Focus the given window: raise it, give it keyboard focus, and re-pan the
    /// layout so focus-following layouts scroll to it.
    pub fn focus_window(&mut self, window: &WindowElement) {
        self.space.raise_element(window, true);
        #[cfg(feature = "xwayland")]
        if let (Some(surface), Some(xwm)) = (window.0.x11_surface(), self.xwm.as_mut()) {
            let _ = xwm.raise_window(surface);
        }
        if let Some(keyboard) = self.seat.get_keyboard() {
            let serial = smithay::utils::SERIAL_COUNTER.next_serial();
            keyboard.set_focus(self, Some(window.clone().into()), serial);
        }
        self.arrange_layout();
    }

    /// Cycle keyboard focus through the visible tiled windows on the focused
    /// output (`direction` = ±1, wrapping around).
    pub fn focus_cycle(&mut self, direction: i32) {
        let Some(output) = self.focused_output() else {
            return;
        };
        let ws = self.active_workspace(&output);
        let windows: Vec<WindowElement> = self
            .space
            .elements_for_output(&output)
            .filter(|w| {
                let st = w.decoration_state();
                return !st.hidden && st.workspace == ws && !st.layout.is_floating
            })
            .cloned()
            .collect();
        if windows.is_empty() {
            return;
        }
        let current = self.focused_window();
        let idx = current
            .and_then(|c| return windows.iter().position(|w| return *w == c))
            .unwrap_or(0);
        let next = (idx as i32 + direction).rem_euclid(windows.len() as i32) as usize;
        self.focus_window(&windows[next]);
    }

    /// Grow/shrink the focused window's column width by `delta` logical px.
    /// The layout honors the override for built-in `columns` and Lua custom
    /// layouts (exposed to Lua as `win.width`). Size changes animate via the
    /// layout move/resize animations.
    pub fn resize_width(&mut self, delta: f64) {
        let Some(window) = self.focused_window() else {
            return;
        };
        {
            let mut st = window.decoration_state();
            let lws = &mut st.layout;
            let base = lws.width_override.unwrap_or_else(|| {
                return self.space
                    .element_geometry(&window)
                    .map(|g| return g.size.w as f64)
                    .unwrap_or(0.)
            });
            lws.width_override = Some((base + delta).clamp(100., 4000.));
        }
        self.arrange_layout();
    }

    /// Toggle windowed fullscreen: the focused window fills the whole work area,
    /// covering the rest of the layout (other windows stay where they are).
    pub fn toggle_maximize(&mut self) {
        let Some(window) = self.focused_window() else {
            return;
        };
        {
            let mut st = window.decoration_state();
            st.layout.full_width = !st.layout.full_width;
        }
        self.arrange_layout();
    }

    /// Toggle true fullscreen (XDG/X11 fullscreen state) for the focused window.
    pub fn toggle_fullscreen(&mut self) {
        use smithay::desktop::WindowSurface;
        use smithay::wayland::shell::xdg::XdgShellHandler;

        let Some(window) = self.focused_window() else {
            return;
        };
        let is_fullscreen = self.space.outputs().any(|o| {
            return o.user_data()
                .get::<crate::shell::FullscreenSurface>()
                .and_then(|f| return f.get())
                .map(|w| return w == window)
                .unwrap_or(false)
        });
        match window.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                if is_fullscreen {
                    self.unfullscreen_request(toplevel.clone());
                } else {
                    self.fullscreen_request(toplevel.clone(), None);
                }
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(surface) => {
                if is_fullscreen {
                    self.unfullscreen_request_x11(surface);
                } else {
                    self.fullscreen_request_x11(surface);
                }
            }
        }
        self.arrange_layout();
    }
}

impl<BackendData: Backend + 'static> AnvilState<BackendData> {
    /// Signal commit-timing barriers up to `frame_target` on every surface of the
    /// given tree and collect the owning clients for a later `blocker_cleared`.
    fn signal_commit_timers(
        surface: &WlSurface,
        states: &smithay::wayland::compositor::SurfaceData,
        frame_target: Time<Monotonic>,
        clients: &mut HashMap<ClientId, Client>,
    ) {
        if let Some(mut commit_timer_state) = states
            .data_map
            .get::<CommitTimerBarrierStateUserData>()
            .map(|commit_timer| return commit_timer.lock().unwrap())
        {
            commit_timer_state.signal_until(frame_target);
            if let Some(client) = surface.client() {
                clients.insert(client.id(), client);
            }
        }
        return
    }

    /// Common per-frame work for all surfaces of `element`: signal commit-timing
    /// barriers, update fractional scale and signal FIFO barriers.
    fn with_output_surface_state(
        surface: &WlSurface,
        states: &smithay::wayland::compositor::SurfaceData,
        output: &Output,
        clients: &mut HashMap<ClientId, Client>,
    ) {
        let primary_scanout_output = surface_primary_scanout_output(surface, states);

        if let Some(output) = primary_scanout_output.as_ref() {
            with_fractional_scale(states, |fraction_scale| {
                fraction_scale.set_preferred_scale(output.current_scale().fractional_scale());
            });
        }

        if primary_scanout_output
            .as_ref()
            .map(|o| return o == output)
            .unwrap_or(true)
        {
            let fifo_barrier = states
                .cached_state
                .get::<FifoBarrierCachedState>()
                .current()
                .barrier
                .take();

            if let Some(fifo_barrier) = fifo_barrier {
                fifo_barrier.signal();
                if let Some(client) = surface.client() {
                    clients.insert(client.id(), client);
                }
            }
        }
    }

    pub fn pre_repaint(&mut self, output: &Output, frame_target: impl Into<Time<Monotonic>>) {
        let frame_target = frame_target.into();

        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();
        self.space.elements_for_output(output).for_each(|window| {
            window.with_surfaces(|surface, states| {
                Self::signal_commit_timers(surface, states, frame_target, &mut clients);
            });
        });

        let map = smithay::desktop::layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.with_surfaces(|surface, states| {
                Self::signal_commit_timers(surface, states, frame_target, &mut clients);
            });
        }
        // Drop the lock to the layer map before calling blocker_cleared, which might end up
        // calling the commit handler which in turn again could access the layer map.
        std::mem::drop(map);

        if let CursorImageStatus::Surface(ref surface) = self.cursor_status {
            with_surfaces_surface_tree(surface, |surface, states| {
                Self::signal_commit_timers(surface, states, frame_target, &mut clients);
            });
        }

        if let Some(surface) = self.dnd_icon.as_ref().map(|icon| return &icon.surface) {
            with_surfaces_surface_tree(surface, |surface, states| {
                Self::signal_commit_timers(surface, states, frame_target, &mut clients);
            });
        }

        let dh = self.display_handle.clone();
        for client in clients.into_values() {
            self.client_compositor_state(&client)
                .blocker_cleared(self, &dh);
        }
    }

    pub fn post_repaint(
        &mut self,
        output: &Output,
        time: impl Into<Duration>,
        dmabuf_feedback: Option<SurfaceDmabufFeedback>,
        render_element_states: &RenderElementStates,
    ) {
        let time = time.into();
        let throttle = Some(Duration::from_secs(1));

        #[allow(clippy::mutable_key_type)]
        let mut clients: HashMap<ClientId, Client> = HashMap::new();

        self.space.elements_for_output(output).for_each(|window| {
            window.with_surfaces(|surface, states| {
                Self::with_output_surface_state(surface, states, output, &mut clients);
            });

            window.send_frame(output, time, throttle, surface_primary_scanout_output);
            if let Some(dmabuf_feedback) = dmabuf_feedback.as_ref() {
                window.send_dmabuf_feedback(
                    output,
                    surface_primary_scanout_output,
                    |surface, _| {
                        return select_dmabuf_feedback(
                            surface,
                            render_element_states,
                            &dmabuf_feedback.render_feedback,
                            &dmabuf_feedback.scanout_feedback,
                        )
                    },
                );
            }
        });
        let map = smithay::desktop::layer_map_for_output(output);
        for layer_surface in map.layers() {
            layer_surface.with_surfaces(|surface, states| {
                Self::with_output_surface_state(surface, states, output, &mut clients);
            });

            layer_surface.send_frame(output, time, throttle, surface_primary_scanout_output);
            if let Some(dmabuf_feedback) = dmabuf_feedback.as_ref() {
                layer_surface.send_dmabuf_feedback(
                    output,
                    surface_primary_scanout_output,
                    |surface, _| {
                        return select_dmabuf_feedback(
                            surface,
                            render_element_states,
                            &dmabuf_feedback.render_feedback,
                            &dmabuf_feedback.scanout_feedback,
                        )
                    },
                );
            }
        }
        // Drop the lock to the layer map before calling blocker_cleared, which might end up
        // calling the commit handler which in turn again could access the layer map.
        std::mem::drop(map);

        if let CursorImageStatus::Surface(ref surface) = self.cursor_status {
            with_surfaces_surface_tree(surface, |surface, states| {
                Self::with_output_surface_state(surface, states, output, &mut clients);
            });
        }

        if let Some(surface) = self.dnd_icon.as_ref().map(|icon| return &icon.surface) {
            with_surfaces_surface_tree(surface, |surface, states| {
                Self::with_output_surface_state(surface, states, output, &mut clients);
            });
        }

        let dh = self.display_handle.clone();
        for client in clients.into_values() {
            self.client_compositor_state(&client)
                .blocker_cleared(self, &dh);
        }
    }
}

pub fn update_primary_scanout_output(
    space: &Space<WindowElement>,
    output: &Output,
    dnd_icon: &Option<DndIcon>,
    cursor_status: &CursorImageStatus,
    render_element_states: &RenderElementStates,
) {
    space.elements_for_output(output).for_each(|window| {
        window.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                None,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    });
    let map = smithay::desktop::layer_map_for_output(output);
    for layer_surface in map.layers() {
        layer_surface.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                None,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }

    if let CursorImageStatus::Surface(surface) = cursor_status {
        with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                None,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }

    if let Some(surface) = dnd_icon.as_ref().map(|icon| return &icon.surface) {
        with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                None,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }
}

#[derive(Debug, Clone)]
pub struct SurfaceDmabufFeedback {
    pub render_feedback: DmabufFeedback,
    pub scanout_feedback: DmabufFeedback,
}

#[profiling::function]
pub fn take_presentation_feedback(
    output: &Output,
    space: &Space<WindowElement>,
    render_element_states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut output_presentation_feedback = OutputPresentationFeedback::new(output);

    space.elements_for_output(output).for_each(|window| {
        window.take_presentation_feedback(
            &mut output_presentation_feedback,
            surface_primary_scanout_output,
            |surface, _| {
                return surface_presentation_feedback_flags_from_states(
                    surface,
                    None,
                    render_element_states,
                )
            },
        );
    });
    let map = smithay::desktop::layer_map_for_output(output);
    for layer_surface in map.layers() {
        layer_surface.take_presentation_feedback(
            &mut output_presentation_feedback,
            surface_primary_scanout_output,
            |surface, _| {
                return surface_presentation_feedback_flags_from_states(
                    surface,
                    None,
                    render_element_states,
                )
            },
        );
    }

    output_presentation_feedback
}

pub trait Backend {
    const HAS_RELATIVE_MOTION: bool = false;
    const HAS_GESTURES: bool = false;
    fn seat_name(&self) -> String;
    fn reset_buffers(&mut self, output: &Output);
    fn early_import(&mut self, surface: &WlSurface);
    fn update_led_state(&mut self, led_state: LedState);
    fn reload_cursor(&mut self, theme: Option<&str>, size: Option<u32>);

    /// Queue a redraw for the given output.
    fn queue_redraw(&mut self, output: &Output);

    /// Access the primary GlesRenderer. Used for capturing window snapshots
    /// in callbacks like toplevel_destroyed where no renderer is otherwise available.
    fn with_primary_renderer<T>(&mut self, f: impl FnOnce(&mut GlesRenderer) -> T) -> Option<T>;

    /// Capture a screenshot of the given output.  Backends with a renderer
    /// (udev, winit, x11) override this; the default returns `None`.
    #[allow(unused_variables)]
    fn capture_screenshot(
        &mut self,
        output: &Output,
        space: &Space<WindowElement>,
        pointer_location: Point<f64, Logical>,
        cursor_status: &CursorImageStatus,
        show_window_preview: bool,
        config: &crate::config::Config,
        now: Duration,
    ) -> Option<CapturedFrame> {
        return None
    }
}

/// Raw pixel data captured from an output, ready to be encoded as PNG.
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub fn save_screenshot_to_file(
    pixels: &[u8],
    width: u32,
    height: u32,
    path: &std::path::Path,
) -> std::io::Result<()> {
    use std::io::BufWriter;
    let file = std::fs::File::create(path)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| return std::io::Error::other(e.to_string()))?;
    writer
        .write_image_data(pixels)
        .map_err(|e| return std::io::Error::other(e.to_string()))?;
    return Ok(())
}

pub fn get_screenshot_path() -> std::path::PathBuf {
    let dir = dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| return std::path::PathBuf::from("/tmp"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    return dir.join(format!("screenshot_{}.png", now))
}

/// Encode raw RGBA pixel data as a PNG byte vector.
pub fn encode_screenshot_png(
    pixels: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, std::io::Error> {
    let mut png_bytes = Vec::with_capacity(pixels.len());
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| return std::io::Error::other(e.to_string()))?;
        writer
            .write_image_data(pixels)
            .map_err(|e| return std::io::Error::other(e.to_string()))?;
    }
    return Ok(png_bytes)
}

// ---------------------------------------------------------------------------
// IPC request handling
// ---------------------------------------------------------------------------

impl<BackendData: Backend + 'static> AnvilState<BackendData> {
    /// Dispatch an IPC request coming from `bakawm-ctl`.
    /// Returns the JSON response plus optional binary payload (PNG bytes).
    pub fn handle_ipc_request(&mut self, request: IpcRequest) -> (IpcResponse, Option<Vec<u8>>) {
        match request {
            IpcRequest::ListWindows => {
                let windows = self.ipc_list_windows();
                return (IpcResponse::Windows { windows }, None)
            }
            IpcRequest::FocusWindow { id } => match self.ipc_focus_window(id) {
                Ok(()) => return (IpcResponse::Ok, None),
                Err(msg) => return (IpcResponse::Error { message: msg }, None),
            },
            IpcRequest::CloseWindow { id } => match self.ipc_close_window(id) {
                Ok(()) => return (IpcResponse::Ok, None),
                Err(msg) => return (IpcResponse::Error { message: msg }, None),
            },
            IpcRequest::MoveWindow { id, x, y } => match self.ipc_move_window(id, x, y) {
                Ok(()) => return (IpcResponse::Ok, None),
                Err(msg) => return (IpcResponse::Error { message: msg }, None),
            },
            IpcRequest::ResizeWindow { id, w, h } => match self.ipc_resize_window(id, w, h) {
                Ok(()) => return (IpcResponse::Ok, None),
                Err(msg) => return (IpcResponse::Error { message: msg }, None),
            },
            IpcRequest::Screenshot { output } => match self.ipc_capture_screenshot(output) {
                Ok((png_bytes, width, height)) => return (
                    IpcResponse::Screenshot {
                        png_length: png_bytes.len() as u64,
                        width,
                        height,
                    },
                    Some(png_bytes),
                ),
                Err(msg) => return (IpcResponse::Error { message: msg }, None),
            },
            IpcRequest::ListOutputs => {
                let outputs = self.ipc_list_outputs();
                return (IpcResponse::Outputs { outputs }, None)
            }
        }
    }

    /// Enumerate windows in z-order.  IDs are `(enumerate_index + 1) as u64`,
    /// matching the introspect D-Bus interface.
    fn ipc_list_windows(&self) -> Vec<WindowInfo> {
        use smithay::desktop::WindowSurface;
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

        let mut windows = Vec::new();
        for (idx, window) in self.space.elements().enumerate() {
            let id = (idx as u64) + 1;
            let (title, app_id) = match window.0.underlying_surface() {
                WindowSurface::Wayland(toplevel) => {
                    let (title, app_id) = with_states(toplevel.wl_surface(), |states| {
                        let role = states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .unwrap()
                            .lock()
                            .unwrap();
                        let title = role.title.clone().unwrap_or_default();
                        let app_id = role
                            .app_id
                            .as_ref()
                            .map(|id| format!("{id}.desktop"))
                            .unwrap_or_default();
                        return (title, app_id)
                    });
                    (title, app_id)
                }
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(surface) => {
                    let title = surface.title();
                    let app_id = "x11".to_owned();
                    (title, app_id)
                }
                #[cfg(not(feature = "xwayland"))]
                _ => (String::new(), String::new()),
            };

            let geometry = self
                .space
                .element_geometry(window)
                .map(|g| return (g.loc.x, g.loc.y, g.size.w, g.size.h));

            windows.push(WindowInfo {
                id,
                title,
                app_id,
                geometry,
            });
        }
        return windows
    }

    /// Find a window by its IPC id (1-based enumerate index).
    fn ipc_find_window(&self, id: u64) -> Option<WindowElement> {
        if id == 0 {
            return None;
        }
        return self.space
            .elements()
            .enumerate()
            .find(|(idx, _)| return (*idx as u64) + 1 == id)
            .map(|(_, w)| return w.clone())
    }

    fn ipc_focus_window(&mut self, id: u64) -> Result<(), String> {
        use smithay::utils::SERIAL_COUNTER;
        let window = self
            .ipc_find_window(id)
            .ok_or_else(|| format!("window {id} not found"))?;
        self.space.raise_element(&window, true);

        #[cfg(feature = "xwayland")]
        if let Some(surface) = window.0.x11_surface()
            && let Some(xwm) = self.xwm.as_mut() {
                let _ = xwm.raise_window(surface);
            }

        if let Some(keyboard) = self.seat.get_keyboard() {
            let serial = SERIAL_COUNTER.next_serial();
            keyboard.set_focus(self, Some(window.into()), serial);
        }
        return Ok(())
    }

    fn ipc_close_window(&mut self, id: u64) -> Result<(), String> {
        let window = self
            .ipc_find_window(id)
            .ok_or_else(|| format!("window {id} not found"))?;

        // Queue close animation (snapshot captured during next render).
        // send_close() is deferred until after the snapshot is captured.
        self.queue_close_animation(&window);

        return Ok(())
    }

    fn ipc_move_window(&mut self, id: u64, x: i32, y: i32) -> Result<(), String> {
        let window = self
            .ipc_find_window(id)
            .ok_or_else(|| format!("window {id} not found"))?;
        self.space.map_element(window, (x, y), false);
        return Ok(())
    }

    fn ipc_resize_window(&mut self, id: u64, w: i32, h: i32) -> Result<(), String> {
        let window = self
            .ipc_find_window(id)
            .ok_or_else(|| format!("window {id} not found"))?;
        if let Some(toplevel) = window.0.toplevel() {
            toplevel.with_pending_state(|state| {
                state.size = Some((w, h).into());
            });
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
            return Ok(())
        } else {
            #[cfg(feature = "xwayland")]
            if let Some(surface) = window.0.x11_surface() {
                let size: Size<i32, Logical> = (w, h).into();
                let _ = surface.configure(Some(Rectangle::from_size(size)));
                return Ok(());
            }
            return Err("window does not support resize".to_owned())
        }
    }

    fn ipc_capture_screenshot(
        &mut self,
        output_name: Option<String>,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        let output = match output_name {
            Some(name) => self
                .space
                .outputs()
                .find(|o| return o.name() == name)
                .cloned()
                .ok_or_else(|| format!("output '{name}' not found"))?,
            None => self
                .space
                .outputs()
                .next()
                .cloned()
                .ok_or_else(|| return "no output available".to_owned())?,
        };

        let pointer_location = self.pointer.current_location();
        let now = self.clock.now();
        let frame = self.backend_data.capture_screenshot(
            &output,
            &self.space,
            pointer_location,
            &self.cursor_status,
            self.show_window_preview,
            &self.config,
            now.into(),
        );

        match frame {
            Some(captured) => {
                let png = encode_screenshot_png(&captured.pixels, captured.width, captured.height)
                    .map_err(|e| format!("failed to encode PNG: {e}"))?;
                return Ok((png, captured.width, captured.height))
            }
            None => return Err("backend does not support screenshots".to_owned()),
        }
    }

    fn ipc_list_outputs(&self) -> Vec<OutputInfo> {
        let mut outputs = Vec::new();
        for output in self.space.outputs() {
            let (width, height) = output
                .current_mode()
                .map(|m| return (m.size.w as u32, m.size.h as u32))
                .unwrap_or((0, 0));
            let scale = output.current_scale().fractional_scale();
            outputs.push(OutputInfo {
                name: output.name(),
                width,
                height,
                scale,
            });
        }
        return outputs
    }
}
