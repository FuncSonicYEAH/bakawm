use glam::Mat3;
use smithay::backend::renderer::gles::{
    GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType,
    UniformValue,
};

use super::shader_element::ShaderProgram;

pub struct Shaders {
    pub border: Option<ShaderProgram>,
    pub shadow: Option<ShaderProgram>,
    pub clipped_surface: Option<GlesTexProgram>,
}

#[derive(Debug, Clone, Copy)]
pub enum ProgramType {
    Border,
    Shadow,
}

impl Shaders {
    fn compile(renderer: &mut GlesRenderer) -> Self {
        let border = ShaderProgram::compile(
            renderer,
            concat!(
                include_str!("shaders/border.frag"),
                include_str!("shaders/rounding_alpha.frag")
            ),
            &[
                UniformName::new("color_from", UniformType::_4f),
                UniformName::new("color_to", UniformType::_4f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("outer_radius", UniformType::_4f),
                UniformName::new("border_width", UniformType::_1f),
            ],
            &[],
        )
        .map_err(|err| {
            tracing::warn!("error compiling border shader: {err:?}");
        })
        .ok();

        let shadow = ShaderProgram::compile(
            renderer,
            concat!(
                include_str!("shaders/shadow.frag"),
                include_str!("shaders/rounding_alpha.frag")
            ),
            &[
                UniformName::new("shadow_color", UniformType::_4f),
                UniformName::new("sigma", UniformType::_1f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("corner_radius", UniformType::_4f),
                UniformName::new("window_input_to_geo", UniformType::Matrix3x3),
                UniformName::new("window_geo_size", UniformType::_2f),
                UniformName::new("window_corner_radius", UniformType::_4f),
            ],
            &[],
        )
        .map_err(|err| {
            tracing::warn!("error compiling shadow shader: {err:?}");
        })
        .ok();

        let clipped_surface = renderer
            .compile_custom_texture_shader(
                concat!(
                    include_str!("shaders/clipped_surface.frag"),
                    include_str!("shaders/rounding_alpha.frag"),
                    "\nvec4 postprocess(vec4 color) { return color; }",
                ),
                &[
                    UniformName::new("u_scale", UniformType::_1f),
                    UniformName::new("geo_size", UniformType::_2f),
                    UniformName::new("corner_radius", UniformType::_4f),
                    UniformName::new("input_to_geo", UniformType::Matrix3x3),
                ],
            )
            .map_err(|err| {
                tracing::warn!("error compiling clipped surface shader: {err:?}");
            })
            .ok();

        Self {
            border,
            shadow,
            clipped_surface,
        }
    }

    pub fn get_from_frame<'a>(frame: &'a mut GlesFrame<'_, '_>) -> &'a Self {
        let data = frame.egl_context().user_data();
        data.get()
            .expect("shaders::init() must be called when creating the renderer")
    }

    pub fn get(renderer: &mut GlesRenderer) -> &Self {
        let data = renderer.egl_context().user_data();
        data.get()
            .expect("shaders::init() must be called when creating the renderer")
    }

    pub fn program(&self, program: ProgramType) -> Option<ShaderProgram> {
        match program {
            ProgramType::Border => self.border.clone(),
            ProgramType::Shadow => self.shadow.clone(),
        }
    }
}

pub fn init(renderer: &mut GlesRenderer) {
    let shaders = Shaders::compile(renderer);
    let data = renderer.egl_context().user_data();
    if !data.insert_if_missing(|| shaders) {
        tracing::error!("shaders were already compiled");
    }
}

pub fn mat3_uniform(name: &'static str, mat: Mat3) -> Uniform<'static> {
    Uniform::new(
        name,
        UniformValue::Matrix3x3 {
            matrices: vec![mat.to_cols_array()],
            transpose: false,
        },
    )
}