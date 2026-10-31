// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Shared Quest-scene rendering for the windowed and headless spectators.
use crate::{caption::Caption, graphics::Renderer, spectator, swarm::Swarm};
use glam::{Mat4, Vec3};
use std::{
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

pub(crate) struct SpectatorView {
    receiver: Receiver<spectator::Frame>,
    frame: Option<spectator::Frame>,
    last_update: Instant,
    caption: Caption,
}

pub(crate) struct RenderInputs<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub renderer: &'a mut Renderer,
    pub swarm: &'a mut Swarm,
    pub target: wgpu::TextureView,
    pub aspect: f32,
    pub waiting_projection: Mat4,
}

impl SpectatorView {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self {
            receiver: spectator::receive_frames(),
            frame: None,
            last_update: Instant::now(),
            caption: Caption::new(device, format),
        }
    }

    pub fn render(&mut self, input: RenderInputs<'_>) {
        for incoming in self.receiver.try_iter() {
            if self
                .frame
                .as_ref()
                .is_none_or(|old| old.sequence != incoming.sequence)
            {
                self.last_update = Instant::now();
                if self.frame.is_none() {
                    log::info!("Spectator receiving Quest scene");
                }
                self.frame = Some(incoming);
            }
        }
        if self.last_update.elapsed() > Duration::from_secs(3) {
            self.frame = None;
        }
        if let Some(incoming) = &self.frame {
            input.renderer.set_environment(incoming.environment);
            if input.swarm.agents.len() != incoming.agents.len() {
                *input.swarm = Swarm::new();
            }
            for (agent, position) in input.swarm.agents.iter_mut().zip(&incoming.agents) {
                agent.position = Vec3::from_array(*position);
            }
            input.swarm.visual = incoming.interaction;
            let right_marker = if incoming.controllers[crate::controller::RIGHT].is_some()
                || incoming.hands[crate::controller::RIGHT].is_some()
            {
                None
            } else {
                incoming.controller.map(Vec3::from_array)
            };
            input
                .renderer
                .update(input.queue, input.swarm, right_marker, incoming.attracting);
            input
                .renderer
                .set_presentation_page(incoming.presentation_page);
            input
                .renderer
                .set_presentation_content_page(incoming.presentation_content_page);
            input.renderer.update_workspace(
                input.device,
                input.queue,
                &incoming.worker_cubes,
                &incoming.worker_stations,
                &incoming.workspace,
            );
            input
                .renderer
                .update_controllers(input.queue, incoming.controllers, incoming.hands);
            self.caption.set_text(input.queue, &incoming.caption);
            self.caption.set_recording_progress(
                input.queue,
                incoming.recording_ms.map(Duration::from_millis),
                Duration::ZERO,
            );
            // Preserve the headset's vertical FOV instead of stretching its
            // portrait eye image to the spectator output aspect ratio.
            let aspect_correction =
                Mat4::from_scale(Vec3::new(incoming.view_aspect / input.aspect, 1.0, 1.0));
            let projection = aspect_correction * Mat4::from_cols_array(&incoming.projection);
            input.renderer.render(
                input.device,
                input.queue,
                &[input.target],
                &[projection],
                Mat4::from_cols_array(&incoming.scene_to_space),
                Some((&self.caption, Mat4::from_cols_array(&incoming.head))),
            );
        } else {
            input.swarm.agents.clear();
            input.swarm.visual = Default::default();
            input.renderer.update(input.queue, input.swarm, None, false);
            input.renderer.update_workspace(
                input.device,
                input.queue,
                &[],
                &[],
                &Default::default(),
            );
            input
                .renderer
                .update_controllers(input.queue, [None, None], [None, None]);
            self.caption.set_text(
                input.queue,
                "SPECTATOR\nWaiting for Quest scene.\n\nStart the PC bridge and Quest app.",
            );
            self.caption
                .set_recording_progress(input.queue, None, Duration::ZERO);
            input.renderer.render(
                input.device,
                input.queue,
                &[input.target],
                &[input.waiting_projection],
                Mat4::IDENTITY,
                Some((
                    &self.caption,
                    Mat4::from_translation(Vec3::new(0.0, 1.6, 0.0)),
                )),
            );
        }
    }
}
