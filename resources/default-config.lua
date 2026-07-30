-- bakawm config — ~/.config/bakawm/config.lua
-- Full Lua: use if/for/functions freely. See `bk.help()` for API reference.

local mod = "Super"
local term = "kitty"
local menu = "fuzzel"

-- ── Binds ──────────────────────────────────────────────────────

bk.bind({ "Ctrl", "Alt" }, "BackSpace", bk.quit())
bk.bind({ mod }, "q",              bk.close_window())
bk.bind({ mod }, "Return",         bk.exec(term))
bk.bind({ mod }, "Z",         bk.exec(menu))
bk.bind({ mod, "Shift" }, "s",     bk.screenshot())
bk.bind({ mod, "Shift" }, "d",     bk.toggle_decorations())
bk.bind({ mod, "Shift" }, "w",     bk.toggle_preview())
bk.bind({ mod, "Shift" }, "p",     bk.scale_up())
bk.bind({ mod, "Shift" }, "m",     bk.scale_down())
bk.bind({ mod, "Shift" }, "r",     bk.rotate_output())
bk.bind({ mod, "Shift" }, "t",     bk.toggle_tint())
bk.bind({ mod }, "1", bk.screen(0))
bk.bind({ mod }, "2", bk.screen(1))
bk.bind({ mod }, "3", bk.screen(2))

-- Example: loop binds
-- for i = 1, 9 do bk.bind({ mod }, tostring(i), bk.screen(i - 1)) end

-- Example: custom function / exec with args
-- bk.exec("alacritty", "-e", "nvim")  → runs: alacritty -e nvim
-- bk.bind({ mod }, "x", function() os.execute("notify-send hi") end)

-- ── Outputs ────────────────────────────────────────────────────

-- bk.output({ name = "eDP-1", mode = { width = 1920, height = 1080, refresh = 60 }, scale = 1.0 })
-- bk.output({ name = "HDMI-A-1", mode = { width = 2560, height = 1440, refresh = 144 }, scale = 1.25 })

-- ── Env ────────────────────────────────────────────────────────

-- bk.env("GTK_THEME", "Adwaita:dark")
-- bk.env("XCURSOR_SIZE", "24")

-- ── Cursor ─────────────────────────────────────────────────────

-- bk.cursor({ theme = "Adwaita", size = 32 })

-- ── Window ─────────────────────────────────────────────────────

bk.window({
    prefer_no_csd = true,
    border  = { width = 1, color = { r = 0, g = 0, b = 0, a = 1 }, inactive_color = { r = .3, g = .3, b = .3, a = 1 } },
    shadow  = { enable = true, offset_x = -20, offset_y = -10, softness = 20, spread = 5, color = { r = 0, g = 0, b = 0, a = .47 } },
    corner_radius = 8,
    resize_modifier = "Super",
})

-- ── Blur ───────────────────────────────────────────────────────

bk.blur({ enable = true, passes = 2, offset = 1.0, xray = false })

-- ── Animations ─────────────────────────────────────────────────

bk.animations({
    enable = true,
    window_open  = { enable = true, duration_ms = 250, curve = "ease-out-cubic", scale = 0.8 },
    window_close = { enable = true, duration_ms = 250, curve = "ease-out-cubic", scale = 0.8 },
    -- Presets: fast / smooth / dramatic / none
    -- window_open  = { enable = true, duration_ms = 120, curve = "ease-out-cubic", scale = 1.0 },
    -- window_close = { enable = true, duration_ms = 100, curve = "linear",        scale = 1.0 },
})

-- ── Rules ──────────────────────────────────────────────────────

-- bk.window_rule({ app_id = "kitty",   window = { corner_radius = 10 }, blur = { enable = true } })
-- bk.window_rule({ title = "VS Code",  window = { corner_radius = 12 } })
-- bk.layer_rule({ namespace = "waybar", blur = { enable = true, xray = true } })

-- ── Autostart ──────────────────────────────────────────────────

-- bk.on_start(function()
--     bk.spawn("waybar")
--     bk.spawn("mako")
-- end)
