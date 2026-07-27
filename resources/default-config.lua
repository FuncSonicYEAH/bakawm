-- bakawm configuration file
-- This file is loaded from ~/.config/bakawm/config.lua

return {
    -- Output configuration
    -- Configure display outputs by name
    outputs = {
        -- {
        --     name = "eDP-1",
        --     mode = { width = 1920, height = 1080, refresh = 60 },
        --     position = { x = 0, y = 0 },
        --     scale = 1.0,
        --     transform = "normal", -- normal, 90, 180, 270, flipped, flipped-90, flipped-180, flipped-270
        -- },
        -- {
        --     name = "HDMI-A-1",
        --     mode = { width = 2560, height = 1440, refresh = 144 },
        --     position = { x = 1920, y = 0 },
        --     scale = 1.25,
        -- },
    },

    -- Key bindings
    binds = {
        -- Quit the compositor
        { modifiers = { "Ctrl", "Alt" }, key = "BackSpace", action = { kind = "Quit" } },

        -- Close the focused window
        { modifiers = { "Super" },       key = "q",          action = { kind = "CloseWindow" } },

        -- Run a terminal
        { modifiers = { "Super" },       key = "Return",     action = { kind = "Run", command = "alacritty" } },

        -- Screenshot
        { modifiers = { "Super", "Shift" }, key = "s",       action = { kind = "Screenshot" } },

        -- Toggle decorations
        { modifiers = { "Super", "Shift" }, key = "d",       action = { kind = "ToggleDecorations" } },

        -- Toggle window preview
        { modifiers = { "Super", "Shift" }, key = "w",       action = { kind = "TogglePreview" } },

        -- Scale
        { modifiers = { "Super", "Shift" }, key = "p",       action = { kind = "ScaleUp" } },
        { modifiers = { "Super", "Shift" }, key = "m",       action = { kind = "ScaleDown" } },

        -- Rotate output
        { modifiers = { "Super", "Shift" }, key = "r",       action = { kind = "RotateOutput" } },

        -- Toggle tint
        { modifiers = { "Super", "Shift" }, key = "t",       action = { kind = "ToggleTint" } },

        -- Switch to screen
        { modifiers = { "Super" }, key = "1", action = { kind = "Screen", n = 0 } },
        { modifiers = { "Super" }, key = "2", action = { kind = "Screen", n = 1 } },
        { modifiers = { "Super" }, key = "3", action = { kind = "Screen", n = 2 } },

        -- Example: custom commands
        -- { modifiers = { "Super" }, key = "e", action = { kind = "Run", command = "nautilus" } },
        -- { modifiers = { "Super" }, key = "b", action = { kind = "Run", command = "firefox" } },
    },

    -- Environment variables
    -- These will be set when running commands from keybindings
    env = {
        -- WAYLAND_DISPLAY is set automatically
        -- Example: set custom GTK theme
        -- GTK_THEME = "Adwaita:dark",
        -- XCURSOR_THEME = "Adwaita",
        -- XCURSOR_SIZE = "24",
    },

    -- Cursor configuration
    cursor = {
        -- Theme name (leave nil to use system default from XCURSOR_THEME env var)
        theme = nil, -- e.g. "Adwaita", "Breeze", "default"

        -- Cursor size in pixels (leave nil to use system default from XCURSOR_SIZE env var)
        size = nil, -- e.g. 24, 32, 48
    },

    -- Window configuration
    window = {
        -- Prefer no client-side decorations
        -- When enabled, the compositor will request server-side decorations and
        -- set the tiled state to make windows rectangular (removing client-side rounded corners).
        prefer_no_csd = true,

        -- Border configuration
        border = {
            -- Border width in pixels (0 to disable)
            width = 0,
            -- Border color for the active/focused window (RGBA, each component 0.0 to 1.0)
            color = { r = 0.0, g = 0.0, b = 0.0, a = 1.0 },
            -- Border color for inactive/unfocused windows (RGBA, each component 0.0 to 1.0)
            inactive_color = { r = 0.3, g = 0.3, b = 0.3, a = 1.0 },
        },

        -- Shadow configuration
        shadow = {
            -- Enable or disable shadows
            enable = true,
            -- Shadow offset in pixels
            offset_x = -10,
            offset_y = -10,
            -- Shadow softness (blur radius) in pixels
            softness = 20,
            -- Shadow spread in pixels (positive expands, negative shrinks)
            spread = 5,
            -- Shadow color (RGBA, each component 0.0 to 1.0)
            color = { r = 0.0, g = 0.0, b = 0.0, a = 0.47 },
        },

        -- Corner radius in pixels (0 for sharp corners)
        corner_radius = 0,

        -- Modifier key that must be held to resize windows with the mouse
        -- Possible values: "Ctrl", "Alt", "Super", "Shift"
        -- Set to "" to allow resizing without any modifier
        resize_modifier = "Ctrl",
    },

    -- Blur configuration
    -- Controls the background blur effect for windows that request it
    -- (via the ext-background-effect-v1 protocol).
    blur = {
        -- Enable or disable blur globally
        enable = true,
        -- Number of blur passes (more passes = smoother but slower)
        passes = 2,
        -- Blur offset/spread in pixels
        offset = 1.0,
        -- Xray mode: when enabled, blur only captures layer shell surfaces
        -- (background/bottom layers like wallpaper and panels). When disabled,
        -- blur captures everything behind the window including other windows.
        -- Default: false (blur captures all content behind the window)
        xray = false,
    },

    -- Animations configuration
    -- Controls window open/close animations.
    animations = {
        -- Globally enable all animations
        enable = true,

        -- Window open animation (scales up from scale to 100%)
        window_open = {
            -- Enable this specific animation
            enable = true,
            -- Duration in milliseconds
            duration_ms = 250,
            -- Easing curve: "linear", "ease-out-quad", "ease-out-cubic", "ease-out-expo",
            -- or { "cubic-bezier", x1, y1, x2, y2 }
            curve = "ease-out-cubic",
            -- Scale factor at start of animation (0.0 to 1.0)
            -- Window starts at this scale and animates to 1.0
            -- e.g. 0.8 = start at 80% size, 0.5 = start at half size
            scale = 0.8,
        },

        -- Window close animation (shrinks from 100% to scale)
        window_close = {
            enable = true,
            duration_ms = 250,
            curve = "ease-out-cubic",
            -- Scale factor at end of animation (0.0 to 1.0)
            -- Window shrinks from 1.0 down to this scale
            -- e.g. 0.0 = shrink to nothing, 0.8 = shrink to 80% size
            scale = 0.0,
        },

        -- ── Animation Presets ──────────────────────────────────────────
        -- Uncomment one of the following presets to replace the defaults above.
        --
        -- Fast & snappy (no scale, short duration):
        -- window_open  = { enable = true, duration_ms = 120, curve = "ease-out-cubic", scale = 1.0 },
        -- window_close = { enable = true, duration_ms = 100, curve = "linear",        scale = 1.0 },
        --
        -- Smooth & elegant (slower, noticeable scale):
        -- window_open  = { enable = true, duration_ms = 350, curve = "ease-out-expo",  scale = 0.85 },
        -- window_close = { enable = true, duration_ms = 300, curve = "ease-out-cubic", scale = 0.0 },
        --
        -- Dramatic pop-in/pop-out:
        -- window_open  = { enable = true, duration_ms = 400, curve = { "cubic-bezier", 0.34, 1.56, 0.64, 1.0 }, scale = 0.5 },
        -- window_close = { enable = true, duration_ms = 300, curve = "ease-out-expo",                          scale = 0.0 },
        --
        -- Minimal (only fade, no scale):
        -- window_open  = { enable = true, duration_ms = 200, curve = "ease-out-cubic", scale = 1.0 },
        -- window_close = { enable = true, duration_ms = 200, curve = "ease-out-cubic", scale = 1.0 },
        --
        -- Disable animations entirely:
        -- enable = false,
    },

    -- Window rules
    -- Apply overrides to windows matching by app_id and/or title.
    -- Rules are evaluated in order; the first matching rule wins.
    -- The `window` field uses the same format as the global `window` config above.
    -- window_rules = {
    --     {
    --         app_id = "kitty",
    --         window = {
    --             border = { width = 2, color = { r = 0.2, g = 0.5, b = 0.8, a = 1.0 },
    --                       inactive_color = { r = 0.1, g = 0.3, b = 0.5, a = 1.0 } },
    --             shadow = { enable = true, offset_x = -10, offset_y = -10, softness = 20, spread = 5,
    --                        color = { r = 0.0, g = 0.0, b = 0.0, a = 0.47 } },
    --             corner_radius = 10,
    --         },
    --         blur = { enable = true, passes = 2, offset = 1.0, xray = true },
    --     },
    --     {
    --         app_id = "firefox",
    --         window = { border = { width = 0 }, shadow = { enable = false } },
    --         blur = { enable = false },
    --     },
    --     {
    --         -- Match by title substring
    --         title = "Visual Studio Code",
    --         window = { corner_radius = 12, border = { width = 1, color = { r = 0.3, g = 0.3, b = 0.3, a = 1.0 } } },
    --         blur = { enable = true, xray = false },
    --     },
    --     {
    --         -- Match by both app_id AND title
    --         app_id = "discord",
    --         title = "Discord",
    --         window = { border = { width = 3, color = { r = 0.58, g = 0.27, b = 0.98, a = 1.0 } },
    --                    shadow = { enable = true }, corner_radius = 8 },
    --         blur = { enable = true, passes = 3, xray = true },
    --     },
    -- },
    window_rules = {},

    -- Layer rules
    -- Apply overrides to layer-shell surfaces matching by namespace.
    -- Rules are evaluated in order; the first matching rule wins.
    -- layer_rules = {
    --     {
    --         namespace = "waybar",
    --         blur = { enable = true, passes = 2, offset = 1.0, xray = true },
    --     },
    --     {
    --         namespace = "rofi",
    --         blur = { enable = true, passes = 3, offset = 2.0, xray = true },
    --     },
    --     {
    --         -- Match any layer surface (wildcard)
    --         namespace = "swaync",
    --         blur = { enable = true, passes = 2, xray = true },
    --     },
    -- },
    layer_rules = {},

    -- Initialization function
    -- Called when the compositor starts up.
    -- Use bakawm.spawn() to run programs, bakawm.run_sh() to run shell code.
    -- WAYLAND_DISPLAY and DISPLAY (if XWayland is enabled) are set automatically.
    -- init = function()
    --     -- Spawn a program at startup
    --     bakawm.spawn("weston-terminal")
    --
    --     -- Run shell code at startup
    --     bakawm.run_sh("echo 'bakawm started' > /tmp/bakawm.log")
    --
    --     -- Spawn multiple programs
    --     bakawm.spawn("waybar")
    --     bakawm.spawn("mako")
    -- end,
}