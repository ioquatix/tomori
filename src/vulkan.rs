//! The only raw graphics interop: OpenXR chooses/creates Vulkan objects, wgpu
//! owns the instance/device and renders into borrowed runtime swapchain images.
use anyhow::{Context, Result, ensure};
use ash::vk::{self, Handle};
use openxr as xr;
use std::ffi::{c_char, c_void};

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub binding: xr::vulkan::SessionCreateInfo,
    _instance: wgpu::Instance,
}

impl Gpu {
    pub fn new(xr: &xr::Instance, system: xr::SystemId) -> Result<Self> {
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
            .application_name(c"base-agent-swarm")
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
        let features = wgpu::Features::empty();
        let limits = wgpu::Limits::default();
        let device_extensions = adapter.adapter.required_device_extensions(features);
        let device_ptrs: Vec<_> = device_extensions.iter().map(|e| e.as_ptr()).collect();
        let mut physical_features = adapter
            .adapter
            .physical_device_features(&device_extensions, features);
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&[1.0])];
        let info = physical_features.add_to_device_create(
            vk::DeviceCreateInfo::default()
                .queue_create_infos(&queues)
                .enabled_extension_names(&device_ptrs),
        );
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
            ash::Device::load(
                raw_instance.fp_v1_0(),
                vk::Device::from_raw(raw_device as _),
            )
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
        Ok(Self {
            device,
            queue,
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
    /// destruction, and the GPU must finish work before xrReleaseSwapchainImage.
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
                let views = std::array::from_fn(|eye| {
                    texture.create_view(&wgpu::TextureViewDescriptor {
                        label: Some("XR eye"),
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        base_array_layer: eye as u32,
                        array_layer_count: Some(1),
                        ..Default::default()
                    })
                });
                Ok(ImportedImage {
                    views,
                    _texture: texture,
                })
            })
            .collect()
    }
}

pub struct ImportedImage {
    pub views: [wgpu::TextureView; 2],
    _texture: wgpu::Texture,
}
