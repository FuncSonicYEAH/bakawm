use smithay::{
    backend::renderer::{
        Color32F, ImportAll, ImportMem, Renderer, RendererSuper, Texture,
        damage::{Error as OutputDamageTrackerError, OutputDamageTracker, RenderOutputResult},
        element::{
            AsRenderElements, Element, Id, Kind, RenderElement, UnderlyingStorage, Wrap,
            surface::WaylandSurfaceRenderElement,
            utils::{
                ConstrainAlign, ConstrainScaleBehavior, CropRenderElement, RelocateRenderElement,
                RescaleRenderElement,
            },
        },
        gles::{GlesError, GlesFrame, GlesRenderer},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    desktop::{
        layer_map_for_output,
        space::{
            ConstrainBehavior, ConstrainReference, Space, SpaceElement, SpaceRenderElements,
            constrain_space_element,
        },
    },
    output::Output,
    utils::{Buffer, Point, Rectangle, Scale, Size, Transform},
    wayland::{
        background_effect::BackgroundEffectSurfaceCachedState, compositor::with_states,
        shell::wlr_layer::Layer as WlrLayer,
    },
};

#[cfg(feature = "debug")]
use crate::drawing::FpsElement;
use crate::{
    config::{BlurConfig, Config},
    drawing::{CLEAR_COLOR, CLEAR_COLOR_FULLSCREEN, PointerRenderElement},
    render_helpers::{blur::BlurOptions, framebuffer_effect::FramebufferEffectElement},
    shell::{
        FullscreenSurface, WindowElement, WindowRenderElement,
        closing_window::ClosingWindowRenderElement,
    },
};

#[cfg(feature = "udev")]
use crate::shell::UdevMultiRenderer;

smithay::backend::renderer::element::render_elements! {
    pub CustomRenderElements<R> where
        R: ImportAll + ImportMem;
    Pointer=PointerRenderElement<R>,
    Surface=WaylandSurfaceRenderElement<R>,
    #[cfg(feature = "debug")]
    // Note: We would like to borrow this element instead, but that would introduce
    // a feature-dependent lifetime, which introduces a lot more feature bounds
    // as the whole type changes and we can't have an unused lifetime (for when "debug" is disabled)
    // in the declaration.
    Fps=FpsElement<R::TextureId>,
}

impl<R: Renderer> std::fmt::Debug for CustomRenderElements<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pointer(arg0) => f.debug_tuple("Pointer").field(arg0).finish(),
            Self::Surface(arg0) => f.debug_tuple("Surface").field(arg0).finish(),
            #[cfg(feature = "debug")]
            Self::Fps(arg0) => f.debug_tuple("Fps").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElements<R, E> where R: ImportAll + ImportMem;
    Space=SpaceRenderElements<R, E>,
    Window=Wrap<E>,
    Custom=CustomRenderElements<R>,
    Preview=CropRenderElement<RelocateRenderElement<RescaleRenderElement<E>>>,
    OpenAnim=RescaleRenderElement<E>,
}

impl<R: Renderer + ImportAll + ImportMem, E: RenderElement<R> + std::fmt::Debug> std::fmt::Debug
    for OutputRenderElements<R, E>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Space(arg0) => f.debug_tuple("Space").field(arg0).finish(),
            Self::Window(arg0) => f.debug_tuple("Window").field(arg0).finish(),
            Self::Custom(arg0) => f.debug_tuple("Custom").field(arg0).finish(),
            Self::Preview(arg0) => f.debug_tuple("Preview").field(arg0).finish(),
            Self::OpenAnim(arg0) => f.debug_tuple("OpenAnim").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

/// Wrapper around [`OutputRenderElements`] that can also hold a [`FramebufferEffectElement`]
/// for background blur effects.
///
/// `FramebufferEffectElement` only implements `RenderElement<GlesRenderer>`, so we can't put it
/// directly into the generic `OutputRenderElements<R, E>`. This wrapper manually implements
/// `RenderElement<GlesRenderer>` and `RenderElement<UdevMultiRenderer>` (udev feature),
/// delegating the Blur variant to `RenderElement<GlesRenderer>` via `frame.as_mut()`.
pub enum OutputRenderElementsWithBlur<R, E>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + 'static,
    E: RenderElement<R>,
{
    Output(OutputRenderElements<R, E>),
    Blur(FramebufferEffectElement),
    ClosingWindow(ClosingWindowRenderElement),
    ResizeSnapshot(crate::layout::ResizeSnapshotRenderElement),
}

impl<R, E> std::fmt::Debug for OutputRenderElementsWithBlur<R, E>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + 'static,
    E: RenderElement<R> + std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Output(e) => f.debug_tuple("Output").field(e).finish(),
            Self::Blur(e) => f.debug_tuple("Blur").field(e).finish(),
            Self::ClosingWindow(e) => f.debug_tuple("ClosingWindow").field(e).finish(),
            Self::ResizeSnapshot(e) => f.debug_tuple("ResizeSnapshot").field(e).finish(),
        }
    }
}

impl<R, E> From<OutputRenderElements<R, E>> for OutputRenderElementsWithBlur<R, E>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + 'static,
    E: RenderElement<R>,
{
    fn from(e: OutputRenderElements<R, E>) -> Self {
        Self::Output(e)
    }
}

impl<R, E> Element for OutputRenderElementsWithBlur<R, E>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + 'static,
    E: RenderElement<R>,
{
    fn id(&self) -> &Id {
        match self {
            Self::Output(e) => e.id(),
            Self::Blur(e) => e.id(),
            Self::ClosingWindow(e) => e.id(),
            Self::ResizeSnapshot(e) => e.id(),
        }
    }

    fn current_commit(&self) -> CommitCounter {
        match self {
            Self::Output(e) => e.current_commit(),
            Self::Blur(e) => e.current_commit(),
            Self::ClosingWindow(e) => e.current_commit(),
            Self::ResizeSnapshot(e) => e.current_commit(),
        }
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, smithay::utils::Physical> {
        match self {
            Self::Output(e) => e.geometry(scale),
            Self::Blur(e) => e.geometry(scale),
            Self::ClosingWindow(e) => e.geometry(scale),
            Self::ResizeSnapshot(e) => e.geometry(scale),
        }
    }

    fn transform(&self) -> Transform {
        match self {
            Self::Output(e) => e.transform(),
            Self::Blur(e) => e.transform(),
            Self::ClosingWindow(e) => e.transform(),
            Self::ResizeSnapshot(e) => e.transform(),
        }
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        match self {
            Self::Output(e) => e.src(),
            Self::Blur(e) => e.src(),
            Self::ClosingWindow(e) => e.src(),
            Self::ResizeSnapshot(e) => e.src(),
        }
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, smithay::utils::Physical> {
        match self {
            Self::Output(e) => e.damage_since(scale, commit),
            Self::Blur(e) => e.damage_since(scale, commit),
            Self::ClosingWindow(e) => e.damage_since(scale, commit),
            Self::ResizeSnapshot(e) => e.damage_since(scale, commit),
        }
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, smithay::utils::Physical> {
        match self {
            Self::Output(e) => e.opaque_regions(scale),
            Self::Blur(e) => e.opaque_regions(scale),
            Self::ClosingWindow(e) => e.opaque_regions(scale),
            Self::ResizeSnapshot(e) => e.opaque_regions(scale),
        }
    }

    fn alpha(&self) -> f32 {
        match self {
            Self::Output(e) => e.alpha(),
            Self::Blur(e) => e.alpha(),
            Self::ClosingWindow(e) => e.alpha(),
            Self::ResizeSnapshot(e) => e.alpha(),
        }
    }

    fn kind(&self) -> Kind {
        match self {
            Self::Output(e) => e.kind(),
            Self::Blur(e) => e.kind(),
            Self::ClosingWindow(e) => e.kind(),
            Self::ResizeSnapshot(e) => e.kind(),
        }
    }

    fn is_framebuffer_effect(&self) -> bool {
        match self {
            Self::Output(e) => e.is_framebuffer_effect(),
            Self::Blur(e) => e.is_framebuffer_effect(),
            Self::ClosingWindow(e) => e.is_framebuffer_effect(),
            Self::ResizeSnapshot(e) => e.is_framebuffer_effect(),
        }
    }
}

impl<E> RenderElement<GlesRenderer> for OutputRenderElementsWithBlur<GlesRenderer, E>
where
    E: RenderElement<GlesRenderer>,
    OutputRenderElements<GlesRenderer, E>: RenderElement<GlesRenderer>,
{
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, smithay::utils::Physical>,
        damage: &[Rectangle<i32, smithay::utils::Physical>],
        opaque_regions: &[Rectangle<i32, smithay::utils::Physical>],
        cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), GlesError> {
        match self {
            Self::Output(e) => e.draw(frame, src, dst, damage, opaque_regions, cache),
            Self::Blur(e) => RenderElement::<GlesRenderer>::draw(
                e,
                frame,
                src,
                dst,
                damage,
                opaque_regions,
                cache,
            ),
            Self::ClosingWindow(e) => RenderElement::<GlesRenderer>::draw(
                e,
                frame,
                src,
                dst,
                damage,
                opaque_regions,
                cache,
            ),
            Self::ResizeSnapshot(e) => RenderElement::<GlesRenderer>::draw(
                e,
                frame,
                src,
                dst,
                damage,
                opaque_regions,
                cache,
            ),
        }
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        match self {
            Self::Output(e) => e.underlying_storage(renderer),
            Self::Blur(e) => e.underlying_storage(renderer),
            Self::ClosingWindow(e) => e.underlying_storage(renderer),
            Self::ResizeSnapshot(e) => e.underlying_storage(renderer),
        }
    }

    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, smithay::utils::Physical>,
        cache: &smithay::utils::user_data::UserDataMap,
    ) -> Result<(), GlesError> {
        match self {
            Self::Output(e) => e.capture_framebuffer(frame, src, dst, cache),
            Self::Blur(e) => {
                RenderElement::<GlesRenderer>::capture_framebuffer(e, frame, src, dst, cache)
            }
            Self::ClosingWindow(e) => {
                RenderElement::<GlesRenderer>::capture_framebuffer(e, frame, src, dst, cache)
            }
            Self::ResizeSnapshot(e) => {
                RenderElement::<GlesRenderer>::capture_framebuffer(e, frame, src, dst, cache)
            }
        }
    }
}

#[cfg(feature = "udev")]
impl<'a, 'b, E> RenderElement<UdevMultiRenderer<'a, 'b>>
    for OutputRenderElementsWithBlur<UdevMultiRenderer<'a, 'b>, E>
where
    E: RenderElement<UdevMultiRenderer<'a, 'b>>,
    OutputRenderElements<UdevMultiRenderer<'a, 'b>, E>: RenderElement<UdevMultiRenderer<'a, 'b>>,
{
    fn draw(
        &self,
        frame: &mut <UdevMultiRenderer<'a, 'b> as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, smithay::utils::Physical>,
        damage: &[Rectangle<i32, smithay::utils::Physical>],
        opaque_regions: &[Rectangle<i32, smithay::utils::Physical>],
        cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), <UdevMultiRenderer<'a, 'b> as RendererSuper>::Error> {
        match self {
            Self::Output(e) => e.draw(frame, src, dst, damage, opaque_regions, cache),
            Self::Blur(e) => RenderElement::<GlesRenderer>::draw(
                e,
                frame.as_mut(),
                src,
                dst,
                damage,
                opaque_regions,
                cache,
            )
            .map_err(Into::into),
            Self::ClosingWindow(e) => RenderElement::<GlesRenderer>::draw(
                e,
                frame.as_mut(),
                src,
                dst,
                damage,
                opaque_regions,
                cache,
            )
            .map_err(Into::into),
            Self::ResizeSnapshot(e) => RenderElement::<GlesRenderer>::draw(
                e,
                frame.as_mut(),
                src,
                dst,
                damage,
                opaque_regions,
                cache,
            )
            .map_err(Into::into),
        }
    }

    fn underlying_storage(
        &self,
        renderer: &mut UdevMultiRenderer<'a, 'b>,
    ) -> Option<UnderlyingStorage<'_>> {
        let gles = renderer.as_mut();
        match self {
            Self::Output(e) => e.underlying_storage(renderer),
            Self::Blur(e) => e.underlying_storage(gles),
            Self::ClosingWindow(e) => e.underlying_storage(gles),
            Self::ResizeSnapshot(e) => e.underlying_storage(gles),
        }
    }

    fn capture_framebuffer(
        &self,
        frame: &mut <UdevMultiRenderer<'a, 'b> as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, smithay::utils::Physical>,
        cache: &smithay::utils::user_data::UserDataMap,
    ) -> Result<(), <UdevMultiRenderer<'a, 'b> as RendererSuper>::Error> {
        match self {
            Self::Output(e) => e.capture_framebuffer(frame, src, dst, cache),
            Self::Blur(e) => RenderElement::<GlesRenderer>::capture_framebuffer(
                e,
                frame.as_mut(),
                src,
                dst,
                cache,
            )
            .map_err(Into::into),
            Self::ClosingWindow(e) => RenderElement::<GlesRenderer>::capture_framebuffer(
                e,
                frame.as_mut(),
                src,
                dst,
                cache,
            )
            .map_err(Into::into),
            Self::ResizeSnapshot(e) => RenderElement::<GlesRenderer>::capture_framebuffer(
                e,
                frame.as_mut(),
                src,
                dst,
                cache,
            )
            .map_err(Into::into),
        }
    }
}

pub fn space_preview_elements<'a, R, C>(
    renderer: &'a mut R,
    space: &'a Space<WindowElement>,
    output: &'a Output,
) -> impl Iterator<Item = C> + 'a
where
    R: Renderer + ImportAll + ImportMem,
    WindowElement: AsRenderElements<R, RenderElement = WindowRenderElement>,
    C: From<CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement>>>>
        + 'a,
{
    let constrain_behavior = ConstrainBehavior {
        reference: ConstrainReference::BoundingBox,
        behavior: ConstrainScaleBehavior::Fit,
        align: ConstrainAlign::CENTER,
    };

    let preview_padding = 10;

    let elements: Vec<_> = space.elements_for_output(output).collect();
    let elements_on_space = elements.len();
    let output_scale = output.current_scale().fractional_scale();
    let output_transform = output.current_transform();
    let output_size = output
        .current_mode()
        .map(|mode| {
            output_transform
                .transform_size(mode.size)
                .to_f64()
                .to_logical(output_scale)
        })
        .unwrap_or_default();

    let max_elements_per_row = 4;
    let elements_per_row = usize::min(elements_on_space, max_elements_per_row);
    let rows = f64::ceil(elements_on_space as f64 / elements_per_row as f64);

    let preview_size = Size::from((
        f64::round(output_size.w / elements_per_row as f64) as i32 - preview_padding * 2,
        f64::round(output_size.h / rows) as i32 - preview_padding * 2,
    ));

    elements
        .into_iter()
        .enumerate()
        .flat_map(move |(element_index, window)| {
            let column = element_index % elements_per_row;
            let row = element_index / elements_per_row;
            let preview_location = Point::from((
                preview_padding + (preview_padding + preview_size.w) * column as i32,
                preview_padding + (preview_padding + preview_size.h) * row as i32,
            ));
            let constrain = Rectangle::new(preview_location, preview_size);
            constrain_space_element(
                renderer,
                window,
                preview_location,
                1.0,
                output_scale,
                constrain,
                constrain_behavior,
            )
        })
}

/// Result of resolving the effective blur config for a window.
struct ResolvedWindowBlur {
    /// The effective blur configuration (global + rule overrides).
    blur: BlurConfig,
    /// Whether a matching window-rule explicitly forces blur for this window
    /// (i.e. the rule has blur.enable = true). Used to force blur on windows
    /// that don't have blur_region set via the protocol.
    rule_forces_blur: bool,
}

/// Resolve the effective blur config for a window by checking window rules.
///
/// Rules are evaluated in order; the first matching rule wins. If no rule matches,
/// the global blur config is used.
fn resolve_window_blur(window: &WindowElement, config: &Config) -> ResolvedWindowBlur {
    use smithay::desktop::WindowSurface;
    use smithay::wayland::compositor::with_states;
    use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

    let (title, app_id): (Option<String>, Option<String>) = match window.0.underlying_surface() {
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

    for rule in &config.window_rules {
        let matches = match (&rule.app_id, &rule.title) {
            (Some(rule_id), Some(rule_title)) => {
                app_id.as_ref().map_or(false, |id| id.contains(rule_id))
                    && title.as_ref().map_or(false, |t| t.contains(rule_title))
            }
            (Some(rule_id), None) => app_id.as_ref().map_or(false, |id| id.contains(rule_id)),
            (None, Some(rule_title)) => title.as_ref().map_or(false, |t| t.contains(rule_title)),
            (None, None) => true,
        };

        if matches {
            if let Some(override_blur) = &rule.blur {
                let mut result = config.blur;
                result.enable = override_blur.enable;
                if let Some(passes) = override_blur.passes {
                    result.passes = passes;
                }
                if let Some(offset) = override_blur.offset {
                    result.offset = offset;
                }
                if let Some(xray) = override_blur.xray {
                    result.xray = xray;
                }
                return ResolvedWindowBlur {
                    blur: result,
                    rule_forces_blur: override_blur.enable,
                };
            }
            // Rule matches but has no blur override; keep looking for a rule with blur
        }
    }

    ResolvedWindowBlur {
        blur: config.blur,
        rule_forces_blur: false,
    }
}

/// Resolve the effective blur config for a layer surface by checking layer rules.
fn resolve_layer_blur(namespace: &str, config: &Config) -> BlurConfig {
    for rule in &config.layer_rules {
        let matches = match &rule.namespace {
            Some(rule_ns) => namespace.contains(rule_ns),
            None => true,
        };

        if matches {
            if let Some(override_blur) = &rule.blur {
                let mut result = config.blur;
                result.enable = override_blur.enable;
                if let Some(passes) = override_blur.passes {
                    result.passes = passes;
                }
                if let Some(offset) = override_blur.offset {
                    result.offset = offset;
                }
                if let Some(xray) = override_blur.xray {
                    result.xray = xray;
                }
                return result;
            }
        }
    }

    config.blur
}

#[profiling::function]
pub fn output_elements<R>(
    output: &Output,
    space: &Space<WindowElement>,
    closing_windows: &[crate::shell::closing_window::ClosingWindow],
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &mut R,
    show_window_preview: bool,
    config: &Config,
) -> (
    Vec<OutputRenderElementsWithBlur<R, WindowRenderElement>>,
    Color32F,
)
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + 'static,
    WindowElement: AsRenderElements<R, RenderElement = WindowRenderElement>,
    WindowRenderElement: RenderElement<R>,
    CustomRenderElements<R>: RenderElement<R>,
{
    let _blur_config = config.blur;

    if let Some(window) = output
        .user_data()
        .get::<FullscreenSurface>()
        .and_then(|f| f.get())
    {
        let scale = output.current_scale().fractional_scale().into();
        let window_render_elements: Vec<WindowRenderElement> =
            AsRenderElements::<R>::render_elements(&window, renderer, (0, 0).into(), scale, 1.0);

        let elements = custom_elements
            .into_iter()
            .map(|e| OutputRenderElementsWithBlur::from(OutputRenderElements::from(e)))
            .chain(window_render_elements.into_iter().map(|e| {
                OutputRenderElementsWithBlur::from(OutputRenderElements::Window(Wrap::from(e)))
            }))
            .collect::<Vec<_>>();
        (elements, CLEAR_COLOR_FULLSCREEN)
    } else {
        let mut output_render_elements: Vec<OutputRenderElementsWithBlur<R, WindowRenderElement>> =
            custom_elements
                .into_iter()
                .map(|e| OutputRenderElementsWithBlur::from(OutputRenderElements::from(e)))
                .collect::<Vec<_>>();

        if show_window_preview && space.elements_for_output(output).next().is_some() {
            output_render_elements.extend(
                space_preview_elements::<R, OutputRenderElements<R, WindowRenderElement>>(
                    renderer, space, output,
                )
                .map(OutputRenderElementsWithBlur::from),
            );
        }

        let output_scale = output.current_scale().fractional_scale();
        let layer_map = layer_map_for_output(output);

        // Render layer-shell surfaces in the correct z-order.
        //
        // `render_output_internal` draws elements via `iter().rev()`, so elements at the end
        // of the vec are drawn first (bottom) and elements at the start are drawn last (top).
        let render_layer =
            |renderer: &mut R,
             layer: WlrLayer,
             elements: &mut Vec<OutputRenderElementsWithBlur<R, WindowRenderElement>>| {
                let surfaces: Vec<_> = layer_map.layers_on(layer).collect();
                for surface in surfaces {
                    let namespace = surface.namespace().to_owned();
                    let layer_blur = resolve_layer_blur(&namespace, config);

                    if let Some(geo) = layer_map.layer_geometry(surface) {
                        let rendered: Vec<WaylandSurfaceRenderElement<R>> =
                            AsRenderElements::<R>::render_elements(
                                surface,
                                renderer,
                                geo.loc.to_physical_precise_round(output_scale),
                                Scale::from(output_scale),
                                1.0,
                            );
                        elements.extend(rendered.into_iter().map(|e| {
                            OutputRenderElementsWithBlur::from(OutputRenderElements::Space(
                                SpaceRenderElements::Surface(e),
                            ))
                        }));

                        // Layer-rule blur: if a matching rule enables blur for this layer surface,
                        // insert a blur element right after it (drawn before it in rev order).
                        if layer_blur.enable {
                            let geometry = geo.to_f64();
                            let blur_elem = FramebufferEffectElement::new(
                                geometry,
                                output_scale,
                                Some(BlurOptions {
                                    passes: layer_blur.passes,
                                    offset: layer_blur.offset,
                                }),
                            );
                            elements.push(OutputRenderElementsWithBlur::Blur(blur_elem));
                        }
                    }
                }
            };

        // Upper layers (rendered on top of windows)
        render_layer(renderer, WlrLayer::Overlay, &mut output_render_elements);
        render_layer(renderer, WlrLayer::Top, &mut output_render_elements);

        // Windows + blur
        //
        // xray mode controls what the blur captures behind the window:
        //   xray ON  (default): blur only captures layer shell (background+bottom).
        //            All blur elements are grouped after all windows in the vec,
        //            so they are drawn after background+bottom but before windows.
        //   xray OFF: blur captures everything behind (layer shell + other windows).
        //            Each window's blur element is inserted right after that window
        //            in the vec, so it captures whatever was already drawn.
        //
        // Vec order (top -> bottom), draw via iter().rev():
        //   xray ON:  [custom, overlay, top, window_A, window_B, blur, bottom, background]
        //             draw: bg → bottom → blur → B → A → top → overlay → custom
        //   xray OFF: [custom, overlay, top, window_A, blur_A, window_B, blur_B, bottom, background]
        //             draw: bg → bottom → blur_B → B → blur_A → A → top → overlay → custom
        if let Some(output_geo) = space.output_geometry(output) {
            // Collect per-window blur for xray ON mode (inserted after all windows)
            let mut xray_blur_elements: Vec<OutputRenderElementsWithBlur<R, WindowRenderElement>> =
                Vec::new();

            // Iterate windows in z-order (topmost first, matching render_elements_for_region's .rev())
            let windows: Vec<_> = space.elements_for_output(output).collect();
            for window in windows.iter().rev() {
                // Skip rendering until the window is properly centered
                // (avoids a position flash on the first frame before centering)
                if window.decoration_state().needs_center {
                    continue;
                }
                // Skip windows on inactive workspaces (fully hidden).
                {
                    let ws = window.decoration_state();
                    if ws.hidden && ws.fade_anim.is_none() {
                        continue;
                    }
                }

                let win_geo = match space.element_geometry(window) {
                    Some(geo) => geo,
                    None => continue,
                };
                let location = win_geo.loc.to_physical_precise_round(output_scale);

                // Render this window's elements
                let window_elements: Vec<WindowRenderElement> =
                    AsRenderElements::<R>::render_elements(
                        &**window,
                        renderer,
                        location - output_geo.loc.to_physical_precise_round(output_scale),
                        Scale::from(output_scale),
                        1.0,
                    );

                for elem in window_elements {
                    // Check if this window has an active open animation
                    let open_anim = window.decoration_state().open_animation.clone();
                    if let Some(ref anim) = open_anim {
                        let progress = anim.clamped_value().clamp(0., 1.);
                        if !anim.is_done() {
                            // Scale from config scale to 1.0 based on animation progress
                            let start_scale = config.animations.window_open.scale;
                            let scale_factor = start_scale + progress * (1.0 - start_scale);
                            // Scale from center of window
                            let center = Point::<i32, smithay::utils::Physical>::from((
                                (win_geo.size.w / 2),
                                (win_geo.size.h / 2),
                            ));
                            let scaled = RescaleRenderElement::from_element(
                                elem,
                                center,
                                scale_factor.max(0.),
                            );
                            output_render_elements.push(OutputRenderElementsWithBlur::from(
                                OutputRenderElements::OpenAnim(scaled),
                            ));
                        } else {
                            output_render_elements.push(OutputRenderElementsWithBlur::from(
                                OutputRenderElements::Space(SpaceRenderElements::Element(
                                    Wrap::from(elem),
                                )),
                            ));
                        }
                    } else {
                        output_render_elements.push(OutputRenderElementsWithBlur::from(
                            OutputRenderElements::Space(SpaceRenderElements::Element(Wrap::from(
                                elem,
                            ))),
                        ));
                    }
                }

                // Check if this window should have blur
                if let Some(wl_surface) = window.wl_surface() {
                    let has_blur_region = with_states(&wl_surface, |states| {
                        states
                            .cached_state
                            .get::<BackgroundEffectSurfaceCachedState>()
                            .current()
                            .blur_region
                            .is_some()
                    });

                    let effective_blur = resolve_window_blur(window, config);

                    // Determine if blur should be applied:
                    // - has blur_region (protocol request): apply unless a rule disables it
                    // - no blur_region but a matching rule explicitly enables blur: force blur
                    let should_blur = if has_blur_region {
                        true // protocol request — apply unless rule disables (effective_blur.enable == false)
                    } else {
                        effective_blur.rule_forces_blur // rule explicitly enabled blur for this window
                    };

                    if should_blur && effective_blur.blur.enable {
                        let bbox = space
                            .element_bbox(window)
                            .unwrap_or_else(|| SpaceElement::bbox(window));
                        let geometry =
                            Rectangle::new(bbox.loc - output_geo.loc, bbox.size).to_f64();
                        let blur_elem = FramebufferEffectElement::new(
                            geometry,
                            output_scale,
                            Some(BlurOptions {
                                passes: effective_blur.blur.passes,
                                offset: effective_blur.blur.offset,
                            }),
                        );

                        if effective_blur.blur.xray {
                            // xray ON: group all blur elements together
                            xray_blur_elements.push(OutputRenderElementsWithBlur::Blur(blur_elem));
                        } else {
                            // xray OFF: insert right after this window
                            output_render_elements
                                .push(OutputRenderElementsWithBlur::Blur(blur_elem));
                        }
                    }
                }

            }

            // Append xray blur elements after all windows (drawn after bg+bottom but before windows)
            output_render_elements.extend(xray_blur_elements);

            // Render closing window animations
            if !closing_windows.is_empty() {
                let output_scale_val = output.current_scale().fractional_scale();
                let output_geo_val = space.output_geometry(output).unwrap_or_default();
                let view_rect = output_geo_val.to_f64();

                for closing in closing_windows {
                    let elem = closing.render(view_rect, Scale::from(output_scale_val));
                    output_render_elements.push(OutputRenderElementsWithBlur::ClosingWindow(elem));
                }
            }
        }

        // Lower layers (rendered below windows). Background is the bottom-most layer, so it
        // must appear last in the vec (drawn first).
        render_layer(renderer, WlrLayer::Bottom, &mut output_render_elements);
        render_layer(renderer, WlrLayer::Background, &mut output_render_elements);

        (output_render_elements, CLEAR_COLOR)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_output<'a, 'd, R>(
    output: &'a Output,
    space: &'a Space<WindowElement>,
    closing_windows: &[crate::shell::closing_window::ClosingWindow],
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &'a mut R,
    framebuffer: &'a mut R::Framebuffer<'_>,
    damage_tracker: &'d mut OutputDamageTracker,
    age: usize,
    show_window_preview: bool,
    config: &Config,
) -> Result<RenderOutputResult<'d>, OutputDamageTrackerError<R::Error>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + 'static,
    WindowElement: AsRenderElements<R, RenderElement = WindowRenderElement>,
    WindowRenderElement: RenderElement<R>,
    CustomRenderElements<R>: RenderElement<R>,
    OutputRenderElements<R, WindowRenderElement>: RenderElement<R>,
    OutputRenderElementsWithBlur<R, WindowRenderElement>: RenderElement<R>,
{
    let (elements, clear_color) = output_elements(
        output,
        space,
        closing_windows,
        custom_elements,
        renderer,
        show_window_preview,
        config,
    );
    damage_tracker.render_output(renderer, framebuffer, age, &elements, clear_color)
}
