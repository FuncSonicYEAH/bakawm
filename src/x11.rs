use std::{
    sync::{Mutex, atomic::Ordering},
    time::Duration,
};

use crate::{
    drawing::*,
    render::*,
    shell::WindowElement,
    state::{AnvilState, Backend, take_presentation_feedback, update_primary_scanout_output},
};
#[cfg(feature = "egl")]
use smithay::backend::renderer::ImportEgl;
#[cfg(feature = "debug")]
use smithay::backend::{allocator::Fourcc, renderer::ImportMem};

use smithay::{
    backend::{
        allocator::{
            dmabuf::{Dmabuf, DmabufAllocator},
            gbm::{GbmAllocator, GbmBufferFlags},
            vulkan::{ImageUsageFlags, VulkanAllocator},
        },
        egl::{EGLContext, EGLDisplay},
        renderer::{
            Bind, ImportDma, ImportMemWl, damage::OutputDamageTracker, element::AsRenderElements,
            gles::GlesRenderer,
        },
        vulkan::{Instance, PhysicalDevice, version::Version},
        x11::{WindowBuilder, X11Backend, X11Event, X11Surface},
    },
    desktop::Space,
    input::{
        keyboard::LedState,
        pointer::{CursorImageAttributes, CursorImageStatus},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        ash::ext,
        calloop::EventLoop,
        gbm,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{Display, protocol::wl_surface},
    },
    utils::{DeviceFd, IsAlive, Logical, Point, Scale, Transform},
    wayland::{
        compositor,
        dmabuf::{
            DmabufFeedback, DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState,
            ImportNotifier,
        },
        presentation::Refresh,
    },
};
use tracing::{error, info, trace, warn};

pub const OUTPUT_NAME: &str = "x11";

#[derive(Debug)]
pub struct X11Data {
    render: bool,
    mode: Mode,
    // FIXME: If GlesRenderer is dropped before X11Surface, then the MakeCurrent call inside Gles2Renderer will
    // fail because the X11Surface is keeping gbm alive.
    renderer: GlesRenderer,
    damage_tracker: OutputDamageTracker,
    surface: X11Surface,
    dmabuf_state: DmabufState,
    _dmabuf_global: DmabufGlobal,
    _dmabuf_default_feedback: DmabufFeedback,
    #[cfg(feature = "debug")]
    fps: fps_ticker::Fps,
}

impl DmabufHandler for AnvilState<X11Data> {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.backend_data.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        if self
            .backend_data
            .renderer
            .import_dmabuf(&dmabuf, None)
            .is_ok()
        {
            let _ = notifier.successful::<AnvilState<X11Data>>();
        } else {
            notifier.failed();
        }
    }
}

impl Backend for X11Data {
    fn seat_name(&self) -> String {
        "x11".to_owned()
    }
    fn reset_buffers(&mut self, _output: &Output) {
        self.surface.reset_buffers();
    }
    fn early_import(&mut self, _surface: &wl_surface::WlSurface) {}
    fn update_led_state(&mut self, _led_state: LedState) {}
    fn reload_cursor(&mut self, _theme: Option<&str>, _size: Option<u32>) {}

    fn queue_redraw(&mut self, _output: &Output) {
        // x11 backend: no-op (continuous rendering)
    }

    fn with_primary_renderer<T>(&mut self, f: impl FnOnce(&mut GlesRenderer) -> T) -> Option<T> {
        // x11 backend doesn't have a GlesRenderer
        let _ = f;
        None
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
            Color32F, ExportMem, Frame, Offscreen, Renderer, Texture,
        };
        use smithay::utils::Rectangle;

        let scale = Scale::from(output.current_scale().fractional_scale());
        let output_transform = output.current_transform();
        let mode = output.current_mode()?;
        let size = mode.size;

        let renderer = &mut self.renderer;

        let output_geometry = space.output_geometry(output)?;

        let mut pointer_element = PointerElement::default();
        let mut custom_elements: Vec<CustomRenderElements<GlesRenderer>> = Vec::new();
        if output_geometry.to_f64().contains(pointer_location) {
            let cursor_hotspot = if let CursorImageStatus::Surface(surface) = cursor_status {
                compositor::with_states(surface, |states| {
                    states
                        .data_map
                        .get::<Mutex<CursorImageAttributes>>()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .hotspot
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
            &[],
            custom_elements,
            renderer,
            show_window_preview,
            config,
        );

        let fourcc = Fourcc::Abgr8888;
        let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);

        let Ok(mut texture): Result<GlesTexture, _> = renderer.create_buffer(fourcc, buffer_size)
        else {
            error!("Failed to create offscreen texture for screenshot");
            return None;
        };

        let Ok(mut target) = renderer.bind(&mut texture) else {
            error!("Failed to bind offscreen texture for screenshot");
            return None;
        };

        let transform_inv = output_transform.invert();
        let output_rect = Rectangle::from_size(transform_inv.transform_size(size));

        let Ok(mut frame) = renderer.render(&mut target, size, transform_inv) else {
            error!("Failed to start rendering for screenshot");
            return None;
        };

        if frame.clear(Color32F::TRANSPARENT, &[output_rect]).is_err() {
            error!("Failed to clear for screenshot");
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
            error!("Failed to finish rendering for screenshot");
            return None;
        }

        let Ok(mapping) =
            renderer.copy_framebuffer(&target, Rectangle::from_size(target.size()), fourcc)
        else {
            error!("Failed to copy framebuffer for screenshot");
            return None;
        };

        let Ok(bytes) = renderer.map_texture(&mapping) else {
            error!("Failed to map texture for screenshot");
            return None;
        };

        Some(CapturedFrame {
            pixels: bytes.to_vec(),
            width: size.w as u32,
            height: size.h as u32,
        })
    }
}

pub fn run_x11() {
    let mut event_loop = EventLoop::try_new().unwrap();
    let display = Display::new().unwrap();
    let mut display_handle = display.handle();

    let backend = X11Backend::new().expect("Failed to initialize X11 backend");
    let handle = backend.handle();

    // Obtain the DRM node the X server uses for direct rendering.
    let (node, fd) = handle
        .drm_node()
        .expect("Could not get DRM node used by X server");

    // Create the gbm device for buffer allocation.
    let device = gbm::Device::new(DeviceFd::from(fd)).expect("Failed to create gbm device");
    // Initialize EGL using the GBM device.
    let egl = unsafe { EGLDisplay::new(device.clone()).expect("Failed to create EGLDisplay") };
    // Create the OpenGL context
    let context = EGLContext::new(&egl).expect("Failed to create EGLContext");

    let window = WindowBuilder::new()
        .title("Anvil")
        .build(&handle)
        .expect("Failed to create first window");

    let skip_vulkan = std::env::var("ANVIL_NO_VULKAN")
        .map(|x| {
            x == "1"
                || x.to_lowercase() == "true"
                || x.to_lowercase() == "yes"
                || x.to_lowercase() == "y"
        })
        .unwrap_or(false);

    let vulkan_allocator = if !skip_vulkan {
        Instance::new(Version::VERSION_1_2, None)
            .ok()
            .and_then(|instance| {
                PhysicalDevice::enumerate(&instance)
                    .ok()
                    .and_then(|devices| {
                        devices
                            .filter(|phd| phd.has_device_extension(ext::physical_device_drm::NAME))
                            .find(|phd| {
                                phd.primary_node().unwrap() == Some(node)
                                    || phd.render_node().unwrap() == Some(node)
                            })
                    })
            })
            .and_then(|physical_device| {
                VulkanAllocator::new(
                    &physical_device,
                    ImageUsageFlags::COLOR_ATTACHMENT | ImageUsageFlags::SAMPLED,
                )
                .ok()
            })
    } else {
        None
    };

    let surface = match vulkan_allocator {
        // Create the surface for the window.
        Some(vulkan_allocator) => handle
            .create_surface(
                &window,
                DmabufAllocator(vulkan_allocator),
                context
                    .dmabuf_render_formats()
                    .iter()
                    .map(|format| format.modifier),
            )
            .expect("Failed to create X11 surface"),
        None => handle
            .create_surface(
                &window,
                DmabufAllocator(GbmAllocator::new(device, GbmBufferFlags::RENDERING)),
                context
                    .dmabuf_render_formats()
                    .iter()
                    .map(|format| format.modifier),
            )
            .expect("Failed to create X11 surface"),
    };

    #[cfg_attr(not(feature = "egl"), allow(unused_mut))]
    let mut renderer =
        unsafe { GlesRenderer::new(context) }.expect("Failed to initialize renderer");

    crate::render_helpers::shaders::init(&mut renderer);
    crate::render_helpers::custom_shaders::init(&mut renderer);
    crate::render_helpers::resources::init(&mut renderer);

    #[cfg(feature = "egl")]
    if renderer.bind_wl_display(&display.handle()).is_ok() {
        info!("EGL hardware-acceleration enabled");
    }

    let dmabuf_formats = renderer.dmabuf_formats();
    let dmabuf_default_feedback = DmabufFeedbackBuilder::new(node.dev_id(), dmabuf_formats)
        .build()
        .unwrap();
    let mut dmabuf_state = DmabufState::new();
    let dmabuf_global = dmabuf_state.create_global_with_default_feedback::<AnvilState<X11Data>>(
        &display.handle(),
        &dmabuf_default_feedback,
    );

    let size = {
        let s = window.size();

        (s.w as i32, s.h as i32).into()
    };

    let mode = Mode {
        size,
        refresh: 60_000,
    };

    #[cfg(feature = "debug")]
    #[allow(deprecated)]
    let fps_image = image::io::Reader::with_format(
        std::io::Cursor::new(FPS_NUMBERS_PNG),
        image::ImageFormat::Png,
    )
    .decode()
    .unwrap();
    #[cfg(feature = "debug")]
    let fps_texture = renderer
        .import_memory(
            &fps_image.to_rgba8(),
            Fourcc::Abgr8888,
            (fps_image.width() as i32, fps_image.height() as i32).into(),
            false,
        )
        .expect("Unable to upload FPS texture");
    #[cfg(feature = "debug")]
    let mut fps_element = FpsElement::new(fps_texture);
    let output = Output::new(
        OUTPUT_NAME.to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Smithay".into(),
            model: "X11".into(),
            serial_number: "Unknown".into(),
        },
    );
    let _global = output.create_global::<AnvilState<X11Data>>(&display.handle());
    output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
    output.set_preferred(mode);

    let damage_tracker = OutputDamageTracker::from_output(&output);

    let data = X11Data {
        render: true,
        mode,
        surface,
        renderer,
        damage_tracker,
        dmabuf_state,
        _dmabuf_global: dmabuf_global,
        _dmabuf_default_feedback: dmabuf_default_feedback,
        #[cfg(feature = "debug")]
        fps: fps_ticker::Fps::default(),
    };

    let mut state = AnvilState::init(display, event_loop.handle(), data, true);
    state
        .shm_state
        .update_formats(state.backend_data.renderer.shm_formats());

    let output_config = state.config.outputs.iter().find(|o| o.name == OUTPUT_NAME);
    let output_position = output_config.and_then(|o| o.position).unwrap_or((0, 0));
    if let Some(output_scale) = output_config.and_then(|o| o.scale) {
        output.change_current_state(
            None,
            None,
            Some(smithay::output::Scale::Fractional(output_scale)),
            None,
        );
    }

    state.space.map_output(&output, output_position);

    let output_clone = output.clone();
    event_loop
        .handle()
        .insert_source(backend, move |event, _, data| match event {
            X11Event::CloseRequested { .. } => {
                data.running.store(false, Ordering::SeqCst);
            }
            X11Event::Resized { new_size, .. } => {
                let output = &output_clone;
                let size = { (new_size.w as i32, new_size.h as i32).into() };

                data.backend_data.mode = Mode {
                    size,
                    refresh: 60_000,
                };
                output.delete_mode(output.current_mode().unwrap());
                output.change_current_state(Some(data.backend_data.mode), None, None, None);
                output.set_preferred(data.backend_data.mode);
                crate::shell::fixup_positions(&mut data.space, data.pointer.current_location());

                data.backend_data.render = true;
            }
            X11Event::PresentCompleted { .. } | X11Event::Refresh { .. } => {
                data.backend_data.render = true;
            }
            X11Event::Input { event, .. } => data.process_input_event_windowed(event, OUTPUT_NAME),
            X11Event::Focus { focused: false, .. } => {
                data.release_all_keys();
            }
            _ => {}
        })
        .expect("Failed to insert X11 Backend into event loop");

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

    let mut pointer_element = PointerElement::default();

    while state.running.load(Ordering::SeqCst) {
        if state.backend_data.render {
            profiling::scope!("render_frame");

            let now = state.clock.now();
            let frame_target = now
                + output
                    .current_mode()
                    .map(|mode| Duration::from_secs_f64(1_000f64 / mode.refresh as f64))
                    .unwrap_or_default();
            state.pre_repaint(&output, frame_target);

            // Advance layout animations (move windows toward their targets).
            state.layout.update(&mut state.space);

            let backend_data = &mut state.backend_data;
            // We need to borrow everything we want to refer to inside the renderer callback otherwise rustc is unhappy.
            #[cfg(feature = "debug")]
            let fps = backend_data.fps.avg().round() as u32;
            #[cfg(feature = "debug")]
            fps_element.update_fps(fps);

            let (mut buffer, age) = backend_data
                .surface
                .buffer()
                .expect("gbm device was destroyed");
            let mut fb = match backend_data.renderer.bind(&mut buffer) {
                Ok(fb) => fb,
                Err(err) => {
                    error!("Error while binding buffer: {}", err);
                    profiling::finish_frame!();
                    continue;
                }
            };

            #[cfg(feature = "debug")]
            if let Some(renderdoc) = state.renderdoc.as_mut() {
                renderdoc.start_frame_capture(
                    backend_data.renderer.egl_context().get_context_handle(),
                    std::ptr::null(),
                );
            }

            // (Re)compile user-defined custom shaders if the config changed.
            crate::render_helpers::custom_shaders::refresh_if_needed(
                &mut backend_data.renderer,
                &state.config,
            );

            let mut elements: Vec<CustomRenderElements<GlesRenderer>> = Vec::new();

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

            let scale = Scale::from(output.current_scale().fractional_scale());
            let cursor_hotspot =
                if let CursorImageStatus::Surface(ref surface) = state.cursor_status {
                    compositor::with_states(surface, |states| {
                        states
                            .data_map
                            .get::<Mutex<CursorImageAttributes>>()
                            .unwrap()
                            .lock()
                            .unwrap()
                            .hotspot
                    })
                } else {
                    (0, 0).into()
                };
            let cursor_pos = state.pointer.current_location();

            pointer_element.set_status(state.cursor_status.clone());
            elements.extend(
                pointer_element.render_elements(
                    &mut backend_data.renderer,
                    (cursor_pos - cursor_hotspot.to_f64())
                        .to_physical(scale)
                        .to_i32_round(),
                    scale,
                    1.0,
                ),
            );

            // draw the dnd icon if any
            if let Some(icon) = state.dnd_icon.as_ref() {
                let dnd_icon_pos = (cursor_pos + icon.offset.to_f64())
                    .to_physical(scale)
                    .to_i32_round();
                if icon.surface.alive() {
                    elements.extend(AsRenderElements::<GlesRenderer>::render_elements(
                        &smithay::desktop::space::SurfaceTree::from_surface(&icon.surface),
                        &mut backend_data.renderer,
                        dnd_icon_pos,
                        scale,
                        1.0,
                    ));
                }
            }

            #[cfg(feature = "debug")]
            elements.push(CustomRenderElements::Fps(fps_element.clone()));

            let render_res = render_output(
                &output,
                &state.space,
                &state.closing_windows,
                elements,
                &mut backend_data.renderer,
                &mut fb,
                &mut backend_data.damage_tracker,
                age as usize,
                state.show_window_preview,
                &state.config,
            );

            match render_res {
                Ok(render_output_result) => {
                    trace!("Finished rendering");
                    let submitted = if let Err(err) = backend_data.surface.submit() {
                        backend_data.surface.reset_buffers();
                        warn!("Failed to submit buffer: {}. Retrying", err);
                        false
                    } else {
                        true
                    };

                    let states = render_output_result.states;

                    update_primary_scanout_output(
                        &state.space,
                        &output,
                        &state.dnd_icon,
                        &state.cursor_status,
                        &states,
                    );

                    #[cfg(feature = "debug")]
                    let rendered = render_output_result.damage.is_some();
                    if render_output_result.damage.is_some() {
                        let mut output_presentation_feedback =
                            take_presentation_feedback(&output, &state.space, &states);
                        output_presentation_feedback.presented(
                            frame_target,
                            output
                                .current_mode()
                                .map(|mode| {
                                    Refresh::fixed(Duration::from_secs_f64(
                                        1_000f64 / mode.refresh as f64,
                                    ))
                                })
                                .unwrap_or(Refresh::Unknown),
                            0,
                            wp_presentation_feedback::Kind::Vsync,
                        )
                    }

                    #[cfg(feature = "debug")]
                    if rendered {
                        if let Some(renderdoc) = state.renderdoc.as_mut() {
                            renderdoc.end_frame_capture(
                                state
                                    .backend_data
                                    .renderer
                                    .egl_context()
                                    .get_context_handle(),
                                std::ptr::null(),
                            );
                        }
                    } else if let Some(renderdoc) = state.renderdoc.as_mut() {
                        renderdoc.discard_frame_capture(
                            state
                                .backend_data
                                .renderer
                                .egl_context()
                                .get_context_handle(),
                            std::ptr::null(),
                        );
                    }

                    state.backend_data.render = !submitted;

                    // Send frame events so that client start drawing their next frame
                    state.post_repaint(&output, frame_target, None, &states);
                }
                Err(err) => {
                    #[cfg(feature = "debug")]
                    if let Some(renderdoc) = state.renderdoc.as_mut() {
                        renderdoc.discard_frame_capture(
                            backend_data.renderer.egl_context().get_context_handle(),
                            std::ptr::null(),
                        );
                    }

                    backend_data.surface.reset_buffers();
                    error!("Rendering error: {}", err);
                    // TODO: convert RenderError into SwapBuffersError and skip temporary (will retry) and panic on ContextLost or recreate
                }
            }

            #[cfg(feature = "debug")]
            state.backend_data.fps.tick();
            window.set_cursor_visible(cursor_visible);
            profiling::finish_frame!();
        }

        let result = event_loop.dispatch(Some(Duration::from_millis(16)), &mut state);
        if result.is_err() {
            state.running.store(false, Ordering::SeqCst);
        } else {
            state.space.refresh();
            state.popups.cleanup();
            display_handle.flush_clients().unwrap();
        }
    }
}
