use std::collections::HashMap;
use std::rc::Rc;

use glam::{Mat3, Vec2};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, Uniform};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::config::CornerRadius;

use super::shader_element::ShaderRenderElement;
use super::shaders::{ProgramType, Shaders, mat3_uniform};

#[derive(Debug, Clone)]
pub struct ShadowRenderElement {
    inner: ShaderRenderElement,
    params: Parameters,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Parameters {
    size: Size<f64, Logical>,
    geometry: Rectangle<f64, Logical>,
    color: [f32; 4],
    sigma: f32,
    corner_radius: CornerRadius,
    scale: f32,
    alpha: f32,
    window_geometry: Rectangle<f64, Logical>,
    window_corner_radius: CornerRadius,
}

impl ShadowRenderElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        size: Size<f64, Logical>,
        geometry: Rectangle<f64, Logical>,
        color: [f32; 4],
        sigma: f32,
        corner_radius: CornerRadius,
        scale: f32,
        window_geometry: Rectangle<f64, Logical>,
        window_corner_radius: CornerRadius,
        alpha: f32,
    ) -> Self {
        let inner = ShaderRenderElement::empty(ProgramType::Shadow, Kind::Unspecified);
        let mut rv = Self {
            inner,
            params: Parameters {
                size,
                geometry,
                color,
                sigma,
                corner_radius,
                scale,
                alpha,
                window_geometry,
                window_corner_radius,
            },
        };
        rv.update_inner();
        return rv
    }

    pub fn empty() -> Self {
        let inner = ShaderRenderElement::empty(ProgramType::Shadow, Kind::Unspecified);
        return Self {
            inner,
            params: Parameters {
                size: Default::default(),
                geometry: Default::default(),
                color: Default::default(),
                sigma: 0.,
                corner_radius: Default::default(),
                scale: 1.,
                alpha: 1.,
                window_geometry: Default::default(),
                window_corner_radius: Default::default(),
            },
        }
    }

    pub fn damage_all(&mut self) {
        self.inner.damage_all();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        size: Size<f64, Logical>,
        geometry: Rectangle<f64, Logical>,
        color: [f32; 4],
        sigma: f32,
        corner_radius: CornerRadius,
        scale: f32,
        window_geometry: Rectangle<f64, Logical>,
        window_corner_radius: CornerRadius,
        alpha: f32,
    ) {
        let params = Parameters {
            size,
            geometry,
            color,
            sigma,
            alpha,
            corner_radius,
            scale,
            window_geometry,
            window_corner_radius,
        };
        if self.params == params {
            return;
        }

        self.params = params;
        self.update_inner();
    }

    fn update_inner(&mut self) {
        let Parameters {
            size,
            geometry,
            color,
            sigma,
            alpha,
            corner_radius,
            scale,
            window_geometry,
            window_corner_radius,
        } = self.params;

        let area_size = Vec2::new(size.w as f32, size.h as f32);

        let geo_loc = Vec2::new(geometry.loc.x as f32, geometry.loc.y as f32);
        let geo_size = Vec2::new(geometry.size.w as f32, geometry.size.h as f32);

        let input_to_geo =
            Mat3::from_scale(area_size) * Mat3::from_translation(-geo_loc / area_size);

        let window_geo_loc = Vec2::new(window_geometry.loc.x as f32, window_geometry.loc.y as f32);
        let window_geo_size =
            Vec2::new(window_geometry.size.w as f32, window_geometry.size.h as f32);

        let window_input_to_geo =
            Mat3::from_scale(area_size) * Mat3::from_translation(-window_geo_loc / area_size);

        self.inner.update(
            size,
            None,
            scale,
            alpha,
            Rc::new([
                Uniform::new("shadow_color", color),
                Uniform::new("sigma", sigma),
                mat3_uniform("input_to_geo", input_to_geo),
                Uniform::new("geo_size", geo_size.to_array()),
                Uniform::new("corner_radius", <[f32; 4]>::from(corner_radius)),
                mat3_uniform("window_input_to_geo", window_input_to_geo),
                Uniform::new("window_geo_size", window_geo_size.to_array()),
                Uniform::new(
                    "window_corner_radius",
                    <[f32; 4]>::from(window_corner_radius),
                ),
            ]),
            HashMap::new(),
        );
    }

    pub fn with_location(mut self, location: Point<f64, Logical>) -> Self {
        self.inner = self.inner.with_location(location);
        return self
    }

    pub fn has_shader(renderer: &mut GlesRenderer) -> bool {
        return Shaders::get(renderer)
            .program(ProgramType::Shadow)
            .is_some()
    }
}

impl Default for ShadowRenderElement {
    fn default() -> Self {
        return Self::empty()
    }
}

impl Element for ShadowRenderElement {
    fn id(&self) -> &Id {
        return self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        return self.inner.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        return self.inner.geometry(scale)
    }

    fn transform(&self) -> Transform {
        return self.inner.transform()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        return self.inner.src()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        return self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        return self.inner.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        return self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        return self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for ShadowRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        return RenderElement::<GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        return self.inner.underlying_storage(renderer)
    }
}
