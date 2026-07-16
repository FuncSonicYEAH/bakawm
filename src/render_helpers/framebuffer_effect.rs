use std::cell::RefCell;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::backend::renderer::gles::{
    ffi, GlesError, GlesFrame, GlesRenderer, GlesTexture,
};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Frame as _, FrameContext as _, Offscreen, Texture as _};
use smithay::gpu_span_location;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Transform};

use super::blur::{Blur, BlurOptions};

/// A render element that captures the current framebuffer contents, applies blur,
/// and draws the result back. This is used to implement background blur effects
/// for surfaces that request it via the ext-background-effect-v1 protocol.
#[derive(Debug)]
pub struct FramebufferEffectElement {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<f64, Logical>,
    scale: f32,
    blur_options: Option<BlurOptions>,
}

impl FramebufferEffectElement {
    pub fn new(
        geometry: Rectangle<f64, Logical>,
        scale: f64,
        blur_options: Option<BlurOptions>,
    ) -> Self {
        Self {
            id: Id::new(),
            commit: CommitCounter::default(),
            geometry,
            scale: scale as f32,
            blur_options,
        }
    }

    pub fn damage_all(&mut self) {
        self.commit.increment();
    }
}

#[derive(Debug)]
struct Inner {
    framebuffer: Option<GlesTexture>,
    blur: Option<Blur>,
    intermediate: Option<GlesTexture>,
}

impl Inner {
    fn new(renderer: &mut GlesRenderer) -> Self {
        Inner {
            framebuffer: None,
            blur: Blur::new(renderer),
            intermediate: None,
        }
    }
}

impl Element for FramebufferEffectElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        let size = self.geometry.size.to_buffer(1., Transform::Normal);
        Rectangle::from_size(size)
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry.to_physical_precise_round(scale)
    }

    fn is_framebuffer_effect(&self) -> bool {
        true
    }
}

impl RenderElement<GlesRenderer> for FramebufferEffectElement {
    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let span_loc = gpu_span_location!("FramebufferEffectElement::capture_framebuffer");
        frame.with_gpu_span(span_loc, |frame| {
            let output_rect = Rectangle::from_size(frame.output_size());
            let transform = frame.transformation();

            tracing::trace!(
                ?dst,
                ?output_rect,
                ?transform,
                geometry = ?self.geometry,
                "capture_framebuffer"
            );

            let mut guard = frame.renderer();

            let inner = cache
                .get_or_insert::<RefCell<Inner>, _>(|| RefCell::new(Inner::new(guard.as_mut())));
            let mut inner = inner.borrow_mut();
            let inner = &mut *inner;

            inner.intermediate = None;

            // Clamp dst to the framebuffer bounds.
            let clamped_dst = match dst.intersection(output_rect) {
                Some(clamped) => clamped,
                None => return Ok(()),
            };
            let clamp_scale = clamped_dst.size.to_f64() / dst.size.to_f64();

            let dst = transform.transform_rect_in(clamped_dst, &output_rect.size);

            // Compute size from our geometry and scale.
            let size = src
                .size
                .to_logical(1., Transform::Normal)
                .upscale(clamp_scale)
                .to_physical_precise_round(self.scale);
            let size = transform.transform_size(size);
            let size = size.to_logical(1).to_buffer(1, Transform::Normal);

            // Recreate framebuffer if needed.
            if inner
                .framebuffer
                .as_ref()
                .is_some_and(|fb| fb.size() != size)
            {
                inner.framebuffer = None;
            }
            let framebuffer = if let Some(fb) = &inner.framebuffer {
                fb
            } else {
                let renderer = guard.as_mut();
                let texture = renderer.create_buffer(Fourcc::Abgr8888, size)?;
                inner.framebuffer.insert(texture)
            };

            // Prepare blur textures.
            let mut blur = Option::zip(inner.blur.as_mut(), self.blur_options);
            if let Some((b, options)) = &mut blur {
                let renderer = guard.as_mut();
                if let Err(err) = b.prepare_textures(
                    |fourcc, size| renderer.create_buffer(fourcc, size),
                    framebuffer,
                    *options,
                ) {
                    tracing::warn!("error preparing blur textures: {err:?}");
                    blur = None;
                }
            }

            // We can't use renderer.with_context() as that will reset the GlesFrame binding.
            drop(guard);

            // Blit the framebuffer contents.
            frame.with_context(|gl| unsafe {
                while gl.GetError() != ffi::NO_ERROR {}

                let mut current_fbo = 0i32;
                gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut current_fbo as *mut _);

                gl.Disable(ffi::SCISSOR_TEST);

                let mut fbo = 0;
                gl.GenFramebuffers(1, &mut fbo as *mut _);
                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);

                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    framebuffer.tex_id(),
                    0,
                );

                gl.BlitFramebuffer(
                    dst.loc.x,
                    dst.loc.y,
                    dst.loc.x + dst.size.w,
                    dst.loc.y + dst.size.h,
                    0,
                    0,
                    size.w,
                    size.h,
                    ffi::COLOR_BUFFER_BIT,
                    ffi::LINEAR,
                );

                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, current_fbo as u32);
                gl.Enable(ffi::SCISSOR_TEST);

                gl.DeleteFramebuffers(1, &mut fbo as *mut _);

                if gl.GetError() != ffi::NO_ERROR {
                    Err(GlesError::BlitError)
                } else {
                    Ok(())
                }
            })??;

            // If blur is off, use the unblurred texture.
            if self.blur_options.is_none() {
                inner.intermediate = Some(framebuffer.clone());
                return Ok(());
            }

            if let Some((blur, options)) = blur {
                let mut guard = frame.renderer();
                let renderer = guard.as_mut();
                match blur.render(renderer, framebuffer, options) {
                    Ok(blurred) => inner.intermediate = Some(blurred),
                    Err(err) => {
                        tracing::warn!("error rendering blur: {err:?}");
                    }
                }
            }

            Ok(())
        })
    }

    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        tracing::trace!(?dst, ?self.geometry, "draw blur element");
        let Some(cache) = cache else {
            tracing::trace!("draw blur: no cache, skipping");
            return Ok(());
        };
        let Some(inner) = cache.get::<RefCell<Inner>>() else {
            return Ok(());
        };
        let mut inner = inner.borrow_mut();
        let inner = &mut *inner;

        let Some(texture) = &inner.intermediate else {
            return Ok(());
        };

        // Clamp the same way as in capture_framebuffer().
        let output_rect = Rectangle::from_size(frame.output_size());
        let clamped_dst = match dst.intersection(output_rect) {
            Some(clamped) => clamped,
            None => return Ok(()),
        };
        let clamp_offset = clamped_dst.loc - dst.loc;

        // Adjust damage for clamped dst.
        let filtered: Vec<Rectangle<i32, Physical>> = if clamped_dst != dst {
            let r = Rectangle::new(clamp_offset, clamped_dst.size);
            damage
                .iter()
                .filter_map(|d| {
                    if let Some(mut crop) = d.intersection(r) {
                        crop.loc -= clamp_offset;
                        Some(crop)
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            damage.to_vec()
        };

        if filtered.is_empty() {
            return Ok(());
        }

        frame.render_texture_from_to(
            texture,
            Rectangle::from_size(texture.size().to_f64()),
            clamped_dst,
            &filtered,
            &[],
            frame.transformation().invert(),
            1.,
            None,
            &[],
        )
    }
}
