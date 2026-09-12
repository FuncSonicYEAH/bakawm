//! Texture buffer with stable element ID for animations.
//!
//! Based on niri's TextureBuffer implementation.

use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
use smithay::backend::renderer::{ContextId, Frame, Renderer, Texture};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use tracing::warn;

/// A texture buffer with a stable element ID.
///
/// The ID is created once at construction time and does not change,
/// which prevents full-screen damage when used across multiple frames.
#[derive(Debug, Clone)]
pub struct TextureBuffer<T: Texture> {
    id: Id,
    commit_counter: CommitCounter,
    renderer_context_id: ContextId<T>,
    texture: T,
    scale: Scale<f64>,
    transform: Transform,
    opaque_regions: Vec<Rectangle<i32, Buffer>>,
}

/// Render element for a [`TextureBuffer`].
#[derive(Debug, Clone)]
pub struct TextureRenderElement<T: Texture> {
    buffer: TextureBuffer<T>,
    location: Point<f64, Logical>,
    alpha: f32,
    src: Option<Rectangle<f64, Logical>>,
    size: Option<Size<f64, Logical>>,
    kind: Kind,
}

impl<T: Texture> TextureBuffer<T> {
    pub fn from_texture<R: Renderer<TextureId = T>>(
        renderer: &R,
        texture: T,
        scale: impl Into<Scale<f64>>,
        transform: Transform,
        opaque_regions: Vec<Rectangle<i32, Buffer>>,
    ) -> Self {
        return TextureBuffer {
            id: Id::new(),
            commit_counter: CommitCounter::default(),
            renderer_context_id: renderer.context_id(),
            texture,
            scale: scale.into(),
            transform,
            opaque_regions,
        }
    }

    pub fn texture(&self) -> &T {
        return &self.texture
    }

    pub fn texture_scale(&self) -> Scale<f64> {
        return self.scale
    }

    pub fn texture_transform(&self) -> Transform {
        return self.transform
    }
}

impl<T: Texture> TextureBuffer<T> {
    pub fn logical_size(&self) -> Size<f64, Logical> {
        return self.texture
            .size()
            .to_f64()
            .to_logical(self.scale, self.transform)
    }
}

impl TextureBuffer<GlesTexture> {
    pub fn is_texture_reference_unique(&mut self) -> bool {
        return self.texture.is_unique_reference()
    }
}

impl<T: Texture> TextureRenderElement<T> {
    pub fn from_texture_buffer(
        buffer: TextureBuffer<T>,
        location: impl Into<Point<f64, Logical>>,
        alpha: f32,
        src: Option<Rectangle<f64, Logical>>,
        size: Option<Size<f64, Logical>>,
        kind: Kind,
    ) -> Self {
        return TextureRenderElement {
            buffer,
            location: location.into(),
            alpha,
            src,
            size,
            kind,
        }
    }

    pub fn buffer(&self) -> &TextureBuffer<T> {
        return &self.buffer
    }
}

impl<T: Texture> TextureRenderElement<T> {
    pub fn logical_size(&self) -> Size<f64, Logical> {
        return self.size
            .or_else(|| return self.src.map(|src| return src.size))
            .unwrap_or_else(|| return self.buffer.logical_size())
    }

    pub fn logical_src(&self) -> Rectangle<f64, Logical> {
        return self.src
            .unwrap_or_else(|| return Rectangle::from_size(self.logical_size()))
    }
}

impl<T: Texture> Element for TextureRenderElement<T> {
    fn id(&self) -> &Id {
        return &self.buffer.id
    }

    fn current_commit(&self) -> CommitCounter {
        return self.buffer.commit_counter
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        let logical_geo = Rectangle::new(self.location, self.logical_size());
        return logical_geo.to_physical_precise_round(scale)
    }

    fn transform(&self) -> Transform {
        return self.buffer.transform
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        return self.src
            .map(|src| {
                return src.to_buffer(
                    self.buffer.scale,
                    self.buffer.transform,
                    &self.buffer.logical_size(),
                )
            })
            .unwrap_or_else(|| return Rectangle::from_size(self.buffer.texture.size()).to_f64())
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let texture_size = self.buffer.texture.size().to_f64();
        let src = self.src();

        return self.buffer
            .opaque_regions
            .iter()
            .filter_map(|region| {
                let mut region = region.to_f64().intersection(src)?;

                region.loc -= src.loc;
                region = region.upscale(texture_size / src.size);

                let logical =
                    region.to_logical(self.buffer.scale, self.buffer.transform, &src.size);
                return Some(logical.to_physical_precise_down(scale))
            })
            .collect()
    }

    fn alpha(&self) -> f32 {
        return self.alpha
    }

    fn kind(&self) -> Kind {
        return self.kind
    }
}

impl<R, T> RenderElement<R> for TextureRenderElement<T>
where
    R: Renderer<TextureId = T>,
    T: Texture,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dest: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        if frame.context_id() != self.buffer.renderer_context_id {
            warn!("trying to render texture from different renderer");
            return Ok(());
        }

        return frame.render_texture_from_to(
            &self.buffer.texture,
            src,
            dest,
            damage,
            opaque_regions,
            self.buffer.transform,
            self.alpha,
        )
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        return None
    }
}
