use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::fmt;

use anyhow::Context as _;
use calloop::LoopHandle;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::GbmDevice;
use smithay::backend::drm::DrmDeviceFd;
use smithay::output::Output;
use smithay::utils::{Physical, Size};
use tracing::warn;

use crate::bakawm_render_elements;
use crate::dbus::mutter_screen_cast::{self, CursorMode, ScreenCastToState, StreamTargetId};
use crate::drawing::{PointerElement, PointerRenderElement};
use crate::render::OutputRenderElementsWithBlur;
use crate::shell::WindowRenderElement;
use crate::state::AnvilState;
use crate::state::Backend;
use crate::udev::UdevData;

mod pw_utils;
use pw_utils::{Cast, CastSizeChange, CursorData, PipeWire, PwToState};

pub type IpcOutputMap = std::collections::HashMap<u64, IpcOutput>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IpcOutput {
    pub name: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub logical: Option<IpcOutputLogical>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IpcOutputLogical {
    pub x: i32,
    pub y: i32,
    pub width: i64,
    pub height: i64,
    pub scale: f64,
    pub transform: u32,
    pub current_mode_id: u64,
    pub modes: Vec<IpcOutputMode>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IpcOutputMode {
    pub id: u64,
    pub width: i64,
    pub height: i64,
    pub refresh_rate: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CastSessionId(u64);

impl CastSessionId {
    pub fn next() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for CastSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CastStreamId(u64);

impl CastStreamId {
    pub fn next() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for CastStreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum CastTarget {
    Nothing,
    Output {
        output: smithay::output::WeakOutput,
        name: String,
    },
    Window {
        id: u64,
    },
}

impl CastTarget {
    pub fn output(output: &Output) -> Self {
        Self::Output {
            output: output.downgrade(),
            name: output.name(),
        }
    }

    pub fn matches_output(&self, weak: &smithay::output::WeakOutput) -> bool {
        matches!(self, CastTarget::Output { output, .. } if output == weak)
    }
}

bakawm_render_elements! {
    CastRenderElement => {
        Output = OutputRenderElementsWithBlur<GlesRenderer, WindowRenderElement>,
        Window = WindowRenderElement,
        Pointer = PointerRenderElement<GlesRenderer>,
        RelocatedPointer = RelocateRenderElement<PointerRenderElement<GlesRenderer>>,
    }
}

pub struct Screencasting {
    pub casts: Vec<Cast>,
    pub pw_to_state: calloop::channel::Sender<PwToState>,
    pub pipewire: Option<PipeWire>,
    pub pending_dynamic_casts: Vec<(CastSessionId, CastStreamId, CursorMode, zbus::object_server::SignalEmitter<'static>)>,
}

impl std::fmt::Debug for Screencasting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screencasting")
            .field("casts", &self.casts.len())
            .field("pipewire", &self.pipewire.is_some())
            .field("pending_dynamic_casts", &self.pending_dynamic_casts.len())
            .finish()
    }
}

impl Screencasting {
    pub fn new(event_loop: &LoopHandle<'static, AnvilState<UdevData>>) -> Self {
        let pw_to_state = {
            let (pw_to_state, from_pipewire) = calloop::channel::channel();
            event_loop
                .insert_source(from_pipewire, move |event, _, state| match event {
                    calloop::channel::Event::Msg(msg) => state.on_pw_msg(msg),
                    calloop::channel::Event::Closed => (),
                })
                .unwrap();
            pw_to_state
        };

        Self {
            casts: vec![],
            pw_to_state,
            pipewire: None,
            pending_dynamic_casts: vec![],
        }
    }

    pub fn new_stub() -> Self {
        let (pw_to_state, _) = calloop::channel::channel();
        Self {
            casts: vec![],
            pw_to_state,
            pipewire: None,
            pending_dynamic_casts: vec![],
        }
    }
}

pub fn render_for_screen_cast_inner(
    screencasting: &mut Screencasting,
    space: &smithay::desktop::Space<crate::shell::WindowElement>,
    pointer_location: smithay::utils::Point<f64, smithay::utils::Logical>,
    show_window_preview: bool,
    renderer: &mut GlesRenderer,
    pointer_element: &PointerElement,
    output: &Output,
    target_presentation_time: Duration,
    blur_config: &crate::config::Config,
) -> Vec<CastSessionId> {
    if screencasting.casts.is_empty() {
        return Vec::new();
    }

    let weak = output.downgrade();
    let mode = output.current_mode().unwrap();
    let transform = output.current_transform();
    let size = transform.transform_size(mode.size);

    let scale = smithay::utils::Scale::from(output.current_scale().fractional_scale());

    let mut casts_to_stop = vec![];

    let mut casts = std::mem::take(&mut screencasting.casts);
    for cast in &mut casts {
        if !cast.is_active() {
            continue;
        }

        match &cast.target {
            CastTarget::Output { output: cast_output, name } => {
                if cast_output != &weak {
                    tracing::debug!(
                        cast_output = ?cast_output.upgrade().map(|o| o.name()),
                        current_output = %output.name(),
                        "cast output mismatch, skipping"
                    );
                    continue;
                }
                tracing::trace!(%name, ?size, "rendering screencast for output");
            }
            CastTarget::Window { .. } => {}
            CastTarget::Nothing => continue,
        }

        match cast.ensure_size(size) {
            Ok(CastSizeChange::Ready) => (),
            Ok(CastSizeChange::Pending) => {
                tracing::debug!("cast size change pending, skipping frame");
                continue;
            }
            Err(err) => {
                warn!("error updating stream size, stopping screencast: {err:?}");
                casts_to_stop.push(cast.session_id);
                continue;
            }
        }

        if cast.check_time_and_schedule(output, target_presentation_time) {
            tracing::debug!("cast rate limited, skipping frame");
            continue;
        }

        let custom_elements: Vec<crate::render::CustomRenderElements<smithay::backend::renderer::gles::GlesRenderer>> = vec![];
        let (elements, _clear_color) = crate::render::output_elements(
            output,
            space,
            &[],
            custom_elements,
            renderer,
            show_window_preview,
            blur_config,
        );

        let mut cast_elements: Vec<CastRenderElement> = vec![];

        let cursor_elem_count = if cast.cursor_mode == CursorMode::Embedded {
            let output_geo = space.output_geometry(output).unwrap();
            let pos = (pointer_location - output_geo.loc.to_f64()).to_physical_precise_round(scale).upscale(-1);
            crate::drawing::draw_pointer(
                renderer,
                pointer_element,
                pointer_location,
                output_geo,
                output,
                &mut |elem| {
                    let elem = RelocateRenderElement::from_element(elem, pos, Relocate::Relative);
                    cast_elements.push(CastRenderElement::from(elem));
                },
            );
            cast_elements.len()
        } else {
            0
        };

        for elem in elements {
            cast_elements.push(CastRenderElement::from(elem));
        }

        let cursor_data = CursorData::compute(
            &cast_elements,
            cursor_elem_count,
            pointer_location,
            scale,
        );

        let rendered = cast.dequeue_buffer_and_render(renderer, &cast_elements, &cursor_data, size, scale);

        if rendered {
            tracing::trace!("screencast frame rendered successfully");
            cast.last_frame_time = get_monotonic_time();
        } else {
            tracing::trace!("screencast frame skipped (no damage or no buffer)");
        }

        cast.check_time_and_schedule(output, target_presentation_time);
    }
    screencasting.casts = casts;

    casts_to_stop
}

pub fn render_windows_for_screen_cast_inner(
    screencasting: &mut Screencasting,
    space: &smithay::desktop::Space<crate::shell::WindowElement>,
    pointer_location: smithay::utils::Point<f64, smithay::utils::Logical>,
    _show_window_preview: bool,
    renderer: &mut GlesRenderer,
    pointer_element: &PointerElement,
    output: &Output,
    target_presentation_time: Duration,
) -> Vec<CastSessionId> {
    if screencasting.casts.is_empty() {
        return Vec::new();
    }

    let scale = smithay::utils::Scale::from(output.current_scale().fractional_scale());

    let mut casts_to_stop = vec![];

    let mut casts = std::mem::take(&mut screencasting.casts);
    for cast in &mut casts {
        if !cast.is_active() {
            continue;
        }

        let window_id = match &cast.target {
            CastTarget::Window { id } => *id,
            _ => continue,
        };

        let window = space
            .elements()
            .enumerate()
            .find(|(idx, _)| (*idx as u64) + 1 == window_id)
            .map(|(_, w)| w);

        let Some(window) = window else {
            if cast.dequeue_buffer_and_clear(renderer) {
                cast.last_frame_time = get_monotonic_time();
            }
            continue;
        };

        let output_geo = space.output_geometry(output).unwrap();
        let window_geo = space.element_geometry(window).unwrap();
        let window_output = space.outputs_for_element(window).first().cloned();

        let Some(window_output) = window_output else {
            continue;
        };

        let bbox = window_geo.size.to_physical_precise_round(scale);

        match cast.ensure_size(bbox) {
            Ok(CastSizeChange::Ready) => (),
            Ok(CastSizeChange::Pending) => {
                continue;
            }
            Err(err) => {
                warn!("error updating stream size, stopping screencast: {err:?}");
                casts_to_stop.push(cast.session_id);
                continue;
            }
        }

        if cast.check_time_and_schedule(&window_output, target_presentation_time) {
            continue;
        }

        let mut elements: Vec<CastRenderElement> = vec![];

        let cursor_elem_count = if cast.cursor_mode == CursorMode::Embedded {
            let rel_pos = (pointer_location - output_geo.loc.to_f64()) - window_geo.loc.to_f64();
            let pos = rel_pos.to_physical_precise_round(scale).upscale(-1);
            crate::drawing::draw_pointer(
                renderer,
                pointer_element,
                pointer_location,
                output_geo,
                output,
                &mut |elem| {
                    let elem = RelocateRenderElement::from_element(elem, pos, Relocate::Relative);
                    elements.push(CastRenderElement::from(elem));
                },
            );
            elements.len()
        } else {
            0
        };

        let window_elements: Vec<WindowRenderElement> =
            smithay::backend::renderer::element::AsRenderElements::<GlesRenderer>::render_elements(
                window,
                renderer,
                window_geo.loc.to_physical_precise_round(scale),
                scale,
                1.0,
            );

        for elem in window_elements {
            elements.push(CastRenderElement::from(elem));
        }

        let cursor_data = CursorData::compute(
            &elements,
            cursor_elem_count,
            pointer_location,
            scale,
        );

        let rendered = cast.dequeue_buffer_and_render(renderer, &elements, &cursor_data, bbox, scale);

        if rendered {
            cast.last_frame_time = get_monotonic_time();
        }

        cast.check_time_and_schedule(&window_output, target_presentation_time);
    }
    screencasting.casts = casts;

    casts_to_stop
}

impl AnvilState<UdevData> {
    fn prepare_pw_cast(&mut self) -> anyhow::Result<(GbmDevice<DrmDeviceFd>, FormatSet)> {
        let gbm = self
            .backend_data
            .primary_gbm_device()
            .context("no GBM device available")?;

        if self.screencasting.pipewire.is_none() {
            tracing::info!("initializing PipeWire for screencast");
            let pw = PipeWire::new(
                self.handle.clone(),
                self.screencasting.pw_to_state.clone(),
            )
            .context("error initializing PipeWire")?;
            self.screencasting.pipewire = Some(pw);
            tracing::info!("PipeWire initialized successfully");
        }

        let render_formats = self
            .backend_data
            .with_primary_renderer(|renderer, _pointer_element| {
                renderer.egl_context().dmabuf_render_formats().clone()
            })
            .unwrap_or_default();

        if render_formats.iter().count() == 0 {
            anyhow::bail!("no DMA-BUF render formats available, screencast will not work");
        }

        tracing::debug!(formats = render_formats.iter().count(), "screencast render formats");

        Ok((gbm, render_formats))
    }

    pub fn on_pw_msg(&mut self, msg: PwToState) {
        match msg {
            PwToState::StopCast { session_id } => self.stop_cast(session_id),
            PwToState::Redraw { stream_id } => self.redraw_cast(stream_id),
            PwToState::FatalError => {
                warn!("stopping PipeWire due to fatal error");
                let casting = &mut self.screencasting;
                if let Some(pw) = casting.pipewire.take() {
                    let mut ids = HashSet::new();
                    for cast in &casting.casts {
                        ids.insert(cast.session_id);
                    }
                    for id in ids {
                        self.stop_cast(id);
                    }
                    self.handle.remove(pw.token);
                }
            }
        }
    }

    fn redraw_cast(&mut self, stream_id: CastStreamId) {
        let casts = &mut self.screencasting.casts;
        let Some(idx) = casts.iter().position(|cast| cast.stream_id == stream_id) else {
            warn!("cast to redraw is missing");
            return;
        };
        let cast = &casts[idx];

        match &cast.target {
            CastTarget::Output { output, .. } => {
                if let Some(output) = output.upgrade() {
                    self.queue_redraw(&output);
                }
            }
            CastTarget::Window { .. } => {
                let outputs: Vec<_> = self.space.outputs().cloned().collect();
                for output in outputs {
                    self.queue_redraw(&output);
                }
            }
            CastTarget::Nothing => {}
        }
    }

    pub fn on_screen_cast_msg(&mut self, msg: ScreenCastToState) {
        match msg {
            ScreenCastToState::StartCast {
                session_id,
                stream_id,
                target,
                cursor_mode,
                signal_ctx,
            } => {
                let _span = tracing::debug_span!("StartCast", %session_id, %stream_id).entered();

                let (target, size, refresh) = match target {
                    StreamTargetId::Output { name } => {
                        let output = self.space.outputs().find(|out| out.name() == name);
                        let Some(output) = output else {
                            warn!("error starting screencast: requested output is missing");
                            self.stop_cast(session_id);
                            return;
                        };

                        let (size, refresh) = cast_params_for_output(output);
                        (CastTarget::output(output), size, refresh)
                    }
                    StreamTargetId::Window { id } => {
                        let window = self.space.elements().enumerate().find(|(idx, _)| (*idx as u64) + 1 == id);
                        let Some((_, window)) = window else {
                            warn!("error starting screencast: requested window {id} is missing");
                            self.stop_cast(session_id);
                            return;
                        };

                        let output = self.space.outputs_for_element(window).first().cloned();
                        let Some(output) = output else {
                            warn!("error starting screencast: window {id} has no output");
                            self.stop_cast(session_id);
                            return;
                        };

                        let (size, refresh) = cast_params_for_output(&output);
                        (CastTarget::Window { id }, size, refresh)
                    }
                };

                let (gbm, render_formats) = match self.prepare_pw_cast() {
                    Ok(x) => x,
                    Err(err) => {
                        warn!("error starting screencast: {err:?}");
                        self.stop_cast(session_id);
                        return;
                    }
                };
                let pw = self.screencasting.pipewire.as_ref().unwrap();

                let alpha = false;

                let res = pw.start_cast(
                    gbm,
                    render_formats,
                    session_id,
                    stream_id,
                    target,
                    size,
                    refresh,
                    alpha,
                    cursor_mode,
                    signal_ctx,
                );
                match res {
                    Ok(cast) => {
                        tracing::info!(%stream_id, "screencast stream started successfully");
                        self.screencasting.casts.push(cast);
                    }
                    Err(err) => {
                        warn!("error starting screencast: {err:?}");
                        self.stop_cast(session_id);
                    }
                }
            }
            ScreenCastToState::StopCast { session_id } => self.stop_cast(session_id),
        }
    }

    pub fn stop_cast(&mut self, session_id: CastSessionId) {
        let _span = tracing::debug_span!("stop_cast", %session_id).entered();

        for i in (0..self.screencasting.casts.len()).rev() {
            let cast = &self.screencasting.casts[i];
            if cast.session_id != session_id {
                continue;
            }

            let cast = self.screencasting.casts.swap_remove(i);
            if let Err(err) = cast.stream.disconnect() {
                warn!("error disconnecting stream: {err:?}");
            }
        }

        self.screencasting
            .pending_dynamic_casts
            .retain(|(sid, _, _, _)| *sid != session_id);

        if let Some(dbus) = &self.dbus {
            let server = dbus.conn_screen_cast.as_ref().unwrap().object_server();
            let path = format!("/org/gnome/Mutter/ScreenCast/Session/u{}", session_id.get());
            if let Ok(iface) = server.interface::<_, mutter_screen_cast::Session>(path) {
                async_io::block_on(async move {
                    iface
                        .get()
                        .stop(server.inner(), iface.signal_emitter().clone())
                        .await
                });
            }
        }
    }

    pub fn stop_casts_for_output(&mut self, output: &Output) {
        let weak = output.downgrade();
        let mut ids = Vec::new();
        for cast in &self.screencasting.casts {
            if cast.target.matches_output(&weak) {
                ids.push(cast.session_id);
            }
        }
        for id in ids {
            self.stop_cast(id);
        }
    }

    pub fn render_for_screen_cast(
        &mut self,
        renderer: &mut GlesRenderer,
        pointer_element: &PointerElement,
        output: &Output,
        target_presentation_time: Duration,
    ) {
        let casts_to_stop = render_for_screen_cast_inner(
            &mut self.screencasting,
            &self.space,
            self.pointer.current_location(),
            self.show_window_preview,
            renderer,
            pointer_element,
            output,
            target_presentation_time,
            &self.config,
        );
        for id in casts_to_stop {
            self.stop_cast(id);
        }
    }

    pub fn render_windows_for_screen_cast(
        &mut self,
        renderer: &mut GlesRenderer,
        pointer_element: &PointerElement,
        output: &Output,
        target_presentation_time: Duration,
    ) {
        let casts_to_stop = render_windows_for_screen_cast_inner(
            &mut self.screencasting,
            &self.space,
            self.pointer.current_location(),
            self.show_window_preview,
            renderer,
            pointer_element,
            output,
            target_presentation_time,
        );
        for id in casts_to_stop {
            self.stop_cast(id);
        }
    }

    pub fn set_dynamic_cast_target(
        &mut self,
        _session_id: CastSessionId,
        stream_id: CastStreamId,
        target: CastTarget,
    ) {
        for cast in &mut self.screencasting.casts {
            if cast.stream_id == stream_id {
                cast.target = target;
                cast.dynamic_target = true;
                return;
            }
        }

        warn!(%stream_id, "set_dynamic_cast_target: cast not found");
    }

    pub fn start_pending_dynamic_casts(&mut self) {
        let pending = std::mem::take(&mut self.screencasting.pending_dynamic_casts);
        for (session_id, stream_id, cursor_mode, signal_ctx) in pending {
            let target = CastTarget::Nothing;
            let (gbm, render_formats) = match self.prepare_pw_cast() {
                Ok(x) => x,
                Err(err) => {
                    warn!("error starting dynamic screencast: {err:?}");
                    self.stop_cast(session_id);
                    continue;
                }
            };
            let pw = self.screencasting.pipewire.as_ref().unwrap();

            let mode = self.space.outputs().next().and_then(|o| o.current_mode());
            let (size, refresh) = match mode {
                Some(mode) => {
                    let transform = self.space.outputs().next().unwrap().current_transform();
                    (transform.transform_size(mode.size), mode.refresh as u32)
                }
                None => (Size::from((1920, 1080)), 60000),
            };

            let alpha = false;
            let res = pw.start_cast(
                gbm,
                render_formats,
                session_id,
                stream_id,
                target,
                size,
                refresh,
                alpha,
                cursor_mode,
                signal_ctx,
            );

            match res {
                Ok(cast) => {
                    tracing::info!(%stream_id, "dynamic screencast stream started");
                    self.screencasting.casts.push(cast);
                }
                Err(err) => {
                    warn!("error starting dynamic screencast: {err:?}");
                    self.stop_cast(session_id);
                }
            }
        }
    }

    fn queue_redraw(&mut self, output: &Output) {
        self.backend_data.queue_redraw(output);
    }
}

fn cast_params_for_output(output: &Output) -> (Size<i32, Physical>, u32) {
    let mode = output.current_mode().unwrap();
    let transform = output.current_transform();
    let size = transform.transform_size(mode.size);
    let refresh = mode.refresh as u32;
    (size, refresh)
}

pub fn get_monotonic_time() -> Duration {
    use smithay::reexports::rustix::time::{clock_gettime, ClockId};
    let ts = clock_gettime(ClockId::Monotonic);
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}