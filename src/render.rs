use smithay::{
    backend::renderer::{
        Color32F, ImportAll, ImportMem, Renderer, Texture,
        damage::{Error as OutputDamageTrackerError, OutputDamageTracker, RenderOutputResult},
        element::{
            AsRenderElements, RenderElement, Wrap,
            surface::WaylandSurfaceRenderElement,
            utils::{
                ConstrainAlign, ConstrainScaleBehavior, CropRenderElement, RelocateRenderElement,
                RescaleRenderElement,
            },
        },
    },
    desktop::{
        layer_map_for_output,
        space::{
            ConstrainBehavior, ConstrainReference, Space, SpaceRenderElements,
            constrain_space_element,
        },
    },
    output::Output,
    utils::{Point, Rectangle, Scale, Size},
    wayland::shell::wlr_layer::Layer as WlrLayer,
};

#[cfg(feature = "debug")]
use crate::drawing::FpsElement;
use crate::{
    drawing::{CLEAR_COLOR, CLEAR_COLOR_FULLSCREEN, PointerRenderElement},
    shell::{FullscreenSurface, WindowElement, WindowRenderElement},
};

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
) -> (Vec<OutputRenderElements<R, WindowRenderElement>>, Color32F)
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
            .map(OutputRenderElements::from)
            .chain(
                window_render_elements
                    .into_iter()
                    .map(|e| OutputRenderElements::Window(Wrap::from(e))),
            )
            .collect::<Vec<_>>();
        (elements, CLEAR_COLOR_FULLSCREEN)
    } else {
        let mut output_render_elements = custom_elements
            .into_iter()
            .map(OutputRenderElements::from)
            .collect::<Vec<_>>();

        if show_window_preview && space.elements_for_output(output).next().is_some() {
            output_render_elements.extend(space_preview_elements(renderer, space, output));
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
        //   [custom, overlay, top, window, bottom, background]
        let render_layer = |renderer: &mut R,
                            layer: WlrLayer,
                            elements: &mut Vec<OutputRenderElements<R, WindowRenderElement>>| {
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
                    elements.extend(
                        rendered
                            .into_iter()
                            .map(|e| OutputRenderElements::Space(SpaceRenderElements::Surface(e))),
                    );
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
                        OutputRenderElements::Space(SpaceRenderElements::Element(Wrap::from(e)))
                    }),
            );
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
) -> Result<RenderOutputResult<'d>, OutputDamageTrackerError<R::Error>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + 'static,
    WindowElement: AsRenderElements<R, RenderElement = WindowRenderElement>,
    WindowRenderElement: RenderElement<R>,
    CustomRenderElements<R>: RenderElement<R>,
    OutputRenderElements<R, WindowRenderElement>: RenderElement<R>,
{
    let (elements, clear_color) =
        output_elements(output, space, custom_elements, renderer, show_window_preview);
    damage_tracker.render_output(renderer, framebuffer, age, &elements, clear_color)
}