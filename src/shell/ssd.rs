use smithay::{
    backend::renderer::{
        Renderer,
        element::{
            AsRenderElements, Kind,
            solid::{SolidColorBuffer, SolidColorRenderElement},
        },
    },
    desktop::WindowSurface,
    input::Seat,
    utils::{Logical, Point, Serial},
    wayland::shell::xdg::XdgShellHandler,
};

use std::cell::{RefCell, RefMut};

use crate::{
    AnvilState,
    animation::Animation,
    config::{CornerRadius, ShadowConfig},
    render_helpers::{
        border::BorderRenderElement, shadow::ShadowRenderElement, texture::TextureBuffer,
    },
    state::Backend,
};

use super::WindowElement;

/// A pre-captured snapshot for close animation.
///
/// When a window surface is about to unmap (commits a null buffer), we capture
/// its contents here. Later, when `toplevel_destroyed` fires, we use this
/// snapshot to create the ClosingWindow animation.
pub struct PendingCloseSnapshot {
    /// Texture buffer containing the captured window contents.
    pub buffer: TextureBuffer<smithay::backend::renderer::gles::GlesTexture>,
    /// Window geometry size in logical coordinates.
    pub geo_size: smithay::utils::Size<f64, Logical>,
    /// Position of the window in the workspace.
    pub pos: smithay::utils::Point<f64, Logical>,
    /// Buffer offset.
    pub buffer_offset: smithay::utils::Point<f64, Logical>,
}

pub struct WindowState {
    pub is_ssd: bool,
    pub header_bar: HeaderBar,
    pub border: BorderState,
    pub shadow: ShadowConfig,
    pub corner_radius: CornerRadius,
    pub has_border_shader: Option<bool>,
    pub has_shadow_shader: Option<bool>,
    pub cached_border_element: Option<BorderRenderElement>,
    pub cached_shadow_element: Option<ShadowRenderElement>,
    /// Window open animation. Progress goes from 0 to 1.
    pub open_animation: Option<Animation>,
    /// Whether a close animation is in progress (window is closing).
    pub close_animation: Option<Animation>,
    /// Whether the window is waiting for its first commit to be properly centered.
    /// While true, the window is not rendered to avoid a position flash.
    pub needs_center: bool,
    /// Pre-captured snapshot for close animation.
    /// Set when the surface unmaps (commits null buffer), used in toplevel_destroyed.
    pub pending_close_snapshot: Option<PendingCloseSnapshot>,
    /// Layout engine state (tiling target, animations).
    pub layout: crate::layout::LayoutWindowState,
    /// Name of the custom shader to apply to this window (from `bk.window`/rules).
    pub shader: Option<String>,
    /// Workspace index this window belongs to (per-output).
    pub workspace: u32,
    /// Whether the window is on the active workspace. Hidden windows are excluded
    /// from the tiling layout and skipped in rendering (once any fade completes).
    pub hidden: bool,
    /// Workspace-switch fade animation. `0 -> 1` when entering, `1 -> 0` when
    /// leaving. While active the window is still rendered with this alpha.
    pub fade_anim: Option<Animation>,
}

#[derive(Debug, Clone)]
pub struct BorderState {
    pub top: SolidColorBuffer,
    pub bottom: SolidColorBuffer,
    pub left: SolidColorBuffer,
    pub right: SolidColorBuffer,
    pub last_width: i32,
    pub last_height: i32,
    pub last_border_width: f64,
    pub last_color: [f32; 4],
    pub active_color: [f32; 4],
    pub inactive_color: [f32; 4],
    pub is_active: bool,
}

impl Default for BorderState {
    fn default() -> Self {
        BorderState {
            top: SolidColorBuffer::default(),
            bottom: SolidColorBuffer::default(),
            left: SolidColorBuffer::default(),
            right: SolidColorBuffer::default(),
            last_width: 0,
            last_height: 0,
            last_border_width: 0.0,
            last_color: [0.0; 4],
            active_color: [0.0, 0.0, 0.0, 1.0],
            inactive_color: [0.3, 0.3, 0.3, 1.0],
            is_active: false,
        }
    }
}

impl BorderState {
    pub fn set_colors(&mut self, active_color: [f32; 4], inactive_color: [f32; 4]) {
        self.active_color = active_color;
        self.inactive_color = inactive_color;
    }

    pub fn set_active(&mut self, active: bool, window_w: i32, window_h: i32, border_width: f64) {
        if self.is_active == active {
            return;
        }
        self.is_active = active;
        let color = if active {
            self.active_color
        } else {
            self.inactive_color
        };
        self.redraw(window_w, window_h, border_width, color);
    }

    pub fn redraw(&mut self, window_w: i32, window_h: i32, border_width: f64, color: [f32; 4]) {
        if window_w == self.last_width
            && window_h == self.last_height
            && (border_width - self.last_border_width).abs() < f64::EPSILON
            && color == self.last_color
        {
            return;
        }

        let bw = border_width as i32;
        let full_w = window_w + bw * 2;
        let _full_h = window_h + bw * 2;

        self.top.update((full_w, bw), color);
        self.bottom.update((full_w, bw), color);
        self.left.update((bw, window_h), color);
        self.right.update((bw, window_h), color);

        self.last_width = window_w;
        self.last_height = window_h;
        self.last_border_width = border_width;
        self.last_color = color;
    }
}

#[derive(Debug, Clone)]
pub struct HeaderBar {
    pub pointer_loc: Option<Point<f64, Logical>>,
    pub width: u32,
    pub close_button_hover: bool,
    pub maximize_button_hover: bool,
    pub background: SolidColorBuffer,
    pub close_button: SolidColorBuffer,
    pub maximize_button: SolidColorBuffer,
}

const BG_COLOR: [f32; 4] = [0.75f32, 0.9f32, 0.78f32, 1f32];
const MAX_COLOR: [f32; 4] = [1f32, 0.965f32, 0.71f32, 1f32];
const CLOSE_COLOR: [f32; 4] = [1f32, 0.66f32, 0.612f32, 1f32];
const MAX_COLOR_HOVER: [f32; 4] = [0.71f32, 0.624f32, 0f32, 1f32];
const CLOSE_COLOR_HOVER: [f32; 4] = [0.75f32, 0.11f32, 0.016f32, 1f32];

pub const HEADER_BAR_HEIGHT: i32 = 32;
const BUTTON_HEIGHT: u32 = HEADER_BAR_HEIGHT as u32;
const BUTTON_WIDTH: u32 = 32;

impl HeaderBar {
    pub fn pointer_enter(&mut self, loc: Point<f64, Logical>) {
        self.pointer_loc = Some(loc);
    }

    pub fn pointer_leave(&mut self) {
        self.pointer_loc = None;
    }

    pub fn clicked<BackendData: Backend>(
        &mut self,
        seat: &Seat<AnvilState<BackendData>>,
        state: &mut AnvilState<BackendData>,
        window: &WindowElement,
        serial: Serial,
    ) {
        match self.pointer_loc.as_ref() {
            Some(loc) if loc.x >= (self.width - BUTTON_WIDTH) as f64 => {
                // Queue close animation (snapshot captured during next render).
                // send_close() is deferred until after the snapshot is captured.
                state.queue_close_animation(window);
            }
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 2)) as f64 => {
                match window.0.underlying_surface() {
                    WindowSurface::Wayland(w) => state.maximize_request(w.clone()),
                    #[cfg(feature = "xwayland")]
                    WindowSurface::X11(w) => {
                        let surface = w.clone();
                        state
                            .handle
                            .insert_idle(move |data| data.maximize_request_x11(&surface));
                    }
                };
            }
            Some(_) => {
                match window.0.underlying_surface() {
                    WindowSurface::Wayland(w) => {
                        let seat = seat.clone();
                        let toplevel = w.clone();
                        state.handle.insert_idle(move |data| {
                            data.move_request_xdg(&toplevel, &seat, serial)
                        });
                    }
                    #[cfg(feature = "xwayland")]
                    WindowSurface::X11(w) => {
                        let window = w.clone();
                        state
                            .handle
                            .insert_idle(move |data| data.move_request_x11(&window));
                    }
                };
            }
            _ => {}
        };
    }

    pub fn touch_down<BackendData: Backend>(
        &mut self,
        seat: &Seat<AnvilState<BackendData>>,
        state: &mut AnvilState<BackendData>,
        window: &WindowElement,
        serial: Serial,
    ) {
        match self.pointer_loc.as_ref() {
            Some(loc) if loc.x >= (self.width - BUTTON_WIDTH) as f64 => {}
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 2)) as f64 => {}
            Some(_) => {
                match window.0.underlying_surface() {
                    WindowSurface::Wayland(w) => {
                        let seat = seat.clone();
                        let toplevel = w.clone();
                        state.handle.insert_idle(move |data| {
                            data.move_request_xdg(&toplevel, &seat, serial)
                        });
                    }
                    #[cfg(feature = "xwayland")]
                    WindowSurface::X11(w) => {
                        let window = w.clone();
                        state
                            .handle
                            .insert_idle(move |data| data.move_request_x11(&window));
                    }
                };
            }
            _ => {}
        };
    }

    pub fn touch_up<BackendData: Backend>(
        &mut self,
        _seat: &Seat<AnvilState<BackendData>>,
        state: &mut AnvilState<BackendData>,
        window: &WindowElement,
        _serial: Serial,
    ) {
        match self.pointer_loc.as_ref() {
            Some(loc) if loc.x >= (self.width - BUTTON_WIDTH) as f64 => {
                // Queue close animation (snapshot captured during next render).
                // send_close() is deferred until after the snapshot is captured.
                state.queue_close_animation(window);
            }
            Some(loc) if loc.x >= (self.width - (BUTTON_WIDTH * 2)) as f64 => {
                match window.0.underlying_surface() {
                    WindowSurface::Wayland(w) => state.maximize_request(w.clone()),
                    #[cfg(feature = "xwayland")]
                    WindowSurface::X11(w) => {
                        let surface = w.clone();
                        state
                            .handle
                            .insert_idle(move |data| data.maximize_request_x11(&surface));
                    }
                };
            }
            _ => {}
        };
    }

    pub fn redraw(&mut self, width: u32) {
        if width == 0 {
            self.width = 0;
            return;
        }

        self.background
            .update((width as i32, HEADER_BAR_HEIGHT), BG_COLOR);

        let mut needs_redraw_buttons = false;
        if width != self.width {
            needs_redraw_buttons = true;
            self.width = width;
        }

        if self
            .pointer_loc
            .as_ref()
            .map(|l| l.x >= (width - BUTTON_WIDTH) as f64)
            .unwrap_or(false)
            && (needs_redraw_buttons || !self.close_button_hover)
        {
            self.close_button.update(
                (BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32),
                CLOSE_COLOR_HOVER,
            );
            self.close_button_hover = true;
        } else if !self
            .pointer_loc
            .as_ref()
            .map(|l| l.x >= (width - BUTTON_WIDTH) as f64)
            .unwrap_or(false)
            && (needs_redraw_buttons || self.close_button_hover)
        {
            self.close_button
                .update((BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32), CLOSE_COLOR);
            self.close_button_hover = false;
        }

        if self
            .pointer_loc
            .as_ref()
            .map(|l| {
                l.x >= (width - BUTTON_WIDTH * 2) as f64 && l.x <= (width - BUTTON_WIDTH) as f64
            })
            .unwrap_or(false)
            && (needs_redraw_buttons || !self.maximize_button_hover)
        {
            self.maximize_button
                .update((BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32), MAX_COLOR_HOVER);
            self.maximize_button_hover = true;
        } else if !self
            .pointer_loc
            .as_ref()
            .map(|l| {
                l.x >= (width - BUTTON_WIDTH * 2) as f64 && l.x <= (width - BUTTON_WIDTH) as f64
            })
            .unwrap_or(false)
            && (needs_redraw_buttons || self.maximize_button_hover)
        {
            self.maximize_button
                .update((BUTTON_WIDTH as i32, BUTTON_HEIGHT as i32), MAX_COLOR);
            self.maximize_button_hover = false;
        }
    }
}

impl<R: Renderer> AsRenderElements<R> for HeaderBar {
    type RenderElement = SolidColorRenderElement;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        _renderer: &mut R,
        location: Point<i32, smithay::utils::Physical>,
        scale: smithay::utils::Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        let header_end_offset: Point<i32, Logical> = Point::from((self.width as i32, 0));
        let button_offset: Point<i32, Logical> = Point::from((BUTTON_WIDTH as i32, 0));

        vec![
            SolidColorRenderElement::from_buffer(
                &self.close_button,
                location + (header_end_offset - button_offset).to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            )
            .into(),
            SolidColorRenderElement::from_buffer(
                &self.maximize_button,
                location
                    + (header_end_offset - button_offset.upscale(2))
                        .to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            )
            .into(),
            SolidColorRenderElement::from_buffer(
                &self.background,
                location,
                scale,
                alpha,
                Kind::Unspecified,
            )
            .into(),
        ]
    }
}

impl WindowElement {
    pub fn decoration_state(&self) -> RefMut<'_, WindowState> {
        self.user_data().insert_if_missing(|| {
            RefCell::new(WindowState {
                is_ssd: false,
                header_bar: HeaderBar {
                    pointer_loc: None,
                    width: 0,
                    close_button_hover: false,
                    maximize_button_hover: false,
                    background: SolidColorBuffer::default(),
                    close_button: SolidColorBuffer::default(),
                    maximize_button: SolidColorBuffer::default(),
                },
                border: BorderState::default(),
                shadow: ShadowConfig::default(),
                corner_radius: CornerRadius::default(),
                has_border_shader: None,
                has_shadow_shader: None,
                cached_border_element: None,
                cached_shadow_element: None,
                open_animation: None,
                close_animation: None,
                needs_center: false,
                pending_close_snapshot: None,
                layout: crate::layout::LayoutWindowState::default(),
                shader: None,
                workspace: 0,
                hidden: false,
                fade_anim: None,
            })
        });

        self.user_data()
            .get::<RefCell<WindowState>>()
            .unwrap()
            .borrow_mut()
    }

    pub fn set_ssd(&self, _ssd: bool) {
        self.decoration_state().is_ssd = false;
    }

    /// Apply global and per-rule configuration to this window's decoration state.
    /// Returns true if the config changed the decoration state.
    /// Call this after mapping a window and after config reload.
    pub fn apply_config(&self, config: &crate::config::Config) -> bool {
        use smithay::desktop::WindowSurface;
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

        let (title, app_id): (Option<String>, Option<String>) = match self.0.underlying_surface() {
            WindowSurface::Wayland(toplevel) => with_states(toplevel.wl_surface(), |states| {
                let role = states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .unwrap()
                    .lock()
                    .unwrap();
                (role.title.clone(), role.app_id.clone())
            }),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(surface) => (Some(surface.title()), None),
            #[cfg(not(feature = "xwayland"))]
            _ => (None, None),
        };

        let rule = config.find_window_rule(app_id.as_deref(), title.as_deref());

        // Use the window config from the rule (merged over global), or just global
        let win_config = match rule.and_then(|r| r.window.as_ref()) {
            Some(partial) => partial.merge_over(&config.window),
            None => config.window.clone(),
        };

        let mut state = self.decoration_state();

        // Check if anything actually changed
        let changed = state.border.active_color != win_config.border.color
            || state.border.inactive_color != win_config.border.inactive_color
            || state.shadow != win_config.shadow
            || state.corner_radius != win_config.corner_radius
            || state.shader != win_config.shader;

        if !changed {
            return false;
        }

        // Apply border config
        state.border.active_color = win_config.border.color;
        state.border.inactive_color = win_config.border.inactive_color;

        // Apply shadow config
        state.shadow = win_config.shadow;

        // Apply corner radius
        state.corner_radius = win_config.corner_radius;

        // Apply custom shader
        state.shader = win_config.shader;

        // Reset shader caches since config may have changed
        state.has_border_shader = None;
        state.has_shadow_shader = None;
        state.cached_border_element = None;
        state.cached_shadow_element = None;

        true
    }
}
