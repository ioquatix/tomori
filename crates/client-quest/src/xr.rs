// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The headset's OpenXR shell: the instance, session, swapchain, actions and spaces, and the frame loop that reads them into [`crate::scene::headset`]'s plain types and carries out what it decides. Only device calls belong here.

//!
//! It needs an OpenXR runtime and a headset, so it is left out of the coverage gate; see the `[[package.metadata.bake.test.coverage.exclude]]` entries in `Cargo.toml`. The values it reads become plain types, and the choices it makes from what the runtime offers are made, in [`crate::xr_types`].
use crate::{
    controller,
    feedback::Hand,
    foveation::{self, Level, Map},
    frame_timing::{self, Segment},
    graphics::Renderer,
    hands,
    haptics::{self, Haptics, Vibrator},
    log_cadence::{self, Cadence},
    multiview,
    scene::headset::{Actions, Controls, Headset, Joint, Lifecycle, Location, View},
    scene_graphics::{self, Panels},
    vulkan::Gpu,
    xr_types,
};
use anyhow::{Context, Result, ensure};
use openxr as xr;
use std::{
    sync::mpsc,
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tomori_render::scenery::Sceneries;

/// Run against the desktop's active OpenXR runtime, connected as `conversation` and `connection` describe.
#[cfg(not(target_os = "android"))]
pub fn run_desktop(
    mut conversation: crate::conversation::Conversation,
    connection: crate::connection::Connection,
    sceneries: &Sceneries,
) -> Result<()> {
    // `TOMORI_PANEL_LOD`, as the headset's `tomori.panel_lod`: `off` draws every panel whole, to measure what it costs.
    conversation.set_panel_lod(tomori_scene::projection::PanelLod::parse(
        std::env::var("TOMORI_PANEL_LOD").ok().as_deref(),
    ));
    run(
        &(),
        crate::graphics::SAMPLES,
        // `TOMORI_FOVEATION`, as the headset's `tomori.foveation`; a desktop runtime without Meta's foveation draws as before.
        foveation::Request::parse(std::env::var("TOMORI_FOVEATION").ok().as_deref()),
        // `TOMORI_MULTIVIEW`, as the headset's `tomori.multiview`.
        multiview::Request::parse(std::env::var("TOMORI_MULTIVIEW").ok().as_deref()),
        || false,
        conversation,
        connection,
        sceneries,
    )
}

const STEREO: xr::ViewConfigurationType = xr::ViewConfigurationType::PRIMARY_STEREO;
const BLEND: xr::EnvironmentBlendMode = xr::EnvironmentBlendMode::OPAQUE;

/// `samples` is the samples per pixel to draw each eye with, such as [`crate::graphics::SAMPLES`] for multisample anti-aliasing; it falls back to 1 where the GPU cannot. `foveation` is the fixed foveation asked for, where the runtime and GPU can, and `multiview` whether both eyes are drawn in one pass, where the GPU and every scenery can; see `design/rendering.md`.
#[allow(clippy::too_many_arguments)]
pub fn run(
    platform: &impl xr::PlatformInfo,
    samples: u32,
    foveation: foveation::Request,
    multiview: multiview::Request,
    mut destroyed: impl FnMut() -> bool,
    mut conversation: crate::conversation::Conversation,
    mut connection: crate::connection::Connection,
    sceneries: &Sceneries,
) -> Result<()> {
    let started = Instant::now();
    let entry = unsafe { xr::Entry::load(platform) }
        .context("loading OpenXR (is an OpenXR runtime installed?)")?;
    let supported = entry.enumerate_extensions()?;
    log::info!(
        "OpenXR controller model extensions: FB={}, EXT={}, EXT interaction={}",
        supported.fb_render_model,
        supported.ext_render_model,
        supported.ext_interaction_render_model
    );
    log::info!(
        "OpenXR hand tracking extension: {}",
        supported.ext_hand_tracking
    );
    log::info!(
        "OpenXR optional input extensions: simultaneous hands/controllers={}, detached controllers={}",
        supported.meta_simultaneous_hands_and_controllers,
        supported.meta_detached_controllers
    );
    // Whether the headset is worn, from its proximity sensor, where the runtime reports it. Meta's runtime advertises it, but its "removed" event is not always delivered, so the session state is followed too.
    log::info!(
        "OpenXR user presence extension: {}",
        supported.ext_user_presence
    );
    // Vibrations use the core haptic action; these richer waveforms are reported for later stages. See `design/client-logging.md`.
    log::info!(
        "OpenXR haptic extensions: amplitude envelope={}, PCM={}",
        supported.fb_haptic_amplitude_envelope,
        supported.fb_haptic_pcm
    );
    let offered = foveation::Extensions::offered(&supported);
    log::info!(
        "OpenXR foveation extensions: FB={}, configuration={}, swapchain update state={}, Vulkan={}",
        offered.foveation,
        offered.configuration,
        offered.update_state,
        offered.vulkan
    );
    ensure!(
        supported.khr_vulkan_enable2,
        "Runtime does not support XR_KHR_vulkan_enable2"
    );
    let mut extensions = xr::ExtensionSet::default();
    extensions.khr_vulkan_enable2 = true;
    extensions.ext_local_floor = supported.ext_local_floor;
    extensions.fb_render_model = supported.fb_render_model;
    extensions.ext_hand_tracking = supported.ext_hand_tracking;
    extensions.ext_user_presence = supported.ext_user_presence;
    let foveation_extensions = offered.enabled(foveation);
    foveation_extensions.enable(&mut extensions);
    #[cfg(target_os = "android")]
    {
        extensions.khr_android_create_instance = true;
    }
    let instance = entry.create_instance(
        &xr::ApplicationInfo {
            application_name: "tomori",
            application_version: 1,
            engine_name: "wgpu",
            engine_version: 1,
            api_version: xr::Version::new(1, 0, 0),
        },
        &extensions,
        &[],
        platform,
    )?;
    log::info!("OpenXR runtime: {:?}", instance.properties()?);
    // Each step of startup can take a while; between them, acknowledge the activity's lifecycle, which NativeActivity waits for on its main thread, so that a pause as the launch settles is acknowledged promptly between steps, though not during one. How long each took is logged below. See `design/arrival.md`.
    let mut steps = xr_types::StartupSteps::new(started);
    steps.done("instance");
    if destroyed() {
        return Ok(());
    }
    let system = instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)?;
    let views = instance.enumerate_view_configuration_views(system, STEREO)?;
    ensure!(views.len() == 2, "Expected a stereo headset");
    ensure!(
        instance
            .enumerate_environment_blend_modes(system, STEREO)?
            .contains(&BLEND),
        "Runtime must support opaque VR"
    );
    let (width, height) = xr_types::eye_size(&views);
    // Declaration order matters: all XR session resources are dropped before
    // the wgpu-owned Vulkan device and instance.
    // The density map's device extension is enabled only for a runtime that can foveate, so that others' devices are as before.
    // Multiview is enabled only where asked for, so that a device drawing each eye in a pass of its own is as before.
    let gpu = Gpu::new(
        &instance,
        system,
        foveation_extensions.maps(),
        multiview.asks(),
    )?;
    let plan = foveation::Plan::new(foveation, gpu.density, foveation_extensions);
    if let Err(why) = plan {
        log::info!("Fixed foveation off: {why}");
    }
    steps.done("GPU");
    if destroyed() {
        return Ok(());
    }
    let (session, mut waiter, mut stream) =
        unsafe { instance.create_session::<xr::Vulkan>(system, &gpu.binding) }?;
    let mut input = Input::new(&instance, &session)?;
    let hand_tracking = HandTracking::new(&instance, &session, system, supported.ext_hand_tracking);
    let spaces = session.enumerate_reference_spaces()?;
    let (space_type, offset) = xr_types::reference_space(supported.ext_local_floor, &spaces);
    let space = session.create_reference_space(space_type, offset)?;
    log::info!("Recenterable reference space: {space_type:?}");
    let (vk_format, format) = xr_types::swapchain_format(&session.enumerate_swapchain_formats()?)
        .context("runtime has no supported sRGB color format")?;
    let swapchain_info = xr::SwapchainCreateInfo {
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT,
        format: vk_format.as_raw() as u32,
        sample_count: 1,
        width,
        height,
        face_count: 1,
        array_size: 2,
        mip_count: 1,
    };
    // Made for the runtime's density maps, at the level asked for, where it can give them; a runtime that cannot gives none, and Tomori's own is used.
    let (mut swapchain, runtime_maps) = match plan {
        Ok(plan) if plan.runtime => match foveated_swapchain(&session, &swapchain_info, plan.level)
        {
            Ok(made) => made,
            Err(error) => {
                log::warn!("Could not foveate the swapchain: {error:#}");
                (session.create_swapchain(&swapchain_info)?, None)
            }
        },
        _ => (session.create_swapchain(&swapchain_info)?, None),
    };
    let images = gpu.import_images(&swapchain, format, width, height)?;
    steps.done("session");
    if destroyed() {
        return Ok(());
    }
    let samples = crate::graphics::supported_samples(&gpu.adapter, format, samples);
    // Each eye's scene pass resolves its samples into the swapchain image before it ends, so the multisampled colour and depth stay in tile memory where the GPU allows, as transient attachments; see `design/desktop.md`.
    log::info!(
        "Drawing {samples}x multisampled; transient attachments save memory: {:?}",
        gpu.adapter.get_info().transient_saves_memory
    );
    // Before the renderer makes its pipelines, so that they are made against the eyes' passes with their density map.
    let map = plan.ok().and_then(|plan| plan.map(runtime_maps.is_some()));
    let mut foveated = map.and_then(|map| {
        let level = foveation.level;
        gpu.foveate(
            level,
            map,
            &images,
            runtime_maps.as_ref(),
            vk_format,
            samples,
            width,
            height,
        )
        .inspect_err(|error| log::warn!("Could not foveate the eyes: {error:#}"))
        .ok()
    });
    let foveation_summary = foveation::summary(
        foveation.level,
        foveated.as_ref().map(|foveated| foveated.map),
    );
    if plan.is_ok() {
        log::info!(
            "Fixed {foveation_summary}{}",
            runtime_maps
                .as_ref()
                .filter(|_| map == Some(Map::Runtime))
                .map(|maps| format!(", {}x{} texels", maps.size.width, maps.size.height))
                .unwrap_or_default()
        );
    }
    // Both eyes in one pass, by multiview, where asked for and the GPU and every scenery can; the density map, attached
    // to the eyes' passes whichever they are, is the eyes', each at its own layer.
    let mut renderer = Renderer::with_views(
        &gpu.device,
        &gpu.queue,
        format,
        width,
        height,
        samples,
        sceneries,
        multiview.views(),
    );
    let eye_passes = renderer.views();
    let views_summary = multiview::summary(multiview, eye_passes);
    match renderer.single_view() {
        Some(why) => log::info!("Drawing {views_summary}: {why}"),
        None => log::info!("Drawing {views_summary}"),
    }
    let order = multiview.order();
    steps.done("renderer");
    log::info!("{steps}");
    if destroyed() {
        return Ok(());
    }
    let mut controller_models =
        ControllerModels::new(supported.fb_render_model, &instance, &session);
    // What is left of the floating panel in a space: the status pill. See `design/interaction.md`.
    let mut panels = Panels::new(&gpu.device, &renderer);
    let mut headset = Headset::new(&mut conversation, connection.feedback(), Instant::now());
    // Runs on every return/unwind, before imported image wrappers and the XR
    // swapchain are dropped, including if a frame fails after queue submission.
    let _gpu_idle = WaitForGpu(&gpu.device);
    let mut event_buffer = xr::EventDataBuffer::new();
    let mut frames = 0_u64;
    let mut frame_log = Cadence::new(log_cadence::FRAME_SUMMARY);
    // What each frame costs this thread, summarized beside the frame log; see `design/rendering.md`.
    let mut frame_times = frame_timing::FrameTimes::default();
    // Whether panels too small to read are drawn as their titles, said as the headset starts and in each summary, so that frames with it and without are told apart from the log alone.
    log::info!("{}", conversation.panel_lod().describe());
    frame_times.set_panel_lod(conversation.panel_lod());
    let mut refreshes = xr_types::Refreshes::default();
    // The last frame's rendering, which may still be on the GPU while this frame's work is done on the CPU.
    let mut rendering: Option<wgpu::SubmissionIndex> = None;
    // Drawing new glyphs, such as a first paragraph of Japanese, costs this thread at most about 1.5 ms a frame; the rest are drawn over the frames after, and the log reports what each frame spent. See `design/text.md`.
    tomori_render::glyphs::budget(tomori_render::glyphs::Budget::FRAME);
    log::info!(
        "Ready: {width}x{height} per eye, {} swapchain images. Either trigger: choose what its ray points at, or attract; A: choose.",
        images.len()
    );
    'main: loop {
        if destroyed() {
            break;
        }
        while let Some(event) = instance.poll_event(&mut event_buffer)? {
            match event {
                xr::Event::SessionStateChanged(event) => {
                    let state = event.state();
                    log::info!("XR state: {state:?}");
                    let lifecycle = headset.session(
                        xr_types::session_state(state),
                        &mut conversation,
                        Instant::now(),
                    );
                    match lifecycle {
                        Lifecycle::Begin => {
                            session.begin(STEREO)?;
                        }
                        Lifecycle::End => {
                            session.end()?;
                        }
                        Lifecycle::Exit => break 'main,
                        Lifecycle::Continue => {}
                    }
                }
                xr::Event::InstanceLossPending(_) => break 'main,
                xr::Event::UserPresenceChangedEXT(change)
                    if change.session() == session.as_raw() =>
                {
                    headset.worn(change.is_user_present(), &mut conversation);
                }
                xr::Event::InteractionProfileChanged(_) => {
                    input.log_interaction_profiles(&instance, &session);
                }
                xr::Event::ReferenceSpaceChangePending(change)
                    if change.session() == session.as_raw()
                        && change.reference_space_type() == space_type =>
                {
                    // Applying the change is logged; the runtime can repeat it.
                    log::debug!(
                        "Reference space change pending at {:?}",
                        change.change_time()
                    );
                    headset.recentre(change.change_time().as_nanos());
                }
                _ => {}
            }
        }
        let mut timer = frame_timing::Timer::start();
        timer.time(Segment::Poll, || {
            connection.poll(headset.focused() && headset.running(), &mut conversation)
        });
        headset.idle(&mut conversation);
        if !headset.running() {
            refreshes.pause();
            microphone(&mut headset, &mut connection, &mut conversation, false);
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        controller_models.refresh(&gpu.device, &gpu.queue, &mut renderer);
        let frame = timer.time(Segment::DisplayWait, || waiter.wait())?;
        let waited = Instant::now();
        refreshes.frame(
            frame.predicted_display_time.as_nanos(),
            frame.predicted_display_period.as_nanos(),
        );
        timer.time(Segment::Runtime, || stream.begin())?;
        let now = Instant::now();
        let dt = headset.tick(now);
        let focused = headset.focused();
        let mut controls = timer.time(Segment::Input, || {
            let controls = input.sample_actions(&session, focused);
            if let Ok(controls) = &controls {
                headset.buttons(controls, &mut conversation, connection.feedback(), now);
            }
            controls
        })?;
        if !frame.should_render {
            stream.end(frame.predicted_display_time, BLEND, &[])?;
            microphone(&mut headset, &mut connection, &mut conversation, false);
            continue;
        }
        let (flags, located) = timer.time(Segment::Locate, || {
            session.locate_views(STEREO, frame.predicted_display_time, &space)
        })?;
        let views: [View; 2] = match located.as_slice() {
            [left, right]
                if flags.contains(
                    xr::ViewStateFlags::POSITION_VALID | xr::ViewStateFlags::ORIENTATION_VALID,
                ) =>
            {
                [xr_types::view(left), xr_types::view(right)]
            }
            _ => {
                stream.end(frame.predicted_display_time, BLEND, &[])?;
                microphone(&mut headset, &mut connection, &mut conversation, false);
                continue;
            }
        };
        // Tomori's own density map is shaped about where each eye looks straight ahead, once they are first known.
        if let Some(foveated) = &mut foveated
            && let Err(error) = foveated.shape(&views)
        {
            log::warn!("Could not shape the density map: {error:#}");
        }
        let hands = timer.time(Segment::Input, || {
            if focused {
                input.sample_poses(
                    &session,
                    &space,
                    frame.predicted_display_time,
                    &mut controls,
                );
                hand_tracking.sample(&space, frame.predicted_display_time)
            } else {
                [None, None]
            }
        });
        let stepping = Instant::now();
        let drawn = headset.update(
            &views,
            frame.predicted_display_time.as_nanos(),
            &controls,
            hands,
            &mut conversation,
            connection.feedback(),
            now,
            dt,
        );
        timer.add(Segment::Step, stepping.elapsed());
        let uploads = timer.time(Segment::Draw, || {
            let uploads = scene_graphics::draw(
                &mut renderer,
                &gpu.device,
                &gpu.queue,
                &mut panels,
                &drawn.frame,
            );
            renderer.update_controllers(&gpu.queue, drawn.controllers, drawn.hands);
            uploads
        });
        timer.meshes(uploads.meshes());
        // Every source is heard from where it is, relative to the head; see `design/spatial-audio.md`.
        timer.time(Segment::Audio, || {
            tomori_scene::spatial_audio::listen(
                &mut connection,
                drawn.frame.listener,
                drawn.frame.agent,
                &conversation,
            )
        });
        // Report this participant's head, controllers and hands to the server, so the others in the space can render them.
        if let Some(presence) = &drawn.frame.presence {
            timer.time(Segment::Presence, || presence.send(&mut connection));
        }
        let (scene_to_space, projections) = (drawn.scene_to_space, drawn.projections);
        let vibrator = XrVibrator {
            action: &input.haptic,
            session: &session,
            hands: &input.hand_paths,
        };
        timer.time(Segment::Haptics, || {
            for (hand, haptic) in headset.haptics(connection.feedback(), now) {
                input.haptics.apply(&vibrator, hand, haptic);
            }
        });
        let image = timer.time(Segment::ImageWait, || -> Result<usize> {
            let image = swapchain.acquire_image()? as usize;
            swapchain.wait_image(xr::Duration::INFINITE)?;
            Ok(image)
        })?;
        // Each eye's pass in the order asked for, or one pass drawing both.
        let ordered = [projections[order[0]], projections[order[1]]];
        let swapped: [wgpu::TextureView; 2];
        let targets = if order == [0, 1] {
            images[image].targets(eye_passes)
        } else {
            swapped = order.map(|eye| images[image].views[eye].clone());
            &swapped
        };
        let encoded = timer.time(Segment::Render, || {
            renderer.encode(
                &gpu.device,
                &gpu.queue,
                targets,
                &ordered,
                scene_to_space,
                &panels.list(scene_to_space),
            )
        });
        let (drawn_panels, culled_panels) = renderer.panels_seen();
        timer.panels(drawn_panels, culled_panels);
        // The frame's budget is the display's period for the CPU's work and the GPU's each, rather than for both one after the other: the last frame's rendering has had all of this frame's CPU work, its eyes' draws encoded too, to finish in, and is waited for only to submit this frame's, so that at most one frame is on the GPU at once and the GPU waits for no more than the submission. A frame later than the display's refresh is shown again turned but not moved, so near things shake as the head moves; see `design/rendering.md`.
        if let Some(previous) = rendering.take() {
            timer
                .time(Segment::GpuWait, || {
                    gpu.device.poll(wgpu::PollType::Wait {
                        submission_index: Some(previous),
                        timeout: None,
                    })
                })
                .context("waiting for the last frame's XR rendering")?;
        }
        rendering = Some(timer.time(Segment::Submit, || renderer.submit(&gpu.queue, encoded)));
        // The image goes back to XR once its commands are submitted to the session's queue, which the runtime's composition waits on, rather than once the GPU has finished them; rendering leaves it COLOR_ATTACHMENT_OPTIMAL, as the OpenXR Vulkan swapchain contract requires.
        let released = Instant::now();
        swapchain.release_image()?;
        let projection_views: Vec<_> = located
            .iter()
            .enumerate()
            .map(|(eye, view)| {
                xr::CompositionLayerProjectionView::new()
                    .pose(view.pose)
                    .fov(view.fov)
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&swapchain)
                            .image_array_index(eye as u32)
                            .image_rect(xr::Rect2Di {
                                offset: xr::Offset2Di { x: 0, y: 0 },
                                extent: xr::Extent2Di {
                                    width: width as i32,
                                    height: height as i32,
                                },
                            }),
                    )
            })
            .collect();
        stream.end(
            frame.predicted_display_time,
            BLEND,
            &[&xr::CompositionLayerProjection::new()
                .space(&space)
                .views(&projection_views)],
        )?;
        timer.add(Segment::Runtime, released.elapsed());
        timer.add(Segment::Frame, waited.elapsed());
        frames += 1;
        timer.time(Segment::Microphone, || {
            microphone(&mut headset, &mut connection, &mut conversation, true)
        });
        frame_times.record(timer.finish());
        // Once at the first frame, then a summary at most once a minute: per-frame lines would push lifecycle events out of logcat within minutes.
        if let Some(due) = frame_log.record(Instant::now()) {
            log::info!(
                "{}",
                xr_types::frame_summary(frames, &due, refreshes.take())
            );
            // What those frames cost this thread, beside their rate; see `design/rendering.md`.
            // And the longest part of polling the connection, which says what a long poll was.
            if let Some(summary) = frame_times.summary() {
                let slowest = connection
                    .slowest()
                    .map(|slowest| format!("; slowest poll: {slowest}"));
                log::info!("{summary}{}", slowest.unwrap_or_default());
            }
            // And what they cost the GPU, each eye's pass, and what they drew.
            // With the foveation they were drawn at, and how many of the eyes' framebuffers were given their density map.
            if let Some(gpu) = renderer.gpu_summary() {
                let framebuffers = foveated
                    .as_ref()
                    .map(|foveated| foveated.framebuffers())
                    .map(|(eyes, other)| format!(" ({eyes} framebuffers, {other} others)"))
                    .unwrap_or_default();
                log::info!("{gpu}; {views_summary}; {foveation_summary}{framebuffers}");
            }
        }
    }
    if headset.running() && session.request_exit().is_ok() {
        // xrEndSession is legal only after the runtime enters STOPPING.
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if let Some(xr::Event::SessionStateChanged(event)) =
                instance.poll_event(&mut event_buffer)?
                && event.state() == xr::SessionState::STOPPING
            {
                session.end()?;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

/// The eyes' swapchain made for the runtime's density maps, with its profile at `level` applied, static rather than dynamic, and the maps it then gives, if it gives one for every image.
fn foveated_swapchain(
    session: &xr::Session<xr::Vulkan>,
    info: &xr::SwapchainCreateInfo<xr::Vulkan>,
    level: Level,
) -> Result<(
    xr::Swapchain<xr::Vulkan>,
    Option<crate::vulkan::RuntimeMaps>,
)> {
    use xr::sys::{self, Handle as _};
    let instance = session.instance();
    let fb = instance
        .exts()
        .fb_foveation
        .as_ref()
        .context("XR_FB_foveation")?;
    let update = instance
        .exts()
        .fb_swapchain_update_state
        .as_ref()
        .context("XR_FB_swapchain_update_state")?;
    let foveation = sys::SwapchainCreateInfoFoveationFB {
        ty: sys::SwapchainCreateInfoFoveationFB::TYPE,
        next: std::ptr::null_mut(),
        flags: sys::SwapchainCreateFoveationFlagsFB::FRAGMENT_DENSITY_MAP,
    };
    let raw = sys::SwapchainCreateInfo {
        ty: sys::SwapchainCreateInfo::TYPE,
        next: (&raw const foveation).cast(),
        create_flags: info.create_flags,
        usage_flags: info.usage_flags,
        format: info.format as i64,
        sample_count: info.sample_count,
        width: info.width,
        height: info.height,
        face_count: info.face_count,
        array_size: info.array_size,
        mip_count: info.mip_count,
    };
    let mut handle = sys::Swapchain::NULL;
    check(unsafe { (instance.fp().create_swapchain)(session.as_raw(), &raw, &mut handle) })
        .context("xrCreateSwapchain")?;
    // SAFETY: made just now for this session.
    let swapchain = unsafe { xr::Swapchain::from_raw(session.clone(), handle) };
    let level_info = sys::FoveationLevelProfileCreateInfoFB {
        ty: sys::FoveationLevelProfileCreateInfoFB::TYPE,
        next: std::ptr::null_mut(),
        level: level.xr(),
        vertical_offset: 0.0,
        dynamic: sys::FoveationDynamicFB::DISABLED,
    };
    let profile_info = sys::FoveationProfileCreateInfoFB {
        ty: sys::FoveationProfileCreateInfoFB::TYPE,
        next: (&raw const level_info).cast_mut().cast(),
    };
    let mut profile = sys::FoveationProfileFB::NULL;
    let applied = check(unsafe {
        (fb.create_foveation_profile)(session.as_raw(), &profile_info, &mut profile)
    })
    .context("xrCreateFoveationProfileFB")
    .and_then(|()| {
        let state = sys::SwapchainStateFoveationFB {
            ty: sys::SwapchainStateFoveationFB::TYPE,
            next: std::ptr::null_mut(),
            flags: sys::SwapchainStateFoveationFlagsFB::EMPTY,
            profile,
        };
        let result =
            unsafe { (update.update_swapchain)(swapchain.as_raw(), (&raw const state).cast()) };
        // The swapchain keeps its state once updated, so the profile is no longer needed.
        unsafe { (fb.destroy_foveation_profile)(profile) };
        check(result).context("xrUpdateSwapchainFB")
    });
    if let Err(error) = applied {
        log::warn!("Could not apply the foveation profile: {error:#}");
        return Ok((swapchain, None));
    }
    let maps = crate::vulkan::runtime_maps(&swapchain).unwrap_or_else(|error| {
        log::warn!("Could not read the runtime's density maps: {error:#}");
        None
    });
    if maps.is_none() {
        log::info!("The runtime gave no density map for the swapchain's images");
    }
    Ok((swapchain, maps))
}

fn check(result: xr::sys::Result) -> Result<()> {
    if result.into_raw() < 0 {
        Err(anyhow::anyhow!("{result}"))
    } else {
        Ok(())
    }
}

/// After a turn of the frame loop, ask for the microphone or check its answer, if it is time; the platform's dialog is answered later, so this never waits for it.
fn microphone(
    headset: &mut Headset,
    connection: &mut crate::connection::Connection,
    conversation: &mut crate::conversation::Conversation,
    presented: bool,
) {
    if headset.microphone_due(presented, Instant::now()) {
        let allowed = connection.microphone_permission();
        headset.microphone_answered(allowed, conversation, Instant::now());
    }
}

/// Optional hand-joint tracking. Invalid or inactive joints are hidden, and
/// losing the extension never stops controller input or the XR session.
struct HandTracking {
    trackers: [Option<xr::HandTracker>; 2],
}

impl HandTracking {
    fn new(
        instance: &xr::Instance,
        session: &xr::Session<xr::Vulkan>,
        system: xr::SystemId,
        enabled: bool,
    ) -> Self {
        if !enabled {
            log::info!("OpenXR hand tracking unavailable; controllers remain active");
            return Self {
                trackers: [None, None],
            };
        }
        match instance.supports_hand_tracking(system) {
            Ok(true) => {}
            Ok(false) => {
                log::info!("Headset reports no hand tracking support");
                return Self {
                    trackers: [None, None],
                };
            }
            Err(error) => {
                log::warn!("Could not query hand tracking support: {error}");
                return Self {
                    trackers: [None, None],
                };
            }
        }
        let trackers =
            [xr::Hand::LEFT, xr::Hand::RIGHT].map(|hand| match session.create_hand_tracker(hand) {
                Ok(tracker) => Some(tracker),
                Err(error) => {
                    log::warn!("Could not create {hand:?} hand tracker: {error}");
                    None
                }
            });
        log::info!(
            "Hand tracking initialized: left={}, right={}",
            trackers[0].is_some(),
            trackers[1].is_some()
        );
        Self { trackers }
    }

    /// Each hand's joints in the reference space, where it is tracked.
    fn sample(
        &self,
        space: &xr::Space,
        time: xr::Time,
    ) -> [Option<[Joint; hands::JOINT_COUNT]>; 2] {
        std::array::from_fn(|hand| {
            let tracker = self.trackers[hand].as_ref()?;
            let joints = match space.locate_hand_joints(tracker, time) {
                Ok(joints) => joints?,
                Err(error) => {
                    log::debug!("Hand tracking unavailable for frame: {error}");
                    return None;
                }
            };
            Some(xr_types::joints(&joints))
        })
    }
}

/// Quest's runtime may not expose controller assets, or may not have a model
/// until controllers are active. Never let optional model loading stop XR.
struct ControllerModels {
    next_attempt: Instant,
    requests: Option<mpsc::SyncSender<()>>,
    updates: mpsc::Receiver<(usize, controller::Model)>,
    worker: Option<JoinHandle<()>>,
}

impl ControllerModels {
    fn new(enabled: bool, instance: &xr::Instance, session: &xr::Session<xr::Vulkan>) -> Self {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (update_tx, update_rx) = mpsc::sync_channel(2);
        let worker = if enabled {
            let instance = instance.clone();
            let session = session.clone();
            match std::thread::Builder::new()
                .name("controller-models".into())
                .spawn(move || {
                    let mut keys = [None, None];
                    while request_rx.recv().is_ok() {
                        load_controller_models(&instance, &session, &update_tx, &mut keys);
                    }
                }) {
                Ok(worker) => Some(worker),
                Err(error) => {
                    log::warn!("Could not start controller model loader: {error}");
                    None
                }
            }
        } else {
            log::info!("XR_FB_render_model unavailable; using fallback controller geometry");
            None
        };
        Self {
            next_attempt: Instant::now(),
            requests: worker.as_ref().map(|_| request_tx),
            updates: update_rx,
            worker,
        }
    }

    fn refresh(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, renderer: &mut Renderer) {
        for (hand, model) in self.updates.try_iter() {
            renderer.set_controller_model(device, queue, hand, &model);
        }
        if Instant::now() >= self.next_attempt {
            self.next_attempt = Instant::now() + Duration::from_secs(5);
            if let Some(requests) = &self.requests {
                let _ = requests.try_send(());
            }
        }
    }
}

impl Drop for ControllerModels {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn load_controller_models(
    instance: &xr::Instance,
    session: &xr::Session<xr::Vulkan>,
    updates: &mpsc::SyncSender<(usize, controller::Model)>,
    keys: &mut [Option<xr::RenderModelKeyFB>; 2],
) {
    let paths = match session.enumerate_render_model_paths_fb() {
        Ok(paths) => paths,
        Err(error) => {
            log::warn!("Could not enumerate controller models: {error}");
            return;
        }
    };
    for path in paths {
        let Ok(name) = instance.path_to_string(path) else {
            continue;
        };
        let Some(hand) = xr_types::controller_hand(&name) else {
            continue;
        };
        let model = (|| -> Result<()> {
            let properties = session.get_render_model_properties_fb(
                path,
                xr::RenderModelFlagsFB::SUPPORTS_GLTF_2_0_SUBSET_1,
            )?;
            if properties.model_key == xr::RenderModelKeyFB::NULL
                || keys[hand] == Some(properties.model_key)
            {
                return Ok(());
            }
            let bytes = session.load_render_model_fb(properties.model_key)?;
            let model = match controller::parse_glb(&bytes) {
                Ok(model) => model,
                Err(error) => {
                    // Model contents are stable for a key. Don't repeatedly
                    // decode an unsupported texture or malformed asset.
                    keys[hand] = Some(properties.model_key);
                    log::warn!(
                        "Could not decode {name}: {error:#}; using bundled controller model"
                    );
                    return Ok(());
                }
            };
            let count = model.vertices.len();
            if updates.try_send((hand, model)).is_ok() {
                keys[hand] = Some(properties.model_key);
                log::info!(
                    "Loaded runtime controller model {} ({count} vertices; static geometry)",
                    properties.model_name
                );
            }
            Ok(())
        })();
        if let Err(error) = model {
            log::warn!("Could not load {name}: {error:#}; using fallback geometry");
        }
    }
}

struct WaitForGpu<'a>(&'a wgpu::Device);
impl Drop for WaitForGpu<'_> {
    fn drop(&mut self) {
        let _ = self.0.poll(wgpu::PollType::wait_indefinitely());
    }
}

struct Input {
    grip_space: xr::Space,
    grip: xr::Action<xr::Posef>,
    left_grip_space: xr::Space,
    left_grip: xr::Action<xr::Posef>,
    /// The right controller's pointing pose, whose forward axis is the ray.
    aim_space: xr::Space,
    aim: xr::Action<xr::Posef>,
    trigger: xr::Action<f32>,
    /// The left controller's pointing pose, trigger and grip, which act on Tomori's own things as the right's do.
    left_aim_space: xr::Space,
    left_aim: xr::Action<xr::Posef>,
    left_trigger: xr::Action<f32>,
    left_squeeze: xr::Action<f32>,
    reset: xr::Action<bool>,
    talk: xr::Action<bool>,
    back: xr::Action<bool>,
    refresh: xr::Action<bool>,
    navigation: xr::Action<xr::Vector2f>,
    presentation_navigation: xr::Action<xr::Vector2f>,
    /// The left stick's click, which toggles the live microphone.
    live_toggle: xr::Action<bool>,
    replay: xr::Action<bool>,
    /// The right grip, which detaches the right wrist's transcript into the world and reattaches it.
    transcript_grip: xr::Action<f32>,
    /// The haptic output action, for both hands.
    haptic: xr::Action<xr::Haptic>,
    haptics: Haptics<xr::sys::Result>,
    set: xr::ActionSet,
    hand_paths: [xr::Path; 2],
    location_errors: [Option<xr::sys::Result>; 2],
    diagnostic_state: Option<(bool, [bool; 2])>,
    last_diagnostic: Instant,
}

impl Input {
    fn new(instance: &xr::Instance, session: &xr::Session<xr::Vulkan>) -> Result<Self> {
        let set = instance.create_action_set("swarm", "Swarm", 0)?;
        let grip = set.create_action::<xr::Posef>("right_grip", "Right controller", &[])?;
        let left_grip = set.create_action::<xr::Posef>("left_grip_pose", "Left controller", &[])?;
        let aim = set.create_action::<xr::Posef>("right_aim", "Point", &[])?;
        let trigger = set.create_action::<f32>("attract", "Attract agents or choose", &[])?;
        let left_aim =
            set.create_action::<xr::Posef>("left_aim", "Point with the left hand", &[])?;
        let left_trigger =
            set.create_action::<f32>("left_choose", "Choose with the left hand", &[])?;
        let left_squeeze =
            set.create_action::<f32>("left_take", "Take hold with the left hand", &[])?;
        let reset = set.create_action::<bool>("reset", "Reset swarm", &[])?;
        let talk = set.create_action::<bool>("talk", "Hold to speak", &[])?;
        let back = set.create_action::<bool>("chat_back", "Back or deny", &[])?;
        let refresh = set.create_action::<bool>("chat_refresh", "Browse sessions", &[])?;
        let navigation = set.create_action::<xr::Vector2f>(
            "chat_navigation",
            "Browse messages and sessions",
            &[],
        )?;
        let presentation_navigation =
            set.create_action::<xr::Vector2f>("result_pages", "Browse result cards", &[])?;
        let live_toggle =
            set.create_action::<bool>("live_toggle", "Toggle the live microphone", &[])?;
        let replay = set.create_action::<bool>("replay", "Replay spoken reply", &[])?;
        let transcript_grip =
            set.create_action::<f32>("transcript_grip", "Detach or reattach the transcript", &[])?;
        let hand_paths = [
            instance.string_to_path("/user/hand/left")?,
            instance.string_to_path("/user/hand/right")?,
        ];
        let haptic = set.create_action::<xr::Haptic>("haptic", "Vibrate", &hand_paths)?;
        instance.suggest_interaction_profile_bindings(
            instance.string_to_path("/interaction_profiles/oculus/touch_controller")?,
            &[
                xr::Binding::new(
                    &grip,
                    instance.string_to_path("/user/hand/right/input/grip/pose")?,
                ),
                xr::Binding::new(
                    &left_grip,
                    instance.string_to_path("/user/hand/left/input/grip/pose")?,
                ),
                xr::Binding::new(
                    &aim,
                    instance.string_to_path("/user/hand/right/input/aim/pose")?,
                ),
                xr::Binding::new(
                    &trigger,
                    instance.string_to_path("/user/hand/right/input/trigger/value")?,
                ),
                xr::Binding::new(
                    &left_aim,
                    instance.string_to_path("/user/hand/left/input/aim/pose")?,
                ),
                xr::Binding::new(
                    &left_trigger,
                    instance.string_to_path("/user/hand/left/input/trigger/value")?,
                ),
                xr::Binding::new(
                    &left_squeeze,
                    instance.string_to_path("/user/hand/left/input/squeeze/value")?,
                ),
                xr::Binding::new(
                    &reset,
                    instance.string_to_path("/user/hand/right/input/a/click")?,
                ),
                xr::Binding::new(
                    &talk,
                    instance.string_to_path("/user/hand/left/input/x/click")?,
                ),
                xr::Binding::new(
                    &back,
                    instance.string_to_path("/user/hand/right/input/b/click")?,
                ),
                xr::Binding::new(
                    &refresh,
                    instance.string_to_path("/user/hand/left/input/y/click")?,
                ),
                xr::Binding::new(
                    &navigation,
                    instance.string_to_path("/user/hand/left/input/thumbstick")?,
                ),
                xr::Binding::new(
                    &presentation_navigation,
                    instance.string_to_path("/user/hand/right/input/thumbstick")?,
                ),
                xr::Binding::new(
                    &live_toggle,
                    instance.string_to_path("/user/hand/left/input/thumbstick/click")?,
                ),
                xr::Binding::new(
                    &replay,
                    instance.string_to_path("/user/hand/right/input/thumbstick/click")?,
                ),
                xr::Binding::new(
                    &transcript_grip,
                    instance.string_to_path("/user/hand/right/input/squeeze/value")?,
                ),
                xr::Binding::new(
                    &haptic,
                    instance.string_to_path("/user/hand/left/output/haptic")?,
                ),
                xr::Binding::new(
                    &haptic,
                    instance.string_to_path("/user/hand/right/output/haptic")?,
                ),
            ],
        )?;
        session.attach_action_sets(&[&set])?;
        let grip_space = grip.create_space(session, xr::Path::NULL, xr::Posef::IDENTITY)?;
        let left_grip_space =
            left_grip.create_space(session, xr::Path::NULL, xr::Posef::IDENTITY)?;
        let aim_space = aim.create_space(session, xr::Path::NULL, xr::Posef::IDENTITY)?;
        let left_aim_space = left_aim.create_space(session, xr::Path::NULL, xr::Posef::IDENTITY)?;
        Ok(Self {
            grip_space,
            grip,
            left_grip_space,
            left_grip,
            aim_space,
            aim,
            trigger,
            left_aim_space,
            left_aim,
            left_trigger,
            left_squeeze,
            reset,
            talk,
            back,
            refresh,
            navigation,
            presentation_navigation,
            live_toggle,
            replay,
            transcript_grip,
            haptic,
            haptics: Haptics::default(),
            set,
            hand_paths,
            location_errors: [None, None],
            diagnostic_state: None,
            last_diagnostic: Instant::now() - Duration::from_secs(1),
        })
    }

    fn log_interaction_profiles(&self, instance: &xr::Instance, session: &xr::Session<xr::Vulkan>) {
        for (hand, path) in self.hand_paths.iter().enumerate() {
            match session.current_interaction_profile(*path) {
                Ok(xr::Path::NULL) => {
                    log::info!("Controller hand {hand} interaction profile: none")
                }
                Ok(profile) => {
                    if let Ok(name) = instance.path_to_string(profile) {
                        log::info!("Controller hand {hand} interaction profile: {name}");
                    }
                }
                Err(error) => {
                    log::warn!("Could not query hand {hand} interaction profile: {error}")
                }
            }
        }
    }

    fn sample_actions(&self, session: &xr::Session<xr::Vulkan>, focused: bool) -> Result<Controls> {
        if !focused {
            return Ok(Controls::default());
        }
        session.sync_actions(&[(&self.set).into()])?;
        Ok(Controls::from_actions(
            Actions {
                trigger: xr_types::action(self.trigger.state(session, xr::Path::NULL)?),
                reset: xr_types::action(self.reset.state(session, xr::Path::NULL)?),
                talk: xr_types::action(self.talk.state(session, xr::Path::NULL)?),
                back: xr_types::action(self.back.state(session, xr::Path::NULL)?),
                refresh: xr_types::action(self.refresh.state(session, xr::Path::NULL)?),
                navigation: xr_types::vector(self.navigation.state(session, xr::Path::NULL)?),
                presentation_navigation: xr_types::vector(
                    self.presentation_navigation
                        .state(session, xr::Path::NULL)?,
                ),
                live_toggle: xr_types::action(self.live_toggle.state(session, xr::Path::NULL)?),
                replay: xr_types::action(self.replay.state(session, xr::Path::NULL)?),
                transcript_grip: xr_types::action(
                    self.transcript_grip.state(session, xr::Path::NULL)?,
                ),
                left_trigger: xr_types::action(self.left_trigger.state(session, xr::Path::NULL)?),
                left_grip: xr_types::action(self.left_squeeze.state(session, xr::Path::NULL)?),
            },
            focused,
        ))
    }

    fn sample_poses(
        &mut self,
        session: &xr::Session<xr::Vulkan>,
        space: &xr::Space,
        time: xr::Time,
        controls: &mut Controls,
    ) {
        let locate = |action: &xr::Action<xr::Posef>,
                      action_space: &xr::Space|
         -> xr::Result<Option<xr::SpaceLocation>> {
            if action.is_active(session, xr::Path::NULL)? {
                Ok(Some(action_space.locate(space, time)?))
            } else {
                Ok(None)
            }
        };
        let locations = [
            locate(&self.left_grip, &self.left_grip_space),
            locate(&self.grip, &self.grip_space),
        ];
        for (hand, location) in locations.iter().enumerate() {
            let error = location.as_ref().err().copied();
            if error != self.location_errors[hand] {
                if let Some(error) = error {
                    log::warn!(
                        "Controller hand {hand} pose unavailable: {error}; buttons remain independent"
                    );
                }
                self.location_errors[hand] = error;
            }
        }
        controls.set_locations(
            locations.map(|location| location.ok().flatten().map(xr_types::location)),
        );
        // Pointing is optional: an aim pose that cannot be located only hides the ray.
        controls.aim = locate(&self.aim, &self.aim_space)
            .ok()
            .flatten()
            .map(xr_types::location)
            .and_then(Location::tracked);
        controls.left_aim = locate(&self.left_aim, &self.left_aim_space)
            .ok()
            .flatten()
            .map(xr_types::location)
            .and_then(Location::tracked);
        // State-change-only and rate-limited: enough to distinguish an absent
        // model from a runtime action/profile switch during a headset check.
        // Debug only: poses come and go whenever a controller is put down or
        // hands replace it, while profile changes are logged at info.
        let diagnostic = (
            controls.talk_active,
            controls.controller_poses.map(|pose| pose.is_some()),
        );
        if self.diagnostic_state != Some(diagnostic)
            && self.last_diagnostic.elapsed() >= Duration::from_secs(1)
        {
            log::debug!(
                "Controller input: X action active={}, drawable poses left/right={:?}",
                diagnostic.0,
                diagnostic.1
            );
            self.diagnostic_state = Some(diagnostic);
            self.last_diagnostic = Instant::now();
        }
    }
}

/// OpenXR's haptic action, as [`Haptics`] calls it.
struct XrVibrator<'a> {
    action: &'a xr::Action<xr::Haptic>,
    session: &'a xr::Session<xr::Vulkan>,
    /// `/user/hand/left` and `/user/hand/right`.
    hands: &'a [xr::Path; 2],
}

impl Vibrator for XrVibrator<'_> {
    type Error = xr::sys::Result;

    fn vibrate(&self, hand: Hand, length: Option<Duration>, amplitude: f32) -> xr::Result<()> {
        let duration =
            haptics::nanoseconds(length).map_or(xr::Duration::MIN_HAPTIC, xr::Duration::from_nanos);
        let vibration = xr::HapticVibration::new()
            .amplitude(amplitude)
            .duration(duration)
            .frequency(xr::FREQUENCY_UNSPECIFIED);
        self.action
            .apply_feedback(self.session, self.hands[haptics::index(hand)], &vibration)
    }

    fn stop(&self, hand: Hand) -> xr::Result<()> {
        self.action
            .stop_feedback(self.session, self.hands[haptics::index(hand)])
    }
}
