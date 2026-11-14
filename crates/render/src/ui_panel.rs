// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Panels of styled text in their own local space, drawn in the scene pass with depth testing and alpha blending, so that a hand or controller in front of one covers it, and then, where a panel's backing is opaque enough, its backing again into depth alone, so that whatever is behind it is hidden though drawn after it (see [`occlusion`](crate::occlusion)).
//!
//! A [`Canvas`] is what a panel shows: rows of [`ui_text`](crate::ui_text) cells on a translucent backing, with a colour per span, row backgrounds, speaker bars, meters and shapes. [`Canvas::mesh`] turns it into quads in the panel's plane, in metres, centred on the panel's origin, with +X right and +Y up, its glyphs drawn from the [`Glyphs`](crate::glyphs::Glyphs) atlas, its shapes from the atlas's shape tiles and its filled quads from its solid block; it needs no GPU, so that layouts are tested without one. A [`Panel`] holds one canvas on the GPU and rebuilds its mesh only when the canvas changes, so that moving it costs one matrix per eye per frame: handed the same shared canvas again, it compares nothing, and handed one that differs only in its frame's colour, it rewrites only its frame's vertices.
use std::sync::Arc;

use glam::Mat4;

pub use tomori_scene::canvas::{Canvas, Vertex};

const ATTRIBUTES: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4, 2 => Float32x2, 3 => Float32];

/// The pipelines every panel is drawn with, in the scene pass: depth tested against the scene without writing depth, and alpha blended, so that panels are translucent and a nearer hand covers them; and then its backing into depth alone.
pub struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    occluder: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    views: crate::multiview::Views,
}

impl Pipeline {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        glyphs: &crate::glyphs::Glyphs,
        samples: u32,
    ) -> Self {
        Self::with_layout(
            device,
            format,
            glyphs.layout(),
            samples,
            crate::multiview::Views::One,
        )
    }

    /// As [`Pipeline::new`], drawing from the atlas a scenery is given in its
    /// [`Target`](crate::scenery::Target), so that a scenery draws panels and labels of its own in the eye's pass. See
    /// `design/text.md`.
    pub fn shared(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        glyphs: &crate::glyphs::SharedGlyphs,
        samples: u32,
    ) -> Self {
        Self::with_views(
            device,
            format,
            glyphs,
            samples,
            crate::multiview::Views::One,
        )
    }

    /// As [`Pipeline::shared`], drawn in passes of `views`, as a renderer drawing both eyes at once draws its own; see
    /// [`crate::multiview`].
    pub fn with_views(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        glyphs: &crate::glyphs::SharedGlyphs,
        samples: u32,
        views: crate::multiview::Views,
    ) -> Self {
        Self::with_layout(device, format, glyphs.layout(), samples, views)
    }

    /// As [`Pipeline::shared`], for a scenery's `target`: its format, samples, atlas and views, so that a scenery draws
    /// panels and labels of its own in the eyes' passes however many views they draw.
    pub fn for_target(target: &crate::scenery::Target<'_>) -> Self {
        Self::with_views(
            target.device,
            target.format,
            &target.glyphs,
            target.samples,
            target.views,
        )
    }

    fn with_layout(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        glyphs: &wgpu::BindGroupLayout,
        samples: u32,
        views: crate::multiview::Views,
    ) -> Self {
        let shader = crate::multiview::shader(
            device,
            "ui_panel.wgsl",
            include_str!("ui_panel.wgsl"),
            views,
        );
        let layout = transforms_layout(device, views);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("panel pipeline layout"),
            bind_group_layouts: &[Some(&layout), Some(glyphs)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("panel pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(VERTICES)],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: crate::glyphs::compilation(format),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
            multiview_mask: views.mask(),
            cache: None,
        });
        let occluder = crate::occlusion::occluder_with_views(
            device,
            "panel backing occluder",
            &pipeline_layout,
            &shader,
            VERTICES,
            format,
            samples,
            views,
        );
        Self {
            pipeline,
            occluder,
            layout,
            views,
        }
    }

    /// The layout of a panel's transforms, by which an image over it is drawn too.
    pub(crate) fn transforms(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// Draw panels from here on with this pipeline and the glyph atlas `glyphs`, from [`Glyphs::binding`](crate::glyphs::Glyphs::binding).
    pub fn set(&self, pass: &mut wgpu::RenderPass<'_>, glyphs: &wgpu::BindGroup) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(1, glyphs, &[]);
    }
}

const VERTICES: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<Vertex>() as u64,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &ATTRIBUTES,
};

/// The bytes of a panel's uniform for an eye, as `ui_panel.wgsl` and `panel_image.wgsl` read it: its transform from its
/// plane to clip space, its model from its plane to the eye's space, the scene's haze and where the eye is.
const SEEN_SIZE: u64 = 160;

/// How an eye sees the space its panels are in: its matrix from that space to its clip space, where it is in it, and the
/// scene's haze, which the panels are faded by as everything else is. See `design/rendering.md`.
#[derive(Clone, Copy, Debug)]
pub struct Seen {
    matrix: Mat4,
    eye: glam::Vec3,
    haze: tomori_assets::haze::Haze,
}

impl Seen {
    /// The eye whose matrix from the panels' space to its clip space is `matrix`, through `haze`.
    pub fn new(matrix: Mat4, haze: tomori_assets::haze::Haze) -> Self {
        Self {
            matrix,
            eye: crate::scenery::eye(matrix),
            haze,
        }
    }
}

/// The layout of a panel's uniform for an eye, [`Panel::prepare_seen`], which its fragments read too, for the haze.
pub(crate) fn transforms_layout(
    device: &wgpu::Device,
    views: crate::multiview::Views,
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("panel transform layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: crate::multiview::Uniforms::binding_size(SEEN_SIZE, views),
            },
            count: None,
        }],
    })
}

/// One panel's canvas on the GPU, with a transform for each eye.
pub struct Panel {
    device: wgpu::Device,
    vertices: wgpu::Buffer,
    capacity: usize,
    /// The most vertices its buffer grows to hold, for a face larger than it holds now: as many as it holds, for a
    /// panel whose buffer never grows.
    most: usize,
    count: u32,
    /// Each eye's transform, and their binding for each pass.
    cameras: crate::multiview::Uniforms,
    bindings: Vec<wgpu::BindGroup>,
    canvas: Option<Arc<Canvas>>,
    /// Whether the mesh has every glyph, rather than lacking some that waited for a later frame's budget.
    complete: bool,
    /// Whether the mesh last shown was cut to the buffer, so that a face too large is warned of once, not every time
    /// it changes.
    cut: bool,
    /// Whether the mesh's first quad is a backing opaque enough to hide what is behind it.
    occludes: bool,
    /// The image the canvas shows, by hash, which is drawn over it from [`Self::image_quad`] once its texture is ready.
    image: Option<String>,
    /// The image's quad, in the panel's plane, with where in the image each corner is.
    image_quad: wgpu::Buffer,
    /// How many times its mesh was built, and its frame alone recoloured.
    pub(crate) builds: (usize, usize),
    /// The box about its mesh and its image's quad, in its own plane, which the renderer culls it by.
    bounds: tomori_assets::bounds::Aabb,
}

impl Panel {
    /// A panel holding at most `capacity` vertices; a larger mesh is cut, and is a layout bug the tests catch.
    pub fn new(device: &wgpu::Device, pipeline: &Pipeline, capacity: usize) -> Self {
        let cameras = crate::multiview::Uniforms::new(
            device,
            "panel eye transform",
            SEEN_SIZE,
            pipeline.views,
        );
        let bindings = cameras
            .buffers()
            .iter()
            .map(|buffer| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("panel eye binding"),
                    layout: &pipeline.layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                })
            })
            .collect();
        let vertices = vertices(device, capacity);
        let image_quad = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("panel image quad"),
            size: 6 * 16,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            device: device.clone(),
            vertices,
            capacity,
            most: capacity,
            count: 0,
            cameras,
            bindings,
            canvas: None,
            complete: false,
            cut: false,
            occludes: false,
            image: None,
            image_quad,
            builds: (0, 0),
            bounds: tomori_assets::bounds::Aabb::EMPTY,
        }
    }

    /// The panel, its buffer grown, as a face larger than it holds is shown, to hold as many vertices as that face has,
    /// up to `most`, so that a face of many small cells, as a wall's content drawn smaller lays out, is drawn whole
    /// while every other keeps a buffer of its usual size.
    pub fn growing(mut self, most: usize) -> Self {
        self.most = most.max(self.capacity);
        self
    }

    /// Show `canvas`, rebuilding the mesh only if it changed, or if glyphs it needs were waiting for the atlas when it was last built.
    pub fn set(&mut self, queue: &wgpu::Queue, canvas: &Canvas) {
        if self.complete
            && self.canvas.as_deref().is_some_and(|shown| {
                shown == canvas
                    && std::mem::discriminant(&shown.kind) == std::mem::discriminant(&canvas.kind)
            })
        {
            return;
        }
        self.show(queue, &Arc::new(canvas.clone()));
    }

    /// Whether what it shows is a projected panel's face, as the frame log counts the panels apart from every other
    /// surface.
    pub fn panel_face(&self) -> bool {
        self.canvas
            .as_ref()
            .is_some_and(|canvas| matches!(canvas.kind, tomori_scene::canvas::Kind::PanelFace))
    }

    /// Show `canvas`, shared with whoever drew it: as [`Self::set`] does, but knowing it at once, without comparing it,
    /// when it is the very canvas shown already, and only recolouring its frame where that is all that changed.
    pub fn show(&mut self, queue: &wgpu::Queue, canvas: &Arc<Canvas>) {
        if let Some(shown) = self.canvas.as_ref().filter(|_| self.complete) {
            if Arc::ptr_eq(shown, canvas) {
                return;
            }
            if **shown == **canvas || self.reframe(queue, canvas) {
                self.canvas = Some(canvas.clone());
                return;
            }
        }
        self.build(queue, canvas);
        self.canvas = Some(canvas.clone());
    }

    /// Recolour its frame as `canvas`'s, if that is all `canvas` changes of what it shows, and say whether it did.
    fn reframe(&mut self, queue: &wgpu::Queue, canvas: &Canvas) -> bool {
        let shown = self.canvas.as_deref().expect("shows a canvas");
        if !canvas.reframes(shown) {
            return false;
        }
        let (start, frame) = canvas.frame_mesh().expect("framed");
        // A mesh cut short of its frame is built again.
        if start + frame.len() > self.count as usize {
            return false;
        }
        queue.write_buffer(
            &self.vertices,
            (start * size_of::<Vertex>()) as u64,
            bytemuck::cast_slice(&frame),
        );
        self.builds.1 += 1;
        true
    }

    /// Build `canvas`'s mesh and write it, with its image's quad.
    fn build(&mut self, queue: &wgpu::Queue, canvas: &Canvas) {
        self.builds.0 += 1;
        let waited = tomori_text::atlas::waited();
        let mut mesh = canvas.mesh();
        self.complete = tomori_text::atlas::waited() == waited;
        // A mesh larger than its buffer grows the buffer, where it may, at least twice as large, within its most, so
        // that a face growing by steps is not given a buffer at each.
        if mesh.len() > self.capacity && self.capacity < self.most {
            self.capacity = mesh.len().max(self.capacity * 2).min(self.most);
            self.vertices = vertices(&self.device, self.capacity);
        }
        // A mesh too large is cut, losing what is drawn last, its text; said once each time a panel's face first is.
        let cut = mesh.len() > self.capacity;
        if cut && !self.cut {
            log::warn!(
                "A panel's mesh of {} vertices was cut to the {} its buffer holds, losing its text.",
                mesh.len(),
                self.capacity
            );
        }
        self.cut = cut;
        if cut {
            mesh.truncate(self.capacity - self.capacity % 6);
        }
        self.count = mesh.len() as u32;
        // A canvas's mesh starts with its backing, unless the backing is wholly transparent.
        self.occludes = crate::occlusion::occludes(canvas.background[3]) && mesh.len() >= 6;
        if !mesh.is_empty() {
            queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&mesh));
        }
        let point = |x: f32, y: f32| glam::Vec3::new(x, y, 0.0);
        self.bounds = tomori_assets::bounds::Aabb::from_points(
            mesh.iter()
                .map(|vertex| point(vertex.position[0], vertex.position[1])),
        );
        self.image = canvas
            .image_rect()
            .map(|(hash, [left, bottom, right, top])| {
                self.bounds = self
                    .bounds
                    .with(point(left, bottom))
                    .with(point(right, top));
                let quad: [[f32; 4]; 6] = [
                    [left, top, 0.0, 0.0],
                    [right, top, 1.0, 0.0],
                    [left, bottom, 0.0, 1.0],
                    [right, top, 1.0, 0.0],
                    [right, bottom, 1.0, 1.0],
                    [left, bottom, 0.0, 1.0],
                ];
                queue.write_buffer(&self.image_quad, 0, bytemuck::cast_slice(&quad));
                hash.to_owned()
            });
    }

    /// The canvas it shows, for the tests.
    #[cfg(test)]
    pub(crate) fn shown(&self) -> Option<&Arc<Canvas>> {
        self.canvas.as_ref()
    }

    /// The box about what it draws, in its own plane, about its middle: empty while it draws nothing.
    pub fn bounds(&self) -> tomori_assets::bounds::Aabb {
        self.bounds
    }

    /// How many triangles it draws.
    pub fn triangles(&self) -> usize {
        self.count as usize / 3
    }

    /// The image its canvas shows, by hash, if any.
    pub fn image(&self) -> Option<&str> {
        self.image.as_deref()
    }

    /// Draw its image's quad for `eye` from `binding`, its texture, with the image pipeline already set on `pass`.
    pub(crate) fn draw_image(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        binding: &wgpu::BindGroup,
        eye: usize,
    ) {
        pass.set_bind_group(0, &self.bindings[eye], &[]);
        pass.set_bind_group(1, binding, &[]);
        pass.set_vertex_buffer(0, self.image_quad.slice(..));
        pass.draw(0..6, 0..1);
    }

    /// Write `eye`'s transform, from the panel's plane to clip space, before the pass that draws it, without haze.
    pub fn prepare(&self, queue: &wgpu::Queue, eye: usize, transform: Mat4) {
        self.prepare_seen(
            queue,
            eye,
            &Seen::new(transform, tomori_assets::haze::Haze::NONE),
            Mat4::IDENTITY,
        );
    }

    /// Write how `eye` sees the panel, `seen`, with `model` from its plane to the space `seen` is in, before the pass
    /// that draws it: its transform to clip space, and where it is from the eye, for the scene's haze.
    pub fn prepare_seen(&self, queue: &wgpu::Queue, eye: usize, seen: &Seen, model: Mat4) {
        let mut uniform = [0.0f32; 40];
        uniform[..16].copy_from_slice(&(seen.matrix * model).to_cols_array());
        uniform[16..32].copy_from_slice(&model.to_cols_array());
        uniform[32..36].copy_from_slice(&seen.haze.to_array());
        uniform[36..39].copy_from_slice(&seen.eye.to_array());
        self.cameras
            .write(queue, eye, bytemuck::cast_slice(&uniform));
    }

    /// Draw for `eye`, or, in a pass drawing both, as eye 0, with [`Pipeline::set`] already called on `pass` with `pipeline`, which it is again once drawn: the
    /// panel, then its backing into depth alone, if it is opaque enough to hide what is behind it.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, pipeline: &Pipeline, eye: usize) {
        if self.count == 0 {
            return;
        }
        pass.set_bind_group(0, &self.bindings[eye], &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..self.count, 0..1);
        if self.occludes {
            pass.set_pipeline(&pipeline.occluder);
            pass.draw(0..6, 0..1);
            pass.set_pipeline(&pipeline.pipeline);
        }
    }
}

/// A panel's buffer of `capacity` vertices.
fn vertices(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("panel quads"),
        size: (capacity * size_of::<Vertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_text::{Row, Style};

    #[test]
    fn a_panel_built_while_glyphs_waited_is_built_again_until_it_has_them() {
        crate::test_gpu::with(|gpu| {
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let glyphs = crate::glyphs::Glyphs::new(&gpu.device);
            let pipeline = Pipeline::new(&gpu.device, format, &glyphs, 1);
            let mut panel = Panel::new(&gpu.device, &pipeline, 1_000);
            let mut canvas = Canvas::new(8, 0.001);
            let kanji = crate::test_support::undrawn('\u{9f9f}', 2);
            canvas
                .rows
                .push(Row::text(&kanji, Style::plain([1.0; 4]), 8));
            // This frame's budget is spent: the panel is drawn without its glyphs, and drawn again next frame.
            let budget = crate::test_support::Budgeted::spent();
            panel.set(&gpu.queue, &canvas);
            assert!(!panel.complete);
            let bare = panel.count;
            tomori_text::atlas::frame();
            panel.set(&gpu.queue, &canvas);
            assert!(!panel.complete);
            assert_eq!(panel.count, bare);
            // With the budget to draw them, it has them, and is not built again.
            drop(budget);
            panel.set(&gpu.queue, &canvas);
            assert!(panel.complete);
            assert_eq!(panel.count, bare + 12);
            let waited = tomori_text::atlas::waited();
            panel.set(&gpu.queue, &canvas);
            assert_eq!(tomori_text::atlas::waited(), waited);
        });
    }

    #[test]
    fn a_wall_s_card_grows_to_draw_whole_every_face_its_content_drawn_at_half_lays_out() {
        use crate::scene_graphics::{CARD_CAPACITY, CARD_MOST};
        use tomori_protocol::{Code, Place, Pose, Wall, WallKind};
        use tomori_scene::projection::{Cells, Fit, face};
        let words =
            "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod ".repeat(200);
        let lines: String = (0..200)
            .map(|line| {
                format!("    let value_{line} = compute(alpha, beta, gamma) + offset * {line};\n")
            })
            .collect();
        let place = Place::Wall {
            wall: "w".into(),
            slot: 0,
        };
        let text = tomori_protocol::Panel::new("text", "Notes", &words, place.clone());
        let code = tomori_protocol::Panel::new("code", "main.rs", &lines, place)
            .with_code(Code::new("rust", 200));
        crate::test_gpu::with(|gpu| {
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let glyphs = crate::glyphs::Glyphs::new(&gpu.device);
            let pipeline = Pipeline::new(&gpu.device, format, &glyphs, 1);
            // Filled to their cells, text and code in a standard slot and a tall one, 9:16, at half, are drawn whole.
            let mut largest = 0;
            for aspect in [None, Some(9.0 / 16.0)] {
                let wall = Wall::new("w", WallKind::Added, Pose::default())
                    .with_aspect(aspect)
                    .with_content_scale(Some(0.5));
                for panel in [&text, &code] {
                    let canvas = face(panel, false, false, Fit::Slot(Cells::of(&wall)));
                    let vertices = canvas.mesh().len();
                    largest = largest.max(vertices);
                    let mut card =
                        Panel::new(&gpu.device, &pipeline, CARD_CAPACITY).growing(CARD_MOST);
                    card.set(&gpu.queue, &canvas);
                    assert!(!card.cut, "{aspect:?} {}: {vertices}", panel.id);
                    assert_eq!(card.count as usize, vertices);
                }
            }
            // Beyond what a card holds at first, as a standard slot's code at half is.
            assert!(largest > CARD_CAPACITY, "{largest}");
            // A buffer grows at least twice as large, as a face needs, and never beyond its most, past which it cuts.
            let row = |columns: usize| {
                let mut canvas = Canvas::new(columns, 0.001);
                canvas.rows = vec![Row::new(vec![('#', Style::plain([1.0; 4])); columns])];
                canvas
            };
            let mut panel = Panel::new(&gpu.device, &pipeline, 64).growing(600);
            panel.set(&gpu.queue, &row(8));
            assert_eq!((panel.capacity, panel.cut), (64, false));
            panel.set(&gpu.queue, &row(12));
            let vertices = row(12).mesh().len();
            assert!(vertices > 64 && vertices <= 128, "{vertices}");
            assert_eq!((panel.capacity, panel.cut), (128, false));
            assert_eq!(panel.count as usize, vertices);
            panel.set(&gpu.queue, &row(200));
            assert_eq!((panel.capacity, panel.cut, panel.count), (600, true, 600));
            // Never smaller again, and one that never grows keeps its buffer.
            panel.set(&gpu.queue, &row(8));
            assert_eq!((panel.capacity, panel.cut), (600, false));
            let mut fixed = Panel::new(&gpu.device, &pipeline, 64);
            fixed.set(&gpu.queue, &row(12));
            assert_eq!((fixed.capacity, fixed.cut, fixed.count), (64, true, 60));
        });
    }

    #[test]
    fn a_panel_cuts_a_mesh_too_large_for_it_and_draws_nothing_when_empty() {
        crate::test_support::log_everything();
        crate::test_gpu::with(|gpu| {
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let glyphs = crate::glyphs::Glyphs::new(&gpu.device);
            let pipeline = Pipeline::new(&gpu.device, format, &glyphs, 1);
            let mut panel = Panel::new(&gpu.device, &pipeline, 12);
            let mut canvas = Canvas::new(8, 0.001);
            let mut row = Row::new(vec![('#', Style::plain([1.0; 4])); 8]);
            row.background = Some([0.2, 0.2, 0.2, 1.0]);
            canvas.rows.push(row);
            assert!(canvas.mesh().len() > 12);
            panel.set(&gpu.queue, &canvas);
            assert_eq!(panel.count, 12);
            // The same canvas again changes nothing.
            panel.set(&gpu.queue, &canvas);
            let mut empty = Canvas::new(1, 1.0);
            empty.background = [0.0; 4];
            panel.set(&gpu.queue, &empty);
            assert_eq!(panel.count, 0);
            let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("panel test"),
                size: wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = target.create_view(&Default::default());
            let depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("panel test depth"),
                size: wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let depth = depth.create_view(&Default::default());
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("panel test"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                glyphs.sync(&gpu.device, &gpu.queue);
                pipeline.set(&mut pass, &glyphs.binding());
                panel.draw(&mut pass, &pipeline, 0);
            }
            gpu.queue.submit([encoder.finish()]);
        });
    }

    /// `canvas` drawn face on, `scale` screen pixels to its font pixel and shifted `shift` of a pixel right and down, as
    /// a headset sees a panel at about a metre, its pixels nowhere on the screen's grid; with where its font pixel `(x,
    /// y)`, from its top-left corner, falls on the image.
    fn drawn_face_on(
        gpu: &crate::test_gpu::Gpu,
        canvas: &Canvas,
        scale: f32,
        shift: f32,
    ) -> (image::RgbaImage, impl Fn(f32, f32) -> (f32, f32) + use<>) {
        face_on_in(
            gpu,
            canvas,
            scale,
            shift,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        )
    }

    /// As [`drawn_face_on`], into a target of `format`: a linear one, such as `Rgba8Unorm`, holds each pixel's coverage.
    fn face_on_in(
        gpu: &crate::test_gpu::Gpu,
        canvas: &Canvas,
        scale: f32,
        shift: f32,
        format: wgpu::TextureFormat,
    ) -> (image::RgbaImage, impl Fn(f32, f32) -> (f32, f32) + use<>) {
        let (columns, rows) = canvas.size_in_pixels();
        // Rows read back are a multiple of 256 bytes.
        let (width, height) = (
            ((columns as f32 * scale) as u32 + 16).next_multiple_of(64),
            (rows as f32 * scale) as u32 + 16,
        );
        // From the panel's plane, in metres about its centre, to clip space: a font pixel `scale` screen pixels.
        let per_metre = scale / canvas.pixel;
        let shifted = 2.0 * shift / glam::Vec2::new(width as f32, -(height as f32));
        let transform = Mat4::from_translation(shifted.extend(0.0))
            * Mat4::from_scale(glam::Vec3::new(
                2.0 * per_metre / width as f32,
                2.0 * per_metre / height as f32,
                1.0,
            ));
        let image = drawn_by(gpu, canvas, &[transform], (width, height), format);
        let place = move |x: f32, y: f32| {
            (
                width as f32 / 2.0 + (x - columns as f32 / 2.0) * scale + shift,
                height as f32 / 2.0 + (y - rows as f32 / 2.0) * scale + shift,
            )
        };
        (image, place)
    }

    /// `canvas` drawn once by each of `transforms`, from its plane in metres about its centre to clip space, onto an
    /// image of `format` `width` by `height`, its width a multiple of 64, with depth, as the headset draws panels.
    fn drawn_by(
        gpu: &crate::test_gpu::Gpu,
        canvas: &Canvas,
        transforms: &[Mat4],
        size: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> image::RgbaImage {
        drawn_within(gpu, canvas, transforms, size, format, 100_000)
    }

    /// As [`drawn_by`], each panel's buffer holding `capacity` vertices, as the renderer's do.
    fn drawn_within(
        gpu: &crate::test_gpu::Gpu,
        canvas: &Canvas,
        transforms: &[Mat4],
        (width, height): (u32, u32),
        format: wgpu::TextureFormat,
        capacity: usize,
    ) -> image::RgbaImage {
        let glyphs = crate::glyphs::Glyphs::new(&gpu.device);
        let pipeline = Pipeline::new(&gpu.device, format, &glyphs, 1);
        let panels: Vec<Panel> = transforms
            .iter()
            .map(|&transform| {
                let mut panel = Panel::new(&gpu.device, &pipeline, capacity);
                panel.set(&gpu.queue, canvas);
                panel.prepare(&gpu.queue, 0, transform);
                panel
            })
            .collect();
        drawing(gpu, &glyphs, &pipeline, &panels, (width, height), format)
    }

    /// `panels`, already set and prepared, drawn with `pipeline` from `glyphs` onto an image of `format` `width` by
    /// `height`, with depth.
    fn drawing(
        gpu: &crate::test_gpu::Gpu,
        glyphs: &crate::glyphs::Glyphs,
        pipeline: &Pipeline,
        panels: &[Panel],
        (width, height): (u32, u32),
        format: wgpu::TextureFormat,
    ) -> image::RgbaImage {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let target = |format, usage, label| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let texture = target(
            format,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            "drawn",
        );
        let depth = target(
            wgpu::TextureFormat::Depth32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            "drawn depth",
        );
        glyphs.sync(&gpu.device, &gpu.queue);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("drawn"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &texture.create_view(&Default::default()),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth.create_view(&Default::default()),
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pipeline.set(&mut pass, &glyphs.binding());
            for panel in panels {
                panel.draw(&mut pass, pipeline, 0);
            }
        }
        gpu.queue.submit([encoder.finish()]);
        crate::test_support::read_back(&gpu.device, &gpu.queue, &texture, (width, height))
    }

    /// A panel's frame recoloured, as the one pointed at is, draws exactly what its face built afresh in that colour
    /// does, and the same face handed over again, shared, is not compared, nor built again.
    #[test]
    fn a_frame_recoloured_draws_what_its_face_built_afresh_does() {
        crate::test_gpu::with(|gpu| {
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let glyphs = crate::glyphs::Glyphs::new(&gpu.device);
            let pipeline = Pipeline::new(&gpu.device, format, &glyphs, 1);
            let panel = tomori_protocol::Panel::new(
                "panel-1",
                "Steps",
                "- [x] Lay it out once\n- [ ] Draw it every frame\n\nThen **nothing** else.",
                tomori_protocol::Place::Wall {
                    wall: tomori_protocol::TOMORI_WALL.into(),
                    slot: 0,
                },
            );
            let fit = tomori_scene::projection::Fit::SLOT;
            let plain = Arc::new(tomori_scene::projection::face(&panel, false, false, fit));
            let pointed = Arc::new(tomori_scene::projection::face(&panel, true, false, fit));
            assert_ne!(plain.border, pointed.border);
            let (columns, rows) = plain.size_in_pixels();
            let (width, height) = ((columns as u32 * 2).next_multiple_of(64), rows as u32 * 2);
            let transform = Mat4::from_scale(glam::Vec3::new(
                2.0 * 2.0 / plain.pixel / width as f32,
                2.0 * 2.0 / plain.pixel / height as f32,
                1.0,
            ));
            let mut recoloured = Panel::new(&gpu.device, &pipeline, 100_000);
            recoloured.show(&gpu.queue, &plain);
            recoloured.show(&gpu.queue, &plain);
            recoloured.show(&gpu.queue, &pointed);
            assert_eq!(recoloured.builds, (1, 1));
            let mut afresh = Panel::new(&gpu.device, &pipeline, 100_000);
            afresh.show(&gpu.queue, &pointed);
            for panel in [&recoloured, &afresh] {
                panel.prepare(&gpu.queue, 0, transform);
            }
            let size = (width, height);
            let recoloured = drawing(&gpu, &glyphs, &pipeline, &[recoloured], size, format);
            let afresh = drawing(&gpu, &glyphs, &pipeline, &[afresh], size, format);
            assert!(
                recoloured.pixels().any(|pixel| pixel.0[1] > 200),
                "the frame shows"
            );
            assert_eq!(recoloured, afresh);
        });
    }

    #[test]
    fn a_mesh_cut_short_of_its_frame_is_built_again_rather_than_recoloured() {
        crate::test_support::log_everything();
        crate::test_gpu::with(|gpu| {
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let glyphs = crate::glyphs::Glyphs::new(&gpu.device);
            let pipeline = Pipeline::new(&gpu.device, format, &glyphs, 1);
            let mut panel = Panel::new(&gpu.device, &pipeline, 12);
            let mut canvas = Canvas::new(4, 0.001);
            canvas.border = Some([0.3, 0.4, 0.5, 0.9]);
            panel.show(&gpu.queue, &Arc::new(canvas.clone()));
            canvas.border = Some([0.35, 0.95, 1.0, 1.0]);
            panel.show(&gpu.queue, &Arc::new(canvas));
            assert_eq!(panel.builds, (2, 0));
        });
    }

    /// How bright `image` is at `(x, y)`, the sum of its channels.
    fn brightness(image: &image::RgbaImage, x: u32, y: u32) -> u32 {
        image.get_pixel(x, y).0[..3]
            .iter()
            .map(|&c| u32::from(c))
            .sum()
    }

    /// How much a line adds to `image` through each pixel of `along`, summed over the pixels `across` it, above the
    /// darkest of them; and the most of those over the least, which a line as bright all along keeps near one.
    fn evenness(
        image: &image::RgbaImage,
        along: std::ops::Range<u32>,
        across: std::ops::Range<u32>,
        at: impl Fn(u32, u32) -> (u32, u32),
    ) -> (Vec<u32>, f32) {
        let added: Vec<u32> = along
            .map(|step| {
                let values: Vec<u32> = across
                    .clone()
                    .map(|side| {
                        let (x, y) = at(step, side);
                        brightness(image, x, y)
                    })
                    .collect();
                let least = values.iter().copied().min().unwrap_or_default();
                values.iter().map(|value| value - least).sum()
            })
            .collect();
        let most = added.iter().copied().max().unwrap_or_default();
        let least = added.iter().copied().min().unwrap_or_default().max(1);
        (added, most as f32 / least as f32)
    }

    /// Set `TOMORI_LIST_PREVIEW` to a PNG path to save what this renders: an environment's queue on a panel in a wall's
    /// slot, face on, its rows drawn as rows: one highlighted on its bar and just chosen, one pointed at, each in a tone,
    /// with its strip and mark, some with a value's thin bar, and, longer than its face, which of its rows show.
    #[test]
    fn a_list_s_rows_are_drawn_with_their_highlight_tones_and_values() {
        use tomori_protocol::{ListRow, chart::Tone};
        crate::test_gpu::with(|gpu| {
            let rows: [(&str, &str, Tone, Option<f32>); 16] = [
                (
                    "pod112",
                    "pod112 · 41 waiting · scaling",
                    Tone::Bad,
                    Some(0.82),
                ),
                ("pod9", "pod9 · 23 waiting", Tone::Warn, Some(0.46)),
                ("pod7", "pod7 · 12 waiting", Tone::Warn, Some(0.24)),
                ("pod31", "pod31 · 3 waiting", Tone::Good, Some(0.06)),
                ("pod4", "pod4 · idle", Tone::Good, Some(0.0)),
                (
                    "pod12",
                    "pod12 · draining, its last jobs moving to pod31 and pod4",
                    Tone::Neutral,
                    None,
                ),
                ("pod5", "pod5 · 2 waiting", Tone::Neutral, Some(0.04)),
                ("pod6", "pod6 · offline", Tone::Bad, None),
                ("pod8", "pod8 · 1 waiting", Tone::Good, Some(0.02)),
                ("pod10", "pod10 · 9 waiting", Tone::Neutral, Some(0.18)),
                ("pod11", "pod11 · 5 waiting", Tone::Good, Some(0.1)),
                ("pod13", "pod13 · 7 waiting", Tone::Warn, Some(0.14)),
                ("pod14", "pod14 · idle", Tone::Good, Some(0.0)),
                ("pod15", "pod15 · 2 waiting", Tone::Neutral, Some(0.04)),
                ("pod16", "pod16 · 4 waiting", Tone::Neutral, Some(0.08)),
                ("pod17", "pod17 · 1 waiting", Tone::Good, Some(0.02)),
            ];
            let body = rows
                .iter()
                .enumerate()
                .map(|(index, (_, text, ..))| {
                    let pointer = if index == 1 { "**▸** " } else { "" };
                    format!("- {pointer}{text}")
                })
                .collect::<Vec<_>>()
                .join("\n");
            let panel = tomori_protocol::Panel::new(
                "panel-1",
                "Attention",
                body,
                tomori_protocol::Place::Wall {
                    wall: tomori_protocol::TOMORI_WALL.into(),
                    slot: 0,
                },
            )
            .with_rows(
                rows.iter()
                    .enumerate()
                    .map(|(index, (id, _, tone, value))| {
                        let row = ListRow::new(*id, true)
                            .with_highlight(index == 1)
                            .with_tone(*tone);
                        match value {
                            Some(value) => row.with_value(*value),
                            None => row,
                        }
                    })
                    .collect(),
            );
            let canvas = tomori_scene::projection::list_face(
                &panel,
                tomori_scene::projection::Fit::SLOT,
                0,
                (Some("pod31"), Some("pod9")),
            );
            let about: String = canvas.rows[1].cells.iter().map(|(ch, _)| *ch).collect();
            assert!(about.starts_with("rows 1–"), "{about}");
            let (image, _) = drawn_face_on(&gpu, &canvas, 2.3, 0.37);
            crate::test_support::preview("TOMORI_LIST_PREVIEW", &image);
            // Each tone's colour shows, in its strips, marks and values.
            let near = |color: [f32; 4]| {
                let target = color.map(|channel| (channel.powf(1.0 / 2.2) * 255.0) as i32);
                image
                    .pixels()
                    .filter(|pixel| {
                        (0..3).all(|at| (i32::from(pixel.0[at]) - target[at]).abs() < 40)
                    })
                    .count()
            };
            let shown = [Tone::Good, Tone::Warn, Tone::Bad]
                .map(|tone| near(tomori_scene::chart::tone(tone)) > 50);
            assert_eq!(shown, [true; 3]);
        });
    }

    /// Set `TOMORI_LIST_TABLE_PREVIEW` to a PNG path to save what this renders: an environment's observers laid out as a
    /// table on a panel in a wall's slot, face on, its figures in columns under a header, numbers right-aligned, each
    /// row's tone as a mark before it, its value under its Busy column, one highlighted and one pointed at.
    #[test]
    fn a_list_laid_out_as_a_table_is_drawn_in_columns_with_its_marks_and_values() {
        use tomori_protocol::{CellAlign, ListColumn, ListRow, chart::Tone};
        crate::test_gpu::with(|gpu| {
            let rows: [(&str, &str, &str, &str, &str, Tone); 9] = [
                ("grpc", "GRPC", "100%", "64", "0 ms", Tone::Bad),
                ("http", "HTTP", "10%", "48", "12 ms", Tone::Neutral),
                ("jobs", "Jobs", "1%", "16", "0 ms", Tone::Good),
                ("kafka", "Kafka consumer", "74%", "32", "140 ms", Tone::Warn),
                ("redis", "Redis", "0%", "8", "0 ms", Tone::Good),
                (
                    "mysql",
                    "MySQL primary",
                    "36%",
                    "128",
                    "3 ms",
                    Tone::Neutral,
                ),
                ("search", "Search", "92%", "24", "1250 ms", Tone::Bad),
                ("cache", "Cache", "5%", "8", "0 ms", Tone::Good),
                ("mail", "Mail", "18%", "4", "9 ms", Tone::Neutral),
            ];
            let body = rows
                .iter()
                .map(|(_, name, busy, max, wait, _)| format!("- {name} · {busy} · {max} · {wait}"))
                .collect::<Vec<_>>()
                .join("\n");
            let panel = tomori_protocol::Panel::new(
                "panel-1",
                "Observers",
                body,
                tomori_protocol::Place::Wall {
                    wall: tomori_protocol::TOMORI_WALL.into(),
                    slot: 0,
                },
            )
            .with_rows(
                rows.iter()
                    .enumerate()
                    .map(|(index, (id, name, busy, max, wait, tone))| {
                        let value = busy.trim_end_matches('%').parse::<f32>().unwrap() / 100.0;
                        ListRow::new(*id, true)
                            .with_highlight(index == 1)
                            .with_tone(*tone)
                            .with_value(value)
                            .with_cells(vec![
                                (*name).into(),
                                (*busy).into(),
                                (*max).into(),
                                (*wait).into(),
                            ])
                    })
                    .collect(),
            )
            .with_columns(vec![
                ListColumn::new("Name"),
                ListColumn::new("Busy")
                    .aligned(CellAlign::Right)
                    .with_value(true),
                ListColumn::new("Max").aligned(CellAlign::Right),
                ListColumn::new("Wait").aligned(CellAlign::Right),
            ]);
            let canvas = tomori_scene::projection::list_face(
                &panel,
                tomori_scene::projection::Fit::SLOT,
                0,
                (Some("kafka"), None),
            );
            let text: String = canvas
                .rows
                .iter()
                .map(|row| row.cells.iter().map(|(ch, _)| *ch).collect::<String>() + "\n")
                .collect();
            assert!(text.contains("Name"), "{text}");
            assert!(text.contains("100%"), "{text}");
            let (image, _) = drawn_face_on(&gpu, &canvas, 2.3, 0.37);
            crate::test_support::preview("TOMORI_LIST_TABLE_PREVIEW", &image);
            let near = |color: [f32; 4]| {
                let target = color.map(|channel| (channel.powf(1.0 / 2.2) * 255.0) as i32);
                image
                    .pixels()
                    .filter(|pixel| {
                        (0..3).all(|at| (i32::from(pixel.0[at]) - target[at]).abs() < 40)
                    })
                    .count()
            };
            let shown = [Tone::Good, Tone::Warn, Tone::Bad]
                .map(|tone| near(tomori_scene::chart::tone(tone)) > 50);
            assert_eq!(shown, [true; 3]);
        });
    }

    /// Set `TOMORI_TABLE_RULE_PREVIEW` to a PNG path to save what this renders: a table on a panel in a wall's slot, face
    /// on, at about the headset's resolution, its pixels off the screen's grid.
    #[test]
    fn a_table_s_rule_and_the_lines_between_its_columns_are_as_bright_all_along() {
        crate::test_gpu::with(|gpu| {
            let body = "| Step | What happens |\n|---|---|\n| 1 | The agent reads the table, lays out its columns, wraps each cell within its own column, and draws the rule below the header and the lines between the columns, each as one line rather than many short pieces |\n| 2 | Striped |\n";
            let panel = tomori_protocol::Panel::new(
                "panel-1",
                "Steps",
                body,
                tomori_protocol::Place::Wall {
                    wall: tomori_protocol::TOMORI_WALL.into(),
                    slot: 0,
                },
            );
            let canvas = tomori_scene::projection::face(
                &panel,
                false,
                false,
                tomori_scene::projection::Fit::SLOT,
            );
            let (image, place) = drawn_face_on(&gpu, &canvas, 2.3, 0.37);
            crate::test_support::preview("TOMORI_TABLE_RULE_PREVIEW", &image);
            let (left, top) = (canvas.padding.0 as f32, canvas.padding.1 as f32);
            let cell = |column: usize, row: usize| {
                place(
                    left + (column * crate::ui_text::CELL_WIDTH) as f32,
                    top + (row * crate::ui_text::CELL_HEIGHT) as f32,
                )
            };
            // The rule below the header: the body's first row of nothing but a rule, below the panel's own.
            let rule = (3..canvas.rows.len())
                .find(|&row| {
                    let cells = &canvas.rows[row].cells;
                    !cells.is_empty() && cells.iter().all(|(ch, _)| *ch == '─')
                })
                .expect("a rule below the header");
            let length = canvas.rows[rule].cells.len();
            let (start, upper) = cell(0, rule);
            let (end, lower) = cell(length, rule + 1);
            let (_, ratio) = evenness(
                &image,
                start.ceil() as u32 + 3..end.floor() as u32 - 3,
                upper.ceil() as u32..lower.floor() as u32,
                |x, y| (x, y),
            );
            assert!(ratio < 1.1, "the rule is uneven along its length: {ratio}");
            // The line between the columns, down the first row's lines, which wrap.
            let lined = |row: usize| canvas.rows[row].cells.iter().position(|(ch, _)| *ch == '▏');
            let column = lined(rule + 1).expect("a line between the columns");
            let lines = (rule + 1..canvas.rows.len())
                .take_while(|&row| {
                    lined(row) == Some(column) && canvas.rows[row].cells[0].1.background.is_none()
                })
                .count();
            assert!(lines >= 4, "the first row wraps: {lines}");
            let (x, first) = cell(column, rule + 1);
            let (_, last) = cell(column, rule + 1 + lines);
            let span = (5.0 * 2.3) as u32;
            let (added, ratio) = evenness(
                &image,
                first.ceil() as u32 + 3..last.floor() as u32 - 3,
                x as u32 - span..x as u32 + span,
                |y, x| (x, y),
            );
            assert!(added.iter().all(|&added| added > 0), "the line has gaps");
            assert!(
                ratio < 1.1,
                "the line between the columns is uneven along its length: {ratio}"
            );
        });
    }

    mod chart;
    mod handles;
    mod shapes;
}
