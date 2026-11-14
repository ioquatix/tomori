// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Bounded, world-space task labels and coordinator-published result pages.
//! This deliberately has no dependency on the head-locked caption renderer.
use crate::delegation::{State, Station, Workspace};
use bytemuck::{Pod, Zeroable};
use tomori_protocol::TimerState;
use tomori_scene::cards::{COLUMNS, ContentCache, MAX_RESULTS, ROWS, clip, results};
use tomori_scene::dials::Dial;
use tomori_scene::shapes::Shape;
use tomori_text::atlas::SOLID;

const MAX_LABELS: usize = 128;
// A focused card keeps all content readable and makes content paging independent
// of the size of neighboring cards.
pub const CARDS_PER_PAGE: usize = 1;
// A glyph is one quad from the atlas. Hard cap is enforced as well, including
// backgrounds, even for hostile input: every supported 160-character title +
// status, card and notice, about 140,000 vertices with 128 signs, and room for
// the buffer to double past that. GPU storage grows on demand, never eagerly to this.
const MAX_VERTICES: usize = 1 << 19;
const INITIAL_VERTICES: usize = 16_384;
const WHITE: [f32; 3] = [0.91, 0.96, 1.0];
const MUTED: [f32; 3] = [0.48, 0.72, 0.85];
const ATTRIBUTES: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4, 2 => Float32x2, 3 => Float32];

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    color: [f32; 4],
    /// Where in the glyph atlas, in its pixels: a glyph's tile, a shape's, or the solid block for everything else.
    uv: [f32; 2],
    weight: f32,
}

#[derive(Clone, Debug, PartialEq)]
struct Sign {
    source_id: String,
    // Both vectors lie in the scene XZ plane; normal points towards whoever the sign faces, in this client its viewer.
    // With the panel's origin, the station and `towards`, they place the sign's mesh rather than make it, so that a sign
    // turning to follow its viewer, or gliding after its worker, is moved rather than built again (see [`mesh_key`]).
    right: [f32; 3],
    normal: [f32; 3],
    state: State,
    station: [f32; 3],
    /// Along the ground from its station towards whoever it faces, which its leader's dot stands behind the station's
    /// middle from.
    towards: [f32; 3],
    /// Whether it stands in its own place, straight above its station, its post hanging below it, rather than moved
    /// aside or up, its post drawn down to its station.
    own: bool,
    /// How much larger it is drawn than one within [`tomori_scene::delegation::SIGN_REACH`] of whoever it faces.
    scale: f32,
    /// For a timer's dial rather than a worker, its state, whose symbol its pill shows, and how many rows its name
    /// takes, the rows after them, its schedule and condition, drawn muted. See `design/timers.md`.
    timer: Option<(TimerState, usize)>,
    /// Whether its worker is pointed at, in this client, when it is drawn outlined and brighter, its leader too, as its
    /// flock is. See `design/interaction.md`.
    highlighted: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Panel {
    origin: [f32; 3],
    pixel: f32,
    columns: usize,
    lines: Vec<String>,
    accent: [f32; 3],
    opacity: f32,
    sign: Option<Sign>,
    /// The row drawn as a rule across the panel rather than as text, such as between a card's header and its content.
    rule: Option<usize>,
}

#[derive(Default)]
struct PageCache {
    panels: Vec<Panel>,
    /// Each panel's mesh, with whether it has every glyph, rather than lacking some that waited for a later frame's budget,
    /// and how many of its first vertices are its surface.
    meshes: Vec<(Panel, Vec<Vertex>, bool, usize)>,
    vertices: Vec<Vertex>,
    ranges: Vec<PanelRange>,
    builds: usize,
    /// Whether every mesh has every glyph.
    complete: bool,
}

struct PanelRange {
    center: glam::Vec3,
    vertices: std::ops::Range<u32>,
    // Background strips, post and edge accent only: text shares this shallow
    // surface, with millimetre offsets. Never compare thousands of glyph quads.
    surface: Vec<[glam::Vec3; 3]>,
    /// Whether the panel is opaque enough that its surface, drawn into depth once the panel is, hides what is behind it.
    occludes: bool,
}

struct ProjectedTriangle {
    points: [glam::Vec3; 3],
    low: glam::Vec2,
    high: glam::Vec2,
}

impl ProjectedTriangle {
    fn depth(&self, p: glam::Vec2) -> f32 {
        let [a, b, c] = self.points;
        let cross = |u: glam::Vec2, v: glam::Vec2| u.perp_dot(v);
        let area = cross(b.truncate() - a.truncate(), c.truncate() - a.truncate());
        let u = cross(p - a.truncate(), c.truncate() - a.truncate()) / area;
        let v = cross(b.truncate() - a.truncate(), p - a.truncate()) / area;
        a.z + u * (b.z - a.z) + v * (c.z - a.z)
    }
}

fn project_surface(surface: &[[glam::Vec3; 3]], matrix: glam::Mat4) -> Vec<ProjectedTriangle> {
    let mut triangles = Vec::new();
    for triangle in surface {
        let mut polygon: Vec<_> = triangle.iter().map(|p| matrix * p.extend(1.0)).collect();
        // Clip before perspective division, including panels crossing the near
        // plane. WebGPU's depth interval is 0..w, not OpenGL's -w..w.
        for plane in [
            glam::Vec4::new(1.0, 0.0, 0.0, 1.0),
            glam::Vec4::new(-1.0, 0.0, 0.0, 1.0),
            glam::Vec4::new(0.0, 1.0, 0.0, 1.0),
            glam::Vec4::new(0.0, -1.0, 0.0, 1.0),
            glam::Vec4::Z,
            glam::Vec4::new(0.0, 0.0, -1.0, 1.0),
        ] {
            if polygon.iter().all(|p| plane.dot(*p) >= 0.0) {
                continue;
            }
            // A polygon clipped away by one plane is empty, and so is wholly inside every later one.
            let input = std::mem::take(&mut polygon);
            let mut a = *input.last().unwrap();
            let mut da = plane.dot(a);
            for b in input {
                let db = plane.dot(b);
                if (da >= 0.0) != (db >= 0.0) {
                    polygon.push(a.lerp(b, da / (da - db)));
                }
                if db >= 0.0 {
                    polygon.push(b);
                }
                (a, da) = (b, db);
            }
        }
        let ndc: Vec<_> = polygon.iter().map(|p| p.truncate() / p.w).collect();
        for i in 1..ndc.len().saturating_sub(1) {
            let points = [ndc[0], ndc[i], ndc[i + 1]];
            let area = (points[1] - points[0])
                .truncate()
                .perp_dot((points[2] - points[0]).truncate());
            if points.iter().all(|p| p.is_finite()) && area.abs() > 1e-10 {
                triangles.push(ProjectedTriangle {
                    low: points
                        .iter()
                        .fold(glam::Vec2::splat(f32::INFINITY), |lo, p| {
                            lo.min(p.truncate())
                        }),
                    high: points
                        .iter()
                        .fold(glam::Vec2::splat(f32::NEG_INFINITY), |hi, p| {
                            hi.max(p.truncate())
                        }),
                    points,
                });
            }
        }
    }
    triangles
}

// Positive means a is behind b wherever their projected surfaces overlap.
// NDC depth is affine within each projected triangle, so checking the vertices
// of their intersection checks its entire area, not just the panel centers.
fn overlap_order(a: &[ProjectedTriangle], b: &[ProjectedTriangle]) -> Option<std::cmp::Ordering> {
    let bounds = |triangles: &[ProjectedTriangle]| {
        triangles.iter().fold(
            (
                glam::Vec2::splat(f32::INFINITY),
                glam::Vec2::splat(f32::NEG_INFINITY),
            ),
            |(lo, hi), t| (lo.min(t.low), hi.max(t.high)),
        )
    };
    let disjoint = |al: glam::Vec2, ah: glam::Vec2, bl: glam::Vec2, bh: glam::Vec2| {
        ah.x <= bl.x || bh.x <= al.x || ah.y <= bl.y || bh.y <= al.y
    };
    let (al, ah) = bounds(a);
    let (bl, bh) = bounds(b);
    if disjoint(al, ah, bl, bh) {
        return None;
    }
    let mut behind = false;
    let mut ahead = false;
    for a in a {
        for b in b {
            if disjoint(a.low, a.high, b.low, b.high) {
                continue;
            }
            let mut polygon: Vec<_> = a.points.iter().map(|p| p.truncate()).collect();
            let [p, q, r] = b.points.map(|p| p.truncate());
            let orientation = (q - p).perp_dot(r - p).signum();
            for (p, q) in [(p, q), (q, r), (r, p)] {
                let input = std::mem::take(&mut polygon);
                if input.is_empty() {
                    break;
                }
                let distance = |v: glam::Vec2| orientation * (q - p).perp_dot(v - p);
                let mut u = *input.last().unwrap();
                let mut du = distance(u);
                for v in input {
                    let dv = distance(v);
                    if (du >= 0.0) != (dv >= 0.0) {
                        polygon.push(u.lerp(v, du / (du - dv)));
                    }
                    if dv >= 0.0 {
                        polygon.push(v);
                    }
                    (u, du) = (v, dv);
                }
            }
            if polygon.len() < 3 {
                continue;
            }
            let area: f32 = (1..polygon.len() - 1)
                .map(|i| (polygon[i] - polygon[0]).perp_dot(polygon[i + 1] - polygon[0]))
                .sum();
            if area.abs() < 1e-10 {
                continue;
            }
            for p in polygon {
                let delta = a.depth(p) - b.depth(p);
                behind |= delta > 1e-6;
                ahead |= delta < -1e-6;
            }
            if behind && ahead {
                return None;
            }
        }
    }
    match (behind, ahead) {
        (true, false) => Some(std::cmp::Ordering::Greater),
        (false, true) => Some(std::cmp::Ordering::Less),
        _ => None,
    }
}

fn panel_order(ranges: &[PanelRange], matrix: glam::Mat4) -> Vec<usize> {
    let projected: Vec<_> = ranges
        .iter()
        .map(|p| project_surface(&p.surface, matrix))
        .collect();
    let mut edges = vec![Vec::new(); ranges.len()];
    let mut incoming = vec![0; ranges.len()];
    for a in 0..ranges.len() {
        for b in a + 1..ranges.len() {
            let edge = match overlap_order(&projected[a], &projected[b]) {
                Some(std::cmp::Ordering::Greater) => Some((a, b)),
                Some(std::cmp::Ordering::Less) => Some((b, a)),
                _ => None,
            };
            if let Some((far, near)) = edge {
                edges[far].push(near);
                incoming[near] += 1;
            }
        }
    }
    let depth = |i: usize| {
        let clip = matrix * ranges[i].center.extend(1.0);
        if clip.w > 0.0 {
            clip.z / clip.w
        } else {
            f32::INFINITY
        }
    };
    let mut remaining: Vec<_> = (0..ranges.len()).collect();
    remaining.sort_by(|&a, &b| depth(b).total_cmp(&depth(a)));
    let mut order = Vec::with_capacity(ranges.len());
    while !remaining.is_empty() {
        // Disjoint panels need no constraint. True intersections, near-equal
        // depth ties and cyclic overlap cannot be resolved by whole-panel draws;
        // fall back deterministically to center depth, not a general OIT claim.
        let index = remaining
            .iter()
            .position(|&i| incoming[i] == 0)
            .unwrap_or(0);
        let far = remaining.remove(index);
        for &near in &edges[far] {
            incoming[near] -= 1;
        }
        order.push(far);
    }
    order
}

impl Panel {
    // The center of the actual panel surface, not the station position or
    // scene home. Its projected depth changes with each eye and recenter.
    fn center(&self) -> glam::Vec3 {
        let x = (self.columns * 9) as f32 * self.pixel * 0.5;
        let y = -((self.lines.len() * 11) as f32) * self.pixel * 0.5;
        let right = self.sign.as_ref().map_or([1.0, 0.0, 0.0], |s| s.right);
        glam::Vec3::from_array(self.origin) + glam::Vec3::from_array(right) * x + glam::Vec3::Y * y
    }
}

impl PageCache {
    fn update(&mut self, panels: Vec<Panel>) -> bool {
        if self.complete && self.panels == panels {
            return false;
        }
        // Match by source before comparing the bounded text/geometry key. This
        // avoids quadratic title comparisons on every opacity-only frame.
        #[derive(PartialEq, Eq, PartialOrd, Ord)]
        enum MeshId {
            Station(String),
            Field([u32; 3], usize),
        }
        let mesh_id = |panel: &Panel| {
            panel.sign.as_ref().map_or_else(
                || MeshId::Field(panel.origin.map(f32::to_bits), panel.columns),
                |sign| MeshId::Station(sign.source_id.clone()),
            )
        };
        let mut old: std::collections::BTreeMap<_, _> = std::mem::take(&mut self.meshes)
            .into_iter()
            .map(|entry| (mesh_id(&entry.0), entry))
            .collect();
        for panel in &panels {
            let key = mesh_key(panel);
            let entry = if let Some(entry) = old
                .remove(&mesh_id(&key))
                .filter(|(cached, _, complete, _)| *complete && *cached == key)
            {
                entry
            } else {
                let waited = tomori_text::atlas::waited();
                let mut vertices = Vec::new();
                let surface = local_mesh(&key, &mut vertices);
                self.builds += 1;
                (
                    key,
                    vertices,
                    tomori_text::atlas::waited() == waited,
                    surface,
                )
            };
            self.meshes.push(entry);
        }
        self.complete = self.meshes.iter().all(|(_, _, complete, _)| *complete);
        self.vertices.clear();
        self.ranges.clear();
        for (panel, (_, mesh, _, surface)) in panels.iter().zip(&self.meshes) {
            let start = self.vertices.len();
            assert!(
                start + mesh.len() + LEADER_VERTICES <= MAX_VERTICES,
                "workspace budget must fit every supported panel"
            );
            // Placed where it stands now, facing whoever it faces, its leader drawn to its worker between its surface and
            // the rest, as it was made.
            place(panel, mesh, *surface, 1.0, &mut self.vertices);
            for vertex in &mut self.vertices[start..] {
                vertex.color[3] *= panel.opacity;
            }
            let end = self.vertices.len();
            if start != end && panel.opacity > 0.0 {
                self.ranges.push(PanelRange {
                    center: panel.center(),
                    occludes: crate::occlusion::occludes(panel.opacity),
                    vertices: start as u32..end as u32,
                    surface: self.vertices[start..start + (end - start).min(*surface)]
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .map(|t| t.map(|v| glam::Vec3::from_array(v.position)))
                        .collect(),
                });
            }
        }
        self.panels = panels;
        true
    }
}

/// What a panel's mesh is made from, which it is cached by: all of it but its opacity, which its vertices are faded by
/// as they are placed, and, for a sign, where it stands, which way it faces and where its worker is, by which it is
/// placed rather than made.
fn mesh_key(panel: &Panel) -> Panel {
    let mut key = panel.clone();
    key.opacity = 1.0;
    if let Some(sign) = &mut key.sign {
        key.origin = [0.0; 3];
        sign.right = [0.0; 3];
        sign.normal = [0.0; 3];
        sign.station = [0.0; 3];
        sign.towards = [0.0; 3];
        sign.own = false;
    }
    key
}

pub struct WorkspaceGraphics {
    pipeline: wgpu::RenderPipeline,
    /// Draws an opaque enough panel's surface into depth alone, once the panel is drawn.
    occluder: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    cache: PageCache,
    count: u32,
    page: usize,
    pages: usize,
    content_page: usize,
    content: ContentCache,
    placements: LabelPlacement,
    vertex_capacity: usize,
    /// The space's timers' dials, whose signs the next [`Self::update`] draws.
    dials: Vec<Dial>,
}

impl WorkspaceGraphics {
    /// `samples` is the scene pass's sample count, which this draws within.
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
        glyphs: &crate::glyphs::Glyphs,
        samples: u32,
    ) -> Self {
        Self::with_views(
            device,
            format,
            camera_layout,
            glyphs,
            samples,
            crate::multiview::Views::One,
        )
    }

    /// As [`Self::new`], drawn in passes of `views`; see [`crate::multiview`].
    pub fn with_views(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
        glyphs: &crate::glyphs::Glyphs,
        samples: u32,
        views: crate::multiview::Views,
    ) -> Self {
        let shader = crate::multiview::shader(
            device,
            "workspace.wgsl",
            include_str!("workspace.wgsl"),
            views,
        );
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world workspace layout"),
            bind_group_layouts: &[Some(camera_layout), Some(glyphs.layout())],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world workspace text"),
            layout: Some(&layout),
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
                // Translucent backgrounds must not write depth and mask the
                // glyphs or other geometry during arrival/departure fades.
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
            "world workspace occluder",
            &layout,
            &shader,
            VERTICES,
            format,
            samples,
            views,
        );
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bounded workspace quads"),
            size: (INITIAL_VERTICES * size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            occluder,
            vertices,
            cache: PageCache::default(),
            count: 0,
            page: 0,
            pages: 0,
            content_page: 0,
            content: ContentCache::default(),
            placements: LabelPlacement::default(),
            vertex_capacity: INITIAL_VERTICES,
            dials: Vec::new(),
        }
    }

    /// The space's timers' dials, whose signs the next [`Self::update`] draws, above each dial, as a worker's sign stands
    /// above it. See `design/timers.md`.
    pub fn set_dials(&mut self, dials: &[Dial]) {
        if self.dials != dials {
            self.dials = dials.to_vec();
        }
    }

    pub fn hide(&mut self) {
        self.count = 0;
    }

    pub fn set_page(&mut self, page: usize) {
        self.page = page;
    }

    pub fn set_content_page(&mut self, page: usize) {
        self.content_page = page;
    }

    #[cfg(test)]
    pub fn page_count(&self) -> usize {
        self.pages
    }

    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        workspace: &Workspace,
        stations: &[Station],
        viewer: Option<glam::Vec3>,
    ) {
        let cards = results(workspace);
        self.page = self.page.min(cards.len().saturating_sub(1));
        self.content.update(cards.get(self.page).copied());
        self.content_page = self
            .content_page
            .min(self.content.pages.len().saturating_sub(1));
        let (mut panels, pages) = layout(
            workspace,
            stations,
            self.page,
            self.content_page,
            &self.content.pages,
            &mut self.placements,
            viewer.map(|viewer| viewer.to_array()),
        );
        panels.extend(dial_panels(
            &self.dials,
            viewer.map(|viewer| viewer.to_array()),
        ));
        self.pages = pages;
        self.page = self.page.min(pages.saturating_sub(1));
        if self.cache.update(panels) && !self.cache.vertices.is_empty() {
            if self.cache.vertices.len() > self.vertex_capacity {
                self.vertex_capacity = self
                    .cache
                    .vertices
                    .len()
                    .next_power_of_two()
                    .min(MAX_VERTICES);
                self.vertices = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("grown bounded workspace quads"),
                    size: (self.vertex_capacity * size_of::<Vertex>()) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(
                &self.vertices,
                0,
                bytemuck::cast_slice(&self.cache.vertices),
            );
        }
        self.count = self.cache.vertices.len() as u32;
    }

    /// Draw separated world panels back-to-front in this eye's actual view.
    /// `scene_to_clip` must match the matrix in `camera`, including recentering.
    /// Ordering uses background/post/accent surfaces; intended panels must have
    /// clearance for their 3–5 mm foreground offsets. Intersecting surfaces or
    /// cyclic overlaps need splitting/OIT and are only best-effort here.
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        camera: &wgpu::BindGroup,
        glyphs: &wgpu::BindGroup,
        scene_to_clip: glam::Mat4,
    ) {
        for (index, _) in self.order(scene_to_clip) {
            self.draw_panel(pass, camera, glyphs, index);
        }
    }

    /// The order to draw its panels in for the eye `scene_to_clip` sees through, back to front, each with how far ahead
    /// of the eye its middle is, so that the renderer can draw the space's panels between them, farthest first.
    /// Blending is order dependent. Preserve each panel's background-first mesh order, but draw whole panels
    /// back-to-front for this eye. Use overlap depths rather than center depths: a tilted card's center can be nearer
    /// even though its overlapping edge is behind the sign.
    pub fn order(&self, scene_to_clip: glam::Mat4) -> Vec<(usize, f32)> {
        if self.count == 0 {
            return Vec::new();
        }
        panel_order(&self.cache.ranges, scene_to_clip)
            .into_iter()
            .map(|index| {
                let ahead = (scene_to_clip * self.cache.ranges[index].center.extend(1.0)).w;
                (index, ahead)
            })
            .collect()
    }

    /// Draw its panel `index`, of [`Self::order`], in the eye `camera` binds. The shared depth attachment still occludes
    /// it with opaque scene bodies. Once drawn, a panel opaque enough to hide what is behind it writes its surface's
    /// depth where it is itself opaque enough, so that panels and labels drawn later, behind it, are hidden; one fading
    /// out does not, so fades never leave invisible occluders.
    pub fn draw_panel(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        camera: &wgpu::BindGroup,
        glyphs: &wgpu::BindGroup,
        index: usize,
    ) {
        let range = &self.cache.ranges[index];
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, camera, &[]);
        pass.set_bind_group(1, glyphs, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(range.vertices.clone(), 0..1);
        if range.occludes {
            let surface = range.vertices.start + range.surface.len() as u32 * 3;
            pass.set_pipeline(&self.occluder);
            pass.draw(range.vertices.start..surface, 0..1);
        }
    }
}

const VERTICES: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<Vertex>() as u64,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &ATTRIBUTES,
};

/// What a worker's sign says of its state, in its pill, and the state's colour, which tints the pill, its symbol and
/// the leader down to the worker.
fn state_label(state: &State) -> (&'static str, [f32; 3]) {
    match state {
        State::Running => ("Running", [0.4, 0.72, 1.0]),
        State::Waiting => ("Waiting", [1.0, 0.72, 0.24]),
        State::Idle => ("Idle", [0.55, 0.7, 0.78]),
        State::Completed => ("Finished", [0.3, 0.95, 0.65]),
        State::Error => ("Error", [1.0, 0.35, 0.3]),
        State::Stopped => ("Stopped", [0.8, 0.68, 0.55]),
        State::Stalled => ("Stalled", [1.0, 0.58, 0.14]),
        State::Unanswered => ("No answer", [1.0, 0.58, 0.14]),
        State::Unknown => ("Unknown", [0.7, 0.65, 0.8]),
    }
}

fn layout(
    workspace: &Workspace,
    stations: &[Station],
    page: usize,
    content_page: usize,
    content: &[Vec<String>],
    placements: &mut LabelPlacement,
    viewer: Option<[f32; 3]>,
) -> (Vec<Panel>, usize) {
    let mut panels = Vec::new();
    let mut notices = Vec::new();
    // Positions come from the client-side scene, not presentation content.
    // The scene must be cleared on a root switch; retained stations during an
    // outage or departure are useful, but cannot claim a current state.
    let mut stations: Vec<_> = stations
        .iter()
        .take(MAX_LABELS)
        .filter(|station| {
            station
                .position
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 100.0)
                && station.opacity.is_finite()
                && (0.0..=1.0).contains(&station.opacity)
                && station.opacity > 0.0
                && !station.id.is_empty()
                && station.id.len() <= 160
                && station.title.chars().take(161).count() <= 160
        })
        .collect();
    stations.sort_by(|a, b| a.id.cmp(&b.id));
    stations.dedup_by(|a, b| a.id == b.id);
    placements.retain(&stations);
    let states: std::collections::BTreeMap<_, _> = workspace
        .nodes
        .iter()
        .take(64)
        .filter(|_| !workspace.stale)
        .map(|node| (node.id.as_str(), node.state))
        .collect();
    for station in &stations {
        let state = states
            .get(station.id.as_str())
            .copied()
            .unwrap_or(State::Unknown);
        panels.push(station_panel(
            station,
            placements.position(station),
            state,
            viewer,
        ));
    }
    if workspace.truncated {
        notices.push("Workspace discovery truncated [LIMIT]".into());
    } else if stations
        .iter()
        .map(|station| station.more.clamp(1, 64))
        .sum::<usize>()
        < stations_count_hint(workspace)
    {
        notices.push("Workspace station projection incomplete".into());
    }
    let results = results(workspace);
    let pages = results.len().div_ceil(CARDS_PER_PAGE);
    let page = page.min(pages.saturating_sub(1));
    for result in results
        .iter()
        .skip(page * CARDS_PER_PAGE)
        .take(CARDS_PER_PAGE)
    {
        let mut lines = vec![clip(&result.title, COLUMNS)];
        lines.push(clip(
            &format!(
                "Source: {} / {}",
                short_ref(&result.source_id),
                short_ref(&result.source_message_id)
            ),
            COLUMNS,
        ));
        let content_page = content_page.min(content.len().saturating_sub(1));
        lines.push(format!(
            "CARD {}/{}  CONTENT {}/{}{}",
            page + 1,
            pages,
            content_page + 1,
            content.len().max(1),
            if workspace.stale { " [STALE]" } else { "" }
        ));
        // A rule between the header and the content, drawn as a line.
        let rule = Some(lines.len());
        lines.push(String::new());
        // Every card has at least one page of content, even if only to say it is empty.
        lines.extend(content.get(content_page).into_iter().flatten().cloned());
        // Keep the card's field dimensions stable even on the last page.
        lines.resize(ROWS, String::new());
        // Beside the coordinator, never behind the head-locked console.
        panels.push(Panel {
            origin: [-4.56, 2.85, -3.35],
            pixel: 0.0051,
            columns: COLUMNS,
            lines,
            accent: [0.25, 0.8, 0.95],
            opacity: 1.0,
            sign: None,
            rule,
        });
    }
    if workspace.stale {
        notices.push("Workspace stale - status UNKNOWN".into());
    } else if workspace.error.is_some() {
        notices.push("Workspace partially unavailable".into());
    }
    if workspace.presentations.len() > MAX_RESULTS {
        notices.push(format!(
            "{} results; first {} paged / rest hidden",
            workspace.presentations.len(),
            MAX_RESULTS
        ));
    }
    if !notices.is_empty() {
        panels.push(notice(notices));
    }
    (panels, pages)
}

/// Along the ground from `from` towards `face`, or towards `fallback` where `face` stands above or below it, or else
/// straight along +Z: which way a sign at `from` faces.
fn facing_from(from: [f32; 3], face: [f32; 3], fallback: [f32; 3]) -> [f32; 3] {
    [face, fallback]
        .into_iter()
        .find_map(|to| {
            glam::Vec2::new(to[0] - from[0], to[2] - from[2])
                .try_normalize()
                .filter(|_| (to[0] - from[0]).hypot(to[2] - from[2]) >= SIGN_NEAREST)
                .map(|d| [d.x, 0.0, d.y])
        })
        .unwrap_or([0.0, 0.0, 1.0])
}

/// The sign of `station`, standing at `position`, turned about its upright to face `viewer`, this client's, or else
/// whoever the agent faces.
fn station_panel(
    station: &Station,
    position: [f32; 3],
    state: State,
    viewer: Option<[f32; 3]>,
) -> Panel {
    // A sign for those gathered into a cluster's "+N more" says how many, listing them while it is pointed at.
    let more = (station.more > 0).then(|| format!("+{} more", station.more));
    let (state_text, accent) = match &more {
        Some(more) => (more.as_str(), MORE_ACCENT),
        None => state_label(&state),
    };
    let [sx, sy, sz] = position;
    // Facing this client's viewer, wherever they walk, or else the eyes at home; upright, turning only about its own upright, so that it never leans.
    let face = viewer.unwrap_or(station.facing);
    let normal = facing_from(position, face, station.facing);
    let right = [normal[2], 0.0, -normal[0]];
    // Beyond reading distance from them, drawn larger in proportion, so that it reads as one there does; in a compact
    // cluster, smaller as its worker's footprint is.
    let scale = station.drawn();
    let pixel = SIGN_PIXEL * scale;
    let lines = label_lines(&station.title, state_text);
    // As wide as its widest row, its title's or its pill's, so that its rows stand centred above its worker.
    let columns = sign_columns(&lines);
    let half_width = (columns * 9) as f32 * pixel * 0.5;
    // Its bottom edge stands a little above the place it was given, whatever its height, so that a long title grows
    // up from its worker rather than down over it; and a little behind it, so that its worker reads in front of it.
    let height = (lines.len() * 11 + 4) as f32 * pixel;
    Panel {
        origin: [
            sx - normal[0] * SIGN_BEHIND - right[0] * half_width,
            sy + SIGN_LIFT * scale + height,
            sz - normal[2] * SIGN_BEHIND - right[2] * half_width,
        ],
        pixel,
        columns,
        lines,
        accent,
        opacity: station.opacity,
        sign: Some(Sign {
            source_id: station.id.clone(),
            right,
            normal,
            state,
            station: station.position,
            towards: facing_from(station.position, face, station.facing),
            own: position == station.position,
            scale,
            timer: None,
            highlighted: station.highlighted,
        }),
        rule: None,
    }
}

/// The cells across a timer's sign, narrower than a worker's, so that the signs of a row of dials stand clear of one
/// another.
const DIAL_COLUMNS: usize = 22;
/// The most rows of a timer's name, and of its condition, on its sign; its schedule takes one.
const DIAL_ROWS: usize = 2;
/// How large a timer's sign is drawn, as a share of a worker's: about as large to the eye, since dials stand nearer
/// whoever watches than the default arc's workers.
const DIAL_SIGN: f32 = 0.8;
/// How far above its dial's middle a timer's sign's bottom edge stands, in metres: clear of its face, its leader between.
const DIAL_LIFT: f32 = tomori_scene::dials::RADIUS + tomori_scene::dials::SIGN_LIFT;

/// The signs of `dials`, at most [`tomori_scene::dials::MOST`] of those valid, each standing above its dial, turned to
/// face `viewer`, or else whoever the dials face: the timer's name, its schedule and its condition, each on a row of its
/// own, muted, and its pill, saying its countdown or its state, in its state's colour. Each stands straight above its own dial,
/// since the dials are placed clear of one another. See `design/timers.md`.
fn dial_panels(dials: &[Dial], viewer: Option<[f32; 3]>) -> Vec<Panel> {
    dials
        .iter()
        .filter(|dial| dial.valid() && dial.opacity > 0.0)
        .take(tomori_scene::dials::MOST * 2)
        .map(|dial| dial_panel(dial, viewer))
        .collect()
}

fn dial_panel(dial: &Dial, viewer: Option<[f32; 3]>) -> Panel {
    let position = dial.position;
    let face = viewer.unwrap_or(dial.facing);
    let normal = facing_from(position, face, dial.facing);
    let right = [normal[2], 0.0, -normal[0]];
    let scale = dial.scale * DIAL_SIGN;
    let pixel = SIGN_PIXEL * scale;
    let mut lines = crate::ui_text::lines(&clip(&dial.name, usize::MAX), DIAL_COLUMNS, DIAL_ROWS);
    if lines.is_empty() {
        lines.push(String::new());
    }
    let named = lines.len();
    lines.push(clip(&dial.schedule, DIAL_COLUMNS));
    lines.extend(crate::ui_text::lines(
        &clip(&dial.condition, usize::MAX),
        DIAL_COLUMNS,
        DIAL_ROWS,
    ));
    lines.push(dial.pill.clone());
    let pill = (pill_width(&dial.pill) / 9.0).ceil() as usize;
    let columns = lines[..lines.len() - 1]
        .iter()
        .map(|line| crate::ui_text::width(line))
        .chain([pill, SIGN_FEWEST_COLUMNS])
        .max()
        .unwrap_or(SIGN_FEWEST_COLUMNS)
        .min(DIAL_COLUMNS);
    let half_width = (columns * 9) as f32 * pixel * 0.5;
    let height = (lines.len() * 11 + 4) as f32 * pixel;
    let [x, y, z] = position;
    Panel {
        origin: [
            x - normal[0] * SIGN_BEHIND - right[0] * half_width,
            y + DIAL_LIFT * dial.scale + height,
            z - normal[2] * SIGN_BEHIND - right[2] * half_width,
        ],
        pixel,
        columns,
        lines,
        accent: tomori_scene::dials::color(dial.state),
        opacity: dial.opacity,
        sign: Some(Sign {
            source_id: format!("timer\u{0}{}", dial.name),
            right,
            normal,
            state: State::Unknown,
            station: position,
            towards: facing_from(position, face, dial.facing),
            own: true,
            scale,
            timer: Some((dial.state, named)),
            highlighted: false,
        }),
        rule: None,
    }
}

/// The accent of the sign for the workers gathered into a cluster's "+N more".
const MORE_ACCENT: [f32; 3] = [0.7, 0.75, 0.85];

/// How large `station`'s sign is drawn, as a share of its full size, as its cluster says: within 0.1 and 1.
#[cfg(test)]
fn compact(station: &Station) -> f32 {
    tomori_scene::delegation::compact_scale(station.scale)
}

/// The fewest cells across a sign, so that one with a short title is still a sign rather than a tag.
const SIGN_FEWEST_COLUMNS: usize = 10;

/// The cells across a sign of `lines`: as many as its widest row takes, its title's or its state's pill's, at least
/// [`SIGN_FEWEST_COLUMNS`] and at most [`SIGN_COLUMNS`].
fn sign_columns(lines: &[String]) -> usize {
    let (status, title) = lines.split_last().expect("a sign has its state's row");
    let pill = (pill_width(status) / 9.0).ceil() as usize;
    title
        .iter()
        .map(|line| crate::ui_text::width(line))
        .chain([pill, SIGN_FEWEST_COLUMNS])
        .max()
        .unwrap_or(SIGN_FEWEST_COLUMNS)
        .min(SIGN_COLUMNS)
}

/// How wide a state's pill is, in font pixels, for its word `status`: its symbol, its word, and a little after.
fn pill_width(status: &str) -> f32 {
    PILL_SYMBOL + crate::ui_text::width(status) as f32 * 9.0 + 5.0
}

/// The cells across a worker's sign.
const SIGN_COLUMNS: usize = 28;
/// The most rows of a sign's title: room for 160 Latin characters, the most a title has. A longer title in cells, such as one in Japanese, two cells a character, ends with "…".
const SIGN_TITLE_ROWS: usize = 6;

/// A sign's rows: its title on one line, wrapped by the one rule for every panel, counted in cells as drawn, then its status.
/// The style rows are laid out in, whose colour the panel gives instead.
const ROW_STYLE: crate::ui_text::Style = crate::ui_text::Style::plain([1.0; 4]);

fn label_lines(title: &str, status: &str) -> Vec<String> {
    let title: String = title.chars().take(160).collect();
    let mut lines = crate::ui_text::lines(&clip(&title, usize::MAX), SIGN_COLUMNS, SIGN_TITLE_ROWS);
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(status.into());
    lines
}

/// How far above its own place a sign may rise to clear the others, in metres: nine of the default arc's closest rows,
/// 0.55 m apart, whose signs never overlap; more for a sign drawn larger (see [`Station::drawn`]).
const SIGN_HIGHEST: f32 = 4.95;
/// How much further than it must a sign rises to clear another, in metres, so that rounding never leaves it short.
const SIGN_SLACK: f32 = 0.001;
/// How far apart, along the arc about home, the columns of signs stand when stations stand together, in metres: wider
/// than a sign, as the default arc's columns are; more for a sign drawn larger.
const SIGN_COLUMN: f32 = 1.1;
/// The columns of signs about a station, as seen from home: its own, then alternately right and left of it, as many as
/// fit within [`SIGN_SPREAD`] of its bearing from home.
const SIGN_COLUMNS_AROUND: [i8; 15] = [0, 1, -1, 2, -2, 3, -3, 4, -4, 5, -5, 6, -6, 7, -7];
/// How far to either side of a station's bearing from home its sign may stand, in radians: 30°, so that a sign moved
/// aside stays near its station, rather than standing over another cluster, and signs stack sooner.
const SIGN_SPREAD: f32 = std::f32::consts::FRAC_PI_6;
/// How far apart two signs must be, vertically or else horizontally, in metres, not to overlap: closer than the default
/// arc's rows and columns, so that each of its signs stands above its own station; more for signs drawn larger.
const SIGN_APART: [f32; 2] = [0.5, 1.0];
/// How far above its station a sign's bottom edge stands in its own place, clear of its worker's flock and completed
/// cube, its post hanging between; more for a sign drawn larger.
const SIGN_LIFT: f32 = tomori_scene::delegation::SIGN_LIFT;
/// How far behind its place a sign stands, in metres, from whoever it faces: so that its worker, whose flock reaches
/// about 0.13 m about its middle, reads in front of the sign's leader and the dot that ends it, its anchor.
const SIGN_BEHIND: f32 = 0.06;
/// How near, along the ground, whoever a sign faces may stand to it before it faces whoever the agent faces instead, in
/// metres: as someone standing beneath it, whose bearing from it would swing with every sway.
const SIGN_NEAREST: f32 = 0.2;
/// How large a sign's font pixel is, in metres, within [`tomori_scene::delegation::SIGN_REACH`] of home.
const SIGN_PIXEL: f32 = 0.0038;
#[cfg(test)]
use tomori_scene::delegation::{SIGN_REACH, sign_scale};

/// Where each station's sign stands: above its own station, wherever it stands, as the default arc and an environment's
/// placement put it; or, when another sign is already within [`SIGN_APART`] of that, straight above it, only as far as
/// clears every other sign, at most [`SIGN_HIGHEST`], so that its leader hangs straight down to its station; and only
/// where no such place is clear, column by column beside it along the arc about whoever it faces, within
/// [`SIGN_SPREAD`] of its bearing from them, as far up each, so that stations placed together, or at one spot, do not
/// share a sign's place, and a sign rises above its station before it moves aside at all. Where every such
/// place is taken, as for very many stations at one spot, the sign stands above its own station, overlapping another,
/// rather than further off. A sign stays where it is while its station does, through metadata, status, order and
/// membership changes; when stations move, as when the workers' arrangement changes and they glide
/// there, every moved station's sign is forgotten before any is placed again, so that none is pushed aside by
/// where a neighbour's stood. A post preserves association. Kept by identity: the sign's place, then the station's
/// place, whom it faces and how large it is drawn that it was worked out for.
#[derive(Default)]
struct LabelPlacement(
    std::collections::BTreeMap<String, [f32; 3]>,
    std::collections::BTreeMap<String, ([f32; 3], [f32; 3], f32)>,
);

impl LabelPlacement {
    /// Keep the signs of `stations` that have not moved, forgetting those of stations that have gone or moved.
    fn retain(&mut self, stations: &[&Station]) {
        let unmoved = |id: &String| {
            stations
                .binary_search_by(|s| s.id.as_str().cmp(id.as_str()))
                .is_ok_and(|index| {
                    let station = stations[index];
                    self.1.get(id) == Some(&(station.position, station.facing, station.drawn()))
                })
        };
        let kept: std::collections::BTreeSet<String> =
            self.0.keys().filter(|id| unmoved(id)).cloned().collect();
        self.0.retain(|id, _| kept.contains(id));
        self.1.retain(|id, _| kept.contains(id));
    }

    fn position(&mut self, station: &Station) -> [f32; 3] {
        if let Some(position) = self.0.get(&station.id) {
            return *position;
        }
        let own = glam::Vec3::from_array(station.position);
        let viewer = glam::Vec3::new(station.facing[0], 0.0, station.facing[2]);
        // Spaced as it is drawn: larger far off, or as its cluster draws it, and smaller in a compact cluster.
        let scale = station.drawn();
        let from = own - viewer;
        // The angle about whoever it faces between columns, a column apart at the station's distance, or half a turn
        // nearer.
        let step = 2.0
            * (SIGN_COLUMN * scale * 0.5 / from.x.hypot(from.z))
                .min(1.0)
                .asin();
        let place = |rise: f32, column: i8| {
            let beside = if column == 0 {
                own
            } else {
                viewer + glam::Quat::from_rotation_y(f32::from(column) * step) * from
            };
            beside + glam::Vec3::Y * rise
        };
        // How far apart its sign and another must stand, as the larger of the two is drawn.
        let apart = |id: &String| scale.max(self.1.get(id).map_or(1.0, |(_, _, drawn)| *drawn));
        let clear = |candidate: &glam::Vec3| {
            self.0.iter().all(|(id, other)| {
                let apart = apart(id);
                let other = glam::Vec3::from_array(*other);
                (candidate.y - other.y).abs() >= SIGN_APART[0] * apart
                    || (candidate.x - other.x).hypot(candidate.z - other.z) >= SIGN_APART[1] * apart
            })
        };
        let columns = SIGN_COLUMNS_AROUND
            .into_iter()
            .filter(|column| f32::from(column.unsigned_abs()) * step <= SIGN_SPREAD + 1e-4);
        // How far it might rise: not at all, or just clear above another sign, within its highest; the lowest first.
        let highest = SIGN_HIGHEST * scale + 1e-4;
        let mut rises: Vec<f32> = std::iter::once(0.0)
            .chain(
                self.0
                    .iter()
                    .map(|(id, other)| other[1] + SIGN_APART[0] * apart(id) + SIGN_SLACK - own.y),
            )
            .filter(|rise| (0.0..=highest).contains(rise))
            .collect();
        rises.sort_by(f32::total_cmp);
        rises.dedup();
        // Its own column first, as low as is clear, so that it rises rather than leaning off aside; then the nearest
        // columns beside it.
        let position = columns
            .flat_map(|column| rises.iter().map(move |rise| place(*rise, column)))
            .find(clear)
            .unwrap_or(own)
            .to_array();
        self.0.insert(station.id.clone(), position);
        self.1.insert(
            station.id.clone(),
            (station.position, station.facing, station.drawn()),
        );
        position
    }
}

fn stations_count_hint(workspace: &Workspace) -> usize {
    workspace
        .nodes
        .iter()
        .take(64)
        .filter(|node| node.parent_id.is_some())
        .count()
}

#[cfg(test)]
fn overlaps(a: &Panel, b: &Panel) -> bool {
    // Bounds are projected from the scene reference observer rather than the
    // world XY plane: signs at the same X but different bearings don't collide.
    let bounds = |p: &Panel| {
        let sign = p.sign.as_ref().expect("only signs are overlap candidates");
        let half = (p.columns * 9 + 6) as f32 * p.pixel * 0.5;
        let mid = (p.columns * 9) as f32 * p.pixel * 0.5;
        let x = p.origin[0] + sign.right[0] * mid;
        let z = p.origin[2] + sign.right[2] * mid;
        let radius = x.hypot(z);
        let angle = x.atan2(-z);
        // Include the shallow curvature and padding conservatively.
        let near = (radius - 0.06).max(0.0);
        let angular_half = if near <= half {
            std::f32::consts::PI
        } else {
            half.atan2(near)
        };
        let top = p.origin[1] + 2.0 * p.pixel - 1.6;
        let bottom = p.origin[1] - (p.lines.len() * 11 + 2) as f32 * p.pixel - 1.6;
        let far = (radius.hypot(half) + 0.01).max(0.0001);
        let near = near.max(0.0001);
        let elevations = [
            top.atan2(near),
            top.atan2(far),
            bottom.atan2(near),
            bottom.atan2(far),
        ];
        let low = elevations.iter().copied().fold(f32::INFINITY, f32::min);
        let high = elevations.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        (angle, angular_half, low, high)
    };
    let (aa, aw, al, ah) = bounds(a);
    let (ba, bw, bl, bh) = bounds(b);
    let delta = (aa - ba).sin().atan2((aa - ba).cos()).abs();
    delta < aw + bw && al < bh && bl < ah
}

fn notice(lines: Vec<String>) -> Panel {
    Panel {
        origin: [-1.1, 0.24, -2.9],
        pixel: 0.0044,
        columns: 56,
        lines: lines.iter().take(3).map(|line| clip(line, 56)).collect(),
        accent: [1.0, 0.75, 0.3],
        opacity: 1.0,
        sign: None,
        rule: None,
    }
}

fn short_ref(text: &str) -> String {
    clip(text, 16)
}

fn quad(mesh: &mut Vec<Vertex>, x: f32, y: f32, z: f32, width: f32, height: f32, color: [f32; 4]) {
    textured(
        mesh,
        [x, y, z, width, height],
        color,
        [SOLID[0], SOLID[1], 0.0, 0.0],
        0.0,
    );
}

/// A quad from its top-left corner `x`, `y`, `z`, `width` across and `height` down, drawn from `tile` of the glyph atlas.
fn textured(mesh: &mut Vec<Vertex>, rect: [f32; 5], color: [f32; 4], tile: [f32; 4], weight: f32) {
    let [x, y, z, width, height] = rect;
    if mesh.len() + 6 > MAX_VERTICES {
        return;
    }
    for [u, v] in [
        [0.0, 0.0],
        [1.0, 0.0],
        [1.0, 1.0],
        [0.0, 0.0],
        [1.0, 1.0],
        [0.0, 1.0],
    ] {
        mesh.push(Vertex {
            position: [x + u * width, y - v * height, z],
            color,
            uv: [tile[0] + u * tile[2], tile[1] + v * tile[3]],
            weight,
        });
    }
}

/// `shape`, from the shared shapes, in font pixels `p` metres each from the panel's text origin, with y down, at depth
/// `z`, as the panel's glyphs are placed; each piece cut into strips no wider than the sign's strips, so that it follows
/// a sign's bow.
fn shape(mesh: &mut Vec<Vertex>, shape: &Shape, p: f32, z: f32) {
    let mut pieces = Vec::new();
    shape.pieces(&mut pieces);
    for piece in pieces {
        for piece in piece.strips(STRIP_PIXELS) {
            if mesh.len() + 6 > MAX_VERTICES {
                return;
            }
            mesh.extend(piece.triangles().map(|([x, y], uv)| Vertex {
                position: [x * p, -y * p, z],
                color: piece.color,
                uv,
                weight: 0.0,
            }));
        }
    }
}

/// The widest strip of a shape on a panel, in font pixels: a sign's backing's, 24 strips across its 28 cells.
const STRIP_PIXELS: f32 = 10.0;

/// The symbol of `state`, drawn as lines about `centre`, `size` font pixels from its middle to its edge, `p` metres to a
/// font pixel: a distinct shape for each state, so that colour is never all that tells them apart.
fn status_symbol(
    mesh: &mut Vec<Vertex>,
    state: State,
    centre: [f32; 2],
    size: f32,
    p: f32,
    color: [f32; 4],
) {
    let mut line = |a: [f32; 2], b: [f32; 2]| {
        // In the symbol's own frame, y up, as it was designed.
        let rule = Shape::Rule {
            from: [centre[0] + a[0] * size, centre[1] - a[1] * size],
            to: [centre[0] + b[0] * size, centre[1] - b[1] * size],
            width: 1.4,
            color,
        };
        shape(mesh, &rule, p, SIGN_TEXT);
    };
    match state {
        State::Completed => {
            line([-1.0, 0.0], [-0.3, -0.7]);
            line([-0.3, -0.7], [1.0, 0.8]);
        }
        State::Error => {
            line([-0.8, -0.8], [0.8, 0.8]);
            line([-0.8, 0.8], [0.8, -0.8]);
        }
        State::Waiting => {
            line([-0.8, 0.9], [0.8, 0.9]);
            line([-0.8, -0.9], [0.8, -0.9]);
            line([-0.7, 0.8], [0.7, -0.8]);
            line([0.7, 0.8], [-0.7, -0.8]);
        }
        State::Running => {
            line([-0.7, -0.9], [-0.7, 0.9]);
            line([-0.7, 0.9], [0.8, 0.0]);
            line([0.8, 0.0], [-0.7, -0.9]);
        }
        State::Idle => {
            line([-0.45, -0.8], [-0.45, 0.8]);
            line([0.45, -0.8], [0.45, 0.8]);
        }
        State::Stopped => {
            line([-0.7, -0.7], [0.7, -0.7]);
            line([0.7, -0.7], [0.7, 0.7]);
            line([0.7, 0.7], [-0.7, 0.7]);
            line([-0.7, 0.7], [-0.7, -0.7]);
        }
        // An exclamation mark.
        State::Stalled => {
            line([0.0, 0.95], [0.0, -0.35]);
            line([0.0, -0.7], [0.0, -0.95]);
        }
        // An envelope, a message waiting.
        State::Unanswered => {
            line([-0.9, 0.6], [0.9, 0.6]);
            line([0.9, 0.6], [0.9, -0.6]);
            line([0.9, -0.6], [-0.9, -0.6]);
            line([-0.9, -0.6], [-0.9, 0.6]);
            line([-0.9, 0.6], [0.0, -0.1]);
            line([0.0, -0.1], [0.9, 0.6]);
        }
        State::Unknown => {
            line([0.0, 0.95], [0.9, 0.0]);
            line([0.9, 0.0], [0.0, -0.95]);
            line([0.0, -0.95], [-0.9, 0.0]);
            line([-0.9, 0.0], [0.0, 0.95]);
        }
    }
}

/// The symbol of a timer's `state`, drawn as [`status_symbol`] draws a worker's: a clock face while it waits, two bars
/// while paused, an arrow through a doorway while it waits for an arrival, a burst just fired, and a diamond for a state
/// this build does not know.
fn timer_symbol(
    mesh: &mut Vec<Vertex>,
    state: TimerState,
    centre: [f32; 2],
    size: f32,
    p: f32,
    color: [f32; 4],
) {
    let mut line = |a: [f32; 2], b: [f32; 2]| {
        let rule = Shape::Rule {
            from: [centre[0] + a[0] * size, centre[1] - a[1] * size],
            to: [centre[0] + b[0] * size, centre[1] - b[1] * size],
            width: 1.4,
            color,
        };
        shape(mesh, &rule, p, SIGN_TEXT);
    };
    match state {
        TimerState::Waiting => {
            let corner = |index: usize| {
                let angle = (index as f32 + 0.5) * std::f32::consts::FRAC_PI_4;
                [angle.sin() * 0.95, angle.cos() * 0.95]
            };
            for index in 0..8 {
                line(corner(index), corner(index + 1));
            }
            line([0.0, 0.0], [0.0, 0.6]);
            line([0.0, 0.0], [0.45, 0.0]);
        }
        TimerState::Paused => {
            line([-0.4, -0.8], [-0.4, 0.8]);
            line([0.4, -0.8], [0.4, 0.8]);
        }
        TimerState::Awaiting => {
            line([0.7, -0.9], [0.7, 0.9]);
            line([-0.9, 0.0], [0.4, 0.0]);
            line([0.4, 0.0], [0.0, 0.4]);
            line([0.4, 0.0], [0.0, -0.4]);
        }
        TimerState::Fired => {
            line([0.0, -0.95], [0.0, 0.95]);
            line([-0.95, 0.0], [0.95, 0.0]);
            line([-0.65, -0.65], [0.65, 0.65]);
            line([-0.65, 0.65], [0.65, -0.65]);
        }
        TimerState::Unknown => {
            line([0.0, 0.95], [0.9, 0.0]);
            line([0.9, 0.0], [0.0, -0.95]);
            line([0.0, -0.95], [-0.9, 0.0]);
            line([-0.9, 0.0], [0.0, 0.95]);
        }
    }
}

/// How a worker's sign is drawn, in font pixels from its text's top-left corner, y down: its plate reaches
/// [`PLATE_MARGIN`] beyond its text across and down, its corners rounded by [`PLATE_RADIUS`]; its title in one colour,
/// [`TITLE`], each row centred; its state as a pill, centred on its last row, [`PILL_HEIGHT`] tall, its symbol in its
/// first [`PILL_SYMBOL`] and its word after; and a leader [`LEADER_WIDTH`] wide from the middle of its plate's bottom
/// edge to just behind its worker's middle, ending in a dot [`LEADER_DOT`] across. Depths, in metres in front of the
/// sign's plane: the leader behind the plate, the pill on it, its text and symbol in front.
const PLATE_MARGIN: [f32; 2] = [2.0, 3.0];
const PLATE_RADIUS: f32 = 3.0;
const PLATE: [f32; 3] = [0.016, 0.03, 0.055];
const TITLE: [f32; 3] = [0.92, 0.95, 1.0];
const PILL_HEIGHT: f32 = 10.0;
const PILL_SYMBOL: f32 = 12.0;
const LEADER_WIDTH: f32 = 1.3;
const LEADER_DOT: f32 = 2.6;
const SIGN_LEADER: f32 = -0.006;
const SIGN_PLATE: f32 = -0.004;
/// A highlighted sign's border, a millimetre behind its plate, so that the plate covers it everywhere but its edge
/// rather than the two fighting for depth.
const SIGN_OUTLINE: f32 = -0.005;
/// How much brighter a sign, its leader and its dot are drawn while its worker is pointed at, as its flock is drawn paler;
/// how far its dark plate is tinted towards its state's colour then; and how wide, in font pixels, the border about its
/// plate's edge is.
const HIGHLIGHT: f32 = 1.15;
const HIGHLIGHT_TINT: f32 = 0.1;
const OUTLINE_WIDTH: f32 = 1.0;
const SIGN_PILL: f32 = -0.002;
const SIGN_TEXT: f32 = 0.0;
/// The most vertices a sign's leader and its dot take.
const LEADER_VERTICES: usize = 120;
/// How deep a sign bows, in metres at the edges of a sign [`SIGN_COLUMNS`] wide, towards whoever it faces: only 5 cm
/// across the roughly one-metre sign, as much for a narrower sign along the same curve.
const SIGN_BOW: f32 = 0.05;

/// A worker's sign: its plate, its title and its state's pill, in the sign's own frame, `p` metres to a font pixel, x
/// across and y up from its text's top-left corner, bowed towards whoever it faces along +z, before it is placed.
/// Returns how many of its first vertices are its plate, its surface, which hides what is behind it.
fn sign_mesh(panel: &Panel, sign: &Sign, mesh: &mut Vec<Vertex>) -> usize {
    let start = mesh.len();
    let p = panel.pixel;
    // Pointed at, everything on it is drawn brighter, as its flock is.
    let lit = if sign.highlighted { HIGHLIGHT } else { 1.0 };
    let color = |rgb: [f32; 3], alpha: f32| {
        [
            (rgb[0] * lit).min(1.0),
            (rgb[1] * lit).min(1.0),
            (rgb[2] * lit).min(1.0),
            alpha * panel.opacity,
        ]
    };
    let width = (panel.columns * 9) as f32;
    let rows = panel.lines.len();
    let height = (rows * 11) as f32;
    // The plate, first, so that it is the surface.
    // Pointed at, its plate is tinted towards its state's colour, so that so dark a plate is seen to brighten.
    let plate_rgb = if sign.highlighted {
        std::array::from_fn(|index| PLATE[index] + panel.accent[index] * HIGHLIGHT_TINT)
    } else {
        PLATE
    };
    // Pointed at, a thin border in its state's colour about its plate's edge, so that which sign is its flock's is
    // plain: its plate drawn over a plate a little larger, in that colour, both its surface.
    if sign.highlighted {
        let border = Shape::Rect {
            x: -PLATE_MARGIN[0] - OUTLINE_WIDTH,
            y: -PLATE_MARGIN[1] - OUTLINE_WIDTH,
            width: width + 2.0 * (PLATE_MARGIN[0] + OUTLINE_WIDTH),
            height: height + 2.0 * (PLATE_MARGIN[1] + OUTLINE_WIDTH),
            color: color(panel.accent, 0.9),
            radius: PLATE_RADIUS + OUTLINE_WIDTH,
            outline: None,
        };
        shape(mesh, &border, p, SIGN_OUTLINE);
    }
    let plate = Shape::Rect {
        x: -PLATE_MARGIN[0],
        y: -PLATE_MARGIN[1],
        width: width + 2.0 * PLATE_MARGIN[0],
        height: height + 2.0 * PLATE_MARGIN[1],
        color: color(plate_rgb, 1.0),
        radius: PLATE_RADIUS,
        outline: None,
    };
    shape(mesh, &plate, p, SIGN_PLATE);
    let surface = mesh.len();
    // A row, centred across the sign.
    let row = |mesh: &mut Vec<Vertex>, text: &str, x: f32, index: usize, rgb: [f32; 3]| {
        let cells: Vec<(char, ())> =
            crate::ui_text::cut(&[crate::ui_text::Span::new(text, ROW_STYLE)], panel.columns)
                .into_iter()
                .map(|(ch, _)| (ch, ()))
                .collect();
        let top = (index * 11) as f32;
        for (glyph, ()) in crate::ui_text::quads(&cells, x, top, 11.0, |()| false) {
            textured(
                mesh,
                [
                    glyph.x * p,
                    -glyph.y * p,
                    SIGN_TEXT,
                    glyph.width * p,
                    glyph.height * p,
                ],
                color(rgb, 1.0),
                glyph.tile,
                glyph.weight,
            );
        }
    };
    let (status, title) = panel
        .lines
        .split_last()
        .expect("a sign has its state's row");
    // The title, every row in one colour; a timer's schedule and condition, after its name, muted.
    let named = sign.timer.map_or(usize::MAX, |(_, named)| named);
    for (index, line) in title.iter().enumerate() {
        let across = crate::ui_text::width(line).min(panel.columns) as f32 * 9.0;
        let rgb = if index < named { TITLE } else { MUTED };
        row(mesh, line, ((width - across) / 2.0).round(), index, rgb);
    }
    // Its state, a pill on the last row: tinted, its symbol, then its word, in the state's colour.
    let top = height - 11.0;
    let pill = pill_width(status);
    let left = ((width - pill) / 2.0).round();
    shape(
        mesh,
        &Shape::Rect {
            x: left,
            y: top + (11.0 - PILL_HEIGHT) / 2.0,
            width: pill,
            height: PILL_HEIGHT,
            color: color(panel.accent, 0.2),
            radius: PILL_HEIGHT / 2.0,
            outline: None,
        },
        p,
        SIGN_PILL,
    );
    let centre = [left + 1.0 + PILL_SYMBOL / 2.0, top + 5.5];
    match sign.timer {
        Some((state, _)) => timer_symbol(mesh, state, centre, 3.0, p, color(panel.accent, 1.0)),
        None => status_symbol(mesh, sign.state, centre, 3.0, p, color(panel.accent, 1.0)),
    }
    row(
        mesh,
        status,
        left + 1.0 + PILL_SYMBOL,
        rows - 1,
        panel.accent,
    );
    // Bowed about its middle.
    let middle = width * p / 2.0;
    let reach = (SIGN_COLUMNS * 9) as f32 * p / 2.0;
    for vertex in &mut mesh[start..] {
        let [x, _, z] = &mut vertex.position;
        *z += SIGN_BOW * ((*x - middle) / reach).powi(2);
    }
    surface
}

/// A sign's leader, from the middle of its plate's bottom edge, where it stands now, to its dot, just behind its
/// worker's middle from whoever it faces, wherever the sign stands, in the state's colour, faint, in the scene: a line
/// and a dot in the plane through them that faces whoever the sign faces, so that both face them too.
fn leader_mesh(panel: &Panel, sign: &Sign, opacity: f32, mesh: &mut Vec<Vertex>) {
    let p = panel.pixel;
    let right = glam::Vec3::from_array(sign.right);
    let towards = glam::Vec3::from_array(sign.towards);
    let height = (panel.lines.len() * 11) as f32 + PLATE_MARGIN[1];
    let from = glam::Vec3::from_array(panel.origin)
        + right * ((panel.columns * 9) as f32 * p / 2.0)
        - glam::Vec3::Y * height * p;
    let to = glam::Vec3::from_array(sign.station) - towards * SIGN_BEHIND;
    let length = from.distance(to);
    let up = (from - to) / length;
    let across = up.cross(towards).try_normalize().unwrap_or(right);
    let out = across.cross(up);
    // Pointed at, brighter and plainer, as its sign and flock are.
    let (lit, leader, dot) = if sign.highlighted {
        (HIGHLIGHT, 0.8, 1.0)
    } else {
        (1.0, 0.55, 0.85)
    };
    let accent = |alpha: f32| {
        [
            (panel.accent[0] * lit).min(1.0),
            (panel.accent[1] * lit).min(1.0),
            (panel.accent[2] * lit).min(1.0),
            alpha * opacity,
        ]
    };
    // A sign's bottom edge stands above its worker, so its leader always has a length.
    let mut pieces = Vec::new();
    Shape::Rule {
        from: [0.0, 0.0],
        to: [0.0, -length / p],
        width: LEADER_WIDTH,
        color: accent(leader),
    }
    .pieces(&mut pieces);
    Shape::Disc {
        centre: [0.0, 0.0],
        radius: LEADER_DOT,
        color: accent(dot),
    }
    .pieces(&mut pieces);
    for piece in pieces {
        mesh.extend(piece.triangles().map(|([x, y], uv)| Vertex {
            position: (to + across * (x * p) - up * (y * p) + out * SIGN_LEADER).to_array(),
            color: piece.color,
            uv,
            weight: 0.0,
        }));
    }
}

/// A panel's mesh, in its own frame: a sign's, before it is placed and its leader drawn, or a card's or notice's, which
/// stands where it is made. Returns how many of its first vertices are its surface, which hides what is behind it.
fn local_mesh(panel: &Panel, mesh: &mut Vec<Vertex>) -> usize {
    let start = mesh.len();
    let end = match &panel.sign {
        Some(sign) => sign_mesh(panel, sign, mesh),
        None => card_mesh(panel, mesh),
    };
    end - start
}

/// `mesh`, `panel`'s made by [`local_mesh`], its first `surface` vertices its surface, placed where `panel` stands and
/// added to `out`: a sign's turned to face whoever it faces, and its leader drawn after its surface, `opacity` opaque,
/// as `mesh` was made; a card's or notice's moved to its origin.
fn place(panel: &Panel, mesh: &[Vertex], surface: usize, opacity: f32, out: &mut Vec<Vertex>) {
    let origin = glam::Vec3::from_array(panel.origin);
    let Some(sign) = &panel.sign else {
        out.extend(mesh.iter().map(|vertex| Vertex {
            position: (origin + glam::Vec3::from_array(vertex.position)).to_array(),
            ..*vertex
        }));
        return;
    };
    let right = glam::Vec3::from_array(sign.right);
    let normal = glam::Vec3::from_array(sign.normal);
    let placed = |vertex: &Vertex| {
        let [x, y, z] = vertex.position;
        Vertex {
            position: (origin + right * x + glam::Vec3::Y * y + normal * z).to_array(),
            ..*vertex
        }
    };
    let surface = surface.min(mesh.len());
    out.extend(mesh[..surface].iter().map(placed));
    leader_mesh(panel, sign, opacity, out);
    out.extend(mesh[surface..].iter().map(placed));
}

/// A panel's mesh, where it stands: a sign's, its leader included, or a card's or notice's. Returns how many of its
/// first vertices are its surface, which hides what is behind it.
#[cfg(test)]
fn panel_mesh(panel: &Panel, mesh: &mut Vec<Vertex>) -> usize {
    let mut local = Vec::new();
    let surface = local_mesh(panel, &mut local);
    place(panel, &local, surface, panel.opacity, mesh);
    surface
}

/// A card's or notice's mesh, in its own frame: its background, its accent bar, its rule and its rows. Returns the
/// length of the mesh once its background is in it.
fn card_mesh(panel: &Panel, mesh: &mut Vec<Vertex>) -> usize {
    let (x, y, z) = (0.0, 0.0, 0.0);
    let p = panel.pixel;
    let width = (panel.columns * 9 + 4) as f32 * p;
    let height = (panel.lines.len() * 11 + 4) as f32 * p;
    let color = |rgb: [f32; 3]| [rgb[0], rgb[1], rgb[2], panel.opacity];
    quad(
        mesh,
        x - p * 2.0,
        y + p * 2.0,
        z - 0.003,
        width,
        height,
        color([0.012, 0.024, 0.045]),
    );
    quad(
        mesh,
        x - p * 3.0,
        y + p * 2.0,
        z,
        p,
        height,
        color(panel.accent),
    );
    let surface = mesh.len();
    if let Some(row) = panel.rule.filter(|&row| row < ROWS.min(panel.lines.len())) {
        let middle = (row * 11) as f32 + 5.5;
        let rule = Shape::Rule {
            from: [0.0, middle],
            to: [(panel.columns * 9) as f32, middle],
            width: 1.0,
            color: color(MUTED),
        };
        shape(mesh, &rule, p, z);
    }
    for (row, line) in panel.lines.iter().take(ROWS).enumerate() {
        let color = color(if row == 0 {
            panel.accent
        } else if row < 4 && panel.columns == COLUMNS {
            MUTED
        } else {
            WHITE
        });
        // Cut by the cells it takes as drawn, with "…", never leaving half a wide character.
        let cells: Vec<(char, ())> = crate::ui_text::cut(
            &[crate::ui_text::Span::new(line.as_str(), ROW_STYLE)],
            panel.columns,
        )
        .into_iter()
        .map(|(ch, _)| (ch, ()))
        .collect();
        let top = (row * 11) as f32;
        for (glyph, ()) in crate::ui_text::quads(&cells, 0.0, top, 11.0, |()| false) {
            textured(
                mesh,
                [
                    x + glyph.x * p,
                    y - glyph.y * p,
                    z,
                    glyph.width * p,
                    glyph.height * p,
                ],
                color,
                glyph.tile,
                glyph.weight,
            );
        }
    }
    surface
}

#[cfg(test)]
mod depth_writes;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation::{Kind, Node, Presentation, Scene};
    use crate::test_gpu;
    use tomori_scene::cards::{CardNavigation, content_pages};

    fn node(id: &str, state: State) -> Node {
        Node {
            id: id.into(),
            parent_id: Some("root".into()),
            title: format!("Task {id}"),
            state,
        }
    }

    fn card(id: &str) -> Presentation {
        Presentation {
            id: id.into(),
            title: format!("Result {id}"),
            source_id: "worker-123".into(),
            source_message_id: "message-456".into(),
            text: "Curated result text".into(),
            ..Default::default()
        }
    }

    fn scene(workspace: &Workspace) -> Scene {
        let mut scene = Scene::new();
        for _ in 0..30 {
            scene.update(Some("root"), workspace, 0.05);
        }
        scene
    }

    fn test_layout(
        workspace: &Workspace,
        stations: &[Station],
        page: usize,
        content: usize,
    ) -> (Vec<Panel>, usize) {
        let cards = results(workspace);
        let mut cache = ContentCache::default();
        cache.update(cards.get(page.min(cards.len().saturating_sub(1))).copied());
        layout(
            workspace,
            stations,
            page,
            content,
            &cache.pages,
            &mut LabelPlacement::default(),
            None,
        )
    }

    /// The glyphs `panel`'s mesh draws, as its quads that sample a glyph rather than the solid block or a shape.
    fn glyph_quads(panel: &Panel) -> usize {
        let mut mesh = Vec::new();
        panel_mesh(panel, &mut mesh);
        mesh.iter()
            .filter(|vertex| vertex.uv != SOLID && !shaped(vertex.uv))
            .count()
            / 6
    }

    /// Whether `uv` is in one of the atlas's shapes' tiles, as a shape's pieces are, rather than a glyph's.
    fn shaped([u, v]: [f32; 2]) -> bool {
        use tomori_text::atlas::{DISC, FRAME, RING, SPREAD, SQUARE, WEDGE};
        let margin = 3.0 * SPREAD;
        let figure = [SQUARE, DISC, RING, FRAME].iter().any(|figure| {
            let reach = figure.reach + margin;
            (u - figure.middle[0]).abs() <= reach && (v - figure.middle[1]).abs() <= reach
        });
        let wedge = u >= WEDGE.start[0] - margin
            && u <= WEDGE.start[0] + WEDGE.length + margin
            && (v - WEDGE.start[1]).abs() <= WEDGE.widest / 2.0 + margin;
        figure || wedge
    }

    /// The glyphs `lines` should draw on a panel `columns` wide: every cell but spaces and the second halves of wide characters, once each line is cut to fit.
    fn glyphs_in(lines: &[String], columns: usize) -> usize {
        lines
            .iter()
            .take(ROWS)
            .map(|line| {
                crate::ui_text::cut(
                    &[crate::ui_text::Span::new(line.as_str(), ROW_STYLE)],
                    columns,
                )
                .iter()
                .filter(|(ch, _)| *ch != ' ' && *ch != crate::ui_text::WIDE)
                .count()
            })
            .sum()
    }

    #[test]
    fn a_compact_sign_is_drawn_and_spaced_smaller_and_a_gathered_sign_says_how_many_it_stands_for()
    {
        let station = Station {
            facing: crate::delegation::HOME_EYES,
            id: "w0".into(),
            title: "Investigate the coop".into(),
            state: State::Running,
            position: [0.0, 1.25, -2.8],
            opacity: 1.0,
            scale: 1.0,
            more: 0,
            size: None,
            highlighted: false,
        };
        let full = station_panel(&station, station.position, State::Running, None);
        let small = Station {
            scale: tomori_protocol::cluster::COMPACT,
            ..station.clone()
        };
        let compacted = station_panel(&small, small.position, State::Running, None);
        assert!((compacted.pixel - full.pixel * tomori_protocol::cluster::COMPACT).abs() < 1e-7);
        assert_eq!(
            compacted.sign.as_ref().unwrap().scale,
            tomori_protocol::cluster::COMPACT
        );
        // Out of bounds, as from a spectator's frame, within them.
        for (scale, drawn) in [(f32::NAN, 1.0), (0.0, 0.1), (3.0, 1.0)] {
            assert_eq!(
                compact(&Station {
                    scale,
                    ..station.clone()
                }),
                drawn
            );
        }
        // Signs of a compact cluster, its columns 0.735 m apart, each stand above their own station.
        let mut placements = LabelPlacement::default();
        let beside = Station {
            id: "w1".into(),
            position: [0.735, 1.25, -2.8],
            ..small.clone()
        };
        assert_eq!(placements.position(&small), small.position);
        assert_eq!(placements.position(&beside), beside.position);
        placements.retain(&[&small, &beside]);
        assert_eq!(placements.position(&beside), beside.position);
        // The sign for four workers gathered into a cluster's "+N more" says so in its pill.
        let gathered = Station {
            title: String::new(),
            more: 4,
            ..small.clone()
        };
        let sign = station_panel(&gathered, gathered.position, State::Running, None);
        assert_eq!(sign.lines.last().unwrap(), "+4 more");
        assert_eq!(sign.accent, MORE_ACCENT);
        // Standing for those it gathers, it leaves the projection complete; without it, it is not.
        let nodes = (0..5)
            .map(|i| crate::delegation::Node {
                id: format!("w{i}"),
                parent_id: Some("root".into()),
                title: String::new(),
                state: State::Running,
            })
            .collect();
        let workspace = Workspace {
            nodes,
            ..Default::default()
        };
        let incomplete = |panels: &[Panel]| {
            panels
                .iter()
                .any(|panel| panel.lines.iter().any(|line| line.contains("incomplete")))
        };
        let gathered = Station {
            id: "w1".into(),
            ..gathered
        };
        let (panels, _) = test_layout(&workspace, &[station.clone(), gathered], 0, 0);
        assert!(!incomplete(&panels));
        let (panels, _) = test_layout(&workspace, &[station], 0, 0);
        assert!(incomplete(&panels));
    }

    #[test]
    fn signs_and_cards_cut_japanese_and_mixed_titles_by_the_cells_they_take() {
        use crate::ui_text::width;
        let titles = [
            "日本語のタイトルです。".repeat(4),
            "Tomoriは日本語 🎉 mixed text ".repeat(6),
            "🎤".repeat(160),
            "日本語".repeat(60),
            format!("{}日本", "x".repeat(27)),
            "完了".into(),
        ];
        for title in &titles {
            // A sign: rows of at most its width in cells, never more than its most rows, and a title that does not fit them ends with "…".
            let lines = label_lines(title, "Finished");
            let (status, rows) = lines.split_last().unwrap();
            assert_eq!(status, "Finished");
            assert!(rows.len() <= SIGN_TITLE_ROWS, "{lines:?}");
            assert!(
                rows.iter().all(|row| width(row) <= SIGN_COLUMNS),
                "{lines:?}"
            );
            let kept: String = rows.concat().chars().filter(|ch| *ch != ' ').collect();
            let whole: String = crate::ui_text::clip(title, usize::MAX)
                .chars()
                .filter(|ch| *ch != ' ')
                .collect();
            assert!(
                kept == whole || rows.last().unwrap().ends_with('…'),
                "{title}: {lines:?}"
            );
            // Its mesh draws every glyph of every row: none is lost past the sign's edge.
            let station = Station {
                facing: crate::delegation::HOME_EYES,
                id: "a".into(),
                title: title.clone(),
                state: State::Completed,
                position: [0.0, 1.25, -2.8],
                opacity: 1.0,
                scale: 1.0,
                more: 0,
                size: None,
                highlighted: false,
            };
            let sign = station_panel(&station, station.position, State::Completed, None);
            assert_eq!(
                glyph_quads(&sign),
                glyphs_in(&sign.lines, SIGN_COLUMNS),
                "{title}"
            );
            assert_eq!(
                glyphs_in(&sign.lines, SIGN_COLUMNS),
                sign.lines
                    .iter()
                    .map(|line| crate::ui_text::drawn(line)
                        .filter(|ch| *ch != ' ' && *ch != crate::ui_text::WIDE)
                        .count())
                    .sum::<usize>(),
                "{title}"
            );
            // A card: its title and source fit its width in cells, ending with "…" where cut, and it draws every glyph of them.
            let mut result = card("a");
            result.title = title.clone();
            result.source_id = format!("ワーカー🤖{title}");
            let workspace = Workspace {
                presentations: vec![result],
                ..Default::default()
            };
            let (panels, _) = test_layout(&workspace, &[], 0, 0);
            let lines = &panels[0].lines;
            for line in &lines[..2] {
                assert!(width(line) <= COLUMNS, "{line}");
            }
            assert!(
                width(title) <= COLUMNS || lines[0].ends_with('…'),
                "{lines:?}"
            );
            assert_eq!(
                glyph_quads(&panels[0]),
                glyphs_in(lines, COLUMNS),
                "{title}"
            );
        }
        // The Japanese title fits its sign whole, fourteen characters a row.
        let lines = label_lines(&titles[0], "Finished");
        assert_eq!(lines[..lines.len() - 1].concat(), titles[0]);
        assert!(lines.iter().all(|line| !line.contains('…')));
        // The longest Japanese title is cut at the sign's last row, with "…".
        let lines = label_lines(&titles[3], "Finished");
        assert_eq!(lines.len(), SIGN_TITLE_ROWS + 1);
        assert!(lines[SIGN_TITLE_ROWS - 1].ends_with('…'), "{lines:?}");
        // A wide character that would cross the edge of a row goes to the next, whole.
        assert_eq!(
            label_lines(&titles[4], "IDLE"),
            ["x".repeat(27), "日本".into(), "IDLE".into()]
        );
        // A sign with no title still has its row.
        assert_eq!(label_lines("", "IDLE"), ["", "IDLE"]);
    }

    #[test]
    fn sorted_pages_have_sources_and_clamp_requested_page() {
        let workspace = Workspace {
            presentations: ["e", "b", "a", "d", "c"].into_iter().map(card).collect(),
            ..Default::default()
        };
        let (first, pages) = test_layout(&workspace, &[], 0, 0);
        assert_eq!(pages, 5);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].lines[0], "Result a");
        assert!(first[0].lines[1].contains("worker-123 / message-456"));
        assert!(first[0].lines[2].contains("1/5"));
        let (last, _) = test_layout(&workspace, &[], usize::MAX, usize::MAX);
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].lines[0], "Result e");
        assert!(last[0].lines[2].contains("5/5"));
        assert_eq!(first[0].origin, last[0].origin);
    }

    #[test]
    fn empty_and_stale_cards_and_control_characters_say_so() {
        let mut empty = card("a");
        empty.text.clear();
        assert_eq!(content_pages(&empty), [["[Empty content]"]]);
        empty.kind = Kind::Table;
        assert_eq!(content_pages(&empty), [["[Empty table]"]]);
        let workspace = Workspace {
            presentations: vec![card("a")],
            stale: true,
            ..Default::default()
        };
        let (panels, _) = test_layout(&workspace, &[], 0, 0);
        assert!(panels[0].lines[2].ends_with(" [STALE]"));
        assert_eq!(
            label_lines("tab\there\nnewline", "IDLE"),
            ["tab here newline", "IDLE"]
        );
    }

    #[test]
    fn every_supported_station_has_a_complete_unique_unclipped_label() {
        for count in [16, 17, 64, 128] {
            for legacy in [false, true] {
                let stations: Vec<_> = (0..count)
                    .map(|i| Station {
                        facing: crate::delegation::HOME_EYES,
                        id: format!("station-{i:03}"),
                        title: format!("{i:03}{}", "W".repeat(157)),
                        position: if legacy {
                            [0.0, 1.4, -2.9]
                        } else {
                            crate::delegation::slot_position(i).to_array()
                        },
                        state: State::Completed,
                        opacity: 1.0,
                        scale: 1.0,
                        more: 0,
                        size: None,
                        highlighted: false,
                    })
                    .collect();
                let mut placements = LabelPlacement::default();
                let (panels, _) = layout(
                    &Workspace::default(),
                    &stations,
                    0,
                    0,
                    &[],
                    &mut placements,
                    None,
                );
                assert_eq!(panels.len(), count, "count={count}, legacy={legacy}");
                let ids: std::collections::BTreeSet<_> = panels
                    .iter()
                    .map(|p| p.sign.as_ref().unwrap().source_id.as_str())
                    .collect();
                assert_eq!(ids.len(), count);
                for (i, panel) in panels.iter().enumerate() {
                    assert_eq!(
                        panel.lines[..panel.lines.len() - 1].concat(),
                        stations[i].title
                    );
                    assert_eq!(panel.lines.last().unwrap(), "Unknown");
                    assert_eq!(panel.sign.as_ref().unwrap().station, stations[i].position);
                    // At one spot, the signs about it hold 30 stations apart, three columns within 30° of ten rows;
                    // beyond that they may overlap, rather than stand far off (see `LabelPlacement`).
                    assert!(
                        (legacy && count > 30)
                            || panels[i + 1..].iter().all(|other| !overlaps(panel, other)),
                        "overlap: count={count}, legacy={legacy}, source={i}"
                    );
                }
                let mut cache = PageCache::default();
                cache.update(panels.clone());
                assert_eq!(cache.ranges.len(), count);
                // Every sign's mesh, and its leader, drawn as it is placed.
                let meshes = cache
                    .meshes
                    .iter()
                    .map(|(_, mesh, _, _)| mesh.len())
                    .sum::<usize>();
                assert!(
                    (meshes + count..=meshes + count * LEADER_VERTICES)
                        .contains(&cache.vertices.len())
                );
                assert_eq!(
                    cache.ranges.last().unwrap().vertices.end as usize,
                    cache.vertices.len()
                );
                assert!(cache.vertices.len() <= MAX_VERTICES);
                // Where the middle of each sign's bottom edge stands, which a title of another length leaves where it is.
                let bottom = |p: &Panel| {
                    let right = glam::Vec3::from_array(p.sign.as_ref().unwrap().right);
                    (glam::Vec3::from_array(p.origin)
                        + right * ((p.columns * 9) as f32 * p.pixel / 2.0)
                        - glam::Vec3::Y * ((p.lines.len() * 11 + 4) as f32 * p.pixel))
                        .to_array()
                };
                let before: std::collections::BTreeMap<_, _> = panels
                    .iter()
                    .map(|p| (p.sign.as_ref().unwrap().source_id.clone(), bottom(p)))
                    .collect();
                let mut edited = stations.clone();
                edited.reverse();
                for s in &mut edited {
                    s.title = "Edited".into();
                    s.state = State::Error;
                    s.opacity = 0.5;
                }
                let (next, _) = layout(
                    &Workspace::default(),
                    &edited,
                    0,
                    0,
                    &[],
                    &mut placements,
                    None,
                );
                for p in &next {
                    let at = bottom(p);
                    let was = before[&p.sign.as_ref().unwrap().source_id];
                    assert!(
                        (0..3).all(|axis| (at[axis] - was[axis]).abs() < 1e-5),
                        "{at:?} {was:?}"
                    );
                }
                let mut faded = panels;
                for p in &mut faded {
                    p.opacity = 0.4;
                }
                let builds = cache.builds;
                cache.update(faded);
                assert_eq!(cache.builds, builds);
            }
        }
    }

    #[test]
    fn all_192_shared_cells_have_clearance_for_maximum_height_labels() {
        let panels: Vec<_> = (0..192)
            .map(|i| {
                let position = crate::delegation::slot_position(i).to_array();
                station_panel(
                    &Station {
                        facing: crate::delegation::HOME_EYES,
                        id: i.to_string(),
                        title: "W".repeat(160),
                        position,
                        state: State::Unknown,
                        opacity: 1.0,
                        scale: 1.0,
                        more: 0,
                        size: None,
                        highlighted: false,
                    },
                    position,
                    State::Unknown,
                    None,
                )
            })
            .collect();
        for (i, panel) in panels.iter().enumerate() {
            for (j, other) in panels.iter().enumerate().skip(i + 1) {
                assert!(!overlaps(panel, other), "cell envelopes overlap: {i}, {j}");
            }
        }
    }

    #[test]
    fn a_sign_stands_above_its_own_station_and_beside_those_placed_with_it() {
        let station = |id: &str, position: [f32; 3]| Station {
            facing: crate::delegation::HOME_EYES,
            id: id.into(),
            title: "A title".into(),
            state: State::Running,
            opacity: 1.0,
            position,
            scale: 1.0,
            more: 0,
            size: None,
            highlighted: false,
        };
        let mut placements = LabelPlacement::default();
        // Placed by an environment off the default arc, or near home, a sign stands above its own station.
        for alone in [
            station("pod", [6.0, 1.2, -9.0]),
            station("home", [0.5, 1.6, -0.5]),
        ] {
            assert_eq!(placements.position(&alone), alone.position);
        }
        // Stations clustered about the pod, 10.8 m from home, stand their signs straight above their own place, each
        // rising only as far as clears the others, as far apart as their larger signs need, rather than moving aside.
        let cluster: Vec<Station> = (0..3)
            .map(|i| station(&format!("near-{i}"), [6.0 + 0.1 * i as f32, 1.2, -9.0]))
            .collect();
        let scale = sign_scale([6.0, 1.2, -9.0], crate::delegation::HOME_EYES);
        assert!((scale - 6f32.hypot(9.0) / SIGN_REACH).abs() < 1e-5);
        let signs: Vec<glam::Vec3> = cluster
            .iter()
            .map(|near| glam::Vec3::from_array(placements.position(near)))
            .collect();
        // The first rises just clear of the pod's, rather than moving a column aside.
        let rise = SIGN_APART[0] * scale + SIGN_SLACK;
        let first = signs[0];
        assert!((first.y - (1.2 + rise)).abs() < 1e-4, "{first}");
        for (index, (sign, near)) in signs.iter().zip(&cluster).enumerate() {
            let near = glam::Vec3::from_array(near.position);
            // Straight above its station, no higher than just clear of those below, as the largest is drawn.
            assert_eq!([sign.x, sign.z], [near.x, near.z], "{index}");
            let largest = sign_scale(cluster[2].position, crate::delegation::HOME_EYES);
            assert!(
                sign.y - near.y
                    <= (index + 1) as f32 * (SIGN_APART[0] * largest + SIGN_SLACK) + 1e-4,
                "{index}: {sign}"
            );
            for other in signs[index + 1..]
                .iter()
                .chain([&glam::Vec3::new(6.0, 1.2, -9.0)])
            {
                // Clear of the others above and below it, since they stand together.
                assert!(
                    (sign.y - other.y).abs() >= SIGN_APART[0] * scale - 1e-4,
                    "{index}: {sign} {other}"
                );
            }
        }
        // Stations at one spot near home, where columns about home cannot part them, stack in rows above it.
        let mut stacked = LabelPlacement::default();
        let rows: Vec<f32> = (0..3)
            .map(|i| stacked.position(&station(&format!("at-{i}"), [0.0, 1.0, 0.0]))[1])
            .collect();
        let rise = SIGN_APART[0] + SIGN_SLACK;
        for (row, y) in rows.iter().enumerate() {
            assert!((y - (1.0 + row as f32 * rise)).abs() < 1e-5, "{rows:?}");
        }
        // A station that moves takes its sign with it, and the place it left is free again.
        let moved = station("pod", [-6.0, 1.2, -9.0]);
        let mut all: Vec<&Station> = cluster.iter().chain([&moved]).collect();
        all.sort_by(|a, b| a.id.cmp(&b.id));
        placements.retain(&all);
        assert_eq!(placements.position(&moved), moved.position);
        assert_eq!(placements.0.len(), 4);
        assert!(!placements.0.contains_key("home"));
    }

    #[test]
    fn signs_at_one_spot_stay_within_30_degrees_and_ten_rows_and_then_overlap() {
        let station = |i: usize, position: [f32; 3]| Station {
            facing: crate::delegation::HOME_EYES,
            id: format!("w-{i:02}"),
            title: "A title".into(),
            state: State::Running,
            opacity: 1.0,
            position,
            scale: 1.0,
            more: 0,
            size: None,
            highlighted: false,
        };
        // 64 at one spot 2 m ahead, and 64 at one spot just ahead of home, where columns cannot part them.
        for spot in [[0.0, 1.2, -2.0], [0.0, 1.2, -0.3]] {
            let stations: Vec<Station> = (0..64).map(|i| station(i, spot)).collect();
            let mut placements = LabelPlacement::default();
            let signs: Vec<glam::Vec3> = stations
                .iter()
                .map(|s| glam::Vec3::from_array(placements.position(s)))
                .collect();
            for sign in &signs {
                // Within 30° of the spot's bearing from home, so never behind the participant, nor more than ten rows
                // up.
                assert!(sign.x.atan2(-sign.z).abs() <= SIGN_SPREAD + 1e-4, "{sign}");
                assert!(sign.z <= 1e-4, "{sign}");
                assert!(sign.y <= 1.2 + SIGN_HIGHEST + 1e-4, "{sign}");
                assert!(sign.y >= 1.2 - 1e-4, "{sign}");
            }
            if spot[2] == -0.3 {
                // Only one column fits near home: ten, each just clear of the one below, and then the rest stand on the
                // station, overlapping.
                assert!((signs[9].y - (1.2 + 9.0 * (SIGN_APART[0] + SIGN_SLACK))).abs() < 1e-4);
                assert_eq!(signs[10], glam::Vec3::from_array(spot));
            }
        }
    }

    #[test]
    fn a_sign_whose_worker_is_pointed_at_is_outlined_and_drawn_brighter_its_leader_too() {
        let station = |highlighted: bool| Station {
            facing: crate::delegation::HOME_EYES,
            id: "w".into(),
            title: "Pointed at".into(),
            state: State::Running,
            opacity: 1.0,
            position: [1.0, 1.4, -3.0],
            scale: 1.0,
            more: 0,
            size: None,
            highlighted,
        };
        // Its plate, the rest of it, and its leader's line and dot, which are drawn in its state's colour, faint.
        let draw = |highlighted: bool| {
            let station = station(highlighted);
            let panel = station_panel(&station, station.position, State::Running, None);
            assert_eq!(panel.sign.as_ref().unwrap().highlighted, highlighted);
            let mut mesh = Vec::new();
            let surface = panel_mesh(&panel, &mut mesh);
            let accent = panel.accent;
            let leader: Vec<Vertex> = mesh[surface..]
                .iter()
                .filter(|v| {
                    v.position[1]
                        < panel.origin[1] - (panel.lines.len() * 11 + 4) as f32 * panel.pixel
                })
                .copied()
                .collect();
            (
                mesh[..surface].to_vec(),
                mesh[surface..].to_vec(),
                leader,
                accent,
            )
        };
        let (plate, rest, leader, accent) = draw(false);
        let (lit_plate, lit_rest, lit_leader, _) = draw(true);
        // Its border: a plate a little larger beneath its own, in its state's colour, reaching an outline's width further,
        // and its antialiased edge with it.
        let extent = |plate: &[Vertex]| {
            let ys = plate.iter().map(|v| v.position[1]);
            ys.clone().fold(f32::NEG_INFINITY, f32::max) - ys.fold(f32::INFINITY, f32::min)
        };
        let pixel = SIGN_PIXEL;
        let grown = extent(&lit_plate) - extent(&plate);
        assert!(
            grown >= 2.0 * OUTLINE_WIDTH * pixel - 1e-5 && grown <= 4.0 * OUTLINE_WIDTH * pixel,
            "{grown}"
        );
        let bright = |v: &Vertex| v.color[0] + v.color[1] + v.color[2];
        assert!(lit_plate.iter().any(|v| {
            (0..3).all(|i| (v.color[i] - (accent[i] * HIGHLIGHT).min(1.0)).abs() < 1e-4)
                && v.color[3] > 0.85
        }));
        // Its plate, tinted towards that colour, its text and pill, and its leader, all brighter.
        let darkest = |plate: &[Vertex]| plate.iter().map(bright).fold(f32::INFINITY, f32::min);
        assert!(darkest(&lit_plate) > darkest(&plate) + 0.05);
        let total = |vertices: &[Vertex]| vertices.iter().map(bright).sum::<f32>();
        assert_eq!(rest.len(), lit_rest.len());
        assert!(total(&lit_rest) > total(&rest) * 1.05);
        assert!(!leader.is_empty() && leader.len() == lit_leader.len());
        let alpha = |vertices: &[Vertex]| vertices.iter().map(|v| v.color[3]).sum::<f32>();
        assert!(alpha(&lit_leader) > alpha(&leader) * 1.15);
        assert!(total(&lit_leader) > total(&leader) * 1.05);
    }

    #[test]
    fn a_sign_moved_off_its_own_place_keeps_a_post_down_to_its_station() {
        let station = Station {
            facing: crate::delegation::HOME_EYES,
            id: "w".into(),
            title: "A title".into(),
            state: State::Running,
            opacity: 1.0,
            position: [0.0, 1.2, -3.0],
            scale: 1.0,
            more: 0,
            size: None,
            highlighted: false,
        };
        // Where its leader ends: the lowest point of its mesh, its dot about the station's middle.
        let leader_end = |sign: [f32; 3], viewer: Option<[f32; 3]>| {
            let panel = station_panel(&station, sign, State::Running, viewer);
            let mut mesh = Vec::new();
            panel_mesh(&panel, &mut mesh);
            mesh.iter()
                .map(|vertex| glam::Vec3::from_array(vertex.position))
                .min_by(|a, b| a.y.total_cmp(&b.y))
                .unwrap()
        };
        let middle = glam::Vec3::from_array(station.position);
        // In its own place, straight above it, stacked above it, or beside it, and seen from home, from either side or
        // from beside the station, its leader reaches down to the station's middle, ending in a dot just behind it from
        // whoever it faces, so that the flock reads in front of its anchor.
        for viewer in [
            None,
            Some([2.5, 1.6, -1.0]),
            Some([-3.0, 1.7, -2.0]),
            Some([0.8, 1.6, -3.0]),
        ] {
            let towards = glam::Vec3::from_array(facing_from(
                station.position,
                viewer.unwrap_or(station.facing),
                station.facing,
            ));
            for place in [
                station.position,
                [0.0, 1.2 + 2.0 * SIGN_APART[0], -3.0],
                [1.1, 1.2, -2.8],
            ] {
                let off = leader_end(place, viewer) - middle;
                let behind = -off.dot(towards);
                let across = off - towards * off.dot(towards);
                assert!(
                    (behind - SIGN_BEHIND).abs() < 0.03 && across.length() < 0.03,
                    "{viewer:?} {place:?}: {off}"
                );
            }
        }
    }

    #[test]
    fn a_small_turn_or_shift_of_the_arrangement_leaves_every_sign_above_its_worker() {
        use crate::delegation::{Node, Scene, Workspace};
        use tomori_scene::anchors::Anchor;
        let workspace = Workspace {
            nodes: (0..16)
                .map(|i| Node {
                    id: format!("w-{i:02}"),
                    parent_id: Some("root".into()),
                    title: format!("Task {i}"),
                    state: State::Running,
                })
                .collect(),
            ..Default::default()
        };
        for arrangement in [
            Anchor {
                orientation: glam::Quat::from_rotation_y(1_f32.to_radians()),
                ..Anchor::AGENT
            },
            Anchor {
                position: tomori_scene::swarm::CENTER + glam::Vec3::new(0.05, 0.0, 0.0),
                ..Anchor::AGENT
            },
        ] {
            let mut scene = Scene::new();
            let mut placements = LabelPlacement::default();
            scene.update(Some("root"), &workspace, 0.0);
            layout(
                &workspace,
                scene.stations(),
                0,
                0,
                &[],
                &mut placements,
                None,
            );
            scene.arrange(arrangement, tomori_scene::anchors::EYES, None, None);
            scene.update(Some("root"), &workspace, 0.0);
            layout(
                &workspace,
                scene.stations(),
                0,
                0,
                &[],
                &mut placements,
                None,
            );
            for station in scene.stations() {
                assert_eq!(
                    placements.0[&station.id], station.position,
                    "{}",
                    station.id
                );
            }
        }
    }

    #[test]
    fn signs_follow_their_workers_when_the_arrangement_moves_and_back() {
        use crate::delegation::{Node, Scene, Workspace};
        use tomori_scene::anchors::Anchor;
        let workspace = Workspace {
            nodes: (0..3)
                .map(|i| Node {
                    id: format!("w-{i}"),
                    parent_id: Some("root".into()),
                    title: format!("Task {i}"),
                    state: State::Running,
                })
                .collect(),
            ..Default::default()
        };
        let mut scene = Scene::new();
        let mut placements = LabelPlacement::default();
        let signs = |scene: &Scene, placements: &mut LabelPlacement| {
            let (panels, _) = layout(&workspace, scene.stations(), 0, 0, &[], placements, None);
            panels
                .iter()
                .map(|panel| (panel.sign.clone().unwrap().source_id, panel.origin))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        scene.update(Some("root"), &workspace, 0.0);
        let home = signs(&scene, &mut placements);
        // The space moves to an environment whose workers anchor stands 6 m further off: every sign moves with its worker.
        let away = Anchor {
            position: tomori_scene::swarm::CENTER + glam::Vec3::new(0.0, 0.0, -6.0),
            ..Anchor::AGENT
        };
        scene.arrange(away, tomori_scene::anchors::EYES, None, None);
        // Its workers glide there, their signs with them.
        for _ in 0..400 {
            scene.update(Some("root"), &workspace, 1.0 / 60.0);
        }
        let moved = signs(&scene, &mut placements);
        for (id, origin) in &moved {
            let station = scene.stations().iter().find(|s| &s.id == id).unwrap();
            // Above its worker, drawn larger so far off, and within 30° of its bearing from home.
            assert!(
                origin[1]
                    >= station.position[1]
                        + SIGN_LIFT * sign_scale(station.position, station.facing)
                        - 1e-4
            );
            let bearing = |at: [f32; 3]| at[0].atan2(-at[2]);
            assert!(
                (bearing(*origin) - bearing(station.position)).abs() <= SIGN_SPREAD + 0.2,
                "{id}"
            );
            assert!((home[id][2] - origin[2]).abs() > 2.0, "{id}");
        }
        // And back, where they were.
        scene.arrange(Anchor::AGENT, tomori_scene::anchors::EYES, None, None);
        for _ in 0..400 {
            scene.update(Some("root"), &workspace, 1.0 / 60.0);
        }
        assert_eq!(signs(&scene, &mut placements), home);
    }

    #[test]
    fn collision_placement_is_bounded_stable_on_membership_edits_and_hostile_bearings() {
        for position in [
            [0.0, 1.7, 0.0],
            [0.0001, 1.6, -0.0001],
            [100.0, -100.0, 100.0],
        ] {
            let mut stations: Vec<_> = (0..128)
                .map(|i| Station {
                    facing: crate::delegation::HOME_EYES,
                    id: format!("w-{i:03}"),
                    title: "A title".into(),
                    state: State::Running,
                    opacity: 1.0,
                    position,
                    scale: 1.0,
                    more: 0,
                    size: None,
                    highlighted: false,
                })
                .collect();
            let mut placements = LabelPlacement::default();
            let (panels, _) = layout(
                &Workspace::default(),
                &stations,
                0,
                0,
                &[],
                &mut placements,
                None,
            );
            assert_eq!(panels.len(), 128);
            assert_eq!(placements.0.len(), 128);
            stations.remove(0);
            let mut new = stations[0].clone();
            new.id = "a-new-first-id".into();
            stations.push(new);
            let before = placements.0.clone();
            layout(
                &Workspace::default(),
                &stations,
                0,
                0,
                &[],
                &mut placements,
                None,
            );
            for s in &stations {
                if let Some(p) = before.get(&s.id) {
                    assert_eq!(placements.0[&s.id], *p);
                }
            }
            assert_eq!(placements.0.len(), 128);
        }
    }

    #[test]
    fn stations_are_bounded_and_do_not_claim_stale_completion() {
        let workspace = Workspace {
            nodes: (0..64)
                .map(|n| node(&format!("worker-{n:02}"), State::Completed))
                .collect(),
            ..Default::default()
        };
        let scene = scene(&workspace);
        let (panels, _) = test_layout(&workspace, scene.stations(), 0, 0);
        let labels: Vec<_> = panels.iter().filter(|p| p.sign.is_some()).collect();
        assert_eq!(labels.len(), 64);
        for (i, label) in labels.iter().enumerate() {
            assert_eq!(label.lines[1], "Finished");
            assert!(
                labels
                    .iter()
                    .skip(i + 1)
                    .all(|other| !overlaps(label, other))
            );
        }
        assert!(!panels.iter().any(|p| p.lines[0].contains("labels hidden")));

        // Partial failures do not erase independently observed statuses.
        let partial = Workspace {
            error: Some("partial".into()),
            truncated: true,
            ..workspace.clone()
        };
        assert!(
            test_layout(&partial, scene.stations(), 0, 0)
                .0
                .iter()
                .filter(|p| p.sign.is_some())
                .all(|p| p.lines[1] == "Finished")
        );
        for stale in [
            Workspace {
                error: Some("offline".into()),
                stale: true,
                ..workspace.clone()
            },
            Workspace::default(),
        ] {
            let (panels, _) = test_layout(&stale, scene.stations(), 0, 0);
            assert!(
                panels
                    .iter()
                    .filter(|p| p.sign.is_some())
                    .all(|p| p.lines[1] == "Unknown")
            );
            assert!(panels.iter().any(|p| p.sign.is_some()));
        }
        assert!(test_layout(&Workspace::default(), &[], 0, 0).0.is_empty());
    }

    #[test]
    fn overflow_notices_are_visible_and_bounded() {
        let workspace = Workspace {
            presentations: (0..MAX_RESULTS + 1).map(|i| card(&i.to_string())).collect(),
            error: Some("offline".into()),
            truncated: true,
            ..Default::default()
        };
        let (panels, pages) = test_layout(&workspace, &[], 0, 0);
        assert_eq!(pages, MAX_RESULTS.div_ceil(CARDS_PER_PAGE));
        let notices: Vec<_> = panels.iter().filter(|p| p.columns == 56).collect();
        assert_eq!(notices.len(), 1);
        assert!(!notices[0].lines.is_empty());
        assert!(notices[0].lines.iter().any(|s| s.contains("first 64")));
    }

    #[test]
    fn vertex_budget_fits_all_panels_even_for_worst_case_glyphs() {
        let maximum_characters = MAX_LABELS * (160 + 9) + CARDS_PER_PAGE * ROWS * COLUMNS + 3 * 56;
        // Per sign: 24 background strips, post, accent, up to 4 symbol strokes.
        let maximum_vertices =
            maximum_characters * 6 + MAX_LABELS * 30 * 6 + (CARDS_PER_PAGE + 1) * 2 * 6;
        assert!(maximum_vertices <= MAX_VERTICES);
    }

    #[test]
    fn opacity_blends_all_label_geometry_and_invalid_stations_are_hidden() {
        let workspace = Workspace {
            nodes: vec![node("a", State::Running)],
            ..Default::default()
        };
        let station = Station {
            facing: crate::delegation::HOME_EYES,
            id: "a".into(),
            title: "Task".into(),
            state: State::Running,
            position: [1.0, 1.0, -2.0],
            opacity: 1.0,
            scale: 1.0,
            more: 0,
            size: None,
            highlighted: false,
        };
        let full = test_layout(&workspace, std::slice::from_ref(&station), 0, 0)
            .0
            .remove(0);
        let half = test_layout(
            &workspace,
            &[Station {
                facing: crate::delegation::HOME_EYES,
                opacity: 0.5,
                ..station.clone()
            }],
            0,
            0,
        )
        .0
        .remove(0);
        let mut a = Vec::new();
        let mut b = Vec::new();
        panel_mesh(&full, &mut a);
        panel_mesh(&half, &mut b);
        assert_eq!(a.len(), b.len());
        for (a, b) in a.iter().zip(&b) {
            assert_eq!(a.position, b.position);
            assert_eq!(&a.color[..3], &b.color[..3]);
            assert_eq!(a.color[3] * 0.5, b.color[3]);
        }
        for opacity in [0.0, f32::NAN, -1.0, 2.0] {
            assert!(
                test_layout(
                    &workspace,
                    &[Station {
                        facing: crate::delegation::HOME_EYES,
                        opacity,
                        ..station.clone()
                    }],
                    0,
                    0
                )
                .0
                .iter()
                .all(|p| p.columns != 28)
            );
        }
    }

    #[test]
    fn a_timer_s_sign_stands_above_its_dial_naming_it_its_schedule_condition_and_countdown() {
        let dial = Dial {
            name: "check the build".into(),
            schedule: "every 15 minutes".into(),
            condition: "when present or on arrival".into(),
            pill: "Paused 12:05".into(),
            state: TimerState::Paused,
            position: [0.0, 1.95, -2.3],
            facing: crate::delegation::HOME_EYES,
            scale: 1.0,
            opacity: 1.0,
            pointed: false,
        };
        let panel = dial_panel(&dial, None);
        // Its name, its schedule, its condition wrapped, then its pill, at most as wide as a timer's sign.
        assert_eq!(
            panel.lines,
            [
                "check the build",
                "every 15 minutes",
                "when present or on",
                "arrival",
                "Paused 12:05"
            ]
        );
        assert!(panel.columns <= DIAL_COLUMNS);
        assert_eq!(panel.accent, tomori_scene::dials::color(TimerState::Paused));
        let sign = panel.sign.as_ref().unwrap();
        assert_eq!(sign.timer, Some((TimerState::Paused, 1)));
        assert_eq!(sign.station, dial.position);
        assert!(sign.own);
        // Above its dial's face, centred over it, facing home.
        let bottom = panel.origin[1] - (panel.lines.len() * 11 + 4) as f32 * panel.pixel;
        assert!(bottom > dial.position[1] + tomori_scene::dials::RADIUS);
        assert!((panel.center().x - dial.position[0]).abs() < 1e-4);
        assert!(sign.normal[2] > 0.99);
        // Facing the viewer when there is one.
        let turned = dial_panel(&dial, Some([2.0, 1.6, -2.3]));
        assert!(turned.sign.unwrap().normal[0] > 0.99);
        // Its schedule and condition muted, its name not; its pill with its state's symbol.
        let mut mesh = Vec::new();
        let surface = sign_mesh(&panel, sign, &mut mesh);
        assert!(surface > 0);
        let muted = |vertex: &&Vertex| vertex.color[..3] == MUTED;
        assert!(mesh.iter().any(|vertex| muted(&vertex)));
        assert!(mesh.iter().any(|vertex| vertex.color[..3] == TITLE));
        // Dials that are not valid, or fully faded, have no sign; the rest one each.
        let faded = Dial {
            opacity: 0.0,
            ..dial.clone()
        };
        let invalid = Dial {
            name: String::new(),
            ..dial.clone()
        };
        assert_eq!(dial_panels(&[dial.clone(), faded, invalid], None).len(), 1);
        assert!(dial_panels(&vec![dial.clone(); 40], None).len() <= tomori_scene::dials::MOST * 2);
        // No larger than the room the dials are placed for, at any size, with its most rows.
        for scale in [0.5, 1.0, 2.0] {
            let tallest = dial_panel(
                &Dial {
                    name: "the timer whose name runs on well past two rows".into(),
                    condition: "when present or on arrival, or later".into(),
                    scale,
                    ..dial.clone()
                },
                None,
            );
            assert_eq!(tallest.lines.len(), 6);
            let width = (tallest.columns * 9) as f32 * tallest.pixel;
            let height = (tallest.lines.len() * 11 + 4) as f32 * tallest.pixel;
            assert!(
                width <= tomori_scene::dials::SIGN_WIDTH * scale + 1e-4,
                "{width}"
            );
            assert!(
                height <= tomori_scene::dials::SIGN_HEIGHT * scale + 1e-4,
                "{height}"
            );
        }
        // A name too long for two rows ends with an ellipsis.
        let long = dial_panel(
            &Dial {
                name: "the timer whose name runs on well past two rows".into(),
                ..dial.clone()
            },
            None,
        );
        assert!(long.lines[1].ends_with('…'));
        // A name that takes no rows still has its row.
        let blank = dial_panel(
            &Dial {
                name: " ".into(),
                ..dial
            },
            None,
        );
        assert_eq!(blank.lines.len(), 5);
    }

    #[test]
    fn each_timer_state_has_a_symbol_of_its_own() {
        let meshes: Vec<Vec<[f32; 3]>> = [
            TimerState::Waiting,
            TimerState::Paused,
            TimerState::Awaiting,
            TimerState::Fired,
            TimerState::Unknown,
        ]
        .into_iter()
        .map(|state| {
            let mut mesh = Vec::new();
            timer_symbol(&mut mesh, state, [6.0, 5.5], 3.0, 0.004, [1.0; 4]);
            assert!(!mesh.is_empty());
            mesh.iter().map(|vertex| vertex.position).collect()
        })
        .collect();
        for (index, mesh) in meshes.iter().enumerate() {
            assert!(!meshes[..index].contains(mesh));
        }
    }

    #[test]
    fn every_content_page_is_navigable_and_matches_the_panel() {
        let mut table = card("table");
        table.kind = Kind::Table;
        table.columns = (0..6).map(|i| format!("C{i}")).collect();
        table.rows = (0..16)
            .map(|r| {
                (0..6)
                    .map(|c| format!("{r:02}{c}{}", "z".repeat(150)))
                    .collect()
            })
            .collect();
        let expected = content_pages(&table);
        let workspace = Workspace {
            presentations: vec![table],
            ..Default::default()
        };
        let mut nav = CardNavigation::default();
        nav.update(Some("root"), &workspace, 0.0, 0.0);
        for (i, page) in expected.iter().enumerate() {
            assert_eq!(nav.content, i);
            let panel = &test_layout(&workspace, &[], nav.card, nav.content).0[0];
            assert_eq!(&panel.lines[4..4 + page.len()], page);
            assert!(panel.lines[2].contains(&format!("CONTENT {}/{}", i + 1, expected.len())));
            assert_eq!(panel.lines.len(), ROWS);
            nav.update(Some("root"), &workspace, 0.0, -1.0);
            nav.update(Some("root"), &workspace, 0.0, 0.0);
        }
        assert_eq!(nav.content, 0);
    }

    #[test]
    fn arriving_and_departing_labels_follow_station_opacity_and_unknown_membership() {
        let workspace = Workspace {
            nodes: vec![node("a", State::Completed)],
            ..Default::default()
        };
        let mut scene = Scene::new();
        scene.update(Some("root"), &Workspace::default(), 0.05);
        scene.update(Some("root"), &workspace, 0.05);
        let arriving = &scene.stations()[0];
        let labels = test_layout(&workspace, scene.stations(), 0, 0).0;
        let panel = labels.iter().find(|p| p.sign.is_some()).unwrap();
        assert!(arriving.opacity > 0.0 && arriving.opacity < 1.0);
        assert_eq!(panel.opacity, arriving.opacity);
        assert_eq!(panel.lines[1], "Finished");
        for _ in 0..40 {
            scene.update(Some("root"), &workspace, 0.05);
        }
        scene.update(Some("root"), &Workspace::default(), 0.05);
        let departing = &scene.stations()[0];
        let labels = test_layout(&Workspace::default(), scene.stations(), 0, 0).0;
        let panel = labels.iter().find(|p| p.sign.is_some()).unwrap();
        assert!(departing.opacity < 1.0);
        assert_eq!(panel.opacity, departing.opacity);
        assert_eq!(panel.lines[1], "Unknown");
    }

    fn test_sign(position: [f32; 3], state: State) -> Panel {
        let station = Station {
            facing: crate::delegation::HOME_EYES,
            id: "a".into(),
            title: "Readable task".into(),
            position,
            state,
            opacity: 1.0,
            scale: 1.0,
            more: 0,
            size: None,
            highlighted: false,
        };
        station_panel(&station, position, state, None)
    }

    #[test]
    fn symbols_are_distinct_geometry_and_follow_observed_state() {
        let states = [
            State::Completed,
            State::Error,
            State::Waiting,
            State::Running,
            State::Idle,
            State::Stopped,
            State::Stalled,
            State::Unanswered,
            State::Unknown,
        ];
        let mut shapes = Vec::new();
        for state in states {
            let panel = test_sign([0.0, 1.4, -2.7], state);
            assert_eq!(panel.lines, ["Readable task", state_label(&state).0]);
            assert_eq!(panel.sign.as_ref().unwrap().state, state);
            let mut symbol = Vec::new();
            status_symbol(&mut symbol, state, [6.0, 5.5], 3.0, panel.pixel, [1.0; 4]);
            assert!(!symbol.is_empty());
            let positions: Vec<_> = symbol.iter().map(|v| v.position).collect();
            assert!(
                !shapes.contains(&positions),
                "duplicate shape for {state:?}"
            );
            shapes.push(positions);
            let mut mesh = Vec::new();
            let surface = panel_mesh(&panel, &mut mesh);
            // Its plate is its surface, and its title's text is drawn beyond its plate, leader, pill and symbol.
            assert!(surface > 0);
            assert!(mesh.len() > surface + symbol.len() + "Readabletask".len() * 6);
        }
        let workspace = Workspace {
            nodes: vec![node("a", State::Error)],
            ..Default::default()
        };
        let old_snapshot = Station {
            facing: crate::delegation::HOME_EYES,
            id: "a".into(),
            title: "old".into(),
            state: State::Completed,
            position: [0.0, 1.4, -3.0],
            opacity: 1.0,
            scale: 1.0,
            more: 0,
            size: None,
            highlighted: false,
        };
        let panel = test_layout(&workspace, &[old_snapshot], 0, 0).0.remove(0);
        assert_eq!(panel.lines[1], "Error");
        assert_eq!(panel.sign.unwrap().state, State::Error);
    }

    #[test]
    fn signs_face_home_with_readable_handedness_finite_fallback_and_bow() {
        for position in [
            [-2.0, 1.4, -2.0],
            [2.0, 1.4, -2.0],
            [0.0, 1.4, 3.0],
            [0.0, 1.4, 0.0],
            [1.0e-30, 1.4, -1.0e-30],
            [100.0, -100.0, 100.0],
        ] {
            let panel = test_sign(position, State::Completed);
            let sign = panel.sign.as_ref().unwrap();
            let [rx, _, rz] = sign.right;
            let [nx, _, nz] = sign.normal;
            assert!((rx * nz - rz * nx - 1.0).abs() < 1e-5);
            assert!((rx * nx + rz * nz).abs() < 1e-5);
            if position[0].hypot(position[2]) > 0.0001 {
                assert!(nx * -position[0] + nz * -position[2] > 0.0);
            } else {
                assert_eq!(sign.normal, [0.0, 0.0, 1.0]);
            }
            let mut mesh = Vec::new();
            panel_mesh(&panel, &mut mesh);
            assert!(mesh.iter().all(|v| {
                v.position
                    .iter()
                    .chain(v.color.iter())
                    .all(|n| n.is_finite())
            }));
            // Its leader reaches down to its station's middle, ending in a dot there.
            let lowest = mesh
                .iter()
                .map(|v| v.position[1])
                .fold(f32::INFINITY, f32::min);
            let dot = (LEADER_DOT + 4.0) * panel.pixel;
            assert!(
                lowest <= position[1] && lowest >= position[1] - dot,
                "{lowest} {position:?}"
            );
            // Its plate's edges bow towards the observer, its middle not.
            let mut surface = Vec::new();
            let plate = panel_mesh(&panel, &mut surface);
            let local = |v: &Vertex| {
                let d = glam::Vec3::from_array(v.position) - glam::Vec3::from_array(panel.origin);
                (
                    d.dot(glam::Vec3::from_array(sign.right)),
                    d.dot(glam::Vec3::from_array(sign.normal)),
                )
            };
            let points: Vec<(f32, f32)> = surface[..plate].iter().map(local).collect();
            let mid = (panel.columns * 9) as f32 * panel.pixel * 0.5;
            let edge = points
                .iter()
                .filter(|(x, _)| (x - mid).abs() > mid * 0.95)
                .map(|(_, z)| *z)
                .fold(f32::NEG_INFINITY, f32::max);
            let middle = points
                .iter()
                .filter(|(x, _)| (x - mid).abs() < mid * 0.1)
                .map(|(_, z)| *z)
                .fold(f32::INFINITY, f32::min);
            // As far as a sign as wide as the widest bows at as far from its middle.
            let reach = (SIGN_COLUMNS * 9) as f32 * panel.pixel * 0.5;
            let bowed = SIGN_BOW * (mid * 0.95 / reach).powi(2);
            assert!(edge > middle + bowed, "{edge} {middle} {bowed}");
        }
        for bad in [f32::NAN, f32::INFINITY, -f32::INFINITY, 101.0] {
            let workspace = Workspace {
                nodes: vec![node("a", State::Running)],
                ..Default::default()
            };
            let station = Station {
                facing: crate::delegation::HOME_EYES,
                id: "a".into(),
                title: "bad".into(),
                state: State::Running,
                position: [bad, 1.4, -2.0],
                opacity: 1.0,
                scale: 1.0,
                more: 0,
                size: None,
                highlighted: false,
            };
            assert!(
                test_layout(&workspace, &[station], 0, 0)
                    .0
                    .iter()
                    .all(|p| p.sign.is_none())
            );
        }
    }

    #[test]
    fn curved_sign_foreground_stays_in_front_of_tessellated_background() {
        let panel = test_sign([1.0, 1.4, -2.7], State::Completed);
        let sign = panel.sign.as_ref().unwrap();
        let mut mesh = Vec::new();
        panel_mesh(&panel, &mut mesh);
        let local = |v: &Vertex| {
            let d = glam::Vec3::from_array(v.position) - glam::Vec3::from_array(panel.origin);
            (
                d.dot(glam::Vec3::from_array(sign.right)),
                d.dot(glam::Vec3::from_array(sign.normal)),
            )
        };
        let mut surface = Vec::new();
        let plate = panel_mesh(&panel, &mut surface);
        let background = &mesh[..plate];
        let mid = (panel.columns * 9) as f32 * panel.pixel * 0.5;
        let reach = (SIGN_COLUMNS * 9) as f32 * panel.pixel * 0.5;
        // Each vertex's depth in front of the sign's plane, before it is bowed.
        let depth = |v: &Vertex| {
            let (x, z) = local(v);
            z - SIGN_BOW * ((x - mid) / reach).powi(2)
        };
        // Check the foreground edges and their midpoints against the straight strip surfaces, not just against the
        // ideal quadratic bow: the text and symbol, 4 mm in front of the plate, and the pill, 2 mm.
        for (quad, clear) in mesh[plate..].as_chunks::<6>().0.iter().filter_map(|quad| {
            let z = depth(&quad[0]);
            if z > SIGN_PILL + 1e-4 {
                Some((quad, 0.0037))
            } else if z > SIGN_PLATE + 1e-4 {
                Some((quad, 0.0017))
            } else {
                None
            }
        }) {
            for (a, b) in [(0, 0), (1, 1), (2, 2), (0, 1), (1, 2)] {
                let (xa, za) = local(&quad[a]);
                let (xb, zb) = local(&quad[b]);
                let (x, z) = ((xa + xb) * 0.5, (za + zb) * 0.5);
                for strip in background.as_chunks::<6>().0 {
                    let (left, zl) = local(&strip[0]);
                    let (right, zr) = local(&strip[1]);
                    if x >= left && x <= right && right > left {
                        let behind = zl + (zr - zl) * (x - left) / (right - left);
                        assert!(
                            z - behind > clear,
                            "foreground is too close to the background"
                        );
                        break;
                    }
                }
            }
        }
    }

    #[test]
    fn surfaces_that_only_touch_along_an_edge_have_no_order() {
        // Two triangles of a square, at different depths, sharing its diagonal: their bounds overlap, but what they
        // share is a line, of no area, so neither is in front.
        let triangle = |points: [[f32; 3]; 3]| {
            let points = points.map(glam::Vec3::from_array);
            ProjectedTriangle {
                points,
                low: points
                    .iter()
                    .fold(glam::Vec2::splat(f32::INFINITY), |low, p| {
                        low.min(p.truncate())
                    }),
                high: points
                    .iter()
                    .fold(glam::Vec2::splat(f32::NEG_INFINITY), |high, p| {
                        high.max(p.truncate())
                    }),
            }
        };
        let near = triangle([[0.0, 0.0, 0.2], [1.0, 0.0, 0.2], [0.0, 1.0, 0.2]]);
        let far = triangle([[1.0, 0.0, 0.8], [1.0, 1.0, 0.8], [0.0, 1.0, 0.8]]);
        assert_eq!(overlap_order(&[near], &[far]), None);
    }

    #[test]
    fn angular_overlap_respects_bearings_wrap_distance_and_height() {
        let a = test_sign([1.0, 1.18, -1.0], State::Running);
        let separate = test_sign([1.0, 1.18, -4.0], State::Running);
        assert!(!overlaps(&a, &separate));
        let same_bearing = test_sign([2.0, 1.18, -2.0], State::Running);
        assert!(overlaps(&a, &same_bearing));
        let high = test_sign([1.0, 3.0, -1.0], State::Running);
        assert!(!overlaps(&a, &high));
        let wrap_left = test_sign([-0.01, 1.18, 3.0], State::Running);
        let wrap_right = test_sign([0.01, 1.18, 3.0], State::Running);
        assert!(overlaps(&wrap_left, &wrap_right));
        let at_home = test_sign([0.0, 1.18, 0.0], State::Running);
        assert!(overlaps(&at_home, &a));
    }

    #[test]
    fn fading_sign_does_not_retessellate_a_static_card_or_sign() {
        let sign = test_sign([0.0, 1.4, -2.7], State::Waiting);
        let workspace = Workspace {
            presentations: vec![card("a")],
            ..Default::default()
        };
        let card = test_layout(&workspace, &[], 0, 0).0.remove(0);
        let mut cache = PageCache::default();
        assert!(cache.update(vec![sign.clone(), card.clone()]));
        let original = cache.vertices.clone();
        for opacity in [0.1, 0.4, 0.9, 1.0] {
            let mut faded = sign.clone();
            faded.opacity = opacity;
            cache.update(vec![faded, card.clone()]);
            assert_eq!(cache.builds, 2);
            let sign_len = cache.ranges[0].vertices.end as usize;
            assert!(
                cache.vertices[..sign_len]
                    .iter()
                    .zip(&original)
                    .all(|(v, before)| v.color[3] == before.color[3] * opacity)
            );
            assert_eq!(
                bytemuck::cast_slice::<Vertex, u8>(&cache.vertices[sign_len..]),
                bytemuck::cast_slice::<Vertex, u8>(&original[sign_len..])
            );
        }
        // Turned to face a viewer walking about it, or gliding after its worker, it is placed again, not built again.
        let station = Station {
            facing: crate::delegation::HOME_EYES,
            id: "a".into(),
            title: "Readable task".into(),
            position: [0.0, 1.4, -2.7],
            state: State::Waiting,
            opacity: 1.0,
            scale: 1.0,
            more: 0,
            size: None,
            highlighted: false,
        };
        for (at, viewer) in [
            ([0.0, 1.4, -2.7], [2.0, 1.6, -1.0]),
            ([0.0, 1.4, -2.7], [-2.5, 1.6, -0.5]),
            ([0.4, 1.5, -2.9], [-2.5, 1.6, -0.5]),
        ] {
            let moved = Station {
                position: at,
                ..station.clone()
            };
            let turned = station_panel(&moved, at, State::Waiting, Some(viewer));
            assert!(cache.update(vec![turned, card.clone()]));
            assert_eq!(cache.builds, 2);
            assert_ne!(cache.vertices[0].position, original[0].position);
        }
    }

    #[test]
    fn overlap_order_uses_visible_surfaces_not_centers_and_changes_with_view() {
        let projection = glam::camera::rh::proj::directx::perspective(0.95, 1.0, 0.05, 50.0);
        let sign = test_sign([0.0, 1.25, -2.8], State::Completed);
        let mut rear = Panel {
            origin: [-0.2, 2.0, -2.92],
            pixel: 0.0051,
            columns: COLUMNS,
            lines: vec![String::new(); ROWS],
            accent: MUTED,
            opacity: 1.0,
            sign: None,
            rule: None,
        };
        for yaw in [-0.3_f32, 0.3] {
            rear.origin[0] = if yaw < 0.0 { -0.2 } else { -1.84 };
            let mut cache = PageCache::default();
            cache.update(vec![sign.clone(), rear.clone()]);
            for x in [-0.032, 0.032] {
                let eye = glam::Vec3::new(x, 1.6, 0.0);
                let direction = glam::Vec3::new(yaw.sin(), 0.0, -yaw.cos());
                let matrix = projection
                    * glam::camera::rh::view::look_at_mat4(eye, eye + direction, glam::Vec3::Y);
                let depth = |p: &Panel| {
                    let clip = matrix * p.center().extend(1.0);
                    clip.z / clip.w
                };
                assert!(
                    depth(&rear) < depth(&sign),
                    "the rear field is nearer than the sign at yaw={yaw}, eye={x}"
                );
                assert_eq!(panel_order(&cache.ranges, matrix), [1, 0]);
                let recenter = glam::Mat4::from_rotation_y(-0.8)
                    * glam::Mat4::from_translation(glam::Vec3::new(-0.4, 0.0, 0.3));
                assert_eq!(
                    panel_order(&cache.ranges, (matrix * recenter.inverse()) * recenter),
                    [1, 0]
                );
            }
        }
        // Put both fields squarely in view from opposite sides. The actual eye
        // position, not a stored home-camera distance, decides their order.
        rear.origin = [-1.0, 2.0, -3.5];
        let front = Panel {
            origin: [-1.0, 2.0, -2.5],
            ..rear.clone()
        };
        let mut cache = PageCache::default();
        cache.update(vec![front, rear]);
        for (z, expected) in [(0.0, [1, 0]), (-6.0, [0, 1])] {
            let matrix = projection
                * glam::camera::rh::view::look_at_mat4(
                    glam::Vec3::new(0.0, 1.6, z),
                    glam::Vec3::new(0.0, 1.6, -3.0),
                    glam::Vec3::Y,
                );
            assert_eq!(panel_order(&cache.ranges, matrix), expected);
        }
    }

    #[test]
    fn surface_projection_clips_near_plane_and_marks_true_intersections_ambiguous() {
        let matrix = glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.05, 50.0);
        let surface = [[
            glam::Vec3::new(-0.1, -0.1, -0.5),
            glam::Vec3::new(0.1, -0.1, -0.5),
            glam::Vec3::new(0.0, 0.1, 0.1),
        ]];
        let projected = project_surface(&surface, matrix);
        assert!(!projected.is_empty());
        assert!(
            projected
                .iter()
                .flat_map(|t| t.points)
                .all(|p| p.is_finite() && p.z >= -1e-5 && p.z <= 1.0)
        );
        let a = project_surface(
            &[[
                glam::Vec3::new(-0.5, -0.5, 0.3),
                glam::Vec3::new(0.5, -0.5, 0.7),
                glam::Vec3::new(0.0, 0.5, 0.5),
            ]],
            glam::Mat4::IDENTITY,
        );
        let b = project_surface(
            &[[
                glam::Vec3::new(-0.5, -0.5, 0.5),
                glam::Vec3::new(0.5, -0.5, 0.5),
                glam::Vec3::new(0.0, 0.5, 0.5),
            ]],
            glam::Mat4::IDENTITY,
        );
        assert_eq!(overlap_order(&a, &b), None);
        assert!(
            project_surface(
                &surface.map(|t| t.map(|p| p + glam::Vec3::new(0.0, 0.0, 2.0))),
                matrix
            )
            .is_empty()
        );
    }

    #[test]
    fn stereo_recenter_keeps_world_signs() {
        let station = Station {
            id: "a".into(),
            position: [-1.0, 1.4, -2.8],
            state: State::Idle,
            title: "Old task".into(),
            opacity: 1.0,
            facing: tomori_scene::delegation::HOME_EYES,
            scale: 1.0,
            more: 0,
            size: None,
            highlighted: false,
        };
        let workspace = Workspace {
            nodes: vec![node("a", State::Idle)],
            ..Default::default()
        };
        let panel = test_layout(&workspace, &[station], 0, 0).0.remove(0);
        let mut mesh = Vec::new();
        panel_mesh(&panel, &mut mesh);
        let projection = glam::camera::rh::proj::directx::perspective(1.0, 1.0, 0.05, 50.0);
        let target = glam::Vec3::new(-1.0, 1.75, -2.8);
        let recenter = glam::Mat4::from_rotation_y(0.7)
            * glam::Mat4::from_translation(glam::Vec3::new(0.5, 0.0, -0.3));
        let mut projected = Vec::new();
        for eye_x in [-0.032, 0.032] {
            let eye = glam::Vec3::new(eye_x, 1.6, 0.0);
            let view =
                projection * glam::camera::rh::view::look_at_mat4(eye, target, glam::Vec3::Y);
            let v = glam::Vec3::from_array(mesh[0].position).extend(1.0);
            let normal = view * v;
            let compensated = (view * recenter.inverse()) * recenter * v;
            assert!(normal.abs_diff_eq(compensated, 1e-5));
            let ndc = normal.truncate() / normal.w;
            assert!(ndc.is_finite() && ndc.x.abs() < 1.0 && ndc.y.abs() < 1.0);
            projected.push(ndc);
        }
        assert_ne!(projected[0], projected[1]);
    }

    /// The pixels of two RGBA images that differ by more than one step in any channel.
    ///
    /// Recentering draws with `(matrix * transform.inverse()) * transform`, which differs from `matrix` only by float rounding, about 1e-7. Lavapipe carries that into interpolated colours that round one step differently, in up to about 300 pixels of a 512×512 image, while NVIDIA and RADV draw identical images; a moved edge, a changed order or a wrong transform differs by far more than one step.
    fn changed_pixels(a: &[u8], b: &[u8]) -> usize {
        assert_eq!(a.len(), b.len());
        a.as_chunks::<4>()
            .0
            .iter()
            .zip(b.as_chunks::<4>().0)
            .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 1))
            .count()
    }

    #[test]
    fn vulkan_sign_images_include_post_text_and_smooth_fade_in_both_eyes() {
        test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
            pollster::block_on(async move {
                let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let camera_layout =
                    crate::graphics::camera_layout(&device, crate::multiview::Views::One);
                let camera = crate::graphics::camera_buffer(&device);
                let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &camera_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: camera.as_entire_binding(),
                    }],
                });
                let extent = wgpu::Extent3d {
                    width: 512,
                    height: 512,
                    depth_or_array_layers: 1,
                };
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let depth = device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Depth32Float,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let view = texture.create_view(&Default::default());
                let depth_view = depth.create_view(&Default::default());
                let glyphs = crate::glyphs::Glyphs::new(&device);
                let mut graphics = WorkspaceGraphics::new(
                    &device,
                    wgpu::TextureFormat::Rgba8Unorm,
                    &camera_layout,
                    &glyphs,
                    1,
                );
                // A depth-writing opaque body in front of part of the sign.
                let shader = device.create_shader_module(hazed_wgsl!("workspace.wgsl"));
                let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("synthetic body layout"),
                    bind_group_layouts: &[Some(&camera_layout), Some(glyphs.layout())],
                    immediate_size: 0,
                });
                let body_pipeline =
                    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: Some("synthetic opaque body"),
                        layout: Some(&layout),
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
                                format: wgpu::TextureFormat::Rgba8Unorm,
                                blend: None,
                                write_mask: wgpu::ColorWrites::ALL,
                            })],
                        }),
                        primitive: wgpu::PrimitiveState {
                            cull_mode: None,
                            ..Default::default()
                        },
                        depth_stencil: Some(wgpu::DepthStencilState {
                            format: wgpu::TextureFormat::Depth32Float,
                            depth_write_enabled: Some(true),
                            depth_compare: Some(wgpu::CompareFunction::Less),
                            stencil: Default::default(),
                            bias: Default::default(),
                        }),
                        multisample: Default::default(),
                        multiview_mask: None,
                        cache: None,
                    });
                let mut body_mesh = Vec::new();
                quad(
                    &mut body_mesh,
                    -0.14,
                    1.8,
                    -1.0,
                    0.14,
                    0.4,
                    [0.05, 0.1, 0.8, 1.0],
                );
                use wgpu::util::DeviceExt;
                let body_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("synthetic body"),
                    contents: bytemuck::cast_slice(&body_mesh),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let workspace = Workspace {
                    nodes: vec![node("a", State::Completed)],
                    ..Default::default()
                };
                let mut station = Station {
                    facing: crate::delegation::HOME_EYES,
                    id: "a".into(),
                    title: "Readable task".into(),
                    state: State::Completed,
                    position: [0.0, 1.25, -2.8],
                    opacity: 1.0,
                    scale: 1.0,
                    more: 0,
                    size: None,
                    highlighted: false,
                };
                // Rendering fixture deliberately exercises a centered arbitrary
                // plane. Real placement is tested separately with 128 labels below.
                graphics
                    .placements
                    .0
                    .insert(station.id.clone(), station.position);
                let capture_order =
                    |graphics: &WorkspaceGraphics, matrix: glam::Mat4, body: bool, sorted: bool| {
                        queue.write_buffer(
                            &camera,
                            0,
                            bytemuck::cast_slice(&crate::graphics::camera(
                                matrix,
                                tomori_assets::haze::Haze::NONE,
                                1,
                            )),
                        );
                        glyphs.sync(&device, &queue);
                        let glyph_binding = glyphs.binding();
                        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                            label: None,
                            size: 512 * 512 * 4,
                            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                            mapped_at_creation: false,
                        });
                        let mut encoder = device.create_command_encoder(&Default::default());
                        {
                            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: None,
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &view,
                                    resolve_target: None,
                                    depth_slice: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color {
                                            r: 0.08,
                                            g: 0.12,
                                            b: 0.16,
                                            a: 1.0,
                                        }),
                                        store: wgpu::StoreOp::Store,
                                    },
                                })],
                                depth_stencil_attachment: Some(
                                    wgpu::RenderPassDepthStencilAttachment {
                                        view: &depth_view,
                                        depth_ops: Some(wgpu::Operations {
                                            load: wgpu::LoadOp::Clear(1.0),
                                            store: wgpu::StoreOp::Discard,
                                        }),
                                        stencil_ops: None,
                                    },
                                ),
                                timestamp_writes: None,
                                occlusion_query_set: None,
                                multiview_mask: None,
                            });
                            if body {
                                pass.set_pipeline(&body_pipeline);
                                pass.set_bind_group(0, &binding, &[]);
                                pass.set_bind_group(1, &glyph_binding, &[]);
                                pass.set_vertex_buffer(0, body_buffer.slice(..));
                                pass.draw(0..6, 0..1);
                            }
                            if sorted {
                                graphics.draw(&mut pass, &binding, &glyph_binding, matrix);
                            } else {
                                // Reproduce the old single blended draw in the test only.
                                pass.set_pipeline(&graphics.pipeline);
                                pass.set_bind_group(0, &binding, &[]);
                                pass.set_bind_group(1, &glyph_binding, &[]);
                                pass.set_vertex_buffer(0, graphics.vertices.slice(..));
                                pass.draw(0..graphics.count, 0..1);
                            }
                        }
                        encoder.copy_texture_to_buffer(
                            texture.as_image_copy(),
                            wgpu::TexelCopyBufferInfo {
                                buffer: &buffer,
                                layout: wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    bytes_per_row: Some(512 * 4),
                                    rows_per_image: Some(512),
                                },
                            },
                            extent,
                        );
                        queue.submit([encoder.finish()]);
                        let (tx, rx) = std::sync::mpsc::channel();
                        buffer
                            .slice(..)
                            .map_async(wgpu::MapMode::Read, move |result| {
                                tx.send(result).unwrap();
                            });
                        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                        rx.recv().unwrap().unwrap();
                        let result = buffer.slice(..).get_mapped_range().unwrap().to_vec();
                        buffer.unmap();
                        result
                    };
                let capture = |graphics: &WorkspaceGraphics, matrix, body| {
                    capture_order(graphics, matrix, body, true)
                };
                let projection =
                    glam::camera::rh::proj::directx::perspective(0.65, 1.0, 0.05, 50.0);
                let mut eye_images = Vec::new();
                for eye_x in [-0.032, 0.032] {
                    let eye = glam::Vec3::new(eye_x, 1.6, 0.0);
                    let matrix = projection
                        * glam::camera::rh::view::look_at_mat4(
                            eye,
                            eye + glam::Vec3::NEG_Z,
                            glam::Vec3::Y,
                        );
                    graphics.hide();
                    let empty = capture(&graphics, matrix, false);
                    station.opacity = 1.0;
                    graphics.update(
                        &device,
                        &queue,
                        &workspace,
                        std::slice::from_ref(&station),
                        None,
                    );
                    let full = capture(&graphics, matrix, false);
                    // A farther result field fills the label's projected area. In
                    // particular, it must not paint over the foreground glyphs.
                    let sign = station_panel(&station, station.position, State::Completed, None);
                    let rear = Panel {
                        origin: [-2.0, 2.5, -4.0],
                        pixel: 0.01,
                        columns: COLUMNS,
                        lines: vec![String::new(); ROWS],
                        accent: MUTED,
                        opacity: 1.0,
                        sign: None,
                        rule: None,
                    };
                    graphics.cache.update(vec![sign, rear]);
                    queue.write_buffer(
                        &graphics.vertices,
                        0,
                        bytemuck::cast_slice(&graphics.cache.vertices),
                    );
                    graphics.count = graphics.cache.vertices.len() as u32;
                    let overlap = capture(&graphics, matrix, false);
                    let visible_glyphs = (225..250)
                        .flat_map(|y| (135..240).map(move |x| (y * 512 + x) * 4))
                        .filter(|&i| {
                            full[i] > 50
                                && full[i + 1] > 180
                                && full[i + 2] > 120
                                && full[i..i + 3] == overlap[i..i + 3]
                        })
                        .count();
                    assert!(
                        visible_glyphs > 10,
                        "rear card hid foreground glyphs: {visible_glyphs}"
                    );
                    station.opacity = 0.2;
                    graphics.update(
                        &device,
                        &queue,
                        &workspace,
                        std::slice::from_ref(&station),
                        None,
                    );
                    let faded = capture(&graphics, matrix, false);
                    let difference =
                        |image: &[u8], x0: usize, x1: usize, y0: usize, y1: usize| -> u64 {
                            (y0..y1)
                                .flat_map(|y| (x0..x1).map(move |x| (y * 512 + x) * 4))
                                .map(|i| {
                                    (0..3)
                                        .map(|c| image[i + c].abs_diff(empty[i + c]) as u64)
                                        .sum::<u64>()
                                })
                                .sum()
                        };
                    let post = difference(&full, 240, 272, 280, 340);
                    let faded_post = difference(&faded, 240, 272, 280, 340);
                    assert!(
                        post > 0 && faded_post > 0 && faded_post < post,
                        "post must render and fade"
                    );
                    let text = difference(&full, 110, 400, 225, 270);
                    let faded_text = difference(&faded, 110, 400, 225, 270);
                    assert!(
                        text > 0 && faded_text > 0 && faded_text < text,
                        "sign and glyphs must render and fade"
                    );
                    let title_pixels = (225..250)
                        .flat_map(|y| (135..240).map(move |x| (y * 512 + x) * 4))
                        .filter(|&i| full[i] > 50 && full[i + 1] > 180 && full[i + 2] > 120)
                        .count();
                    assert!(title_pixels > 0, "title glyph pixels must be visible");
                    let transform = glam::Mat4::from_rotation_y(0.3)
                        * glam::Mat4::from_translation(glam::Vec3::new(0.4, 0.0, -0.2));
                    station.opacity = 1.0;
                    graphics.update(
                        &device,
                        &queue,
                        &workspace,
                        std::slice::from_ref(&station),
                        None,
                    );
                    let compensated =
                        capture(&graphics, (matrix * transform.inverse()) * transform, false);
                    let changed = changed_pixels(&full, &compensated);
                    assert!(changed < 256, "recenter changed the sign: {changed} pixels");
                    eye_images.push(full);
                }
                assert_ne!(eye_images[0], eye_images[1]);

                // Exercise the real panel meshes and depth attachment at multiple
                // distances and bearings, in both insertion orders and both eyes.
                // The rear field contains no glyphs, making foreground glyph pixels
                // unambiguous. Images use synthetic content only.
                let upload = |graphics: &mut WorkspaceGraphics, panels: Vec<Panel>| {
                    graphics.cache.update(panels);
                    queue.write_buffer(
                        &graphics.vertices,
                        0,
                        bytemuck::cast_slice(&graphics.cache.vertices),
                    );
                    graphics.count = graphics.cache.vertices.len() as u32;
                };
                let projection =
                    glam::camera::rh::proj::directx::perspective(0.95, 1.0, 0.05, 50.0);
                for distance in [1.8, 2.8, 4.0] {
                    for bearing in [-0.45_f32, 0.0, 0.45] {
                        let mut sign_station = station.clone();
                        sign_station.opacity = 1.0;
                        sign_station.position[0] = distance * bearing.sin();
                        sign_station.position[2] = -distance * bearing.cos();
                        let sign = station_panel(
                            &sign_station,
                            sign_station.position,
                            State::Completed,
                            None,
                        );
                        let rear = Panel {
                            origin: [-3.0, 3.2, -distance - 1.5],
                            pixel: 0.016,
                            columns: COLUMNS,
                            lines: vec![String::new(); ROWS],
                            accent: MUTED,
                            opacity: 1.0,
                            sign: None,
                            rule: None,
                        };
                        for yaw in [-0.18_f32, 0.18] {
                            for eye_x in [-0.032, 0.032] {
                                let eye = glam::Vec3::new(0.15 + eye_x, 1.6, 0.1);
                                let heading = bearing + yaw;
                                let direction = glam::Vec3::new(heading.sin(), 0.0, -heading.cos());
                                let matrix = projection
                                    * glam::camera::rh::view::look_at_mat4(
                                        eye,
                                        eye + direction,
                                        glam::Vec3::Y,
                                    );
                                upload(&mut graphics, vec![rear.clone()]);
                                let rear_image = capture(&graphics, matrix, false);
                                upload(&mut graphics, vec![sign.clone()]);
                                let sign_image = capture(&graphics, matrix, false);
                                upload(&mut graphics, vec![sign.clone(), rear.clone()]);
                                let forward = capture(&graphics, matrix, false);
                                let buggy = capture_order(&graphics, matrix, false, false);
                                upload(&mut graphics, vec![rear.clone(), sign.clone()]);
                                let reverse = capture(&graphics, matrix, false);
                                assert_eq!(
                                    forward, reverse,
                                    "insertion order changed the image: distance {distance}, yaw {yaw}, eye {eye_x}"
                                );
                                let glyphs: Vec<_> = (0..512 * 512)
                                    .map(|p| p * 4)
                                    // The title's white at half coverage or more, which nothing else of the sign is: at four metres its strokes are under half a pixel wide, and antialiased rather than aliased, so few pixels are wholly covered.
                                    .filter(|&i| {
                                        sign_image[i] > 110
                                            && sign_image[i + 1] > 110
                                            && sign_image[i + 2] > 110
                                            && rear_image[i + 1] < 100
                                    })
                                    .collect();
                                assert!(
                                    glyphs.len() > 5,
                                    "no visible test glyphs: distance {distance}, yaw {yaw}, eye {eye_x}"
                                );
                                assert!(
                                    glyphs
                                        .iter()
                                        .all(|&i| sign_image[i..i + 3] == forward[i..i + 3]),
                                    "rear card covered foreground text: distance {distance}, yaw {yaw}, eye {eye_x}"
                                );
                                assert!(
                                    glyphs
                                        .iter()
                                        .any(|&i| sign_image[i..i + 3] != buggy[i..i + 3]),
                                    "old single-draw bug must fail the glyph-preservation assertion"
                                );
                                let pixel = glyphs[glyphs.len() / 2];
                                let mut previous = rear_image[pixel + 1];
                                for opacity in [0.0, 0.25, 0.5, 0.75, 1.0] {
                                    let mut faded = sign.clone();
                                    faded.opacity = opacity;
                                    upload(&mut graphics, vec![faded, rear.clone()]);
                                    let image = capture(&graphics, matrix, false);
                                    if opacity == 0.0 {
                                        assert_eq!(
                                            image, rear_image,
                                            "invisible panels must not occlude"
                                        );
                                    } else {
                                        assert!(
                                            image[pixel + 1] > previous,
                                            "fade must progress smoothly"
                                        );
                                    }
                                    previous = image[pixel + 1];
                                }
                                assert_eq!(previous, sign_image[pixel + 1]);

                                let transform = glam::Mat4::from_rotation_y(0.3)
                                    * glam::Mat4::from_translation(glam::Vec3::new(0.4, 0.0, -0.2));
                                let recentered = capture(
                                    &graphics,
                                    (matrix * transform.inverse()) * transform,
                                    false,
                                );
                                let changed = changed_pixels(&forward, &recentered);
                                assert!(
                                    changed < 128,
                                    "recenter changed overlap: {changed} pixels"
                                );
                            }
                        }
                    }
                }

                // A small real sign/card depth gap and an off-axis card center make
                // center sorting wrong, although these surfaces never intersect.
                // Mirror the case so both yaw signs and both eye positions matter.
                let mut overlap_previews = Vec::new();
                let angled_projection =
                    glam::camera::rh::proj::directx::perspective(1.1, 1.0, 0.05, 50.0);
                for yaw in [-0.3_f32, 0.3] {
                    let sign = test_sign([0.0, 1.25, -2.8], State::Completed);
                    let rear = Panel {
                        origin: [if yaw < 0.0 { -0.2 } else { -1.84 }, 2.0, -2.92],
                        pixel: 0.0051,
                        columns: COLUMNS,
                        lines: vec![String::new(); ROWS],
                        accent: MUTED,
                        opacity: 1.0,
                        sign: None,
                        rule: None,
                    };
                    for eye_x in [-0.032, 0.032] {
                        let eye = glam::Vec3::new(eye_x, 1.6, 0.0);
                        let direction = glam::Vec3::new(yaw.sin(), 0.0, -yaw.cos());
                        let matrix = angled_projection
                            * glam::camera::rh::view::look_at_mat4(
                                eye,
                                eye + direction,
                                glam::Vec3::Y,
                            );
                        let depth = |p: &Panel| {
                            let clip = matrix * p.center().extend(1.0);
                            clip.z / clip.w
                        };
                        assert!(
                            depth(&rear) < depth(&sign),
                            "fixture must defeat center-depth sorting"
                        );
                        upload(&mut graphics, vec![sign.clone()]);
                        let sign_image = capture(&graphics, matrix, false);
                        upload(&mut graphics, vec![sign.clone(), rear.clone()]);
                        assert_eq!(panel_order(&graphics.cache.ranges, matrix), vec![1, 0]);
                        let fixed = capture(&graphics, matrix, false);
                        let buggy = capture_order(&graphics, matrix, false, false);
                        let glyphs: Vec<_> = (0..512 * 512)
                            .map(|p| p * 4)
                            // The title's white, which nothing else of the sign is.
                            .filter(|&i| {
                                sign_image[i] > 150
                                    && sign_image[i + 1] > 150
                                    && sign_image[i + 2] > 150
                            })
                            .collect();
                        assert!(glyphs.len() > 5);
                        assert!(
                            glyphs
                                .iter()
                                .all(|&i| sign_image[i..i + 3] == fixed[i..i + 3])
                        );
                        assert!(
                            glyphs
                                .iter()
                                .any(|&i| sign_image[i..i + 3] != buggy[i..i + 3])
                        );
                        upload(&mut graphics, vec![rear.clone(), sign.clone()]);
                        assert_eq!(fixed, capture(&graphics, matrix, false));
                        overlap_previews.extend([buggy, fixed]);
                    }
                }

                // A depth-writing body must cover the overlapping part of the sign,
                // while unoccluded glyphs remain visible in front of the rear card.
                let sign = test_sign([0.0, 1.25, -2.8], State::Completed);
                let rear = Panel {
                    origin: [-3.0, 3.2, -4.3],
                    pixel: 0.016,
                    columns: COLUMNS,
                    lines: vec![String::new(); ROWS],
                    accent: MUTED,
                    opacity: 1.0,
                    sign: None,
                    rule: None,
                };
                for eye_x in [-0.032, 0.032] {
                    let eye = glam::Vec3::new(eye_x, 1.6, 0.0);
                    let matrix = projection
                        * glam::camera::rh::view::look_at_mat4(
                            eye,
                            eye + glam::Vec3::NEG_Z,
                            glam::Vec3::Y,
                        );
                    upload(&mut graphics, vec![sign.clone()]);
                    let sign_image = capture(&graphics, matrix, false);
                    graphics.hide();
                    let body_image = capture(&graphics, matrix, true);
                    upload(&mut graphics, vec![sign.clone(), rear.clone()]);
                    let result = capture(&graphics, matrix, true);
                    let mut blocked = 0;
                    let mut visible = 0;
                    for i in (0..sign_image.len()).step_by(4) {
                        // The title's white, which nothing else of the sign is.
                        if sign_image[i] > 150 && sign_image[i + 1] > 150 && sign_image[i + 2] > 150
                        {
                            if body_image[i + 2] > 190 && body_image[i + 1] < 50 {
                                blocked += 1;
                                assert_eq!(
                                    &result[i..i + 4],
                                    &body_image[i..i + 4],
                                    "body must occlude text"
                                );
                            } else {
                                visible += 1;
                                assert_eq!(
                                    &result[i..i + 3],
                                    &sign_image[i..i + 3],
                                    "unblocked text must remain visible"
                                );
                            }
                        }
                    }
                    assert!(
                        blocked > 5 && visible > 5,
                        "test needs both blocked and visible glyphs: {blocked}, {visible}"
                    );

                    // Conversely, a genuinely nearer card must cover the sign;
                    // the fix must not simply force all signs on top of cards.
                    let mut near = rear.clone();
                    near.origin[2] = -1.7;
                    upload(&mut graphics, vec![near.clone()]);
                    let near_image = capture(&graphics, matrix, false);
                    for panels in [vec![sign.clone(), near.clone()], vec![near, sign.clone()]] {
                        upload(&mut graphics, panels);
                        assert_eq!(near_image, capture(&graphics, matrix, false));
                    }
                }
                {
                    // Rows: yaw-/left, yaw-/right, yaw+/left, yaw+/right.
                    // Columns: old insertion order, overlap-aware painter order.
                    let mut sheet = image::RgbaImage::new(1024, 2048);
                    for (i, bytes) in overlap_previews.into_iter().enumerate() {
                        let tile = image::RgbaImage::from_raw(512, 512, bytes).unwrap();
                        image::imageops::replace(
                            &mut sheet,
                            &tile,
                            (i % 2) as i64 * 512,
                            (i / 2) as i64 * 512,
                        );
                    }
                    crate::test_support::preview("TOMORI_SIGN_PREVIEW", &sheet);
                }

                // Exercise the production placement, complete mesh upload and GPU
                // buffer growth with every supported label. Aim at each actual
                // sign in turn; this is world navigation, not an overlay bypass.
                let stations: Vec<_> = (0..128)
                    .map(|i| Station {
                        facing: crate::delegation::HOME_EYES,
                        id: format!("density-{i:03}"),
                        title: format!("{i:03} {}", "READABLE ".repeat(18))
                            .chars()
                            .take(160)
                            .collect(),
                        position: crate::delegation::slot_position(i).to_array(),
                        state: State::Unknown,
                        opacity: 1.0,
                        scale: 1.0,
                        more: 0,
                        size: None,
                        highlighted: false,
                    })
                    .collect();
                graphics.update(&device, &queue, &Workspace::default(), &stations, None);
                assert_eq!(graphics.cache.ranges.len(), 128);
                // Every sign's mesh, and its leader, drawn as it is placed.
                let meshes = graphics
                    .cache
                    .meshes
                    .iter()
                    .map(|(_, mesh, _, _)| mesh.len())
                    .sum::<usize>();
                assert!(
                    (meshes + 128..=meshes + 128 * LEADER_VERTICES)
                        .contains(&graphics.cache.vertices.len())
                );
                assert!(graphics.vertex_capacity >= graphics.cache.vertices.len());
                assert!(graphics.vertex_capacity < MAX_VERTICES);
                eprintln!(
                    "128 full labels: {} vertices, {} GPU bytes ({} byte hard ceiling)",
                    graphics.cache.vertices.len(),
                    graphics.vertex_capacity * size_of::<Vertex>(),
                    MAX_VERTICES * size_of::<Vertex>()
                );
                let mut sheet = image::RgbaImage::new(4096, 2048);
                for (i, panel) in graphics.cache.panels.iter().enumerate() {
                    let eye = glam::Vec3::new(if i % 2 == 0 { -0.032 } else { 0.032 }, 1.6, 0.0);
                    let matrix = projection
                        * glam::camera::rh::view::look_at_mat4(eye, panel.center(), glam::Vec3::Y);
                    let image = capture(&graphics, matrix, false);
                    // Across the whole sign, which stands about the middle of the view, its rows centred.
                    let glyphs = (230..285)
                        .flat_map(|y| (180..330).map(move |x| (y * 512 + x) * 4))
                        // The title's white at about half coverage or more: from two to five metres, and as much as 50° above the eyes, strokes are under a pixel wide and antialiased rather than aliased, so few pixels are wholly covered.
                        .filter(|&p| image[p] > 110 && image[p + 1] > 110 && image[p + 2] > 110)
                        .count();
                    // Signs higher than 56° above the eyes, as the top rows of more workers than a space shows at once
                    // stand when the agent is nearer than 3.2 m, are seen too obliquely for this check until they turn
                    // to face the viewer; every one of the first 64 slots stands lower.
                    let centre = panel.center();
                    let elevation = (centre.y - 1.6).atan2(centre.x.hypot(centre.z));
                    assert!(
                        glyphs > 4 || elevation > 56f32.to_radians(),
                        "source {i} has no readable center glyphs"
                    );
                    if i < 64 {
                        assert!(elevation < 56f32.to_radians(), "source {i}");
                    }
                    {
                        let sheet = &mut sheet;
                        let tile = image::RgbaImage::from_raw(512, 512, image).unwrap();
                        let tile = image::imageops::resize(
                            &tile,
                            256,
                            256,
                            image::imageops::FilterType::Triangle,
                        );
                        image::imageops::replace(
                            sheet,
                            &tile,
                            (i % 16) as i64 * 256,
                            (i / 16) as i64 * 256,
                        );
                    }
                }
                crate::test_support::preview("TOMORI_DENSITY_PREVIEW", &sheet);
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                assert!(scope.pop().await.is_none());
            })
        });
    }

    #[test]
    fn shapes_stop_at_the_vertex_limit_and_a_stroke_needs_a_length() {
        let mut mesh = Vec::new();
        let rule = |to: [f32; 2]| Shape::Rule {
            from: [0.0, 0.0],
            to,
            width: 10.0,
            color: [1.0; 4],
        };
        shape(&mut mesh, &rule([0.0, 0.0]), 0.001, 0.0);
        assert!(mesh.is_empty());
        mesh.resize(
            MAX_VERTICES - 5,
            Vertex {
                position: [0.0; 3],
                color: [0.0; 4],
                uv: [0.0; 2],
                weight: 0.0,
            },
        );
        quad(&mut mesh, 0.0, 0.0, 0.0, 1.0, 1.0, [1.0; 4]);
        shape(&mut mesh, &rule([1000.0, 0.0]), 0.001, 0.0);
        assert_eq!(mesh.len(), MAX_VERTICES - 5);
    }

    #[test]
    fn characters_the_font_lacks_are_drawn_as_a_question_mark() {
        let panel = |line: &str| Panel {
            origin: [0.0, 2.0, -3.0],
            pixel: 0.005,
            columns: COLUMNS,
            lines: vec![line.into()],
            accent: WHITE,
            opacity: 1.0,
            sign: None,
            rule: None,
        };
        let mesh = |panel: Panel| {
            let mut mesh = Vec::new();
            panel_mesh(&panel, &mut mesh);
            mesh.into_iter()
                .map(|vertex| vertex.position)
                .collect::<Vec<_>>()
        };
        assert_eq!(mesh(panel("\u{1f600}")), mesh(panel("?")));
    }

    #[test]
    fn a_sign_built_while_glyphs_waited_is_built_again_until_it_has_them() {
        let station = Station {
            facing: crate::delegation::HOME_EYES,
            id: "a".into(),
            title: crate::test_support::undrawn('\u{9f00}', 2),
            state: State::Completed,
            position: [0.0, 1.25, -2.8],
            opacity: 1.0,
            scale: 1.0,
            more: 0,
            size: None,
            highlighted: false,
        };
        let sign = station_panel(&station, station.position, State::Completed, None);
        let card = Panel {
            origin: [0.0, 2.0, -3.0],
            pixel: 0.005,
            columns: COLUMNS,
            // No text, so that it needs no glyph.
            lines: vec![String::new()],
            accent: WHITE,
            opacity: 1.0,
            sign: None,
            rule: None,
        };
        let mut cache = PageCache::default();
        let budget = crate::test_support::Budgeted::spent();
        assert!(cache.update(vec![sign.clone(), card.clone()]));
        assert!(!cache.complete);
        let bare = cache.vertices.len();
        // Only the sign lacked glyphs, so only it is built again, still without its kanji while the budget is spent, which no other test draws.
        tomori_text::atlas::frame();
        assert!(cache.update(vec![sign.clone(), card.clone()]));
        assert_eq!(cache.builds, 3);
        assert!(!cache.complete);
        drop(budget);
        assert!(cache.update(vec![sign.clone(), card.clone()]));
        assert!(cache.complete);
        let mut whole = Vec::new();
        panel_mesh(&sign, &mut whole);
        panel_mesh(&card, &mut whole);
        assert!(whole.len() > bare);
        assert_eq!((cache.builds, cache.vertices.len()), (4, whole.len()));
        assert!(!cache.update(vec![sign, card]));
        assert_eq!(cache.builds, 4);
    }

    #[test]
    fn mesh_is_finite_bounded_cached_and_clears() {
        let panels: Vec<_> = (0..20)
            .map(|i| Panel {
                origin: [i as f32, 2.0, -3.0],
                pixel: 0.005,
                columns: COLUMNS,
                lines: vec!["MW@#".repeat(11); ROWS],
                accent: WHITE,
                opacity: 1.0,
                sign: None,
                rule: None,
            })
            .collect();
        let mut cache = PageCache::default();
        assert!(cache.update(panels.clone()));
        assert!(cache.vertices.len() <= MAX_VERTICES);
        assert!(
            cache
                .vertices
                .iter()
                .all(|v| v.position.iter().all(|v| v.is_finite()))
        );
        for _ in 0..1000 {
            assert!(!cache.update(panels.clone()));
        }
        assert_eq!(
            cache.builds, 20,
            "static pages must not rebuild glyph meshes"
        );
        assert!(cache.update(Vec::new()));
        assert!(cache.vertices.is_empty());
        assert!(cache.ranges.is_empty());
        assert!(cache.meshes.is_empty());
    }

    /// Where someone stands to look at the arc about `origin`: as far from its middle as `audience` is, turned `degrees`
    /// about it, to its left for positive, at the height of `audience`.
    fn about(origin: glam::Vec3, audience: glam::Vec3, degrees: f32) -> [f32; 3] {
        let from = (audience - origin).with_y(0.0);
        (origin.with_y(audience.y) + glam::Quat::from_rotation_y(degrees.to_radians()) * from)
            .to_array()
    }

    #[test]
    fn each_sign_turns_upright_to_face_its_viewer_wherever_they_stand_its_rows_centred_above_its_worker()
     {
        use tomori_scene::anchors::{Anchor, EYES, HomeArc};
        let stations: Vec<Station> = (0..6)
            .map(|slot| Station {
                facing: EYES.to_array(),
                id: format!("w-{slot}"),
                title: ["Mm", "MMMM MMMM", "MMMMMMMMMMMMMMMMMMMMMMMMMMM MMM"][slot % 3].into(),
                state: State::Running,
                opacity: 1.0,
                position: HomeArc::at(Anchor::AGENT, EYES, slot).to_array(),
                scale: 1.0,
                more: 0,
                size: None,
                highlighted: false,
            })
            .collect();
        let middle = glam::Vec3::new(0.0, 1.2, -3.2);
        // From home, 45° to the left and 70° to the right about the arc's middle, from behind it, and from close beside
        // one worker.
        for viewer in [
            EYES.to_array(),
            about(middle, EYES, 45.0),
            about(middle, EYES, -70.0),
            about(middle, EYES, 180.0),
            (glam::Vec3::from_array(stations[2].position) + glam::Vec3::new(0.5, 0.3, 0.6))
                .to_array(),
        ] {
            let mut placements = LabelPlacement::default();
            let (panels, _) = layout(
                &Workspace::default(),
                &stations,
                0,
                0,
                &[],
                &mut placements,
                Some(viewer),
            );
            for (panel, station) in panels.iter().zip(&stations) {
                let sign = panel.sign.as_ref().unwrap();
                let (right, normal) = (
                    glam::Vec3::from_array(sign.right),
                    glam::Vec3::from_array(sign.normal),
                );
                // Upright: it turns only about its upright, never leaning.
                assert_eq!((right.y, normal.y), (0.0, 0.0));
                assert!((right.length() - 1.0).abs() < 1e-5 && right.dot(normal).abs() < 1e-5);
                assert!(
                    (right.cross(glam::Vec3::Y) - normal).length() < 1e-5,
                    "readable, not mirrored"
                );
                // Facing the viewer, along the ground.
                let place = glam::Vec3::from_array(station.position);
                let towards = (glam::Vec3::from_array(viewer) - place)
                    .with_y(0.0)
                    .normalize();
                assert!(
                    normal.dot(towards) > 0.99999,
                    "{viewer:?}: {normal} {towards}"
                );
                // Its plate's middle stands straight above its worker, a little behind it from the viewer.
                let width = (panel.columns * 9) as f32 * panel.pixel;
                let plate = glam::Vec3::from_array(panel.origin) + right * (width / 2.0);
                let off = (plate - place).with_y(0.0);
                assert!(off.dot(right).abs() < 1e-4, "{viewer:?}: {off}");
                assert!(
                    (off.dot(normal) + SIGN_BEHIND).abs() < 1e-4,
                    "{viewer:?}: {off}"
                );
                // As wide as its widest row, and each row centred across it, within half a cell.
                assert_eq!(
                    panel.columns,
                    sign_columns(&panel.lines),
                    "{:?}",
                    panel.lines
                );
                let mut local = Vec::new();
                local_mesh(panel, &mut local);
                for (index, line) in panel.lines.iter().enumerate() {
                    let (top, bottom) = (
                        -((index * 11) as f32) * panel.pixel,
                        -(((index + 1) * 11) as f32) * panel.pixel,
                    );
                    // Its glyphs, in front of its plate and pill, before it is bowed.
                    let reach = (SIGN_COLUMNS * 9) as f32 * panel.pixel / 2.0;
                    let xs: Vec<f32> = local
                        .as_chunks::<6>()
                        .0
                        .iter()
                        .filter(|quad| {
                            let [x, y, z] = quad
                                .iter()
                                .map(|v| glam::Vec3::from_array(v.position) / 6.0)
                                .sum::<glam::Vec3>()
                                .to_array();
                            y < top
                                && y > bottom
                                && z - SIGN_BOW * ((x - width / 2.0) / reach).powi(2)
                                    >= SIGN_TEXT - 1e-4
                        })
                        .flatten()
                        .map(|v| v.position[0])
                        .collect();
                    if index + 1 == panel.lines.len() {
                        // Its state's pill is centred as a whole, its symbol and word inside it.
                        continue;
                    }
                    assert!(!xs.is_empty(), "{line:?}");
                    let low = xs.iter().copied().fold(f32::INFINITY, f32::min);
                    let high = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                    assert!(
                        ((low + high) / 2.0 - width / 2.0).abs() <= 4.5 * panel.pixel,
                        "{line:?}: {low} {high} {width}"
                    );
                }
            }
        }
    }

    #[test]
    fn signs_turned_to_face_any_viewer_never_reach_one_another() {
        use tomori_scene::anchors::{Anchor, EYES, HomeArc};
        // Sixteen workers, each with a title six rows long, the longest a sign shows, at home and about a workers anchor
        // across a yard, where its signs are drawn larger.
        let yard = Anchor {
            position: glam::Vec3::new(2.0, 1.5, -7.7),
            orientation: glam::Quat::IDENTITY,
        };
        for (origin, audience) in [(Anchor::AGENT, EYES), (yard, EYES)] {
            let stations: Vec<Station> = (0..16)
                .map(|slot| Station {
                    facing: audience.to_array(),
                    id: format!("w-{slot:02}"),
                    title: "Wide words ".repeat(14),
                    state: State::Running,
                    opacity: 1.0,
                    position: HomeArc::at(origin, audience, slot).to_array(),
                    scale: 1.0,
                    more: 0,
                    size: None,
                    highlighted: false,
                })
                .collect();
            let centre = stations
                .iter()
                .map(|s| glam::Vec3::from_array(s.position))
                .sum::<glam::Vec3>()
                / 16.0;
            let mut placements = LabelPlacement::default();
            // From whoever the agent faces, 45° to the left and 70° to the right about the arc's middle, from behind
            // it, from within it, and from between two of its workers.
            let between = (glam::Vec3::from_array(stations[0].position)
                + glam::Vec3::from_array(stations[2].position))
                / 2.0;
            for viewer in [
                audience.to_array(),
                about(centre, audience, 45.0),
                about(centre, audience, -70.0),
                about(centre, audience, 180.0),
                centre.with_y(audience.y).to_array(),
                between.with_y(audience.y).to_array(),
            ] {
                let (panels, _) = layout(
                    &Workspace::default(),
                    &stations,
                    0,
                    0,
                    &[],
                    &mut placements,
                    Some(viewer),
                );
                // Each sign's plate, as placed for this viewer: about the upright it turns on, how far it reaches
                // along the ground, and how high it stands.
                let plates: Vec<(glam::Vec3, f32, f32, f32)> = panels
                    .iter()
                    .map(|panel| {
                        let mut mesh = Vec::new();
                        let surface = panel_mesh(panel, &mut mesh);
                        let plate = &mesh[..surface];
                        let normal = glam::Vec3::from_array(panel.sign.as_ref().unwrap().normal);
                        let width = (panel.columns * 9) as f32 * panel.pixel;
                        let axis = glam::Vec3::from_array(panel.origin)
                            + glam::Vec3::from_array(panel.sign.as_ref().unwrap().right)
                                * (width / 2.0)
                            + normal * SIGN_BEHIND;
                        let reach = plate
                            .iter()
                            .map(|v| {
                                (glam::Vec3::from_array(v.position) - axis)
                                    .with_y(0.0)
                                    .length()
                            })
                            .fold(0.0, f32::max);
                        let low = plate
                            .iter()
                            .map(|v| v.position[1])
                            .fold(f32::INFINITY, f32::min);
                        let high = plate
                            .iter()
                            .map(|v| v.position[1])
                            .fold(f32::NEG_INFINITY, f32::max);
                        (axis, reach, low, high)
                    })
                    .collect();
                for (index, (axis, reach, low, high)) in plates.iter().enumerate() {
                    // Turning about its upright, a sign reaches no further than half the room it is given, whichever
                    // way it faces.
                    let scale = panels[index].sign.as_ref().unwrap().scale;
                    assert!(*reach <= SIGN_APART[1] * scale / 2.0, "{reach}");
                    for (other, (them, their_reach, their_low, their_high)) in
                        plates.iter().enumerate().skip(index + 1)
                    {
                        let apart = (*axis - *them).with_y(0.0).length();
                        assert!(
                            high < their_low || their_high < low || apart > reach + their_reach,
                            "{viewer:?}: sign {index} reaches sign {other}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn on_the_default_arc_no_sign_reaches_another_worker_nor_another_sign_at_home_or_about_an_anchor()
     {
        use tomori_scene::anchors::{Anchor, EYES, HomeArc};
        // How far about its middle a worker's flock or completed cube reaches.
        const WORKER: f32 = 0.2;
        // At home, and about a workers anchor further than the arc's radius, as the farmyard's beside its coop, 3.6 m
        // from where participants arrive, and one across a yard, 8 m off: the arc widens, its signs drawn larger with it.
        let anchor = |x: f32, z: f32| Anchor {
            position: glam::Vec3::new(x, 1.5, z),
            orientation: glam::Quat::IDENTITY,
        };
        for (origin, audience) in [
            (Anchor::AGENT, EYES),
            (anchor(-1.6, -3.2), EYES),
            (anchor(2.0, -7.7), EYES),
        ] {
            // Sixteen workers, each with a title six rows long, the longest a sign shows.
            let stations: Vec<Station> = (0..16)
                .map(|slot| Station {
                    facing: audience.to_array(),
                    id: format!("w-{slot:02}"),
                    title: "Wide words ".repeat(14),
                    state: State::Running,
                    opacity: 1.0,
                    position: HomeArc::at(origin, audience, slot).to_array(),
                    scale: 1.0,
                    more: 0,
                    size: None,
                    highlighted: false,
                })
                .collect();
            let mut placements = LabelPlacement::default();
            let (panels, _) = layout(
                &Workspace::default(),
                &stations,
                0,
                0,
                &[],
                &mut placements,
                None,
            );
            assert_eq!(panels.len(), 16);
            // Each sign as a box: across along its right, up from its bottom to its top, and a little deep.
            let boxes: Vec<(glam::Vec3, glam::Vec3, glam::Vec3, f32, f32, f32)> = panels
                .iter()
                .map(|panel| {
                    let sign = panel.sign.as_ref().unwrap();
                    assert_eq!(panel.lines.len(), SIGN_TITLE_ROWS + 1);
                    let right = glam::Vec3::from_array(sign.right);
                    let width = (panel.columns * 9 + 4) as f32 * panel.pixel;
                    let height = (panel.lines.len() * 11 + 4) as f32 * panel.pixel;
                    // Its background starts two pixels before its origin.
                    let middle = glam::Vec3::from_array(panel.origin)
                        + right * (width / 2.0 - 2.0 * panel.pixel);
                    (
                        middle,
                        right,
                        glam::Vec3::from_array(sign.normal),
                        width / 2.0,
                        panel.origin[1] - height,
                        panel.origin[1] + 2.0 * panel.pixel,
                    )
                })
                .collect();
            let inside = |(middle, right, normal, half, bottom, top): &(
                glam::Vec3,
                glam::Vec3,
                glam::Vec3,
                f32,
                f32,
                f32,
            ),
                          point: glam::Vec3,
                          margin: f32| {
                let offset = point - *middle;
                offset.dot(*right).abs() <= half + margin
                    && offset.dot(*normal).abs() <= 0.08 + margin
                    && (bottom - margin..=top + margin).contains(&point.y)
            };
            for (index, (panel, sign)) in panels.iter().zip(&boxes).enumerate() {
                let own = panel.sign.as_ref().unwrap();
                // Straight above its own worker, its bottom clear of its flock, the post between.
                assert!(own.own, "{index}");
                let station = glam::Vec3::from_array(own.station);
                assert!((sign.0 - station).dot(sign.1).abs() < 1e-3, "{index}");
                assert!(sign.4 >= station.y + WORKER, "{index}");
                for (other, station) in stations
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                {
                    let at = glam::Vec3::from_array(station.position);
                    assert!(
                        !inside(sign, at, WORKER),
                        "sign {index} reaches worker {other}"
                    );
                }
                // Nor does it reach another sign: their middles and corners stand outside it.
                for (other, them) in boxes
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                {
                    let corners = [-1.0, 1.0].into_iter().flat_map(|side| {
                        [them.4, them.5].map(|y| (them.0 + them.1 * (side * them.3)).with_y(y))
                    });
                    for corner in corners {
                        assert!(
                            !inside(sign, corner, 0.0),
                            "sign {index} reaches sign {other}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn in_a_ring_each_layer_s_signs_stand_at_one_height_above_its_swarms_and_below_the_next() {
        use tomori_protocol::cluster::{Anchor, Cluster, Formation, HOME};
        // How far about its middle a worker's flock reaches, and how much clearer than that every sign and leader keeps.
        const FLOCK: f32 = 0.13;
        const MARGIN: f32 = 0.02;
        // The farmyard's home ring, as `presence` reported it when a lower worker's leader ran up through the swarm above
        // it to its sign; a whole ring before home facing it; and one across a yard, whose signs are drawn larger.
        let rings = [
            Anchor::new(
                HOME,
                tomori_protocol::Pose::upright([-3.47, 1.5, 0.07], 89f32.to_radians()),
            )
            .with_sweep(300.0),
            Anchor::new(
                HOME,
                tomori_protocol::Pose::at([0.0, 1.4, -3.5])
                    .facing_towards(crate::delegation::HOME_EYES),
            ),
            Anchor::new(
                HOME,
                tomori_protocol::Pose::at([1.0, 1.5, -7.0])
                    .facing_towards(crate::delegation::HOME_EYES),
            ),
        ];
        // Titles of one to six rows, the most a sign shows.
        let titles = [
            "Centre home ring on workbench".to_owned(),
            "Wide words ".repeat(14),
            "Smoother agent turning and gliding".to_owned(),
            "Debug following during replies, and then the farmyard's mountains and bush".to_owned(),
            "Ambient audio".to_owned(),
            "Restore swarm flight on join/leave".to_owned(),
        ];
        let mut seed = 0x5eed_u64;
        for anchor in rings {
            let cluster = Cluster::new(&anchor.with_formation(Formation::Ring));
            let size = cluster.sign_size();
            assert!(size.is_some());
            for count in 1..=18 {
                let slots = cluster.slots(count);
                // One sign for those gathered into the "+N more", where its slot stands.
                let gathered = slots.iter().filter(|slot| slot.more).count();
                let standing: Vec<_> = slots
                    .iter()
                    .filter(|slot| !slot.more)
                    .chain(slots.iter().find(|slot| slot.more))
                    .collect();
                // Placed in the order of their identifiers, which need not be their slots': in slot order, its reverse
                // and shuffled.
                for order in 0..8 {
                    let mut ids: Vec<usize> = (0..standing.len()).collect();
                    match order {
                        0 => {}
                        1 => ids.reverse(),
                        _ => {
                            for i in (1..ids.len()).rev() {
                                seed = seed
                                    .wrapping_mul(6_364_136_223_846_793_005)
                                    .wrapping_add(1_442_695_040_888_963_407);
                                ids.swap(i, (seed >> 33) as usize % (i + 1));
                            }
                        }
                    }
                    let stations: Vec<Station> = standing
                        .iter()
                        .enumerate()
                        .map(|(index, place)| Station {
                            facing: crate::delegation::HOME_EYES,
                            id: format!("w-{:02}", ids[index]),
                            title: titles[index % titles.len()].clone(),
                            state: State::Running,
                            position: place.position,
                            opacity: 1.0,
                            scale: place.scale,
                            more: if place.more { gathered } else { 0 },
                            size,
                            highlighted: false,
                        })
                        .collect();
                    let mut placements = LabelPlacement::default();
                    let (panels, _) = layout(
                        &Workspace::default(),
                        &stations,
                        0,
                        0,
                        &[],
                        &mut placements,
                        None,
                    );
                    assert_eq!(panels.len(), stations.len());
                    // Each sign's worker, its plate's axis, reach about it, bottom and top, and its leader's ends.
                    struct Drawn {
                        worker: glam::Vec3,
                        axis: glam::Vec3,
                        reach: f32,
                        low: f32,
                        high: f32,
                        bottom: glam::Vec3,
                        dot: glam::Vec3,
                    }
                    let drawn: Vec<Drawn> = panels
                        .iter()
                        .map(|panel| {
                            let sign = panel.sign.as_ref().unwrap();
                            let worker = glam::Vec3::from_array(sign.station);
                            let label =
                                format!("{count} workers, order {order}, {}", sign.source_id);
                            // In its own place, straight above its worker, never risen or moved aside.
                            assert!(sign.own, "{label}");
                            let mut mesh = Vec::new();
                            let surface = panel_mesh(panel, &mut mesh);
                            let plate = &mesh[..surface];
                            let right = glam::Vec3::from_array(sign.right);
                            let width = (panel.columns * 9) as f32 * panel.pixel;
                            let axis = glam::Vec3::from_array(panel.origin)
                                + right * (width / 2.0)
                                + glam::Vec3::from_array(sign.normal) * SIGN_BEHIND;
                            let reach = plate
                                .iter()
                                .map(|v| {
                                    (glam::Vec3::from_array(v.position) - axis)
                                        .with_y(0.0)
                                        .length()
                                })
                                .fold(0.0, f32::max);
                            let low = plate
                                .iter()
                                .map(|v| v.position[1])
                                .fold(f32::INFINITY, f32::min);
                            let high = plate
                                .iter()
                                .map(|v| v.position[1])
                                .fold(f32::NEG_INFINITY, f32::max);
                            let bottom = glam::Vec3::from_array(panel.origin)
                                + right * (width / 2.0)
                                - glam::Vec3::Y
                                    * ((panel.lines.len() * 11) as f32 + PLATE_MARGIN[1])
                                    * panel.pixel;
                            let dot = worker - glam::Vec3::from_array(sign.towards) * SIGN_BEHIND;
                            // Its leader hangs straight down to its worker, a short way, clear of its own flock.
                            let leader = bottom - dot;
                            let lean = leader.with_y(0.0).length().atan2(leader.y).to_degrees();
                            assert!(lean < 0.1, "{label}: leans {lean}°");
                            assert!(low >= worker.y + FLOCK, "{label}");
                            Drawn {
                                worker,
                                axis,
                                reach,
                                low,
                                high,
                                bottom,
                                dot,
                            }
                        })
                        .collect();
                    for (index, sign) in drawn.iter().enumerate() {
                        let label = format!("{count} workers, order {order}, sign {index}");
                        for (other, them) in drawn
                            .iter()
                            .enumerate()
                            .filter(|(other, _)| *other != index)
                        {
                            // The signs of one layer stand at one height.
                            if (sign.worker.y - them.worker.y).abs() < 1e-4 {
                                assert!(
                                    (sign.bottom.y - them.bottom.y).abs() < 1e-4,
                                    "{label}, {other}"
                                );
                            }
                            // Never reaching another, turned any way.
                            let apart = (sign.axis - them.axis).with_y(0.0).length();
                            assert!(
                                sign.high < them.low
                                    || them.high < sign.low
                                    || apart > sign.reach + them.reach,
                                "{label} reaches sign {other}"
                            );
                            // In a column, swarm, sign, swarm, sign: a sign reaching over a worker above it stands below
                            // its flock.
                            let beside = (sign.axis - them.worker).with_y(0.0).length();
                            if beside <= sign.reach + FLOCK + MARGIN
                                && them.worker.y > sign.worker.y
                            {
                                assert!(
                                    sign.high + MARGIN <= them.worker.y - FLOCK,
                                    "{label} reaches the flock of {other}"
                                );
                            }
                            // Its leader passes clear of every other flock and sign.
                            let (from, to) = (sign.dot.y, sign.bottom.y);
                            let nearest = them.worker.y.clamp(from, to);
                            let off = (sign.dot.with_y(nearest) - them.worker).length();
                            assert!(
                                off > FLOCK + MARGIN,
                                "{label}'s leader passes the flock of {other}"
                            );
                            let across = (sign.dot - them.axis).with_y(0.0).length();
                            assert!(
                                to < them.low || them.high < from || across > them.reach + MARGIN,
                                "{label}'s leader passes sign {other}"
                            );
                        }
                    }
                }
            }
        }
    }
}
