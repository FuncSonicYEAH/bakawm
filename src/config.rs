use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::{Function, Lua, Result as LuaResult, Table, Value};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use smithay::backend::renderer::gles::{Uniform, UniformName, UniformValue};
use smithay::reexports::calloop::{
    channel,
    timer::{TimeoutAction, Timer},
};
use tracing::{info, warn};

/// Monotonic counter used to invalidate compiled custom shaders on config reload.
static SHADER_GEN: AtomicU64 = AtomicU64::new(0);

const CONFIG_DIR_NAME: &str = "bakawm";
const CONFIG_FILE_NAME: &str = "config.lua";

#[derive(Debug)]
pub struct Config {
    pub outputs: Vec<OutputConfig>,
    pub binds: Vec<BindConfig>,
    pub env: HashMap<String, String>,
    pub cursor: CursorConfig,
    pub window: WindowConfig,
    pub blur: BlurConfig,
    pub animations: AnimationsConfig,
    pub layout: LayoutConfig,
    pub custom_shaders: Vec<CustomShaderConfig>,
    pub window_rules: Vec<WindowRule>,
    pub layer_rules: Vec<LayerRule>,
    pub init_commands: Vec<String>,
    pub init_shell_commands: Vec<String>,
    /// Monotonic counter bumped on every config load/reload.
    /// Used to invalidate compiled custom shaders.
    pub shader_gen: u64,
    /// Lua runtime state with callback functions for `BindAction::Callback`.
    /// Not cloned on config reload — the new config brings its own `LuaConfig`.
    pub lua_config: Option<Box<LuaConfig>>,
}

impl Clone for Config {
    fn clone(&self) -> Self {
        return Config {
            outputs: self.outputs.clone(),
            binds: self.binds.clone(),
            env: self.env.clone(),
            cursor: self.cursor.clone(),
            window: self.window.clone(),
            blur: self.blur,
            animations: self.animations,
            layout: self.layout,
            custom_shaders: self.custom_shaders.clone(),
            window_rules: self.window_rules.clone(),
            layer_rules: self.layer_rules.clone(),
            init_commands: self.init_commands.clone(),
            init_shell_commands: self.init_shell_commands.clone(),
            shader_gen: self.shader_gen,
            // LuaConfig is not cloned — callbacks are not needed across clones
            lua_config: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub name: String,
    pub mode: Option<ModeConfig>,
    pub position: Option<(i32, i32)>,
    pub scale: Option<f64>,
    pub transform: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModeConfig {
    pub width: i32,
    pub height: i32,
    pub refresh: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct BindConfig {
    pub modifiers: Vec<String>,
    pub key: String,
    pub action: BindAction,
}

#[derive(Debug, Clone)]
pub enum BindAction {
    Quit,
    CloseWindow,
    Run(String),
    Screenshot,
    ToggleDecorations,
    TogglePreview,
    ScaleUp,
    ScaleDown,
    RotateOutput,
    ToggleTint,
    ToggleFloating,
    VtSwitch(i32),
    Screen(usize),
    /// Focus the next window in the layout (cycling).
    FocusNext,
    /// Focus the previous window in the layout (cycling).
    FocusPrev,
    /// Switch to the next workspace (creating it if needed).
    WorkspaceNext,
    /// Switch to the previous workspace.
    WorkspacePrev,
    /// Switch to a specific workspace index.
    Workspace(usize),
    /// Grow the focused window's column width.
    ResizeWidthUp,
    /// Shrink the focused window's column width.
    ResizeWidthDown,
    /// Toggle true fullscreen (XDG/X11 fullscreen state).
    ToggleFullscreen,
    /// Toggle windowed fullscreen: the focused window fills the work area.
    ToggleMaximize,
    /// Index into `LuaConfig::callbacks` for a custom Lua function.
    Callback(usize),
}

/// Holds the Lua state and callback functions for the configuration.
/// Stored in `AnvilState` so that `BindAction::Callback` can invoke Lua functions at runtime.
pub struct LuaConfig {
    /// The Lua VM — kept alive so that `Function` handles in `callbacks` remain valid.
    #[allow(dead_code)]
    lua: Lua,
    callbacks: Vec<Function>,
}

impl std::fmt::Debug for LuaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        return f.debug_struct("LuaConfig")
            .field("callbacks_count", &self.callbacks.len())
            .finish()
    }
}

impl LuaConfig {
    /// Invoke a callback by its index. Returns an error if the index is out of bounds
    /// or the Lua call fails.
    pub fn invoke_callback(&self, idx: usize) -> LuaResult<()> {
        match self.callbacks.get(idx) {
            Some(func) => return func.call::<()>(()),
            None => return Err(mlua::Error::external(format!(
                "Callback index {} out of bounds (max {})",
                idx,
                self.callbacks.len()
            ))),
        }
    }

    /// Invoke a custom layout function.
    ///
    /// `windows` is `(layout_id, width, height, width_override)` for each window
    /// (width_override = 0 means unset), `area` is `(x, y, w, h)` of the work
    /// area. The function returns the target geometry for each window (same
    /// order as `windows`), or `None` for windows that should not be moved by
    /// the layout.
    pub fn invoke_layout(
        &self,
        idx: usize,
        windows: &[(u64, f64, f64, f64)],
        area: (f64, f64, f64, f64),
        gap: f64,
    ) -> LuaResult<Vec<Option<(f64, f64, f64, f64)>>> {
        let func = self.callbacks.get(idx).ok_or_else(|| {
            return mlua::Error::external(format!(
                "Layout function index {} out of bounds (max {})",
                idx,
                self.callbacks.len()
            ))
        })?;

        let lua = &self.lua;
        let windows_table = lua.create_table()?;
        for (i, (id, w, h, width_override)) in windows.iter().enumerate() {
            let entry = lua.create_table()?;
            entry.set("id", *id)?;
            entry.set("w", *w)?;
            entry.set("h", *h)?;
            entry.set("width", *width_override)?;
            windows_table.set(i + 1, entry)?;
        }

        let area_table = lua.create_table()?;
        area_table.set("x", area.0)?;
        area_table.set("y", area.1)?;
        area_table.set("w", area.2)?;
        area_table.set("h", area.3)?;
        area_table.set("gap", gap)?;

        let result: Table = func.call::<Table>((windows_table, area_table))?;

        let mut out = Vec::with_capacity(windows.len());
        for i in 0..windows.len() {
            match result.get::<Option<Table>>(i + 1)? {
                Some(t) => {
                    let x = t.get::<f64>("x")?;
                    let y = t.get::<f64>("y")?;
                    let w = t.get::<f64>("w")?;
                    let h = t.get::<f64>("h")?;
                    out.push(Some((x, y, w, h)));
                }
                None => out.push(None),
            }
        }
        return Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct CursorConfig {
    pub theme: Option<String>,
    pub size: Option<u32>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct CornerRadius {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

impl From<CornerRadius> for [f32; 4] {
    fn from(value: CornerRadius) -> Self {
        return [
            value.top_left,
            value.top_right,
            value.bottom_right,
            value.bottom_left,
        ]
    }
}

impl From<f32> for CornerRadius {
    fn from(value: f32) -> Self {
        return Self {
            top_left: value,
            top_right: value,
            bottom_right: value,
            bottom_left: value,
        }
    }
}

impl CornerRadius {
    pub fn fit_to(self, width: f32, height: f32) -> Self {
        let reduction = f32::min(
            f32::min(
                width / (self.top_left + self.top_right),
                width / (self.bottom_left + self.bottom_right),
            ),
            f32::min(
                height / (self.top_left + self.bottom_left),
                height / (self.top_right + self.bottom_right),
            ),
        );
        let reduction = f32::min(1., reduction);

        return Self {
            top_left: self.top_left * reduction,
            top_right: self.top_right * reduction,
            bottom_right: self.bottom_right * reduction,
            bottom_left: self.bottom_left * reduction,
        }
    }

    pub fn expanded_by(mut self, width: f32) -> Self {
        if self.top_left > 0. {
            self.top_left += width;
        }
        if self.top_right > 0. {
            self.top_right += width;
        }
        if self.bottom_right > 0. {
            self.bottom_right += width;
        }
        if self.bottom_left > 0. {
            self.bottom_left += width;
        }

        if width < 0. {
            self.top_left = self.top_left.max(0.);
            self.top_right = self.top_right.max(0.);
            self.bottom_left = self.bottom_left.max(0.);
            self.bottom_right = self.bottom_right.max(0.);
        }

        return self
    }

    pub fn scaled_by(self, scale: f32) -> Self {
        return Self {
            top_left: self.top_left * scale,
            top_right: self.top_right * scale,
            bottom_right: self.bottom_right * scale,
            bottom_left: self.bottom_left * scale,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WindowConfig {
    pub prefer_no_csd: bool,
    pub border: BorderConfig,
    pub shadow: ShadowConfig,
    pub corner_radius: CornerRadius,
    pub resize_modifier: String,
    /// Whether the window floats freely (not managed by the layout engine).
    pub floating: bool,
    /// Name of a custom shader (from `bk.shader`) applied to this window.
    pub shader: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BorderConfig {
    pub width: f64,
    pub color: [f32; 4],
    pub inactive_color: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowConfig {
    pub enable: bool,
    pub offset_x: f64,
    pub offset_y: f64,
    pub softness: f64,
    pub spread: f64,
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlurConfig {
    pub enable: bool,
    pub passes: u8,
    pub offset: f64,
    pub xray: bool,
}

impl Default for BorderConfig {
    fn default() -> Self {
        return BorderConfig {
            width: 0.0,
            color: [0.0, 0.0, 0.0, 1.0],
            inactive_color: [0.3, 0.3, 0.3, 1.0],
        }
    }
}

impl Default for ShadowConfig {
    fn default() -> Self {
        return ShadowConfig {
            enable: false,
            offset_x: 0.0,
            offset_y: 5.0,
            softness: 30.0,
            spread: 5.0,
            color: [0.0, 0.0, 0.0, 0.47],
        }
    }
}

impl Default for BlurConfig {
    fn default() -> Self {
        return BlurConfig {
            enable: true,
            passes: 2,
            offset: 1.0,
            xray: false,
        }
    }
}

/// Rule that matches windows by app_id and/or title and applies overrides.
#[derive(Debug, Clone)]
pub struct WindowRule {
    /// Match by app-id (substring match). None means match any.
    pub app_id: Option<String>,
    /// Match by title (substring match). None means match any.
    pub title: Option<String>,
    /// Override window settings. Only non-None fields override the global config.
    pub window: Option<PartialWindowConfig>,
    /// Override blur settings for matching windows.
    pub blur: Option<BlurOverride>,
}

/// Partial window config used in window rules.
/// Only non-None fields override the corresponding global config values.
#[derive(Debug, Clone, Default)]
pub struct PartialWindowConfig {
    pub prefer_no_csd: Option<bool>,
    pub border: Option<BorderConfig>,
    pub shadow: Option<ShadowConfig>,
    pub corner_radius: Option<CornerRadius>,
    pub resize_modifier: Option<String>,
    pub floating: Option<bool>,
    pub shader: Option<String>,
}

impl PartialWindowConfig {
    /// Apply this partial config on top of a base WindowConfig, returning the merged result.
    pub fn merge_over(&self, base: &WindowConfig) -> WindowConfig {
        return WindowConfig {
            prefer_no_csd: self.prefer_no_csd.unwrap_or(base.prefer_no_csd),
            border: self.border.as_ref().unwrap_or(&base.border).clone(),
            shadow: self.shadow.unwrap_or(base.shadow),
            corner_radius: self.corner_radius.unwrap_or(base.corner_radius),
            resize_modifier: self
                .resize_modifier
                .as_ref()
                .unwrap_or(&base.resize_modifier)
                .clone(),
            floating: self.floating.unwrap_or(base.floating),
            shader: self.shader.clone().or_else(|| return base.shader.clone()),
        }
    }
}

/// Blur override within a rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlurOverride {
    pub enable: bool,
    pub passes: Option<u8>,
    pub offset: Option<f64>,
    pub xray: Option<bool>,
}

/// Rule that matches layer-shell surfaces by namespace and applies overrides.
#[derive(Debug, Clone)]
pub struct LayerRule {
    /// Match by namespace (substring match). Empty means match any.
    pub namespace: Option<String>,
    /// Override blur settings for matching layer surfaces.
    pub blur: Option<BlurOverride>,
}

/// Animation configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationsConfig {
    /// Globally enable all animations.
    pub enable: bool,
    /// Window open animation config.
    pub window_open: WindowAnimConfig,
    /// Window close animation config.
    pub window_close: WindowAnimConfig,
    /// Workspace-switch fade animation config.
    pub workspace_switch: WindowAnimConfig,
}

/// Per-animation config for window open/close.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowAnimConfig {
    /// Enable this specific animation.
    pub enable: bool,
    /// Duration in milliseconds.
    pub duration_ms: u32,
    /// Easing curve.
    pub curve: AnimCurve,
    /// Scale factor at the start of the animation.
    /// For open: the window starts at this scale and animates to 1.0 (e.g. 0.8 = start at 80%).
    /// For close: the window starts at 1.0 and animates to this scale (e.g. 0.0 = shrink to nothing).
    pub scale: f64,
}

/// Easing curve type for animations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnimCurve {
    Linear,
    EaseOutQuad,
    EaseOutCubic,
    EaseOutExpo,
    CubicBezier(f64, f64, f64, f64),
}

impl Default for AnimationsConfig {
    fn default() -> Self {
        return AnimationsConfig {
            enable: true,
            window_open: WindowAnimConfig {
                enable: true,
                duration_ms: 250,
                curve: AnimCurve::EaseOutCubic,
                scale: 0.8,
            },
            window_close: WindowAnimConfig {
                enable: true,
                duration_ms: 250,
                curve: AnimCurve::EaseOutCubic,
                scale: 0.0,
            },
            workspace_switch: WindowAnimConfig {
                enable: true,
                duration_ms: 200,
                curve: AnimCurve::EaseOutCubic,
                scale: 1.0,
            },
        }
    }
}

impl AnimCurve {
    /// Convert to the animation module's Curve type.
    pub fn to_curve(self) -> crate::animation::Curve {
        match self {
            AnimCurve::Linear => return crate::animation::Curve::Linear,
            AnimCurve::EaseOutQuad => return crate::animation::Curve::EaseOutQuad,
            AnimCurve::EaseOutCubic => return crate::animation::Curve::EaseOutCubic,
            AnimCurve::EaseOutExpo => return crate::animation::Curve::EaseOutExpo,
            AnimCurve::CubicBezier(x1, y1, x2, y2) => {
                return crate::animation::Curve::CubicBezier { x1, y1, x2, y2 }
            }
        }
    }
}

/// A generic animation specifier: off, an easing curve, or a spring.
///
/// Used for layout adjustment animations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnimConfig {
    /// No animation — values snap instantly.
    Off,
    /// An easing curve with a duration.
    Curve { duration_ms: u32, curve: AnimCurve },
    /// A spring with the given parameters.
    Spring {
        damping_ratio: f64,
        stiffness: f64,
        epsilon: f64,
    },
}

impl AnimConfig {
    /// Convert to a runtime [`crate::animation::Animation`] between `from` and `to`.
    pub fn to_animation(&self, from: f64, to: f64) -> crate::animation::Animation {
        match self {
            AnimConfig::Off => return crate::animation::Animation::new_off(),
            AnimConfig::Curve { duration_ms, curve } => {
                return crate::animation::Animation::ease(from, to, *duration_ms, curve.to_curve())
            }
            AnimConfig::Spring {
                damping_ratio,
                stiffness,
                epsilon,
            } => {
                let params =
                    crate::animation::SpringParams::new(*damping_ratio, *stiffness, *epsilon);
                let spring = crate::animation::Spring {
                    from,
                    to,
                    initial_velocity: 0.,
                    params,
                };
                return crate::animation::Animation::spring(from, to, spring)
            }
        }
    }
}

/// Built-in or user-defined layout engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LayoutType {
    /// No layout — windows float freely (default, preserves classic behavior).
    Floating,
    /// Vertical columns, one window per column (niri-style).
    Columns,
    /// Uniform grid, filling rows left-to-right.
    Grid,
    /// One master window on the left plus a stack of the rest on the right.
    MasterStack,
    /// All windows maximized to the work area.
    Maximize,
    /// A user-defined Lua function (see `LayoutConfig::custom_fn`).
    Custom,
}

/// Output work-area margins.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Margins {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

impl Default for Margins {
    fn default() -> Self {
        return Self {
            top: 0.,
            bottom: 0.,
            left: 0.,
            right: 0.,
        }
    }
}

/// Animation configuration for layout adjustments.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutAnimConfig {
    /// Animation used when windows move to new positions.
    pub move_anim: AnimConfig,
    /// Animation used when windows change size.
    pub resize_anim: AnimConfig,
}

impl Default for LayoutAnimConfig {
    fn default() -> Self {
        return Self {
            move_anim: AnimConfig::Curve {
                duration_ms: 250,
                curve: AnimCurve::EaseOutCubic,
            },
            resize_anim: AnimConfig::Curve {
                duration_ms: 200,
                curve: AnimCurve::EaseOutCubic,
            },
        }
    }
}

/// The tiling layout configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutConfig {
    pub layout: LayoutType,
    /// Gap between windows, in logical pixels.
    pub gap: f64,
    /// Margins around the work area.
    pub margins: Margins,
    /// Fraction of the work area taken by the master in `MasterStack`.
    pub master_ratio: f64,
    /// Animation used for layout adjustments.
    pub animation: LayoutAnimConfig,
    /// Index into `LuaConfig::callbacks` for a custom layout function
    /// (used when `layout == LayoutType::Custom`).
    pub custom_fn: Option<usize>,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        return Self {
            layout: LayoutType::Floating,
            gap: 8.,
            margins: Margins::default(),
            master_ratio: 0.5,
            animation: LayoutAnimConfig::default(),
            custom_fn: None,
        }
    }
}

/// How a custom shader fragment is applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CustomShaderKind {
    /// The user writes a `vec4 postprocess(vec4 color)` function. The compositor
    /// wraps it in the standard texture pipeline (sampling, corner-radius clip, alpha).
    PostProcess,
    /// The user supplies the complete fragment shader following the smithay
    /// texture-program contract (`//_DEFINES`, `tex`, `v_coords`, `alpha`).
    Full,
}

/// A single shader uniform parameter.
#[derive(Debug, Clone)]
pub struct ShaderUniform {
    pub name: String,
    pub value: UniformValue,
}

impl ShaderUniform {
    /// Build a `UniformName` for this parameter.
    pub fn name(&self) -> UniformName<'static> {
        return UniformName::new(self.name.clone(), self.value.type_())
    }

    /// Build a `Uniform` for this parameter.
    pub fn uniform(&self) -> Uniform<'static> {
        return Uniform::new(self.name.clone(), self.value.clone())
    }
}

/// A user-defined shader (defined via `bk.shader`).
#[derive(Debug, Clone)]
pub struct CustomShaderConfig {
    pub name: String,
    pub kind: CustomShaderKind,
    /// GLSL fragment source (already resolved from inline text or a file path).
    pub fragment: String,
    /// Static uniform parameters to upload with the shader.
    pub uniforms: Vec<ShaderUniform>,
}

impl Default for WindowConfig {
    fn default() -> Self {
        return WindowConfig {
            prefer_no_csd: true,
            border: BorderConfig {
                width: 0.0,
                color: [0.0, 0.0, 0.0, 1.0],
                inactive_color: [0.3, 0.3, 0.3, 1.0],
            },
            shadow: ShadowConfig::default(),
            corner_radius: CornerRadius::default(),
            resize_modifier: "Ctrl".into(),
            floating: false,
            shader: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        return Config {
            outputs: Vec::new(),
            binds: vec![
                BindConfig {
                    modifiers: vec!["Ctrl".into(), "Alt".into()],
                    key: "BackSpace".into(),
                    action: BindAction::Quit,
                },
                BindConfig {
                    modifiers: vec!["Super".into()],
                    key: "q".into(),
                    action: BindAction::Quit,
                },
                BindConfig {
                    modifiers: vec!["Super".into()],
                    key: "c".into(),
                    action: BindAction::CloseWindow,
                },
                BindConfig {
                    modifiers: vec!["Super".into()],
                    key: "Return".into(),
                    action: BindAction::Run("weston-terminal".into()),
                },
                BindConfig {
                    modifiers: vec!["Super".into(), "Shift".into()],
                    key: "s".into(),
                    action: BindAction::Screenshot,
                },
                BindConfig {
                    modifiers: vec!["Super".into(), "Shift".into()],
                    key: "d".into(),
                    action: BindAction::ToggleDecorations,
                },
                BindConfig {
                    modifiers: vec!["Super".into(), "Shift".into()],
                    key: "w".into(),
                    action: BindAction::TogglePreview,
                },
                BindConfig {
                    modifiers: vec!["Super".into(), "Shift".into()],
                    key: "p".into(),
                    action: BindAction::ScaleUp,
                },
                BindConfig {
                    modifiers: vec!["Super".into(), "Shift".into()],
                    key: "m".into(),
                    action: BindAction::ScaleDown,
                },
                BindConfig {
                    modifiers: vec!["Super".into(), "Shift".into()],
                    key: "r".into(),
                    action: BindAction::RotateOutput,
                },
                BindConfig {
                    modifiers: vec!["Super".into(), "Shift".into()],
                    key: "t".into(),
                    action: BindAction::ToggleTint,
                },
                BindConfig {
                    modifiers: vec!["Super".into()],
                    key: "1".into(),
                    action: BindAction::Screen(0),
                },
                BindConfig {
                    modifiers: vec!["Super".into()],
                    key: "2".into(),
                    action: BindAction::Screen(1),
                },
                BindConfig {
                    modifiers: vec!["Super".into()],
                    key: "3".into(),
                    action: BindAction::Screen(2),
                },
            ],
            env: HashMap::new(),
            cursor: CursorConfig {
                theme: None,
                size: None,
            },
            window: WindowConfig::default(),
            blur: BlurConfig::default(),
            animations: AnimationsConfig::default(),
            layout: LayoutConfig::default(),
            custom_shaders: Vec::new(),
            window_rules: Vec::new(),
            layer_rules: Vec::new(),
            init_commands: Vec::new(),
            init_shell_commands: Vec::new(),
            shader_gen: 0,
            lua_config: None,
        }
    }
}

impl Config {
    /// Find the first window rule matching the given app_id and title.
    pub fn find_window_rule(
        &self,
        app_id: Option<&str>,
        title: Option<&str>,
    ) -> Option<&WindowRule> {
        return self.window_rules.iter().find(|rule| {
            let matches = match (&rule.app_id, &rule.title) {
                (Some(rule_id), Some(rule_title)) => {
                    app_id.is_some_and(|id| return id.contains(rule_id))
                        && title.is_some_and(|t| return t.contains(rule_title))
                }
                (Some(rule_id), None) => app_id.is_some_and(|id| return id.contains(rule_id)),
                (None, Some(rule_title)) => title.is_some_and(|t| return t.contains(rule_title)),
                (None, None) => true,
            };
            return matches
        })
    }
}

fn config_dir() -> PathBuf {
    return dirs::config_dir()
        .unwrap_or_else(|| return PathBuf::from("."))
        .join(CONFIG_DIR_NAME)
}

pub fn config_path() -> PathBuf {
    return config_dir().join(CONFIG_FILE_NAME)
}

pub fn load_config() -> Config {
    let path = config_path();

    if !path.exists() {
        info!(
            "No config file found at {:?}, creating default config",
            path
        );
        if let Err(e) = create_default_config() {
            warn!("Failed to create default config: {}", e);
            return Config::default();
        }
        // Now parse the newly created config file instead of using hardcoded defaults
        return match parse_lua_config(&path) {
            Ok(config) => {
                info!("Loaded config from {:?}", path);
                finalize_config(config)
            }
            Err(e) => {
                warn!(
                    "Failed to parse newly created config: {}, using defaults",
                    e
                );
                finalize_config(Config::default())
            }
        };
    }

    match parse_lua_config(&path) {
        Ok(config) => {
            info!(
                "Loaded config from {:?} (animations: enable={}, open={}ms/{:?}, close={}ms/{:?})",
                path,
                config.animations.enable,
                config.animations.window_open.duration_ms,
                config.animations.window_open.curve,
                config.animations.window_close.duration_ms,
                config.animations.window_close.curve,
            );
            return finalize_config(config)
        }
        Err(e) => {
            warn!(
                "Failed to parse config file {:?}: {}, using defaults",
                path, e
            );
            return finalize_config(Config::default())
        }
    }
}

pub fn reload_config(current: &Config) -> Config {
    let path = config_path();

    if !path.exists() {
        info!("Config file removed at {:?}, keeping current config", path);
        return current.clone();
    }

    match parse_lua_config(&path) {
        Ok(new_config) => {
            info!(
                "Reloaded config from {:?} (animations: enable={}, open={}ms/{:?}, close={}ms/{:?})",
                path,
                new_config.animations.enable,
                new_config.animations.window_open.duration_ms,
                new_config.animations.window_open.curve,
                new_config.animations.window_close.duration_ms,
                new_config.animations.window_close.curve,
            );
            return finalize_config(new_config)
        }
        Err(e) => {
            warn!(
                "Failed to reload config file {:?}: {}, keeping current config",
                path, e
            );
            return current.clone()
        }
    }
}

/// Assign a fresh `shader_gen` to a freshly parsed config so that compiled
/// custom shaders get invalidated on reload.
fn finalize_config(mut config: Config) -> Config {
    config.shader_gen = SHADER_GEN.fetch_add(1, Ordering::Relaxed) + 1;
    return config
}

pub fn spawn_config_watcher<B: crate::state::Backend + 'static>(
    handle: &smithay::reexports::calloop::LoopHandle<'static, crate::state::AnvilState<B>>,
) -> Option<notify::RecommendedWatcher> {
    let (sender, source) = channel::channel::<()>();

    let watcher_result = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                        let _ = sender.send(());
                    }
                    _ => {}
                }
            }
        },
        notify::Config::default().with_poll_interval(Duration::from_secs(3)),
    );

    let mut watcher = match watcher_result {
        Ok(w) => w,
        Err(e) => {
            warn!("Failed to create config file watcher: {}", e);
            return None;
        }
    };

    let config_dir = config_dir();
    if config_dir.exists()
        && let Err(e) = watcher.watch(&config_dir, RecursiveMode::NonRecursive) {
            warn!("Failed to watch config directory {:?}: {}", config_dir, e);
            return None;
        }

    let config_file = config_path();
    if config_file.exists()
        && let Err(e) = watcher.watch(&config_file, RecursiveMode::NonRecursive) {
            warn!("Failed to watch config file {:?}: {}", config_file, e);
            return None;
        }

    let handle_clone = handle.clone();
    if let Err(e) = handle.insert_source(source, move |_event, _, data| {
        if data.config_reload_timer.is_none() {
            data.config_reload_timer = Some(Instant::now());
            let _ = handle_clone.insert_source(
                Timer::from_duration(Duration::from_millis(500)),
                move |_, _, data| {
                    if data.config_reload_timer.take().is_some() {
                        info!("Config file changed, reloading...");
                        data.reload_config();
                    }
                    return TimeoutAction::Drop
                },
            );
        }
    }) {
        warn!("Failed to insert config watcher into event loop: {}", e);
        return None;
    }

    info!("Watching config file for changes at {:?}", config_file);
    return Some(watcher)
}

fn create_default_config() -> Result<(), Box<dyn std::error::Error>> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;

    let default_lua = include_str!("../resources/default-config.lua");
    fs::write(config_path(), default_lua)?;
    return Ok(())
}

fn parse_lua_config(path: &PathBuf) -> LuaResult<Config> {
    let lua = Lua::new();

    // ── Collected state ──────────────────────────────────────────
    let collected_outputs: Arc<Mutex<Vec<OutputConfig>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_binds: Arc<Mutex<Vec<BindConfig>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_env: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let collected_cursor: Arc<Mutex<Option<CursorConfig>>> = Arc::new(Mutex::new(None));
    let collected_window: Arc<Mutex<Option<WindowConfig>>> = Arc::new(Mutex::new(None));
    let collected_blur: Arc<Mutex<Option<BlurConfig>>> = Arc::new(Mutex::new(None));
    let collected_animations: Arc<Mutex<Option<AnimationsConfig>>> = Arc::new(Mutex::new(None));
    let collected_layout: Arc<Mutex<Option<LayoutConfig>>> = Arc::new(Mutex::new(None));
    let collected_custom_shaders: Arc<Mutex<Vec<CustomShaderConfig>>> =
        Arc::new(Mutex::new(Vec::new()));
    let collected_window_rules: Arc<Mutex<Vec<WindowRule>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_layer_rules: Arc<Mutex<Vec<LayerRule>>> = Arc::new(Mutex::new(Vec::new()));
    let spawn_commands: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let shell_commands: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let callbacks: Arc<Mutex<Vec<Function>>> = Arc::new(Mutex::new(Vec::new()));

    // ── bk table: imperative API ─────────────────────────────────

    // bk.bind(modifiers, key, action)
    let bind_fn = {
        let collected_binds = collected_binds.clone();
        let callbacks = callbacks.clone();
        lua.create_function(
            move |lua, (modifiers, key, action): (Value, String, Value)| {
                let mods = parse_modifiers_value(lua, modifiers)?;
                let bind_action = parse_action_value(lua, &callbacks, action)?;
                collected_binds.lock().unwrap().push(BindConfig {
                    modifiers: mods,
                    key,
                    action: bind_action,
                });
                return Ok(())
            },
        )?
    };

    // bk.output(config_table)
    let output_fn = {
        let collected_outputs = collected_outputs.clone();
        lua.create_function(move |_, t: Table| {
            collected_outputs
                .lock()
                .unwrap()
                .push(parse_single_output(&t)?);
            return Ok(())
        })?
    };

    // bk.env(key, value)
    let env_fn = {
        let collected_env = collected_env.clone();
        lua.create_function(move |_, (k, v): (String, String)| {
            collected_env.lock().unwrap().insert(k, v);
            return Ok(())
        })?
    };

    // bk.window(config_table)
    let window_fn = {
        let collected_window = collected_window.clone();
        lua.create_function(move |_, t: Table| {
            let w = parse_window(&t)?;
            *collected_window.lock().unwrap() = Some(w);
            return Ok(())
        })?
    };

    // bk.animations(config_table)
    let animations_fn = {
        let collected_animations = collected_animations.clone();
        lua.create_function(move |_, t: Table| {
            let a = parse_animations(&t)?;
            *collected_animations.lock().unwrap() = Some(a);
            return Ok(())
        })?
    };

    // bk.blur(config_table)
    let blur_fn = {
        let collected_blur = collected_blur.clone();
        lua.create_function(move |_, t: Table| {
            let b = parse_blur(&t)?;
            *collected_blur.lock().unwrap() = Some(b);
            return Ok(())
        })?
    };

    // bk.layout(config_table)
    let layout_fn = {
        let collected_layout = collected_layout.clone();
        let callbacks = callbacks.clone();
        lua.create_function(move |_, t: Table| {
            let l = parse_layout(&t, &callbacks)?;
            *collected_layout.lock().unwrap() = Some(l);
            return Ok(())
        })?
    };

    // bk.shader(config_table)
    let shader_fn = {
        let collected_custom_shaders = collected_custom_shaders.clone();
        lua.create_function(move |_, t: Table| {
            let s = parse_custom_shader(&t)?;
            collected_custom_shaders.lock().unwrap().push(s);
            return Ok(())
        })?
    };

    // bk.cursor(config_table)
    let cursor_fn = {
        let collected_cursor = collected_cursor.clone();
        lua.create_function(move |_, t: Table| {
            let c = parse_cursor(&t)?;
            *collected_cursor.lock().unwrap() = Some(c);
            return Ok(())
        })?
    };

    // bk.window_rule(rule_table)
    let window_rule_fn = {
        let collected_window_rules = collected_window_rules.clone();
        lua.create_function(move |_, t: Table| {
            collected_window_rules
                .lock()
                .unwrap()
                .push(parse_single_window_rule(&t)?);
            return Ok(())
        })?
    };

    // bk.layer_rule(rule_table)
    let layer_rule_fn = {
        let collected_layer_rules = collected_layer_rules.clone();
        lua.create_function(move |_, t: Table| {
            collected_layer_rules
                .lock()
                .unwrap()
                .push(parse_single_layer_rule(&t)?);
            return Ok(())
        })?
    };

    // bk.on_start(function)
    let on_start_fn = lua.create_function(move |_, func: Function| {
        // Call the init function immediately; it should use bk.spawn/bk.run_sh
        // which already push to the shared spawn_commands/shell_commands vectors
        return func.call::<()>(())
    })?;

    // bk.spawn(cmd)
    let spawn_fn = {
        let spawn_commands = spawn_commands.clone();
        lua.create_function(move |_, cmd: String| {
            spawn_commands.lock().unwrap().push(cmd);
            return Ok(())
        })?
    };

    // bk.run_sh(code)
    let run_sh_fn = {
        let shell_commands = shell_commands.clone();
        lua.create_function(move |_, code: String| {
            shell_commands.lock().unwrap().push(code);
            return Ok(())
        })?
    };

    // ── Action descriptor functions (return tables for bk.bind) ──

    let make_action = |lua: &Lua, t: &str| -> LuaResult<Table> {
        let table = lua.create_table()?;
        table.set("type", t)?;
        return Ok(table)
    };

    let quit_fn = lua.create_function(move |lua, ()| return make_action(lua, "Quit"))?;
    let close_window_fn = lua.create_function(move |lua, ()| return make_action(lua, "CloseWindow"))?;
    let screenshot_fn = lua.create_function(move |lua, ()| return make_action(lua, "Screenshot"))?;
    let toggle_decorations_fn =
        lua.create_function(move |lua, ()| return make_action(lua, "ToggleDecorations"))?;
    let toggle_preview_fn =
        lua.create_function(move |lua, ()| return make_action(lua, "TogglePreview"))?;
    let scale_up_fn = lua.create_function(move |lua, ()| return make_action(lua, "ScaleUp"))?;
    let scale_down_fn = lua.create_function(move |lua, ()| return make_action(lua, "ScaleDown"))?;
    let rotate_output_fn = lua.create_function(move |lua, ()| return make_action(lua, "RotateOutput"))?;
    let toggle_tint_fn = lua.create_function(move |lua, ()| return make_action(lua, "ToggleTint"))?;
    let toggle_floating_fn =
        lua.create_function(move |lua, ()| return make_action(lua, "ToggleFloating"))?;

    let exec_fn = lua.create_function(move |lua, args: mlua::Variadic<String>| {
        let table = lua.create_table()?;
        table.set("type", "Run")?;
        table.set("command", args.join(" "))?;
        return Ok(table)
    })?;

    let vt_switch_fn = lua.create_function(move |lua, n: i32| {
        let table = lua.create_table()?;
        table.set("type", "VtSwitch")?;
        table.set("n", n)?;
        return Ok(table)
    })?;

    let screen_fn = lua.create_function(move |lua, n: usize| {
        let table = lua.create_table()?;
        table.set("type", "Screen")?;
        table.set("n", n)?;
        return Ok(table)
    })?;

    let focus_next_fn = lua.create_function(move |lua, ()| return make_action(lua, "FocusNext"))?;
    let focus_prev_fn = lua.create_function(move |lua, ()| return make_action(lua, "FocusPrev"))?;
    let workspace_next_fn =
        lua.create_function(move |lua, ()| return make_action(lua, "WorkspaceNext"))?;
    let workspace_prev_fn =
        lua.create_function(move |lua, ()| return make_action(lua, "WorkspacePrev"))?;
    let workspace_fn = lua.create_function(move |lua, n: usize| {
        let table = lua.create_table()?;
        table.set("type", "Workspace")?;
        table.set("n", n)?;
        return Ok(table)
    })?;
    let resize_width_up_fn = lua.create_function(move |lua, ()| return make_action(lua, "ResizeWidthUp"))?;
    let resize_width_down_fn =
        lua.create_function(move |lua, ()| return make_action(lua, "ResizeWidthDown"))?;
    let toggle_fullscreen_fn =
        lua.create_function(move |lua, ()| return make_action(lua, "ToggleFullscreen"))?;
    let toggle_maximize_fn =
        lua.create_function(move |lua, ()| return make_action(lua, "ToggleMaximize"))?;

    // ── Assemble bk global table ─────────────────────────────────
    let bk = lua.create_table()?;
    bk.set("bind", bind_fn)?;
    bk.set("output", output_fn)?;
    bk.set("env", env_fn)?;
    bk.set("window", window_fn)?;
    bk.set("animations", animations_fn)?;
    bk.set("blur", blur_fn)?;
    bk.set("layout", layout_fn)?;
    bk.set("shader", shader_fn)?;
    bk.set("cursor", cursor_fn)?;
    bk.set("window_rule", window_rule_fn)?;
    bk.set("layer_rule", layer_rule_fn)?;
    bk.set("on_start", on_start_fn)?;
    bk.set("spawn", spawn_fn)?;
    bk.set("run_sh", run_sh_fn)?;
    // Action functions
    bk.set("quit", quit_fn)?;
    bk.set("close_window", close_window_fn)?;
    bk.set("exec", exec_fn)?;
    bk.set("screenshot", screenshot_fn)?;
    bk.set("toggle_decorations", toggle_decorations_fn)?;
    bk.set("toggle_preview", toggle_preview_fn)?;
    bk.set("scale_up", scale_up_fn)?;
    bk.set("scale_down", scale_down_fn)?;
    bk.set("rotate_output", rotate_output_fn)?;
    bk.set("toggle_tint", toggle_tint_fn)?;
    bk.set("toggle_floating", toggle_floating_fn)?;
    bk.set("vt_switch", vt_switch_fn)?;
    bk.set("screen", screen_fn)?;
    bk.set("focus_next", focus_next_fn)?;
    bk.set("focus_prev", focus_prev_fn)?;
    bk.set("workspace_next", workspace_next_fn)?;
    bk.set("workspace_prev", workspace_prev_fn)?;
    bk.set("workspace", workspace_fn)?;
    bk.set("resize_width_up", resize_width_up_fn)?;
    bk.set("resize_width_down", resize_width_down_fn)?;
    bk.set("toggle_fullscreen", toggle_fullscreen_fn)?;
    bk.set("toggle_maximize", toggle_maximize_fn)?;

    // Keep `bakawm` as an alias for backward compatibility (spawn/run_sh)
    let bakawm = lua.create_table()?;
    bakawm.set("spawn", bk.get::<Function>("spawn")?)?;
    bakawm.set("run_sh", bk.get::<Function>("run_sh")?)?;

    lua.globals().set("bk", bk)?;
    lua.globals().set("bakawm", bakawm)?;

    // ── Execute the script ───────────────────────────────────────
    let code = fs::read_to_string(path)?;
    let exec_result = lua.load(&code).eval::<Value>();

    // Build config from collected state
    let mut config = Config::default();
    config.binds.clear();

    // Check if the script returned a table (old-style config)
    match exec_result {
        Ok(Value::Table(result)) => {
            // Old-style return { ... } config — parse from the returned table
            if let Value::Table(outputs) = result.get::<Value>("outputs")? {
                config.outputs = parse_outputs(&outputs)?;
            }
            if let Value::Table(binds) = result.get::<Value>("binds")? {
                config.binds = parse_binds(&binds)?;
            }
            if let Value::Table(env) = result.get::<Value>("env")? {
                config.env = parse_env(&env)?;
            }
            if let Value::Table(cursor) = result.get::<Value>("cursor")? {
                config.cursor = parse_cursor(&cursor)?;
            }
            if let Value::Table(window) = result.get::<Value>("window")? {
                config.window = parse_window(&window)?;
            }
            if let Value::Table(blur) = result.get::<Value>("blur")? {
                config.blur = parse_blur(&blur)?;
            }
            if let Value::Table(animations) = result.get::<Value>("animations")? {
                config.animations = parse_animations(&animations)?;
            }
            if let Value::Table(window_rules) = result.get::<Value>("window_rules")? {
                config.window_rules = parse_window_rules(&window_rules)?;
            }
            if let Value::Table(layer_rules) = result.get::<Value>("layer_rules")? {
                config.layer_rules = parse_layer_rules(&layer_rules)?;
            }
            if let Value::Function(init_fn) = result.get::<Value>("init")? {
                init_fn.call::<()>(())?;
            }
            // spawn/run_sh already collected via the registered functions
        }
        Ok(_) | Err(_) => {
            // New-style imperative config (no return table) or error
            // Use the collected state from bk.* calls
        }
    }

    // Merge collected imperative state (always applies, even for old-style that also uses bk.*)
    {
        let collected = collected_outputs.lock().unwrap();
        if !collected.is_empty() {
            config.outputs = collected.clone();
        }
    }
    {
        let collected = collected_binds.lock().unwrap();
        if !collected.is_empty() {
            config.binds = collected.clone();
        }
    }
    {
        let collected = collected_env.lock().unwrap();
        if !collected.is_empty() {
            config.env = collected.clone();
        }
    }
    {
        let collected = collected_cursor.lock().unwrap();
        if let Some(c) = collected.as_ref() {
            config.cursor = c.clone();
        }
    }
    {
        let collected = collected_window.lock().unwrap();
        if let Some(w) = collected.as_ref() {
            config.window = w.clone();
        }
    }
    {
        let collected = collected_blur.lock().unwrap();
        if let Some(b) = collected.as_ref() {
            config.blur = *b;
        }
    }
    {
        let collected = collected_animations.lock().unwrap();
        if let Some(a) = collected.as_ref() {
            config.animations = *a;
        }
    }
    {
        let collected = collected_layout.lock().unwrap();
        if let Some(l) = collected.as_ref() {
            config.layout = *l;
        }
    }
    {
        let collected = collected_custom_shaders.lock().unwrap();
        if !collected.is_empty() {
            config.custom_shaders = collected.clone();
        }
    }
    {
        let collected = collected_window_rules.lock().unwrap();
        if !collected.is_empty() {
            config.window_rules = collected.clone();
        }
    }
    {
        let collected = collected_layer_rules.lock().unwrap();
        if !collected.is_empty() {
            config.layer_rules = collected.clone();
        }
    }

    config.init_commands = spawn_commands.lock().unwrap().clone();
    config.init_shell_commands = shell_commands.lock().unwrap().clone();

    // Build the LuaConfig with callbacks
    let lua_callbacks = callbacks.lock().unwrap().clone();
    config.lua_config = Some(Box::new(LuaConfig {
        lua,
        callbacks: lua_callbacks,
    }));

    return Ok(config)
}

/// Parse the `modifiers` parameter of `bk.bind()`.
/// Accepts either a table of strings or a single string.
fn parse_modifiers_value(_lua: &Lua, value: Value) -> LuaResult<Vec<String>> {
    match value {
        Value::Table(t) => {
            let mut mods = Vec::new();
            for m in t.sequence_values::<String>() {
                mods.push(m?);
            }
            return Ok(mods)
        }
        Value::String(s) => return Ok(vec![s.to_str()?.to_owned()]),
        _ => return Ok(Vec::new()),
    }
}

/// Parse the `action` parameter of `bk.bind()`.
/// Accepts either an action descriptor table (from bk.quit(), etc.) or a Lua function.
fn parse_action_value(
    _lua: &Lua,
    callbacks: &Arc<Mutex<Vec<Function>>>,
    value: Value,
) -> LuaResult<BindAction> {
    match value {
        Value::Table(t) => {
            // Action descriptor table from bk.quit(), bk.exec(cmd), etc.
            let kind: String = t.get("type")?;
            match kind.as_str() {
                "Quit" => return Ok(BindAction::Quit),
                "CloseWindow" => return Ok(BindAction::CloseWindow),
                "Run" => {
                    let command: String = t.get("command")?;
                    return Ok(BindAction::Run(command))
                }
                "Screenshot" => return Ok(BindAction::Screenshot),
                "ToggleDecorations" => return Ok(BindAction::ToggleDecorations),
                "TogglePreview" => return Ok(BindAction::TogglePreview),
                "ScaleUp" => return Ok(BindAction::ScaleUp),
                "ScaleDown" => return Ok(BindAction::ScaleDown),
                "RotateOutput" => return Ok(BindAction::RotateOutput),
                "ToggleTint" => return Ok(BindAction::ToggleTint),
                "ToggleFloating" => return Ok(BindAction::ToggleFloating),
                "VtSwitch" => {
                    let n: i32 = t.get("n")?;
                    return Ok(BindAction::VtSwitch(n))
                }
                "Screen" => {
                    let n: usize = t.get("n")?;
                    return Ok(BindAction::Screen(n))
                }
                "FocusNext" => return Ok(BindAction::FocusNext),
                "FocusPrev" => return Ok(BindAction::FocusPrev),
                "WorkspaceNext" => return Ok(BindAction::WorkspaceNext),
                "WorkspacePrev" => return Ok(BindAction::WorkspacePrev),
                "Workspace" => {
                    let n: usize = t.get("n")?;
                    return Ok(BindAction::Workspace(n))
                }
                "ResizeWidthUp" => return Ok(BindAction::ResizeWidthUp),
                "ResizeWidthDown" => return Ok(BindAction::ResizeWidthDown),
                "ToggleFullscreen" => return Ok(BindAction::ToggleFullscreen),
                "ToggleMaximize" => return Ok(BindAction::ToggleMaximize),
                other => return Err(mlua::Error::external(format!(
                    "Unknown bind action type: {}",
                    other
                ))),
            }
        }
        Value::Function(func) => {
            let idx = callbacks.lock().unwrap().len();
            callbacks.lock().unwrap().push(func);
            return Ok(BindAction::Callback(idx))
        }
        other => return Err(mlua::Error::external(format!(
            "bind action must be a table or function, got {:?}",
            other
        ))),
    }
}

/// Parse a single output config from a Lua table.
fn parse_single_output(t: &Table) -> LuaResult<OutputConfig> {
    let name: String = t.get("name")?;

    let mode = if let Value::Table(mode_table) = t.get::<Value>("mode")? {
        Some(ModeConfig {
            width: mode_table.get("width")?,
            height: mode_table.get("height")?,
            refresh: mode_table.get("refresh").ok(),
        })
    } else {
        None
    };

    let position = if let Value::Table(pos_table) = t.get::<Value>("position")? {
        Some((pos_table.get("x")?, pos_table.get("y")?))
    } else {
        None
    };

    let scale: Option<f64> = t.get("scale").ok();
    let transform: Option<String> = t.get("transform").ok();

    return Ok(OutputConfig {
        name,
        mode,
        position,
        scale,
        transform,
    })
}

/// Parse a single window rule from a Lua table.
fn parse_single_window_rule(t: &Table) -> LuaResult<WindowRule> {
    let app_id: Option<String> = t.get("app_id").ok();
    let title: Option<String> = t.get("title").ok();

    let window = if let Value::Table(window_table) = t.get::<Value>("window")? {
        Some(parse_partial_window(&window_table)?)
    } else {
        None
    };

    let blur = if let Value::Table(blur_table) = t.get::<Value>("blur")? {
        Some(parse_blur_override(&blur_table)?)
    } else {
        None
    };

    return Ok(WindowRule {
        app_id,
        title,
        window,
        blur,
    })
}

/// Parse a single layer rule from a Lua table.
fn parse_single_layer_rule(t: &Table) -> LuaResult<LayerRule> {
    let namespace: Option<String> = t.get("namespace").ok();

    let blur = if let Value::Table(blur_table) = t.get::<Value>("blur")? {
        Some(parse_blur_override(&blur_table)?)
    } else {
        None
    };

    return Ok(LayerRule { namespace, blur })
}

fn parse_outputs(table: &Table) -> LuaResult<Vec<OutputConfig>> {
    let mut outputs = Vec::new();
    for pair in table.sequence_values::<Table>() {
        let t = pair?;
        let name: String = t.get("name")?;

        let mode = if let Value::Table(mode_table) = t.get::<Value>("mode")? {
            Some(ModeConfig {
                width: mode_table.get("width")?,
                height: mode_table.get("height")?,
                refresh: mode_table.get("refresh").ok(),
            })
        } else {
            None
        };

        let position = if let Value::Table(pos_table) = t.get::<Value>("position")? {
            Some((pos_table.get("x")?, pos_table.get("y")?))
        } else {
            None
        };

        let scale: Option<f64> = t.get("scale").ok();
        let transform: Option<String> = t.get("transform").ok();

        outputs.push(OutputConfig {
            name,
            mode,
            position,
            scale,
            transform,
        });
    }
    return Ok(outputs)
}

fn parse_binds(table: &Table) -> LuaResult<Vec<BindConfig>> {
    let mut binds = Vec::new();
    for pair in table.sequence_values::<Table>() {
        let t = pair?;

        let modifiers_value: Value = t.get("modifiers")?;
        let modifiers = match modifiers_value {
            Value::Table(mod_table) => {
                let mut mods = Vec::new();
                for m in mod_table.sequence_values::<String>() {
                    mods.push(m?);
                }
                mods
            }
            Value::String(s) => vec![s.to_str()?.to_owned()],
            _ => Vec::new(),
        };

        let key: String = t.get("key")?;
        let action = parse_bind_action(&t.get::<Table>("action")?)?;

        binds.push(BindConfig {
            modifiers,
            key,
            action,
        });
    }
    return Ok(binds)
}

fn parse_bind_action(table: &Table) -> LuaResult<BindAction> {
    let kind: String = table.get("kind")?;
    match kind.as_str() {
        "Quit" => return Ok(BindAction::Quit),
        "CloseWindow" => return Ok(BindAction::CloseWindow),
        "Run" => {
            let command: String = table.get("command")?;
            return Ok(BindAction::Run(command))
        }
        "Screenshot" => return Ok(BindAction::Screenshot),
        "ToggleDecorations" => return Ok(BindAction::ToggleDecorations),
        "TogglePreview" => return Ok(BindAction::TogglePreview),
        "ScaleUp" => return Ok(BindAction::ScaleUp),
        "ScaleDown" => return Ok(BindAction::ScaleDown),
        "RotateOutput" => return Ok(BindAction::RotateOutput),
        "ToggleTint" => return Ok(BindAction::ToggleTint),
        "VtSwitch" => {
            let n: i32 = table.get("n")?;
            return Ok(BindAction::VtSwitch(n))
        }
        "Screen" => {
            let n: usize = table.get("n")?;
            return Ok(BindAction::Screen(n))
        }
        other => return Err(mlua::Error::external(format!(
            "Unknown bind action kind: {}",
            other
        ))),
    }
}

fn parse_env(table: &Table) -> LuaResult<HashMap<String, String>> {
    let mut env = HashMap::new();
    for pair in table.pairs::<String, String>() {
        let (k, v) = pair?;
        env.insert(k, v);
    }
    return Ok(env)
}

fn parse_cursor(table: &Table) -> LuaResult<CursorConfig> {
    let theme: Option<String> = table.get("theme").ok();
    let size: Option<u32> = table.get("size").ok();
    return Ok(CursorConfig { theme, size })
}

fn parse_window(table: &Table) -> LuaResult<WindowConfig> {
    let prefer_no_csd: Option<bool> = table.get("prefer_no_csd").ok();
    let resize_modifier: Option<String> = table.get("resize_modifier").ok();
    let floating: Option<bool> = table.get("floating").ok();
    let shader: Option<String> = table.get("shader").ok();

    let corner_radius = if let Value::Table(cr_table) = table.get::<Value>("corner_radius")? {
        let top_left: Option<f32> = cr_table.get("top_left").ok();
        let top_right: Option<f32> = cr_table.get("top_right").ok();
        let bottom_right: Option<f32> = cr_table.get("bottom_right").ok();
        let bottom_left: Option<f32> = cr_table.get("bottom_left").ok();
        Some(CornerRadius {
            top_left: top_left.unwrap_or(0.0),
            top_right: top_right.unwrap_or(0.0),
            bottom_right: bottom_right.unwrap_or(0.0),
            bottom_left: bottom_left.unwrap_or(0.0),
        })
    } else {
        let uniform: Option<f32> = table.get("corner_radius").ok();
        uniform.map(CornerRadius::from)
    };

    let border = if let Value::Table(border_table) = table.get::<Value>("border")? {
        parse_border_config(&border_table)?
    } else {
        BorderConfig::default()
    };

    let shadow = if let Value::Table(shadow_table) = table.get::<Value>("shadow")? {
        parse_shadow_config(&shadow_table)?
    } else {
        ShadowConfig::default()
    };

    let mut window = WindowConfig::default();
    if let Some(prefer_no_csd) = prefer_no_csd {
        window.prefer_no_csd = prefer_no_csd;
    }
    window.border = border;
    window.shadow = shadow;
    if let Some(corner_radius) = corner_radius {
        window.corner_radius = corner_radius;
    }
    if let Some(resize_modifier) = resize_modifier {
        window.resize_modifier = resize_modifier;
    }
    if let Some(floating) = floating {
        window.floating = floating;
    }
    if let Some(shader) = shader {
        window.shader = Some(shader);
    }

    return Ok(window)
}

fn parse_blur(table: &Table) -> LuaResult<BlurConfig> {
    let mut blur = BlurConfig::default();
    if let Ok(enable) = table.get::<bool>("enable") {
        blur.enable = enable;
    }
    if let Ok(passes) = table.get::<u8>("passes") {
        blur.passes = passes;
    }
    if let Ok(offset) = table.get::<f64>("offset") {
        blur.offset = offset;
    }
    if let Ok(xray) = table.get::<bool>("xray") {
        blur.xray = xray;
    }
    return Ok(blur)
}

fn parse_blur_override(table: &Table) -> LuaResult<BlurOverride> {
    let enable: bool = table.get("enable")?;
    let passes = table.get::<u8>("passes").ok();
    let offset = table.get::<f64>("offset").ok();
    let xray = table.get::<bool>("xray").ok();
    return Ok(BlurOverride {
        enable,
        passes,
        offset,
        xray,
    })
}

fn parse_animations(table: &Table) -> LuaResult<AnimationsConfig> {
    let mut animations = AnimationsConfig::default();

    if let Ok(enable) = table.get::<bool>("enable") {
        animations.enable = enable;
    }

    if let Value::Table(open_table) = table.get::<Value>("window_open")? {
        animations.window_open = parse_window_anim(&open_table)?;
    }

    if let Value::Table(close_table) = table.get::<Value>("window_close")? {
        animations.window_close = parse_window_anim(&close_table)?;
    }

    if let Value::Table(ws_table) = table.get::<Value>("workspace_switch")? {
        animations.workspace_switch = parse_window_anim(&ws_table)?;
    }

    return Ok(animations)
}

fn parse_window_anim(table: &Table) -> LuaResult<WindowAnimConfig> {
    let mut anim = WindowAnimConfig {
        enable: true,
        duration_ms: 150,
        curve: AnimCurve::EaseOutCubic,
        scale: 0.8,
    };

    if let Ok(enable) = table.get::<bool>("enable") {
        anim.enable = enable;
    }
    if let Ok(duration_ms) = table.get::<u32>("duration_ms") {
        anim.duration_ms = duration_ms;
    }
    if let Ok(curve) = parse_anim_curve(table) {
        anim.curve = curve;
    }
    if let Ok(scale) = table.get::<f64>("scale") {
        anim.scale = scale.clamp(0.0, 1.0);
    }

    return Ok(anim)
}

fn parse_anim_curve(table: &Table) -> LuaResult<AnimCurve> {
    let curve_value: Value = table.get("curve")?;

    match curve_value {
        Value::String(s) => {
            let curve_str = s.to_str()?.to_string();
            match curve_str.as_str() {
                "linear" => return Ok(AnimCurve::Linear),
                "ease-out-quad" => return Ok(AnimCurve::EaseOutQuad),
                "ease-out-cubic" => return Ok(AnimCurve::EaseOutCubic),
                "ease-out-expo" => return Ok(AnimCurve::EaseOutExpo),
                other => return Err(mlua::Error::external(format!(
                    "Unknown animation curve: {}",
                    other
                ))),
            }
        }
        Value::Table(t) => {
            let kind: String = t.get(1)?;
            match kind.as_str() {
                "cubic-bezier" => {
                    let x1: f64 = t.get(2)?;
                    let y1: f64 = t.get(3)?;
                    let x2: f64 = t.get(4)?;
                    let y2: f64 = t.get(5)?;
                    return Ok(AnimCurve::CubicBezier(x1, y1, x2, y2))
                }
                other => return Err(mlua::Error::external(format!(
                    "Unknown parametric curve: {}",
                    other
                ))),
            }
        }
        _ => return Err(mlua::Error::external("curve must be a string or table")),
    }
}

/// Parse a generic animation specifier: `{ curve = ..., duration_ms = ... }`,
/// `{ spring = { damping_ratio = ..., stiffness = ..., epsilon = ... } }`, or
/// `false` / `"off"` to disable.
fn parse_anim_config(table: &Table) -> LuaResult<AnimConfig> {
    // NOTE: use an explicit turbofish here. `let off: Option<Value> =
    // table.get("off").ok();` silently makes `get` infer `V = Value` (the
    // `.ok()` sits between the call and the type annotation), so a *missing*
    // key returns `Some(Value::Nil)` and every animation table would be parsed
    // as `Off`.
    let off = table.get::<Option<Value>>("off")?;
    if off.is_some() {
        return Ok(AnimConfig::Off);
    }
    let enable = table.get::<Option<bool>>("enable")?;
    if enable == Some(false) {
        return Ok(AnimConfig::Off);
    }

    if let Value::Table(spring_table) = table.get::<Value>("spring")? {
        let damping_ratio: f64 = spring_table.get("damping_ratio").unwrap_or(0.8);
        let stiffness: f64 = spring_table.get("stiffness").unwrap_or(300.0);
        let epsilon: f64 = spring_table.get("epsilon").unwrap_or(0.01);
        return Ok(AnimConfig::Spring {
            damping_ratio,
            stiffness,
            epsilon,
        });
    }

    let duration_ms: u32 = table.get("duration_ms").unwrap_or(250);
    let curve: AnimCurve = match parse_anim_curve(table) {
        Ok(c) => c,
        Err(_) => AnimCurve::EaseOutCubic,
    };
    return Ok(AnimConfig::Curve { duration_ms, curve })
}

/// Parse a layout animation config: `{ move = {...}, resize = {...} }`.
fn parse_layout_anim(table: &Table) -> LuaResult<LayoutAnimConfig> {
    let mut anim = LayoutAnimConfig::default();

    if let Value::Table(move_table) = table.get::<Value>("move")? {
        anim.move_anim = parse_anim_config(&move_table)?;
    }
    if let Value::Table(resize_table) = table.get::<Value>("resize")? {
        anim.resize_anim = parse_anim_config(&resize_table)?;
    }

    return Ok(anim)
}

fn parse_layout_type(t: &Table) -> LuaResult<LayoutType> {
    let layout: String = t.get("type").unwrap_or_else(|_| return "floating".to_string());
    match layout.as_str() {
        "floating" => return Ok(LayoutType::Floating),
        "columns" => return Ok(LayoutType::Columns),
        "grid" => return Ok(LayoutType::Grid),
        "master-stack" => return Ok(LayoutType::MasterStack),
        "maximize" => return Ok(LayoutType::Maximize),
        "custom" => return Ok(LayoutType::Custom),
        other => return Err(mlua::Error::external(format!(
            "Unknown layout type: {}",
            other
        ))),
    }
}

fn parse_margins(t: &Table) -> LuaResult<Margins> {
    let top: f64 = t.get("top").unwrap_or(0.);
    let bottom: f64 = t.get("bottom").unwrap_or(0.);
    let left: f64 = t.get("left").unwrap_or(0.);
    let right: f64 = t.get("right").unwrap_or(0.);
    return Ok(Margins {
        top,
        bottom,
        left,
        right,
    })
}

/// Parse a `bk.layout` config table.
///
/// ```lua
/// bk.layout({ type = "columns", gap = 8, margins = { top = 0, bottom = 0, left = 0, right = 0 },
///             master_ratio = 0.5,
///             animation = { move = { duration_ms = 250, curve = "ease-out-cubic" },
///                           resize = { spring = { damping_ratio = 0.8, stiffness = 300 } } },
///             fn = function(windows, area) ... end })
/// ```
fn parse_layout(t: &Table, callbacks: &Arc<Mutex<Vec<Function>>>) -> LuaResult<LayoutConfig> {
    let mut layout = LayoutConfig::default();

    layout.layout = parse_layout_type(t)?;

    if let Ok(gap) = t.get::<f64>("gap") {
        layout.gap = gap.max(0.);
    }
    if let Value::Table(margins_table) = t.get::<Value>("margins")? {
        layout.margins = parse_margins(&margins_table)?;
    }
    if let Ok(master_ratio) = t.get::<f64>("master_ratio") {
        layout.master_ratio = master_ratio.clamp(0.1, 0.9);
    }
    if let Value::Table(anim_table) = t.get::<Value>("animation")? {
        layout.animation = parse_layout_anim(&anim_table)?;
    }

    if let Value::Function(func) = t.get::<Value>("fn")? {
        if layout.layout == LayoutType::Custom {
            let idx = callbacks.lock().unwrap().len();
            callbacks.lock().unwrap().push(func);
            layout.custom_fn = Some(idx);
        } else {
            return Err(mlua::Error::external(
                "layout `fn` is only allowed with type = \"custom\"",
            ));
        }
    }

    return Ok(layout)
}

/// Parse a single shader uniform value. Numbers become floats, integers become
/// ints, booleans become ints (0/1), arrays become vec2/vec3/vec4.
fn parse_shader_uniform_value(name: String, value: Value) -> LuaResult<ShaderUniform> {
    let uniform = match value {
        Value::Number(n) => ShaderUniform {
            name,
            value: UniformValue::_1f(n as f32),
        },
        Value::Integer(i) => ShaderUniform {
            name,
            value: UniformValue::_1i(i as i32),
        },
        Value::Boolean(b) => ShaderUniform {
            name,
            value: UniformValue::_1i(b as i32),
        },
        Value::Table(t) => {
            let mut vals = Vec::new();
            for v in t.sequence_values::<Value>() {
                let v = v?;
                match v {
                    Value::Number(n) => vals.push(n as f32),
                    _ => {
                        return Err(mlua::Error::external(
                            "shader uniform arrays must contain numbers",
                        ));
                    }
                }
            }
            let value = match vals.as_slice() {
                [a, b] => UniformValue::_2f(*a, *b),
                [a, b, c] => UniformValue::_3f(*a, *b, *c),
                [a, b, c, d] => UniformValue::_4f(*a, *b, *c, *d),
                _ => {
                    return Err(mlua::Error::external(
                        "shader uniform arrays must have 2, 3 or 4 elements",
                    ));
                }
            };
            ShaderUniform { name, value }
        }
        _ => {
            return Err(mlua::Error::external(format!(
                "unsupported shader uniform value for `{}`",
                name
            )));
        }
    };
    return Ok(uniform)
}

/// Parse a `bk.shader` config table.
///
/// ```lua
/// bk.shader({ name = "sepia",
///             kind = "postprocess",           -- or "full"
///             fragment = "sepia.frag",        -- path relative to the config dir
///             uniforms = { intensity = 0.6 } })
/// ```
fn parse_custom_shader(t: &Table) -> LuaResult<CustomShaderConfig> {
    let name: String = t.get("name")?;
    let kind_str: String = t.get("kind").unwrap_or_else(|_| return "postprocess".to_string());
    let kind = match kind_str.as_str() {
        "postprocess" => CustomShaderKind::PostProcess,
        "full" => CustomShaderKind::Full,
        other => {
            return Err(mlua::Error::external(format!(
                "Unknown shader kind: {} (expected \"postprocess\" or \"full\")",
                other
            )));
        }
    };

    let fragment: String = t.get("fragment")?;
    // If the fragment is not inline GLSL (doesn't look like a shader), treat it as
    // a file path relative to the config directory.
    let fragment = resolve_fragment_source(&fragment)?;

    let mut uniforms = Vec::new();
    if let Value::Table(uniforms_table) = t.get::<Value>("uniforms")? {
        for pair in uniforms_table.pairs::<String, Value>() {
            let (name, value) = pair?;
            uniforms.push(parse_shader_uniform_value(name, value)?);
        }
    }

    return Ok(CustomShaderConfig {
        name,
        kind,
        fragment,
        uniforms,
    })
}

/// Resolve a fragment shader source. If the string looks like a shader body
/// (contains GLSL keywords) it is used as-is; otherwise it is treated as a file
/// path relative to the config directory.
fn resolve_fragment_source(fragment: &str) -> LuaResult<String> {
    let trimmed = fragment.trim();
    let looks_like_glsl = trimmed.contains("void main")
        || trimmed.contains("postprocess")
        || trimmed.contains("precision ")
        || trimmed.starts_with("#version")
        || trimmed.starts_with("#if")
        || trimmed.starts_with("uniform ");

    if looks_like_glsl {
        return Ok(fragment.to_string());
    }

    let path = config_dir().join(trimmed);
    return fs::read_to_string(&path).map_err(|err| {
        return mlua::Error::external(format!("Failed to read shader file {:?}: {err}", path))
    })
}

fn parse_window_rules(table: &Table) -> LuaResult<Vec<WindowRule>> {
    let mut rules = Vec::new();
    for pair in table.sequence_values::<Table>() {
        let t = pair?;

        let app_id: Option<String> = t.get("app_id").ok();
        let title: Option<String> = t.get("title").ok();

        let window = if let Value::Table(window_table) = t.get::<Value>("window")? {
            Some(parse_partial_window(&window_table)?)
        } else {
            None
        };

        let blur = if let Value::Table(blur_table) = t.get::<Value>("blur")? {
            Some(parse_blur_override(&blur_table)?)
        } else {
            None
        };

        rules.push(WindowRule {
            app_id,
            title,
            window,
            blur,
        });
    }
    return Ok(rules)
}

/// Parse a partial window config where all fields are optional.
/// Only fields explicitly set in the Lua table will be Some.
fn parse_partial_window(table: &Table) -> LuaResult<PartialWindowConfig> {
    let prefer_no_csd: Option<bool> = table.get("prefer_no_csd").ok();
    let resize_modifier: Option<String> = table.get("resize_modifier").ok();
    let floating: Option<bool> = table.get("floating").ok();
    let shader: Option<String> = table.get("shader").ok();

    let corner_radius = if let Value::Table(cr_table) = table.get::<Value>("corner_radius")? {
        let top_left: Option<f32> = cr_table.get("top_left").ok();
        let top_right: Option<f32> = cr_table.get("top_right").ok();
        let bottom_right: Option<f32> = cr_table.get("bottom_right").ok();
        let bottom_left: Option<f32> = cr_table.get("bottom_left").ok();
        Some(CornerRadius {
            top_left: top_left.unwrap_or(0.0),
            top_right: top_right.unwrap_or(0.0),
            bottom_right: bottom_right.unwrap_or(0.0),
            bottom_left: bottom_left.unwrap_or(0.0),
        })
    } else {
        table
            .get::<f32>("corner_radius")
            .ok()
            .map(CornerRadius::from)
    };

    let border = if let Value::Table(border_table) = table.get::<Value>("border")? {
        Some(parse_border_config(&border_table)?)
    } else {
        None
    };

    let shadow = if let Value::Table(shadow_table) = table.get::<Value>("shadow")? {
        Some(parse_shadow_config(&shadow_table)?)
    } else {
        None
    };

    return Ok(PartialWindowConfig {
        prefer_no_csd,
        border,
        shadow,
        corner_radius,
        resize_modifier,
        floating,
        shader,
    })
}

fn parse_layer_rules(table: &Table) -> LuaResult<Vec<LayerRule>> {
    let mut rules = Vec::new();
    for pair in table.sequence_values::<Table>() {
        let t = pair?;

        let namespace: Option<String> = t.get("namespace").ok();

        let blur = if let Value::Table(blur_table) = t.get::<Value>("blur")? {
            Some(parse_blur_override(&blur_table)?)
        } else {
            None
        };

        rules.push(LayerRule { namespace, blur });
    }
    return Ok(rules)
}

/// Helper: parse a border config table (reused by window rules).
fn parse_border_config(table: &Table) -> LuaResult<BorderConfig> {
    let width: Option<f64> = table.get("width").ok();
    let color = if let Value::Table(color_table) = table.get::<Value>("color")? {
        let r: f32 = color_table.get("r")?;
        let g: f32 = color_table.get("g")?;
        let b: f32 = color_table.get("b")?;
        let a: Option<f32> = color_table.get("a").ok();
        [r, g, b, a.unwrap_or(1.0)]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    let inactive_color = if let Value::Table(color_table) = table.get::<Value>("inactive_color")? {
        let r: f32 = color_table.get("r")?;
        let g: f32 = color_table.get("g")?;
        let b: f32 = color_table.get("b")?;
        let a: Option<f32> = color_table.get("a").ok();
        [r, g, b, a.unwrap_or(1.0)]
    } else {
        [0.3, 0.3, 0.3, 1.0]
    };
    return Ok(BorderConfig {
        width: width.unwrap_or(0.0),
        color,
        inactive_color,
    })
}

/// Helper: parse a shadow config table (reused by window rules).
fn parse_shadow_config(table: &Table) -> LuaResult<ShadowConfig> {
    let on: Option<bool> = table.get("enable").ok();
    let offset_x: Option<f64> = table.get("offset_x").ok();
    let offset_y: Option<f64> = table.get("offset_y").ok();
    let softness: Option<f64> = table.get("softness").ok();
    let spread: Option<f64> = table.get("spread").ok();
    let color = if let Value::Table(color_table) = table.get::<Value>("color")? {
        let r: f32 = color_table.get("r")?;
        let g: f32 = color_table.get("g")?;
        let b: f32 = color_table.get("b")?;
        let a: Option<f32> = color_table.get("a").ok();
        [r, g, b, a.unwrap_or(0.47)]
    } else {
        [0.0, 0.0, 0.0, 0.47]
    };
    let mut sc = ShadowConfig {
        color,
        ..ShadowConfig::default()
    };
    if let Some(on) = on {
        sc.enable = on;
    }
    if let Some(offset_x) = offset_x {
        sc.offset_x = offset_x;
    }
    if let Some(offset_y) = offset_y {
        sc.offset_y = offset_y;
    }
    if let Some(softness) = softness {
        sc.softness = softness;
    }
    if let Some(spread) = spread {
        sc.spread = spread;
    }
    return Ok(sc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_default_config() {
        let lua_code = include_str!("../resources/default-config.lua");
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert!(config.outputs.is_empty());
        assert!(!config.binds.is_empty());
        assert!(
            config
                .binds
                .iter()
                .any(|b| matches!(b.action, BindAction::Quit))
        );
        assert!(
            config
                .binds
                .iter()
                .any(|b| matches!(b.action, BindAction::Run(_)))
        );
        assert!(
            config
                .binds
                .iter()
                .any(|b| matches!(b.action, BindAction::Screenshot))
        );
        assert!(config.cursor.theme.is_none());
        assert!(config.cursor.size.is_none());
    }

    #[test]
    fn test_parse_full_config() {
        let lua_code = r#"
return {
    outputs = {
        {
            name = "eDP-1",
            mode = { width = 1920, height = 1080, refresh = 60 },
            position = { x = 0, y = 0 },
            scale = 1.5,
            transform = "normal",
        },
    },
    binds = {
        { modifiers = { "Super" }, key = "Return", action = { kind = "Run", command = "alacritty" } },
        { modifiers = { "Ctrl", "Alt" }, key = "BackSpace", action = { kind = "Quit" } },
    },
    env = {
        GTK_THEME = "Adwaita:dark",
    },
    cursor = {
        theme = "Adwaita",
        size = 32,
    },
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.outputs.len(), 1);
        assert_eq!(config.outputs[0].name, "eDP-1");
        assert!(config.outputs[0].mode.is_some());
        let mode = config.outputs[0].mode.as_ref().unwrap();
        assert_eq!(mode.width, 1920);
        assert_eq!(mode.height, 1080);
        assert_eq!(mode.refresh, Some(60));
        assert_eq!(config.outputs[0].position, Some((0, 0)));
        assert_eq!(config.outputs[0].scale, Some(1.5));
        assert_eq!(config.outputs[0].transform, Some("normal".to_string()));

        assert_eq!(config.binds.len(), 2);
        assert_eq!(config.binds[0].modifiers, vec!["Super"]);
        assert_eq!(config.binds[0].key, "Return");
        assert!(matches!(&config.binds[0].action, BindAction::Run(cmd) if cmd == "alacritty"));

        assert_eq!(config.env.get("GTK_THEME").unwrap(), "Adwaita:dark");

        assert_eq!(config.cursor.theme, Some("Adwaita".to_string()));
        assert_eq!(config.cursor.size, Some(32));
    }

    #[test]
    fn test_default_config_has_binds() {
        let config = Config::default();
        assert!(!config.binds.is_empty());
        assert!(config.outputs.is_empty());
        assert!(config.env.is_empty());
        assert!(config.cursor.theme.is_none());
        assert!(config.cursor.size.is_none());
    }

    #[test]
    fn test_spawn_and_run_sh_in_init() {
        let lua_code = r#"
return {
    binds = {
        { modifiers = { "Super" }, key = "q", action = { kind = "Quit" } },
    },
    init = function()
        bakawm.spawn("weston-terminal")
        bakawm.spawn("waybar")
        bakawm.run_sh("echo hello")
        bakawm.run_sh("sleep 1 && echo world")
    end,
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.init_commands, vec!["weston-terminal", "waybar"]);
        assert_eq!(
            config.init_shell_commands,
            vec!["echo hello", "sleep 1 && echo world"]
        );
    }

    #[test]
    fn test_spawn_and_run_sh_top_level() {
        let lua_code = r#"
bakawm.spawn("alacritty")
bakawm.run_sh("notify-send 'Welcome!'")

return {
    binds = {
        { modifiers = { "Super" }, key = "q", action = { kind = "Quit" } },
    },
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.init_commands, vec!["alacritty"]);
        assert_eq!(config.init_shell_commands, vec!["notify-send 'Welcome!'"]);
    }

    #[test]
    fn test_parse_animations_config() {
        let lua_code = r#"
return {
    binds = {
        { modifiers = { "Super" }, key = "q", action = { kind = "Quit" } },
    },
    animations = {
        enable = true,
        window_open = {
            enable = true,
            duration_ms = 200,
            curve = "ease-out-expo",
        },
        window_close = {
            enable = false,
            duration_ms = 300,
            curve = "linear",
        },
    },
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert!(config.animations.enable);
        assert!(config.animations.window_open.enable);
        assert_eq!(config.animations.window_open.duration_ms, 200);
        assert_eq!(config.animations.window_open.curve, AnimCurve::EaseOutExpo);
        assert!(!config.animations.window_close.enable);
        assert_eq!(config.animations.window_close.duration_ms, 300);
        assert_eq!(config.animations.window_close.curve, AnimCurve::Linear);
    }

    #[test]
    fn test_parse_cubic_bezier_curve() {
        let lua_code = r#"
return {
    binds = {
        { modifiers = { "Super" }, key = "q", action = { kind = "Quit" } },
    },
    animations = {
        window_open = {
            curve = { "cubic-bezier", 0.25, 0.1, 0.25, 1.0 },
        },
    },
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(
            config.animations.window_open.curve,
            AnimCurve::CubicBezier(0.25, 0.1, 0.25, 1.0)
        );
    }

    #[test]
    fn test_imperative_bind_api() {
        let lua_code = r#"
local mod = "Super"

bk.bind({ "Ctrl", "Alt" }, "BackSpace", bk.quit())
bk.bind({ mod }, "q", bk.close_window())
bk.bind({ mod }, "Return", bk.exec("alacritty"))
bk.bind({ mod, "Shift" }, "s", bk.screenshot())
bk.bind({ mod }, "1", bk.screen(0))
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.binds.len(), 5);
        assert_eq!(config.binds[0].modifiers, vec!["Ctrl", "Alt"]);
        assert_eq!(config.binds[0].key, "BackSpace");
        assert!(matches!(config.binds[0].action, BindAction::Quit));

        assert_eq!(config.binds[1].modifiers, vec!["Super"]);
        assert_eq!(config.binds[1].key, "q");
        assert!(matches!(config.binds[1].action, BindAction::CloseWindow));

        assert!(matches!(&config.binds[2].action, BindAction::Run(cmd) if cmd == "alacritty"));
        assert!(matches!(config.binds[3].action, BindAction::Screenshot));
        assert!(matches!(&config.binds[4].action, BindAction::Screen(n) if *n == 0));
    }

    #[test]
    fn test_imperative_config_api() {
        let lua_code = r#"
bk.env("GTK_THEME", "Adwaita:dark")
bk.env("XCURSOR_SIZE", "24")

bk.cursor({ theme = "Adwaita", size = 32 })

bk.window({
    prefer_no_csd = true,
    corner_radius = 10,
})

bk.blur({ enable = true, passes = 3 })

bk.animations({
    enable = true,
    window_open = { duration_ms = 200, curve = "ease-out-expo" },
    window_close = { enable = false },
})

bk.output({
    name = "eDP-1",
    mode = { width = 1920, height = 1080, refresh = 60 },
    position = { x = 0, y = 0 },
    scale = 1.5,
})

bk.bind({ "Super" }, "q", bk.quit())
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.env.get("GTK_THEME").unwrap(), "Adwaita:dark");
        assert_eq!(config.env.get("XCURSOR_SIZE").unwrap(), "24");

        assert_eq!(config.cursor.theme, Some("Adwaita".to_string()));
        assert_eq!(config.cursor.size, Some(32));

        assert!(config.window.prefer_no_csd);
        assert_eq!(config.window.corner_radius, CornerRadius::from(10.0));

        assert!(config.blur.enable);
        assert_eq!(config.blur.passes, 3);

        assert!(config.animations.enable);
        assert_eq!(config.animations.window_open.duration_ms, 200);
        assert_eq!(config.animations.window_open.curve, AnimCurve::EaseOutExpo);
        assert!(!config.animations.window_close.enable);

        assert_eq!(config.outputs.len(), 1);
        assert_eq!(config.outputs[0].name, "eDP-1");
        assert_eq!(config.outputs[0].scale, Some(1.5));
    }

    #[test]
    fn test_imperative_for_loop() {
        let lua_code = r#"
for i = 1, 3 do
    bk.bind({ "Super" }, tostring(i), bk.screen(i - 1))
end
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.binds.len(), 3);
        assert!(matches!(&config.binds[0].action, BindAction::Screen(n) if *n == 0));
        assert!(matches!(&config.binds[1].action, BindAction::Screen(n) if *n == 1));
        assert!(matches!(&config.binds[2].action, BindAction::Screen(n) if *n == 2));
    }

    #[test]
    fn test_imperative_callback_bind() {
        let lua_code = r#"
bk.bind({ "Super" }, "x", function()
    -- Custom Lua function as bind action
    os.execute("notify-send 'Hello'")
end)

bk.bind({ "Super" }, "q", bk.quit())
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.binds.len(), 2);
        // First bind should be a Callback
        assert!(matches!(config.binds[0].action, BindAction::Callback(0)));
        // Second bind is a normal action
        assert!(matches!(config.binds[1].action, BindAction::Quit));

        // LuaConfig should have been created with one callback
        assert!(config.lua_config.is_some());
        let lua_config = config.lua_config.as_ref().unwrap();
        assert_eq!(lua_config.callbacks.len(), 1);
    }

    #[test]
    fn test_imperative_on_start() {
        let lua_code = r#"
bk.on_start(function()
    bk.spawn("waybar")
    bk.spawn("mako")
    bk.run_sh("echo hello")
end)

bk.bind({ "Super" }, "q", bk.quit())
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.init_commands, vec!["waybar", "mako"]);
        assert_eq!(config.init_shell_commands, vec!["echo hello"]);
    }

    #[test]
    fn test_imperative_window_and_layer_rules() {
        let lua_code = r#"
bk.window_rule({
    app_id = "kitty",
    window = { border = { width = 2 }, corner_radius = 10 },
    blur = { enable = true, passes = 2, xray = true },
})

bk.window_rule({
    title = "Visual Studio Code",
    window = { corner_radius = 12 },
})

bk.layer_rule({
    namespace = "waybar",
    blur = { enable = true, passes = 2, xray = true },
})

bk.bind({ "Super" }, "q", bk.quit())
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.window_rules.len(), 2);
        assert_eq!(config.window_rules[0].app_id, Some("kitty".to_string()));
        assert_eq!(
            config.window_rules[1].title,
            Some("Visual Studio Code".to_string())
        );

        assert_eq!(config.layer_rules.len(), 1);
        assert_eq!(config.layer_rules[0].namespace, Some("waybar".to_string()));
    }

    #[test]
    fn test_imperative_spawn_and_run_sh_top_level() {
        let lua_code = r#"
bk.spawn("alacritty")
bk.run_sh("notify-send 'Welcome!'")

bk.bind({ "Super" }, "q", bk.quit())
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.init_commands, vec!["alacritty"]);
        assert_eq!(config.init_shell_commands, vec!["notify-send 'Welcome!'"]);
    }

    #[test]
    fn test_backward_compat_return_table() {
        // Old-style return { ... } config should still work
        let lua_code = r#"
return {
    binds = {
        { modifiers = { "Super" }, key = "q", action = { kind = "Quit" } },
        { modifiers = { "Super" }, key = "Return", action = { kind = "Run", command = "alacritty" } },
    },
    env = {
        GTK_THEME = "Adwaita:dark",
    },
    cursor = {
        theme = "Adwaita",
        size = 32,
    },
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.binds.len(), 2);
        assert!(matches!(config.binds[0].action, BindAction::Quit));
        assert!(matches!(&config.binds[1].action, BindAction::Run(cmd) if cmd == "alacritty"));
        assert_eq!(config.env.get("GTK_THEME").unwrap(), "Adwaita:dark");
        assert_eq!(config.cursor.theme, Some("Adwaita".to_string()));
        assert_eq!(config.cursor.size, Some(32));
    }

    #[test]
    fn test_exec_variadic_args() {
        let lua_code = r#"
local term = "alacritty"
bk.bind({ "Super" }, "Return", bk.exec(term, "-e", "bash"))
bk.bind({ "Super" }, "q", bk.exec("kitty"))
"#;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.lua");
        let mut file = fs::File::create(&config_path).unwrap();
        file.write_all(lua_code.as_bytes()).unwrap();

        let config = parse_lua_config(&config_path).unwrap();

        assert_eq!(config.binds.len(), 2);
        assert!(
            matches!(&config.binds[0].action, BindAction::Run(cmd) if cmd == "alacritty -e bash")
        );
        assert!(matches!(&config.binds[1].action, BindAction::Run(cmd) if cmd == "kitty"));
    }

    #[test]
    fn parse_anim_config_presence() {
        let lua = Lua::new();

        // A plain easing table with no `off`/`enable` keys must parse as a
        // curve, not `Off` (regression test: the `off` presence check used to
        // return `Some(Value::Nil)` for a missing key, forcing every table to
        // `Off`).
        let t = lua.create_table().unwrap();
        t.set("duration_ms", 250).unwrap();
        t.set("curve", "ease-out-cubic").unwrap();
        let parsed = parse_anim_config(&t).unwrap();
        assert_eq!(
            parsed,
            AnimConfig::Curve {
                duration_ms: 250,
                curve: AnimCurve::EaseOutCubic
            }
        );

        // An explicit `off = true` must still disable the animation.
        let t = lua.create_table().unwrap();
        t.set("off", true).unwrap();
        assert_eq!(parse_anim_config(&t).unwrap(), AnimConfig::Off);

        // `enable = false` must disable, `enable = true` must not.
        let t = lua.create_table().unwrap();
        t.set("enable", false).unwrap();
        assert_eq!(parse_anim_config(&t).unwrap(), AnimConfig::Off);
        let t = lua.create_table().unwrap();
        t.set("enable", true).unwrap();
        t.set("duration_ms", 100).unwrap();
        assert_ne!(parse_anim_config(&t).unwrap(), AnimConfig::Off);

        // Springs must still parse.
        let t = lua.create_table().unwrap();
        let spring = lua.create_table().unwrap();
        spring.set("damping_ratio", 0.8).unwrap();
        spring.set("stiffness", 300.0).unwrap();
        t.set("spring", spring).unwrap();
        assert_eq!(
            parse_anim_config(&t).unwrap(),
            AnimConfig::Spring {
                damping_ratio: 0.8,
                stiffness: 300.0,
                epsilon: 0.01
            }
        );
    }
}
