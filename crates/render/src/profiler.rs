// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! What each frame costs the GPU, summarized once a minute beside the frame thread's own costs: each eye's pass timed
//! by the GPU's timestamps, where its device has them, and, where marks within a pass mean something, as on a desktop
//! GPU rather than a tiled one, each layer of it, the scenery's opaque bodies, Tomori's, the scenery's translucent
//! surfaces, the panels and signs, and the labels; with how much was drawn and culled a frame. See
//! `design/rendering.md`.
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tomori_assets::bounds::Tally;

/// The features a device is asked for, where its adapter has them, for the profiler to time the GPU with.
pub const FEATURES: wgpu::Features =
    wgpu::Features::TIMESTAMP_QUERY.union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES);

/// The layers of an eye's pass, as marks within it divide it, in the order they are drawn.
pub const LAYERS: [&str; 5] = ["scenery", "tomori", "translucent", "panels", "labels"];

/// Timestamps written for each eye where layers are marked: its pass's start, a mark after each layer but the last,
/// and its end; where they are not, only its start and its end, so that every query resolved was written, as Vulkan
/// requires of results waited for.
const MARKS: u32 = LAYERS.len() as u32 + 1;

/// How many frames' timestamps may be on their way back from the GPU at once; a frame finding none free is not timed.
const IN_FLIGHT: usize = 4;

/// The bytes between one frame's resolved timestamps and the next's, as resolving a query set requires.
const STRIDE: u64 = wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT;

/// A frame's timestamps' way back: free, written by the GPU and being mapped, or mapped and ready to be read.
const FREE: u8 = 0;
const MAPPING: u8 = 1;
const MAPPED: u8 = 2;

/// One frame's timestamps read back.
struct Readback {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    eyes: usize,
}

impl Readback {
    /// The bytes of its frame's timestamps, `marks` an eye.
    fn size(&self, marks: u32) -> u64 {
        u64::from(self.eyes as u32 * marks) * 8
    }
}

/// The GPU's timestamps, where the device has them.
struct Queries {
    set: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    readbacks: Vec<Readback>,
    /// Nanoseconds a tick.
    period: f32,
    /// Whether layers are marked within each eye's pass.
    within: bool,
    /// The timestamps written for each eye: [`MARKS`] where layers are marked, or its pass's start and end.
    marks: u32,
}

impl Queries {
    /// The first query `eye` of `frame` writes.
    fn first(&self, frame: Frame, eye: usize) -> u32 {
        (frame.slot as u32 * 2 + eye as u32) * self.marks
    }
}

/// One frame as the GPU drew it: each eye's pass, and each layer, both eyes', in milliseconds, if marked.
#[derive(Clone, Debug, Default, PartialEq)]
struct Timed {
    eyes: Vec<f64>,
    layers: Option<[f64; LAYERS.len()]>,
}

/// The frame being drawn: which readback its timestamps go to, the first of its queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    slot: usize,
}

/// Times each eye's pass on the GPU, and keeps what was drawn a frame, for a summary once a minute.
pub struct Profiler {
    queries: Option<Queries>,
    timed: Vec<Timed>,
    tallies: Vec<Tally>,
    /// How many views each pass draws: where one draws both eyes, there is no time for each.
    views: crate::multiview::Views,
}

impl Profiler {
    /// A profiler for `device`, timing with its timestamps if it has them, as its features say, and marking each
    /// layer within a pass, `within`, if it can, which only means something on a GPU that draws each draw as it comes
    /// rather than in tiles. Without them, it keeps only what was drawn.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, within: bool) -> Self {
        let features = device.features();
        if !features.contains(wgpu::Features::TIMESTAMP_QUERY) {
            return Self::untimed();
        }
        let within = within && features.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES);
        let marks = if within { MARKS } else { 2 };
        let queries = Some({
            let count = (IN_FLIGHT as u32) * 2 * marks;
            let set = device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("Frame profiler"),
                ty: wgpu::QueryType::Timestamp,
                count,
            });
            let resolved = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Frame profiler resolved"),
                size: STRIDE * IN_FLIGHT as u64,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let readbacks = (0..IN_FLIGHT)
                .map(|_| Readback {
                    buffer: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("Frame profiler readback"),
                        size: u64::from(2 * marks) * 8,
                        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    state: Arc::new(AtomicU8::new(FREE)),
                    eyes: 0,
                })
                .collect();
            Queries {
                set,
                resolved,
                readbacks,
                period: queue.get_timestamp_period(),
                within,
                marks,
            }
        });
        Self {
            queries,
            timed: Vec::new(),
            tallies: Vec::new(),
            views: crate::multiview::Views::One,
        }
    }

    /// Each pass draws `views`: with both, one pass draws both eyes, and only its time is said, as the eyes'.
    pub fn set_views(&mut self, views: crate::multiview::Views) {
        self.views = views;
    }

    /// A profiler that keeps only what was drawn, for a device without timestamps.
    pub fn untimed() -> Self {
        Self {
            queries: None,
            timed: Vec::new(),
            tallies: Vec::new(),
            views: crate::multiview::Views::One,
        }
    }

    /// Whether it times the GPU.
    pub fn times(&self) -> bool {
        self.queries.is_some()
    }

    /// Read the timestamps of frames the GPU has finished, then start a frame of `eyes` eyes, timed if a readback is
    /// free for it.
    pub fn begin(&mut self, eyes: usize) -> Option<Frame> {
        let queries = self.queries.as_mut()?;
        for readback in &mut queries.readbacks {
            if readback.state.load(Ordering::Acquire) != MAPPED {
                continue;
            }
            let ticks: Vec<u64> = {
                let data = readback
                    .buffer
                    .slice(..readback.size(queries.marks))
                    .get_mapped_range()
                    .expect("a mapped readback reads");
                bytemuck::cast_slice(&data).to_vec()
            };
            readback.buffer.unmap();
            readback.state.store(FREE, Ordering::Release);
            let period = f64::from(queries.period);
            let between = |from: usize, to: usize| {
                ticks[to].saturating_sub(ticks[from]) as f64 * period / 1e6
            };
            let marks = queries.marks as usize;
            let eyes = (0..readback.eyes)
                .map(|eye| between(eye * marks, eye * marks + marks - 1))
                .collect();
            let layers = queries.within.then(|| {
                std::array::from_fn(|layer| {
                    (0..readback.eyes)
                        .map(|eye| between(eye * marks + layer, eye * marks + layer + 1))
                        .sum()
                })
            });
            self.timed.push(Timed { eyes, layers });
        }
        let slot = queries
            .readbacks
            .iter()
            .position(|readback| readback.state.load(Ordering::Acquire) == FREE)?;
        let readback = &mut queries.readbacks[slot];
        readback.eyes = eyes;
        readback.state.store(MAPPING, Ordering::Release);
        Some(Frame { slot })
    }

    /// The timestamps `eye`'s pass of `frame` writes at its start and end.
    pub fn pass(
        &self,
        frame: Option<Frame>,
        eye: usize,
    ) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let (queries, frame) = (self.queries.as_ref()?, frame?);
        let first = queries.first(frame, eye);
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &queries.set,
            beginning_of_pass_write_index: Some(first),
            end_of_pass_write_index: Some(first + queries.marks - 1),
        })
    }

    /// Mark the end of layer `layer` of `eye`'s pass in `frame`, if layers are marked.
    pub fn mark(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        frame: Option<Frame>,
        eye: usize,
        layer: usize,
    ) {
        if let (Some(queries), Some(frame)) = (&self.queries, frame)
            && queries.within
        {
            pass.write_timestamp(&queries.set, queries.first(frame, eye) + 1 + layer as u32);
        }
    }

    /// Resolve `frame`'s timestamps and copy them out for reading, after its passes.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder, frame: Option<Frame>) {
        let (Some(queries), Some(frame)) = (&self.queries, frame) else {
            return;
        };
        let readback = &queries.readbacks[frame.slot];
        let first = queries.first(frame, 0);
        let count = readback.eyes as u32 * queries.marks;
        let offset = STRIDE * frame.slot as u64;
        encoder.resolve_query_set(
            &queries.set,
            first..first + count,
            &queries.resolved,
            offset,
        );
        encoder.copy_buffer_to_buffer(
            &queries.resolved,
            offset,
            &readback.buffer,
            0,
            readback.size(queries.marks),
        );
    }

    /// Once `frame` is submitted: read its timestamps back once the GPU has written them.
    pub fn submitted(&self, frame: Option<Frame>) {
        let (Some(queries), Some(frame)) = (&self.queries, frame) else {
            return;
        };
        let readback = &queries.readbacks[frame.slot];
        let state = readback.state.clone();
        readback
            .buffer
            .slice(..readback.size(queries.marks))
            .map_async(wgpu::MapMode::Read, move |result| {
                // A readback that could not be mapped is not timed, and is free again.
                state.store(u8::from(result.is_ok()) * MAPPED, Ordering::Release);
            });
    }

    /// Keep what a frame drew, in each eye.
    pub fn tally(&mut self, tally: Tally) {
        self.tallies.push(tally);
    }

    /// A line summarizing the frames since the last, which are then forgotten, or `None` if there were none: of the
    /// frames timed, the GPU's time for both eyes and for each, and for each layer if marked, the mean, the 95th
    /// percentile and the most in milliseconds; and of every frame, the pieces drawn and culled, the draws and the
    /// triangles they submit, in each eye, their mean, 95th percentile and most.
    pub fn summary(&mut self) -> Option<String> {
        if self.tallies.is_empty() && self.timed.is_empty() {
            return None;
        }
        let spread = |mut values: Vec<f64>| -> [f64; 3] {
            values.sort_by(f64::total_cmp);
            let mean = values.iter().sum::<f64>() / values.len().max(1) as f64;
            let at = |index: usize| values.get(index).copied().unwrap_or_default();
            [
                mean,
                at(values.len() * 95 / 100),
                at(values.len().saturating_sub(1)),
            ]
        };
        let times = if self.timed.is_empty() {
            "no timestamps".to_owned()
        } else {
            let eyes = self
                .timed
                .iter()
                .map(|timed| timed.eyes.len())
                .max()
                .unwrap_or(0);
            let mut parts = vec![(
                "eyes",
                spread(
                    self.timed
                        .iter()
                        .map(|timed| timed.eyes.iter().sum())
                        .collect(),
                ),
            )];
            // A pass drawing both eyes has no time for each.
            let each = match self.views {
                crate::multiview::Views::One => eyes.min(2),
                crate::multiview::Views::Both => 0,
            };
            for (eye, name) in ["left", "right"].into_iter().enumerate().take(each) {
                parts.push((
                    name,
                    spread(
                        self.timed
                            .iter()
                            .filter_map(|timed| timed.eyes.get(eye).copied())
                            .collect(),
                    ),
                ));
            }
            if self.timed.iter().all(|timed| timed.layers.is_some()) {
                for (layer, name) in LAYERS.into_iter().enumerate() {
                    parts.push((
                        name,
                        spread(
                            self.timed
                                .iter()
                                .flat_map(|timed| timed.layers.map(|layers| layers[layer]))
                                .collect(),
                        ),
                    ));
                }
            }
            let parts: Vec<String> = parts
                .into_iter()
                .map(|(name, [mean, p95, max])| format!("{name} {mean:.2}/{p95:.2}/{max:.2}"))
                .collect();
            format!(
                "{} timed, ms mean/p95/max: {}",
                self.timed.len(),
                parts.join(", ")
            )
        };
        let count = |name: &str, of: fn(&Tally) -> usize| {
            let [mean, p95, max] =
                spread(self.tallies.iter().map(|tally| of(tally) as f64).collect());
            format!("{name} {mean:.1}/{p95:.0}/{max:.0}")
        };
        let counts = [
            count("drawn", |tally| tally.drawn),
            count("culled", |tally| tally.culled),
            count("draws", |tally| tally.draws),
            count("triangles", |tally| tally.triangles),
        ];
        let line = format!(
            "GPU over {} frames, {}; an eye: {}",
            self.tallies.len(),
            times,
            counts.join(", ")
        );
        self.timed.clear();
        self.tallies.clear();
        Some(line)
    }
}

#[cfg(test)]
mod tests;
