//! Bounded, world-space task labels and coordinator-published result pages.
//! This deliberately has no dependency on the head-locked caption renderer.
use crate::delegation::{Kind, Presentation, State, Station, Workspace};
use bytemuck::{Pod, Zeroable};
use font8x8::{BASIC_FONTS, UnicodeFonts};

const MAX_LABELS: usize = 128;
const MAX_RESULTS: usize = 64;
// A focused card keeps all content readable and makes content paging independent
// of the size of neighboring cards.
pub const CARDS_PER_PAGE: usize = 1;
const COLUMNS: usize = 44;
const ROWS: usize = 18;
const CONTENT_ROWS: usize = ROWS - 4;
// Horizontal glyph runs need at most four quads per bitmap row. Hard cap is
// enforced as well, including backgrounds, even for hostile input.
// Every supported 160-character title + status, card and notice, even at
// four runs per bitmap row. GPU storage grows on demand, never eagerly to this.
const MAX_VERTICES: usize = 4_500_000;
const INITIAL_VERTICES: usize = 16_384;
const WHITE: [f32; 3] = [0.91, 0.96, 1.0];
const MUTED: [f32; 3] = [0.48, 0.72, 0.85];
const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    color: [f32; 4],
}

#[derive(Clone, Debug, PartialEq)]
struct Sign {
    source_id: String,
    // Both vectors lie in the scene XZ plane; normal points towards scene home.
    right: [f32; 3],
    normal: [f32; 3],
    state: State,
    station: [f32; 3],
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
}

/// A bounded cached copy of the focused card's formatted content. It is shared
/// by the navigation and rendering paths so both use exactly the same pages.
#[derive(Default)]
struct ContentCache {
    card: Option<Presentation>,
    pages: Vec<Vec<String>>,
}

fn same_card(a: &Presentation, b: &Presentation) -> bool {
    a.kind == b.kind && a.text == b.text && a.columns == b.columns && a.rows == b.rows
}

impl ContentCache {
    fn update(&mut self, card: Option<&Presentation>) {
        // Never clone unbounded or structurally malformed network data.
        let valid = card.is_none_or(|card| {
            !(card.text.len() > 4096
                || card.columns.len() > 6
                || card.rows.len() > 16
                || card.columns.iter().any(|s| s.len() > 80)
                || card.rows.iter().any(|row| {
                    row.len() > 6
                        || row.iter().any(|s| s.len() > 160)
                        || (card.kind == Kind::Table && row.len() != card.columns.len())
                }))
        });
        if valid
            && match (&self.card, card) {
                (Some(a), Some(b)) => same_card(a, b),
                (None, None) => true,
                _ => false,
            }
        {
            return;
        }
        let bounded = card.map(|card| {
            if !valid {
                Presentation {
                    text: "[INVALID CONTENT]".into(),
                    ..Default::default()
                }
            } else {
                Presentation {
                    kind: card.kind,
                    text: card.text.clone(),
                    columns: card.columns.clone(),
                    rows: card.rows.clone(),
                    ..Default::default()
                }
            }
        });
        if match (&self.card, &bounded) {
            (Some(a), Some(b)) => same_card(a, b),
            (None, None) => true,
            _ => false,
        } {
            return;
        }
        self.pages = bounded.as_ref().map(content_pages).unwrap_or_default();
        self.card = bounded;
    }
}

fn results(workspace: &Workspace) -> Vec<&Presentation> {
    let mut results: Vec<_> = workspace.presentations.iter().take(MAX_RESULTS).collect();
    results.sort_by(|a, b| a.id.cmp(&b.id));
    results
}

/// Right-stick navigation, reset on root change and clamped on content edits.
#[derive(Default)]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub struct CardNavigation {
    root: Option<String>,
    pub card: usize,
    pub content: usize,
    latched: bool,
    cache: ContentCache,
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
impl CardNavigation {
    pub fn release(&mut self) {
        self.latched = false;
    }

    pub fn update(&mut self, root: Option<&str>, workspace: &Workspace, x: f32, y: f32) {
        let changed_root = self.root.as_deref() != root;
        if changed_root {
            self.root = root.map(str::to_owned);
            self.card = 0;
            self.content = 0;
            self.latched = false;
        }
        let cards = results(workspace);
        self.card = self.card.min(cards.len().saturating_sub(1));
        let moving = x.is_finite() && y.is_finite() && (x.abs() > 0.6 || y.abs() > 0.6);
        let action = moving && !self.latched && !changed_root;
        self.latched = moving;
        if action && x.abs() > y.abs() && !cards.is_empty() {
            self.card = step(self.card, cards.len(), x > 0.0);
            self.content = 0;
        }
        self.cache.update(cards.get(self.card).copied());
        let count = self.cache.pages.len();
        self.content = self.content.min(count.saturating_sub(1));
        if action && y.abs() >= x.abs() && count > 0 {
            self.content = step(self.content, count, y < 0.0);
        }
    }
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn step(current: usize, count: usize, forward: bool) -> usize {
    if forward {
        (current + 1) % count
    } else {
        (current + count - 1) % count
    }
}

#[derive(Default)]
struct PageCache {
    panels: Vec<Panel>,
    meshes: Vec<(Panel, Vec<Vertex>)>,
    vertices: Vec<Vertex>,
    ranges: Vec<PanelRange>,
    builds: usize,
}

struct PanelRange {
    center: glam::Vec3,
    vertices: std::ops::Range<u32>,
    // Background strips, post and edge accent only: text shares this shallow
    // surface, with millimetre offsets. Never compare thousands of glyph runs.
    surface: Vec<[glam::Vec3; 3]>,
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
            let input = std::mem::take(&mut polygon);
            if input.is_empty() {
                break;
            }
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
        if self.panels == panels {
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
            let mut key = panel.clone();
            key.opacity = 1.0;
            let entry = if let Some(entry) = old
                .remove(&mesh_id(&key))
                .filter(|(cached, _)| *cached == key)
            {
                entry
            } else {
                let mut vertices = Vec::new();
                panel_mesh(&key, &mut vertices);
                self.builds += 1;
                (key, vertices)
            };
            self.meshes.push(entry);
        }
        self.vertices.clear();
        self.ranges.clear();
        for (panel, (_, mesh)) in panels.iter().zip(&self.meshes) {
            let start = self.vertices.len() as u32;
            assert!(
                self.vertices.len() + mesh.len() <= MAX_VERTICES,
                "workspace budget must fit every supported panel"
            );
            self.vertices.extend(mesh.iter().map(|v| {
                let mut v = *v;
                v.color[3] *= panel.opacity;
                v
            }));
            let end = self.vertices.len() as u32;
            if start != end && panel.opacity > 0.0 {
                self.ranges.push(PanelRange {
                    center: panel.center(),
                    vertices: start..end,
                    surface: mesh[..((end - start) as usize).min(if panel.sign.is_some() {
                        26 * 6
                    } else {
                        2 * 6
                    })]
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

pub struct WorkspaceGraphics {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    cache: PageCache,
    count: u32,
    page: usize,
    pages: usize,
    content_page: usize,
    content: ContentCache,
    placements: LabelPlacement,
    vertex_capacity: usize,
}

impl WorkspaceGraphics {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("workspace.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world workspace layout"),
            bind_group_layouts: &[Some(camera_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world workspace text"),
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
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bounded workspace glyph runs"),
            size: (INITIAL_VERTICES * size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            vertices,
            cache: PageCache::default(),
            count: 0,
            page: 0,
            pages: 0,
            content_page: 0,
            content: ContentCache::default(),
            placements: LabelPlacement::default(),
            vertex_capacity: INITIAL_VERTICES,
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
    ) {
        let cards = results(workspace);
        self.page = self.page.min(cards.len().saturating_sub(1));
        self.content.update(cards.get(self.page).copied());
        self.content_page = self
            .content_page
            .min(self.content.pages.len().saturating_sub(1));
        let (panels, pages) = layout(
            workspace,
            stations,
            self.page,
            self.content_page,
            &self.content.pages,
            &mut self.placements,
        );
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
                    label: Some("grown bounded workspace glyph runs"),
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
        scene_to_clip: glam::Mat4,
    ) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, camera, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        // Blending is order dependent. Preserve each panel's background-first
        // mesh order, but draw whole panels back-to-front for this eye. The
        // shared depth attachment still occludes them with opaque scene bodies;
        // no panel writes depth, so fades never leave invisible occluders.
        // Use overlap depths rather than center depths: a tilted card's center
        // can be nearer even though its overlapping edge is behind the sign.
        for index in panel_order(&self.cache.ranges, scene_to_clip) {
            pass.draw(self.cache.ranges[index].vertices.clone(), 0..1);
        }
    }
}

fn state_label(state: &State) -> (&'static str, [f32; 3]) {
    match state {
        State::Running => ("RUNNING", [0.4, 0.72, 1.0]),
        State::Waiting => ("WAITING", [1.0, 0.72, 0.24]),
        State::Idle => ("IDLE", [0.55, 0.7, 0.78]),
        State::Completed => ("COMPLETED", [0.3, 0.95, 0.65]),
        State::Error => ("ERROR", [1.0, 0.35, 0.3]),
        State::Unknown => ("UNKNOWN", [0.7, 0.65, 0.8]),
    }
}

fn layout(
    workspace: &Workspace,
    stations: &[Station],
    page: usize,
    content_page: usize,
    content: &[Vec<String>],
    placements: &mut LabelPlacement,
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
        panels.push(station_panel(station, placements.position(station), state));
    }
    if workspace.truncated {
        notices.push("Workspace discovery truncated [LIMIT]".into());
    } else if stations.len() < stations_count_hint(workspace) {
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
        lines.push("-".repeat(COLUMNS));
        if let Some(lines_on_page) = content.get(content_page) {
            lines.extend(lines_on_page.iter().cloned());
        }
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

fn station_panel(station: &Station, position: [f32; 3], state: State) -> Panel {
    let (state_text, accent) = state_label(&state);
    let [sx, sy, sz] = position;
    let radius = sx.hypot(sz);
    let normal = if radius > 0.0001 {
        [-sx / radius, 0.0, -sz / radius]
    } else {
        [0.0, 0.0, 1.0]
    };
    let right = [normal[2], 0.0, -normal[0]];
    let pixel = 0.0038;
    let half_width = (28 * 9) as f32 * pixel * 0.5;
    Panel {
        origin: [
            sx + normal[0] * 0.10 - right[0] * half_width,
            sy + 0.42,
            sz + normal[2] * 0.10 - right[2] * half_width,
        ],
        pixel,
        columns: 28,
        lines: label_lines(&station.title, state_text),
        accent,
        opacity: station.opacity,
        sign: Some(Sign {
            source_id: station.id.clone(),
            right,
            normal,
            state,
            station: station.position,
        }),
    }
}

fn label_lines(title: &str, status: &str) -> Vec<String> {
    let mut lines = vec![String::new()];
    for ch in title.chars().take(160) {
        if lines.last().unwrap().chars().count() == 28 {
            lines.push(String::new());
        }
        lines
            .last_mut()
            .unwrap()
            .push(if ch.is_control() { ' ' } else { ch });
    }
    lines.push(status.into());
    lines
}

/// Stable retained placement for legacy/arbitrary same-bearing snapshots.
/// Normal scene stations already occupy the shared, non-overlapping lattice.
/// Arbitrary old snapshots are assigned the nearest free cell, rather than
/// letting a hostile near-home panel occupy most of the observer's sightlines.
/// Worker anchors never move. Placements survive metadata/status/order changes
/// and are bounded by the supported station set. A post preserves association.
#[derive(Default)]
struct LabelPlacement(std::collections::BTreeMap<String, [f32; 3]>);

impl LabelPlacement {
    fn retain(&mut self, stations: &[&Station]) {
        self.0.retain(|id, _| {
            stations
                .binary_search_by(|s| s.id.as_str().cmp(id.as_str()))
                .is_ok()
        });
    }

    fn position(&mut self, station: &Station) -> [f32; 3] {
        if let Some(position) = self.0.get(&station.id) {
            return *position;
        }
        // At most 127 existing labels reserve cells; 192 fixed cells fit every
        // supported station even under adversarial bearings and title lengths.
        let position = (0..192)
            .map(crate::delegation::slot_position)
            .map(|p| p.to_array())
            .filter(|p| !self.0.values().any(|occupied| *occupied == *p))
            .min_by(|a, b| {
                glam::Vec3::from_array(*a)
                    .distance_squared(glam::Vec3::from_array(station.position))
                    .total_cmp(
                        &glam::Vec3::from_array(*b)
                            .distance_squared(glam::Vec3::from_array(station.position)),
                    )
            })
            .expect("192 distinct label cells fit the 128 station input bound");
        self.0.insert(station.id.clone(), position);
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
    }
}

fn short_ref(text: &str) -> String {
    clip(text, 16)
}

/// Character-based truncation is safe for every UTF-8 boundary. Unsupported
/// glyphs become '?' in the bitmap mesh, never a byte-indexing panic.
fn clip(text: &str, columns: usize) -> String {
    let mut chars = text.chars().filter(|c| !c.is_control());
    let mut out: String = chars.by_ref().take(columns).collect();
    if chars.next().is_some() && columns >= 3 {
        out = out.chars().take(columns - 3).collect();
        out.push_str("...");
    }
    out
}

/// The bitmap font only supports basic glyphs. Show other Unicode scalars and
/// control characters as explicit code-point escapes rather than losing them.
fn wrap(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut just_wrapped = false;
    for ch in text.chars() {
        if ch == '\n' {
            if !just_wrapped || !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            just_wrapped = false;
            continue;
        }
        let token = if ch.is_control() || BASIC_FONTS.get(ch).is_none() {
            format!("\\u{{{:X}}}", ch as u32)
        } else {
            ch.to_string()
        };
        // Never split an escape, so it remains identifiable across pages.
        if line.len() + token.len() > COLUMNS {
            lines.push(std::mem::take(&mut line));
        }
        line.push_str(&token);
        just_wrapped = false;
        if line.len() == COLUMNS {
            lines.push(std::mem::take(&mut line));
            just_wrapped = true;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn content_pages(card: &Presentation) -> Vec<Vec<String>> {
    let mut lines = Vec::new();
    match card.kind {
        Kind::Markdown => lines.extend(wrap(&card.text)),
        Kind::Table => {
            if card.columns.is_empty() {
                lines.push("[Empty table]".into());
            } else {
                // Tag every source line so a continuation page identifies the
                // cell even when a long value crosses a page boundary.
                let mut tagged = Vec::new();
                for (column, header) in card.columns.iter().enumerate() {
                    let label = format!("COLUMN {}/{} HEADER", column + 1, card.columns.len());
                    tagged.push((label.clone(), label.clone()));
                    tagged.extend(wrap(header).into_iter().map(|line| (label.clone(), line)));
                    for (row, values) in card.rows.iter().enumerate() {
                        let label =
                            format!("COLUMN {}  ROW {}/{}", column + 1, row + 1, card.rows.len());
                        tagged.push((label.clone(), label.clone()));
                        tagged.extend(
                            wrap(&values[column])
                                .into_iter()
                                .map(|line| (label.clone(), line)),
                        );
                    }
                }
                return tagged
                    .chunks(CONTENT_ROWS - 1)
                    .map(|chunk| {
                        let mut page = vec![format!("[{}]", chunk[0].0)];
                        page.extend(chunk.iter().map(|(_, line)| line.clone()));
                        page
                    })
                    .collect();
            }
        }
    }
    if lines.is_empty() {
        lines.push("[Empty content]".into());
    }
    lines
        .chunks(CONTENT_ROWS)
        .map(|page| page.to_vec())
        .collect()
}

fn quad(mesh: &mut Vec<Vertex>, x: f32, y: f32, z: f32, width: f32, height: f32, color: [f32; 4]) {
    if mesh.len() + 6 > MAX_VERTICES {
        return;
    }
    for [u, v] in [
        [0.0, 0.0],
        [1.0, 0.0],
        [1.0, -1.0],
        [0.0, 0.0],
        [1.0, -1.0],
        [0.0, -1.0],
    ] {
        mesh.push(Vertex {
            position: [x + u * width, y + v * height, z],
            color,
        });
    }
}

// Small, font-independent line symbols. Coordinates are local panel metres.
fn stroke(mesh: &mut Vec<Vertex>, a: [f32; 2], b: [f32; 2], width: f32, color: [f32; 4]) {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let length = dx.hypot(dy);
    if length <= f32::EPSILON || mesh.len() + 6 > MAX_VERTICES {
        return;
    }
    let [nx, ny] = [-dy * width / (2.0 * length), dx * width / (2.0 * length)];
    let points = [
        [a[0] + nx, a[1] + ny],
        [b[0] + nx, b[1] + ny],
        [b[0] - nx, b[1] - ny],
        [a[0] + nx, a[1] + ny],
        [b[0] - nx, b[1] - ny],
        [a[0] - nx, a[1] - ny],
    ];
    mesh.extend(points.map(|[x, y]| Vertex {
        position: [x, y, 0.001],
        color,
    }));
}

fn status_symbol(mesh: &mut Vec<Vertex>, state: State, p: f32, color: [f32; 4]) {
    // Right of the status word, with room for even "COMPLETED".
    let x = 14.0 * 9.0 * p;
    let y = -15.0 * p;
    let size = 5.0 * p;
    let mut line = |a: [f32; 2], b: [f32; 2]| {
        stroke(
            mesh,
            [x + a[0] * size, y + a[1] * size],
            [x + b[0] * size, y + b[1] * size],
            1.5 * p,
            color,
        );
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
        State::Unknown => {
            line([0.0, 0.95], [0.9, 0.0]);
            line([0.9, 0.0], [0.0, -0.95]);
            line([0.0, -0.95], [-0.9, 0.0]);
            line([-0.9, 0.0], [0.0, 0.95]);
        }
    }
}

fn panel_mesh(panel: &Panel, mesh: &mut Vec<Vertex>) {
    let start = mesh.len();
    // Work in panel-local coordinates, then rotate and gently bow the sign.
    let (x, y, z) = (0.0, 0.0, 0.0);
    let p = panel.pixel;
    let width = (panel.columns * 9 + 4) as f32 * p;
    let height = (panel.lines.len() * 11 + 4) as f32 * p;
    let color = |rgb: [f32; 3]| [rgb[0], rgb[1], rgb[2], panel.opacity];
    if panel.sign.is_some() {
        let mid = (panel.columns * 9) as f32 * p * 0.5;
        // A thin post reaches down towards the station without dominating it.
        quad(
            mesh,
            mid - 1.3 * p,
            y - height + 2.0 * p,
            z - 0.008,
            2.6 * p,
            0.29,
            color([0.16, 0.25, 0.3]),
        );
        // Strip tessellation tracks the shallow arc; foreground glyphs stay in
        // front by 4 mm even between strip vertices.
        const STRIPS: usize = 24;
        for i in 0..STRIPS {
            quad(
                mesh,
                x - p * 2.0 + width * i as f32 / STRIPS as f32,
                y + p * 2.0,
                z - 0.004,
                width / STRIPS as f32,
                height,
                color([0.012, 0.024, 0.045]),
            );
        }
    } else {
        quad(
            mesh,
            x - p * 2.0,
            y + p * 2.0,
            z - 0.003,
            width,
            height,
            color([0.012, 0.024, 0.045]),
        );
    }
    quad(
        mesh,
        x - p * 3.0,
        y + p * 2.0,
        z,
        p,
        height,
        color(panel.accent),
    );
    if let Some(sign) = &panel.sign {
        let symbol_start = mesh.len();
        status_symbol(mesh, sign.state, p, color(panel.accent));
        for vertex in &mut mesh[symbol_start..] {
            vertex.position[1] -= panel.lines.len().saturating_sub(2) as f32 * 11.0 * p;
        }
    }
    for (row, line) in panel.lines.iter().take(ROWS).enumerate() {
        for (column, ch) in line.chars().take(panel.columns).enumerate() {
            let glyph = BASIC_FONTS
                .get(ch)
                .or_else(|| BASIC_FONTS.get('?'))
                .unwrap();
            for (gy, bits) in glyph.iter().enumerate() {
                let mut gx = 0;
                while gx < 8 {
                    if bits & (1 << gx) == 0 {
                        gx += 1;
                        continue;
                    }
                    let start = gx;
                    while gx < 8 && bits & (1 << gx) != 0 {
                        gx += 1;
                    }
                    quad(
                        mesh,
                        x + (column * 9 + start) as f32 * p,
                        y - (row * 11 + gy) as f32 * p,
                        z,
                        (gx - start) as f32 * p,
                        p,
                        color(if row == 0 {
                            panel.accent
                        } else if row < 4 && panel.columns == COLUMNS {
                            MUTED
                        } else {
                            WHITE
                        }),
                    );
                }
            }
        }
    }
    for vertex in &mut mesh[start..] {
        let [x, y, z] = vertex.position;
        vertex.position = if let Some(sign) = &panel.sign {
            let mid = (panel.columns * 9) as f32 * p * 0.5;
            // Only 5 cm of depth across the roughly one-metre sign.
            let bow = 0.05 * ((x - mid) / mid).powi(2);
            [
                panel.origin[0] + sign.right[0] * x + sign.normal[0] * (z + bow),
                panel.origin[1] + y,
                panel.origin[2] + sign.right[2] * x + sign.normal[2] * (z + bow),
            ]
        } else {
            [
                panel.origin[0] + x,
                panel.origin[1] + y,
                panel.origin[2] + z,
            ]
        };
    }
    if let Some(sign) = &panel.sign {
        let center = panel.center();
        let station = glam::Vec3::from_array(sign.station);
        // A displaced legacy label keeps a thin, literal station association;
        // this is a signpost, not an inferred worker communication edge.
        if (center.x - station.x).hypot(center.z - station.z) > 0.2 {
            for (index, side) in [(2, 1.0), (4, 1.0), (5, -1.0)] {
                mesh[start + index].position = (station
                    + glam::Vec3::Y * 0.18
                    + glam::Vec3::from_array(sign.right) * (side * 1.3 * p))
                    .to_array();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation::{Node, Scene};

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
        )
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
    fn every_supported_station_has_a_complete_unique_unclipped_label() {
        for count in [16, 17, 64, 128] {
            for legacy in [false, true] {
                let stations: Vec<_> = (0..count)
                    .map(|i| Station {
                        id: format!("station-{i:03}"),
                        title: format!("{i:03}{}", "W".repeat(157)),
                        position: if legacy {
                            [0.0, 1.4, -2.9]
                        } else {
                            crate::delegation::slot_position(i).to_array()
                        },
                        state: State::Completed,
                        opacity: 1.0,
                    })
                    .collect();
                let mut placements = LabelPlacement::default();
                let (panels, _) =
                    layout(&Workspace::default(), &stations, 0, 0, &[], &mut placements);
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
                    assert_eq!(panel.lines.last().unwrap(), "UNKNOWN");
                    assert_eq!(panel.sign.as_ref().unwrap().station, stations[i].position);
                    assert!(
                        panels[i + 1..].iter().all(|other| !overlaps(panel, other)),
                        "overlap: count={count}, legacy={legacy}, source={i}"
                    );
                }
                let mut cache = PageCache::default();
                cache.update(panels.clone());
                assert_eq!(cache.ranges.len(), count);
                assert_eq!(
                    cache.vertices.len(),
                    cache
                        .meshes
                        .iter()
                        .map(|(_, mesh)| mesh.len())
                        .sum::<usize>()
                );
                assert_eq!(
                    cache.ranges.last().unwrap().vertices.end as usize,
                    cache.vertices.len()
                );
                assert!(cache.vertices.len() <= MAX_VERTICES);
                let before: std::collections::BTreeMap<_, _> = panels
                    .iter()
                    .map(|p| (p.sign.as_ref().unwrap().source_id.clone(), p.origin))
                    .collect();
                let mut edited = stations.clone();
                edited.reverse();
                for s in &mut edited {
                    s.title = "Edited".into();
                    s.state = State::Error;
                    s.opacity = 0.5;
                }
                let (next, _) = layout(&Workspace::default(), &edited, 0, 0, &[], &mut placements);
                for p in &next {
                    assert_eq!(p.origin, before[&p.sign.as_ref().unwrap().source_id]);
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
                        id: i.to_string(),
                        title: "W".repeat(160),
                        position,
                        state: State::Unknown,
                        opacity: 1.0,
                    },
                    position,
                    State::Unknown,
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
    fn collision_placement_is_bounded_stable_on_membership_edits_and_hostile_bearings() {
        for position in [
            [0.0, 1.7, 0.0],
            [0.0001, 1.6, -0.0001],
            [100.0, -100.0, 100.0],
        ] {
            let mut stations: Vec<_> = (0..128)
                .map(|i| Station {
                    id: format!("w-{i:03}"),
                    title: "A title".into(),
                    state: State::Running,
                    opacity: 1.0,
                    position,
                })
                .collect();
            let mut placements = LabelPlacement::default();
            let (panels, _) = layout(&Workspace::default(), &stations, 0, 0, &[], &mut placements);
            assert_eq!(panels.len(), 128);
            assert_eq!(placements.0.len(), 128);
            stations.remove(0);
            let mut new = stations[0].clone();
            new.id = "a-new-first-id".into();
            stations.push(new);
            let before = placements.0.clone();
            layout(&Workspace::default(), &stations, 0, 0, &[], &mut placements);
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
        let labels: Vec<_> = panels.iter().filter(|p| p.columns == 28).collect();
        assert_eq!(labels.len(), 64);
        for (i, label) in labels.iter().enumerate() {
            assert_eq!(label.lines[1], "COMPLETED");
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
                .filter(|p| p.columns == 28)
                .all(|p| p.lines[1] == "COMPLETED")
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
                    .filter(|p| p.columns == 28)
                    .all(|p| p.lines[1] == "UNKNOWN")
            );
            assert!(panels.iter().any(|p| p.columns == 28));
        }
        assert!(test_layout(&Workspace::default(), &[], 0, 0).0.is_empty());
    }

    #[test]
    fn table_all_cells_and_headers_are_reachable_once() {
        let mut table = card("table");
        table.kind = Kind::Table;
        table.columns = (0..6)
            .map(|i| format!("HEADER{i}{}", "h".repeat(73)))
            .collect();
        table.rows = (0..16)
            .map(|i| {
                (0..6)
                    .map(|j| format!("{i:02}:{j}:{}", "v".repeat(155)))
                    .collect()
            })
            .collect();
        let pages = content_pages(&table);
        assert!(pages.len() > 10);
        assert!(pages.iter().all(
            |page| page.len() <= CONTENT_ROWS && page.iter().all(|line| line.len() <= COLUMNS)
        ));
        let lines: Vec<_> = pages
            .iter()
            .flat_map(|page| page.iter().skip(1))
            .cloned()
            .collect();
        let mut expected = Vec::new();
        for (c, header) in table.columns.iter().enumerate() {
            expected.push(format!("COLUMN {}/6 HEADER", c + 1));
            expected.extend(wrap(header));
            for (r, values) in table.rows.iter().enumerate() {
                expected.push(format!("COLUMN {}  ROW {}/16", c + 1, r + 1));
                expected.extend(wrap(&values[c]));
            }
        }
        assert_eq!(lines, expected);
        for c in 0..6 {
            for r in 0..16 {
                let label = format!("COLUMN {}  ROW {}/16", c + 1, r + 1);
                let start = lines.iter().position(|line| line == &label).unwrap() + 1;
                let count = wrap(&table.rows[r][c]).len();
                assert_eq!(lines[start..start + count].concat(), table.rows[r][c]);
            }
        }
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
            maximum_characters * 8 * 4 * 6 + MAX_LABELS * 30 * 6 + (CARDS_PER_PAGE + 1) * 2 * 6;
        assert!(maximum_vertices <= MAX_VERTICES);
    }

    #[test]
    fn maximum_markdown_and_unicode_are_reachable_without_clipping() {
        let text = (0..4096)
            .map(|i| (b'!' + (i % 90) as u8) as char)
            .collect::<String>();
        let card = Presentation {
            text: text.clone(),
            ..Default::default()
        };
        let pages = content_pages(&card);
        assert_eq!(pages.iter().flatten().cloned().collect::<String>(), text);
        assert!(
            pages
                .iter()
                .all(|p| p.len() <= CONTENT_ROWS && p.iter().all(|s| s.len() <= COLUMNS))
        );
        assert_eq!(
            wrap("a🦀é中\t\rb").concat(),
            "a\\u{1F980}\\u{E9}\\u{4E2D}\\u{9}\\u{D}b"
        );
        let unicode = "🦀".repeat(1024);
        let unicode_card = Presentation {
            text: unicode,
            ..Default::default()
        };
        let displayed = content_pages(&unicode_card)
            .iter()
            .flatten()
            .cloned()
            .collect::<String>();
        assert_eq!(displayed, "\\u{1F980}".repeat(1024));
        assert_eq!(wrap(&"x\n".repeat(2048)).len(), 2048);
        assert_eq!(clip("é中🦀xyz", 4), "é...");
        assert!(wrap("").is_empty());
    }

    #[test]
    fn stick_navigation_wraps_latches_clamps_and_resets_on_root_change() {
        let mut a = card("a");
        a.text = "x".repeat(4096);
        let mut b = card("b");
        b.text = "y".repeat(4096);
        let mut workspace = Workspace {
            presentations: vec![b, a],
            ..Default::default()
        };
        let mut nav = CardNavigation::default();
        nav.update(Some("root"), &workspace, 0.0, 0.0);
        let pages = nav.cache.pages.len();
        assert!(pages > 1);
        nav.update(Some("root"), &workspace, 0.0, -1.0);
        assert_eq!(nav.content, 1);
        nav.update(Some("root"), &workspace, 0.0, -1.0);
        assert_eq!(nav.content, 1, "holding does not repeat");
        nav.release();
        nav.update(Some("root"), &workspace, 0.0, 1.0);
        assert_eq!(nav.content, 0);
        nav.release();
        nav.update(Some("root"), &workspace, 0.0, 1.0);
        assert_eq!(nav.content, pages - 1);
        nav.release();
        nav.update(Some("root"), &workspace, 1.0, 0.7);
        assert_eq!((nav.card, nav.content), (1, 0));
        nav.release();
        nav.update(Some("root"), &workspace, -1.0, 0.0);
        assert_eq!(nav.card, 0);
        nav.release();
        nav.update(Some("root"), &workspace, -1.0, 0.0);
        assert_eq!(nav.card, 1);
        workspace.presentations.truncate(1);
        nav.update(Some("root"), &workspace, 0.0, 0.0);
        assert_eq!(nav.card, 0);
        nav.content = usize::MAX;
        workspace.presentations[0].text = "short".into();
        nav.update(Some("root"), &workspace, 0.0, 0.0);
        assert_eq!(nav.content, 0);
        nav.card = 10;
        nav.content = 10;
        nav.update(Some("other"), &workspace, 0.0, -1.0);
        assert_eq!((nav.card, nav.content), (0, 0));
        nav.update(Some("other"), &workspace, 1.0, 0.0);
        assert_eq!((nav.card, nav.content), (0, 0));
    }

    #[test]
    fn opacity_blends_all_label_geometry_and_invalid_stations_are_hidden() {
        let workspace = Workspace {
            nodes: vec![node("a", State::Running)],
            ..Default::default()
        };
        let station = Station {
            id: "a".into(),
            title: "Task".into(),
            state: State::Running,
            position: [1.0, 1.0, -2.0],
            opacity: 1.0,
        };
        let full = test_layout(&workspace, std::slice::from_ref(&station), 0, 0)
            .0
            .remove(0);
        let half = test_layout(
            &workspace,
            &[Station {
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
    fn malformed_content_is_rejected_without_allocating_unbounded_pages() {
        let mut card = card("bad");
        card.text = "x".repeat(4097);
        let mut cache = ContentCache::default();
        cache.update(Some(&card));
        assert_eq!(cache.pages, vec![vec!["[INVALID CONTENT]".to_string()]]);
        card.kind = Kind::Table;
        card.text.clear();
        card.columns = vec!["a".into()];
        card.rows = vec![vec![]];
        cache.update(Some(&card));
        assert_eq!(cache.pages, vec![vec!["[INVALID CONTENT]".to_string()]]);
    }

    #[test]
    fn maximum_multibyte_table_cells_survive_wrapping_and_pagination() {
        let mut table = card("unicode");
        table.kind = Kind::Table;
        table.columns = (0..6).map(|_| "é".repeat(40)).collect();
        table.rows = (0..16)
            .map(|_| (0..6).map(|_| "🦀".repeat(40)).collect())
            .collect();
        let pages = content_pages(&table);
        let lines: Vec<_> = pages
            .iter()
            .flat_map(|page| page.iter().skip(1))
            .cloned()
            .collect();
        for column in 0..6 {
            for row in 0..16 {
                let marker = format!("COLUMN {}  ROW {}/16", column + 1, row + 1);
                let start = lines.iter().position(|line| line == &marker).unwrap() + 1;
                let n = wrap(&table.rows[row][column]).len();
                assert_eq!(lines[start..start + n].concat(), "\\u{1F980}".repeat(40));
            }
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
        let panel = labels.iter().find(|p| p.columns == 28).unwrap();
        assert!(arriving.opacity > 0.0 && arriving.opacity < 1.0);
        assert_eq!(panel.opacity, arriving.opacity);
        assert_eq!(panel.lines[1], "COMPLETED");
        for _ in 0..40 {
            scene.update(Some("root"), &workspace, 0.05);
        }
        scene.update(Some("root"), &Workspace::default(), 0.05);
        let departing = &scene.stations()[0];
        let labels = test_layout(&Workspace::default(), scene.stations(), 0, 0).0;
        let panel = labels.iter().find(|p| p.columns == 28).unwrap();
        assert!(departing.opacity < 1.0);
        assert_eq!(panel.opacity, departing.opacity);
        assert_eq!(panel.lines[1], "UNKNOWN");
    }

    fn test_sign(position: [f32; 3], state: State) -> Panel {
        let station = Station {
            id: "a".into(),
            title: "Readable task".into(),
            position,
            state,
            opacity: 1.0,
        };
        station_panel(&station, position, state)
    }

    #[test]
    fn symbols_are_distinct_geometry_and_follow_observed_state() {
        let states = [
            State::Completed,
            State::Error,
            State::Waiting,
            State::Running,
            State::Idle,
            State::Unknown,
        ];
        let mut shapes = Vec::new();
        for state in states {
            let panel = test_sign([0.0, 1.4, -2.7], state);
            assert_eq!(panel.lines, ["Readable task", state_label(&state).0]);
            assert_eq!(panel.sign.as_ref().unwrap().state, state);
            let mut symbol = Vec::new();
            status_symbol(&mut symbol, state, panel.pixel, [1.0; 4]);
            assert!(!symbol.is_empty());
            let positions: Vec<_> = symbol.iter().map(|v| v.position).collect();
            assert!(
                !shapes.contains(&positions),
                "duplicate shape for {state:?}"
            );
            shapes.push(positions);
            let mut mesh = Vec::new();
            panel_mesh(&panel, &mut mesh);
            // Text runs still exist beyond the post, background and symbol.
            assert!(mesh.len() > (24 + 2 + symbol.len() / 6) * 6);
        }
        let workspace = Workspace {
            nodes: vec![node("a", State::Error)],
            ..Default::default()
        };
        let old_snapshot = Station {
            id: "a".into(),
            title: "old".into(),
            state: State::Completed,
            position: [0.0, 1.4, -3.0],
            opacity: 1.0,
        };
        let panel = test_layout(&workspace, &[old_snapshot], 0, 0).0.remove(0);
        assert_eq!(panel.lines[1], "ERROR");
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
            // First quad is the post, extending well below the sign.
            let post_bottom = mesh[..6]
                .iter()
                .map(|v| v.position[1])
                .fold(f32::INFINITY, f32::min);
            assert!(post_bottom < panel.origin[1] - 0.35);
            // Both background edges bow towards the observer, with text in front.
            let mid = 12 * 6;
            let distance = |v: &Vertex| {
                (v.position[0] - panel.origin[0]) * nx + (v.position[2] - panel.origin[2]) * nz
            };
            assert!(distance(&mesh[6]) > distance(&mesh[6 + mid]) + 0.03);
        }
        for bad in [f32::NAN, f32::INFINITY, -f32::INFINITY, 101.0] {
            let workspace = Workspace {
                nodes: vec![node("a", State::Running)],
                ..Default::default()
            };
            let station = Station {
                id: "a".into(),
                title: "bad".into(),
                state: State::Running,
                position: [bad, 1.4, -2.0],
                opacity: 1.0,
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
        let background = &mesh[6..(1 + 24) * 6];
        // Check the foreground edges and their midpoints against the straight
        // strip surfaces, not just against the ideal quadratic bow.
        for quad in mesh[(1 + 24 + 1) * 6..].as_chunks::<6>().0 {
            for (a, b) in [(0, 0), (1, 1), (2, 2), (0, 1), (1, 2)] {
                let (xa, za) = local(&quad[a]);
                let (xb, zb) = local(&quad[b]);
                let (x, z) = ((xa + xb) * 0.5, (za + zb) * 0.5);
                for strip in background.as_chunks::<6>().0 {
                    let (left, zl) = local(&strip[0]);
                    let (right, zr) = local(&strip[1]);
                    if x >= left && x <= right {
                        let behind = zl + (zr - zl) * (x - left) / (right - left);
                        assert!(
                            z - behind > 0.0037,
                            "foreground is too close to the background"
                        );
                        break;
                    }
                }
            }
        }
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
            let sign_len = cache.meshes[0].1.len();
            assert!(
                cache.vertices[..sign_len]
                    .iter()
                    .all(|v| v.color[3] == opacity)
            );
            assert_eq!(
                bytemuck::cast_slice::<Vertex, u8>(&cache.vertices[sign_len..]),
                bytemuck::cast_slice::<Vertex, u8>(&original[sign_len..])
            );
        }
    }

    #[test]
    fn overlap_order_uses_visible_surfaces_not_centers_and_changes_with_view() {
        let projection = glam::Mat4::perspective_rh(0.95, 1.0, 0.05, 50.0);
        let sign = test_sign([0.0, 1.25, -2.8], State::Completed);
        let mut rear = Panel {
            origin: [-0.2, 2.0, -2.76],
            pixel: 0.0051,
            columns: COLUMNS,
            lines: vec![String::new(); ROWS],
            accent: MUTED,
            opacity: 1.0,
            sign: None,
        };
        for yaw in [-0.3_f32, 0.3] {
            rear.origin[0] = if yaw < 0.0 { -0.2 } else { -1.84 };
            let mut cache = PageCache::default();
            cache.update(vec![sign.clone(), rear.clone()]);
            for x in [-0.032, 0.032] {
                let eye = glam::Vec3::new(x, 1.6, 0.0);
                let direction = glam::Vec3::new(yaw.sin(), 0.0, -yaw.cos());
                let matrix =
                    projection * glam::Mat4::look_at_rh(eye, eye + direction, glam::Vec3::Y);
                let depth = |p: &Panel| {
                    let clip = matrix * p.center().extend(1.0);
                    clip.z / clip.w
                };
                assert!(
                    depth(&rear) < depth(&sign),
                    "yaw={yaw}, eye={x}, rear={}, sign={}",
                    depth(&rear),
                    depth(&sign)
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
                * glam::Mat4::look_at_rh(
                    glam::Vec3::new(0.0, 1.6, z),
                    glam::Vec3::new(0.0, 1.6, -3.0),
                    glam::Vec3::Y,
                );
            assert_eq!(panel_order(&cache.ranges, matrix), expected);
        }
    }

    #[test]
    fn surface_projection_clips_near_plane_and_marks_true_intersections_ambiguous() {
        let matrix = glam::Mat4::perspective_rh(1.0, 1.0, 0.05, 50.0);
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
    fn legacy_snapshot_and_stereo_recenter_keep_world_signs() {
        let station: Station = serde_json::from_str(
            r#"{"id":"a","title":"Old task","state":"idle","position":[-1.0,1.4,-2.8]}"#,
        )
        .unwrap();
        assert_eq!(station.opacity, 1.0);
        let workspace = Workspace {
            nodes: vec![node("a", State::Idle)],
            ..Default::default()
        };
        let panel = test_layout(&workspace, &[station], 0, 0).0.remove(0);
        let mut mesh = Vec::new();
        panel_mesh(&panel, &mut mesh);
        let projection = glam::Mat4::perspective_rh(1.0, 1.0, 0.05, 50.0);
        let target = glam::Vec3::new(-1.0, 1.75, -2.8);
        let recenter = glam::Mat4::from_rotation_y(0.7)
            * glam::Mat4::from_translation(glam::Vec3::new(0.5, 0.0, -0.3));
        let mut projected = Vec::new();
        for eye_x in [-0.032, 0.032] {
            let eye = glam::Vec3::new(eye_x, 1.6, 0.0);
            let view = projection * glam::Mat4::look_at_rh(eye, target, glam::Vec3::Y);
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

    #[test]
    #[ignore = "requires a Vulkan adapter"]
    fn vulkan_sign_images_include_post_text_and_smooth_fade_in_both_eyes() {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let adapter = instance.request_adapter(&Default::default()).await.unwrap();
            let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
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
            let camera = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
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
            let mut graphics =
                WorkspaceGraphics::new(&device, wgpu::TextureFormat::Rgba8Unorm, &camera_layout);
            // A depth-writing opaque body in front of part of the sign.
            let shader = device.create_shader_module(wgpu::include_wgsl!("workspace.wgsl"));
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("synthetic body layout"),
                bind_group_layouts: &[Some(&camera_layout)],
                immediate_size: 0,
            });
            let body_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                id: "a".into(),
                title: "Readable task".into(),
                state: State::Completed,
                position: [0.0, 1.25, -2.8],
                opacity: 1.0,
            };
            // Rendering fixture deliberately exercises a centered arbitrary
            // plane. Real placement is tested separately with 128 labels below.
            graphics
                .placements
                .0
                .insert(station.id.clone(), station.position);
            let capture_order = |graphics: &WorkspaceGraphics,
                                 matrix: glam::Mat4,
                                 body: bool,
                                 sorted: bool| {
                queue.write_buffer(&camera, 0, bytemuck::cast_slice(&matrix.to_cols_array()));
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
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &depth_view,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(1.0),
                                store: wgpu::StoreOp::Discard,
                            }),
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    if body {
                        pass.set_pipeline(&body_pipeline);
                        pass.set_bind_group(0, &binding, &[]);
                        pass.set_vertex_buffer(0, body_buffer.slice(..));
                        pass.draw(0..6, 0..1);
                    }
                    if sorted {
                        graphics.draw(&mut pass, &binding, matrix);
                    } else if graphics.count > 0 {
                        // Reproduce the old single blended draw in the test only.
                        pass.set_pipeline(&graphics.pipeline);
                        pass.set_bind_group(0, &binding, &[]);
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
            let projection = glam::Mat4::perspective_rh(0.65, 1.0, 0.05, 50.0);
            let mut eye_images = Vec::new();
            for eye_x in [-0.032, 0.032] {
                let eye = glam::Vec3::new(eye_x, 1.6, 0.0);
                let matrix = projection
                    * glam::Mat4::look_at_rh(eye, eye + glam::Vec3::NEG_Z, glam::Vec3::Y);
                graphics.hide();
                let empty = capture(&graphics, matrix, false);
                station.opacity = 1.0;
                graphics.update(&device, &queue, &workspace, std::slice::from_ref(&station));
                let full = capture(&graphics, matrix, false);
                // A farther result field fills the label's projected area. In
                // particular, it must not paint over the foreground glyphs.
                let sign = station_panel(&station, station.position, State::Completed);
                let rear = Panel {
                    origin: [-2.0, 2.5, -4.0],
                    pixel: 0.01,
                    columns: COLUMNS,
                    lines: vec![String::new(); ROWS],
                    accent: MUTED,
                    opacity: 1.0,
                    sign: None,
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
                graphics.update(&device, &queue, &workspace, std::slice::from_ref(&station));
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
                graphics.update(&device, &queue, &workspace, std::slice::from_ref(&station));
                let compensated =
                    capture(&graphics, (matrix * transform.inverse()) * transform, false);
                assert!(
                    full.as_chunks::<4>()
                        .0
                        .iter()
                        .zip(compensated.as_chunks::<4>().0.iter())
                        .filter(|(a, b)| a != b)
                        .count()
                        < 256
                );
                eye_images.push(full);
            }
            assert_ne!(eye_images[0], eye_images[1]);

            // Exercise the real panel meshes and depth attachment at multiple
            // distances and bearings, in both insertion orders and both eyes.
            // The rear field contains no glyphs, making foreground glyph pixels
            // unambiguous. Images use synthetic content only.
            let upload = |graphics: &mut WorkspaceGraphics, panels: Vec<Panel>| {
                graphics.cache.update(panels);
                if !graphics.cache.vertices.is_empty() {
                    queue.write_buffer(
                        &graphics.vertices,
                        0,
                        bytemuck::cast_slice(&graphics.cache.vertices),
                    );
                }
                graphics.count = graphics.cache.vertices.len() as u32;
            };
            let projection = glam::Mat4::perspective_rh(0.95, 1.0, 0.05, 50.0);
            for distance in [1.8, 2.8, 4.0] {
                for bearing in [-0.45_f32, 0.0, 0.45] {
                    let mut sign_station = station.clone();
                    sign_station.opacity = 1.0;
                    sign_station.position[0] = distance * bearing.sin();
                    sign_station.position[2] = -distance * bearing.cos();
                    let sign =
                        station_panel(&sign_station, sign_station.position, State::Completed);
                    let rear = Panel {
                        origin: [-3.0, 3.2, -distance - 1.5],
                        pixel: 0.016,
                        columns: COLUMNS,
                        lines: vec![String::new(); ROWS],
                        accent: MUTED,
                        opacity: 1.0,
                        sign: None,
                    };
                    for yaw in [-0.18_f32, 0.18] {
                        for eye_x in [-0.032, 0.032] {
                            let eye = glam::Vec3::new(0.15 + eye_x, 1.6, 0.1);
                            let heading = bearing + yaw;
                            let direction = glam::Vec3::new(heading.sin(), 0.0, -heading.cos());
                            let matrix = projection
                                * glam::Mat4::look_at_rh(eye, eye + direction, glam::Vec3::Y);
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
                                .filter(|&i| {
                                    sign_image[i] > 50
                                        && sign_image[i + 1] > 180
                                        && sign_image[i + 2] > 120
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
                            let changed = forward
                                .as_chunks::<4>()
                                .0
                                .iter()
                                .zip(recentered.as_chunks::<4>().0)
                                .filter(|(a, b)| a != b)
                                .count();
                            assert!(changed < 128, "recenter changed overlap: {changed} pixels");
                        }
                    }
                }
            }

            // A small real sign/card depth gap and an off-axis card center make
            // center sorting wrong, although these surfaces never intersect.
            // Mirror the case so both yaw signs and both eye positions matter.
            let mut overlap_previews = Vec::new();
            let angled_projection = glam::Mat4::perspective_rh(1.1, 1.0, 0.05, 50.0);
            for yaw in [-0.3_f32, 0.3] {
                let sign = test_sign([0.0, 1.25, -2.8], State::Completed);
                let rear = Panel {
                    origin: [if yaw < 0.0 { -0.2 } else { -1.84 }, 2.0, -2.76],
                    pixel: 0.0051,
                    columns: COLUMNS,
                    lines: vec![String::new(); ROWS],
                    accent: MUTED,
                    opacity: 1.0,
                    sign: None,
                };
                for eye_x in [-0.032, 0.032] {
                    let eye = glam::Vec3::new(eye_x, 1.6, 0.0);
                    let direction = glam::Vec3::new(yaw.sin(), 0.0, -yaw.cos());
                    let matrix = angled_projection
                        * glam::Mat4::look_at_rh(eye, eye + direction, glam::Vec3::Y);
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
                        .filter(|&i| {
                            sign_image[i] > 50 && sign_image[i + 1] > 180 && sign_image[i + 2] > 120
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
            };
            for eye_x in [-0.032, 0.032] {
                let eye = glam::Vec3::new(eye_x, 1.6, 0.0);
                let matrix = projection
                    * glam::Mat4::look_at_rh(eye, eye + glam::Vec3::NEG_Z, glam::Vec3::Y);
                upload(&mut graphics, vec![sign.clone()]);
                let sign_image = capture(&graphics, matrix, false);
                graphics.hide();
                let body_image = capture(&graphics, matrix, true);
                upload(&mut graphics, vec![sign.clone(), rear.clone()]);
                let result = capture(&graphics, matrix, true);
                let mut blocked = 0;
                let mut visible = 0;
                for i in (0..sign_image.len()).step_by(4) {
                    if sign_image[i] > 50 && sign_image[i + 1] > 180 && sign_image[i + 2] > 120 {
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
            if let Ok(path) = std::env::var("TOMORI_SIGN_PREVIEW") {
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
                sheet.save(path).unwrap();
            }

            // Exercise the production placement, complete mesh upload and GPU
            // buffer growth with every supported label. Aim at each actual
            // sign in turn; this is world navigation, not an overlay bypass.
            let stations: Vec<_> = (0..128)
                .map(|i| Station {
                    id: format!("density-{i:03}"),
                    title: format!("{i:03} {}", "READABLE ".repeat(18))
                        .chars()
                        .take(160)
                        .collect(),
                    position: crate::delegation::slot_position(i).to_array(),
                    state: State::Unknown,
                    opacity: 1.0,
                })
                .collect();
            graphics.update(&device, &queue, &Workspace::default(), &stations);
            assert_eq!(graphics.cache.ranges.len(), 128);
            assert_eq!(
                graphics.cache.vertices.len(),
                graphics
                    .cache
                    .meshes
                    .iter()
                    .map(|(_, mesh)| mesh.len())
                    .sum::<usize>()
            );
            assert!(graphics.vertex_capacity >= graphics.cache.vertices.len());
            assert!(graphics.vertex_capacity < MAX_VERTICES);
            eprintln!(
                "128 full labels: {} vertices, {} GPU bytes ({} byte hard ceiling)",
                graphics.cache.vertices.len(),
                graphics.vertex_capacity * size_of::<Vertex>(),
                MAX_VERTICES * size_of::<Vertex>()
            );
            let preview_path = std::env::var("TOMORI_DENSITY_PREVIEW").ok();
            let mut sheet = preview_path
                .as_ref()
                .map(|_| image::RgbaImage::new(4096, 2048));
            for (i, panel) in graphics.cache.panels.iter().enumerate() {
                let eye = glam::Vec3::new(if i % 2 == 0 { -0.032 } else { 0.032 }, 1.6, 0.0);
                let matrix =
                    projection * glam::Mat4::look_at_rh(eye, panel.center(), glam::Vec3::Y);
                let image = capture(&graphics, matrix, false);
                let glyphs = (245..270)
                    .flat_map(|y| (180..330).map(move |x| (y * 512 + x) * 4))
                    .filter(|&p| image[p] > 200 && image[p + 1] > 200 && image[p + 2] > 200)
                    .count();
                assert!(glyphs > 4, "source {i} has no readable center glyphs");
                if let Some(sheet) = &mut sheet {
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
            if let (Some(sheet), Some(path)) = (sheet, preview_path) {
                sheet.save(path).unwrap();
            }
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            assert!(scope.pop().await.is_none());
        });
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
}
