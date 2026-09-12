use std::{
    sync::{Mutex, atomic::Ordering},
    time::Duration,
};

#[cfg(feature = "egl")]
use smithay::backend::renderer::ImportEgl;
#[cfg(feature = "debug")]
use smithay::{
    backend::{allocator::Fourcc, renderer::ImportMem},
    reexports::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle},
};

use smithay::{
    backend::{
        SwapBuffersError,
        allocator::dmabuf::Dmabuf,
        egl::EGLDevice,
        renderer::{
            ImportDma, ImportMemWl,
            damage::{Error as OutputDamageTrackerError, OutputDamageTracker},
            element::AsRenderElements,
            gles::GlesRenderer,
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::Space,
    input::{
        keyboard::LedState,
        pointer::{CursorImageAttributes, CursorImageStatus},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::EventLoop,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{Display, protocol::wl_surface},
        winit::event_loop::pump_events::PumpStatus,
    },
    utils::{IsAlive, Logical, Point, Scale, Transform},
    wayland::{
        compositor,
        dmabuf::{
            DmabufFeedback, DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState,
            ImportNotifier,
        },
        presentation::Refresh,
    },
};
use tracing::{error, info, warn};

use crate::shell::WindowElement;
use crate::state::{
    AnvilState, Backend, take_presentation_feedback, update_primary_scanout_output,
};
use crate::{drawing::*, render::*};

pub const OUTPUT_NAME: &str = "winit";

pub struct WinitData {
    backend: WinitGraphicsBackend<GlesRenderer>,
    damage_tracker: OutputDamageTracker,
    dmabuf_state: (DmabufState, DmabufGlobal, Option<DmabufFeedback>),
    full_redraw: u8,
    #[cfg(feature = "debug")]
    pub fps: fps_ticker::Fps,
}

impl DmabufHandler for AnvilState<WinitData> {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        return &mut self.backend_data.dmabuf_state.0
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        if self
            .backend_data
            .backend
            .renderer()
            .import_dmabuf(&dmabuf, None)
            .is_ok()
        {
            let _ = notifier.successful::<AnvilState<WinitData>>();
        } else {
            notifier.failed();
        }
    }
}

impl Backend for WinitData {
    fn seat_name(&self) -> String {
        return String::from("winit")
    }
    fn reset_buffers(&mut self, _output: &Output) {
        self.full_redraw = 4;
    }
    fn early_import(&mut self, _surface: &wl_surface::WlSurface) {}
    fn update_led_state(&mut self, _led_state: LedState) {}
    fn reload_cursor(&mut self, _theme: Option<&str>, _size: Option<u32>) {}

    fn queue_redraw(&mut self, _output: &Output) {
        // winit continuously renders, so just flag a full redraw
        self.full_redraw = self.full_redraw.max(2);
    }

    fn with_primary_renderer<T>(&mut self, f: impl FnOnce(&mut GlesRenderer) -> T) -> Option<T> {
        let renderer = self.backend.renderer();
        return Some(f(renderer))
    }

    fn capture_screenshot(
        &mut self,
        output: &Output,
        space: &Space<WindowElement>,
        pointer_location: Point<f64, Logical>,
        cursor_status: &CursorImageStatus,
        show_window_preview: bool,
        config: &crate::config::Config,
        _now: Duration,
    ) -> Option<crate::state::CapturedFrame> {
        use crate::render::output_elements;
        use crate::state::CapturedFrame;
        use smithay::backend::allocator::Fourcc;
        use smithay::backend::renderer::element::{AsRenderElements, Element, RenderElement};
        use smithay::backend::renderer::gles::GlesTexture;
        use smithay::backend::renderer::{
            Bind, Color32F, ExportMem, Frame, Offscreen, Renderer, Texture,
        };
        use smithay::utils::Rectangle;

        let scale = Scale::from(output.current_scale().fractional_scale());
        let output_transform = output.current_transform();
        let mode = output.current_mode()?;
        let size = mode.size;

        let renderer = self.backend.renderer();

        let output_geometry = space.output_geometry(output)?;

        let mut pointer_element = PointerElement::default();
        let mut custom_elements: Vec<CustomRenderElements<GlesRenderer>> = Vec::new();
        if output_geometry.to_f64().contains(pointer_location) {
            let cursor_hotspot = if let CursorImageStatus::Surface(surface) = cursor_status {
                compositor::with_states(surface, |states| {
                    return states
                        .data_map
                        .get::<Mutex<CursorImageAttributes>>()
                        .map(|attrs| return attrs.lock().unwrap().hotspot)
                        .unwrap_or_default()
                })
            } else {
                (0, 0).into()
            };
            let cursor_pos = pointer_location - output_geometry.loc.to_f64();
            pointer_element.set_status(cursor_status.clone());
            custom_elements.extend(
                pointer_element.render_elements(
                    renderer,
                    (cursor_pos - cursor_hotspot.to_f64())
                        .to_physical(scale)
                        .to_i32_round(),
                    scale,
                    1.0,
                ),
            );
        }

        let (elements, _clear_color) = output_elements(
            output,
            space,
            &[] as &[crate::shell::closing_window::ClosingWindow],
            custom_elements,
            renderer,
            show_window_preview,
            config,
        );

        let fourcc = Fourcc::Abgr8888;
        let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);

        let Ok(mut texture): Result<GlesTexture, _> = renderer.create_buffer(fourcc, buffer_size)
        else {
            warn!("Failed to create offscreen texture for screenshot");
            return None;
        };

        let Ok(mut target) = renderer.bind(&mut texture) else {
            warn!("Failed to bind offscreen texture for screenshot");
            return None;
        };

        let transform_inv = output_transform.invert();
        let output_rect = Rectangle::from_size(transform_inv.transform_size(size));

        let Ok(mut frame) = renderer.render(&mut target, size, transform_inv) else {
            warn!("Failed to start rendering for screenshot");
            return None;
        };

        if frame.clear(Color32F::TRANSPARENT, &[output_rect]).is_err() {
            warn!("Failed to clear for screenshot");
            return None;
        }

        for element in elements.iter().rev() {
            let src = element.src();
            let dst = element.geometry(scale);
            if let Some(mut damage) = output_rect.intersection(dst) {
                damage.loc -= dst.loc;
                let blur_cache = if element.is_framebuffer_effect() {
                    Some(smithay::utils::user_data::UserDataMap::new())
                } else {
                    None
                };
                if let Some(ref cache) = blur_cache {
                    let _ = element.capture_framebuffer(&mut frame, src, dst, cache);
                }
                let _ = element.draw(&mut frame, src, dst, &[damage], &[], blur_cache.as_ref());
            }
        }

        if frame.finish().is_err() {
            warn!("Failed to finish rendering for screenshot");
            return None;
        }

        let Ok(mapping) =
            renderer.copy_framebuffer(&target, Rectangle::from_size(target.size()), fourcc)
        else {
            warn!("Failed to copy framebuffer for screenshot");
            return None;
        };

        let Ok(bytes) = renderer.map_texture(&mapping) else {
            warn!("Failed to map texture for screenshot");
            return None;
        };

        return Some(CapturedFrame {
            pixels: bytes.to_vec(),
            width: size.w as u32,
            height: size.h as u32,
        })
    }
}

pub fn run_winit() {
    let mut event_loop = EventLoop::try_new().unwrap();
    let display = Display::new().unwrap();
    let mut display_handle = display.handle();

    #[cfg_attr(not(feature = "egl"), allow(unused_mut))]
    let (mut backend, mut winit) = match winit::init::<GlesRenderer>() {
        Ok(ret) => ret,
        Err(err) => {
            error!("Failed to initialize Winit backend: {}", err);
            return;
        }
    };

    {
        let renderer = backend.renderer();
        crate::render_helpers::shaders::init(renderer);
        crate::render_helpers::custom_shaders::init(renderer);
        crate::render_helpers::resources::init(renderer);
    }

    let size = backend.window_size();

    let mode = Mode {
        size,
        refresh: 60_000,
    };
    let output = Output::new(
        OUTPUT_NAME.to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Smithay".into(),
            model: "Winit".into(),
            serial_number: "Unknown".into(),
        },
    );
    let _global = output.create_global::<AnvilState<WinitData>>(&display.handle());
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    #[cfg(feature = "debug")]
    #[allow(deprecated)]
    let fps_image = image::io::Reader::with_format(
        std::io::Cursor::new(FPS_NUMBERS_PNG),
        image::ImageFormat::Png,
    )
    .decode()
    .unwrap();
    #[cfg(feature = "debug")]
    let fps_texture = backend
        .renderer()
        .import_memory(
            &fps_image.to_rgba8(),
            Fourcc::Abgr8888,
            (fps_image.width() as i32, fps_image.height() as i32).into(),
            false,
        )
        .expect("Unable to upload FPS texture");
    #[cfg(feature = "debug")]
    let mut fps_element = FpsElement::new(fps_texture);

    let render_node = EGLDevice::device_for_display(backend.renderer().egl_context().display())
        .and_then(|device| return device.try_get_render_node());

    let dmabuf_default_feedback = match render_node {
        Ok(Some(node)) => {
            let dmabuf_formats = backend.renderer().dmabuf_formats();
            let dmabuf_default_feedback = DmabufFeedbackBuilder::new(node.dev_id(), dmabuf_formats)
                .build()
                .unwrap();
            Some(dmabuf_default_feedback)
        }
        Ok(None) => {
            warn!("failed to query render node, dmabuf will use v3");
            None
        }
        Err(err) => {
            warn!(?err, "failed to egl device for display, dmabuf will use v3");
            None
        }
    };

    // if we failed to build dmabuf feedback we fall back to dmabuf v3
    // Note: egl on Mesa requires either v4 or wl_drm (initialized with bind_wl_display)
    let dmabuf_state = if let Some(default_feedback) = dmabuf_default_feedback {
        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global = dmabuf_state
            .create_global_with_default_feedback::<AnvilState<WinitData>>(
                &display.handle(),
                &default_feedback,
            );
        (dmabuf_state, dmabuf_global, Some(default_feedback))
    } else {
        let dmabuf_formats = backend.renderer().dmabuf_formats();
        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global =
            dmabuf_state.create_global::<AnvilState<WinitData>>(&display.handle(), dmabuf_formats);
        (dmabuf_state, dmabuf_global, None)
    };

    #[cfg(feature = "egl")]
    if backend
        .renderer()
        .bind_wl_display(&display.handle())
        .is_ok()
    {
        info!("EGL hardware-acceleration enabled");
    };

    let data = {
        let damage_tracker = OutputDamageTracker::from_output(&output);

        WinitData {
            backend,
            damage_tracker,
            dmabuf_state,
            full_redraw: 0,
            #[cfg(feature = "debug")]
            fps: fps_ticker::Fps::default(),
        }
    };
    let mut state = AnvilState::init(display, event_loop.handle(), data, true);
    state
        .shm_state
        .update_formats(state.backend_data.backend.renderer().shm_formats());

    let output_config = state.config.outputs.iter().find(|o| return o.name == OUTPUT_NAME);
    let output_position = output_config.and_then(|o| return o.position).unwrap_or((0, 0));
    let output_scale = output_config.and_then(|o| return o.scale);
    let output_transform =
        output_config
            .and_then(|o| return o.transform.as_deref())
            .and_then(|t| match t {
                "normal" => return Some(Transform::Normal),
                "90" => return Some(Transform::_90),
                "180" => return Some(Transform::_180),
                "270" => return Some(Transform::_270),
                "flipped" => return Some(Transform::Flipped),
                "flipped-90" => return Some(Transform::Flipped90),
                "flipped-180" => return Some(Transform::Flipped180),
                "flipped-270" => return Some(Transform::Flipped270),
                _ => return None,
            });

    if let Some(scale) = output_scale {
        output.change_current_state(
            None,
            None,
            Some(smithay::output::Scale::Fractional(scale)),
            None,
        );
    }
    if let Some(transform) = output_transform {
        output.change_current_state(None, Some(transform), None, None);
    }

    state.space.map_output(&output, output_position);

    #[cfg(feature = "xwayland")]
    state.start_xwayland();

    state.run_init_commands();

    // Start the bakawm-ctl IPC server.
    match crate::ipc::start_ipc_server(&mut state) {
        Ok(path) => info!("IPC socket listening at {}", path.display()),
        Err(err) => warn!("Failed to start IPC server: {}", err),
    }

    if let Some(watcher) = crate::config::spawn_config_watcher(&event_loop.handle()) {
        state.config_watcher = Some(crate::state::ConfigWatcher(watcher));
    }

    info!("Initialization completed, starting the main loop.");

    #[cfg(feature = "systemd")]
    {
        if std::env::var_os("NOTIFY_SOCKET").is_some() {
            let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
        }
    }

    let mut pointer_element = PointerElement::default();

    while state.running.load(Ordering::SeqCst) {
        let status = winit.dispatch_new_events(|event| match event {
            WinitEvent::Resized { size, .. } => {
                // We only have one output
                let Some(output) = state.space.outputs().next().cloned() else {
                    return;
                };
                state.space.map_output(&output, (0, 0));
                let mode = Mode {
                    size,
                    refresh: 60_000,
                };
                output.change_current_state(Some(mode), None, None, None);
                output.set_preferred(mode);
                crate::shell::fixup_positions(&mut state.space, state.pointer.current_location());
            }
            WinitEvent::Input(event) => state.process_input_event_windowed(event, OUTPUT_NAME),
            _ => (),
        });

        if let PumpStatus::Exit(_) = status {
            state.running.store(false, Ordering::SeqCst);
            break;
        }

        // drawing logic
        {
            let now = state.clock.now();
            let frame_target = now
                + output
                    .current_mode()
                    .map(|mode| return Duration::from_secs_f64(1_000f64 / mode.refresh as f64))
                    .unwrap_or_default();
            state.pre_repaint(&output, frame_target);

            let backend = &mut state.backend_data.backend;

            // draw the cursor as relevant
            // reset the cursor if the surface is no longer alive
            let mut reset = false;
            if let CursorImageStatus::Surface(ref surface) = state.cursor_status {
                reset = !surface.alive();
            }
            if reset {
                state.cursor_status = CursorImageStatus::default_named();
            }
            let cursor_visible = !matches!(state.cursor_status, CursorImageStatus::Surface(_));

            pointer_element.set_status(state.cursor_status.clone());

            #[cfg(feature = "debug")]
            let fps = state.backend_data.fps.avg().round() as u32;
            #[cfg(feature = "debug")]
            fps_element.update_fps(fps);

            let full_redraw = &mut state.backend_data.full_redraw;
            *full_redraw = full_redraw.saturating_sub(1);
            let space = &mut state.space;
            // Advance layout animations (move windows toward their targets).
            state.layout.update(space);
            let damage_tracker = &mut state.backend_data.damage_tracker;
            let show_window_preview = state.show_window_preview;

            let dnd_icon = state.dnd_icon.as_ref();

            let scale = Scale::from(output.current_scale().fractional_scale());
            let cursor_hotspot =
                if let CursorImageStatus::Surface(ref surface) = state.cursor_status {
                    compositor::with_states(surface, |states| {
                        return states
                            .data_map
                            .get::<Mutex<CursorImageAttributes>>()
                            .map(|attrs| return attrs.lock().unwrap().hotspot)
                            .unwrap_or_default()
                    })
                } else {
                    (0, 0).into()
                };
            let cursor_pos = state.pointer.current_location();

            #[cfg(feature = "debug")]
            let mut renderdoc = state.renderdoc.as_mut();

            let age = if *full_redraw > 0 {
                0
            } else {
                backend.buffer_age().unwrap_or(0)
            };
            #[cfg(feature = "debug")]
            let window_handle = backend
                .window()
                .window_handle()
                .map(|handle| {
                    if let RawWindowHandle::Wayland(handle) = handle.as_raw() {
                        return handle.surface.as_ptr();
                    } else {
                        return std::ptr::null_mut();
                    }
                })
                .unwrap_or_else(|_| return std::ptr::null_mut());
            let render_res = backend.bind().and_then(|(renderer, mut fb)| {
                #[cfg(feature = "debug")]
                if let Some(renderdoc) = renderdoc.as_mut() {
                    renderdoc.start_frame_capture(
                        renderer.egl_context().get_context_handle(),
                        window_handle,
                    );
                }

                // (Re)compile user-defined custom shaders if the config changed.
                crate::render_helpers::custom_shaders::refresh_if_needed(renderer, &state.config);

                let mut elements = Vec::<CustomRenderElements<GlesRenderer>>::new();

                elements.extend(
                    pointer_element.render_elements(
                        renderer,
                        (cursor_pos - cursor_hotspot.to_f64())
                            .to_physical(scale)
                            .to_i32_round(),
                        scale,
                        1.0,
                    ),
                );

                // draw the dnd icon if any
                if let Some(icon) = dnd_icon {
                    let dnd_icon_pos = (cursor_pos + icon.offset.to_f64())
                        .to_physical(scale)
                        .to_i32_round();
                    if icon.surface.alive() {
                        elements.extend(AsRenderElements::<GlesRenderer>::render_elements(
                            &smithay::desktop::space::SurfaceTree::from_surface(&icon.surface),
                            renderer,
                            dnd_icon_pos,
                            scale,
                            1.0,
                        ));
                    }
                }

                #[cfg(feature = "debug")]
                elements.push(CustomRenderElements::Fps(fps_element.clone()));

                return render_output(
                    &output,
                    space,
                    &state.closing_windows,
                    elements,
                    renderer,
                    &mut fb,
                    damage_tracker,
                    age,
                    show_window_preview,
                    &state.config,
                )
                .map_err(|err| match err {
                    OutputDamageTrackerError::Rendering(err) => return err.into(),
                    // The winit surface always has a mode set, so this cannot occur.
                    err @ OutputDamageTrackerError::OutputNoMode(_) => {
                        return SwapBuffersError::ContextLost(Box::new(err))
                    }
                })
            });

            match render_res {
                Ok(render_output_result) => {
                    let has_rendered = render_output_result.damage.is_some();
                    if let Some(damage) = render_output_result.damage
                        && let Err(err) = backend.submit(Some(damage)) {
                            warn!("Failed to submit buffer: {}", err);
                        }

                    #[cfg(feature = "debug")]
                    if let Some(renderdoc) = renderdoc.as_mut() {
                        renderdoc.end_frame_capture(
                            backend.renderer().egl_context().get_context_handle(),
                            backend
                                .window()
                                .window_handle()
                                .map(|handle| {
                                    if let RawWindowHandle::Wayland(handle) = handle.as_raw() {
                                        return handle.surface.as_ptr();
                                    } else {
                                        return std::ptr::null_mut();
                                    }
                                })
                                .unwrap_or_else(|_| return std::ptr::null_mut()),
                        );
                    }

                    backend.window().set_cursor_visible(cursor_visible);

                    // Extract states first to release the render_output_result borrow
                    // (which holds a reference to damage_tracker -> state)
                    let states = render_output_result.states;

                    // Cleanup finished close animations
                    state.cleanup_finished_close_animations();

                    update_primary_scanout_output(
                        &state.space,
                        &output,
                        &state.dnd_icon,
                        &state.cursor_status,
                        &states,
                    );

                    if has_rendered {
                        let mut output_presentation_feedback =
                            take_presentation_feedback(&output, &state.space, &states);
                        output_presentation_feedback.presented(
                            frame_target,
                            output
                                .current_mode()
                                .map(|mode| {
                                    return Refresh::fixed(Duration::from_secs_f64(
                                        1_000f64 / mode.refresh as f64,
                                    ))
                                })
                                .unwrap_or(Refresh::Unknown),
                            0,
                            wp_presentation_feedback::Kind::Vsync,
                        )
                    }

                    // Send frame events so that client start drawing their next frame
                    state.post_repaint(&output, frame_target, None, &states);

                    if state.pending_screenshot {
                        state.pending_screenshot = false;
                        state.take_screenshot_winit(&output);
                    }
                }
                Err(SwapBuffersError::ContextLost(err)) => {
                    #[cfg(feature = "debug")]
                    if let Some(renderdoc) = renderdoc.as_mut() {
                        renderdoc.discard_frame_capture(
                            backend.renderer().egl_context().get_context_handle(),
                            backend
                                .window()
                                .window_handle()
                                .map(|handle| {
                                    if let RawWindowHandle::Wayland(handle) = handle.as_raw() {
                                        return handle.surface.as_ptr();
                                    } else {
                                        return std::ptr::null_mut();
                                    }
                                })
                                .unwrap_or_else(|_| return std::ptr::null_mut()),
                        );
                    }

                    error!("Critical Rendering Error: {}", err);
                    state.running.store(false, Ordering::SeqCst);
                }
                Err(err) => warn!("Rendering error: {}", err),
            }
        }

        let result = event_loop.dispatch(Some(Duration::from_millis(16)), &mut state);
        if result.is_err() {
            state.running.store(false, Ordering::SeqCst);
        } else {
            state.space.refresh();
            state.popups.cleanup();
            display_handle.flush_clients().unwrap();
        }

        #[cfg(feature = "debug")]
        state.backend_data.fps.tick();
    }
}

impl AnvilState<WinitData> {
    fn take_screenshot_winit(&mut self, output: &Output) {
        use crate::state::{get_screenshot_path, save_screenshot_to_file};

        let pointer_location = self.pointer.current_location();
        let now = self.clock.now();

        let Some(captured) = self.backend_data.capture_screenshot(
            output,
            &self.space,
            pointer_location,
            &self.cursor_status,
            self.show_window_preview,
            &self.config,
            now.into(),
        ) else {
            warn!("Failed to capture screenshot");
            return;
        };

        let path = get_screenshot_path();
        match save_screenshot_to_file(&captured.pixels, captured.width, captured.height, &path) {
            Ok(()) => {
                info!("Screenshot saved to {:?}", path);
            }
            Err(err) => {
                warn!("Failed to save screenshot: {}", err);
            }
        }
    }
}
