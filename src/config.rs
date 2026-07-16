use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::{Lua, Result as LuaResult, Table, Value};
use notify::{RecommendedWatcher, RecursiveMode, Watcher, Event, EventKind};
use smithay::reexports::calloop::{channel, timer::{Timer, TimeoutAction}};
use tracing::{info, warn};

const CONFIG_DIR_NAME: &str = "bakawm";
const CONFIG_FILE_NAME: &str = "config.lua";

#[derive(Debug, Clone)]
pub struct Config {
    pub outputs: Vec<OutputConfig>,
    pub binds: Vec<BindConfig>,
    pub env: HashMap<String, String>,
    pub cursor: CursorConfig,
    pub window: WindowConfig,
    pub blur: BlurConfig,
    pub window_rules: Vec<WindowRule>,
    pub layer_rules: Vec<LayerRule>,
    pub init_commands: Vec<String>,
    pub init_shell_commands: Vec<String>,
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
    VtSwitch(i32),
    Screen(usize),
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
        [
            value.top_left,
            value.top_right,
            value.bottom_right,
            value.bottom_left,
        ]
    }
}

impl From<f32> for CornerRadius {
    fn from(value: f32) -> Self {
        Self {
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

        Self {
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

        self
    }

    pub fn scaled_by(self, scale: f32) -> Self {
        Self {
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
        BorderConfig {
            width: 0.0,
            color: [0.0, 0.0, 0.0, 1.0],
            inactive_color: [0.3, 0.3, 0.3, 1.0],
        }
    }
}

impl Default for ShadowConfig {
    fn default() -> Self {
        ShadowConfig {
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
        BlurConfig {
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
}

impl PartialWindowConfig {
    /// Apply this partial config on top of a base WindowConfig, returning the merged result.
    pub fn merge_over(&self, base: &WindowConfig) -> WindowConfig {
        WindowConfig {
            prefer_no_csd: self.prefer_no_csd.unwrap_or(base.prefer_no_csd),
            border: self.border.as_ref().unwrap_or(&base.border).clone(),
            shadow: self.shadow.unwrap_or(base.shadow),
            corner_radius: self.corner_radius.unwrap_or(base.corner_radius),
            resize_modifier: self.resize_modifier.as_ref().unwrap_or(&base.resize_modifier).clone(),
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

impl Default for WindowConfig {
    fn default() -> Self {
        WindowConfig {
            prefer_no_csd: true,
            border: BorderConfig {
                width: 0.0,
                color: [0.0, 0.0, 0.0, 1.0],
                inactive_color: [0.3, 0.3, 0.3, 1.0],
            },
            shadow: ShadowConfig::default(),
            corner_radius: CornerRadius::default(),
            resize_modifier: "Ctrl".into(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
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
            window_rules: Vec::new(),
            layer_rules: Vec::new(),
            init_commands: Vec::new(),
            init_shell_commands: Vec::new(),
        }
    }
}

impl Config {
    /// Find the first window rule matching the given app_id and title.
    pub fn find_window_rule(&self, app_id: Option<&str>, title: Option<&str>) -> Option<&WindowRule> {
        self.window_rules.iter().find(|rule| {
            let matches = match (&rule.app_id, &rule.title) {
                (Some(rule_id), Some(rule_title)) => {
                    app_id.map_or(false, |id| id.contains(rule_id))
                        && title.map_or(false, |t| t.contains(rule_title))
                }
                (Some(rule_id), None) => app_id.map_or(false, |id| id.contains(rule_id)),
                (None, Some(rule_title)) => title.map_or(false, |t| t.contains(rule_title)),
                (None, None) => true,
            };
            matches
        })
    }
}

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(CONFIG_DIR_NAME)
}

pub fn config_path() -> PathBuf {
    config_dir().join(CONFIG_FILE_NAME)
}

pub fn load_config() -> Config {
    let path = config_path();

    if !path.exists() {
        info!("No config file found at {:?}, creating default config", path);
        if let Err(e) = create_default_config() {
            warn!("Failed to create default config: {}", e);
        }
        return Config::default();
    }

    match parse_lua_config(&path) {
        Ok(config) => {
            info!("Loaded config from {:?}", path);
            config
        }
        Err(e) => {
            warn!("Failed to parse config file {:?}: {}, using defaults", path, e);
            Config::default()
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
            info!("Reloaded config from {:?}", path);
            new_config
        }
        Err(e) => {
            warn!("Failed to reload config file {:?}: {}, keeping current config", path, e);
            current.clone()
        }
    }
}

pub fn spawn_config_watcher<B: crate::state::Backend + 'static>(
    handle: &smithay::reexports::calloop::LoopHandle<'static, crate::state::AnvilState<B>>,
) -> Option<notify::RecommendedWatcher> {
    let (sender, source) = channel::channel::<()>();

    let watcher_result = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                match event.kind {
                    EventKind::Create(_)
                    | EventKind::Modify(_)
                    | EventKind::Remove(_) => {
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
    if config_dir.exists() {
        if let Err(e) = watcher.watch(&config_dir, RecursiveMode::NonRecursive) {
            warn!("Failed to watch config directory {:?}: {}", config_dir, e);
            return None;
        }
    }

    let config_file = config_path();
    if config_file.exists() {
        if let Err(e) = watcher.watch(&config_file, RecursiveMode::NonRecursive) {
            warn!("Failed to watch config file {:?}: {}", config_file, e);
            return None;
        }
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
                    TimeoutAction::Drop
                },
            );
        }
    }) {
        warn!("Failed to insert config watcher into event loop: {}", e);
        return None;
    }

    info!("Watching config file for changes at {:?}", config_file);
    Some(watcher)
}

fn create_default_config() -> Result<(), Box<dyn std::error::Error>> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;

    let default_lua = include_str!("../resources/default-config.lua");
    fs::write(config_path(), default_lua)?;
    Ok(())
}

fn parse_lua_config(path: &PathBuf) -> LuaResult<Config> {
    let lua = Lua::new();

    let spawn_commands: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let shell_commands: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let spawn_fn = lua.create_function({
        let spawn_commands = spawn_commands.clone();
        move |_, cmd: String| {
            spawn_commands.lock().unwrap().push(cmd);
            Ok(())
        }
    })?;

    let run_sh_fn = lua.create_function({
        let shell_commands = shell_commands.clone();
        move |_, code: String| {
            shell_commands.lock().unwrap().push(code);
            Ok(())
        }
    })?;

    let bakawm_table = lua.create_table()?;
    bakawm_table.set("spawn", spawn_fn)?;
    bakawm_table.set("run_sh", run_sh_fn)?;
    lua.globals().set("bakawm", bakawm_table)?;

    let code = fs::read_to_string(path)?;
    let result = lua.load(&code).eval::<Table>()?;

    let mut config = Config::default();
    config.binds.clear();

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

    if let Value::Table(window_rules) = result.get::<Value>("window_rules")? {
        config.window_rules = parse_window_rules(&window_rules)?;
    }

    if let Value::Table(layer_rules) = result.get::<Value>("layer_rules")? {
        config.layer_rules = parse_layer_rules(&layer_rules)?;
    }

    if let Value::Function(init_fn) = result.get::<Value>("init")? {
        init_fn.call::<()>(())?;
    }

    config.init_commands = spawn_commands.lock().unwrap().clone();
    config.init_shell_commands = shell_commands.lock().unwrap().clone();

    Ok(config)
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
    Ok(outputs)
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
    Ok(binds)
}

fn parse_bind_action(table: &Table) -> LuaResult<BindAction> {
    let kind: String = table.get("kind")?;
    match kind.as_str() {
        "Quit" => Ok(BindAction::Quit),
        "CloseWindow" => Ok(BindAction::CloseWindow),
        "Run" => {
            let command: String = table.get("command")?;
            Ok(BindAction::Run(command))
        }
        "Screenshot" => Ok(BindAction::Screenshot),
        "ToggleDecorations" => Ok(BindAction::ToggleDecorations),
        "TogglePreview" => Ok(BindAction::TogglePreview),
        "ScaleUp" => Ok(BindAction::ScaleUp),
        "ScaleDown" => Ok(BindAction::ScaleDown),
        "RotateOutput" => Ok(BindAction::RotateOutput),
        "ToggleTint" => Ok(BindAction::ToggleTint),
        "VtSwitch" => {
            let n: i32 = table.get("n")?;
            Ok(BindAction::VtSwitch(n))
        }
        "Screen" => {
            let n: usize = table.get("n")?;
            Ok(BindAction::Screen(n))
        }
        other => Err(mlua::Error::external(format!(
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
    Ok(env)
}

fn parse_cursor(table: &Table) -> LuaResult<CursorConfig> {
    let theme: Option<String> = table.get("theme").ok();
    let size: Option<u32> = table.get("size").ok();
    Ok(CursorConfig { theme, size })
}

fn parse_window(table: &Table) -> LuaResult<WindowConfig> {
    let prefer_no_csd: Option<bool> = table.get("prefer_no_csd").ok();
    let resize_modifier: Option<String> = table.get("resize_modifier").ok();

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

    Ok(window)
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
    Ok(blur)
}

fn parse_blur_override(table: &Table) -> LuaResult<BlurOverride> {
    let enable: bool = table.get("enable")?;
    let passes = table.get::<u8>("passes").ok();
    let offset = table.get::<f64>("offset").ok();
    let xray = table.get::<bool>("xray").ok();
    Ok(BlurOverride {
        enable,
        passes,
        offset,
        xray,
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
    Ok(rules)
}

/// Parse a partial window config where all fields are optional.
/// Only fields explicitly set in the Lua table will be Some.
fn parse_partial_window(table: &Table) -> LuaResult<PartialWindowConfig> {
    let prefer_no_csd: Option<bool> = table.get("prefer_no_csd").ok();
    let resize_modifier: Option<String> = table.get("resize_modifier").ok();

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
        table.get::<f32>("corner_radius").ok().map(CornerRadius::from)
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

    Ok(PartialWindowConfig {
        prefer_no_csd,
        border,
        shadow,
        corner_radius,
        resize_modifier,
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
    Ok(rules)
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
    Ok(BorderConfig {
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
    let mut sc = ShadowConfig { color, ..ShadowConfig::default() };
    if let Some(on) = on { sc.enable = on; }
    if let Some(offset_x) = offset_x { sc.offset_x = offset_x; }
    if let Some(offset_y) = offset_y { sc.offset_y = offset_y; }
    if let Some(softness) = softness { sc.softness = softness; }
    if let Some(spread) = spread { sc.spread = spread; }
    Ok(sc)
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
        assert!(config.binds.iter().any(|b| matches!(b.action, BindAction::Quit)));
        assert!(config.binds.iter().any(|b| matches!(b.action, BindAction::Run(_))));
        assert!(config.binds.iter().any(|b| matches!(b.action, BindAction::Screenshot)));
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
        assert_eq!(config.init_shell_commands, vec!["echo hello", "sleep 1 && echo world"]);
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
}