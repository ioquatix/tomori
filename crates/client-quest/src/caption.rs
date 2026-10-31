// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A small, head-relative text panel. Bitmap glyphs need no font files/atlas.
use bytemuck::{Pod, Zeroable};
use font8x8::{BASIC_FONTS, UnicodeFonts};
use glam::Mat4;
use std::time::Duration;

pub const COLUMNS: usize = 40;
pub const ROWS: usize = 12;
const PIXEL: f32 = 0.0034;
const MAX_VERTICES: usize = (COLUMNS * ROWS * 64 + 1) * 6;
const MAX_RECORDING_VERTICES: usize = (COLUMNS * 2 * 64 + 4) * 6;
const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x3];

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 3],
}

pub struct Caption {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    cameras: [wgpu::Buffer; 2],
    bindings: [wgpu::BindGroup; 2],
    count: u32,
    text: String,
    recording_vertices: wgpu::Buffer,
    recording_count: u32,
    recording: Option<RecordingProgress>,
}

impl Caption {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("caption.wgsl"));
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("caption camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            }],
        });
        let cameras = std::array::from_fn(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("caption eye transform"),
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let bindings = std::array::from_fn(|eye| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("caption eye binding"),
                layout: &layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: cameras[eye].as_entire_binding(),
                }],
            })
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("caption pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("caption pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &ATTRIBUTES,
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caption glyphs"),
            size: (MAX_VERTICES * size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let recording_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("recording progress bar"),
            size: (MAX_RECORDING_VERTICES * size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            vertices,
            cameras,
            bindings,
            count: 0,
            text: String::new(),
            recording_vertices,
            recording_count: 0,
            recording: None,
        }
    }

    /// A separate mesh keeps the timer visible in every chat/voice view without
    /// replacing history or rebuilding all glyphs on each animation update.
    pub fn set_recording_progress(
        &mut self,
        queue: &wgpu::Queue,
        elapsed: Option<Duration>,
        maximum: Duration,
    ) {
        let progress = elapsed.map(|elapsed| RecordingProgress::new(elapsed, maximum));
        if progress == self.recording {
            return;
        }
        self.recording = progress;
        if let Some(progress) = progress {
            let mesh = recording_mesh(progress);
            self.recording_count = mesh.len() as u32;
            queue.write_buffer(&self.recording_vertices, 0, bytemuck::cast_slice(&mesh));
        } else {
            self.recording_count = 0;
        }
    }

    pub fn set_text(&mut self, queue: &wgpu::Queue, text: &str) {
        if self.text == text {
            return;
        }
        self.text = text.to_owned();
        let mesh = mesh(text);
        self.count = mesh.len() as u32;
        queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&mesh));
    }

    pub fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        eye: usize,
        world_to_clip: Mat4,
        head: Mat4,
    ) {
        let model = head * Mat4::from_translation(glam::Vec3::new(0.0, -0.32, -1.7));
        queue.write_buffer(
            &self.cameras[eye],
            0,
            bytemuck::cast_slice(&(world_to_clip * model).to_cols_array()),
        );
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("voice caption overlay"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bindings[eye], &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..self.count, 0..1);
        if self.recording_count > 0 {
            pass.set_vertex_buffer(0, self.recording_vertices.slice(..));
            pass.draw(0..self.recording_count, 0..1);
        }
    }
}

fn quad(vertices: &mut Vec<Vertex>, x: f32, y: f32, width: f32, height: f32, color: [f32; 3]) {
    for position in [
        [x, y],
        [x + width, y],
        [x, y - height],
        [x + width, y],
        [x + width, y - height],
        [x, y - height],
    ] {
        vertices.push(Vertex { position, color });
    }
}

fn mesh(text: &str) -> Vec<Vertex> {
    let mut vertices = Vec::new();
    quad(
        &mut vertices,
        -0.68,
        0.28,
        1.36,
        0.56,
        [0.008, 0.018, 0.035],
    );
    for (row, line) in wrap(text).iter().enumerate() {
        let color = if row == 0 {
            [0.12, 0.85, 1.0]
        } else {
            [0.92, 0.96, 1.0]
        };
        glyph_line(
            &mut vertices,
            line,
            -0.62,
            0.24 - (row * 12) as f32 * PIXEL,
            color,
        );
    }
    assert!(vertices.len() <= MAX_VERTICES);
    vertices
}

fn glyph_line(vertices: &mut Vec<Vertex>, text: &str, left: f32, top: f32, color: [f32; 3]) {
    for (column, ch) in text.chars().take(COLUMNS).enumerate() {
        let glyph = BASIC_FONTS.get(ch).unwrap_or([0; 8]);
        for (y, bits) in glyph.iter().enumerate() {
            for x in 0..8 {
                if bits & (1 << x) != 0 {
                    quad(
                        vertices,
                        left + (column * 9 + x) as f32 * PIXEL,
                        top - y as f32 * PIXEL,
                        PIXEL,
                        PIXEL,
                        color,
                    );
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RecordingProgress {
    elapsed_ms: u64,
    maximum_ms: u64,
}

impl RecordingProgress {
    fn new(elapsed: Duration, maximum: Duration) -> Self {
        let maximum_ms = maximum.as_millis().min(u64::MAX as u128) as u64;
        // A zero maximum represents a live, unbounded recording.
        let elapsed_ms = if maximum_ms == 0 {
            elapsed.as_millis().min(u64::MAX as u128) as u64
        } else {
            elapsed.as_millis().min(maximum_ms as u128) as u64
        };
        // Animate at 10 Hz, with an exact completed state at the cutoff.
        let elapsed_ms = if elapsed_ms == maximum_ms {
            elapsed_ms
        } else {
            elapsed_ms / 100 * 100
        };
        Self {
            elapsed_ms,
            maximum_ms,
        }
    }

    fn fraction(self) -> f32 {
        if self.maximum_ms == 0 {
            1.0
        } else {
            self.elapsed_ms as f32 / self.maximum_ms as f32
        }
    }

    fn remaining_ms(self) -> u64 {
        self.maximum_ms.saturating_sub(self.elapsed_ms)
    }

    fn color(self) -> [f32; 3] {
        if self.maximum_ms == 0 {
            return [0.1, 0.85, 0.55];
        }
        match self.remaining_ms() {
            0..=5_000 => [1.0, 0.12, 0.06],
            5_001..=10_000 => [1.0, 0.6, 0.06],
            _ => [0.1, 0.85, 0.55],
        }
    }

    fn label(self) -> String {
        fn clock(seconds: u64) -> String {
            format!("{:02}:{:02}", seconds / 60, seconds % 60)
        }
        if self.maximum_ms == 0 {
            return format!("REC {} | TO THE AGENT", clock(self.elapsed_ms / 1000));
        }
        format!(
            "REC {} / {} | {} left",
            clock(self.elapsed_ms / 1000),
            clock(self.maximum_ms.div_ceil(1000)),
            clock(self.remaining_ms().div_ceil(1000))
        )
    }
}

fn recording_mesh(progress: RecordingProgress) -> Vec<Vertex> {
    let mut vertices = Vec::new();
    let color = progress.color();
    // Above the chat panel, rather than stealing a line of conversation text.
    quad(
        &mut vertices,
        -0.68,
        0.49,
        1.36,
        0.20,
        [0.008, 0.018, 0.035],
    );
    glyph_line(&mut vertices, &progress.label(), -0.62, 0.46, color);
    quad(
        &mut vertices,
        -0.62,
        0.405,
        1.24,
        0.035,
        [0.035, 0.07, 0.09],
    );
    if progress.fraction() > 0.0 {
        quad(
            &mut vertices,
            -0.62,
            0.405,
            1.24 * progress.fraction(),
            0.035,
            color,
        );
    }
    glyph_line(
        &mut vertices,
        "Release X when finished",
        -0.62,
        0.34,
        [0.92, 0.96, 1.0],
    );
    assert!(vertices.len() <= MAX_RECORDING_VERTICES);
    vertices
}

pub fn wrapped_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let word: String = word
                .chars()
                .map(|c| match c {
                    '\u{2018}' | '\u{2019}' => '\'',
                    '\u{201c}' | '\u{201d}' => '"',
                    '\u{2013}' | '\u{2014}' => '-',
                    c if c.is_ascii_graphic() => c,
                    _ => '?',
                })
                .collect();
            if !line.is_empty() && line.len() + 1 + word.len() > COLUMNS {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            for ch in word.chars() {
                if line.len() == COLUMNS {
                    lines.push(std::mem::take(&mut line));
                }
                line.push(ch);
            }
        }
        lines.push(line);
    }
    lines
}

fn wrap(text: &str) -> Vec<String> {
    let mut lines = wrapped_lines(text);
    if lines.len() > ROWS {
        lines.truncate(ROWS);
        let last = lines.last_mut().unwrap();
        last.truncate(COLUMNS - 3);
        last.push_str("...");
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_progress_tracks_elapsed_remaining_and_cutoff() {
        let maximum = Duration::from_secs(60);
        let progress = RecordingProgress::new(Duration::from_secs(15), maximum);
        assert_eq!(progress.fraction(), 0.25);
        assert_eq!(progress.remaining_ms(), 45_000);
        assert_eq!(progress.label(), "REC 00:15 / 01:00 | 00:45 left");
        let finished = RecordingProgress::new(Duration::from_secs(90), maximum);
        assert_eq!(finished.fraction(), 1.0);
        assert_eq!(finished.remaining_ms(), 0);
        assert_eq!(finished.label(), "REC 01:00 / 01:00 | 00:00 left");
        assert_eq!(
            RecordingProgress::new(Duration::ZERO, Duration::ZERO).fraction(),
            1.0
        );
        let live = RecordingProgress::new(Duration::from_secs(125), Duration::ZERO);
        assert_eq!(live.label(), "REC 02:05 | TO THE AGENT");
        assert_eq!(live.color(), [0.1, 0.85, 0.55]);
        assert_eq!(
            RecordingProgress::new(Duration::from_millis(1001), maximum),
            RecordingProgress::new(Duration::from_millis(1099), maximum)
        );
    }

    #[test]
    fn recording_bar_is_bounded_and_warns_near_the_limit() {
        let maximum = Duration::from_secs(60);
        let early = RecordingProgress::new(Duration::from_secs(10), maximum);
        let warning = RecordingProgress::new(Duration::from_secs(50), maximum);
        let critical = RecordingProgress::new(Duration::from_secs(55), maximum);
        assert_ne!(early.color(), warning.color());
        assert_ne!(warning.color(), critical.color());
        for seconds in [0, 1, 30, 50, 55, 60, 90] {
            let progress = RecordingProgress::new(Duration::from_secs(seconds), maximum);
            let mesh = recording_mesh(progress);
            assert!(mesh.len() <= MAX_RECORDING_VERTICES);
            assert!(
                mesh.iter()
                    .all(|v| v.position[0].abs() <= 0.681
                        && (0.289..=0.491).contains(&v.position[1]))
            );
        }
    }

    #[test]
    fn caption_layout_bounds_words_unicode_and_long_transcripts() {
        for text in [
            "".to_owned(),
            "VOICE\nReady.\n\nHello world!".to_owned(),
            "a".repeat(900),
            "Curly ‘quotes’ and emoji 🎤".repeat(100),
        ] {
            let lines = wrap(&text);
            assert!(lines.len() <= ROWS);
            assert!(
                lines
                    .iter()
                    .all(|line| line.is_ascii() && line.len() <= COLUMNS)
            );
            assert!(mesh(&text).len() <= MAX_VERTICES);
        }
        assert_eq!(wrap("hello\n\nworld"), ["hello", "", "world"]);
    }
}
