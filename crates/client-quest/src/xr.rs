// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::{caption::Caption, controller, graphics::Renderer, hands, swarm::Swarm, vulkan::Gpu};
use anyhow::{Context, Result, ensure};
use ash::vk;
use glam::{Mat4, Quat, Vec3, Vec4};
use openxr as xr;
use std::{
    sync::mpsc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

const STEREO: xr::ViewConfigurationType = xr::ViewConfigurationType::PRIMARY_STEREO;
const BLEND: xr::EnvironmentBlendMode = xr::EnvironmentBlendMode::OPAQUE;

pub fn run(
    platform: &impl xr::PlatformInfo,
    mut destroyed: impl FnMut() -> bool,
    mut conversation: crate::conversation::Conversation,
    mut connection: crate::connection::Connection,
) -> Result<()> {
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
    ensure!(
        supported.khr_vulkan_enable2,
        "Runtime does not support XR_KHR_vulkan_enable2"
    );
    let mut extensions = xr::ExtensionSet::default();
    extensions.khr_vulkan_enable2 = true;
    extensions.ext_local_floor = supported.ext_local_floor;
    extensions.fb_render_model = supported.fb_render_model;
    extensions.ext_hand_tracking = supported.ext_hand_tracking;
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
    let system = instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)?;
    let views = instance.enumerate_view_configuration_views(system, STEREO)?;
    ensure!(views.len() == 2, "Expected a stereo headset");
    ensure!(
        instance
            .enumerate_environment_blend_modes(system, STEREO)?
            .contains(&BLEND),
        "Runtime must support opaque VR"
    );
    let width = views
        .iter()
        .map(|v| v.recommended_image_rect_width)
        .max()
        .unwrap();
    let height = views
        .iter()
        .map(|v| v.recommended_image_rect_height)
        .max()
        .unwrap();
    // Declaration order matters: all XR session resources are dropped before
    // the wgpu-owned Vulkan device and instance.
    let gpu = Gpu::new(&instance, system)?;
    let (session, mut waiter, mut stream) =
        unsafe { instance.create_session::<xr::Vulkan>(system, &gpu.binding) }?;
    let mut input = Input::new(&instance, &session)?;
    let hand_tracking = HandTracking::new(&instance, &session, system, supported.ext_hand_tracking);
    let spaces = session.enumerate_reference_spaces()?;
    // STAGE is intentionally not used: Quest keeps it world-locked when the
    // system Meta-button recenter is invoked. LOCAL_FLOOR follows LOCAL's
    // horizontal recenter while keeping the runtime-estimated floor height.
    let space_type =
        if supported.ext_local_floor && spaces.contains(&xr::ReferenceSpaceType::LOCAL_FLOOR) {
            xr::ReferenceSpaceType::LOCAL_FLOOR
        } else {
            xr::ReferenceSpaceType::LOCAL
        };
    let offset = if space_type == xr::ReferenceSpaceType::LOCAL {
        xr::Posef {
            position: xr::Vector3f {
                x: 0.0,
                y: -1.6,
                z: 0.0,
            },
            ..xr::Posef::IDENTITY
        }
    } else {
        xr::Posef::IDENTITY
    };
    let space = session.create_reference_space(space_type, offset)?;
    log::info!("Recenterable reference space: {space_type:?}");
    let mut scene_anchor = SceneAnchor::new();
    let formats = session.enumerate_swapchain_formats()?;
    let (vk_format, format) = [
        (
            vk::Format::R8G8B8A8_SRGB,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ),
        (
            vk::Format::B8G8R8A8_SRGB,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ),
    ]
    .into_iter()
    .find(|(f, _)| formats.contains(&(f.as_raw() as u32)))
    .context("runtime has no supported sRGB color format")?;
    let mut swapchain = session.create_swapchain(&xr::SwapchainCreateInfo {
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT,
        format: vk_format.as_raw() as u32,
        sample_count: 1,
        width,
        height,
        face_count: 1,
        array_size: 2,
        mip_count: 1,
    })?;
    let images = gpu.import_images(&swapchain, format, width, height)?;
    let mut renderer = Renderer::new(&gpu.device, &gpu.queue, format, width, height);
    let mut controller_models =
        ControllerModels::new(supported.fb_render_model, &instance, &session);
    let mut caption = Caption::new(&gpu.device, format);
    caption.set_recording_progress(&gpu.queue, None, Duration::ZERO);
    // Runs on every return/unwind, before imported image wrappers and the XR
    // swapchain are dropped, including if a frame fails after queue submission.
    let _gpu_idle = WaitForGpu(&gpu.device);
    let mut swarm = Swarm::new();
    let mut event_buffer = xr::EventDataBuffer::new();
    let mut running = false;
    let mut focused = false;
    let mut last_frame = Instant::now();
    let mut frames = 0_u64;
    let mut stick_latched = false;
    let mut environment_latched = false;
    let mut choice_latched = false;
    let mut was_talking = false;
    // After focus loss, X must be released before it records again, so that holding it through a system menu does not start a recording.
    let mut talk_blocked = false;
    let mut signals = crate::interaction::Signals::default();
    #[cfg(target_os = "android")]
    let mut delegation_scene = crate::delegation::Scene::new();
    #[cfg(target_os = "android")]
    let mut coordinator_samples = Vec::with_capacity(crate::swarm::AGENT_COUNT);
    #[cfg(target_os = "android")]
    let mut presentation_navigation = crate::workspace_graphics::CardNavigation::default();
    log::info!(
        "Ready: {width}x{height} per eye, {} swapchain images. Right trigger: attract; A: choose or reset.",
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
                    if focused && state != xr::SessionState::FOCUSED {
                        // Do not lose a real focus-loss transition if the
                        // runtime also queues a refocus in this poll batch.
                        stick_latched = false;
                        environment_latched = false;
                        choice_latched = false;
                        was_talking = false;
                        talk_blocked = true;
                        conversation.stop_talking();
                        #[cfg(target_os = "android")]
                        presentation_navigation.release();
                    }
                    focused = state == xr::SessionState::FOCUSED;
                    match state {
                        xr::SessionState::READY => {
                            session.begin(STEREO)?;
                            running = true;
                            last_frame = Instant::now();
                        }
                        xr::SessionState::STOPPING => {
                            session.end()?;
                            running = false;
                        }
                        xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => break 'main,
                        _ => {}
                    }
                }
                xr::Event::InstanceLossPending(_) => break 'main,
                xr::Event::InteractionProfileChanged(_) => {
                    input.log_interaction_profiles(&instance, &session);
                }
                xr::Event::ReferenceSpaceChangePending(change)
                    if change.session() == session.as_raw()
                        && change.reference_space_type() == space_type =>
                {
                    log::info!(
                        "Reference space change pending at {:?}",
                        change.change_time()
                    );
                    scene_anchor.schedule(change.change_time());
                }
                _ => {}
            }
        }
        connection.poll(focused && running, &mut conversation);
        if !focused || !running {
            stick_latched = false;
            environment_latched = false;
            choice_latched = false;
            was_talking = false;
            talk_blocked = true;
            conversation.stop_talking();
            #[cfg(target_os = "android")]
            presentation_navigation.release();
        }
        if !running {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        controller_models.refresh(&gpu.device, &gpu.queue, &mut renderer);
        let frame = waiter.wait()?;
        stream.begin()?;
        let now = Instant::now();
        let dt = now.duration_since(last_frame).as_secs_f32();
        last_frame = now;
        // Button/axis actions and their consumers run on every running frame,
        // even if the compositor or head/controller tracking cannot render.
        // Only actual session focus loss cancels an utterance; an X release
        // while still focused must finalize/send it instead.
        let mut controls = input.sample_actions(&session, focused)?;
        let environment_pressed = controls.environment_grip > 0.75;
        if environment_pressed && !environment_latched {
            renderer.set_environment(renderer.environment().next());
            log::info!("Environment: {:?}", renderer.environment());
        }
        environment_latched = environment_pressed;
        let stick = controls.navigation;
        let moving = stick.x.abs() > 0.6 || stick.y.abs() > 0.6;
        let navigate = moving && !stick_latched;
        stick_latched = moving;
        if navigate {
            if stick.y.abs() >= stick.x.abs() {
                conversation.navigate(0, if stick.y > 0.0 { -1 } else { 1 });
            } else {
                conversation.navigate(if stick.x > 0.0 { 1 } else { -1 }, 0);
            }
        }
        if controls.back {
            conversation.back();
        }
        if controls.refresh {
            conversation.refresh();
        }
        // While an approval offers standing options, the right stick chooses
        // among them instead of paging presentation cards.
        let choosing = conversation.is_choosing();
        let right = controls.presentation_navigation;
        let choice_moving = choosing && right.x.abs() > 0.6;
        if choice_moving && !choice_latched {
            conversation.choose(if right.x > 0.0 { 1 } else { -1 });
        }
        choice_latched = choice_moving;
        let reset = controls.reset && conversation.accept();
        // Hold X to talk to the agent; releasing it while focused sends the utterance.
        if focused && !controls.talk {
            talk_blocked = false;
        }
        let talking = controls.talk && !talk_blocked;
        if talking != was_talking {
            conversation.talk(talking);
        }
        was_talking = talking;
        // Spoken replies and the audience microphone return with server playback.
        let _ = (
            controls.sound_toggle,
            controls.replay,
            controls.audience_grip,
        );
        #[cfg(target_os = "android")]
        presentation_navigation.update(
            None,
            &crate::delegation::Workspace::default(),
            if choosing { 0.0 } else { right.x },
            if choosing { 0.0 } else { right.y },
        );
        caption.set_text(&gpu.queue, &conversation.text());
        caption.set_recording_progress(&gpu.queue, conversation.recording(), Duration::ZERO);
        if reset {
            swarm = Swarm::new();
        }
        if !frame.should_render {
            stream.end(frame.predicted_display_time, BLEND, &[])?;
            continue;
        }
        let (flags, views) = session.locate_views(STEREO, frame.predicted_display_time, &space)?;
        if views.len() != 2
            || !flags.contains(
                xr::ViewStateFlags::POSITION_VALID | xr::ViewStateFlags::ORIENTATION_VALID,
            )
        {
            stream.end(frame.predicted_display_time, BLEND, &[])?;
            continue;
        }
        scene_anchor.update(&views, frame.predicted_display_time);
        if focused {
            input.sample_poses(
                &session,
                &space,
                frame.predicted_display_time,
                &mut controls,
            );
        }
        {
            let visual = signals.update(
                crate::interaction::Inputs {
                    session: if focused {
                        conversation.activity()
                    } else {
                        crate::interaction::SessionActivity::Unknown
                    },
                    ..Default::default()
                },
                dt,
            );
            if visual.mode != swarm.visual.mode {
                log::info!(
                    "Swarm interaction: {:?}, session_working={}",
                    visual.mode,
                    visual.working
                );
            }
            swarm.set_visual(visual, dt);
        }
        // The simulation stays in its own coordinates; tracked controller
        // positions must be transformed into that same space after recenter.
        let controller = controls.controller.map(|p| scene_anchor.to_scene(p));
        let controller_poses = controls
            .controller_poses
            .map(|pose| pose.and_then(|pose| scene_anchor.to_scene_pose(pose)));
        let tracked_hands = if focused {
            hand_tracking.sample(&space, frame.predicted_display_time, &scene_anchor)
        } else {
            [None, None]
        };
        swarm.step(dt, controller.filter(|_| controls.attracting));
        renderer.update(
            &gpu.queue,
            &swarm,
            if controller_poses[controller::RIGHT].is_some()
                || tracked_hands[controller::RIGHT].is_some()
            {
                None
            } else {
                controller
            },
            controls.attracting,
        );
        // Workers arrive with the space state once the scene reads it.
        #[cfg(target_os = "android")]
        let (root, current_workspace) = (None, &crate::delegation::Workspace::default());
        #[cfg(target_os = "android")]
        {
            renderer.set_presentation_page(presentation_navigation.card);
            renderer.set_presentation_content_page(presentation_navigation.content);
            coordinator_samples.clear();
            coordinator_samples.extend(swarm.agents.iter().map(|agent| agent.position.to_array()));
            delegation_scene.set_coordinator_samples(&coordinator_samples);
            delegation_scene.update(root, current_workspace, dt);
            renderer.update_workspace(
                &gpu.device,
                &gpu.queue,
                delegation_scene.cubes(),
                delegation_scene.stations(),
                current_workspace,
            );
        }
        renderer.update_controllers(&gpu.queue, controller_poses, tracked_hands);
        let image = swapchain.acquire_image()? as usize;
        swapchain.wait_image(xr::Duration::INFINITE)?;
        let submission = renderer.render(
            &gpu.device,
            &gpu.queue,
            &images[image].views,
            &[view_projection(&views[0]), view_projection(&views[1])],
            scene_anchor.transform(),
            Some((&caption, head_pose(&views))),
        );
        // A deliberately simple, conservative bridge: finish GPU work before
        // handing images back to XR. Rendering leaves COLOR_ATTACHMENT_OPTIMAL,
        // as required by the OpenXR Vulkan swapchain contract.
        let completed = gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        });
        swapchain.release_image()?;
        completed.context("waiting for XR rendering")?;
        let projection_views: Vec<_> = views
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
        frames += 1;
        if frames == 1 || frames.is_multiple_of(720) {
            log::info!("Submitted stereo frame {frames}");
        }
    }
    if running && session.request_exit().is_ok() {
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

    fn sample(
        &self,
        space: &xr::Space,
        time: xr::Time,
        anchor: &SceneAnchor,
    ) -> [Option<hands::Hand>; 2] {
        let scene_from_space = anchor.transform().inverse();
        std::array::from_fn(|hand| {
            let tracker = self.trackers[hand].as_ref()?;
            let joints = match space.locate_hand_joints(tracker, time) {
                Ok(Some(joints)) => joints,
                Ok(None) => return None,
                Err(error) => {
                    log::debug!("Hand tracking unavailable for frame: {error}");
                    return None;
                }
            };
            let mut out = hands::Hand {
                joints: [None; hands::JOINT_COUNT],
            };
            for (index, joint) in joints.iter().enumerate() {
                if !joint
                    .location_flags
                    .contains(xr::SpaceLocationFlags::POSITION_VALID)
                {
                    continue;
                }
                let position = joint.pose.position;
                let scene_position = scene_from_space
                    .transform_point3(Vec3::new(position.x, position.y, position.z));
                let radius = if joint.radius.is_finite() && joint.radius > 0.0 {
                    joint.radius.clamp(0.001, 0.1)
                } else {
                    0.006
                };
                let value = hands::Joint {
                    position: scene_position.to_array(),
                    radius,
                };
                if value.valid() {
                    out.joints[index] = Some(value);
                }
            }
            hands::BONES
                .iter()
                .any(|(a, b)| out.joints[*a].is_some() && out.joints[*b].is_some())
                .then_some(out)
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
        let hand = match name.as_str() {
            "/model_fb/controller/left" => controller::LEFT,
            "/model_fb/controller/right" => controller::RIGHT,
            _ => continue,
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
    trigger: xr::Action<f32>,
    reset: xr::Action<bool>,
    talk: xr::Action<bool>,
    back: xr::Action<bool>,
    refresh: xr::Action<bool>,
    navigation: xr::Action<xr::Vector2f>,
    presentation_navigation: xr::Action<xr::Vector2f>,
    sound_toggle: xr::Action<bool>,
    replay: xr::Action<bool>,
    audience_grip: xr::Action<f32>,
    environment_grip: xr::Action<f32>,
    set: xr::ActionSet,
    hand_paths: [xr::Path; 2],
    location_errors: [Option<xr::sys::Result>; 2],
    diagnostic_state: Option<(bool, [bool; 2])>,
    last_diagnostic: Instant,
}

/// A single synchronized action snapshot. No pose/tracking state participates
/// in interpreting held buttons, press edges, or inactive/disconnected actions.
struct ActionInputs {
    trigger: xr::ActionState<f32>,
    reset: xr::ActionState<bool>,
    talk: xr::ActionState<bool>,
    back: xr::ActionState<bool>,
    refresh: xr::ActionState<bool>,
    navigation: xr::ActionState<xr::Vector2f>,
    presentation_navigation: xr::ActionState<xr::Vector2f>,
    sound_toggle: xr::ActionState<bool>,
    replay: xr::ActionState<bool>,
    audience_grip: xr::ActionState<f32>,
    environment_grip: xr::ActionState<f32>,
}

#[derive(Default)]
struct Controls {
    controller: Option<Vec3>,
    controller_poses: [Option<xr::Posef>; 2],
    attracting: bool,
    reset: bool,
    talk: bool,
    talk_active: bool,
    back: bool,
    refresh: bool,
    navigation: xr::Vector2f,
    presentation_navigation: xr::Vector2f,
    sound_toggle: bool,
    replay: bool,
    audience_grip: f32,
    environment_grip: f32,
}

impl Controls {
    fn from_actions(actions: ActionInputs, focused: bool) -> Self {
        if !focused {
            return Self::default();
        }
        let held = |state: xr::ActionState<bool>| state.is_active && state.current_state;
        let pressed = |state: xr::ActionState<bool>| held(state) && state.changed_since_last_sync;
        let axis = |state: xr::ActionState<xr::Vector2f>| {
            if state.is_active {
                state.current_state
            } else {
                xr::Vector2f::default()
            }
        };
        Self {
            attracting: actions.trigger.is_active && actions.trigger.current_state > 0.2,
            reset: pressed(actions.reset),
            talk: held(actions.talk),
            talk_active: actions.talk.is_active,
            back: pressed(actions.back),
            refresh: pressed(actions.refresh),
            navigation: axis(actions.navigation),
            presentation_navigation: axis(actions.presentation_navigation),
            sound_toggle: pressed(actions.sound_toggle),
            replay: pressed(actions.replay),
            audience_grip: if actions.audience_grip.is_active {
                actions.audience_grip.current_state
            } else {
                0.0
            },
            environment_grip: if actions.environment_grip.is_active {
                actions.environment_grip.current_state
            } else {
                0.0
            },
            ..Self::default()
        }
    }

    fn set_locations(&mut self, locations: [xr::Result<Option<xr::SpaceLocation>>; 2]) {
        // A missing/invalid pose or a failed optional locate must never discard
        // this frame's button release or press. Do not reuse previous poses.
        let locations = locations.map(|location| location.ok().flatten());
        self.controller = locations[controller::RIGHT]
            .filter(|location| {
                location
                    .location_flags
                    .contains(xr::SpaceLocationFlags::POSITION_VALID)
            })
            .map(|location| {
                Vec3::new(
                    location.pose.position.x,
                    location.pose.position.y,
                    location.pose.position.z,
                )
            });
        self.controller_poses = locations.map(|location| {
            location
                .filter(|location| {
                    location.location_flags.contains(
                        xr::SpaceLocationFlags::POSITION_VALID
                            | xr::SpaceLocationFlags::ORIENTATION_VALID,
                    )
                })
                .map(|location| location.pose)
        });
    }
}

impl Input {
    fn new(instance: &xr::Instance, session: &xr::Session<xr::Vulkan>) -> Result<Self> {
        let set = instance.create_action_set("swarm", "Swarm", 0)?;
        let grip = set.create_action::<xr::Posef>("right_grip", "Right controller", &[])?;
        let left_grip = set.create_action::<xr::Posef>("left_grip_pose", "Left controller", &[])?;
        let trigger = set.create_action::<f32>("attract", "Attract agents", &[])?;
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
        let sound_toggle =
            set.create_action::<bool>("sound_toggle", "Toggle spoken replies", &[])?;
        let replay = set.create_action::<bool>("replay", "Replay spoken reply", &[])?;
        let audience_grip =
            set.create_action::<f32>("audience_mic", "Toggle live audience microphone", &[])?;
        let environment_grip =
            set.create_action::<f32>("environment", "Switch environment", &[])?;
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
                    &trigger,
                    instance.string_to_path("/user/hand/right/input/trigger/value")?,
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
                    &sound_toggle,
                    instance.string_to_path("/user/hand/left/input/thumbstick/click")?,
                ),
                xr::Binding::new(
                    &replay,
                    instance.string_to_path("/user/hand/right/input/thumbstick/click")?,
                ),
                xr::Binding::new(
                    &audience_grip,
                    instance.string_to_path("/user/hand/left/input/squeeze/value")?,
                ),
                xr::Binding::new(
                    &environment_grip,
                    instance.string_to_path("/user/hand/right/input/squeeze/value")?,
                ),
            ],
        )?;
        session.attach_action_sets(&[&set])?;
        let grip_space = grip.create_space(session, xr::Path::NULL, xr::Posef::IDENTITY)?;
        let left_grip_space =
            left_grip.create_space(session, xr::Path::NULL, xr::Posef::IDENTITY)?;
        Ok(Self {
            grip_space,
            grip,
            left_grip_space,
            left_grip,
            trigger,
            reset,
            talk,
            back,
            refresh,
            navigation,
            presentation_navigation,
            sound_toggle,
            replay,
            audience_grip,
            environment_grip,
            set,
            hand_paths: [
                instance.string_to_path("/user/hand/left")?,
                instance.string_to_path("/user/hand/right")?,
            ],
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
            ActionInputs {
                trigger: self.trigger.state(session, xr::Path::NULL)?,
                reset: self.reset.state(session, xr::Path::NULL)?,
                talk: self.talk.state(session, xr::Path::NULL)?,
                back: self.back.state(session, xr::Path::NULL)?,
                refresh: self.refresh.state(session, xr::Path::NULL)?,
                navigation: self.navigation.state(session, xr::Path::NULL)?,
                presentation_navigation: self
                    .presentation_navigation
                    .state(session, xr::Path::NULL)?,
                sound_toggle: self.sound_toggle.state(session, xr::Path::NULL)?,
                replay: self.replay.state(session, xr::Path::NULL)?,
                audience_grip: self.audience_grip.state(session, xr::Path::NULL)?,
                environment_grip: self.environment_grip.state(session, xr::Path::NULL)?,
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
        controls.set_locations(locations);
        // State-change-only and rate-limited: enough to distinguish an absent
        // model from a runtime action/profile switch during a headset check.
        let diagnostic = (
            controls.talk_active,
            controls.controller_poses.map(|pose| pose.is_some()),
        );
        if self.diagnostic_state != Some(diagnostic)
            && self.last_diagnostic.elapsed() >= Duration::from_secs(1)
        {
            log::info!(
                "Controller input: X action active={}, drawable poses left/right={:?}",
                diagnostic.0,
                diagnostic.1
            );
            self.diagnostic_state = Some(diagnostic);
            self.last_diagnostic = Instant::now();
        }
    }
}

/// Positions the simulation once at startup, then follows explicit runtime
/// reference-space changes only at their effective predicted display time.
/// It does not re-anchor on focus/visibility changes or normal head movement.
struct SceneAnchor {
    scene_to_space: Option<Mat4>,
    pending: Vec<xr::Time>,
}

impl SceneAnchor {
    fn new() -> Self {
        Self {
            scene_to_space: None,
            pending: Vec::new(),
        }
    }

    fn schedule(&mut self, time: xr::Time) {
        self.pending.push(time);
    }

    fn update(&mut self, views: &[xr::View], display_time: xr::Time) {
        let recentered = self
            .pending
            .iter()
            .any(|time| time.as_nanos() <= display_time.as_nanos());
        self.pending
            .retain(|time| time.as_nanos() > display_time.as_nanos());
        if recentered && self.scene_to_space.is_some() {
            // A system recenter makes the new local origin face the user's
            // current forward direction. Reset the startup offset so the
            // existing swarm appears in front, without resetting its state.
            self.scene_to_space = Some(Mat4::IDENTITY);
            log::info!("Applied OpenXR recenter at display time {display_time:?}");
        } else if self.scene_to_space.is_none() {
            self.scene_to_space = initial_scene_transform(views);
            if self.scene_to_space.is_some() {
                log::info!("Aligned scene to initial headset direction");
            }
        }
    }

    fn transform(&self) -> Mat4 {
        self.scene_to_space.unwrap_or(Mat4::IDENTITY)
    }
    fn to_scene(&self, point: Vec3) -> Vec3 {
        self.transform().inverse().transform_point3(point)
    }

    fn to_scene_pose(&self, pose: xr::Posef) -> Option<controller::Pose> {
        let pose = controller::Pose {
            position: [pose.position.x, pose.position.y, pose.position.z],
            orientation: [
                pose.orientation.x,
                pose.orientation.y,
                pose.orientation.z,
                pose.orientation.w,
            ],
        };
        pose.valid()
            .then(|| self.transform().inverse() * pose.transform())
            .and_then(controller::Pose::from_transform)
    }
}

fn initial_scene_transform(views: &[xr::View]) -> Option<Mat4> {
    let head = head_pose(views);
    let forward = head.transform_vector3(-Vec3::Z);
    let horizontal = Vec3::new(forward.x, 0.0, forward.z);
    // Looking straight up/down has no well-defined horizontal heading. Wait
    // for a valid orientation rather than generating an unstable yaw.
    if !horizontal.is_finite() || horizontal.length_squared() < 0.0001 {
        return None;
    }
    let yaw = (-horizontal.x).atan2(-horizontal.z);
    let eye = head.transform_point3(Vec3::ZERO);
    if !eye.is_finite() {
        return None;
    }
    Some(Mat4::from_rotation_translation(
        Quat::from_rotation_y(yaw),
        Vec3::new(eye.x, 0.0, eye.z),
    ))
}

fn head_pose(views: &[xr::View]) -> Mat4 {
    let left = views[0].pose.position;
    let right = views[1].pose.position;
    let q = views[0].pose.orientation;
    Mat4::from_rotation_translation(
        Quat::from_xyzw(q.x, q.y, q.z, q.w),
        Vec3::new(left.x + right.x, left.y + right.y, left.z + right.z) * 0.5,
    )
}

fn view_projection(view: &xr::View) -> Mat4 {
    let fov = view.fov;
    let (left, right, down, up) = (
        fov.angle_left.tan(),
        fov.angle_right.tan(),
        fov.angle_down.tan(),
        fov.angle_up.tan(),
    );
    let (near, far) = (0.05, 50.0);
    // Asymmetric OpenXR frustum, right-handed -Z forward, WebGPU depth 0..1.
    // wgpu's Vulkan backend handles the framebuffer Y convention itself.
    let projection = Mat4::from_cols(
        Vec4::new(2.0 / (right - left), 0.0, 0.0, 0.0),
        Vec4::new(0.0, 2.0 / (up - down), 0.0, 0.0),
        Vec4::new(
            (right + left) / (right - left),
            (up + down) / (up - down),
            -far / (far - near),
            -1.0,
        ),
        Vec4::new(0.0, 0.0, -(far * near) / (far - near), 0.0),
    );
    let p = view.pose.position;
    let q = view.pose.orientation;
    projection
        * Mat4::from_rotation_translation(
            Quat::from_xyzw(q.x, q.y, q.z, q.w),
            Vec3::new(p.x, p.y, p.z),
        )
        .inverse()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action<T: xr::ActionInput>(value: T, active: bool, changed: bool) -> xr::ActionState<T> {
        xr::ActionState {
            current_state: value,
            changed_since_last_sync: changed,
            last_change_time: xr::Time::from_nanos(1),
            is_active: active,
        }
    }

    fn actions(held: bool, active: bool, changed: bool) -> ActionInputs {
        ActionInputs {
            trigger: action(if held { 1.0 } else { 0.0 }, active, changed),
            reset: action(held, active, changed),
            talk: action(held, active, changed),
            back: action(held, active, changed),
            refresh: action(held, active, changed),
            navigation: action(xr::Vector2f { x: 0.8, y: -0.7 }, active, changed),
            presentation_navigation: action(xr::Vector2f { x: -0.9, y: 0.6 }, active, changed),
            sound_toggle: action(held, active, changed),
            replay: action(held, active, changed),
            audience_grip: action(if held { 1.0 } else { 0.0 }, active, changed),
            environment_grip: action(if held { 1.0 } else { 0.0 }, active, changed),
        }
    }

    fn location(flags: xr::SpaceLocationFlags) -> xr::Result<Option<xr::SpaceLocation>> {
        Ok(Some(xr::SpaceLocation {
            location_flags: flags,
            pose: xr::Posef::IDENTITY,
        }))
    }

    fn assert_press_edges(controls: &Controls, expected: bool) {
        assert_eq!(controls.reset, expected);
        assert_eq!(controls.back, expected);
        assert_eq!(controls.refresh, expected);
        assert_eq!(controls.sound_toggle, expected);
        assert_eq!(controls.replay, expected);
    }

    #[test]
    fn held_x_survives_tracking_loss_and_releases_while_untracked() {
        let valid =
            xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;
        let frames = [
            (true, true, [location(valid), location(valid)]),
            // The runtime can supply invalid locations, no active pose, or a
            // locate error without losing the independent X action.
            (
                true,
                false,
                [location(xr::SpaceLocationFlags::EMPTY), Ok(None)],
            ),
            (
                true,
                false,
                [Err(xr::sys::Result::ERROR_POSE_INVALID), Ok(None)],
            ),
            (false, true, [Ok(None), Ok(None)]),
        ];
        for (held, changed, locations) in frames {
            let mut controls = Controls::from_actions(actions(held, true, changed), true);
            controls.set_locations(locations);
            assert_eq!(controls.talk, held);
            assert_eq!(controls.audience_grip, if held { 1.0 } else { 0.0 });
            assert_eq!(controls.environment_grip, if held { 1.0 } else { 0.0 });
            assert_press_edges(&controls, held && changed);
        }
    }

    #[test]
    fn new_untracked_presses_and_menu_axes_do_not_require_poses() {
        for locations in [
            [Ok(None), Ok(None)],
            [
                location(xr::SpaceLocationFlags::EMPTY),
                location(xr::SpaceLocationFlags::EMPTY),
            ],
            [
                Err(xr::sys::Result::ERROR_TIME_INVALID),
                Err(xr::sys::Result::ERROR_POSE_INVALID),
            ],
        ] {
            let mut controls = Controls::from_actions(actions(true, true, true), true);
            controls.set_locations(locations);
            assert!(controls.talk && controls.attracting);
            assert_press_edges(&controls, true);
            assert_eq!(controls.audience_grip, 1.0);
            assert_eq!(controls.navigation.x, 0.8);
            assert_eq!(controls.presentation_navigation.x, -0.9);
            assert!(controls.controller.is_none());
            assert!(controls.controller_poses.iter().all(Option::is_none));
        }
    }

    #[test]
    fn pose_validity_only_changes_spatial_controls_and_never_reuses_old_poses() {
        let mut controls = Controls::from_actions(actions(true, true, true), true);
        let valid =
            xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;
        controls.set_locations([location(valid), location(valid)]);
        assert!(controls.controller_poses.iter().all(Option::is_some));
        controls.set_locations([
            location(xr::SpaceLocationFlags::ORIENTATION_VALID),
            location(xr::SpaceLocationFlags::POSITION_VALID),
        ]);
        assert!(controls.controller.is_some());
        assert!(controls.controller_poses.iter().all(Option::is_none));
        controls.set_locations([Ok(None), Err(xr::sys::Result::ERROR_POSE_INVALID)]);
        assert!(controls.controller.is_none());
        assert!(controls.controller_poses.iter().all(Option::is_none));
        assert!(controls.talk);
        assert_press_edges(&controls, true);
    }

    #[test]
    fn one_hand_location_error_does_not_hide_the_other_controller() {
        let mut controls = Controls::from_actions(actions(true, true, true), true);
        let valid =
            xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;
        controls.set_locations([Err(xr::sys::Result::ERROR_POSE_INVALID), location(valid)]);
        assert!(controls.controller_poses[controller::LEFT].is_none());
        assert!(controls.controller_poses[controller::RIGHT].is_some());
        assert!(controls.controller.is_some());
        assert!(controls.talk);
        assert_press_edges(&controls, true);
    }

    #[test]
    fn inactive_actions_and_focus_loss_clear_held_buttons_axes_and_edges() {
        for (active, focused) in [(false, true), (true, false), (false, false)] {
            // Deliberately stale held/changed values: neither a disconnected
            // controller nor an unfocused session may keep recording alive.
            let controls = Controls::from_actions(actions(true, active, true), focused);
            assert!(!controls.talk && !controls.attracting);
            assert!(!controls.talk_active);
            assert_press_edges(&controls, false);
            assert_eq!(controls.audience_grip, 0.0);
            assert_eq!(controls.environment_grip, 0.0);
            assert_eq!((controls.navigation.x, controls.navigation.y), (0.0, 0.0));
            assert_eq!(
                (
                    controls.presentation_navigation.x,
                    controls.presentation_navigation.y
                ),
                (0.0, 0.0)
            );
        }
        // Refocusing samples fresh action state, not a cached button. Held X
        // is level-triggered; one-shot menu actions still require a new edge.
        let controls = Controls::from_actions(actions(true, true, false), true);
        assert!(controls.talk);
        assert_press_edges(&controls, false);
        let released = Controls::from_actions(actions(false, true, true), true);
        assert!(!released.talk);
        assert_press_edges(&released, false);
    }

    #[test]
    fn audience_grip_does_not_start_push_to_talk() {
        let mut input = actions(false, true, false);
        input.audience_grip = action(1.0, true, true);
        let controls = Controls::from_actions(input, true);
        assert_eq!(controls.audience_grip, 1.0);
        assert!(!controls.talk);
        assert_press_edges(&controls, false);
    }

    #[test]
    fn environment_grip_is_independent_of_chat_and_attraction() {
        let mut input = actions(false, true, false);
        input.environment_grip = action(1.0, true, true);
        let controls = Controls::from_actions(input, true);
        assert_eq!(controls.environment_grip, 1.0);
        assert_eq!(controls.audience_grip, 0.0);
        assert!(!controls.talk && !controls.attracting);
        assert_press_edges(&controls, false);
    }

    #[test]
    fn controller_pose_preserves_position_and_orientation_through_scene_anchor() {
        let scene_to_space =
            Mat4::from_rotation_translation(Quat::from_rotation_y(0.7), Vec3::new(1.0, 0.0, -2.0));
        let anchor = SceneAnchor {
            scene_to_space: Some(scene_to_space),
            pending: Vec::new(),
        };
        let q = Quat::from_rotation_x(0.4) * Quat::from_rotation_y(-0.2);
        let pose = xr::Posef {
            orientation: xr::Quaternionf {
                x: q.x,
                y: q.y,
                z: q.z,
                w: q.w,
            },
            position: xr::Vector3f {
                x: 0.3,
                y: 1.2,
                z: -0.8,
            },
        };
        let scene_pose = anchor.to_scene_pose(pose).unwrap();
        let expected = Mat4::from_rotation_translation(q, Vec3::new(0.3, 1.2, -0.8));
        assert!((scene_to_space * scene_pose.transform()).abs_diff_eq(expected, 1e-5));
        let invalid = xr::Posef {
            orientation: xr::Quaternionf {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                w: 0.0,
            },
            ..pose
        };
        assert!(anchor.to_scene_pose(invalid).is_none());
    }

    fn stereo(position: Vec3, orientation: Quat) -> Vec<xr::View> {
        let q = xr::Quaternionf {
            x: orientation.x,
            y: orientation.y,
            z: orientation.z,
            w: orientation.w,
        };
        [-0.032, 0.032]
            .into_iter()
            .map(|x| {
                let p = position + orientation * Vec3::X * x;
                xr::View {
                    pose: xr::Posef {
                        orientation: q,
                        position: xr::Vector3f {
                            x: p.x,
                            y: p.y,
                            z: p.z,
                        },
                    },
                    fov: xr::Fovf {
                        angle_left: -0.7,
                        angle_right: 0.7,
                        angle_down: -0.7,
                        angle_up: 0.7,
                    },
                }
            })
            .collect()
    }

    #[test]
    fn startup_scene_aligns_with_head_yaw_and_position_but_not_pitch() {
        for yaw in [0.0, 0.7, -1.5, std::f32::consts::PI] {
            let orientation = Quat::from_rotation_y(yaw) * Quat::from_rotation_x(0.35);
            let views = stereo(Vec3::new(2.0, 1.7, -3.0), orientation);
            let transform = initial_scene_transform(&views).unwrap();
            let scene_forward = transform.transform_vector3(-Vec3::Z);
            let expected = Quat::from_rotation_y(yaw) * -Vec3::Z;
            assert!(scene_forward.distance(expected) < 0.0001);
            assert!(
                transform
                    .transform_point3(Vec3::ZERO)
                    .distance(Vec3::new(2.0, 0.0, -3.0))
                    < 0.0001
            );
            assert!(
                (transform.transform_point3(crate::swarm::CENTER).y - crate::swarm::CENTER.y).abs()
                    < 0.0001
            );
        }
        assert!(
            initial_scene_transform(&stereo(
                Vec3::Y,
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)
            ))
            .is_none()
        );
    }

    #[test]
    fn recenter_waits_until_runtime_change_time_and_preserves_simulation_coordinates() {
        let mut anchor = SceneAnchor::new();
        let views = stereo(Vec3::new(2.0, 1.6, 1.0), Quat::from_rotation_y(1.0));
        anchor.update(&views, xr::Time::from_nanos(10));
        let original = anchor.transform();
        let controller = Vec3::new(0.2, 1.0, -0.4);
        assert!(
            anchor
                .to_scene(original.transform_point3(controller))
                .distance(controller)
                < 0.0001
        );
        anchor.schedule(xr::Time::from_nanos(40));
        anchor.schedule(xr::Time::from_nanos(20));
        anchor.update(&views, xr::Time::from_nanos(19));
        assert_eq!(anchor.transform(), original);
        anchor.update(&views, xr::Time::from_nanos(20));
        assert_eq!(anchor.transform(), Mat4::IDENTITY);
        assert_eq!(anchor.pending.len(), 1);
        anchor.update(&views, xr::Time::from_nanos(40));
        assert!(anchor.pending.is_empty());
        assert_eq!(anchor.to_scene(controller), controller);
    }
    #[test]
    fn caption_uses_midpoint_between_the_eyes() {
        let q = xr::Quaternionf {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            w: 1.0,
        };
        let views: Vec<_> = [-0.032, 0.032]
            .into_iter()
            .map(|x| xr::View {
                pose: xr::Posef {
                    orientation: q,
                    position: xr::Vector3f { x, y: 1.6, z: 0.4 },
                },
                fov: xr::Fovf {
                    angle_left: -0.7,
                    angle_right: 0.7,
                    angle_down: -0.7,
                    angle_up: 0.7,
                },
            })
            .collect();
        assert!(
            head_pose(&views)
                .transform_point3(Vec3::ZERO)
                .distance(Vec3::new(0.0, 1.6, 0.4))
                < 0.0001
        );
    }

    #[test]
    fn xr_projection_maps_frustum_and_depth_correctly() {
        let view = xr::View {
            pose: xr::Posef::IDENTITY,
            fov: xr::Fovf {
                angle_left: -0.7,
                angle_right: 0.9,
                angle_down: -0.8,
                angle_up: 0.75,
            },
        };
        let m = view_projection(&view);
        for (x, y, z, expected) in [
            (view.fov.angle_left.tan(), 0.0, -1.0, -1.0),
            (view.fov.angle_right.tan(), 0.0, -1.0, 1.0),
        ] {
            assert!((m.project_point3(Vec3::new(x, y, z)).x - expected).abs() < 0.0001);
        }
        assert!(m.project_point3(Vec3::new(0.0, 0.0, -0.05)).z.abs() < 0.0001);
        assert!((m.project_point3(Vec3::new(0.0, 0.0, -50.0)).z - 1.0).abs() < 0.0001);
    }
}
