// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Image panels' images on the GPU: each PNG the server made is decoded on a thread of its own, with every mipmap level
//! made from it in linear light, so that a decode never holds up a frame, and uploaded as an sRGB texture, sampled
//! trilinearly and anisotropically, so that an image is sharp near and does not shimmer far or at an angle. Textures are
//! kept by hash, once each however many panels show the image, within a budget of GPU memory: a texture no panel has
//! shown for a while is let go, and when a new one does not fit, those not shown this frame go first, the least recently
//! shown first, and then the new one is uploaded without its largest levels, down to [`SMALLEST`], rather than not at
//! all. See `design/projection.md`.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// The most bytes of image textures, with their mipmaps: about six of the largest images, or a few dozen of a more usual
/// size, a small part of a Quest 3's memory.
pub const TEXTURE_BYTES: usize = 128 * 1024 * 1024;

/// The smallest longer side a texture is cut down to, to fit the budget, in pixels.
pub const SMALLEST: u32 = 256;

/// How many frames a texture no panel shows is kept, about ten seconds on a headset, so that one shown again soon, as
/// when the space returns to an environment, is not decoded again.
pub const EXPIRE_FRAMES: u64 = 900;

/// How many textures are uploaded in one frame, so that several images arriving together do not drop a frame.
const UPLOADS_PER_FRAME: usize = 1;

/// The longest side of an image decoded: the server sends none larger.
const SIDE: u32 = tomori_protocol::IMAGE_SIDE;

/// One mipmap level, 8-bit RGBA in sRGB, its rows packed.
#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// `png`, an image panel's PNG, decoded, with every mipmap level down to a pixel, or why it could not be.
pub fn decode(png: &[u8]) -> Result<Vec<Level>, String> {
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(SIDE);
    limits.max_image_height = Some(SIDE);
    reader.limits(limits);
    let image = reader.decode().map_err(|error| error.to_string())?;
    let rgba = image.into_rgba8();
    Ok(mipmaps(Level {
        width: rgba.width(),
        height: rgba.height(),
        pixels: rgba.into_raw(),
    }))
}

/// sRGB's value of each byte in linear light.
fn linear_table() -> &'static [f32; 256] {
    static TABLE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|byte| {
            let value = byte as f32 / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        })
    })
}

/// `value`, in linear light, as an sRGB byte.
fn encode(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0 + 0.5) as u8
}

/// For each of `to` pixels across a level, the pixels of the `from` across the level above that it covers, each with its
/// share: the two above it where `from` is even, and three, the middle one wholly and its neighbours in part, where it is
/// odd, so that every pixel above counts alike and the last column or row of an odd level is never left out.
fn spans(from: u32, to: u32) -> Vec<Vec<(u32, f32)>> {
    let scale = f64::from(from) / f64::from(to);
    (0..to)
        .map(|index| {
            let (start, end) = (f64::from(index) * scale, f64::from(index + 1) * scale);
            (start.floor() as u32..(end.ceil() as u32).min(from))
                .map(|pixel| {
                    let covered = end.min(f64::from(pixel + 1)) - start.max(f64::from(pixel));
                    (pixel, (covered / scale) as f32)
                })
                .collect()
        })
        .collect()
}

/// `level` and every level below it, each half the size of the one above, rounded down, down to a pixel, each pixel the
/// average in linear light of the part of the level above it covers, so that an image does not darken as it shrinks,
/// nor lose its right and bottom edges where a level's width or height is odd.
pub fn mipmaps(level: Level) -> Vec<Level> {
    let table = linear_table();
    let mut levels = vec![level];
    loop {
        let above = levels.last().expect("one at least");
        if above.width == 1 && above.height == 1 {
            return levels;
        }
        let (width, height) = ((above.width / 2).max(1), (above.height / 2).max(1));
        let (across, down) = (spans(above.width, width), spans(above.height, height));
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for rows in &down {
            for columns in &across {
                let mut sum = [0.0f32; 4];
                for (sy, row_share) in rows {
                    for (sx, column_share) in columns {
                        let share = row_share * column_share;
                        let at = ((sy * above.width + sx) * 4) as usize;
                        for channel in 0..3 {
                            sum[channel] += share * table[above.pixels[at + channel] as usize];
                        }
                        sum[3] += share * f32::from(above.pixels[at + 3]);
                    }
                }
                pixels.extend([
                    encode(sum[0]),
                    encode(sum[1]),
                    encode(sum[2]),
                    (sum[3] + 0.5).min(255.0) as u8,
                ]);
            }
        }
        levels.push(Level {
            width,
            height,
            pixels,
        });
    }
}

/// The bytes `levels` take.
fn bytes(levels: &[Level]) -> usize {
    levels.iter().map(|level| level.pixels.len()).sum()
}

/// What the decoding thread sends back: an image's hash and its levels, or why it could not be decoded.
type Decoded = (String, Result<Vec<Level>, String>);

/// The thread that decodes images.
struct Worker {
    send: Sender<(String, Arc<[u8]>)>,
    receive: Receiver<Decoded>,
}

impl Worker {
    /// The thread, started. One that cannot start drops its ends of the channels, so that nothing is decoded and every
    /// image's panel shows where it would be.
    fn start() -> Self {
        let (send, requests) = channel::<(String, Arc<[u8]>)>();
        let (results, receive) = channel();
        let _ = std::thread::Builder::new()
            .name("tomori-images".into())
            .spawn(move || {
                // Until the renderer, and so the requests' sender, is dropped.
                for (hash, png) in requests {
                    let decoded = decode(&png);
                    let _ = results.send((hash, decoded));
                }
            });
        Self { send, receive }
    }
}

/// An image's texture.
struct Texture {
    binding: wgpu::BindGroup,
    bytes: usize,
    /// The last frame a panel showed it in.
    shown: u64,
    /// How many of its largest levels were left out, to fit the budget, and how many it has.
    skipped: usize,
    levels: usize,
}

/// The pipeline that draws images over their panels, and their textures. See the module's documentation.
pub struct PanelImages {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Whether textures are sRGB, so that they are sampled in linear light, as an sRGB target blends; into another
    /// target, they are sampled as they are stored, as that target expects.
    srgb: bool,
    textures: HashMap<String, Texture>,
    /// Sent to be decoded, not yet back.
    decoding: HashSet<String>,
    /// Decoded, waiting for a frame to upload them in.
    decoded: Vec<(String, Vec<Level>)>,
    /// Those that could not be decoded, which are not tried again.
    failed: HashSet<String>,
    worker: Option<Worker>,
    budget: usize,
    used: usize,
    frame: u64,
}

const VERTEX: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: 16,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
};

impl PanelImages {
    /// The pipeline drawing into `format` with `samples`, its transforms bound as a panel's are, with `transforms`.
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        samples: u32,
        transforms: &wgpu::BindGroupLayout,
    ) -> Self {
        Self::with_views(
            device,
            format,
            samples,
            transforms,
            crate::multiview::Views::One,
        )
    }

    /// As [`Self::new`], drawn in passes of `views`, as the panels its transforms are bound for are; see
    /// [`crate::multiview`].
    pub fn with_views(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        samples: u32,
        transforms: &wgpu::BindGroupLayout,
        views: crate::multiview::Views,
    ) -> Self {
        let shader = crate::multiview::shader(
            device,
            "panel_image.wgsl",
            include_str!("panel_image.wgsl"),
            views,
        );
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("panel image layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("panel image pipeline layout"),
            bind_group_layouts: &[Some(transforms), Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("panel image pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(VERTEX)],
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
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            // Tested against depth as its panel is, and drawn in its plane, in front of its backing's depth, which is
            // written a little behind it.
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("panel image sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
            srgb: format.is_srgb(),
            textures: HashMap::new(),
            decoding: HashSet::new(),
            decoded: Vec::new(),
            failed: HashSet::new(),
            worker: None,
            budget: TEXTURE_BYTES,
            used: 0,
            frame: 0,
        }
    }

    /// Keep textures within `budget` bytes from now on.
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
    }

    /// How many bytes the textures take.
    pub fn used(&self) -> usize {
        self.used
    }

    /// How many of the largest mipmap levels of the texture of `hash` were left out to fit the budget, and how many it
    /// has, if it has one.
    pub fn levels(&self, hash: &str) -> Option<(usize, usize)> {
        self.textures
            .get(hash)
            .map(|texture| (texture.skipped, texture.levels))
    }

    /// The bind group drawing the image `hash`, once its texture is ready.
    pub fn binding(&self, hash: &str) -> Option<&wgpu::BindGroup> {
        self.textures.get(hash).map(|texture| &texture.binding)
    }

    /// This frame's panels show `shown`, each image by its hash with its PNG: keep their textures, decode those without
    /// one, upload one decoded since, and let go of those no panel has shown for [`EXPIRE_FRAMES`].
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shown: &[(String, Arc<[u8]>)],
    ) {
        self.frame += 1;
        let frame = self.frame;
        for (hash, png) in shown {
            if let Some(texture) = self.textures.get_mut(hash) {
                texture.shown = frame;
                continue;
            }
            if self.decoding.contains(hash) || self.failed.contains(hash) {
                continue;
            }
            // Not sent again while it is decoded, nor ever, should the thread not have started.
            let worker = self.worker.get_or_insert_with(Worker::start);
            let _ = worker.send.send((hash.clone(), png.clone()));
            self.decoding.insert(hash.clone());
        }
        if let Some(worker) = &self.worker {
            while let Ok((hash, decoded)) = worker.receive.try_recv() {
                self.decoding.remove(&hash);
                match decoded {
                    Ok(levels) => self.decoded.push((hash, levels)),
                    Err(error) => {
                        log::warn!("Could not decode an image panel's image: {error}.");
                        self.failed.insert(hash);
                    }
                }
            }
        }
        // Only what is still shown is uploaded; the rest is dropped, and decoded again if it is shown again.
        self.decoded
            .retain(|(hash, _)| shown.iter().any(|(shown, _)| shown == hash));
        for _ in 0..UPLOADS_PER_FRAME {
            let Some((hash, levels)) = self.decoded.pop() else {
                break;
            };
            // One that does not fit yet waits, decoded, for room.
            if let Err(waiting) = self.upload(device, queue, hash, levels) {
                self.decoded.insert(0, waiting);
                break;
            }
        }
        let expired: Vec<String> = self
            .textures
            .iter()
            .filter(|(_, texture)| frame - texture.shown > EXPIRE_FRAMES)
            .map(|(hash, _)| hash.clone())
            .collect();
        for hash in expired {
            self.evict(&hash);
        }
    }

    fn evict(&mut self, hash: &str) {
        self.used -= self
            .textures
            .remove(hash)
            .map_or(0, |texture| texture.bytes);
    }

    /// Upload `levels`, the image `hash`, making room within the budget: letting go of the textures not shown this
    /// frame, the least recently shown first, and then leaving out its largest levels, down to [`SMALLEST`]. Returns it,
    /// not uploaded, if it does not fit even so.
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        hash: String,
        levels: Vec<Level>,
    ) -> Result<(), (String, Vec<Level>)> {
        while self.used + bytes(&levels) > self.budget {
            let oldest = self
                .textures
                .iter()
                .filter(|(_, texture)| texture.shown < self.frame)
                .min_by_key(|(_, texture)| texture.shown)
                .map(|(hash, _)| hash.clone());
            let Some(oldest) = oldest else {
                break;
            };
            self.evict(&oldest);
        }
        let room = self.budget.saturating_sub(self.used);
        let mut skipped = 0;
        while skipped + 1 < levels.len()
            && bytes(&levels[skipped..]) > room
            && levels[skipped + 1].width.max(levels[skipped + 1].height) >= SMALLEST
        {
            skipped += 1;
        }
        let kept = &levels[skipped..];
        if bytes(kept) > room {
            log::debug!("An image panel's image waits for room within the texture budget.");
            return Err((hash, levels));
        }
        let format = if self.srgb {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("panel image"),
            size: wgpu::Extent3d {
                width: kept[0].width,
                height: kept[0].height,
                depth_or_array_layers: 1,
            },
            mip_level_count: kept.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (mip, level) in kept.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: mip as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &level.pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(level.width * 4),
                    rows_per_image: Some(level.height),
                },
                wgpu::Extent3d {
                    width: level.width,
                    height: level.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&Default::default());
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("panel image binding"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let bytes = bytes(kept);
        self.used += bytes;
        self.textures.insert(
            hash,
            Texture {
                binding,
                bytes,
                shown: self.frame,
                skipped,
                levels: kept.len(),
            },
        );
        Ok(())
    }

    /// Draw `panel`'s image for `eye`, if its texture is ready, leaving the image pipeline set; returns whether it drew.
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        panel: &crate::ui_panel::Panel,
        eye: usize,
    ) -> bool {
        let Some(binding) = panel.image().and_then(|hash| self.binding(hash)) else {
            return false;
        };
        pass.set_pipeline(&self.pipeline);
        panel.draw_image(pass, binding, eye);
        true
    }
}

#[cfg(test)]
mod tests;
