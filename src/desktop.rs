// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use crate::{
    graphics::Renderer,
    spectator_view::{RenderInputs, SpectatorView},
    swarm::{CENTER, Swarm},
};
use anyhow::{Context, Result};
use glam::{Mat4, Vec2, Vec3};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize, Size},
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

const TITLE: &str =
    "Tomori | LMB attract · RMB orbit · wheel zoom · E environment · R reset · Space pause";
const SPECTATOR_WIDTH: u32 = 1920;
const SPECTATOR_HEIGHT: u32 = 1080;

fn initial_window_size(spectator: bool) -> Size {
    if spectator {
        // Request physical render pixels, not logical points, so desktop DPI
        // scaling doesn't silently change the intended OBS capture resolution.
        PhysicalSize::new(SPECTATOR_WIDTH, SPECTATOR_HEIGHT).into()
    } else {
        LogicalSize::new(1100, 760).into()
    }
}

pub fn run(spectator: bool, pipewire: bool) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App {
        spectator,
        pipewire,
        ..Default::default()
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[derive(Default)]
struct App {
    state: Option<State>,
    error: Option<anyhow::Error>,
    spectator: bool,
    pipewire: bool,
    next_video_frame: Option<Instant>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let result = (|| {
            let mut attributes = Window::default_attributes()
                .with_title(if self.spectator {
                    "Tomori | Quest spectator (OBS)"
                } else {
                    TITLE
                })
                .with_decorations(!self.spectator)
                .with_inner_size(initial_window_size(self.spectator));
            if self.spectator {
                // Keep the content itself at least 1080p even when the desktop
                // work area excludes a panel. Escape closes the borderless window.
                attributes = attributes
                    .with_min_inner_size(PhysicalSize::new(SPECTATOR_WIDTH, SPECTATOR_HEIGHT));
            }
            let window = Arc::new(event_loop.create_window(attributes)?);
            pollster::block_on(State::new(window, self.spectator, self.pipewire))
        })();
        match result {
            Ok(state) => {
                state.window.request_redraw();
                if self.pipewire {
                    self.next_video_frame = Some(Instant::now());
                }
                self.state = Some(state);
            }
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else {
            return;
        };
        if id != state.window.id() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::Occluded(occluded) => {
                state.occluded = occluded;
                state.last_frame = Instant::now();
                if !occluded {
                    state.window.request_redraw();
                }
            }
            WindowEvent::Focused(false) | WindowEvent::CursorLeft { .. } => {
                state.attracting = false;
                state.orbiting = false;
            }
            WindowEvent::CursorMoved { position, .. } if state.spectator.is_none() => {
                let cursor = Vec2::new(position.x as f32, position.y as f32);
                if state.orbiting {
                    state.camera.orbit(cursor - state.cursor);
                }
                state.cursor = cursor;
            }
            WindowEvent::MouseInput {
                state: pressed,
                button,
                ..
            } if state.spectator.is_none() => {
                let pressed = pressed == ElementState::Pressed;
                match button {
                    MouseButton::Left => state.attracting = pressed,
                    MouseButton::Right => state.orbiting = pressed,
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } if state.spectator.is_none() => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 0.12,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 * 0.002,
                };
                state.camera.zoom(amount);
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Escape) => event_loop.exit(),
                    PhysicalKey::Code(KeyCode::KeyR) if state.spectator.is_none() => {
                        state.swarm = Swarm::new()
                    }
                    PhysicalKey::Code(KeyCode::KeyE) if state.spectator.is_none() => {
                        state
                            .renderer
                            .set_environment(state.renderer.environment().next());
                        log::info!("Environment: {:?}", state.renderer.environment());
                    }
                    PhysicalKey::Code(KeyCode::Space) if state.spectator.is_none() => {
                        state.paused = !state.paused;
                        state.window.set_title(if state.paused {
                            "Tomori [paused] — Space to resume"
                        } else {
                            TITLE
                        });
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                if self.pipewire {
                    // This mode is driven by the timer, even if the window is hidden.
                    return;
                }
                if state.occluded || state.size.width == 0 || state.size.height == 0 {
                    return;
                }
                if let Err(error) = state.draw() {
                    self.error = Some(error);
                    event_loop.exit();
                    return;
                }
                state.window.request_redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(next) = self.next_video_frame else {
            return;
        };
        let now = Instant::now();
        if now >= next {
            if let Some(state) = &mut self.state
                && let Err(error) = state.draw()
            {
                self.error = Some(error);
                event_loop.exit();
                return;
            }
            // Do not catch up with bursts after stalls.
            self.next_video_frame = Some(
                Instant::now()
                    + Duration::from_secs_f64(1.0 / f64::from(crate::pipewire_video::FPS)),
            );
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_video_frame.unwrap()));
    }
}

struct State {
    renderer: Renderer,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    window: Arc<Window>,
    size: PhysicalSize<u32>,
    swarm: Swarm,
    camera: Camera,
    cursor: Vec2,
    attracting: bool,
    orbiting: bool,
    paused: bool,
    occluded: bool,
    last_frame: Instant,
    first_frame: bool,
    spectator: Option<SpectatorView>,
    video: Option<crate::pipewire_video::Video>,
}

impl State {
    async fn new(window: Arc<Window>, spectator: bool, pipewire: bool) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .context("finding a desktop Vulkan adapter")?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("desktop device"),
                ..Default::default()
            })
            .await?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .context("surface has no supported configuration")?;
        let caps = surface.get_capabilities(&adapter);
        config.format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(config.format);
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);
        let video = if pipewire {
            Some(crate::pipewire_video::Video::new(
                &device,
                Some(config.format),
            )?)
        } else {
            None
        };
        let render_format = if pipewire {
            crate::pipewire_video::FORMAT
        } else {
            config.format
        };
        let (render_width, render_height) = if pipewire {
            (crate::pipewire_video::WIDTH, crate::pipewire_video::HEIGHT)
        } else {
            (config.width, config.height)
        };
        let renderer = Renderer::new(&device, &queue, render_format, render_width, render_height);
        let spectator = spectator.then(|| SpectatorView::new(&device, render_format));
        log::info!(
            "Desktop adapter: {} — {}x{}",
            adapter.get_info().name,
            config.width,
            config.height
        );
        if spectator.is_some() {
            log::info!("Spectator: Esc closes the borderless window.");
        } else {
            log::info!(
                "Left mouse: attract; right drag: orbit; wheel: zoom; R: reset; Space: pause; Esc: quit."
            );
        }
        Ok(Self {
            renderer,
            surface,
            device,
            queue,
            config,
            window,
            size,
            swarm: Swarm::new(),
            camera: Camera::default(),
            cursor: Vec2::new(size.width as f32, size.height as f32) * 0.5,
            attracting: false,
            orbiting: false,
            paused: false,
            occluded: false,
            last_frame: Instant::now(),
            first_frame: true,
            spectator,
            video,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.size = size;
        self.last_frame = Instant::now();
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        if self.video.is_some() {
            log::info!(
                "Spectator preview size: {}x{} physical pixels (PipeWire output remains {}x{})",
                size.width,
                size.height,
                crate::pipewire_video::WIDTH,
                crate::pipewire_video::HEIGHT
            );
        } else if self.spectator.is_some() {
            log::info!(
                "Spectator render size: {}x{} physical pixels",
                size.width,
                size.height
            );
        }
        self.surface.configure(&self.device, &self.config);
        if self.video.is_none() {
            self.renderer.resize(&self.device, size.width, size.height);
        }
        self.window.request_redraw();
    }

    fn draw(&mut self) -> Result<()> {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        let frame = if self.occluded || self.size.width == 0 || self.size.height == 0 {
            None
        } else {
            match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame)
                | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Some(frame),
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    self.surface.configure(&self.device, &self.config);
                    None
                }
                wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                    None
                }
                other => anyhow::bail!("Could not acquire desktop frame: {other:?}"),
            }
        };
        if frame.is_none() && self.video.is_none() {
            return Ok(());
        }
        let size = if self.video.is_some() {
            Vec2::new(
                crate::pipewire_video::WIDTH as f32,
                crate::pipewire_video::HEIGHT as f32,
            )
        } else {
            Vec2::new(self.size.width as f32, self.size.height as f32)
        };
        let matrix = self.camera.view_projection(size.x / size.y);
        let attractor = self
            .attracting
            .then(|| self.camera.attractor(self.cursor, size));
        let target = match &self.video {
            Some(video) => video.view.clone(),
            None => frame
                .as_ref()
                .unwrap()
                .texture
                .create_view(&Default::default()),
        };
        if let Some(spectator) = &mut self.spectator {
            spectator.render(RenderInputs {
                device: &self.device,
                queue: &self.queue,
                renderer: &mut self.renderer,
                swarm: &mut self.swarm,
                target,
                aspect: size.x / size.y,
                waiting_projection: matrix,
            });
        } else {
            if !self.paused {
                self.swarm.step(dt, attractor);
            }
            self.renderer
                .update(&self.queue, &self.swarm, attractor, self.attracting);
            self.renderer.render(
                &self.device,
                &self.queue,
                &[target],
                &[matrix],
                Mat4::IDENTITY,
                None,
            );
        }
        if let Some(video) = &mut self.video {
            if let Err(error) = video.capture(&self.device, &self.queue) {
                log::error!("PipeWire output stopped: {error:#}; preview remains available");
            }
            if let Some(frame) = &frame {
                let preview = frame.texture.create_view(&Default::default());
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("spectator preview"),
                        });
                video.blitter.as_ref().expect("preview blitter").copy(
                    &self.device,
                    &mut encoder,
                    &video.view,
                    &preview,
                );
                self.queue.submit([encoder.finish()]);
            }
        }
        if let Some(frame) = frame {
            self.window.pre_present_notify();
            self.queue.present(frame);
        }
        if self.first_frame {
            log::info!("Presented first desktop frame");
            self.first_frame = false;
        }
        Ok(())
    }
}

pub(crate) struct Camera {
    yaw: f32,
    pitch: f32,
    distance: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: 0.18,
            distance: 3.5,
        }
    }
}

impl Camera {
    fn position(&self) -> Vec3 {
        CENTER
            + self.distance
                * Vec3::new(
                    self.yaw.sin() * self.pitch.cos(),
                    self.pitch.sin(),
                    self.yaw.cos() * self.pitch.cos(),
                )
    }

    pub(crate) fn view_projection(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(60_f32.to_radians(), aspect, 0.05, 50.0)
            * Mat4::look_at_rh(self.position(), CENTER, Vec3::Y)
    }

    fn orbit(&mut self, delta: Vec2) {
        self.yaw -= delta.x * 0.006;
        self.pitch = (self.pitch + delta.y * 0.006).clamp(-1.2, 1.2);
    }

    fn zoom(&mut self, amount: f32) {
        self.distance = (self.distance * (-amount).exp()).clamp(1.0, 10.0);
    }

    fn attractor(&self, cursor: Vec2, size: Vec2) -> Vec3 {
        let ndc = Vec2::new(cursor.x / size.x * 2.0 - 1.0, 1.0 - cursor.y / size.y * 2.0);
        let inverse = self.view_projection(size.x / size.y).inverse();
        let near = inverse.project_point3(ndc.extend(0.0));
        let far = inverse.project_point3(ndc.extend(1.0));
        let ray = (far - near).normalize();
        let normal = (self.position() - CENTER).normalize();
        // Intersect a camera-facing plane through the swarm's center.
        near + ray * (CENTER - near).dot(normal) / ray.dot(normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectator_requests_1080p_physical_pixels() {
        assert_eq!(
            initial_window_size(true),
            Size::Physical(PhysicalSize::new(1920, 1080))
        );
        assert_eq!(
            initial_window_size(false),
            Size::Logical(LogicalSize::new(1100.0, 760.0))
        );
    }

    #[test]
    fn center_cursor_targets_swarm_even_after_orbit_and_zoom() {
        let mut camera = Camera::default();
        let size = Vec2::new(1100.0, 760.0);
        for delta in [
            Vec2::ZERO,
            Vec2::new(230.0, -100.0),
            Vec2::new(-400.0, 300.0),
        ] {
            camera.orbit(delta);
            camera.zoom(0.4);
            assert!(camera.attractor(size * 0.5, size).distance(CENTER) < 0.001);
        }
    }

    #[test]
    fn cursor_motion_maps_right_and_up_on_the_attraction_plane() {
        let camera = Camera::default();
        let size = Vec2::new(1100.0, 760.0);
        let center = size * 0.5;
        let right = camera.attractor(center + Vec2::new(100.0, 0.0), size);
        let up = camera.attractor(center - Vec2::new(0.0, 100.0), size);
        let normal = (camera.position() - CENTER).normalize();
        assert!(right.x > CENTER.x);
        assert!(up.y > CENTER.y);
        assert!((right - CENTER).dot(normal).abs() < 0.001);
        assert!((up - CENTER).dot(normal).abs() < 0.001);
    }

    #[test]
    fn camera_stays_finite_at_input_limits() {
        let mut camera = Camera::default();
        for amount in [-100.0, 100.0] {
            camera.zoom(amount);
            camera.orbit(Vec2::splat(amount * 100.0));
            assert!((1.0..=10.0).contains(&camera.distance));
            assert!(camera.view_projection(1.0).is_finite());
            for cursor in [Vec2::ZERO, Vec2::splat(1000.0)] {
                assert!(camera.attractor(cursor, Vec2::splat(1000.0)).is_finite());
            }
        }
    }
}
