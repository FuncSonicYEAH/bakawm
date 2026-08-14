//! Runtime registry and render elements for user-defined custom shaders.
//!
//! Custom shaders are declared in the config via `bk.shader` and stored in
//! [`crate::config::CustomShaderConfig`]. They are compiled lazily on the
//! renderer and cached keyed by the config's `shader_gen` counter, so a config
//! reload recompiles them without needing access to the renderer at reload time.
//!
//! Two kinds are supported:
//! - `PostProcess`: the user supplies a `vec4 postprocess(vec4 color)` function
//!   that runs after the standard texture sampling (and `alpha`) pipeline.
//! - `Full`: the user supplies a complete fragment shader following the smithay
//!   texture-program contract (`//_DEFINES_`, sampler `tex`, `varying v_coords`,
//!   `uniform float alpha`).

use std::cell::RefCell;
use std::collections::HashMap;

use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Transform};

use crate::config::{Config, CustomShaderKind, ShaderUniform};

/// The standard texture pipeline used for `PostProcess` shaders.
const POSTPROCESS_HEADER: &str = concat!(
    "#version 100\n\n",
    "//_DEFINES_\n\n",
    "#if defined(EXTERNAL)\n",
    "#extension GL_OES_EGL_image_external : require\n",
    "#endif\n\n",
    "precision highp float;\n",
    "#if defined(EXTERNAL)\n",
    "uniform samplerExternalOES tex;\n",
    "#else\n",
    "uniform sampler2D tex;\n",
    "#endif\n\n",
    "uniform float alpha;\n",
    "varying vec2 v_coords;\n\n",
    "vec4 postprocess(vec4 color);\n",
);

const POSTPROCESS_FOOTER: &str = "\nvoid main() {\n\
    \x20   vec4 color = texture2D(tex, v_coords);\n\
    \x20   color = color * alpha;\n\
    \x20   color = postprocess(color);\n\
    \x20   gl_FragColor = color;\n\
}\n";

/// A compiled custom shader program with its uniform parameters.
#[derive(Debug, Clone)]
pub struct CustomShaderProgram {
    pub program: GlesTexProgram,
    pub uniforms: Vec<ShaderUniform>,
}

/// Registry of compiled user shaders, stored in the EGL context's user data.
#[derive(Debug, Default)]
pub struct CustomShaders {
    programs: HashMap<String, CustomShaderProgram>,
    generation: u64,
}

impl CustomShaders {
    pub fn get(&self, name: &str) -> Option<&CustomShaderProgram> {
        self.programs.get(name)
    }
}

/// Insert the (initially empty) registry into the EGL context's user data.
pub fn init(renderer: &mut GlesRenderer) {
    let data = renderer.egl_context().user_data();
    data.insert_if_missing(|| RefCell::new(CustomShaders::default()));
}

/// Recompile user shaders when the config generation changed.
///
/// Must be called with a live renderer (e.g. at the start of every frame), since
/// config reloads have no renderer access.
pub fn refresh_if_needed(renderer: &mut GlesRenderer, config: &Config) {
    let stale = renderer
        .egl_context()
        .user_data()
        .get::<RefCell<CustomShaders>>()
        .map(|cell| cell.borrow().generation != config.shader_gen)
        .unwrap_or(false);
    if !stale {
        return;
    }

    let mut new = CustomShaders {
        generation: config.shader_gen,
        programs: HashMap::with_capacity(config.custom_shaders.len()),
    };

    for shader in &config.custom_shaders {
        let source = match shader.kind {
            CustomShaderKind::Full => shader.fragment.clone(),
            CustomShaderKind::PostProcess => {
                let mut source = String::with_capacity(
                    POSTPROCESS_HEADER.len() + shader.fragment.len() + POSTPROCESS_FOOTER.len(),
                );
                source.push_str(POSTPROCESS_HEADER);
                source.push_str(&shader.fragment);
                source.push_str(POSTPROCESS_FOOTER);
                source
            }
        };

        let additional_uniforms: Vec<_> = shader.uniforms.iter().map(|u| u.name()).collect();
        match renderer.compile_custom_texture_shader(&source, &additional_uniforms) {
            Ok(program) => {
                new.programs.insert(
                    shader.name.clone(),
                    CustomShaderProgram {
                        program,
                        uniforms: shader.uniforms.clone(),
                    },
                );
            }
            Err(err) => {
                tracing::warn!(
                    shader = shader.name,
                    "error compiling custom shader \"{}\": {err:?}",
                    shader.name
                );
            }
        }
    }

    if let Some(cell) = renderer
        .egl_context()
        .user_data()
        .get::<RefCell<CustomShaders>>()
    {
        *cell.borrow_mut() = new;
    }
}

/// Look up a compiled shader by name (returns a clone so no borrow is held).
pub fn get_program(renderer: &mut GlesRenderer, name: &str) -> Option<CustomShaderProgram> {
    let cell = renderer
        .egl_context()
        .user_data()
        .get::<RefCell<CustomShaders>>()?;
    cell.borrow().get(name).cloned()
}

/// A render element that draws a window surface with a custom shader.
#[derive(Debug)]
pub struct CustomShaderRenderElement {
    inner: WaylandSurfaceRenderElement<GlesRenderer>,
    program: GlesTexProgram,
    uniforms: Vec<ShaderUniform>,
}

impl CustomShaderRenderElement {
    pub fn new(
        elem: WaylandSurfaceRenderElement<GlesRenderer>,
        program: &CustomShaderProgram,
    ) -> Self {
        Self {
            inner: elem,
            program: program.program.clone(),
            uniforms: program.uniforms.clone(),
        }
    }
}

impl Element for CustomShaderRenderElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.inner.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for CustomShaderRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let uniforms: Vec<Uniform<'static>> = self.uniforms.iter().map(|u| u.uniform()).collect();
        frame.override_default_tex_program(self.program.clone(), uniforms);
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
        Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
