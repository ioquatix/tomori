// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Drawing both eyes in one render pass, by multiview, where the device can: how many [`Views`] a pass draws, the
//! shaders rewritten for them, each eye's uniforms held as the pass binds them ([`Uniforms`]), and the eyes' two-layer
//! targets. Every pipeline drawn in an eye's pass is made for the renderer's views, and a scenery is told them in its
//! [`Target`](crate::scenery::Target). See `design/rendering.md`.
//!
//! A vertex shader drawing both views never gives a function of its own an eye's uniform, the element of its array, by
//! value: the Quest 3's Adreno driver fails to make such a pipeline for two views with `VK_ERROR_UNKNOWN`, as it did
//! Tomori's cubes, whose camera was given whole to the function that grows them, which wgpu takes as the device lost,
//! every object made after it invalid. It is given the fields it reads instead, and
//! `test_gpu::validate_wgsl` refuses a shader that does otherwise.
pub use tomori_assets::views::{VIEW_INDEX_MARK, VIEW_MARK, VIEWS_MARK, Views};

/// The features a device is asked for, where its adapter has them, to draw both eyes in one pass: multiview, and
/// multisampled targets of two layers, for multisample anti-aliasing.
pub const FEATURES: wgpu::Features =
    wgpu::Features::MULTIVIEW.union(wgpu::Features::MULTISAMPLE_ARRAY);

/// `limits`, with as many views a pass as `adapter` draws, up to two, where it has multiview, for a device to be asked
/// for with [`FEATURES`].
pub fn limits(adapter: &wgpu::Adapter, limits: wgpu::Limits) -> wgpu::Limits {
    wgpu::Limits {
        max_multiview_view_count: adapter.limits().max_multiview_view_count.min(2),
        ..limits
    }
}

/// Whether `device` can draw both eyes in one pass with `samples` samples a pixel: a Vulkan device, as the headset's is,
/// with multiview, for at least two views, and, multisampled, multisampled targets of two layers.
pub fn supported(device: &wgpu::Device, samples: u32) -> bool {
    unsupported(device, samples).is_none()
}

/// Why `device` cannot draw both eyes in one pass with `samples` samples a pixel, for the log, or none if it can.
pub fn unsupported(device: &wgpu::Device, samples: u32) -> Option<&'static str> {
    missing(
        vulkan(device),
        device.features(),
        device.limits().max_multiview_view_count,
        samples,
    )
}

/// Why a device, Vulkan's or not, with `features` and drawing at most `views` views a pass, cannot draw both eyes in one
/// pass with `samples` samples a pixel, or none if it can.
fn missing(
    vulkan: bool,
    features: wgpu::Features,
    views: u32,
    samples: u32,
) -> Option<&'static str> {
    [
        (!vulkan, "the device is not Vulkan's"),
        (
            !features.contains(wgpu::Features::MULTIVIEW),
            "the device has no multiview",
        ),
        (views < 2, "the device draws fewer than two views a pass"),
        (
            samples > 1 && !features.contains(wgpu::Features::MULTISAMPLE_ARRAY),
            "the device has no multisampled targets of two layers",
        ),
    ]
    .into_iter()
    .find_map(|(missing, why)| missing.then_some(why))
}

/// Whether `device` is Vulkan's, on the platforms wgpu draws with Vulkan natively, as the headset's.
#[cfg(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd"
))]
fn vulkan(device: &wgpu::Device) -> bool {
    // SAFETY: only asks whether the device is Vulkan's.
    unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }.is_some()
}

/// Nothing else draws with Vulkan, such as macOS, which draws with Metal.
#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd"
)))]
fn vulkan(_device: &wgpu::Device) -> bool {
    false
}

/// `views`, if `device` can draw them with `samples` samples a pixel, or else [`Views::One`].
pub fn usable(device: &wgpu::Device, views: Views, samples: u32) -> Views {
    if views == Views::Both && !supported(device, samples) {
        Views::One
    } else {
        views
    }
}

/// A shader module of `source`, a shader written for one view (see [`Views`]), for `views`, after the shared haze
/// functions, [`crate::haze`], which every shader drawing in the world calls, unless it begins with them already.
pub fn shader(
    device: &wgpu::Device,
    label: &str,
    source: &str,
    views: Views,
) -> wgpu::ShaderModule {
    let hazed = if source.starts_with(crate::haze::WGSL) {
        source.to_owned()
    } else {
        crate::haze::shader(source)
    };
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(views.shader(&hazed).into()),
    })
}

/// A uniform of `size` bytes for each eye, a multiple of 16: in a buffer of its own for each where each eye has a pass,
/// or both in one, an array of two the shaders index by their view, where one pass draws both. Each eye's is written at
/// its own place, so that both are written before the frame is submitted.
pub struct Uniforms {
    buffers: Vec<wgpu::Buffer>,
    size: u64,
    views: Views,
}

impl Uniforms {
    pub fn new(device: &wgpu::Device, label: &str, size: u64, views: Views) -> Self {
        Self::with_usage(device, label, size, views, wgpu::BufferUsages::empty())
    }

    /// As [`Self::new`], its buffers usable as `usage` says besides.
    pub(crate) fn with_usage(
        device: &wgpu::Device,
        label: &str,
        size: u64,
        views: Views,
        usage: wgpu::BufferUsages,
    ) -> Self {
        assert_eq!(size % 16, 0, "an eye's uniform is a multiple of 16 bytes");
        let buffers = (0..views.passes(2))
            .map(|_| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: size * views.count() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST | usage,
                    mapped_at_creation: false,
                })
            })
            .collect();
        Self {
            buffers,
            size,
            views,
        }
    }

    /// The size a binding of an eye's uniforms of `size` bytes holds, for its layout.
    pub fn binding_size(size: u64, views: Views) -> Option<wgpu::BufferSize> {
        wgpu::BufferSize::new(size * views.count() as u64)
    }

    /// Write `eye`'s uniform, before the pass that draws it.
    pub fn write(&self, queue: &wgpu::Queue, eye: usize, bytes: &[u8]) {
        debug_assert_eq!(bytes.len() as u64, self.size);
        match self.views {
            Views::One => queue.write_buffer(&self.buffers[eye], 0, bytes),
            Views::Both => queue.write_buffer(&self.buffers[0], eye as u64 * self.size, bytes),
        }
    }

    /// The buffer pass `pass` binds: each eye's own, or, drawing both, the one holding both.
    pub fn buffer(&self, pass: usize) -> &wgpu::Buffer {
        &self.buffers[pass]
    }

    /// The buffers, one for each pass.
    pub fn buffers(&self) -> &[wgpu::Buffer] {
        &self.buffers
    }

    pub fn views(&self) -> Views {
        self.views
    }
}

/// The eyes' attachments made by the renderer, `width` by `height` in `format` with `samples` samples a pixel, for
/// `views`: one for each eye where each has a pass, or one of two layers, viewed as an array, that the pass draws both
/// eyes into.
pub fn attachments(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
    (width, height): (u32, u32),
    samples: u32,
    usage: wgpu::TextureUsages,
    views: Views,
) -> Vec<wgpu::TextureView> {
    (0..views.passes(2))
        .map(|_| {
            layered(
                device,
                &wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: views.count() as u32,
                    },
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                },
            )
            .create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(match views {
                    Views::One => wgpu::TextureViewDimension::D2,
                    Views::Both => wgpu::TextureViewDimension::D2Array,
                }),
                ..Default::default()
            })
        })
        .collect()
}

/// A texture as `descriptor` describes it. wgpu makes a transient attachment, kept in tile memory and never backed by
/// memory of its own where the GPU allows, only of one layer; one of two, as drawing both eyes at once needs, is made
/// through wgpu-hal, as wgpu makes a transient one of a layer, in lazily allocated memory where the GPU has it, on the
/// Vulkan device every pass drawing both eyes is drawn by ([`supported`]).
fn layered(device: &wgpu::Device, descriptor: &wgpu::TextureDescriptor<'_>) -> wgpu::Texture {
    if descriptor.size.depth_or_array_layers == 1
        || !descriptor
            .usage
            .contains(wgpu::TextureUsages::TRANSIENT_ATTACHMENT)
    {
        return device.create_texture(descriptor);
    }
    transient(device, descriptor)
}

/// Without Vulkan, as on macOS, where no pass draws both eyes, an attachment of two layers that is not transient.
#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd"
)))]
fn transient(device: &wgpu::Device, descriptor: &wgpu::TextureDescriptor<'_>) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        usage: descriptor.usage - wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
        ..*descriptor
    })
}

/// A transient attachment of two layers, as [`layered`] makes one on Vulkan.
#[cfg(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd"
))]
fn transient(device: &wgpu::Device, descriptor: &wgpu::TextureDescriptor<'_>) -> wgpu::Texture {
    use wgpu::hal::{Device as _, api::Vulkan};
    let target = if descriptor.format.is_depth_stencil_format() {
        wgpu::TextureUses::DEPTH_STENCIL_READ | wgpu::TextureUses::DEPTH_STENCIL_WRITE
    } else {
        wgpu::TextureUses::COLOR_TARGET
    };
    // SAFETY: the texture is made by `device`'s own Vulkan device as `descriptor` describes it, one mip level of
    // attachments only, which wgpu takes as it would a texture it made, its state unknown until its first pass, which
    // clears it.
    let made = unsafe {
        let hal = device
            .as_hal::<Vulkan>()
            .expect("both eyes are drawn at once on Vulkan");
        hal.create_texture(&wgpu::hal::TextureDescriptor {
            label: descriptor.label,
            size: descriptor.size,
            mip_level_count: descriptor.mip_level_count,
            sample_count: descriptor.sample_count,
            dimension: descriptor.dimension,
            format: descriptor.format,
            usage: target | wgpu::TextureUses::TRANSIENT,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        })
        .expect("the eyes' transient attachments of two layers are made")
    };
    // SAFETY: as above.
    unsafe {
        device.create_texture_from_hal::<Vulkan>(made, descriptor, wgpu::TextureUses::UNINITIALIZED)
    }
}

#[cfg(test)]
mod tests;
