// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Render the Quest spectator directly to PipeWire without a display surface.
use crate::{
    desktop::Camera,
    graphics::Renderer,
    pipewire_video::{self, Video},
    spectator_view::{RenderInputs, SpectatorView},
    swarm::Swarm,
};
use anyhow::{Context, Result};
use std::time::{Duration, Instant};

pub fn run() -> Result<()> {
    let mut state = pollster::block_on(State::new())?;
    let interval = Duration::from_secs_f64(1.0 / f64::from(pipewire_video::FPS));
    let mut next = Instant::now();
    log::info!("Headless spectator running; Ctrl-C stops the PipeWire source.");
    loop {
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        state.draw()?;
        next = next_frame_time(next, Instant::now(), interval);
    }
}

fn next_frame_time(previous: Instant, now: Instant, interval: Duration) -> Instant {
    // Keep an even cadence without issuing catch-up bursts after a slow frame.
    (previous + interval).max(now)
}

struct State {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    video: Video,
    spectator: SpectatorView,
    swarm: Swarm,
    waiting_projection: glam::Mat4,
}

impl State {
    async fn new() -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                ..Default::default()
            })
            .await
            .context("finding a headless Vulkan adapter")?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("headless spectator device"),
                ..Default::default()
            })
            .await
            .context("creating a headless spectator device")?;
        log::info!("Headless spectator adapter: {}", adapter.get_info().name);
        let renderer = Renderer::new(
            &device,
            &queue,
            pipewire_video::FORMAT,
            pipewire_video::WIDTH,
            pipewire_video::HEIGHT,
        );
        let spectator = SpectatorView::new(&device, pipewire_video::FORMAT);
        let video = Video::new(&device, None)?;
        let aspect = pipewire_video::WIDTH as f32 / pipewire_video::HEIGHT as f32;
        Ok(Self {
            device,
            queue,
            renderer,
            video,
            spectator,
            swarm: Swarm::new(),
            waiting_projection: Camera::default().view_projection(aspect),
        })
    }

    fn draw(&mut self) -> Result<()> {
        self.spectator.render(RenderInputs {
            device: &self.device,
            queue: &self.queue,
            renderer: &mut self.renderer,
            swarm: &mut self.swarm,
            target: self.video.view.clone(),
            aspect: pipewire_video::WIDTH as f32 / pipewire_video::HEIGHT as f32,
            waiting_projection: self.waiting_projection,
        });
        self.video
            .capture(&self.device, &self.queue)
            .context("publishing headless spectator frame")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_schedule_keeps_cadence_and_drops_missed_ticks() {
        let start = Instant::now();
        let interval = Duration::from_millis(33);
        assert_eq!(
            next_frame_time(start, start + Duration::from_millis(5), interval),
            start + interval
        );
        assert_eq!(
            next_frame_time(start, start + Duration::from_millis(200), interval),
            start + Duration::from_millis(200)
        );
    }
}
