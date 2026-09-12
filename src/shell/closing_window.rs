//! Closing window animation.
//!
//! When a window is closed, we capture its contents as a texture and animate
//! the fade-out + scale-down using that texture. This avoids issues with
//! the window surface being destroyed during animation.

use smithay::backend::renderer::element::utils::{
    Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size};

use crate::animation::Animation;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};

/// A window that is currently closing with an animation.
#[derive(Debug)]
pub struct ClosingWindow {
    /// Texture snapshot of the window contents.
    buffer: TextureBuffer<GlesTexture>,
    /// Window geometry size in logical coordinates.
    geo_size: Size<f64, Logical>,
    /// Position of the window in the workspace.
    pos: Point<f64, Logical>,
    /// Texture offset.
    buffer_offset: Point<f64, Logical>,
    /// The closing animation (progress 0 -> 1, where 1 = fully closed).
    anim: Animation,
    /// Scale factor at the end of the animation (from config).
    /// The window scales from 1.0 down to this value.
    end_scale: f64,
}

impl ClosingWindow {
    pub fn new(
        buffer: TextureBuffer<GlesTexture>,
        geo_size: Size<f64, Logical>,
        pos: Point<f64, Logical>,
        buffer_offset: Point<f64, Logical>,
        anim: Animation,
        end_scale: f64,
    ) -> Self {
        return Self {
            buffer,
            geo_size,
            pos,
            buffer_offset,
            anim,
            end_scale,
        }
    }

    /// Whether the closing animation is still ongoing.
    pub fn is_animating(&self) -> bool {
        return !self.anim.is_done()
    }

    /// Get the position of this closing window.
    pub fn pos(&self) -> Point<f64, Logical> {
        return self.pos
    }

    /// Render this closing window as a render element.
    pub fn render(
        &self,
        view_rect: Rectangle<f64, Logical>,
        scale: Scale<f64>,
    ) -> ClosingWindowRenderElement {
        let progress = self.anim.clamped_value().clamp(0., 1.);

        // Alpha fades from 1 to 0.
        let alpha = (1. - progress) as f32;

        // Scale from 1.0 to end_scale based on animation progress.
        // e.g. end_scale=0.0: shrinks to nothing, end_scale=0.8: shrinks to 80%.
        let scale_factor = 1.0 - progress * (1.0 - self.end_scale);

        let elem = TextureRenderElement::from_texture_buffer(
            self.buffer.clone(),
            Point::from((0., 0.)),
            alpha,
            None,
            None,
            Kind::Unspecified,
        );

        // Scale from center of window geometry.
        let center = self.geo_size.to_point().downscale(2.);
        let elem = RescaleRenderElement::from_element(
            elem,
            (center - self.buffer_offset).to_physical_precise_round(scale),
            scale_factor.max(0.),
        );

        // Relocate to the window position relative to the view.
        let mut location = self.pos + self.buffer_offset;
        location.x -= view_rect.loc.x;
        location.y -= view_rect.loc.y;
        let elem = RelocateRenderElement::from_element(
            elem,
            location.to_physical_precise_round(scale),
            Relocate::Relative,
        );

        return ClosingWindowRenderElement(elem)
    }
}

/// Render element wrapper for a closing window.
#[derive(Debug)]
pub struct ClosingWindowRenderElement(
    RelocateRenderElement<RescaleRenderElement<TextureRenderElement<GlesTexture>>>,
);

impl Element for ClosingWindowRenderElement {
    fn id(&self) -> &Id {
        return self.0.id()
    }

    fn current_commit(&self) -> CommitCounter {
        return self.0.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        return self.0.geometry(scale)
    }

    fn transform(&self) -> smithay::utils::Transform {
        return self.0.transform()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        return self.0.src()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> smithay::backend::renderer::utils::DamageSet<i32, Physical> {
        return self.0.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        return self.0.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        return self.0.alpha()
    }

    fn kind(&self) -> Kind {
        return self.0.kind()
    }
}

impl RenderElement<GlesRenderer> for ClosingWindowRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        return RenderElement::<GlesRenderer>::draw(&self.0, frame, src, dst, damage, opaque_regions, cache)
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        return self.0.underlying_storage(renderer)
    }
}

#[cfg(feature = "udev")]
impl<'a, 'b>
    RenderElement<
        smithay::backend::renderer::multigpu::MultiRenderer<
            'a,
            'b,
            smithay::backend::renderer::multigpu::gbm::GbmGlesBackend<
                GlesRenderer,
                smithay::backend::drm::DrmDeviceFd,
            >,
            smithay::backend::renderer::multigpu::gbm::GbmGlesBackend<
                GlesRenderer,
                smithay::backend::drm::DrmDeviceFd,
            >,
        >,
    > for ClosingWindowRenderElement
{
    fn draw(
        &self,
        frame: &mut <smithay::backend::renderer::multigpu::MultiRenderer<
            'a,
            'b,
            smithay::backend::renderer::multigpu::gbm::GbmGlesBackend<
                GlesRenderer,
                smithay::backend::drm::DrmDeviceFd,
            >,
            smithay::backend::renderer::multigpu::gbm::GbmGlesBackend<
                GlesRenderer,
                smithay::backend::drm::DrmDeviceFd,
            >,
        > as smithay::backend::renderer::RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<
        (),
        <smithay::backend::renderer::multigpu::MultiRenderer<
            'a,
            'b,
            smithay::backend::renderer::multigpu::gbm::GbmGlesBackend<
                GlesRenderer,
                smithay::backend::drm::DrmDeviceFd,
            >,
            smithay::backend::renderer::multigpu::gbm::GbmGlesBackend<
                GlesRenderer,
                smithay::backend::drm::DrmDeviceFd,
            >,
        > as smithay::backend::renderer::RendererSuper>::Error,
    > {
        return RenderElement::<GlesRenderer>::draw(
            &self.0,
            frame.as_mut(),
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )
        .map_err(Into::into)
    }

    fn underlying_storage(
        &self,
        renderer: &mut smithay::backend::renderer::multigpu::MultiRenderer<
            'a,
            'b,
            smithay::backend::renderer::multigpu::gbm::GbmGlesBackend<
                GlesRenderer,
                smithay::backend::drm::DrmDeviceFd,
            >,
            smithay::backend::renderer::multigpu::gbm::GbmGlesBackend<
                GlesRenderer,
                smithay::backend::drm::DrmDeviceFd,
            >,
        >,
    ) -> Option<UnderlyingStorage<'_>> {
        return self.0.underlying_storage(renderer.as_mut())
    }
}
