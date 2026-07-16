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
    },

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