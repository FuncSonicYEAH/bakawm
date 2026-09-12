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

-- Layout / workspace navigation:
-- bk.bind({ mod, "Shift" }, "Left",  bk.focus_prev())
-- bk.bind({ mod, "Shift" }, "Right", bk.focus_next())
-- bk.bind({ mod, "Shift" }, "Down",  bk.workspace_next())   -- workspaces extend infinitely downward
-- bk.bind({ mod, "Shift" }, "Up",    bk.workspace_prev())
-- bk.bind({ mod }, "4", bk.workspace(3))                    -- jump to workspace 4
bk.bind({ mod }, "space", bk.toggle_floating())            -- toggle focused window tiled/floating (layout mode)

-- Layout width / fullscreen:
-- bk.bind({ mod, "Shift" }, "J", bk.resize_width_down())    -- shrink focused column
-- bk.bind({ mod, "Shift" }, "K", bk.resize_width_up())      -- grow focused column
-- bk.bind({ mod }, "F", bk.toggle_fullscreen())             -- true fullscreen (XDG/X11)
-- bk.bind({ mod }, "M", bk.toggle_maximize())               -- windowed fullscreen (fills work area)

-- Example: loop binds
-- for i = 1, 9 do bk.bind({ mod }, tostring(i), bk.screen(i - 1)) end
-- for i = 1, 9 do bk.bind({ mod, "Alt" }, tostring(i), bk.workspace(i - 1)) end

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
    -- shader = "sepia",   -- apply a custom shader to every window (see bk.shader below)
})

-- ── Layout ─────────────────────────────────────────────────────

-- bk.layout({ type = "columns", gap = 8,
--             margins = { top = 0, bottom = 0, left = 0, right = 0 },
--             animation = { move = { duration_ms = 250, curve = "ease-out-cubic" },
--                           resize = { spring = { damping_ratio = 0.8, stiffness = 300 } } } })
-- bk.layout({ type = "grid" })                -- types: floating | columns | grid | master-stack | maximize | custom
-- bk.layout({ type = "master-stack", master_ratio = 0.6 })

-- Custom layout via Lua: fn(windows, area) must return a table of {x,y,w,h}
-- rects aligned with `windows` (nil entries leave the window floating).
-- windows[i] = { id = <layout id>, w = <logical px>, h = <logical px> }
-- area       = { x, y, w, h } of the margin-adjusted work area (spacing between
-- windows is up to the function; the config `gap` is not applied automatically).
-- bk.layout({ type = "custom", animation = { move = { duration_ms = 250 } },
--             fn = function(windows, area)
--                 local gap = 8
--                 local n = #windows
--                 local w = (area.w - gap * (n - 1)) / n
--                 local rects = {}
--                 for i = 1, n do
--                     rects[i] = { x = area.x + (i - 1) * (w + gap), y = area.y,
--                                  w = w, h = area.h }
--                 end
--                 return rects
--             end })

-- -- ── Scrolling layout (full layout code i think) ──────────────────────────────
-- 
-- Tunables
-- local gap     = 8    -- spacing between columns (fallback; area.gap is the config `gap`)
-- local ratio   = 0.4  -- column width as a fraction of the work-area width
-- local min_w   = 280  -- clamp column width (logical px)
-- local max_w   = 900  -- clamp column width (logical px)
-- 
-- Persistent state, kept across layout calls via upvalues.
-- One keyed state per output (keyed by the work-area X position).
-- local columns_by = {}  -- output key -> ordered list of window layout ids
-- local active_by  = {}  -- output key -> focused window layout id
-- local offset_by  = {}  -- output key -> horizontal view offset (<= 0)
-- 
-- local function clamp(v, lo, hi)
--     if v < lo then return lo end
--     if v > hi then return hi end
--     return v
-- end
-- 
-- local function index_of(t, id)
--     for i, v in ipairs(t) do
--         if v == id then return i end
--     end
--     return nil
-- end
-- 
-- local function col_x(i, col_w, gap)
--     return (i - 1) * (col_w + gap)
-- end
-- 
-- Global layout variable: the scrolling-layout function.
-- function layout(windows, area)
--     local gap = area.gap or gap   -- honor the config `gap` (bk.layout gap = ...)
--     local key = area.x
--     local columns = columns_by[key] or {}
--     local active  = active_by[key]
--     local offset  = offset_by[key] or 0
-- 
--     local focused = windows[1] and windows[1].id or nil
-- 
--     -- Drop columns whose window no longer exists.
--     local present = {}
--     for _, win in ipairs(windows) do present[win.id] = true end
--     local next_cols = {}
--     for _, id in ipairs(columns) do
--         if present[id] then next_cols[#next_cols + 1] = id end
--     end
--     columns = next_cols
-- 
--     -- Insert new windows as new columns right after the active column.
--     local insert_at = index_of(columns, active) or 0
--     for _, win in ipairs(windows) do
--         if not index_of(columns, win.id) then
--             insert_at = insert_at + 1
--             table.insert(columns, insert_at, win.id)
--         end
--     end
-- 
--     local n = #columns
--     if n == 0 then
--         offset_by[key] = 0
--         active_by[key] = nil
--         columns_by[key] = columns
--         return {}
--     end
-- 
--     local col_w = clamp(area.w * ratio, min_w, max_w)
--     col_w = math.min(col_w, area.w - 2 * gap)
-- 
--     -- Per-window width overrides (win.width > 0, set via bk.resize_width_*):
--     -- a column honoring it keeps its own width, others share the default.
--     local hint = {}
--     for _, win in ipairs(windows) do hint[win.id] = win.width or 0 end
-- 
--     -- Column widths and start offsets (logical px), in column order.
--     local widths, starts = {}, {}
--     local total_w = 0
--     for i, id in ipairs(columns) do
--         local h = hint[id]
--         local w = (h and h > 0) and h or col_w
--         widths[i] = w
--         starts[i] = total_w
--         total_w = total_w + w + gap
--     end
--     total_w = total_w - gap  -- last column has no trailing gap
-- 
--     local view_w = area.w
-- 
--     if focused ~= active then
--         active = focused
--     end
-- 
--     -- Compute the view offset for the active column (niri-style fit
--     -- logic: prefer the alignment that causes the least motion).
--     if total_w > view_w and active then
--         local ai = index_of(columns, active) or 1
--         local cx = starts[ai]
--         local cw = widths[ai]
--         local already_visible =
--             cx + offset >= -0.001 and cx + cw + offset <= view_w + 0.001
--         local target
--         if already_visible then
--             target = offset
--         else
--             local to_left  = math.abs(offset - (-cx))
--             local to_right = math.abs(offset - (view_w - cw - cx))
--             if to_left <= to_right then
--                 target = -cx
--             else
--                 target = view_w - cw - cx
--             end
--         end
--         offset = clamp(target, -(total_w - view_w), 0)
--     else
--         offset = 0
--     end
-- 
--     -- Return rects aligned with `windows` (the WM zips them by index).
--     local by_id = {}
--     for i, id in ipairs(columns) do by_id[id] = i end
--     local rects = {}
--     for i, win in ipairs(windows) do
--         local ci = by_id[win.id]
--         if ci then
--             rects[i] = {
--                 x = math.floor(area.x + starts[ci] + offset + 0.5),
--                 y = math.floor(area.y + gap + 0.5),
--                 w = math.floor(widths[ci] + 0.5),
--                 h = math.max(1, math.floor(area.h - 2 * gap + 0.5)),
--             }
--         else
--             rects[i] = nil
--         end
--     end
-- 
--     columns_by[key] = columns
--     active_by[key] = active
--     offset_by[key] = offset
--     return rects
-- end
-- 
-- The layout (bk.layout) uses the global `layout` variable.
-- bk.layout({
--     type = "custom",
--     gap = 8,          -- exposed to the layout fn as `area.gap`
--     animation = {
--         move   = { duration_ms = 250, curve = "ease-out-cubic" },
--         resize = { spring = { damping_ratio = 0.8, stiffness = 300 } },
--     },
--     fn = layout,
-- })

-- ── Custom shaders ─────────────────────────────────────────────

-- A "postprocess" shader only defines `vec4 postprocess(vec4 color)` which runs
-- after the default texture sampling/alpha. The `fragment` field can be inline
-- GLSL or a path to a .frag file relative to the config directory.
bk.shader({ name = "sepia", kind = "postprocess",
             uniforms = { intensity = 0.8 },
             fragment = [[
                 vec4 postprocess(vec4 color) {
                     float lum = dot(color.rgb, vec3(0.299, 0.587, 0.114));
                     vec3 sepia = vec3(lum);
                     sepia.r *= 1.07; sepia.g *= 0.99; sepia.b *= 0.75;
                     return vec4(mix(color.rgb, sepia, intensity), color.a);
                 }
             ]] })

-- Apply it to windows: bk.window({ shader = "sepia" }) or per-app:
bk.window_rule({ app_id = "kitty", window = { shader = "sepia" } })

-- ── Blur ───────────────────────────────────────────────────────

bk.blur({ enable = true, passes = 2, offset = 1.0, xray = false })

-- ── Animations ─────────────────────────────────────────────────

bk.animations({
    enable = true,
    window_open  = { enable = true, duration_ms = 250, curve = "ease-out-cubic", scale = 0.8 },
    window_close = { enable = true, duration_ms = 250, curve = "ease-out-cubic", scale = 0.8 },
    workspace_switch = { enable = true, duration_ms = 200, curve = "ease-out-cubic" },
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
