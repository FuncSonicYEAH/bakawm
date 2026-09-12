# bakawm 优化计划

> **状态：✅ 全部完成（2026-09-11）**
> - 显式 return：全部三个 crate root 启用 `#![warn(clippy::implicit_return)]`，~1350 处全部改写，
>   default 与 `debug,dinit` 两个 feature 组合下 clippy 均为 0 错误 0 该 lint 警告
>   （同时 allow 了直接矛盾的 `clippy::needless_return`）。
> - 稳定性：grabs.rs / shell/xdg.rs / shell/x11.rs / input_handler.rs / state.rs / udev.rs /
>   winit.rs / x11.rs 中客户端可触发的 unwrap/panic 全部改为安全降级
>   （含 udev 键位 match 的 `_ => unreachable!()` 与 Lua `Callback` 缺失分支）。
> - 流畅度：render.rs 每窗口每帧只取一次 `decoration_state()`、`open_animation` 克隆移出逐元素循环、
>   无 window_rules 时跳过 blur 规则解析；layout.rs 移除每帧 `move_anim.clone()`。
> - 可读性：state.rs `pre_repaint`/`post_repaint` 四段重复的 surface-tree 块提取为
>   `signal_commit_timers` / `with_output_surface_state` 两个辅助函数；input_handler.rs 的
>   `BindAction` match 缩进已修复。
> - 验证：`cargo check`（全 target）、`cargo clippy --all-targets`（default + debug,dinit）、
>   `cargo test`（19 passed）、`cargo build --release` 全部通过。

目标：**提高稳定性、提升流畅度、提高代码可读性**，外加把所有带返回值的函数改成显式 `return`。
原则：改动最小化、不引入行为变化（除 panic → 安全降级）、每步之后 `cargo check` 必须通过。

参考项目：`cankao/niri`（动画/输入处理风格）、`cankao/smithay`（anvil 示例的容错写法）。

---

## 0. 现状摘要（已完成的代码勘查）

- 规模：src 约 27,000 行；最大文件 `config.rs` (2801)、`state.rs` (2361)、`udev.rs` (2259)、`input_handler.rs` (1886)。
- `cargo check` 当前通过；clippy 0.1.98 可用。
- `.unwrap()` 共 **387 处**，分布：config.rs 117（多为 Lua 解析，启动期，风险低）、udev.rs 37、input_handler.rs 36、shell/xdg.rs 32、shell/x11.rs 30、state.rs 24、shell/grabs.rs 18……
- 渲染热路径 `render.rs::output_elements` 里每个窗口每帧重复调用 `decoration_state()`（RefCell 锁）3+ 次，且 `open_animation.clone()` 在**每个渲染元素**的循环里做。
- `state.rs::pre_repaint` / `post_repaint` 各有 4 段几乎相同的 surface-tree 遍历代码（space / layer / cursor / dnd icon），两函数合计约 300 行重复。
- `input_handler.rs` 末尾 `BindAction` match 缩进错乱。
- `clippy::implicit_return` 试跑：**约 700+ 处**函数末尾隐式返回（1380 条诊断含 help 行）。

---

## 1. 显式 return（独立、可先做，机械改动）

**做法**：
1. 在 `src/lib.rs` 与 `src/ctl/main.rs` 顶部加 `#![warn(clippy::implicit_return)]`。
2. `cargo clippy --fix --allow-dirty --all-features` 自动改写（含子二进制 bakawm-ctl）。
3. 对 `--fix` 处理不了的位置（宏内、闭包等）手工补 `return`。
4. **保留该 lint**（不删 crate attribute），后续新代码也会被约束。

注意：
- `--fix` 会把 `match` 尾表达式等改成 `return ...`，需确认没有把 `?` / `try` 块改坏；改完 diff 全量过一遍。
- `--all-features` 需要 screencast 的 pipewire/dbus 头文件，若环境缺库则退回 `--no-default-features --features "egl,winit,x11,udev,xwayland,libei,systemd"` 分两次 fix（默认 + debug）。
- trait impl 里的 `#[inline]` 小函数也会被改，属预期。

**风险**：低（纯语法重写），验证手段 = `cargo check` + diff 审阅。

---

## 2. 稳定性：消除客户端可触发的 panic

Wayland WM 里客户端事件（destroy / commit / unmap 时序）不应能 panic 掉整个合成器。按文件处理，原则：
- **数据结构映射类** unwrap（`from_bits`、`try_from`）→ `unwrap_or` / `map_err` 或保留 `expect` + 注释（不可达时）。
- **查询类** unwrap（`window_for_surface().unwrap()`、`element_location().unwrap()`、`grab_start_data().unwrap()`、`output_geometry().unwrap()`）→ 提前 `return`（`.ok()` / `let ... else`），niri 的惯用写法。
- **状态机类** `panic!("invalid resize state")` → `warn!` + 降级为 `NotResizing`。

### 2.1 `src/shell/grabs.rs`
- `From<xdg_toplevel::ResizeEdge> for ResizeEdge`：`Self::from_bits(x as u32).unwrap()` → `Self::from_bits(...).unwrap_or(Self::NONE)`（或 `expect`，因 xdg 协议值域是封闭的，但 NONE 降级更稳）。
- `From<ResizeEdge> for xdg_toplevel::ResizeEdge`：`try_from(...).unwrap()` → `unwrap_or(Self::from_bits_truncate)` 等安全映射。
- `PointerResizeSurfaceGrab::button` / `TouchResizeSurfaceGrab::up`：`panic!("invalid resize state: ...")`（2×2 处）→ 状态非 `Resizing` 时 `warn!` 并直接置 `NotResizing`，继续正常收尾（unset grab / configure）。
- `with_states(..., |s| ... .unwrap())` 拿 `RefCell<SurfaceData>`：改用 `get_or_insert` 风格或 `if let Some`，防 surface 已 destroy。
- `configure_with_sync(...).unwrap()`（X11 路径 4 处）→ `let _ = ...`（X11 窗口销毁竞态下失败是常态，anvil/niri 均忽略）。

### 2.2 `src/shell/xdg.rs`
- `window_for_surface(...).unwrap()`（move_grab / resize_grab / fullscreen 等 ~4 处）→ `let Some(window) = ... else { return; }`（客户端拖拽中 destroy toplevel 是常见竞态）。
- `grab_start_data().unwrap()`（pointer/touch 各 2 处）→ `else { return }`。
- `element_location(&window).unwrap()`（~6 处）→ `else { return }`。
- `Seat::from_resource(&seat).unwrap()`（3 处）→ `else { return }`（协议上不该失败，但降级无副作用）。
- `outputs_for_window.pop().unwrap()` / `output_geometry(&output).unwrap()`（fullscreen 几何合并处）→ 空列表/无几何时回退 `space.outputs().next()`，再不行 `return`。
- `space.outputs().next().unwrap().clone()`（`new_layer_surface` 默认输出）→ 无输出时 `warn!` + `return`（该请求本就无处安放）。

### 2.3 `src/shell/x11.rs`
- `self.xwm.as_mut().unwrap()`（xwm getter）→ 返回 `Option<&mut X11Wm>` 或调用处 `else { return }`。
- `element_bbox / element_location / set_mapped / configure` 系列 unwrap（~20 处）→ X11 窗口在事件间隙销毁时全部安全降级（`let Some(...) else { return }` / `let _ =`）。
- `expect("No outputs found")`（2 处）→ `else { return }`。

### 2.4 `src/input_handler.rs`
- `update_keyboard_focus`：`self.xwm.as_mut().unwrap().raise_window(surface).unwrap()`（2 处）→ `if let (Some(surface), Some(xwm)) = ...` + `let _ =`（与 `state.rs::focus_window` 已有的写法对齐）。
- `on_pointer_move_absolute` / `clamp_coords` / `process_input_event`(udev) 中 `output_geometry(o).unwrap()`（~10 处）→ `filter_map` + 空守卫；输出集合为空时直接 return（headless/输出拔除瞬间会走到）。
- `outputs().find(...).unwrap().clone()`（windowed 后端的 ScaleUp/Down/Rotate）→ `let Some(output) = ... else { return }`。
- `keyboard_key_to_action` 里 `self.seat.get_keyboard().unwrap()` → `let Some(keyboard) = ... else { return KeyAction::None }`。

### 2.5 `src/state.rs`
- `start_xwayland`：`XWayland::spawn(...).expect("failed to start XWayland")` / `X11Wm::start_wm(...).expect(...)` / `set_cursor(...).expect(...)` → 改为 `warn!` + 优雅降级（XWayland 起不来不应该杀掉整个 session；`data.xwm` 保持 `None` 即可）。
- `on_introspect_msg` / `ipc_list_windows`：`data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap()` → `.and_then(...)` 链（surface 未完全初始化时可能缺 role data）。
- `new_fractional_scale` 内部的 unwrap 已是嵌套 with_states，改为 safe 化。
- `ipc_focus_window` 等已有 Result 包装，无需动。

### 2.6 `src/udev.rs`
- `render_surface`：`space.output_geometry(output).unwrap()` → `else { return Ok((false, states-类似)) }` 或上游判空（输出刚被拔掉时 render 仍可能被调度）。
- `get_surface_dmabuf_feedback` 等处的 unwrap 逐个评估。
- `frame_finish` / `render` 已有较好的错误分支，`insert_source(...).expect(...)`（timer 注册 3 处）→ 保留（calloop 关闭后才会失败，panic 可接受）或改 log+return。
- `device_added` 等处 unwrap 只影响单设备，降级为 warn。

### 2.7 `src/winit.rs` / `src/x11.rs`（后端）
- 各 10-12 处 unwrap：多为 `output_geometry` 与 mode 查询，同样降级。

### 2.8 不动的 unwrap
- `config.rs`（117 处）：全部在 Lua/serde 解析的启动路径与锁 guard（`.lock().unwrap()` 是 mutex 中毒语义，保持原样符合生态惯例）。
- `screencasting/`、`dbus/`：pw/zbus 线程内，单独一轮评估，本次只处理明显竞态点。

---

## 3. 流畅度：渲染与输入热路径

### 3.1 `src/render.rs::output_elements`
- **每窗口只取一次 `decoration_state()`**：目前在窗口循环里取了 3 次（needs_center 检查、hidden 检查、open_animation），并在**每个元素**循环里再 `clone()` open_animation → 改为循环开头取一次 `RefMut`，把 `needs_center / hidden / open_anim 进度 / scale_factor` 先算成局部值再 `drop` 锁，元素循环只读局部值。
- **blur 规则短路**：`resolve_window_blur` 每窗口每帧 `with_states` 拿 title/app_id 并 clone String。当 `config.window_rules` 为空且 `config.blur.enable == false`（以及 xray 相关分支）时跳过；title/app_id 用 `Arc<str>` 或先查规则表是否需要（窗口多数场景无规则 → 零开销路径）。
- 同样短路 `resolve_layer_blur`。
- `has_blur_region` 的 `with_states` 查询只在 `config.blur.enable || 有 blur 规则` 时执行。

### 3.2 `src/shell/element.rs::render_elements`
- `state.open_animation` 完成时的清理依赖 `anim.is_done()` 副作用——保持行为，但把 `ClippedSurfaceRenderElement::shader(renderer).cloned()` 等 per-frame 查询维持现状（已有缓存字段 `has_border_shader`/`has_shadow_shader`/`cached_*_element`）。
- 边框/阴影插入用 `vec.insert(0, ...)` 4 次 → 预分配 `Vec::with_capacity(window_elements.len() + 6)` 并用 `splice`/倒序 push，避免 O(n²) 搬移（小 n 影响有限，属顺手优化）。

### 3.3 `src/udev.rs::render_surface`
- `pointer_image` 查找：`pointer_images.iter().find_map` 每帧线性扫 → 无需改（≤64 项，缓存命中为常态），只加注释。
- 帧调度：`has_active_animations()` 每 vblank 遍历全部窗口 → 该函数已经轻量，保持。
- `frame_finish` 里动画路径 `Timer::from_duration(1ms)` 保持（注释已说明防 GPU 管线拥堵）。

### 3.4 `src/layout.rs`
- `Layout::update` 每帧对全部窗口 `decoration_state()` + `move_anim.clone()`：把 clone 去掉（读 `anim.is_done()`/`clamped_value()` 用引用即可），仅写回时借 `&mut`。窗口多时省一半 RefCell 流量。
- `compute_plans` 已是 O(n)，保持。

### 3.5 `src/state.rs::has_active_animations`
- 在 `post_repaint`/`frame_finish` 中每帧调用，可接受；但 `switch_workspace` 等调用点可缓存结果——本次不改（收益小，避免复杂化）。

---

## 4. 可读性

### 4.1 `src/state.rs`：抽出 surface-tree 工具
- `pre_repaint` / `post_repaint` 各 4 段几乎相同的遍历 → 新增私有泛型函数（各一个）：
  - `fn for_each_repaint_surface(...)`: 统一遍历 space 窗口 + layer surfaces + cursor surface tree + dnd icon，回调拿到 `(surface, states)`；
  - pre 用它发 `commit_timer_state.signal_until`；post 用它做 fifo barrier + fractional scale + send_frame + dmabuf feedback。
- 注意保留原有语义：layer map 锁必须在对 client 调 `blocker_cleared` 之前 drop（现有注释说明了原因），抽函数时用两阶段（collect clients → drop map → clear blockers）保住这一点。
- 预计 `pre_repaint`/`post_repaint` 从 ~150 行/个 缩到 ~50 行/个，消掉 ~250 行重复。

### 4.2 `src/input_handler.rs`
- 修复文件末尾 `BindAction => KeyAction` match 的缩进错乱块（约 20 行，纯格式）。
- `process_keyboard_shortcut` 的按键名 match（F1..F12、Page_Up 等手工表）→ 用 `keysym_get_name` 归一化对比或查表宏收敛（保持行为一致：modified/raw 双匹配），~60 行 → ~15 行。
- `detect_resize_edges`、`required_mods` 已清晰，不动。

### 4.3 `src/shell/grabs.rs`
- 5 个 grab impl 的 gesture/axis/frame 转发样板完全一致 → 保持（trait 设计使然，niri 同样写法），只在本次顺带统一注释格式，不做宏抽象（可读性反而下降）。

### 4.4 杂项
- `src/state.rs::start_close_animation_inner` 等 `pub(crate)` 内部链保持；仅把 `capture_close_snapshot` 里 ~10 处 `match ... { Ok(t) => t, Err(e) => { warn; return None } }` 收敛成 `match ... else`/helper（`?` 化需改返回类型为 `Result`，不值当，用局部闭包 `try_log` 或保持 match 但收敛 log 格式）。
- `render.rs` 顶部 `render_elements!` 生成的 Debug impl 手写重复 → 保持（宏限制，注释已说明）。

---

## 5. 执行顺序与验证

| 步骤 | 内容 | 验证 |
|---|---|---|
| 1 | `#![warn(clippy::implicit_return)]` + `cargo clippy --fix`，手工收尾 | `cargo check`；diff 审阅 |
| 2 | 稳定性改造 2.1 → 2.3（shell 层，最高危） | `cargo check` + `cargo clippy -- -D warnings` |
| 3 | 稳定性改造 2.4 → 2.7（输入与后端） | 同上 |
| 4 | 流畅度 3.1 → 3.4 | 同上 + 手动 review 行为不变（动画进度计算次序） |
| 5 | 可读性 4.1（pre/post_repaint 重构） | 同上，重点核对 blocker_cleared 时序 |
| 6 | 可读性 4.2 → 4.4 | 同上 |
| 7 | 全量验证 | `cargo check --all-features`（或分特性）、`cargo clippy` 无警告、`cargo test`、`cargo build --release` |

回滚策略：每步独立、无交叉依赖；若 `--fix` diff 过大难审，可分 crate attribute 只加在单个模块逐步推进。

---

## 6. 明确不做的事

- 不改动画/布局语义（帧调度策略、`Timer::from_duration(1ms)` 等已调优过，注释里记录了教训，见 `.trae/documents/add-animation-system.md`）。
- 不引入新依赖（keyframe、tokio 等）。
- 不做 `unsafe` 清理与 `send_sync` 重构（Smithay 框架约束，超本次范围）。
- 不动 `cankao/` 参考项目。
