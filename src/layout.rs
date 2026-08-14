//! Layout engine for tiling windows.
//!
//! Computes target geometries for windows (built-in layouts or a user-defined
//! Lua function), animates layout adjustments (position + size) with
//! configurable easing/spring curves, and renders a size transition snapshot
//! while a window is being resized by the layout.

use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::utils::DamageSet;
use smithay::desktop::{Space, WindowSurface, layer_map_for_output};
use smithay::output::Output;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::animation::Animation;
use crate::config::{Config, LayoutConfig, LayoutType, LuaConfig};
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::shell::{UdevMultiRenderer, WindowElement};
use crate::state::{AnvilState, Backend};

/// Persistent state of the layout engine.
#[derive(Debug, Default)]
pub struct Layout {
    next_layout_id: u64,
}

impl Layout {
    fn assign_layout_id(&mut self, state: &mut LayoutWindowState) {
        if state.layout_id == 0 {
            state.layout_id = self.next_layout_id;
            self.next_layout_id += 1;
        }
    }

    /// Compute the target geometry for every tiled window on every output.
    pub fn compute_plans(
        space: &Space<WindowElement>,
        config: &LayoutConfig,
        lua: Option<&LuaConfig>,
    ) -> Vec<Plan> {
        let mut plans = Vec::new();
        if config.layout == LayoutType::Floating {
            return plans;
        }

        for output in space.outputs() {
            let Some(area) = work_area(space, output, config) else {
                continue;
            };

            // Topmost first, matching the render order.
            let mut windows: Vec<WindowElement> =
                space.elements_for_output(output).rev().cloned().collect();
            windows.retain(|w| {
                let s = w.decoration_state();
                !(s.layout.is_floating || s.needs_center || s.hidden)
            });
            if windows.is_empty() {
                continue;
            }
            let n = windows.len();

            // Windowed fullscreen: the flagged window fills the whole work area,
            // covering the rest of the layout (others stay where they are).
            if let Some(fw) = windows.iter().find(|w| w.decoration_state().layout.full_width) {
                plans.push(Plan {
                    window: fw.clone(),
                    target: area,
                });
                continue;
            }

            // Per-window column width overrides (None = layout default).
            let widths: Vec<Option<f64>> = windows
                .iter()
                .map(|w| w.decoration_state().layout.width_override.filter(|v| *v > 0.0))
                .collect();

            if config.layout == LayoutType::Custom {
                let custom_rects = match (config.custom_fn, lua) {
                    (Some(idx), Some(lua)) => {
                        let windows_data: Vec<(u64, f64, f64, f64)> = windows
                            .iter()
                            .map(|w| {
                                let s = w.decoration_state();
                                let geo = space.element_geometry(w).unwrap_or_default().to_f64();
                                (
                                    s.layout.layout_id,
                                    geo.size.w,
                                    geo.size.h,
                                    s.layout.width_override.unwrap_or(0.0),
                                )
                            })
                            .collect();
                        let area_t = (area.loc.x, area.loc.y, area.size.w, area.size.h);
                        match lua.invoke_layout(idx, &windows_data, area_t, config.gap) {
                            Ok(geos) => Some(geos),
                            Err(e) => {
                                tracing::warn!("custom layout function failed: {e}");
                                None
                            }
                        }
                    }
                    _ => None,
                };

                match custom_rects {
                    Some(geos) => {
                        for (window, geo) in windows.into_iter().zip(geos) {
                            if let Some((x, y, w, h)) = geo {
                                plans.push(Plan {
                                    window,
                                    target: Rectangle::new(
                                        Point::from((x, y)),
                                        Size::from((w.max(0.), h.max(0.))),
                                    ),
                                });
                            }
                        }
                    }
                    None => {
                        for (window, rect) in
                            windows.into_iter().zip(columns_layout(area, &widths, config.gap))
                        {
                            plans.push(Plan {
                                window,
                                target: rect,
                            });
                        }
                    }
                }
                continue;
            }

            let rects = match config.layout {
                LayoutType::Columns => columns_layout(area, &widths, config.gap),
                LayoutType::Grid => grid_layout(area, n, config.gap),
                LayoutType::MasterStack => {
                    master_stack_layout(area, n, config.gap, config.master_ratio)
                }
                LayoutType::Maximize => maximize_layout(area, n, config.gap),
                LayoutType::Floating | LayoutType::Custom => unreachable!(),
            };

            for (window, rect) in windows.into_iter().zip(rects) {
                plans.push(Plan {
                    window,
                    target: rect,
                });
            }
        }

        plans
    }

    /// Advance layout animations. Called once per frame.
    ///
    /// Returns `true` if any window moved this frame (a redraw is needed).
    pub fn update(&mut self, space: &mut Space<WindowElement>) -> bool {
        let mut changed = false;
        let mut relocates: Vec<(WindowElement, Point<f64, Logical>)> = Vec::new();

        for window in space.elements() {
            let mut state = window.decoration_state();

            // Finalize completed workspace-switch fade animations.
            if state.fade_anim.as_ref().is_some_and(|f| f.is_done()) {
                state.fade_anim = None;
                changed = true;
            }

            let lws = &mut state.layout;

            if let Some((start, anim)) = lws.move_anim.clone() {
                if anim.is_done() {
                    if let Some(target) = lws.target {
                        relocates.push((window.clone(), target.loc));
                    }
                    lws.move_anim = None;
                    changed = true;
                } else {
                    let value = anim.clamped_value();
                    let dest = lws.target.map(|t| t.loc).unwrap_or(start);
                    let pos = Point::from((
                        start.x + (dest.x - start.x) * value,
                        start.y + (dest.y - start.y) * value,
                    ));
                    relocates.push((window.clone(), pos));
                }
            }
        }

        for (window, pos) in relocates {
            space.relocate_element(&window, pos.to_i32_round());
        }

        changed
    }
}

/// Target geometry for a single window.
#[derive(Debug, Clone)]
pub struct Plan {
    pub window: WindowElement,
    pub target: Rectangle<f64, Logical>,
}

/// Whether two logical sizes differ by more than half a pixel.
///
/// Layout targets can be fractional (e.g. `(1920 - gap) / n`), while clients
/// commit integer sizes. Comparing exactly would never settle, re-capturing a
/// resize snapshot on every commit and leaving a permanent "ghost" window.
fn sizes_differ(a: Size<f64, Logical>, b: Size<f64, Logical>) -> bool {
    (a.w - b.w).abs() > 0.5 || (a.h - b.h).abs() > 0.5
}

/// Per-window state used by the layout engine.
#[derive(Debug, Clone)]
pub struct LayoutWindowState {
    /// Stable id assigned to this window (used by Lua custom layouts).
    pub layout_id: u64,
    /// Whether this window is excluded from the layout (free-floating).
    pub is_floating: bool,
    /// Whether the window has been placed by the layout at least once.
    /// New windows are placed instantly; existing windows animate.
    pub initialized: bool,
    /// Current target geometry.
    pub target: Option<Rectangle<f64, Logical>>,
    /// Active position animation: `(start_pos, anim)`. The current position is
    /// `start_pos` interpolated to `target.loc` by the animation value.
    pub move_anim: Option<(Point<f64, Logical>, Animation)>,
    /// Active size transition snapshot.
    pub resize_snapshot: Option<ResizeSnapshot>,
    /// Per-window column width override (logical px). `None` = layout default.
    /// Honor is up to the layout: built-in `columns` and Lua custom layouts.
    pub width_override: Option<f64>,
    /// Windowed fullscreen: the window is tiled to the full work area,
    /// covering the rest of the layout (other windows keep their position).
    pub full_width: bool,
}

impl Default for LayoutWindowState {
    fn default() -> Self {
        Self {
            layout_id: 0,
            is_floating: false,
            initialized: false,
            target: None,
            move_anim: None,
            resize_snapshot: None,
            width_override: None,
            full_width: false,
        }
    }
}

/// A snapshot of a window captured when a layout-induced resize begins.
///
/// It is rendered on top of the window, stretched from the old size to the
/// target size, to smooth the transition until the animation completes.
#[derive(Debug, Clone)]
pub struct ResizeSnapshot {
    /// Captured texture of the window (at its old size).
    pub buffer: TextureBuffer<GlesTexture>,
    /// Position of the snapshot in output-relative logical coordinates.
    pub pos: Point<f64, Logical>,
    /// Window size at capture time (logical).
    pub geo_size: Size<f64, Logical>,
    /// Texture offset.
    pub buffer_offset: Point<f64, Logical>,
    /// Animation from 0 to 1.
    pub anim: Animation,
    /// Target size to animate to (logical).
    pub target_size: Size<f64, Logical>,
}

impl ResizeSnapshot {
    /// Build the render element for the current animation progress.
    pub fn render(&self, _scale: Scale<f64>) -> Option<ResizeSnapshotRenderElement> {
        if self.anim.is_done() {
            return None;
        }
        let progress = self.anim.clamped_value().clamp(0., 1.);
        let size = self.geo_size + (self.target_size - self.geo_size) * progress;
        // The texture is captured from the window *and* its decorations (border,
        // shadow), which surround the content. Sample only the window content
        // (offset by `buffer_offset`) so the transition stays aligned with the
        // window, instead of drawing a scaled-down "ghost" of the whole
        // decorated capture inside it.
        let src = Rectangle::new(self.buffer_offset, self.geo_size);
        let elem = TextureRenderElement::from_texture_buffer(
            self.buffer.clone(),
            self.pos,
            1.0,
            Some(src),
            Some(size),
            Kind::Unspecified,
        );
        Some(ResizeSnapshotRenderElement(elem))
    }
}

/// Render element that draws a [`ResizeSnapshot`].
#[derive(Debug)]
pub struct ResizeSnapshotRenderElement(TextureRenderElement<GlesTexture>);

impl Element for ResizeSnapshotRenderElement {
    fn id(&self) -> &Id {
        self.0.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.0.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.0.geometry(scale)
    }

    fn transform(&self) -> Transform {
        self.0.transform()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.0.src()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.0.damage_since(scale, commit)
    }

    fn opaque_regions(
        &self,
        scale: Scale<f64>,
    ) -> smithay::backend::renderer::utils::OpaqueRegions<i32, Physical> {
        self.0.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.0.alpha()
    }

    fn kind(&self) -> Kind {
        self.0.kind()
    }
}

impl RenderElement<GlesRenderer> for ResizeSnapshotRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        RenderElement::<GlesRenderer>::draw(&self.0, frame, src, dst, damage, opaque_regions, cache)
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        self.0.underlying_storage(renderer)
    }
}

#[cfg(feature = "udev")]
impl<'a, 'b> RenderElement<UdevMultiRenderer<'a, 'b>> for ResizeSnapshotRenderElement {
    fn draw(
        &self,
        frame: &mut <UdevMultiRenderer<'a, 'b> as smithay::backend::renderer::RendererSuper>::Frame<
            '_,
            '_,
        >,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), <UdevMultiRenderer<'a, 'b> as smithay::backend::renderer::RendererSuper>::Error>
    {
        RenderElement::<GlesRenderer>::draw(
            &self.0,
            frame.as_mut(),
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )
        .map_err(Into::into)
    }

    fn underlying_storage(
        &self,
        renderer: &mut UdevMultiRenderer<'a, 'b>,
    ) -> Option<UnderlyingStorage<'_>> {
        self.0.underlying_storage(renderer.as_mut())
    }
}

fn work_area(
    space: &Space<WindowElement>,
    output: &Output,
    config: &LayoutConfig,
) -> Option<Rectangle<f64, Logical>> {
    let geo = space.output_geometry(output)?;
    let m = config.margins;

    // Avoid shell layer surfaces (bars/panels with an exclusive zone): tiled
    // windows never overlap the shell, like niri/hyprland.
    let zone = layer_map_for_output(output).non_exclusive_zone();
    let zone_global = Rectangle::<f64, Logical>::new(
        Point::from((
            geo.loc.x as f64 + zone.loc.x as f64,
            geo.loc.y as f64 + zone.loc.y as f64,
        )),
        Size::from((zone.size.w as f64, zone.size.h as f64)),
    );

    // Apply the layout margins on top of the shell zone, so tiled windows keep
    // the configured gap from every screen edge *and* from bars/panels.
    Some(Rectangle::new(
        Point::from((zone_global.loc.x + m.left, zone_global.loc.y + m.top)),
        Size::from((
            (zone_global.size.w - m.left - m.right).max(0.),
            (zone_global.size.h - m.top - m.bottom).max(0.),
        )),
    ))
}

/// One vertical column per window, filling the work area. Windows with a
/// `width_override` get their fixed width; the remaining space is shared
/// equally among the others.
fn columns_layout(
    area: Rectangle<f64, Logical>,
    widths: &[Option<f64>],
    gap: f64,
) -> Vec<Rectangle<f64, Logical>> {
    let n = widths.len();
    if n == 0 {
        return Vec::new();
    }
    let fixed: f64 = widths.iter().filter_map(|w| *w).sum();
    let flex_count = widths.iter().filter(|w| w.is_none()).count();
    let flex_w = if flex_count > 0 {
        ((area.size.w - fixed - gap * (n as f64 - 1.)) / flex_count as f64).max(0.)
    } else {
        0.
    };
    let mut rects = Vec::with_capacity(n);
    let mut x = area.loc.x;
    for w in widths {
        let col_w = w.unwrap_or(flex_w);
        rects.push(Rectangle::new(
            Point::from((x, area.loc.y)),
            Size::from((col_w, area.size.h)),
        ));
        x += col_w + gap;
    }
    rects
}

/// Uniform grid, filling rows left-to-right, top-to-bottom.
fn grid_layout(area: Rectangle<f64, Logical>, n: usize, gap: f64) -> Vec<Rectangle<f64, Logical>> {
    if n == 0 {
        return Vec::new();
    }
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = (n + cols - 1) / cols;
    let cell_w = ((area.size.w - gap * (cols as f64 - 1.)) / cols as f64).max(0.);
    let cell_h = ((area.size.h - gap * (rows as f64 - 1.)) / rows as f64).max(0.);
    (0..n)
        .map(|i| {
            let r = i / cols;
            let c = i % cols;
            let x = area.loc.x + c as f64 * (cell_w + gap);
            let y = area.loc.y + r as f64 * (cell_h + gap);
            Rectangle::new(Point::from((x, y)), Size::from((cell_w, cell_h)))
        })
        .collect()
}

/// One master window on the left, the rest stacked on the right.
fn master_stack_layout(
    area: Rectangle<f64, Logical>,
    n: usize,
    gap: f64,
    master_ratio: f64,
) -> Vec<Rectangle<f64, Logical>> {
    if n == 0 {
        return Vec::new();
    }
    let mut rects = Vec::with_capacity(n);
    let master_w = (area.size.w * master_ratio).max(0.);
    rects.push(Rectangle::new(
        area.loc,
        Size::from((master_w, area.size.h)),
    ));

    let stack_n = n - 1;
    if stack_n > 0 {
        let stack_x = area.loc.x + master_w + gap;
        let stack_w = (area.size.w - master_w - gap).max(0.);
        let stack_h = ((area.size.h - gap * (stack_n as f64 - 1.)) / stack_n as f64).max(0.);
        for i in 0..stack_n {
            let y = area.loc.y + i as f64 * (stack_h + gap);
            rects.push(Rectangle::new(
                Point::from((stack_x, y)),
                Size::from((stack_w, stack_h)),
            ));
        }
    }
    rects
}

/// Every window maximized to the work area.
fn maximize_layout(
    area: Rectangle<f64, Logical>,
    n: usize,
    _gap: f64,
) -> Vec<Rectangle<f64, Logical>> {
    (0..n).map(|_| area).collect()
}

/// Apply a single plan to one window: start/advance position and size animations,
/// send a configure when the target size differs, and store the target.
fn apply_plan(
    space: &mut Space<WindowElement>,
    layout: &mut Layout,
    config: &Config,
    plan: Plan,
) {
    let anim_config = &config.layout.animation;
    let animations_enabled = config.animations.enable;
    let move_anim_of = || -> Animation {
        if animations_enabled {
            anim_config.move_anim.to_animation(0.0, 1.0)
        } else {
            Animation::new_off()
        }
    };

    let mut deco = plan.window.decoration_state();
    layout.assign_layout_id(&mut deco.layout);
    let lws = &mut deco.layout;

    let current = space
        .element_geometry(&plan.window)
        .map(|g| g.to_f64())
        .unwrap_or_else(|| Rectangle::new(plan.target.loc, Size::from((0., 0.))));
    let current_loc = current.loc;
    let target_loc = plan.target.loc;
    let target_size = plan.target.size;

    // --- Position animation ---
    let same_target = lws.target.map(|t| t.loc == target_loc).unwrap_or(false);
    match lws.move_anim.clone() {
        Some((_start, anim)) => {
            if !anim.is_done() && !same_target {
                // Target changed: restart from the current animated position.
                lws.move_anim = Some((current_loc, move_anim_of()));
            }
            // If the animation is done or still converging to the same target,
            // keep it; update() finishes converged animations.
        }
        None => {
            if !lws.initialized {
                // New window: place directly, no animation.
                space.relocate_element(&plan.window, target_loc.to_i32_round());
            } else if current_loc != target_loc {
                let anim = move_anim_of();
                if anim.is_done() {
                    space.relocate_element(&plan.window, target_loc.to_i32_round());
                } else {
                    lws.move_anim = Some((current_loc, anim));
                }
            }
        }
    }

    // --- Size ---
    // The window is asked to resize directly (no snapshot overlay): a texture
    // "ghost" of the old content rendered on top of the resizing window was
    // visible and confusing, so the resize-snapshot transition is disabled.
    if sizes_differ(current.size, target_size) {
        configure_window_size(&plan.window, plan.target.size);
    }

    lws.target = Some(plan.target);
    lws.initialized = true;
}

/// Ask the client to resize its surface to the given size.
fn configure_window_size(window: &WindowElement, size: Size<f64, Logical>) {
    let size = Size::from((size.w.max(0.) as i32, size.h.max(0.) as i32));
    match &window.0.underlying_surface() {
        WindowSurface::Wayland(xdg) => {
            xdg.with_pending_state(|state| {
                state.size = Some(size);
            });
            xdg.send_pending_configure();
        }
        #[cfg(feature = "xwayland")]
        WindowSurface::X11(x11) => {
            let loc = window.0.geometry().loc;
            let _ = x11.configure(Some(Rectangle::new(loc, size)));
        }
    }
}

impl<B: Backend> AnvilState<B> {
    /// Recompute the layout and animate all windows to their new targets.
    pub fn arrange_layout(&mut self) {
        let config = self.config.layout;
        if config.layout == LayoutType::Floating {
            return;
        }
        let lua = self.lua_config.as_ref().map(|l| &**l);
        let plans = Layout::compute_plans(&self.space, &config, lua);
        if plans.is_empty() {
            return;
        }

        let space = &mut self.space;
        let layout = &mut self.layout;
        let cfg = &self.config;
        for plan in plans {
            apply_plan(space, layout, cfg, plan);
        }
    }

    /// Reorder `window` within the active layout so it takes the slot under its
    /// current position (the column whose center is nearest to the window's
    /// center, left-to-right).
    ///
    /// The built-in layouts assign slots by z-order (topmost window = first
    /// slot), so reordering is done by rebuilding the z-order of the visible
    /// tiled windows on the window's output. After the reorder the layout is
    /// re-run, animating every window to its new position.
    pub fn reorder_layout_window(&mut self, window: &WindowElement) {
        // Re-attach the window to the layout first (it was temporarily marked
        // floating while being dragged) so it is included in the reorder.
        {
            let mut st = window.decoration_state();
            let lws = &mut st.layout;
            lws.is_floating = false;
            lws.move_anim = None;
            lws.target = None;
        }

        let Some(geo) = self.space.element_geometry(window) else {
            return;
        };
        let dragged_center_x = geo.loc.x as f64 + geo.size.w as f64 / 2.0;

        let Some(output) = self
            .space
            .outputs_for_element(window)
            .first()
            .cloned()
            .or_else(|| self.space.outputs().next().cloned())
        else {
            return;
        };

        // Other tiled, visible windows in slot order (topmost first).
        let others: Vec<WindowElement> = self
            .space
            .elements_for_output(&output)
            .rev()
            .filter(|w| {
                let s = w.decoration_state();
                w != &window && !s.hidden && !s.layout.is_floating
            })
            .cloned()
            .collect();

        // Insertion index (left-to-right): after every column whose center the
        // dragged window's center is to the right of.
        let mut target = others.len();
        for (i, w) in others.iter().enumerate() {
            let Some(g) = self.space.element_geometry(w) else {
                continue;
            };
            let cx = g.loc.x as f64 + g.size.w as f64 / 2.0;
            if dragged_center_x > cx {
                target = i + 1;
            }
        }

        // Rebuild the z-order. `elements_for_output` yields bottom-to-top, and
        // the layout reads it topmost-first, so `target` (top-first index)
        // maps to bottom position `ordered.len() - target`.
        let mut ordered: Vec<WindowElement> = self
            .space
            .elements_for_output(&output)
            .filter(|w| {
                let s = w.decoration_state();
                !s.hidden && !s.layout.is_floating
            })
            .cloned()
            .collect();
        ordered.retain(|w| w != window);
        let idx_from_bottom = ordered.len().saturating_sub(target.min(ordered.len()));
        ordered.insert(idx_from_bottom, window.clone());

        // Raising in bottom-to-top order gives exactly that stacking order.
        for w in ordered.iter() {
            self.space.raise_element(w, false);
        }

        self.arrange_layout();
    }

    /// Toggle whether a window is excluded from the layout (free-floating).
    pub fn toggle_window_floating(&mut self, window: &WindowElement) {
        let mut deco = window.decoration_state();
        let lws = &mut deco.layout;
        lws.is_floating = !lws.is_floating;
        lws.move_anim = None;
        lws.resize_snapshot = None;
        lws.target = None;
        drop(deco);

        self.arrange_layout();
    }
}
