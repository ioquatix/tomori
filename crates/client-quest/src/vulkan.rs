// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The only raw graphics interop: OpenXR chooses/creates Vulkan objects, wgpu
//! owns the instance/device and renders into borrowed runtime swapchain images.
//! Where the eyes are foveated, the device's render pass and framebuffer
//! functions are wrapped to give the eyes' passes their density map, which wgpu
//! cannot; see [`crate::foveation`].
use crate::foveation::{self, DensityPass, EyePass, Level, Map, Passes};
use crate::scene::headset::View;
use anyhow::{Context, Result, anyhow, ensure};
use ash::vk::{self, Handle};
use openxr as xr;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// The runtime's physical device, for the sample counts it can draw with.
    pub adapter: wgpu::Adapter,
    pub binding: xr::vulkan::SessionCreateInfo,
    /// What the GPU offers for drawing at a fragment density map, enabled where it is usable and foveation is asked for.
    pub density: foveation::Gpu,
    texels: [vk::Extent2D; 2],
    raw: Raw,
    _instance: wgpu::Instance,
}

/// The Vulkan function tables and handles beneath wgpu, for the density map's own image; wgpu owns and destroys the instance and device.
#[derive(Clone)]
struct Raw {
    instance: ash::Instance,
    device: ash::Device,
    physical: vk::PhysicalDevice,
    family: u32,
}

impl Gpu {
    /// The runtime's Vulkan device, with `VK_EXT_fragment_density_map` enabled where `foveate` and the GPU can draw at it,
    /// and multiview, for both eyes in one pass, where `multiview` and the GPU has it.
    pub fn new(
        xr: &xr::Instance,
        system: xr::SystemId,
        foveate: bool,
        multiview: bool,
    ) -> Result<Self> {
        const API: u32 = vk::API_VERSION_1_1;
        let requirements = xr.graphics_requirements::<xr::Vulkan>(system)?;
        ensure!(
            requirements.min_api_version_supported <= xr::Version::new(1, 1, 0),
            "Runtime requires Vulkan newer than 1.1"
        );
        // SAFETY: Vulkan's loader is supplied by the OS; all raw objects below
        // are created through XR_KHR_vulkan_enable2 for this runtime/system.
        let entry = unsafe { ash::Entry::load() }.context("loading Vulkan")?;
        let flags = wgpu::InstanceFlags::empty();
        let extensions = wgpu_hal::vulkan::Instance::desired_extensions(&entry, API, flags)?;
        let extension_ptrs: Vec<_> = extensions.iter().map(|e| e.as_ptr()).collect();
        let app_info = vk::ApplicationInfo::default()
            .application_name(c"tomori")
            .api_version(API);
        let info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&extension_ptrs);
        // OpenXR and ash use ABI-identical Vulkan function pointers with
        // different opaque-handle types.
        let get_proc = unsafe {
            std::mem::transmute::<
                unsafe extern "system" fn(
                    vk::Instance,
                    *const c_char,
                ) -> Option<unsafe extern "system" fn()>,
                unsafe extern "system" fn(
                    *const c_void,
                    *const c_char,
                ) -> Option<unsafe extern "system" fn()>,
            >(entry.static_fn().get_instance_proc_addr)
        };
        let raw_instance = unsafe {
            xr.create_vulkan_instance(system, get_proc, &info as *const _ as *const _)?
                .map_err(vk::Result::from_raw)?
        };
        let raw_instance = unsafe {
            ash::Instance::load(entry.static_fn(), vk::Instance::from_raw(raw_instance as _))
        };
        let physical = vk::PhysicalDevice::from_raw(unsafe {
            xr.vulkan_graphics_device(system, raw_instance.handle().as_raw() as _)? as _
        });
        let properties = unsafe { raw_instance.get_physical_device_properties(physical) };
        ensure!(
            properties.api_version >= API,
            "GPU does not support Vulkan 1.1"
        );
        let instance_handle = raw_instance.handle().as_raw() as *const c_void;
        let families =
            unsafe { raw_instance.get_physical_device_queue_family_properties(physical) };
        let family = families
            .iter()
            .position(|q| q.queue_count > 0 && q.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .context("no graphics queue")? as u32;
        // None transfers ownership of VkInstance to wgpu-hal. Record the API
        // version actually requested, not the physical device's maximum.
        let hal_instance = unsafe {
            wgpu_hal::vulkan::Instance::from_raw(
                entry.clone(),
                raw_instance.clone(),
                API,
                32,
                None,
                extensions,
                flags,
                wgpu::MemoryBudgetThresholds::default(),
                false,
                None,
            )?
        };
        let adapter = hal_instance
            .expose_adapter(physical)
            .context("exposing XR Vulkan adapter")?;
        // The GPU's timestamps, where it has them, for the frame log, and multiview where asked for; see
        // `design/rendering.md`.
        let wanted = if multiview {
            tomori_render::profiler::FEATURES | tomori_render::multiview::FEATURES
        } else {
            tomori_render::profiler::FEATURES
        };
        let features = adapter.features & wanted;
        let limits = wgpu::Limits {
            max_multiview_view_count: if features.contains(wgpu::Features::MULTIVIEW) {
                adapter.capabilities.limits.max_multiview_view_count.min(2)
            } else {
                0
            },
            ..wgpu::Limits::default()
        };
        let device_extensions = adapter.adapter.required_device_extensions(features);
        let mut device_ptrs: Vec<_> = device_extensions.iter().map(|e| e.as_ptr()).collect();
        let (density, texels) = if foveate {
            density_support(&raw_instance, physical)
        } else {
            (foveation::Gpu::default(), [vk::Extent2D::default(); 2])
        };
        // Enabled beside wgpu's own, which it does not know of: only the wrapped functions below use it.
        if density.usable() {
            device_ptrs.push(ash::ext::fragment_density_map::NAME.as_ptr());
        }
        let mut density_features = vk::PhysicalDeviceFragmentDensityMapFeaturesEXT::default()
            .fragment_density_map(true)
            .fragment_density_map_non_subsampled_images(true);
        let mut physical_features = adapter
            .adapter
            .physical_device_features(&device_extensions, features);
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&[1.0])];
        let mut info = physical_features.add_to_device_create(
            vk::DeviceCreateInfo::default()
                .queue_create_infos(&queues)
                .enabled_extension_names(&device_ptrs),
        );
        if density.usable() {
            info = info.push_next(&mut density_features);
        }
        let raw_device = unsafe {
            xr.create_vulkan_device(
                system,
                get_proc,
                physical.as_raw() as _,
                &info as *const _ as *const _,
            )?
            .map_err(vk::Result::from_raw)?
        };
        let raw_device = unsafe {
            let raw_device = vk::Device::from_raw(raw_device as _);
            if density.usable() {
                wrapped(raw_instance.fp_v1_0(), raw_device)
            } else {
                ash::Device::load(raw_instance.fp_v1_0(), raw_device)
            }
        };
        let raw = Raw {
            instance: raw_instance.clone(),
            device: raw_device.clone(),
            physical,
            family,
        };
        let device_handle = raw_device.handle().as_raw() as *const c_void;
        let open_device = unsafe {
            adapter.adapter.device_from_raw(
                raw_device,
                None,
                &device_extensions,
                features,
                &limits,
                &wgpu::MemoryHints::Performance,
                family,
                0,
            )?
        };
        let instance = unsafe { wgpu::Instance::from_hal::<wgpu_hal::api::Vulkan>(hal_instance) };
        let adapter = unsafe { instance.create_adapter_from_hal(adapter) };
        let (device, queue) = unsafe {
            adapter.create_device_from_hal(
                open_device,
                &wgpu::DeviceDescriptor {
                    label: Some("XR device"),
                    required_features: features,
                    required_limits: limits,
                    memory_hints: wgpu::MemoryHints::Performance,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                    trace: wgpu::Trace::Off,
                },
            )?
        };
        log::info!("Vulkan adapter: {}", adapter.get_info().name);
        // wgpu loses the device without an error of its own when the driver fails a call unexpectedly, and every
        // object made after is invalid: the log says why, before whatever then fails.
        device.set_device_lost_callback(|reason, message| {
            log::error!("The GPU device was lost ({reason:?}): {message}");
        });
        Ok(Self {
            device,
            queue,
            adapter,
            density,
            texels,
            raw,
            _instance: instance,
            binding: xr::vulkan::SessionCreateInfo {
                instance: instance_handle,
                physical_device: physical.as_raw() as *const c_void,
                device: device_handle,
                queue_family_index: family,
                queue_index: 0,
            },
        })
    }

    /// Borrow XR-owned images. These wrappers MUST be dropped before swapchain
    /// destruction, after the GPU has finished with them, and an image's commands
    /// must be submitted to the session's queue before xrReleaseSwapchainImage.
    pub fn import_images(
        &self,
        swapchain: &xr::Swapchain<xr::Vulkan>,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Result<Vec<ImportedImage>> {
        swapchain
            .enumerate_images()?
            .into_iter()
            .map(|image| {
                let size = wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 2,
                };
                // SAFETY: the descriptor matches the XR swapchain. A no-op callback
                // prevents wgpu from destroying the runtime-owned VkImage; External
                // prevents it from freeing runtime-owned VkDeviceMemory.
                let hal_texture = unsafe {
                    let hal = self
                        .device
                        .as_hal::<wgpu_hal::api::Vulkan>()
                        .context("not a Vulkan device")?;
                    hal.texture_from_raw(
                        vk::Image::from_raw(image),
                        &wgpu_hal::TextureDescriptor {
                            label: Some("borrowed XR image"),
                            size,
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format,
                            usage: wgpu::TextureUses::COLOR_TARGET,
                            memory_flags: wgpu_hal::MemoryFlags::empty(),
                            view_formats: vec![],
                        },
                        Some(Box::new(|| {})),
                        wgpu_hal::vulkan::TextureMemory::External,
                    )
                };
                let texture = unsafe {
                    self.device
                        .create_texture_from_hal::<wgpu_hal::api::Vulkan>(
                            hal_texture,
                            &wgpu::TextureDescriptor {
                                label: Some("XR stereo image"),
                                size,
                                mip_level_count: 1,
                                sample_count: 1,
                                dimension: wgpu::TextureDimension::D2,
                                format,
                                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                                view_formats: &[],
                            },
                            wgpu::TextureUses::UNINITIALIZED,
                        )
                };
                let views: [wgpu::TextureView; 2] = std::array::from_fn(|eye| {
                    texture.create_view(&wgpu::TextureViewDescriptor {
                        label: Some("XR eye"),
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        base_array_layer: eye as u32,
                        array_layer_count: Some(1),
                        ..Default::default()
                    })
                });
                // Both eyes' layers, which one pass draws both into by multiview.
                let layers = texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("XR eyes"),
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                });
                // The Vulkan view each eye is drawn into, and both, by which its framebuffer is given that eye's density
                // map, or both eyes'.
                let raw = |view: &wgpu::TextureView| unsafe {
                    view.as_hal::<wgpu_hal::api::Vulkan>()
                        .map_or(vk::ImageView::null(), |view| view.raw_handle())
                };
                let targets = views.each_ref().map(raw);
                let layers_target = raw(&layers);
                Ok(ImportedImage {
                    views,
                    layers: vec![layers],
                    targets,
                    layers_target,
                    _texture: texture,
                })
            })
            .collect()
    }
}

pub struct ImportedImage {
    pub views: [wgpu::TextureView; 2],
    /// Both eyes' layers, as one pass drawing both by multiview is given them.
    pub layers: Vec<wgpu::TextureView>,
    targets: [vk::ImageView; 2],
    layers_target: vk::ImageView,
    _texture: wgpu::Texture,
}

impl ImportedImage {
    /// The targets of a frame's passes drawing `views`: each eye's layer, or both.
    pub fn targets(&self, views: tomori_render::multiview::Views) -> &[wgpu::TextureView] {
        match views {
            tomori_render::multiview::Views::One => &self.views,
            tomori_render::multiview::Views::Both => &self.layers,
        }
    }
}

/// The density maps the runtime gave for a swapchain's images, `XR_FB_foveation_vulkan`'s, one for each image, in order, with their size in texels.
pub struct RuntimeMaps {
    pub images: Vec<vk::Image>,
    pub size: vk::Extent2D,
}

impl Gpu {
    /// Foveate the eyes drawn into `images`, of `width` by `height` pixels in `format` with `samples` each, at `level`, from the runtime's density `maps` for `Map::Runtime` or Tomori's own: from now on each render pass shaped as an eye's is made with a density map, and each of its framebuffers given its eye's. Tomori's own map is full everywhere until [`Foveation::shape`] is told the eyes.
    ///
    /// Call before the renderer makes its pipelines, so that they are made against passes with the map too.
    #[allow(clippy::too_many_arguments)]
    pub fn foveate(
        &self,
        level: Level,
        map: Map,
        images: &[ImportedImage],
        maps: Option<&RuntimeMaps>,
        format: vk::Format,
        samples: u32,
        width: u32,
        height: u32,
    ) -> Result<Foveation> {
        ensure!(
            self.density.usable(),
            "the GPU cannot draw at a density map"
        );
        let size = foveation::map_size(width, height, self.texels[0], self.texels[1]);
        let mut foveation = Foveation {
            raw: self.raw.clone(),
            level,
            map,
            size,
            own: OwnMap::default(),
            views: Vec::new(),
            shaped: false,
        };
        foveation.own = foveation.own_map()?;
        let device = &self.raw.device;
        let mut passes = passes();
        match (map, maps) {
            (Map::Runtime, Some(maps)) => {
                ensure!(
                    maps.images.len() == images.len(),
                    "a density map for each swapchain image"
                );
                for (image, density) in images.iter().zip(&maps.images) {
                    for (eye, &target) in image.targets.iter().enumerate() {
                        let view = unsafe { layer_view(device, *density, eye as u32) }?;
                        foveation.views.push(view);
                        passes.target(target, view);
                    }
                    // Both eyes drawn in one pass by multiview are each drawn at its own layer of the map.
                    let both = unsafe { layers_view(device, *density) }?;
                    foveation.views.push(both);
                    passes.target(image.layers_target, both);
                }
            }
            _ => {
                for image in images {
                    for eye in 0..2 {
                        passes.target(image.targets[eye], foveation.own.views[eye]);
                    }
                    passes.target(image.layers_target, foveation.own.both);
                }
            }
        }
        passes.foveate(
            EyePass {
                format,
                samples: vk::SampleCountFlags::from_raw(samples),
            },
            foveation.own.views[2],
        );
        Ok(foveation)
    }
}

/// The eyes' foveation, kept while they are drawn: Tomori's own density map, and the views of the runtime's. Dropped after the GPU has finished drawing, before the swapchain whose maps it views.
pub struct Foveation {
    raw: Raw,
    pub level: Level,
    pub map: Map,
    size: vk::Extent2D,
    own: OwnMap,
    views: Vec<vk::ImageView>,
    shaped: bool,
}

/// Tomori's own density map: one image of three layers, the left eye's, the right eye's and one full everywhere, for a framebuffer of the eyes' shape drawing anything else; and the first two together, for both eyes drawn in one pass by multiview.
#[derive(Default)]
struct OwnMap {
    image: vk::Image,
    memory: vk::DeviceMemory,
    views: [vk::ImageView; 3],
    both: vk::ImageView,
}

impl Foveation {
    /// Shape Tomori's own map for the eyes' `views`, once, the first frame they are located: about where each looks straight ahead. The runtime's map needs nothing.
    pub fn shape(&mut self, views: &[View; 2]) -> Result<()> {
        if self.shaped || self.map != Map::Own {
            return Ok(());
        }
        self.shaped = true;
        let layers = [
            foveation::own_map(self.level, self.size, Some(foveation::centre(views[0].fov))),
            foveation::own_map(self.level, self.size, Some(foveation::centre(views[1].fov))),
            foveation::own_map(Level::None, self.size, None),
        ];
        unsafe {
            self.upload(
                self.own.image,
                &layers,
                vk::ImageLayout::FRAGMENT_DENSITY_MAP_OPTIMAL_EXT,
            )
        }
    }

    /// How many framebuffers have been given an eye's density map, and how many of the eyes' shape drew something else.
    pub fn framebuffers(&self) -> (u64, u64) {
        let passes = passes();
        (passes.framebuffers, passes.unknown)
    }

    fn own_map(&self) -> Result<OwnMap> {
        let device = &self.raw.device;
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8_UNORM)
            .extent(vk::Extent3D {
                width: self.size.width,
                height: self.size.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(3)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::FRAGMENT_DENSITY_MAP_EXT | vk::ImageUsageFlags::TRANSFER_DST,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let mut own = OwnMap::default();
        unsafe {
            own.image = device.create_image(&info, None)?;
            let requirements = device.get_image_memory_requirements(own.image);
            let memory = self.memory_type(
                requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )?;
            own.memory = device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory),
                None,
            )?;
            device.bind_image_memory(own.image, own.memory, 0)?;
            for layer in 0..3 {
                own.views[layer] = layer_view(device, own.image, layer as u32)?;
            }
            own.both = layers_view(device, own.image)?;
            let full = foveation::own_map(Level::None, self.size, None);
            self.upload(
                own.image,
                &[full.clone(), full.clone(), full],
                vk::ImageLayout::UNDEFINED,
            )?;
        }
        Ok(own)
    }

    fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Result<u32> {
        let properties = unsafe {
            self.raw
                .instance
                .get_physical_device_memory_properties(self.raw.physical)
        };
        (0..properties.memory_type_count)
            .find(|index| {
                bits & (1 << index) != 0
                    && properties.memory_types[*index as usize]
                        .property_flags
                        .contains(flags)
            })
            .context("no memory for the density map")
    }

    /// Write `layers` into `image`, from `layout`, leaving it as a density map is read, and wait for it on the session's queue: only at startup and on the first frame, while nothing else is submitted.
    unsafe fn upload(
        &self,
        image: vk::Image,
        layers: &[Vec<u8>],
        layout: vk::ImageLayout,
    ) -> Result<()> {
        let device = &self.raw.device;
        let bytes: Vec<u8> = layers.concat();
        let layer_size = layers[0].len() as u64;
        unsafe {
            let buffer = device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(bytes.len() as u64)
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?;
            let requirements = device.get_buffer_memory_requirements(buffer);
            let memory = device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(self.memory_type(
                        requirements.memory_type_bits,
                        vk::MemoryPropertyFlags::HOST_VISIBLE
                            | vk::MemoryPropertyFlags::HOST_COHERENT,
                    )?),
                None,
            )?;
            device.bind_buffer_memory(buffer, memory, 0)?;
            let mapped =
                device.map_memory(memory, 0, bytes.len() as u64, vk::MemoryMapFlags::empty())?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast(), bytes.len());
            device.unmap_memory(memory);
            let pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                    .queue_family_index(self.raw.family),
                None,
            )?;
            let commands = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: layers.len() as u32,
            };
            device.begin_command_buffer(
                commands,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            device.cmd_pipeline_barrier(
                commands,
                vk::PipelineStageFlags::TOP_OF_PIPE
                    | vk::PipelineStageFlags::FRAGMENT_DENSITY_PROCESS_EXT,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[vk::ImageMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::empty())
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .old_layout(layout)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(range)],
            );
            let regions: Vec<_> = (0..layers.len() as u32)
                .map(|layer| vk::BufferImageCopy {
                    buffer_offset: layer as u64 * layer_size,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: layer,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D {
                        width: self.size.width,
                        height: self.size.height,
                        depth: 1,
                    },
                })
                .collect();
            device.cmd_copy_buffer_to_image(
                commands,
                buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &regions,
            );
            device.cmd_pipeline_barrier(
                commands,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_DENSITY_PROCESS_EXT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[vk::ImageMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::FRAGMENT_DENSITY_MAP_READ_EXT)
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::FRAGMENT_DENSITY_MAP_OPTIMAL_EXT)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(range)],
            );
            device.end_command_buffer(commands)?;
            let queue = device.get_device_queue(self.raw.family, 0);
            // Whatever is already on the queue may read the map: let it finish first.
            device.queue_wait_idle(queue)?;
            let submitted = device
                .queue_submit(
                    queue,
                    &[vk::SubmitInfo::default().command_buffers(&[commands])],
                    vk::Fence::null(),
                )
                .and_then(|()| device.queue_wait_idle(queue));
            device.destroy_command_pool(pool, None);
            device.destroy_buffer(buffer, None);
            device.free_memory(memory, None);
            submitted?;
        }
        Ok(())
    }
}

impl Drop for Foveation {
    fn drop(&mut self) {
        passes().stop();
        let device = &self.raw.device;
        unsafe {
            for view in self
                .views
                .drain(..)
                .chain(self.own.views)
                .chain([self.own.both])
            {
                if view != vk::ImageView::null() {
                    device.destroy_image_view(view, None);
                }
            }
            if self.own.image != vk::Image::null() {
                device.destroy_image(self.own.image, None);
            }
            if self.own.memory != vk::DeviceMemory::null() {
                device.free_memory(self.own.memory, None);
            }
        }
    }
}

/// A view of one `layer` of a density map.
unsafe fn layer_view(device: &ash::Device, image: vk::Image, layer: u32) -> Result<vk::ImageView> {
    let info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(vk::Format::R8G8_UNORM)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: layer,
            layer_count: 1,
        });
    Ok(unsafe { device.create_image_view(&info, None) }?)
}

/// A view of a density map's first two layers, the eyes', for both drawn in one pass by multiview, each at its own.
unsafe fn layers_view(device: &ash::Device, image: vk::Image) -> Result<vk::ImageView> {
    let info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D_ARRAY)
        .format(vk::Format::R8G8_UNORM)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 2,
        });
    Ok(unsafe { device.create_image_view(&info, None) }?)
}

/// What `physical` offers for drawing at a fragment density map, and the least and greatest sizes of the map's texels.
fn density_support(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
) -> (foveation::Gpu, [vk::Extent2D; 2]) {
    let extension = unsafe { instance.enumerate_device_extension_properties(physical) }
        .unwrap_or_default()
        .iter()
        .any(|extension| {
            extension
                .extension_name_as_c_str()
                .is_ok_and(|name| name == ash::ext::fragment_density_map::NAME)
        });
    if !extension {
        return (foveation::Gpu::default(), [vk::Extent2D::default(); 2]);
    }
    let mut features = vk::PhysicalDeviceFragmentDensityMapFeaturesEXT::default();
    let mut all = vk::PhysicalDeviceFeatures2::default().push_next(&mut features);
    unsafe { instance.get_physical_device_features2(physical, &mut all) };
    let mut properties = vk::PhysicalDeviceFragmentDensityMapPropertiesEXT::default();
    let mut all = vk::PhysicalDeviceProperties2::default().push_next(&mut properties);
    unsafe { instance.get_physical_device_properties2(physical, &mut all) };
    let format =
        unsafe { instance.get_physical_device_format_properties(physical, vk::Format::R8G8_UNORM) };
    (
        foveation::Gpu {
            extension,
            density_map: features.fragment_density_map == vk::TRUE,
            non_subsampled_images: features.fragment_density_map_non_subsampled_images == vk::TRUE,
            format: format
                .optimal_tiling_features
                .contains(vk::FormatFeatureFlags::FRAGMENT_DENSITY_MAP_EXT),
        },
        [
            properties.min_fragment_density_texel_size,
            properties.max_fragment_density_texel_size,
        ],
    )
}

/// The device's own functions, which wgpu-hal calls, wrapped so that the eyes' render passes and their framebuffers are made with a density map, as [`Passes`] decides.
#[derive(Clone, Copy)]
struct Wrapped {
    create_render_pass: vk::PFN_vkCreateRenderPass,
    destroy_render_pass: vk::PFN_vkDestroyRenderPass,
    create_framebuffer: vk::PFN_vkCreateFramebuffer,
}

/// Each wrapped device's own functions, since a device's may be called only with it, and an activity made again in the same process makes another device.
static WRAPPED: LazyLock<Mutex<HashMap<vk::Device, Wrapped>>> = LazyLock::new(Mutex::default);
static PASSES: LazyLock<Mutex<Passes>> = LazyLock::new(Mutex::default);

fn passes() -> MutexGuard<'static, Passes> {
    PASSES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `device`'s functions, three of them wrapped.
unsafe fn wrapped(instance: &ash::InstanceFnV1_0, device: vk::Device) -> ash::Device {
    let real = unsafe { ash::Device::load(instance, device) };
    let functions = real.fp_v1_0();
    WRAPPED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            device,
            Wrapped {
                create_render_pass: functions.create_render_pass,
                destroy_render_pass: functions.destroy_render_pass,
                create_framebuffer: functions.create_framebuffer,
            },
        );
    unsafe {
        ash::Device::load_with(
            |name: &CStr| match name.to_bytes() {
                b"vkCreateRenderPass" => create_render_pass as *const c_void,
                b"vkDestroyRenderPass" => destroy_render_pass as *const c_void,
                b"vkCreateFramebuffer" => create_framebuffer as *const c_void,
                _ => std::mem::transmute::<vk::PFN_vkVoidFunction, *const c_void>((instance
                    .get_device_proc_addr)(
                    device,
                    name.as_ptr(),
                )),
            },
            device,
        )
    }
}

/// `device`'s own functions, kept before any is called; never a panic, which cannot unwind out of a function the driver's caller calls.
fn real(device: vk::Device) -> Option<Wrapped> {
    WRAPPED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&device)
        .copied()
}

/// The most attachments a framebuffer given a density map is made with without allocating: an eye's has its samples, its resolve, its depth and the map.
const ATTACHMENTS: usize = 8;

unsafe extern "system" fn create_render_pass(
    device: vk::Device,
    info: *const vk::RenderPassCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    pass: *mut vk::RenderPass,
) -> vk::Result {
    let Some(real) = real(device) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let eye = passes().eye();
    unsafe {
        let given = &*info;
        if let Some(eye) = eye
            && eye.matches(given)
        {
            let density = DensityPass::new(given);
            let result = (real.create_render_pass)(device, density.info(), allocator, pass);
            if result == vk::Result::SUCCESS {
                passes().created(*pass);
            }
            return result;
        }
        (real.create_render_pass)(device, info, allocator, pass)
    }
}

unsafe extern "system" fn destroy_render_pass(
    device: vk::Device,
    pass: vk::RenderPass,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    passes().destroyed(pass);
    if let Some(real) = real(device) {
        unsafe { (real.destroy_render_pass)(device, pass, allocator) }
    }
}

unsafe extern "system" fn create_framebuffer(
    device: vk::Device,
    info: *const vk::FramebufferCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    framebuffer: *mut vk::Framebuffer,
) -> vk::Result {
    let Some(real) = real(device) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    unsafe {
        let given = &*info;
        // An imageless framebuffer, which wgpu 30 does not make, names its attachments elsewhere, so it is left as it is.
        if given.flags.contains(vk::FramebufferCreateFlags::IMAGELESS) {
            return (real.create_framebuffer)(device, info, allocator, framebuffer);
        }
        let attachments = foveation::slice(given.p_attachments, given.attachment_count);
        let density = passes().density(given.render_pass, attachments);
        if let Some(density) = density {
            // wgpu makes each frame's framebuffers anew, so an eye's are made without allocating.
            let mut stack = [vk::ImageView::null(); ATTACHMENTS];
            let mut heap = Vec::new();
            let views = if attachments.len() < ATTACHMENTS {
                stack[..attachments.len()].copy_from_slice(attachments);
                stack[attachments.len()] = density;
                &stack[..=attachments.len()]
            } else {
                heap.extend_from_slice(attachments);
                heap.push(density);
                &heap[..]
            };
            let mut with = *given;
            with.attachment_count = views.len() as u32;
            with.p_attachments = views.as_ptr();
            return (real.create_framebuffer)(device, &with, allocator, framebuffer);
        }
        (real.create_framebuffer)(device, info, allocator, framebuffer)
    }
}

/// The runtime's density maps for `swapchain`'s images, by `XR_FB_foveation_vulkan`, where it gives one for every image.
pub fn runtime_maps(swapchain: &xr::Swapchain<xr::Vulkan>) -> Result<Option<RuntimeMaps>> {
    use xr::sys;
    let enumerate = swapchain.instance().fp().enumerate_swapchain_images;
    let mut count = 0;
    check(unsafe { enumerate(swapchain.as_raw(), 0, &mut count, std::ptr::null_mut()) })?;
    let mut maps = vec![
        sys::SwapchainImageFoveationVulkanFB {
            ty: sys::SwapchainImageFoveationVulkanFB::TYPE,
            next: std::ptr::null_mut(),
            image: 0,
            width: 0,
            height: 0,
        };
        count as usize
    ];
    let base = maps.as_mut_ptr();
    let mut images: Vec<_> = (0..count as usize)
        .map(|index| sys::SwapchainImageVulkanKHR {
            ty: sys::SwapchainImageVulkanKHR::TYPE,
            next: unsafe { base.add(index) }.cast(),
            image: 0,
        })
        .collect();
    check(unsafe {
        enumerate(
            swapchain.as_raw(),
            count,
            &mut count,
            images.as_mut_ptr().cast(),
        )
    })?;
    drop(images);
    if maps.is_empty()
        || maps
            .iter()
            .any(|map| map.image == 0 || map.width == 0 || map.height == 0)
    {
        return Ok(None);
    }
    Ok(Some(RuntimeMaps {
        images: maps
            .iter()
            .map(|map| vk::Image::from_raw(map.image))
            .collect(),
        size: vk::Extent2D {
            width: maps[0].width,
            height: maps[0].height,
        },
    }))
}

fn check(result: xr::sys::Result) -> Result<()> {
    if result.into_raw() < 0 {
        Err(anyhow!("{result}"))
    } else {
        Ok(())
    }
}
