// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! What is nearer the eye is drawn in front, whatever order it is drawn in: see `design/rendering.md`.
//!
//! Panels, labels and signs are translucent, so they are drawn with the depth test and without writing depth, each blended
//! over what is behind it. On its own, that leaves which of two of them is in front to the order they are drawn in. So
//! once a surface is drawn, the backing behind its text, where it is opaque enough to hide what is behind it, is drawn
//! again into depth alone, with an [`occluder`] pipeline, fragment by fragment, only where it is itself opaque enough,
//! so that a rounded corner's outside or a glyph's margin, though in the same quad, writes none: whatever is drawn after it and behind it is hidden, as the
//! scene's opaque bodies hide it, and whatever is drawn after it and in front of it still covers it. Its depth is written
//! a little behind it, by [`TIE`], so that between surfaces about as far away as each other, such as a panel carried over
//! a wall's slot and the panel in that slot, the order the frame step draws them in still decides. A surface too faint
//! to hide what is behind it, such as a wall's translucent board, a slot's outline or a fading label, writes no depth, so
//! that nothing vanishes behind what can barely be seen. Arrival's labels, the hand menu's in a space, are drawn last,
//! so nothing after them needs their depth, and they write none.

/// How opaque a backing must be, from 0 to 1, to hide what is behind it: one that shows less than two fifths of what is
/// behind it.
pub const OCCLUDING: f32 = 0.6;

/// How far behind a backing its depth is written, in the depth buffer's units, from 0 at the near plane to 1 at the far.
/// With every client's near plane at 5 cm (`tomori_scene::depth`), which decides it, whatever the far plane, a surface drawn later and less than about
/// `TIE * distance² / 0.05` metres behind it, about 2% of its distance for each metre of it, is still drawn over it: 4 mm
/// at arm's length, 2 cm at 1 m, and 11 cm at 2.3 m, as far as the Tomori wall, enough for a panel carried in one of its
/// slots and the hint below it, which faces the eyes and so leans back under the panel seen from above. Further behind,
/// it is hidden.
pub const TIE: f32 = 1e-3;

/// Whether a backing of `alpha` hides what is behind it.
pub fn occludes(alpha: f32) -> bool {
    alpha >= OCCLUDING
}

/// A pipeline that draws `vertex`'s triangles into depth alone, [`TIE`] behind where they are, depth tested, with no
/// colour, within a pass of `format` and `samples`: the backings of surfaces already drawn, only where each fragment is
/// at least [`OCCLUDING`] opaque. `shader` has the vertex entry point `vs_occlude`, which moves its position back by its
/// override `TIE`, and the fragment entry point `fs_occlude`, whose colour is not written, and which discards a fragment
/// less opaque than its override `OCCLUDING`, as its colour's alpha and its coverage of the atlas, under its override
/// `GAMMA` as the surface's own pipeline draws it, make it: so that a rounded corner's outside, a glyph's margin or an
/// antialiased edge, drawn in the same quad as what is opaque, never hides what is drawn after it behind it.
pub fn occluder(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vertex: wgpu::VertexBufferLayout<'_>,
    format: wgpu::TextureFormat,
    samples: u32,
) -> wgpu::RenderPipeline {
    occluder_with_views(
        device,
        label,
        layout,
        shader,
        vertex,
        format,
        samples,
        crate::multiview::Views::One,
    )
}

/// As [`occluder`], within a pass of `views`, `shader` made for them; see [`crate::multiview`].
#[allow(clippy::too_many_arguments)]
pub fn occluder_with_views(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vertex: wgpu::VertexBufferLayout<'_>,
    format: wgpu::TextureFormat,
    samples: u32,
    views: crate::multiview::Views,
) -> wgpu::RenderPipeline {
    occluder_at(
        device, label, layout, shader, vertex, format, samples, OCCLUDING, views,
    )
}

/// As [`occluder`], writing depth only where a fragment is at least `occluding` opaque: none is discarded at 0, as every
/// backing's whole quad wrote its depth before November 2026, which tests compare with.
#[allow(clippy::too_many_arguments)]
pub(crate) fn occluder_at(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vertex: wgpu::VertexBufferLayout<'_>,
    format: wgpu::TextureFormat,
    samples: u32,
    occluding: f32,
    views: crate::multiview::Views,
) -> wgpu::RenderPipeline {
    // The fragment stage's overrides: the threshold, and, where the target is sRGB, the glyphs' `GAMMA`, as the
    // surface's own pipeline draws it.
    let fragment = [
        ("OCCLUDING", f64::from(occluding)),
        ("GAMMA", f64::from(crate::glyphs::GAMMA)),
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_occlude"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[("TIE", TIE as f64)],
                ..Default::default()
            },
            buffers: &[Some(vertex)],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_occlude"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &fragment[..if format.is_srgb() { 2 } else { 1 }],
                ..Default::default()
            },
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::empty(),
            })],
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_occluder_is_made_from_a_surface_s_shader_for_a_pass_of_one_view() {
        crate::test_gpu::with(|crate::test_gpu::Gpu { device, .. }| {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let glyphs = crate::glyphs::Glyphs::new(&device);
            let transforms =
                crate::ui_panel::transforms_layout(&device, crate::multiview::Views::One);
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&transforms), Some(glyphs.layout())],
                immediate_size: 0,
            });
            let shader = device.create_shader_module(hazed_wgsl!("ui_panel.wgsl"));
            occluder(
                &device,
                "test occluder",
                &layout,
                &shader,
                wgpu::VertexBufferLayout {
                    array_stride: 36,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4, 2 => Float32x2, 3 => Float32],
                },
                format,
                4,
            );
            assert!(pollster::block_on(scope.pop()).is_none());
        });
    }

    #[test]
    fn backings_hide_what_is_behind_them_only_when_opaque_enough() {
        // A panel's face, the label on the ray, the hint, the pill and a worker's sign hide what is behind them.
        for alpha in [1.0, 0.95, 0.88, 0.82, OCCLUDING] {
            assert!(occludes(alpha), "{alpha}");
        }
        // A wall's board, a curved wall's slots, a slot's outline and a label fading out do not.
        for alpha in [0.5, 0.45, 0.16, 0.0] {
            assert!(!occludes(alpha), "{alpha}");
        }
    }

    /// Through the headset's own projection, in the depth buffer's 32-bit floats: a panel a couple of millimetres
    /// before its wall's board is drawn in front of it, never fighting it, from arm's length to across a room; and
    /// [`TIE`] reaches as far behind a backing as it says, 4 mm at arm's length, 2 cm at 1 m and 11 cm at 2.3 m.
    #[test]
    fn near_surfaces_keep_their_order_and_their_tie_through_the_headset_s_projection() {
        use glam::Vec3;
        use tomori_scene::scene::headset::{Fov, View, view_projection};
        let clip = view_projection(&View {
            pose: tomori_scene::controller::Pose {
                position: [0.0, 1.6, 0.0],
                orientation: [0.0, 0.0, 0.0, 1.0],
            },
            fov: Fov {
                left: -0.9,
                right: 0.7,
                down: -0.87,
                up: 0.7,
            },
        });
        let depth = |distance: f32| clip.project_point3(Vec3::new(0.2, 1.5, -distance)).z;
        for distance in [0.3, 0.45, 1.0, 2.3, 4.0, 8.0] {
            let (panel, board) = (depth(distance - 0.002), depth(distance));
            assert!(panel < board, "{distance} m: {panel} and {board}");
            assert!((0.0..1.0).contains(&panel));
        }
        for (distance, reach) in [(0.45, 0.004), (1.0, 0.02), (2.3, 0.11)] {
            let tied = depth(distance) + TIE;
            assert!(depth(distance + reach * 0.9) < tied, "{distance} m");
            assert!(depth(distance + reach * 1.1) > tied, "{distance} m");
        }
    }
}
