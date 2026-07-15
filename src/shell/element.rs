use std::{borrow::Cow, time::Duration};

use smithay::{
    backend::renderer::{
        element::{
            AsRenderElements, Element, Id, Kind, RenderElement, UnderlyingStorage,
            solid::SolidColorRenderElement,
            surface::WaylandSurfaceRenderElement,
        },
        gles::{GlesError, GlesFrame, GlesRenderer},
        multigpu::MultiRenderer,
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    desktop::{
        Window, WindowSurface, WindowSurfaceType, space::SpaceElement, utils::OutputPresentationFeedback,
    },
    input::{
        Seat,
        pointer::{
            AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
            GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
            GestureSwipeUpdateEvent, MotionEvent, PointerTarget, RelativeMotionEvent,
        },
        touch::{FrameMarker, TouchTarget},
    },
    output::Output,
    reexports::{
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Buffer, IsAlive, Logical, Physical, Point, Rectangle, Scale, Serial, Size, Transform, user_data::UserDataMap},
    wayland::{compositor::SurfaceData as WlSurfaceData, dmabuf::DmabufFeedback, seat::WaylandFocus},
};


use crate::{AnvilState, config::CornerRadius, focus::PointerFocusTarget, render_helpers::{border::BorderRenderElement, clipped_surface::ClippedSurfaceRenderElement, shadow::ShadowRenderElement}, state::Backend};

#[derive(Debug, Clone, PartialEq)]
pub struct WindowElement(pub Window);

impl WindowElement {
    pub fn surface_under(
        &self,
        location: Point<f64, Logical>,
        window_type: WindowSurfaceType,
    ) -> Option<(PointerFocusTarget, Point<i32, Logical>)> {
        let surface_under = self.0.surface_under(location, window_type);
        let (under, loc) = match self.0.underlying_surface() {
            WindowSurface::Wayland(_) => {
                surface_under.map(|(surface, loc)| (PointerFocusTarget::WlSurface(surface), loc))
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(s) => {
                surface_under.map(|(_, loc)| (PointerFocusTarget::X11Surface(s.clone()), loc))
            }
        }?;
        Some((under, loc))
    }

    pub fn with_surfaces<F>(&self, processor: F)
    where
        F: FnMut(&WlSurface, &WlSurfaceData),
    {
        self.0.with_surfaces(processor);
    }

    pub fn send_frame<T, F>(
        &self,
        output: &Output,
        time: T,
        throttle: Option<Duration>,
        primary_scan_out_output: F,
    ) where
        T: Into<Duration>,
        F: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
    {
        self.0.send_frame(output, time, throttle, primary_scan_out_output)
    }

    pub fn send_dmabuf_feedback<'a, P, F>(
        &self,
        output: &Output,
        primary_scan_out_output: P,
        select_dmabuf_feedback: F,
    ) where
        P: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
        F: Fn(&WlSurface, &WlSurfaceData) -> &'a DmabufFeedback + Copy,
    {
        self.0
            .send_dmabuf_feedback(output, primary_scan_out_output, select_dmabuf_feedback)
    }

    pub fn take_presentation_feedback<F1, F2>(
        &self,
        output_feedback: &mut OutputPresentationFeedback,
        primary_scan_out_output: F1,
        presentation_feedback_flags: F2,
    ) where
        F1: FnMut(&WlSurface, &WlSurfaceData) -> Option<Output> + Copy,
        F2: FnMut(&WlSurface, &WlSurfaceData) -> wp_presentation_feedback::Kind + Copy,
    {
        self.0.take_presentation_feedback(
            output_feedback,
            primary_scan_out_output,
            presentation_feedback_flags,
        )
    }

    #[cfg(feature = "xwayland")]
    #[inline]
    pub fn is_x11(&self) -> bool {
        self.0.is_x11()
    }

    #[inline]
    pub fn is_wayland(&self) -> bool {
        self.0.is_wayland()
    }

    #[inline]
    pub fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }

    #[inline]
    pub fn user_data(&self) -> &UserDataMap {
        self.0.user_data()
    }
}

impl IsAlive for WindowElement {
    #[inline]
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SSD(WindowElement);

impl IsAlive for SSD {
    #[inline]
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

impl WaylandFocus for SSD {
    #[inline]
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }
}

impl<BackendData: Backend> PointerTarget<AnvilState<BackendData>> for SSD {
    fn enter(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        event: &MotionEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
        }
    }
    fn motion(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        event: &MotionEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
        }
    }
    fn relative_motion(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &RelativeMotionEvent,
    ) {
    }
    fn button(
        &self,
        seat: &Seat<AnvilState<BackendData>>,
        data: &mut AnvilState<BackendData>,
        event: &ButtonEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.clicked(seat, data, &self.0, event.serial);
        }
    }
    fn axis(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _frame: AxisFrame,
    ) {
    }
    fn frame(&self, _seat: &Seat<AnvilState<BackendData>>, _data: &mut AnvilState<BackendData>) {}
    fn leave(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _serial: Serial,
        _time: u32,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_leave();
        }
    }
    fn gesture_swipe_begin(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GestureSwipeBeginEvent,
    ) {
    }
    fn gesture_swipe_update(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GestureSwipeUpdateEvent,
    ) {
    }
    fn gesture_swipe_end(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GestureSwipeEndEvent,
    ) {
    }
    fn gesture_pinch_begin(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GesturePinchBeginEvent,
    ) {
    }
    fn gesture_pinch_update(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GesturePinchUpdateEvent,
    ) {
    }
    fn gesture_pinch_end(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GesturePinchEndEvent,
    ) {
    }
    fn gesture_hold_begin(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GestureHoldBeginEvent,
    ) {
    }
    fn gesture_hold_end(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &GestureHoldEndEvent,
    ) {
    }
}

impl<BackendData: Backend> TouchTarget<AnvilState<BackendData>> for SSD {
    fn down(
        &self,
        seat: &Seat<AnvilState<BackendData>>,
        data: &mut AnvilState<BackendData>,
        event: &smithay::input::touch::DownEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
            state.header_bar.touch_down(seat, data, &self.0, event.serial);
        }
    }

    fn up(
        &self,
        seat: &Seat<AnvilState<BackendData>>,
        data: &mut AnvilState<BackendData>,
        event: &smithay::input::touch::UpEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.touch_up(seat, data, &self.0, event.serial);
        }
    }

    fn motion(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        event: &smithay::input::touch::MotionEvent,
    ) {
        let mut state = self.0.decoration_state();
        if state.is_ssd {
            state.header_bar.pointer_enter(event.location);
        }
    }

    fn frame(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _marker: FrameMarker,
    ) {
    }

    fn cancel(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _marker: FrameMarker,
    ) {
    }

    fn shape(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &smithay::input::touch::ShapeEvent,
    ) {
    }

    fn orientation(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
        _event: &smithay::input::touch::OrientationEvent,
    ) {
    }

    fn last_frame(
        &self,
        _seat: &Seat<AnvilState<BackendData>>,
        _data: &mut AnvilState<BackendData>,
    ) -> Option<FrameMarker> {
        // It would be more correct to store the marker on frame and cancel,
        // but since we're ignoring those anyway, no need for the added complexity.
        None
    }
}

impl SpaceElement for WindowElement {
    fn geometry(&self) -> Rectangle<i32, Logical> {
        SpaceElement::geometry(&self.0)
    }
    fn bbox(&self) -> Rectangle<i32, Logical> {
        SpaceElement::bbox(&self.0)
    }
    fn is_in_input_region(&self, point: &Point<f64, Logical>) -> bool {
        SpaceElement::is_in_input_region(&self.0, point)
    }
    fn z_index(&self) -> u8 {
        SpaceElement::z_index(&self.0)
    }

    fn set_activate(&self, activated: bool) {
        SpaceElement::set_activate(&self.0, activated);
    }
    fn output_enter(&self, output: &Output, overlap: Rectangle<i32, Logical>) {
        SpaceElement::output_enter(&self.0, output, overlap);
    }
    fn output_leave(&self, output: &Output) {
        SpaceElement::output_leave(&self.0, output);
    }
    #[profiling::function]
    fn refresh(&self) {
        SpaceElement::refresh(&self.0);
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum WindowRenderElement {
    Window(WaylandSurfaceRenderElement<GlesRenderer>),
    Decoration(SolidColorRenderElement),
    Border(BorderRenderElement),
    Shadow(ShadowRenderElement),
    ClippedSurface(ClippedSurfaceRenderElement),
}

impl Element for WindowRenderElement {
    fn id(&self) -> &Id {
        match self {
            Self::Window(e) => e.id(),
            Self::Decoration(e) => e.id(),
            Self::Border(e) => e.id(),
            Self::Shadow(e) => e.id(),
            Self::ClippedSurface(e) => e.id(),
        }
    }

    fn current_commit(&self) -> CommitCounter {
        match self {
            Self::Window(e) => e.current_commit(),
            Self::Decoration(e) => e.current_commit(),
            Self::Border(e) => e.current_commit(),
            Self::Shadow(e) => e.current_commit(),
            Self::ClippedSurface(e) => e.current_commit(),
        }
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        match self {
            Self::Window(e) => e.geometry(scale),
            Self::Decoration(e) => e.geometry(scale),
            Self::Border(e) => e.geometry(scale),
            Self::Shadow(e) => e.geometry(scale),
            Self::ClippedSurface(e) => e.geometry(scale),
        }
    }

    fn transform(&self) -> Transform {
        match self {
            Self::Window(e) => e.transform(),
            Self::Decoration(e) => e.transform(),
            Self::Border(e) => e.transform(),
            Self::Shadow(e) => e.transform(),
            Self::ClippedSurface(e) => e.transform(),
        }
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        match self {
            Self::Window(e) => e.src(),
            Self::Decoration(e) => e.src(),
            Self::Border(e) => e.src(),
            Self::Shadow(e) => e.src(),
            Self::ClippedSurface(e) => e.src(),
        }
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        match self {
            Self::Window(e) => e.damage_since(scale, commit),
            Self::Decoration(e) => e.damage_since(scale, commit),
            Self::Border(e) => e.damage_since(scale, commit),
            Self::Shadow(e) => e.damage_since(scale, commit),
            Self::ClippedSurface(e) => e.damage_since(scale, commit),
        }
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        match self {
            Self::Window(e) => e.opaque_regions(scale),
            Self::Decoration(e) => e.opaque_regions(scale),
            Self::Border(e) => e.opaque_regions(scale),
            Self::Shadow(e) => e.opaque_regions(scale),
            Self::ClippedSurface(e) => e.opaque_regions(scale),
        }
    }

    fn alpha(&self) -> f32 {
        match self {
            Self::Window(e) => e.alpha(),
            Self::Decoration(e) => e.alpha(),
            Self::Border(e) => e.alpha(),
            Self::Shadow(e) => e.alpha(),
            Self::ClippedSurface(e) => e.alpha(),
        }
    }

    fn kind(&self) -> Kind {
        match self {
            Self::Window(e) => e.kind(),
            Self::Decoration(e) => e.kind(),
            Self::Border(e) => e.kind(),
            Self::Shadow(e) => e.kind(),
            Self::ClippedSurface(e) => e.kind(),
        }
    }
}

impl RenderElement<GlesRenderer> for WindowRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), GlesError> {
        match self {
            Self::Window(e) => RenderElement::<GlesRenderer>::draw(e, frame, src, dst, damage, opaque_regions, cache),
            Self::Decoration(e) => RenderElement::<GlesRenderer>::draw(e, frame, src, dst, damage, opaque_regions, cache),
            Self::Border(e) => RenderElement::<GlesRenderer>::draw(e, frame, src, dst, damage, opaque_regions, cache),
            Self::Shadow(e) => RenderElement::<GlesRenderer>::draw(e, frame, src, dst, damage, opaque_regions, cache),
            Self::ClippedSurface(e) => RenderElement::<GlesRenderer>::draw(e, frame, src, dst, damage, opaque_regions, cache),
        }
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        match self {
            Self::Window(e) => e.underlying_storage(renderer),
            Self::Decoration(e) => e.underlying_storage(renderer),
            Self::Border(e) => e.underlying_storage(renderer),
            Self::Shadow(e) => e.underlying_storage(renderer),
            Self::ClippedSurface(e) => e.underlying_storage(renderer),
        }
    }

    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &smithay::utils::user_data::UserDataMap,
    ) -> Result<(), GlesError> {
        match self {
            Self::Window(e) => RenderElement::<GlesRenderer>::capture_framebuffer(e, frame, src, dst, cache),
            Self::Decoration(e) => RenderElement::<GlesRenderer>::capture_framebuffer(e, frame, src, dst, cache),
            Self::Border(e) => RenderElement::<GlesRenderer>::capture_framebuffer(e, frame, src, dst, cache),
            Self::Shadow(e) => RenderElement::<GlesRenderer>::capture_framebuffer(e, frame, src, dst, cache),
            Self::ClippedSurface(e) => RenderElement::<GlesRenderer>::capture_framebuffer(e, frame, src, dst, cache),
        }
    }
}

type UdevMultiRenderer<'a, 'b> = MultiRenderer<
    'a,
    'b,
    smithay::backend::renderer::multigpu::gbm::GbmGlesBackend<smithay::backend::renderer::gles::GlesRenderer, smithay::backend::drm::DrmDeviceFd>,
    smithay::backend::renderer::multigpu::gbm::GbmGlesBackend<smithay::backend::renderer::gles::GlesRenderer, smithay::backend::drm::DrmDeviceFd>,
>;

#[cfg(feature = "udev")]
impl<'a, 'b> RenderElement<UdevMultiRenderer<'a, 'b>> for WindowRenderElement
{
    fn draw(
        &self,
        frame: &mut <UdevMultiRenderer<'a, 'b> as smithay::backend::renderer::RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), <UdevMultiRenderer<'a, 'b> as smithay::backend::renderer::RendererSuper>::Error> {
        match self {
            Self::Window(e) => RenderElement::<GlesRenderer>::draw(e, frame.as_mut(), src, dst, damage, opaque_regions, cache).map_err(Into::into),
            Self::Decoration(e) => RenderElement::<GlesRenderer>::draw(e, frame.as_mut(), src, dst, damage, opaque_regions, cache).map_err(Into::into),
            Self::Border(e) => RenderElement::<GlesRenderer>::draw(e, frame.as_mut(), src, dst, damage, opaque_regions, cache).map_err(Into::into),
            Self::Shadow(e) => RenderElement::<GlesRenderer>::draw(e, frame.as_mut(), src, dst, damage, opaque_regions, cache).map_err(Into::into),
            Self::ClippedSurface(e) => RenderElement::<GlesRenderer>::draw(e, frame.as_mut(), src, dst, damage, opaque_regions, cache).map_err(Into::into),
        }
    }

    fn underlying_storage(&self, renderer: &mut UdevMultiRenderer<'a, 'b>) -> Option<UnderlyingStorage<'_>> {
        let gles = renderer.as_mut();
        match self {
            Self::Window(e) => e.underlying_storage(gles),
            Self::Decoration(e) => e.underlying_storage(gles),
            Self::Border(e) => e.underlying_storage(gles),
            Self::Shadow(e) => e.underlying_storage(gles),
            Self::ClippedSurface(e) => e.underlying_storage(gles),
        }
    }

    fn capture_framebuffer(
        &self,
        frame: &mut <UdevMultiRenderer<'a, 'b> as smithay::backend::renderer::RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &smithay::utils::user_data::UserDataMap,
    ) -> Result<(), <UdevMultiRenderer<'a, 'b> as smithay::backend::renderer::RendererSuper>::Error> {
        match self {
            Self::Window(e) => RenderElement::<GlesRenderer>::capture_framebuffer(e, frame.as_mut(), src, dst, cache).map_err(Into::into),
            Self::Decoration(e) => RenderElement::<GlesRenderer>::capture_framebuffer(e, frame.as_mut(), src, dst, cache).map_err(Into::into),
            Self::Border(e) => RenderElement::<GlesRenderer>::capture_framebuffer(e, frame.as_mut(), src, dst, cache).map_err(Into::into),
            Self::Shadow(e) => RenderElement::<GlesRenderer>::capture_framebuffer(e, frame.as_mut(), src, dst, cache).map_err(Into::into),
            Self::ClippedSurface(e) => RenderElement::<GlesRenderer>::capture_framebuffer(e, frame.as_mut(), src, dst, cache).map_err(Into::into),
        }
    }
}

impl From<WaylandSurfaceRenderElement<GlesRenderer>> for WindowRenderElement {
    fn from(e: WaylandSurfaceRenderElement<GlesRenderer>) -> Self {
        Self::Window(e)
    }
}

impl From<SolidColorRenderElement> for WindowRenderElement {
    fn from(e: SolidColorRenderElement) -> Self {
        Self::Decoration(e)
    }
}

impl From<BorderRenderElement> for WindowRenderElement {
    fn from(e: BorderRenderElement) -> Self {
        Self::Border(e)
    }
}

impl From<ShadowRenderElement> for WindowRenderElement {
    fn from(e: ShadowRenderElement) -> Self {
        Self::Shadow(e)
    }
}

impl From<ClippedSurfaceRenderElement> for WindowRenderElement {
    fn from(e: ClippedSurfaceRenderElement) -> Self {
        Self::ClippedSurface(e)
    }
}

#[cfg(feature = "udev")]
impl<'a, 'b> AsRenderElements<UdevMultiRenderer<'a, 'b>> for WindowElement {
    type RenderElement = WindowRenderElement;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        renderer: &mut UdevMultiRenderer<'a, 'b>,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        AsRenderElements::<GlesRenderer>::render_elements::<C>(self, renderer.as_mut(), location, scale, alpha)
    }
}

impl AsRenderElements<GlesRenderer> for WindowElement {
    type RenderElement = WindowRenderElement;

    fn render_elements<C: From<Self::RenderElement>>(
        &self,
        renderer: &mut GlesRenderer,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Vec<C> {
        let window_bbox = SpaceElement::bbox(&self.0);
        let mut state = self.decoration_state();
        let window_geo = SpaceElement::geometry(&self.0);

        let border_width = state.border.last_border_width;
        let border_color = if state.border.is_active {
            state.border.active_color
        } else {
            state.border.inactive_color
        };
        let corner_radius = state.corner_radius;
        let shadow_config = state.shadow;
        let has_border = border_width >= 0.5;
        let has_corners = corner_radius != CornerRadius::default();
        let has_shadow = shadow_config.enable;

        let geo_offset_physical: Point<i32, Physical> =
            window_geo.loc.to_physical_precise_round(scale);
        let content_location: Point<i32, Physical> = Point::from((
            location.x + geo_offset_physical.x,
            location.y + geo_offset_physical.y,
        ));

        let window_geo_loc_logical: Point<f64, Logical> =
            Point::<f64, Physical>::from((content_location.x as f64, content_location.y as f64))
                .to_logical(scale);
        let window_geo_size_logical: Size<f64, Logical> = window_geo.size.to_f64();
        let window_geo_logical: Rectangle<f64, Logical> =
            Rectangle::new(window_geo_loc_logical, window_geo_size_logical);
        let radius = corner_radius.fit_to(window_geo.size.w as f32, window_geo.size.h as f32);

        let clip_shader = if has_corners {
            ClippedSurfaceRenderElement::shader(renderer).cloned()
        } else {
            None
        };
        let has_border_shader = if has_border && has_corners {
            *state.has_border_shader.get_or_insert_with(|| BorderRenderElement::has_shader(renderer))
        } else {
            false
        };
        let has_shadow_shader = if has_shadow {
            *state.has_shadow_shader.get_or_insert_with(|| ShadowRenderElement::has_shader(renderer))
        } else {
            false
        };

        let window_elements: Vec<WindowRenderElement> =
            AsRenderElements::render_elements(&self.0, renderer, location, scale, alpha);

        let mut vec: Vec<C> = {
            let mut result = Vec::new();
            for elem in window_elements {
                match elem {
                    WindowRenderElement::Window(wayland_elem) => {
                        if has_corners {
                            if let Some(shader) = clip_shader.clone() {
                                if ClippedSurfaceRenderElement::will_clip(
                                    &wayland_elem,
                                    scale,
                                    window_geo_logical,
                                    radius,
                                ) {
                                    result.push(
                                        C::from(WindowRenderElement::ClippedSurface(
                                            ClippedSurfaceRenderElement::new(
                                                wayland_elem,
                                                scale,
                                                window_geo_logical,
                                                shader,
                                                radius,
                                            ),
                                        )),
                                    );
                                    continue;
                                }
                            }
                        }
                        result.push(C::from(WindowRenderElement::Window(wayland_elem)));
                    }
                    elem => result.push(C::from(elem)),
                }
            }
            result
        };

        if has_border && !window_bbox.is_empty() {
            state.border.redraw(window_geo.size.w, window_geo.size.h, border_width, border_color);

            if has_corners && has_border_shader {
                let bw = border_width as f32;
                let outer_radius = radius.expanded_by(bw);

                let full_size: Size<f64, Logical> = Size::new(
                    window_geo_size_logical.w + border_width * 2.,
                    window_geo_size_logical.h + border_width * 2.,
                );
                let border_geo = Rectangle::from_size(full_size);

                let cached = state
                    .cached_border_element
                    .get_or_insert_with(BorderRenderElement::empty);
                cached.update(
                    full_size,
                    border_color,
                    border_color,
                    border_geo,
                    bw,
                    outer_radius,
                    scale.x as f32,
                    alpha,
                );
                let border_elem = cached
                    .clone()
                    .with_location(
                        Point::from((
                            window_geo_logical.loc.x - border_width as f64,
                            window_geo_logical.loc.y - border_width as f64,
                        )),
                    );

                vec.insert(0, C::from(WindowRenderElement::Border(border_elem)));
            } else {
                let bw_phys = (scale.x * border_width) as i32;
                let win_w_phys = (scale.x * window_geo.size.w as f64) as i32;
                let win_h_phys = (scale.y * window_geo.size.h as f64) as i32;
                let full_w_phys = win_w_phys + bw_phys * 2;
                let full_h_phys = win_h_phys + bw_phys * 2;

                let border_loc: Point<i32, Physical> = Point::from((
                    content_location.x - bw_phys,
                    content_location.y - bw_phys,
                ));

                vec.insert(0,
                    WindowRenderElement::Decoration(
                        SolidColorRenderElement::from_buffer(
                            &state.border.top,
                            border_loc,
                            scale,
                            alpha,
                            Kind::Unspecified,
                        )
                    ).into(),
                );
                vec.insert(0,
                    WindowRenderElement::Decoration(
                        SolidColorRenderElement::from_buffer(
                            &state.border.bottom,
                            Point::from((border_loc.x, border_loc.y + full_h_phys - bw_phys)),
                            scale,
                            alpha,
                            Kind::Unspecified,
                        )
                    ).into(),
                );
                vec.insert(0,
                    WindowRenderElement::Decoration(
                        SolidColorRenderElement::from_buffer(
                            &state.border.left,
                            Point::from((border_loc.x, border_loc.y + bw_phys)),
                            scale,
                            alpha,
                            Kind::Unspecified,
                        )
                    ).into(),
                );
                vec.insert(0,
                    WindowRenderElement::Decoration(
                        SolidColorRenderElement::from_buffer(
                            &state.border.right,
                            Point::from((border_loc.x + full_w_phys - bw_phys, border_loc.y + bw_phys)),
                            scale,
                            alpha,
                            Kind::Unspecified,
                        )
                    ).into(),
                );
            }
        }

        if has_shadow && !window_bbox.is_empty() && has_shadow_shader {
            let ceil = |logical: f64| (logical * scale.x).ceil() / scale.x;

            let sigma = (shadow_config.softness / 2.) as f32;
            let width = ceil(sigma as f64 * 3.);

            let offset = Point::from((
                ceil(shadow_config.offset_x),
                ceil(shadow_config.offset_y),
            ));
            let spread = ceil(shadow_config.spread.abs()).copysign(shadow_config.spread);
            let offset = offset - Point::from((spread, spread));

            let win_radius = radius.fit_to(
                window_geo_size_logical.w as f32,
                window_geo_size_logical.h as f32,
            );

            let box_size = if shadow_config.spread >= 0. {
                window_geo_size_logical + Size::from((spread, spread)).upscale(2.)
            } else {
                window_geo_size_logical - Size::from((-spread, -spread)).upscale(2.)
            };
            let shadow_radius = win_radius.expanded_by(spread as f32);

            let shader_size = box_size + Size::from((width, width)).upscale(2.);

            let shader_geo = Rectangle::new(Point::from((-width, -width)), shader_size);
            let window_geo_for_shadow = Rectangle::new(
                Point::from((0., 0.)) - offset,
                window_geo_size_logical,
            );

            let shadow_elem = {
                let cached = state
                    .cached_shadow_element
                    .get_or_insert_with(ShadowRenderElement::empty);
                cached.update(
                    shader_size,
                    Rectangle::new(shader_geo.loc.upscale(-1.), box_size),
                    shadow_config.color,
                    sigma,
                    shadow_radius,
                    scale.x as f32,
                    window_geo_for_shadow,
                    win_radius,
                    alpha,
                );
                cached
                    .clone()
                    .with_location(
                        Point::from((
                            window_geo_logical.loc.x + offset.x,
                            window_geo_logical.loc.y + offset.y,
                        )),
                    )
            };

            vec.insert(0, C::from(WindowRenderElement::Shadow(shadow_elem)));
        }

        vec
    }
}