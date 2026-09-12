use glam::{Mat3, Vec2};
use smithay::backend::renderer::buffer_y_inverted;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::config::CornerRadius;

use super::shaders::{Shaders, mat3_uniform};

#[derive(Debug)]
pub struct ClippedSurfaceRenderElement {
    inner: WaylandSurfaceRenderElement<GlesRenderer>,
    program: GlesTexProgram,
    corner_radius: CornerRadius,
    geometry: Rectangle<f64, Logical>,
    scale: f32,
}

impl ClippedSurfaceRenderElement {
    pub fn new(
        elem: WaylandSurfaceRenderElement<GlesRenderer>,
        scale: Scale<f64>,
        geometry: Rectangle<f64, Logical>,
        program: GlesTexProgram,
        corner_radius: CornerRadius,
    ) -> Self {
        return Self {
            inner: elem,
            program,
            corner_radius,
            geometry,
            scale: scale.x as f32,
        }
    }

    fn compute_uniforms(&self) -> Vec<Uniform<'static>> {
        let scale = Scale::from(f64::from(self.scale));
        let elem_geo = self.inner.geometry(scale);

        let elem_geo_loc = Vec2::new(elem_geo.loc.x as f32, elem_geo.loc.y as f32);
        let elem_geo_size = Vec2::new(elem_geo.size.w as f32, elem_geo.size.h as f32);

        let geo = self.geometry.to_physical_precise_round(scale);
        let geo_loc = Vec2::new(geo.loc.x, geo.loc.y);
        let geo_size = Vec2::new(geo.size.w, geo.size.h);

        let buf_size = self.inner.buffer_size();
        let buf_size = Vec2::new(buf_size.w as f32, buf_size.h as f32);

        let view = self.inner.view();
        let src_loc = Vec2::new(view.src.loc.x as f32, view.src.loc.y as f32);
        let src_size = Vec2::new(view.src.size.w as f32, view.src.size.h as f32);

        let transform = self.inner.transform();
        let transform = match transform {
            Transform::_90 => Transform::_270,
            Transform::_270 => Transform::_90,
            x => x,
        };
        let transform_matrix = Mat3::from_translation(Vec2::new(0.5, 0.5))
            * Mat3::from_cols_array(transform.matrix().as_ref())
            * Mat3::from_translation(-Vec2::new(0.5, 0.5));

        let y_invert = if buffer_y_inverted(self.inner.buffer()).unwrap_or(false) {
            Mat3::from_scale(Vec2::new(1., -1.))
        } else {
            Mat3::IDENTITY
        };

        let input_to_geo = transform_matrix
            * Mat3::from_scale(elem_geo_size / geo_size)
            * Mat3::from_translation((elem_geo_loc - geo_loc) / elem_geo_size)
            * Mat3::from_scale(buf_size / src_size)
            * Mat3::from_translation(-src_loc / buf_size)
            * y_invert;

        let geo_size = (self.geometry.size.w as f32, self.geometry.size.h as f32);

        return vec![
            Uniform::new("u_scale", self.scale),
            Uniform::new("geo_size", geo_size),
            Uniform::new("corner_radius", <[f32; 4]>::from(self.corner_radius)),
            mat3_uniform("input_to_geo", input_to_geo),
        ]
    }

    pub fn shader(renderer: &mut GlesRenderer) -> Option<&GlesTexProgram> {
        return Shaders::get(renderer).clipped_surface.as_ref()
    }

    pub fn will_clip(
        elem: &WaylandSurfaceRenderElement<GlesRenderer>,
        scale: Scale<f64>,
        geometry: Rectangle<f64, Logical>,
        corner_radius: CornerRadius,
    ) -> bool {
        let elem_geo = elem.geometry(scale);
        let geo = geometry.to_physical_precise_round(scale);

        if corner_radius == CornerRadius::default() {
            return !geo.contains_rect(elem_geo)
        } else {
            let corners = Self::rounded_corners(geometry, corner_radius);
            let corners = corners
                .into_iter()
                .map(|rect| return rect.to_physical_precise_up(scale));
            let geo = Rectangle::subtract_rects_many([geo], corners);
            return !Rectangle::subtract_rects_many([elem_geo], geo).is_empty()
        }
    }

    fn rounded_corners(
        geo: Rectangle<f64, Logical>,
        corner_radius: CornerRadius,
    ) -> [Rectangle<f64, Logical>; 4] {
        let top_left = corner_radius.top_left as f64;
        let top_right = corner_radius.top_right as f64;
        let bottom_right = corner_radius.bottom_right as f64;
        let bottom_left = corner_radius.bottom_left as f64;

        return [
            Rectangle::new(geo.loc, Size::from((top_left, top_left))),
            Rectangle::new(
                Point::from((geo.loc.x + geo.size.w - top_right, geo.loc.y)),
                Size::from((top_right, top_right)),
            ),
            Rectangle::new(
                Point::from((
                    geo.loc.x + geo.size.w - bottom_right,
                    geo.loc.y + geo.size.h - bottom_right,
                )),
                Size::from((bottom_right, bottom_right)),
            ),
            Rectangle::new(
                Point::from((geo.loc.x, geo.loc.y + geo.size.h - bottom_left)),
                Size::from((bottom_left, bottom_left)),
            ),
        ]
    }
}

impl Element for ClippedSurfaceRenderElement {
    fn id(&self) -> &Id {
        return self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        return self.inner.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        return self.inner.geometry(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        return self.inner.src()
    }

    fn transform(&self) -> Transform {
        return self.inner.transform()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        let damage = self.inner.damage_since(scale, commit);
        let mut geo = self.geometry.to_physical_precise_round(scale);
        geo.loc -= self.geometry(scale).loc;
        return damage
            .into_iter()
            .filter_map(|rect| return rect.intersection(geo))
            .collect()
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let regions = self.inner.opaque_regions(scale);
        let mut geo = self.geometry.to_physical_precise_round(scale);
        geo.loc -= self.geometry(scale).loc;
        let regions = regions
            .into_iter()
            .filter_map(|rect| return rect.intersection(geo));

        if self.corner_radius == CornerRadius::default() {
            return regions.collect()
        } else {
            let corners = Self::rounded_corners(self.geometry, self.corner_radius);
            let elem_loc = self.geometry(scale).loc;
            let corners = corners.into_iter().map(|rect| {
                let mut rect = rect.to_physical_precise_up(scale);
                rect.loc -= elem_loc;
                return rect
            });
            return OpaqueRegions::from_slice(&Rectangle::subtract_rects_many(regions, corners))
        }
    }

    fn alpha(&self) -> f32 {
        return self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        return self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for ClippedSurfaceRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.override_default_tex_program(self.program.clone(), self.compute_uniforms());
        RenderElement::<GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )?;
        frame.clear_tex_program_override();
        return Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        return None
    }
}
