pub mod border;
pub mod clipped_surface;
pub mod render_elements;
pub mod resources;
pub mod shader_element;
pub mod shaders;
pub mod shadow;

#[cfg(feature = "xdp-gnome-screencast")]
use anyhow::Context;

#[cfg(feature = "xdp-gnome-screencast")]
pub struct RenderCtx<'a, R> {
    pub renderer: &'a mut R,
    pub target: RenderTarget,
}

#[cfg(feature = "xdp-gnome-screencast")]
impl<'a, R> RenderCtx<'a, R> {
    #[inline]
    pub fn r<'b>(&'b mut self) -> RenderCtx<'b, R> {
        RenderCtx {
            renderer: self.renderer,
            target: self.target,
        }
    }
}

#[cfg(feature = "xdp-gnome-screencast")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTarget {
    Output = 0,
    Screencast,
    ScreenCapture,
}

#[cfg(feature = "xdp-gnome-screencast")]
pub fn encompassing_geo(
    scale: smithay::utils::Scale<f64>,
    elements: impl Iterator<Item = impl smithay::backend::renderer::element::Element>,
) -> smithay::utils::Rectangle<i32, smithay::utils::Physical> {
    elements
        .map(|ele| ele.geometry(scale))
        .reduce(|a, b| a.merge(b))
        .unwrap_or_default()
}

#[cfg(feature = "xdp-gnome-screencast")]
pub fn render_and_download(
    mut renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    size: smithay::utils::Size<i32, smithay::utils::Physical>,
    scale: smithay::utils::Scale<f64>,
    transform: smithay::utils::Transform,
    fourcc: smithay::backend::allocator::Fourcc,
    elements: impl Iterator<Item = impl smithay::backend::renderer::element::RenderElement<smithay::backend::renderer::gles::GlesRenderer>>,
) -> anyhow::Result<smithay::backend::renderer::gles::GlesMapping> {
    use smithay::backend::renderer::{Bind, ExportMem, Frame, Offscreen, Renderer, Texture};
    use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
    use smithay::utils::Rectangle;

    let buffer_size = size.to_logical(1).to_buffer(1, transform);
    let mut texture: GlesTexture = <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(&mut renderer, fourcc, buffer_size)
        .context("error creating texture")?;
    let mut target = <GlesRenderer as Bind<GlesTexture>>::bind(&mut renderer, &mut texture)
        .context("error binding texture")?;

    let output_transform = transform.invert();
    let output_rect = Rectangle::from_size(output_transform.transform_size(size));

    let mut frame = renderer.render(&mut target, size, output_transform)
        .context("error starting frame")?;
    frame.clear(smithay::backend::renderer::Color32F::TRANSPARENT, &[output_rect])
        .context("error clearing")?;

    for element in elements {
        let geo = element.geometry(scale);
        let src = element.src();
        element.draw(
            &mut frame,
            src,
            geo,
            &[geo],
            &[],
            None,
        ).context("error drawing element")?;
    }

    let _sync_point = frame.finish().context("error finishing frame")?;

    let target_size = target.size();
    renderer.copy_framebuffer(&target, Rectangle::from_size(target_size), fourcc)
        .context("error copying framebuffer")
}

#[cfg(feature = "xdp-gnome-screencast")]
pub fn clear_dmabuf(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    mut dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
) -> anyhow::Result<smithay::backend::renderer::sync::SyncPoint> {
    use smithay::backend::renderer::{Bind, Frame, Renderer};
    use smithay::backend::allocator::Buffer;
    use smithay::utils::Rectangle;

    let size = dmabuf.size();
    let size = size.to_logical(1, smithay::utils::Transform::Normal).to_physical(smithay::utils::Scale::from(1));
    let mut target = renderer.bind(&mut dmabuf).context("error binding dmabuf")?;
    let mut frame = renderer.render(&mut target, size, smithay::utils::Transform::Normal)
        .context("error starting frame")?;
    frame.clear(smithay::backend::renderer::Color32F::TRANSPARENT, &[Rectangle::from_size(size)])
        .context("error clearing")?;
    frame.finish().context("error finishing frame")
}

#[cfg(feature = "xdp-gnome-screencast")]
pub fn render_to_dmabuf(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    damage_tracker: &mut smithay::backend::renderer::damage::OutputDamageTracker,
    dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
    elements: &[impl smithay::backend::renderer::element::RenderElement<smithay::backend::renderer::gles::GlesRenderer>],
    states: smithay::backend::renderer::element::RenderElementStates,
) -> anyhow::Result<smithay::backend::renderer::sync::SyncPoint> {
    use smithay::backend::renderer::Bind;
    use smithay::backend::renderer::damage::RenderOutputResult;
    use smithay::backend::allocator::Buffer;
    use smithay::utils::Size;

    let (size, _scale, _transform): (Size<i32, smithay::utils::Physical>, _, _) =
        damage_tracker.mode().try_into().unwrap();

    let dmabuf_size = dmabuf.size();
    tracing::info!("render_to_dmabuf: dmabuf_size={:?}, tracker_size={:?}, elements={}", dmabuf_size, size, elements.len());
    anyhow::ensure!(
        dmabuf_size.w == size.w && dmabuf_size.h == size.h,
        "invalid buffer size: dmabuf={:?} expected={:?}",
        dmabuf_size, size
    );

    let mut dmabuf_clone = dmabuf.clone();
    let mut target = renderer.bind(&mut dmabuf_clone).map_err(|e| {
        anyhow::anyhow!("error binding dmabuf: {:?}", e)
    })?;

    let clear_color = smithay::backend::renderer::Color32F::TRANSPARENT;
    let res: RenderOutputResult<'_> = damage_tracker
        .render_output_with_states(
            renderer,
            &mut target,
            0,
            elements,
            clear_color,
            states,
        )
        .map_err(|e| {
            anyhow::anyhow!("error rendering to dmabuf: {:?}", e)
        })?;

    tracing::info!("render_to_dmabuf: render_output_with_states completed, damage={:?}", res.damage);

    Ok(res.sync)
}