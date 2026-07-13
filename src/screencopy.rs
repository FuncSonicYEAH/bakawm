use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer, Fourcc};
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1,
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm::Format;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Physical, Point, Rectangle, Size};
use smithay::wayland::{dmabuf, shm};
use tracing::trace;
use zwlr_screencopy_frame_v1::{Flags, ZwlrScreencopyFrameV1};
use zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

const VERSION: u32 = 3;

#[derive(Debug)]
pub struct ScreencopyQueue {
    pending_frames: HashSet<ZwlrScreencopyFrameV1>,
    screencopies: Vec<Screencopy>,
}

impl ScreencopyQueue {
    fn new() -> Self {
        Self {
            pending_frames: HashSet::new(),
            screencopies: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pending_frames.is_empty() && self.screencopies.is_empty()
    }

    pub fn push(&mut self, screencopy: Screencopy) {
        self.screencopies.push(screencopy);
    }

    pub fn pop(&mut self) -> Option<Screencopy> {
        if self.screencopies.is_empty() {
            None
        } else {
            Some(self.screencopies.remove(0))
        }
    }

    pub fn has_screencopies(&self) -> bool {
        !self.screencopies.is_empty()
    }

    fn remove_frame(&mut self, frame: &ZwlrScreencopyFrameV1) {
        self.pending_frames.remove(frame);
        self.screencopies.retain(|s| s.frame != *frame);
    }
}

#[derive(Default, Debug)]
pub struct ScreencopyManagerState {
    queues: HashMap<ZwlrScreencopyManagerV1, ScreencopyQueue>,
}

pub struct ScreencopyManagerGlobalData {
    filter: Box<dyn for<'c> Fn(&'c Client) -> bool + Send + Sync>,
}

impl ScreencopyManagerState {
    pub fn new<D, F>(display: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyManagerGlobalData>
            + Dispatch<ZwlrScreencopyManagerV1, ()>
            + Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>
            + ScreencopyHandler
            + 'static,
        F: for<'c> Fn(&'c Client) -> bool + Send + Sync + 'static,
    {
        let global_data = ScreencopyManagerGlobalData {
            filter: Box::new(filter),
        };
        display.create_global::<D, ZwlrScreencopyManagerV1, _>(VERSION, global_data);

        Self {
            queues: HashMap::new(),
        }
    }

    pub fn with_queues_mut(&mut self, mut f: impl FnMut(&mut ScreencopyQueue)) {
        for queue in self.queues.values_mut() {
            f(queue);
        }
        self.queues
            .retain(|manager, queue| manager.is_alive() || !queue.is_empty());
    }

    pub fn remove_output(&mut self, output: &Output) {
        for queue in self.queues.values_mut() {
            queue
                .screencopies
                .retain(|screencopy| screencopy.output() != output);
        }
    }
}

impl<D> GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyManagerGlobalData, D>
    for ScreencopyManagerState
where
    D: GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyManagerGlobalData>
        + Dispatch<ZwlrScreencopyManagerV1, ()>
        + Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>
        + ScreencopyHandler
        + 'static,
{
    fn bind(
        state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        manager: New<ZwlrScreencopyManagerV1>,
        _manager_state: &ScreencopyManagerGlobalData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(manager, ());
        let state = state.screencopy_state();
        let queue = ScreencopyQueue::new();
        state.queues.insert(manager, queue);
    }

    fn can_view(client: Client, global_data: &ScreencopyManagerGlobalData) -> bool {
        (global_data.filter)(&client)
    }
}

impl<D> Dispatch<ZwlrScreencopyManagerV1, (), D> for ScreencopyManagerState
where
    D: GlobalDispatch<ZwlrScreencopyManagerV1, ScreencopyManagerGlobalData>
        + Dispatch<ZwlrScreencopyManagerV1, ()>
        + Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState>
        + ScreencopyHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        let (frame, overlay_cursor, buffer_size, region_loc, output) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => {
                let Some(output) = Output::from_resource(&output) else {
                    trace!("screencopy client requested non-existent output");
                    let frame = data_init.init(frame, ScreencopyFrameState::Failed);
                    frame.failed();
                    return;
                };
                let buffer_size = output.current_mode().unwrap().size;
                let region_loc = Point::from((0, 0));
                (frame, overlay_cursor, buffer_size, region_loc, output)
            }
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                x,
                y,
                width,
                height,
                output,
            } => {
                if width <= 0 || height <= 0 {
                    trace!("screencopy client requested invalid sized region");
                    let frame = data_init.init(frame, ScreencopyFrameState::Failed);
                    frame.failed();
                    return;
                }
                let Some(output) = Output::from_resource(&output) else {
                    trace!("screencopy client requested non-existent output");
                    let frame = data_init.init(frame, ScreencopyFrameState::Failed);
                    frame.failed();
                    return;
                };
                let output_transform = output.current_transform();
                let output_physical_size =
                    output_transform.transform_size(output.current_mode().unwrap().size);
                let output_rect = Rectangle::from_size(output_physical_size);
                let output_scale = output.current_scale().fractional_scale();
                let rect = Rectangle::new(Point::from((x, y)), Size::from((width, height)));
                let physical_rect = rect.to_physical_precise_round(output_scale);
                let Some(clamped_rect) = physical_rect.intersection(output_rect) else {
                    trace!("screencopy client requested region outside of output");
                    let frame = data_init.init(frame, ScreencopyFrameState::Failed);
                    frame.failed();
                    return;
                };
                let untransformed_rect = output_transform
                    .invert()
                    .transform_rect_in(clamped_rect, &output_physical_size);
                (
                    frame,
                    overlay_cursor,
                    untransformed_rect.size,
                    clamped_rect.loc,
                    output,
                )
            }
            zwlr_screencopy_manager_v1::Request::Destroy => return,
            _ => unreachable!(),
        };

        let overlay_cursor = overlay_cursor != 0;
        let info = ScreencopyFrameInfo {
            output,
            buffer_size,
            region_loc,
            overlay_cursor,
        };

        let frame_resource = data_init.init(
            frame,
            ScreencopyFrameState::Pending {
                manager: manager.clone(),
                info: info.clone(),
                copied: Arc::new(AtomicBool::new(false)),
            },
        );

        frame_resource.buffer(
            Format::Xrgb8888,
            buffer_size.w as u32,
            buffer_size.h as u32,
            buffer_size.w as u32 * 4,
        );

        if frame_resource.version() >= 3 {
            frame_resource.linux_dmabuf(
                Fourcc::Xrgb8888 as u32,
                buffer_size.w as u32,
                buffer_size.h as u32,
            );
            frame_resource.buffer_done();
        }

        let state = state.screencopy_state();
        let queue = state.queues.get_mut(manager).unwrap();
        queue.pending_frames.insert(frame_resource);
    }

    fn destroyed(
        state: &mut D,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        manager: &ZwlrScreencopyManagerV1,
        _data: &(),
    ) {
        let state = state.screencopy_state();
        if let Some(queue) = state.queues.get_mut(manager) {
            if queue.is_empty() {
                state.queues.remove(manager);
            }
        }
    }
}

pub trait ScreencopyHandler {
    fn screencopy_state(&mut self) -> &mut ScreencopyManagerState;
    fn frame(&mut self, manager: &ZwlrScreencopyManagerV1, screencopy: Screencopy);
}

#[macro_export]
macro_rules! delegate_screencopy {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1: $crate::screencopy::ScreencopyManagerGlobalData
        ] => $crate::screencopy::ScreencopyManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1: ()
        ] => $crate::screencopy::ScreencopyManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1: $crate::screencopy::ScreencopyFrameState
        ] => $crate::screencopy::ScreencopyManagerState);
    };
}

#[derive(Clone)]
pub struct ScreencopyFrameInfo {
    output: Output,
    buffer_size: Size<i32, Physical>,
    region_loc: Point<i32, Physical>,
    overlay_cursor: bool,
}

pub enum ScreencopyFrameState {
    Failed,
    Pending {
        manager: ZwlrScreencopyManagerV1,
        info: ScreencopyFrameInfo,
        copied: Arc<AtomicBool>,
    },
}

impl<D> Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState, D> for ScreencopyManagerState
where
    D: Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameState> + ScreencopyHandler + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &ScreencopyFrameState,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        if matches!(request, zwlr_screencopy_frame_v1::Request::Destroy) {
            if let ScreencopyFrameState::Pending { manager, .. } = data {
                let state = state.screencopy_state();
                if let Some(queue) = state.queues.get_mut(manager) {
                    queue.remove_frame(frame);
                }
            }
            return;
        }

        let ScreencopyFrameState::Pending {
            manager,
            info,
            copied,
        } = data
        else {
            return;
        };

        if copied.load(Ordering::SeqCst) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "copy was already requested",
            );
            return;
        }

        let (buffer, with_damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            _ => unreachable!(),
        };

        let size = info.buffer_size;

        let buffer = if let Ok(dmabuf) = dmabuf::get_dmabuf(&buffer) {
            if dmabuf.format().code == Fourcc::Xrgb8888
                && dmabuf.width() == size.w as u32
                && dmabuf.height() == size.h as u32
            {
                ScreencopyBuffer::Dmabuf(dmabuf.clone())
            } else {
                frame.post_error(
                    zwlr_screencopy_frame_v1::Error::InvalidBuffer,
                    "invalid dmabuf parameters",
                );
                return;
            }
        } else if shm::with_buffer_contents(&buffer, |_, shm_len, buffer_data| {
            buffer_data.format == Format::Xrgb8888
                && buffer_data.width == size.w
                && buffer_data.height == size.h
                && buffer_data.stride == size.w * 4
                && shm_len == buffer_data.stride as usize * buffer_data.height as usize
        })
        .unwrap_or(false)
        {
            ScreencopyBuffer::Shm(buffer)
        } else {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::InvalidBuffer,
                "invalid buffer",
            );
            return;
        };

        copied.store(true, Ordering::SeqCst);

        let screencopy = Screencopy {
            info: info.clone(),
            frame: frame.clone(),
            buffer,
            with_damage,
            submitted: false,
        };

        state.frame(manager, screencopy);

        let state = state.screencopy_state();
        let queue = state.queues.get_mut(manager).unwrap();
        queue.pending_frames.remove(frame);
        if queue.is_empty() && !manager.is_alive() {
            state.queues.remove(manager);
        }
    }

    fn destroyed(
        state: &mut D,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        frame: &ZwlrScreencopyFrameV1,
        data: &ScreencopyFrameState,
    ) {
        let ScreencopyFrameState::Pending { manager, .. } = data else {
            return;
        };

        let state = state.screencopy_state();
        let Some(queue) = state.queues.get_mut(manager) else {
            return;
        };

        queue.remove_frame(frame);

        if queue.is_empty() && !manager.is_alive() {
            state.queues.remove(manager);
        }
    }
}

#[derive(Clone)]
pub enum ScreencopyBuffer {
    Dmabuf(Dmabuf),
    Shm(WlBuffer),
}

pub struct Screencopy {
    info: ScreencopyFrameInfo,
    frame: ZwlrScreencopyFrameV1,
    buffer: ScreencopyBuffer,
    with_damage: bool,
    submitted: bool,
}

impl std::fmt::Debug for Screencopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screencopy")
            .field("with_damage", &self.with_damage)
            .field("submitted", &self.submitted)
            .finish()
    }
}

impl Drop for Screencopy {
    fn drop(&mut self) {
        if !self.submitted {
            self.frame.failed();
        }
    }
}

impl Screencopy {
    pub fn buffer(&self) -> &ScreencopyBuffer {
        &self.buffer
    }

    pub fn region_loc(&self) -> Point<i32, Physical> {
        self.info.region_loc
    }

    pub fn buffer_size(&self) -> Size<i32, Physical> {
        self.info.buffer_size
    }

    pub fn output(&self) -> &Output {
        &self.info.output
    }

    pub fn overlay_cursor(&self) -> bool {
        self.info.overlay_cursor
    }

    pub fn with_damage(&self) -> bool {
        self.with_damage
    }

    pub fn damage(
        &self,
        damages: impl Iterator<Item = Rectangle<i32, smithay::utils::Buffer>>,
    ) {
        for Rectangle { loc, size } in damages {
            self.frame
                .damage(loc.x as u32, loc.y as u32, size.w as u32, size.h as u32);
        }
    }

    pub fn submit(mut self, y_invert: bool, timestamp: Duration) {
        self.frame.flags(if y_invert {
            Flags::YInvert
        } else {
            Flags::empty()
        });

        let tv_sec_hi = (timestamp.as_secs() >> 32) as u32;
        let tv_sec_lo = (timestamp.as_secs() & 0xFFFFFFFF) as u32;
        let tv_nsec = timestamp.subsec_nanos();
        self.frame.ready(tv_sec_hi, tv_sec_lo, tv_nsec);

        self.submitted = true;
    }
}