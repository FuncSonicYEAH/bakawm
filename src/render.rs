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
        background_effect::BackgroundEffectSurfaceCachedState,
        compositor::with_states,
        shell::wlr_layer::Layer as WlrLayer,
    },
};

#[cfg(feature = "debug")]
use crate::drawing::FpsElement;
use crate::{
    config::BlurConfig,
    drawing::{CLEAR_COLOR, CLEAR_COLOR_FULLSCREEN, PointerRenderElement},
    render_helpers::{blur::BlurOptions, framebuffer_effect::FramebufferEffectElement},
    shell::{FullscreenSurface, WindowElement, WindowRenderElement},
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
        }
    }

    fn current_commit(&self) -> CommitCounter {
        match self {
            Self::Output(e) => e.current_commit(),
            Self::Blur(e) => e.current_commit(),
        }
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, smithay::utils::Physical> {
        match self {
            Self::Output(e) => e.geometry(scale),
            Self::Blur(e) => e.geometry(scale),
        }
    }

    fn transform(&self) -> Transform {
        match self {
            Self::Output(e) => e.transform(),
            Self::Blur(e) => e.transform(),
        }
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        match self {
            Self::Output(e) => e.src(),
            Self::Blur(e) => e.src(),
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
        }
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, smithay::utils::Physical> {
        match self {
            Self::Output(e) => e.opaque_regions(scale),
            Self::Blur(e) => e.opaque_regions(scale),
        }
    }

    fn alpha(&self) -> f32 {
        match self {
            Self::Output(e) => e.alpha(),
            Self::Blur(e) => e.alpha(),
        }
    }

    fn kind(&self) -> Kind {
        match self {
            Self::Output(e) => e.kind(),
            Self::Blur(e) => e.kind(),
        }
    }

    fn is_framebuffer_effect(&self) -> bool {
        match self {
            Self::Output(e) => e.is_framebuffer_effect(),
            Self::Blur(e) => e.is_framebuffer_effect(),
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
            Self::Blur(e) => e.draw(frame, src, dst, damage, opaque_regions, cache),
        }
    }

    fn underlying_storage(
        &self,
        renderer: &mut GlesRenderer,
    ) -> Option<UnderlyingStorage<'_>> {
        match self {
            Self::Output(e) => e.underlying_storage(renderer),
            Self::Blur(e) => e.underlying_storage(renderer),
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
            Self::Blur(e) => e.capture_framebuffer(frame, src, dst, cache),
        }
    }
}

#[cfg(feature = "udev")]
impl<'a, 'b, E> RenderElement<UdevMultiRenderer<'a, 'b>>
    for OutputRenderElementsWithBlur<UdevMultiRenderer<'a, 'b>, E>
where
    E: RenderElement<UdevMultiRenderer<'a, 'b>>,
    OutputRenderElements<UdevMultiRenderer<'a, 'b>, E>:
        RenderElement<UdevMultiRenderer<'a, 'b>>,
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
            Self::Output(e) => {
                e.draw(frame, src, dst, damage, opaque_regions, cache)
            }
            Self::Blur(e) => {
                RenderElement::<GlesRenderer>::draw(
                    e, frame.as_mut(), src, dst, damage, opaque_regions, cache,
                )
                .map_err(Into::into)
            }
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
            Self::Blur(e) => {
                RenderElement::<GlesRenderer>::capture_framebuffer(e, frame.as_mut(), src, dst, cache)
                    .map_err(Into::into)
            }
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
    C: From<CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement>>>> + 'a,
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

#[profiling::function]
pub fn output_elements<R>(
    output: &Output,
    space: &Space<WindowElement>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &mut R,
    show_window_preview: bool,
    blur_config: BlurConfig,
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
            .chain(
                window_render_elements.into_iter().map(|e| {
                    OutputRenderElementsWithBlur::from(OutputRenderElements::Window(Wrap::from(e)))
                }),
            )
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
                    renderer,
                    space,
                    output,
                )
                .map(OutputRenderElementsWithBlur::from),
            );
        }

        let output_scale = output.current_scale().fractional_scale();
        let layer_map = layer_map_for_output(output);

        // Render layer-shell surfaces in the correct z-order.
        //
        // smithay's `space_render_elements` mixes the Background and Bottom layers together
        // relying on insertion order, which breaks when the wallpaper (background) starts
        // after the bar (bottom) and ends up rendered on top of it. We render each layer
        // explicitly in the correct stacking order instead.
        //
        // `render_output_internal` draws elements via `iter().rev()`, so elements at the end
        // of the vec are drawn first (bottom) and elements at the start are drawn last (top).
        // Final vec order (top -> bottom):
        //   [custom, overlay, top, window, blur, bottom, background]
        let render_layer = |renderer: &mut R,
                            layer: WlrLayer,
                            elements: &mut Vec<OutputRenderElementsWithBlur<R, WindowRenderElement>>| {
            let surfaces: Vec<_> = layer_map.layers_on(layer).collect();
            for surface in surfaces {
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
                }
            }
        };

        // Upper layers (rendered on top of windows)
        render_layer(renderer, WlrLayer::Overlay, &mut output_render_elements);
        render_layer(renderer, WlrLayer::Top, &mut output_render_elements);

        // Windows
        if let Some(output_geo) = space.output_geometry(output) {
            output_render_elements.extend(
                space
                    .render_elements_for_region(renderer, &output_geo, output_scale, 1.0)
                    .into_iter()
                    .map(|e| {
                        OutputRenderElementsWithBlur::from(OutputRenderElements::Space(
                            SpaceRenderElements::Element(Wrap::from(e)),
                        ))
                    }),
            );

            // Blur elements for windows with blur regions.
            //
            // These are inserted after windows but before bottom/background in the vec
            // (top -> bottom), so they are drawn after background+bottom but before windows
            // in the reversed draw order. The blur element captures the framebuffer content
            // (background + bottom layers) and applies a blur, then the window is drawn on top.
            for window in space.elements_for_output(output) {
                if let Some(wl_surface) = window.wl_surface() {
                    let has_blur = with_states(&wl_surface, |states| {
                        states
                            .cached_state
                            .get::<BackgroundEffectSurfaceCachedState>()
                            .current()
                            .blur_region
                            .is_some()
                    });
                    if has_blur && blur_config.enable {
                        // Use `space.element_bbox` to get the window bbox in space-global
                        // coordinates. `SpaceElement::bbox(&window)` returns the window's
                        // own bbox (loc relative to the window, usually (0,0)), not its
                        // position in the space.
                        let bbox = space.element_bbox(&window).unwrap_or_else(|| {
                            SpaceElement::bbox(&window)
                        });
                        let geometry = Rectangle::new(
                            bbox.loc - output_geo.loc,
                            bbox.size,
                        )
                        .to_f64();
                        let blur_elem = FramebufferEffectElement::new(
                            geometry,
                            output_scale,
                            Some(BlurOptions {
                                passes: blur_config.passes,
                                offset: blur_config.offset,
                            }),
                        );
                        output_render_elements
                            .push(OutputRenderElementsWithBlur::Blur(blur_elem));
                    }
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
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &'a mut R,
    framebuffer: &'a mut R::Framebuffer<'_>,
    damage_tracker: &'d mut OutputDamageTracker,
    age: usize,
    show_window_preview: bool,
    blur_config: BlurConfig,
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
    let (elements, clear_color) =
        output_elements(output, space, custom_elements, renderer, show_window_preview, blur_config);
    damage_tracker.render_output(renderer, framebuffer, age, &elements, clear_color)
}