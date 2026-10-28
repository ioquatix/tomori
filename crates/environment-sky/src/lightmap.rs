//! Build-time AO (alpha) and occluded lantern irradiance (RGB), sampled in linear space.
use wgpu::util::DeviceExt;

const BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scenery-lightmap.rgba"));

pub(super) struct Lightmap {
    pub layout: wgpu::BindGroupLayout,
    pub binding: wgpu::BindGroup,
    _texture: wgpu::Texture,
}

impl Lightmap {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let size = crate::mesh::ATLAS_SIZE;
        assert_eq!(BYTES.len(), (size * size * 4) as usize);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Baked scenery lighting layout"),
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
        let texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("Baked AO and lantern lightmap"),
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            BYTES,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Baked lighting linear sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Baked scenery lighting"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &texture.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Self {
            layout,
            binding,
            _texture: texture,
        }
    }

    #[cfg(test)]
    pub fn set_enabled(&self, queue: &wgpu::Queue, enabled: bool) {
        let neutral;
        let bytes = if enabled {
            BYTES
        } else {
            neutral =
                [0, 0, 0, 255].repeat((crate::mesh::ATLAS_SIZE * crate::mesh::ATLAS_SIZE) as usize);
            &neutral
        };
        queue.write_texture(
            self._texture.as_image_copy(),
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(crate::mesh::ATLAS_SIZE * 4),
                rows_per_image: Some(crate::mesh::ATLAS_SIZE),
            },
            self._texture.size(),
        );
    }
}
