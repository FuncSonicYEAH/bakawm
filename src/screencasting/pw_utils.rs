use std::cell::RefCell;
use std::cmp::min;
use std::collections::HashMap;
use std::io::Cursor;
use std::iter::zip;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Duration;
use std::{mem, slice};

use anyhow::Context;
use calloop::RegistrationToken;
use calloop::timer::{TimeoutAction, Timer};
use pipewire::context::ContextRc;
use pipewire::core::{CoreRc, PW_ID_CORE};
use pipewire::main_loop::MainLoopRc;
use pipewire::properties::PropertiesBox;
use pipewire::spa::buffer::DataType;
use pipewire::spa::param::ParamType;
use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire::spa::param::format_utils::parse_format;
use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
use pipewire::spa::pod::deserialize::PodDeserializer;
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{self, ChoiceValue, Pod, PodPropFlags, Property, PropertyFlags};
use pipewire::spa::sys::*;
use pipewire::spa::utils::{
    Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Rectangle, SpaTypes,
};
use pipewire::spa::{self};
use pipewire::stream::{Stream, StreamFlags, StreamListener, StreamRc, StreamState};
use pipewire::sys::{pw_buffer, pw_check_library_version, pw_stream_queue_buffer};
use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmBuffer, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Format, Fourcc};
use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::renderer::ExportMem;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::element::{Element, RenderElement};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::sync::SyncPoint;
use smithay::output::{Output, OutputModeSource};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use smithay::reexports::gbm::Modifier;
use smithay::utils::{Logical, Physical, Point, Scale, Size, Transform};
use tracing::{debug, error, trace, warn};
use zbus::object_server::SignalEmitter;

use crate::dbus::mutter_screen_cast::{self, CursorMode};
use crate::screencasting::{CastSessionId, CastStreamId, CastTarget, get_monotonic_time};
use crate::state::AnvilState;
use crate::state::Backend;
use crate::udev::UdevData;

use super::CastRenderElement;

const CAST_DELAY_ALLOWANCE: Duration = Duration::from_micros(100);

const CURSOR_FORMAT: spa_video_format = SPA_VIDEO_FORMAT_BGRA;
const CURSOR_BPP: u32 = 4;
const CURSOR_WIDTH: u32 = 384;
const CURSOR_HEIGHT: u32 = 384;
const CURSOR_BITMAP_SIZE: usize = (CURSOR_WIDTH * CURSOR_HEIGHT * CURSOR_BPP) as usize;
const CURSOR_META_SIZE: usize =
    mem::size_of::<spa_meta_cursor>() + mem::size_of::<spa_meta_bitmap>() + CURSOR_BITMAP_SIZE;
const BITMAP_META_OFFSET: usize = mem::size_of::<spa_meta_cursor>();
const BITMAP_DATA_OFFSET: usize = mem::size_of::<spa_meta_bitmap>();

pub struct PipeWire {
    _context: ContextRc,
    pub core: CoreRc,
    pub token: RegistrationToken,
    event_loop: LoopHandle<'static, AnvilState<UdevData>>,
    to_state: calloop::channel::Sender<PwToState>,
}

pub enum PwToState {
    StopCast { session_id: CastSessionId },
    Redraw { stream_id: CastStreamId },
    FatalError,
}

pub struct Cast {
    event_loop: LoopHandle<'static, AnvilState<UdevData>>,
    pub session_id: CastSessionId,
    pub stream_id: CastStreamId,
    _listener: StreamListener<()>,
    pub stream: StreamRc,
    pub target: CastTarget,
    pub dynamic_target: bool,
    formats: FormatSet,
    offer_alpha: bool,
    pub cursor_mode: CursorMode,
    pub last_frame_time: Duration,
    scheduled_redraw: Option<RegistrationToken>,
    sequence_counter: u64,
    inner: Rc<RefCell<CastInner>>,
}

#[derive(Debug)]
struct CastInner {
    is_active: bool,
    node_id: Option<u32>,
    state: CastState,
    refresh: u32,
    min_time_between_frames: Duration,
    dmabufs: HashMap<i64, Dmabuf>,
    rendering_buffers: Vec<(NonNull<pw_buffer>, SyncPoint)>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum CastState {
    ResizePending {
        pending_size: Size<u32, Physical>,
    },
    ConfirmationPending {
        size: Size<u32, Physical>,
        alpha: bool,
        modifier: Modifier,
        plane_count: i32,
    },
    Ready {
        size: Size<u32, Physical>,
        alpha: bool,
        modifier: Modifier,
        plane_count: i32,
        damage_tracker: Option<OutputDamageTracker>,
        cursor_damage_tracker: Option<OutputDamageTracker>,
        last_cursor_location: Option<Point<i32, Physical>>,
    },
}

#[derive(PartialEq, Eq)]
pub enum CastSizeChange {
    Ready,
    Pending,
}

pub struct CursorData<'a, E> {
    elem_count: usize,
    relocated: Vec<RelocateRenderElement<&'a E>>,
    location: Point<i32, Physical>,
    hotspot: Point<i32, Physical>,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
}

impl<'a, E: Element> CursorData<'a, E> {
    pub fn compute(
        elements: &'a [E],
        elem_count: usize,
        location: Point<f64, Logical>,
        scale: Scale<f64>,
    ) -> Self {
        let pointer_elements = &elements[..elem_count];
        let location = location.to_physical_precise_round(scale);

        let geo = crate::render_helpers::encompassing_geo(scale, pointer_elements.iter());
        let relocated = Vec::from_iter(pointer_elements.iter().map(|elem| {
            return RelocateRenderElement::from_element(elem, geo.loc.upscale(-1), Relocate::Relative)
        }));

        return Self {
            elem_count,
            relocated,
            location,
            hotspot: location - geo.loc,
            size: geo.size,
            scale,
        }
    }
}

macro_rules! make_params {
    ($params:ident, $formats:expr, $size:expr, $refresh:expr, $alpha:expr) => {
        let mut b1 = Vec::new();
        let mut b2 = Vec::new();

        let o1 = make_video_params($formats, $size, $refresh, false);
        let pod1 = make_pod(&mut b1, o1);

        let mut p1;
        let mut p2;
        $params = if $alpha {
            let o2 = make_video_params($formats, $size, $refresh, true);
            p2 = [pod1, make_pod(&mut b2, o2)];
            &mut p2[..]
        } else {
            p1 = [pod1];
            &mut p1[..]
        };
    };
}

impl PipeWire {
    pub fn new(
        event_loop: LoopHandle<'static, AnvilState<UdevData>>,
        to_state: calloop::channel::Sender<PwToState>,
    ) -> anyhow::Result<Self> {
        let main_loop = MainLoopRc::new(None).context("error creating MainLoop")?;
        let context = ContextRc::new(&main_loop, None).context("error creating Context")?;
        let core = context.connect_rc(None).context("error creating Core")?;

        let to_state_ = to_state.clone();
        let listener = core
            .add_listener_local()
            .error(move |id, seq, res, message| {
                warn!(id, seq, res, message, "pw error");

                if id == PW_ID_CORE && res == -32
                    && let Err(err) = to_state_.send(PwToState::FatalError) {
                        warn!("error sending FatalError to state: {err:?}");
                    }
            })
            .register();
        mem::forget(listener);

        struct AsFdWrapper(MainLoopRc);
        impl AsFd for AsFdWrapper {
            fn as_fd(&self) -> BorrowedFd<'_> {
                return self.0.loop_().fd()
            }
        }
        let generic = Generic::new(AsFdWrapper(main_loop), Interest::READ, Mode::Level);
        let token = event_loop
            .insert_source(generic, move |_, wrapper, _| {
                wrapper.0.loop_().iterate(Duration::ZERO);
                return Ok(PostAction::Continue)
            })
            .unwrap();

        return Ok(Self {
            _context: context,
            core,
            token,
            event_loop,
            to_state,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start_cast(
        &self,
        gbm: GbmDevice<DrmDeviceFd>,
        formats: FormatSet,
        session_id: CastSessionId,
        stream_id: CastStreamId,
        target: CastTarget,
        size: Size<i32, Physical>,
        refresh: u32,
        alpha: bool,
        mut cursor_mode: CursorMode,
        signal_ctx: SignalEmitter<'static>,
    ) -> anyhow::Result<Cast> {
        let to_state_ = self.to_state.clone();
        let stop_cast = move || {
            if let Err(err) = to_state_.send(PwToState::StopCast { session_id }) {
                warn!(%session_id, "error sending StopCast to state: {err:?}");
            }
        };
        let to_state_ = self.to_state.clone();
        let redraw = move || {
            if let Err(err) = to_state_.send(PwToState::Redraw { stream_id }) {
                warn!(%stream_id, "error sending Redraw to state: {err:?}");
            }
        };
        let redraw_ = redraw.clone();

        let stream = StreamRc::new(
            self.core.clone(),
            "bakawm-screen-cast-src",
            PropertiesBox::new(),
        )
        .context("error creating Stream")?;

        if cursor_mode == CursorMode::Metadata && !pw_version_supports_cursor_metadata() {
            debug!(
                "metadata cursor mode requested, but PipeWire is too old (need >= 1.4.8); \
                 switching to embedded cursor"
            );
            cursor_mode = CursorMode::Embedded;
        }

        let pending_size = Size::from((size.w as u32, size.h as u32));

        let inner = Rc::new(RefCell::new(CastInner {
            is_active: false,
            node_id: None,
            state: CastState::ResizePending { pending_size },
            refresh,
            min_time_between_frames: Duration::ZERO,
            dmabufs: HashMap::new(),
            rendering_buffers: Vec::new(),
        }));

        let listener = stream
            .add_local_listener_with_user_data(())
            .state_changed({
                let inner = inner.clone();
                let stop_cast = stop_cast.clone();
                move |stream, (), old, new| {
                    let _span = tracing::span!(tracing::Level::DEBUG, "state_changed", %stream_id).entered();
                    debug!("{old:?} -> {new:?}");
                    let mut inner = inner.borrow_mut();

                    match new {
                        StreamState::Paused => {
                            if inner.node_id.is_none() {
                                let id = stream.node_id();
                                inner.node_id = Some(id);
                                debug!("sending PipeWireStreamAdded signal with node_id={id}");

                                async_io::block_on(async {
                                    let res = mutter_screen_cast::Stream::pipe_wire_stream_added(
                                        &signal_ctx,
                                        id,
                                    )
                                    .await;

                                    if let Err(err) = res {
                                        warn!("error sending PipeWireStreamAdded: {err:?}");
                                        stop_cast();
                                    }
                                });
                            }

                            inner.is_active = false;
                        }
                        StreamState::Error(_) => {
                            if inner.is_active {
                                inner.is_active = false;
                                stop_cast();
                            }
                        }
                        StreamState::Unconnected => (),
                        StreamState::Connecting => (),
                        StreamState::Streaming => {
                            inner.is_active = true;
                            debug!(node_id = ?inner.node_id, "screencast stream now streaming, requesting redraw");
                            redraw();
                        }
                    }
                }
            })
            .param_changed({
                let inner = inner.clone();
                let stop_cast = stop_cast.clone();
                let gbm = gbm.clone();
                let formats = formats.clone();
                move |stream, (), id, pod| {
                    let id = ParamType::from_raw(id);
                    trace!(%stream_id, ?id, "param_changed");
                    let mut inner = inner.borrow_mut();
                    let inner = &mut *inner;

                    if id != ParamType::Format {
                        return;
                    }

                    let Some(pod) = pod else { return };

                    let (m_type, m_subtype) = match parse_format(pod) {
                        Ok(x) => x,
                        Err(err) => {
                            warn!("error parsing format: {err:?}");
                            return;
                        }
                    };

                    if m_type != MediaType::Video || m_subtype != MediaSubtype::Raw {
                        return;
                    }

                    let mut format = VideoInfoRaw::new();
                    format.parse(pod).unwrap();
                    debug!("got format = {format:?}");

                    let format_size = Size::from((format.size().width, format.size().height));

                    let state = &mut inner.state;
                    if format_size != state.expected_format_size() {
                        if !matches!(&*state, CastState::ResizePending { .. }) {
                            warn!("wrong size, but we're not resizing");
                            stop_cast();
                            return;
                        }

                        debug!("wrong size, waiting");
                        return;
                    }

                    let format_has_alpha = format.format() == VideoFormat::BGRA;
                    let fourcc = if format_has_alpha {
                        Fourcc::Argb8888
                    } else {
                        Fourcc::Xrgb8888
                    };

                    let max_frame_rate = format.max_framerate();
                    let min_frame_time = Duration::from_micros(
                        1_000_000 * u64::from(max_frame_rate.denom)
                            / u64::from(max_frame_rate.num),
                    );
                    inner.min_time_between_frames = min_frame_time;

                    let object = pod.as_object().unwrap();
                    let Some(prop_modifier) =
                        object.find_prop(spa::utils::Id(FormatProperties::VideoModifier.0))
                    else {
                        warn!("modifier prop missing");
                        stop_cast();
                        return;
                    };

                    if prop_modifier.flags().contains(PodPropFlags::DONT_FIXATE) {
                        debug!("fixating the modifier");

                        let pod_modifier = prop_modifier.value();
                        let Ok((_, modifiers)) =
                            PodDeserializer::deserialize_from::<Choice<i64>>(pod_modifier.as_bytes())
                        else {
                            warn!("wrong modifier property type");
                            stop_cast();
                            return;
                        };

                        let ChoiceEnum::Enum { alternatives, .. } = modifiers.1 else {
                            warn!("wrong modifier choice type");
                            stop_cast();
                            return;
                        };

                        let (modifier, plane_count) = match find_preferred_modifier(
                            &gbm,
                            format_size,
                            fourcc,
                            alternatives,
                        ) {
                            Ok(x) => x,
                            Err(err) => {
                                warn!("couldn't find preferred modifier: {err:?}");
                                stop_cast();
                                return;
                            }
                        };

                        debug!(
                            "allocation successful (modifier={modifier:?}, plane_count={plane_count}), moving to confirmation pending"
                        );

                        *state = CastState::ConfirmationPending {
                            size: format_size,
                            alpha: format_has_alpha,
                            modifier,
                            plane_count: plane_count as i32,
                        };

                        let fixated_format = FormatSet::from_iter([Format {
                            code: fourcc,
                            modifier,
                        }]);

                        let mut b1 = Vec::new();
                        let mut b2 = Vec::new();

                        let o1 = make_video_params(
                            &fixated_format,
                            format_size,
                            inner.refresh,
                            format_has_alpha,
                        );
                        let pod1 = make_pod(&mut b1, o1);

                        let o2 = make_video_params(
                            &formats,
                            format_size,
                            inner.refresh,
                            format_has_alpha,
                        );
                        let mut params = [pod1, make_pod(&mut b2, o2)];

                        if let Err(err) = stream.update_params(&mut params) {
                            warn!("error updating stream params: {err:?}");
                            stop_cast();
                        }

                        return;
                    }

                    let plane_count = match &*state {
                        CastState::ConfirmationPending {
                            size,
                            alpha,
                            modifier,
                            plane_count,
                        }
                        | CastState::Ready {
                            size,
                            alpha,
                            modifier,
                            plane_count,
                            ..
                        } if *alpha == format_has_alpha
                            && *modifier == Modifier::from(format.modifier()) =>
                        {
                            let size = *size;
                            let alpha = *alpha;
                            let modifier = *modifier;
                            let plane_count = *plane_count;

                            let (damage_tracker, cursor_damage_tracker) =
                                if let CastState::Ready {
                                    damage_tracker,
                                    cursor_damage_tracker,
                                    ..
                                } = &mut *state
                                {
                                    (damage_tracker.take(), cursor_damage_tracker.take())
                                } else {
                                    (None, None)
                                };

                            debug!("moving to ready state");

                            *state = CastState::Ready {
                                size,
                                alpha,
                                modifier,
                                plane_count,
                                damage_tracker,
                                cursor_damage_tracker,
                                last_cursor_location: None,
                            };

                            plane_count
                        }
                        _ => {
                            let (modifier, plane_count) = match find_preferred_modifier(
                                &gbm,
                                format_size,
                                fourcc,
                                vec![format.modifier() as i64],
                            ) {
                                Ok(x) => x,
                                Err(err) => {
                                    warn!("test allocation failed: {err:?}");
                                    stop_cast();
                                    return;
                                }
                            };

                            debug!(
                                "allocation successful (modifier={modifier:?}, plane_count={plane_count}), moving to ready"
                            );

                            *state = CastState::Ready {
                                size: format_size,
                                alpha: format_has_alpha,
                                modifier,
                                plane_count: plane_count as i32,
                                damage_tracker: None,
                                cursor_damage_tracker: None,
                                last_cursor_location: None,
                            };

                            plane_count as i32
                        }
                    };

                    let o1 = pod::object!(
                        SpaTypes::ObjectParamBuffers,
                        ParamType::Buffers,
                        Property::new(
                            SPA_PARAM_BUFFERS_buffers,
                            pod::Value::Choice(ChoiceValue::Int(Choice(
                                ChoiceFlags::empty(),
                                ChoiceEnum::Range {
                                    default: 8,
                                    min: 2,
                                    max: 16
                                }
                            ))),
                        ),
                        Property::new(SPA_PARAM_BUFFERS_blocks, pod::Value::Int(plane_count)),
                        Property::new(
                            SPA_PARAM_BUFFERS_dataType,
                            pod::Value::Choice(ChoiceValue::Int(Choice(
                                ChoiceFlags::empty(),
                                ChoiceEnum::Flags {
                                    default: 1 << DataType::DmaBuf.as_raw(),
                                    flags: vec![1 << DataType::DmaBuf.as_raw()],
                                },
                            ))),
                        ),
                    );

                    let o2 = pod::object!(
                        SpaTypes::ObjectParamMeta,
                        ParamType::Meta,
                        Property::new(
                            SPA_PARAM_META_type,
                            pod::Value::Id(spa::utils::Id(SPA_META_Header))
                        ),
                        Property::new(
                            SPA_PARAM_META_size,
                            pod::Value::Int(mem::size_of::<spa_meta_header>() as i32)
                        ),
                    );
                    let mut b1 = vec![];
                    let mut b2 = vec![];
                    let mut params = vec![make_pod(&mut b1, o1), make_pod(&mut b2, o2)];

                    let mut b_cursor = vec![];
                    if cursor_mode == CursorMode::Metadata {
                        let o_cursor = pod::object!(
                            SpaTypes::ObjectParamMeta,
                            ParamType::Meta,
                            Property::new(
                                SPA_PARAM_META_type,
                                pod::Value::Id(spa::utils::Id(SPA_META_Cursor))
                            ),
                            Property::new(
                                SPA_PARAM_META_size,
                                pod::Value::Int(CURSOR_META_SIZE as i32)
                            ),
                        );
                        params.push(make_pod(&mut b_cursor, o_cursor));
                    }

                    if let Err(err) = stream.update_params(&mut params) {
                        warn!("error updating stream params: {err:?}");
                        stop_cast();
                    }
                }
            })
            .add_buffer({
                let inner = inner.clone();
                let stop_cast = stop_cast.clone();
                move |stream, (), buffer| {
                    let _span = tracing::span!(tracing::Level::DEBUG, "add_buffer", %stream_id).entered();
                    let mut inner = inner.borrow_mut();

                    let (size, alpha, modifier) = if let CastState::Ready {
                        size,
                        alpha,
                        modifier,
                        ..
                    } = &inner.state
                    {
                        (*size, *alpha, *modifier)
                    } else {
                        trace!("add_buffer, but not ready yet");
                        return;
                    };

                    trace!("size={size:?}, alpha={alpha}, modifier={modifier:?}");

                    unsafe {
                        let spa_buffer = (*buffer).buffer;

                        let fourcc = if alpha {
                            Fourcc::Argb8888
                        } else {
                            Fourcc::Xrgb8888
                        };

                        let dmabuf = match allocate_dmabuf(&gbm, size, fourcc, modifier) {
                            Ok(dmabuf) => dmabuf,
                            Err(err) => {
                                warn!("error allocating dmabuf: {err:?}");
                                stop_cast();
                                return;
                            }
                        };

                        let plane_count = dmabuf.num_planes();
                        assert_eq!((*spa_buffer).n_datas as usize, plane_count);

                        for (i, (fd, (stride, offset))) in
                            zip(dmabuf.handles(), zip(dmabuf.strides(), dmabuf.offsets()))
                                .enumerate()
                        {
                            let spa_data = (*spa_buffer).datas.add(i);
                            assert!((*spa_data).type_ & (1 << DataType::DmaBuf.as_raw()) > 0);

                            (*spa_data).type_ = DataType::DmaBuf.as_raw();
                            (*spa_data).maxsize = 1;
                            (*spa_data).fd = fd.as_raw_fd() as i64;
                            (*spa_data).flags = SPA_DATA_FLAG_READWRITE;

                            let chunk = (*spa_data).chunk;
                            (*chunk).stride = stride as i32;
                            (*chunk).offset = offset;
                        }

                        let fd = (*(*spa_buffer).datas).fd;
                        assert!(inner.dmabufs.insert(fd, dmabuf).is_none());
                    }

                    if inner.dmabufs.len() == 1 && stream.state() == StreamState::Streaming {
                        redraw_();
                    }
                }
            })
            .remove_buffer({
                let inner = inner.clone();
                move |_stream, (), buffer| {
                    trace!(%stream_id, "remove_buffer");
                    let mut inner = inner.borrow_mut();

                    inner
                        .rendering_buffers
                        .retain(|(buf, _)| return buf.as_ptr() != buffer);

                    unsafe {
                        let spa_buffer = (*buffer).buffer;
                        let spa_data = (*spa_buffer).datas;
                        assert!((*spa_buffer).n_datas > 0);

                        let fd = (*spa_data).fd;
                        inner.dmabufs.remove(&fd);
                    }
                }
            })
            .register()
            .unwrap();

        trace!(
            %stream_id,
            "starting pw stream with size={pending_size:?}, refresh={refresh:?}"
        );

        let params;
        make_params!(params, &formats, pending_size, refresh, alpha);
        stream
            .connect(
                Direction::Output,
                None,
                StreamFlags::DRIVER | StreamFlags::ALLOC_BUFFERS,
                params,
            )
            .context("error connecting stream")?;

        let cast = Cast {
            event_loop: self.event_loop.clone(),
            session_id,
            stream_id,
            stream,
            _listener: listener,
            target,
            dynamic_target: false,
            formats,
            offer_alpha: alpha,
            cursor_mode,
            last_frame_time: Duration::ZERO,
            scheduled_redraw: None,
            sequence_counter: 0,
            inner,
        };
        return Ok(cast)
    }
}

impl Cast {
    pub fn is_active(&self) -> bool {
        return self.inner.borrow().is_active
    }

    pub fn node_id(&self) -> Option<u32> {
        return self.inner.borrow().node_id
    }

    pub fn ensure_size(&self, size: Size<i32, Physical>) -> anyhow::Result<CastSizeChange> {
        let mut inner = self.inner.borrow_mut();

        let new_size = Size::from((size.w as u32, size.h as u32));

        let state = &mut inner.state;
        if matches!(state, CastState::Ready { size, .. } if *size == new_size) {
            return Ok(CastSizeChange::Ready);
        }

        if state.pending_size() == Some(new_size) {
            debug!("stream size still hasn't changed, skipping frame");
            return Ok(CastSizeChange::Pending);
        }

        debug!("cast size changed, updating stream size");

        *state = CastState::ResizePending {
            pending_size: new_size,
        };

        let params;
        make_params!(
            params,
            &self.formats,
            new_size,
            inner.refresh,
            self.offer_alpha
        );
        self.stream
            .update_params(params)
            .context("error updating stream params")?;

        return Ok(CastSizeChange::Pending)
    }

    pub fn set_refresh(&mut self, refresh: u32) -> anyhow::Result<()> {
        let mut inner = self.inner.borrow_mut();

        if inner.refresh == refresh {
            return Ok(());
        }

        debug!("cast FPS changed, updating stream FPS");
        inner.refresh = refresh;

        let size = inner.state.expected_format_size();
        let params;
        make_params!(params, &self.formats, size, refresh, self.offer_alpha);
        self.stream
            .update_params(params)
            .context("error updating stream params")?;

        return Ok(())
    }

    fn compute_extra_delay(&self, target_frame_time: Duration) -> Duration {
        let inner = self.inner.borrow();

        let last = self.last_frame_time;
        let min = inner.min_time_between_frames;

        if last.is_zero() {
            return Duration::ZERO;
        }

        if target_frame_time < last {
            warn!(?target_frame_time, ?last, "target frame time is below last");
            return Duration::ZERO;
        }

        let diff = target_frame_time - last;
        if diff < min {
            return min - diff
        } else {
            return Duration::ZERO
        }
    }

    fn schedule_redraw(&mut self, output: Output, target_time: Duration) {
        if self.scheduled_redraw.is_some() {
            return;
        }

        let now = get_monotonic_time();
        let duration = target_time.saturating_sub(now);
        let timer = Timer::from_duration(duration);
        let token = self
            .event_loop
            .insert_source(timer, move |_, _, state| {
                state.backend_data.queue_redraw(&output);
                return TimeoutAction::Drop
            })
            .unwrap();
        self.scheduled_redraw = Some(token);
    }

    fn remove_scheduled_redraw(&mut self) {
        if let Some(token) = self.scheduled_redraw.take() {
            self.event_loop.remove(token);
        }
    }

    pub fn check_time_and_schedule(
        &mut self,
        output: &Output,
        target_frame_time: Duration,
    ) -> bool {
        let delay = self.compute_extra_delay(target_frame_time);
        if delay >= CAST_DELAY_ALLOWANCE {
            self.schedule_redraw(output.clone(), target_frame_time + delay);
            return true
        } else {
            self.remove_scheduled_redraw();
            return false
        }
    }

    fn dequeue_available_buffer(&mut self) -> Option<NonNull<pw_buffer>> {
        let buf = unsafe { NonNull::new(self.stream.dequeue_raw_buffer()) };
        if buf.is_none() {
            tracing::warn!(
                "dequeue_available_buffer: no buffer available, stream state={:?}",
                self.stream.state()
            );
        }
        return buf
    }

    fn queue_completed_buffers(&mut self) {
        let mut inner = self.inner.borrow_mut();

        let first_in_progress_idx = inner
            .rendering_buffers
            .iter()
            .position(|(_, sync)| return !sync.is_reached())
            .unwrap_or(inner.rendering_buffers.len());

        let count = first_in_progress_idx;
        for (buffer, _) in inner.rendering_buffers.drain(..first_in_progress_idx) {
            trace!("queueing completed buffer");
            unsafe {
                pw_stream_queue_buffer(self.stream.as_raw_ptr(), buffer.as_ptr());
            }
        }
        if count > 0 {
            tracing::trace!(
                "queue_completed_buffers: queued {count} buffers, remaining={}",
                inner.rendering_buffers.len()
            );
        }
    }

    unsafe fn queue_after_sync(&mut self, pw_buffer: NonNull<pw_buffer>, sync_point: SyncPoint) {
        let mut inner = self.inner.borrow_mut();

        let mut sync_point = sync_point;
        let sync_fd = match sync_point.export() {
            Some(sync_fd) => {
                tracing::debug!("queue_after_sync: exported sync fd, will wait for completion");
                Some(sync_fd)
            }
            None => {
                tracing::debug!(
                    "queue_after_sync: cannot export sync fd, using signaled sync point"
                );
                sync_point = SyncPoint::signaled();
                None
            }
        };

        inner.rendering_buffers.push((pw_buffer, sync_point));
        drop(inner);

        match sync_fd {
            None => {
                self.queue_completed_buffers();
            }
            Some(sync_fd) => {
                let stream_id = self.stream_id;
                let source = Generic::new(sync_fd, Interest::READ, Mode::OneShot);
                self.event_loop
                    .insert_source(source, move |_, _, state| {
                        for cast in &mut state.screencasting.casts {
                            if cast.stream_id == stream_id {
                                cast.queue_completed_buffers();
                            }
                        }

                        return Ok(PostAction::Remove)
                    })
                    .unwrap();
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn dequeue_buffer_and_render(
        &mut self,
        renderer: &mut GlesRenderer,
        mut elements: &[CastRenderElement],
        cursor_data: &CursorData<'_, CastRenderElement>,
        size: Size<i32, Physical>,
        scale: Scale<f64>,
    ) -> bool {
        tracing::trace!(
            "dequeue_buffer_and_render called, elements={}, size={:?}",
            elements.len(),
            size
        );
        let mut inner = self.inner.borrow_mut();

        let CastState::Ready {
            damage_tracker,
            cursor_damage_tracker,
            last_cursor_location,
            ..
        } = &mut inner.state
        else {
            error!("cast must be in Ready state to render");
            return false;
        };
        let damage_tracker = damage_tracker
            .get_or_insert_with(|| return OutputDamageTracker::new(size, scale, Transform::Normal));
        let cursor_damage_tracker = cursor_damage_tracker.get_or_insert_with(|| {
            return OutputDamageTracker::new(
                Size::from((CURSOR_WIDTH as _, CURSOR_HEIGHT as _)),
                scale,
                Transform::Normal,
            )
        });

        let OutputModeSource::Static { scale: t_scale, .. } = damage_tracker.mode() else {
            unreachable!();
        };
        if *t_scale != scale {
            *damage_tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
            *cursor_damage_tracker = OutputDamageTracker::new(
                Size::from((CURSOR_WIDTH as _, CURSOR_HEIGHT as _)),
                scale,
                Transform::Normal,
            );
        }

        let mut has_cursor_update = false;
        let mut redraw_cursor = false;

        if self.cursor_mode == CursorMode::Metadata || self.cursor_mode == CursorMode::Hidden {
            elements = &elements[cursor_data.elem_count..];
        }
        let (damage, states) = damage_tracker.damage_output(1, elements).unwrap();
        tracing::trace!(
            "damage_output result: damage={:?}, elements_count={}",
            damage.is_some(),
            elements.len()
        );

        if self.cursor_mode == CursorMode::Metadata {
            let (damage, _states) = cursor_damage_tracker
                .damage_output(1, &cursor_data.relocated)
                .unwrap();
            redraw_cursor = damage.is_some();
            has_cursor_update =
                redraw_cursor || *last_cursor_location != Some(cursor_data.location);
        }

        if damage.is_none() && !has_cursor_update {
            tracing::trace!("no damage, skipping frame");
            return false;
        }
        *last_cursor_location = Some(cursor_data.location);
        drop(inner);

        let Some(pw_buffer) = self.dequeue_available_buffer() else {
            warn!("no available buffer in pw stream, skipping frame");
            return false;
        };
        let buffer = pw_buffer.as_ptr();

        let mut inner = self.inner.borrow_mut();
        let inner_ = &mut *inner;
        let CastState::Ready { damage_tracker, .. } = &mut inner_.state else {
            unreachable!()
        };
        let damage_tracker = damage_tracker.as_mut().unwrap();

        unsafe {
            let spa_buffer = (*buffer).buffer;

            if self.cursor_mode == CursorMode::Metadata {
                add_cursor_metadata(renderer, spa_buffer, cursor_data, redraw_cursor);
            }

            let fd = (*(*spa_buffer).datas).fd;
            let dmabuf = inner_.dmabufs[&fd].clone();

            let res = crate::render_helpers::render_to_dmabuf(
                renderer,
                damage_tracker,
                dmabuf,
                elements,
                states,
            );
            drop(inner);

            match res {
                Ok(sync_point) => {
                    tracing::trace!(
                        "rendered frame to dmabuf successfully, seq={}",
                        self.sequence_counter
                    );
                    mark_buffer_as_good(pw_buffer, &mut self.sequence_counter);
                    self.queue_after_sync(pw_buffer, sync_point);
                    return true
                }
                Err(err) => {
                    warn!("error rendering to dmabuf: {err:?}");
                    return_unused_buffer(&self.stream, pw_buffer);
                    return false
                }
            }
        }
    }

    pub fn dequeue_buffer_and_clear(&mut self, renderer: &mut GlesRenderer) -> bool {
        let mut inner = self.inner.borrow_mut();

        if let CastState::Ready {
            damage_tracker,
            cursor_damage_tracker,
            ..
        } = &mut inner.state
        {
            *damage_tracker = None;
            *cursor_damage_tracker = None;
        };
        drop(inner);

        let Some(pw_buffer) = self.dequeue_available_buffer() else {
            warn!("no available buffer in pw stream, skipping frame");
            return false;
        };
        let buffer = pw_buffer.as_ptr();

        unsafe {
            let spa_buffer = (*buffer).buffer;

            if self.cursor_mode == CursorMode::Metadata {
                add_invisible_cursor(spa_buffer);
            }

            let fd = (*(*spa_buffer).datas).fd;
            let dmabuf = self.inner.borrow().dmabufs[&fd].clone();

            match crate::render_helpers::clear_dmabuf(renderer, dmabuf) {
                Ok(sync_point) => {
                    mark_buffer_as_good(pw_buffer, &mut self.sequence_counter);
                    self.queue_after_sync(pw_buffer, sync_point);
                    return true
                }
                Err(err) => {
                    warn!("error clearing dmabuf: {err:?}");
                    return_unused_buffer(&self.stream, pw_buffer);
                    return false
                }
            }
        }
    }
}

impl CastState {
    fn pending_size(&self) -> Option<Size<u32, Physical>> {
        match self {
            CastState::ResizePending { pending_size } => return Some(*pending_size),
            CastState::ConfirmationPending { size, .. } => return Some(*size),
            CastState::Ready { .. } => return None,
        }
    }

    fn expected_format_size(&self) -> Size<u32, Physical> {
        match self {
            CastState::ResizePending { pending_size } => return *pending_size,
            CastState::ConfirmationPending { size, .. } => return *size,
            CastState::Ready { size, .. } => return *size,
        }
    }
}

fn pw_version_supports_cursor_metadata() -> bool {
    unsafe { return pw_check_library_version(1, 4, 8) }
}

fn make_video_params(
    formats: &FormatSet,
    size: Size<u32, Physical>,
    refresh: u32,
    alpha: bool,
) -> pod::Object {
    let format = if alpha {
        VideoFormat::BGRA
    } else {
        VideoFormat::BGRx
    };

    let fourcc = if alpha {
        Fourcc::Argb8888
    } else {
        Fourcc::Xrgb8888
    };

    let formats: Vec<_> = formats
        .iter()
        .filter_map(|f| return (f.code == fourcc).then_some(u64::from(f.modifier) as i64))
        .collect();

    let dont_fixate = if formats.len() > 1 {
        PropertyFlags::DONT_FIXATE
    } else {
        PropertyFlags::empty()
    };

    return pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(FormatProperties::VideoFormat, Id, format),
        Property {
            key: FormatProperties::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY | dont_fixate,
            value: pod::Value::Choice(ChoiceValue::Long(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: formats[0],
                    alternatives: formats,
                }
            )))
        },
        pod::property!(
            FormatProperties::VideoSize,
            Rectangle,
            Rectangle {
                width: size.w,
                height: size.h,
            }
        ),
        pod::property!(
            FormatProperties::VideoFramerate,
            Fraction,
            Fraction { num: 0, denom: 1 }
        ),
        pod::property!(
            FormatProperties::VideoMaxFramerate,
            Choice,
            Range,
            Fraction,
            Fraction {
                num: refresh,
                denom: 1000
            },
            Fraction { num: 1, denom: 1 },
            Fraction {
                num: refresh,
                denom: 1000
            }
        ),
    )
}

fn make_pod(buffer: &mut Vec<u8>, object: pod::Object) -> &Pod {
    PodSerializer::serialize(Cursor::new(&mut *buffer), &pod::Value::Object(object)).unwrap();
    return Pod::from_bytes(buffer).unwrap()
}

fn find_preferred_modifier(
    gbm: &GbmDevice<DrmDeviceFd>,
    size: Size<u32, Physical>,
    fourcc: Fourcc,
    modifiers: Vec<i64>,
) -> anyhow::Result<(Modifier, usize)> {
    let (buffer, modifier) = allocate_buffer(gbm, size, fourcc, &modifiers)?;

    let dmabuf = buffer
        .export()
        .context("error exporting GBM buffer object as dmabuf")?;
    let plane_count = dmabuf.num_planes();

    return Ok((modifier, plane_count))
}

fn allocate_buffer(
    gbm: &GbmDevice<DrmDeviceFd>,
    size: Size<u32, Physical>,
    fourcc: Fourcc,
    modifiers: &[i64],
) -> anyhow::Result<(GbmBuffer, Modifier)> {
    let (w, h) = (size.w, size.h);
    let flags = GbmBufferFlags::RENDERING;

    if modifiers.len() == 1 && Modifier::from(modifiers[0] as u64) == Modifier::Invalid {
        let bo = gbm
            .create_buffer_object::<()>(w, h, fourcc, flags)
            .context("error creating GBM buffer object")?;

        let buffer = GbmBuffer::from_bo(bo, true);
        return Ok((buffer, Modifier::Invalid))
    } else {
        let modifiers = modifiers
            .iter()
            .map(|m| return Modifier::from(*m as u64))
            .filter(|m| return *m != Modifier::Invalid);

        let bo = gbm
            .create_buffer_object_with_modifiers2::<()>(w, h, fourcc, modifiers, flags)
            .context("error creating GBM buffer object")?;

        let modifier = bo.modifier();
        let buffer = GbmBuffer::from_bo(bo, false);
        return Ok((buffer, modifier))
    }
}

fn allocate_dmabuf(
    gbm: &GbmDevice<DrmDeviceFd>,
    size: Size<u32, Physical>,
    fourcc: Fourcc,
    modifier: Modifier,
) -> anyhow::Result<Dmabuf> {
    let (buffer, _modifier) = allocate_buffer(gbm, size, fourcc, &[u64::from(modifier) as i64])?;
    let dmabuf = buffer
        .export()
        .context("error exporting GBM buffer object as dmabuf")?;
    return Ok(dmabuf)
}

unsafe fn return_unused_buffer(stream: &Stream, pw_buffer: NonNull<pw_buffer>) {
    unsafe {
        let pw_buffer = pw_buffer.as_ptr();
        let spa_buffer = (*pw_buffer).buffer;
        let chunk = (*(*spa_buffer).datas).chunk;
        (*chunk).size = 0;
        (*chunk).flags = SPA_CHUNK_FLAG_CORRUPTED as i32;

        if let Some(header) = find_meta_header(spa_buffer) {
            let header = header.as_ptr();
            (*header).flags = SPA_META_HEADER_FLAG_CORRUPTED;
        }

        pw_stream_queue_buffer(stream.as_raw_ptr(), pw_buffer);
    }
}

unsafe fn mark_buffer_as_good(pw_buffer: NonNull<pw_buffer>, sequence: &mut u64) {
    unsafe {
        let pw_buffer = pw_buffer.as_ptr();
        let spa_buffer = (*pw_buffer).buffer;
        let chunk = (*(*spa_buffer).datas).chunk;

        (*chunk).size = 1;
        (*chunk).flags = SPA_CHUNK_FLAG_NONE as i32;

        *sequence = sequence.wrapping_add(1);
        if let Some(header) = find_meta_header(spa_buffer) {
            let header = header.as_ptr();
            (*header).flags = 0;
            (*header).seq = *sequence;
            (*header).pts = -1;
        }
    }
}

unsafe fn find_meta_header(buffer: *mut spa_buffer) -> Option<NonNull<spa_meta_header>> {
    unsafe {
        let p =
            spa_buffer_find_meta_data(buffer, SPA_META_Header, mem::size_of::<spa_meta_header>())
                .cast();
        return NonNull::new(p)
    }
}

unsafe fn add_invisible_cursor(spa_buffer: *mut spa_buffer) {
    unsafe {
        let cursor_meta_ptr: *mut spa_meta_cursor = spa_buffer_find_meta_data(
            spa_buffer,
            SPA_META_Cursor,
            mem::size_of::<spa_meta_cursor>(),
        )
        .cast();
        let Some(cursor_meta) = cursor_meta_ptr.as_mut() else {
            return;
        };

        cursor_meta.id = 1;
        cursor_meta.position.x = 0;
        cursor_meta.position.y = 0;
        cursor_meta.hotspot.x = 0;
        cursor_meta.hotspot.y = 0;
        cursor_meta.bitmap_offset = BITMAP_META_OFFSET as _;

        let bitmap_meta_ptr = cursor_meta_ptr
            .byte_add(BITMAP_META_OFFSET)
            .cast::<spa_meta_bitmap>();
        let bitmap_meta = &mut *bitmap_meta_ptr;

        bitmap_meta.offset = BITMAP_DATA_OFFSET as _;
        bitmap_meta.size.width = 1;
        bitmap_meta.size.height = 1;
        bitmap_meta.stride = CURSOR_BPP as i32;
        bitmap_meta.format = CURSOR_FORMAT;

        let bitmap_data = bitmap_meta_ptr.cast::<u8>().add(BITMAP_DATA_OFFSET);
        let bitmap_slice = slice::from_raw_parts_mut(bitmap_data, CURSOR_BITMAP_SIZE);
        bitmap_slice[..4].copy_from_slice(&[0, 0, 0, 0]);
    }
}

unsafe fn add_cursor_metadata(
    renderer: &mut GlesRenderer,
    spa_buffer: *mut spa_buffer,
    cursor_data: &CursorData<'_, impl RenderElement<GlesRenderer>>,
    redraw: bool,
) {
    unsafe {
        let cursor_meta_ptr: *mut spa_meta_cursor = spa_buffer_find_meta_data(
            spa_buffer,
            SPA_META_Cursor,
            mem::size_of::<spa_meta_cursor>(),
        )
        .cast();
        let Some(cursor_meta) = cursor_meta_ptr.as_mut() else {
            return;
        };

        cursor_meta.id = 1;
        cursor_meta.position.x = cursor_data.location.x;
        cursor_meta.position.y = cursor_data.location.y;
        cursor_meta.hotspot.x = cursor_data.hotspot.x;
        cursor_meta.hotspot.y = cursor_data.hotspot.y;

        if !redraw {
            trace!("cursor not damaged, skipping rerendering");
            cursor_meta.bitmap_offset = 0;
            return;
        }

        cursor_meta.bitmap_offset = BITMAP_META_OFFSET as _;

        let bitmap_meta_ptr = cursor_meta_ptr
            .byte_add(BITMAP_META_OFFSET)
            .cast::<spa_meta_bitmap>();
        let bitmap_meta = &mut *bitmap_meta_ptr;

        bitmap_meta.offset = BITMAP_DATA_OFFSET as _;
        bitmap_meta.size.width = 1;
        bitmap_meta.size.height = 1;
        bitmap_meta.stride = CURSOR_BPP as i32;
        bitmap_meta.format = CURSOR_FORMAT;

        let bitmap_data = bitmap_meta_ptr.cast::<u8>().add(BITMAP_DATA_OFFSET);
        let bitmap_slice = slice::from_raw_parts_mut(bitmap_data, CURSOR_BITMAP_SIZE);
        bitmap_slice[..4].copy_from_slice(&[0, 0, 0, 0]);

        let size = Size::new(
            min(cursor_data.size.w, CURSOR_WIDTH as i32),
            min(cursor_data.size.h, CURSOR_HEIGHT as i32),
        );
        if size.w == 0 || size.h == 0 {
            trace!("cursor is invisible, skipping rendering");
            return;
        }

        let mapping = match crate::render_helpers::render_and_download(
            renderer,
            size,
            cursor_data.scale,
            Transform::Normal,
            Fourcc::Argb8888,
            cursor_data.relocated.iter().rev(),
        ) {
            Ok(mapping) => mapping,
            Err(err) => {
                warn!("error rendering cursor: {err:?}");
                return;
            }
        };
        let pixels = match renderer.map_texture(&mapping) {
            Ok(pixels) => pixels,
            Err(err) => {
                warn!("error mapping cursor texture: {err:?}");
                return;
            }
        };

        bitmap_slice[..pixels.len()].copy_from_slice(pixels);

        bitmap_meta.size.width = size.w as _;
        bitmap_meta.size.height = size.h as _;
        bitmap_meta.stride = size.w * CURSOR_BPP as i32;
    }
}
