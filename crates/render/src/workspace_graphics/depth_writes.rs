// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Depth is written fragment by fragment, only where a backing is opaque enough, and translucent surfaces are drawn back
//! to front: a sign's rounded corner, a glyph quad's margin and a rounded shape's outside never hide a translucent wall
//! drawn after them behind them, so that the background never shows through the wall in a halo about them. See
//! `design/rendering.md`. `TOMORI_DEPTH_WRITES_PREVIEW` and `TOMORI_SIGN_CORNER_PREVIEW`, set to PNG paths, save what
//! these draw.
use super::*;
use crate::test_gpu;

const SIZE: u32 = 192;
/// A distinctive background, opaque, far behind.
const MAGENTA: [f32; 4] = [1.0, 0.0, 1.0, 1.0];
/// A translucent wall's board, between the background and what stands in front of it, too faint to write depth.
const WALL: [f32; 4] = [0.0, 1.0, 0.0, 0.5];
/// What stands in front, in one colour, so that its coverage is plain.
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// A quad from `(x, y)` to `(x + size, y - size)` at depth `z`, in `color`, drawn from the solid block.
fn square(x: f32, y: f32, z: f32, size: f32, color: [f32; 4]) -> Vec<Vertex> {
    let mut mesh = Vec::new();
    quad(&mut mesh, x, y, z, size, size, color);
    mesh
}

/// `mesh` moved by `offset`, at depth `z`, in `color`.
fn recoloured(mesh: &[Vertex], offset: [f32; 2], z: f32, color: [f32; 4]) -> Vec<Vertex> {
    mesh.iter()
        .map(|vertex| Vertex {
            position: [
                vertex.position[0] + offset[0],
                vertex.position[1] + offset[1],
                z,
            ],
            color,
            ..*vertex
        })
        .collect()
}

/// `mesh`, drawn from the solid block instead, so that every quad of it covers all its pixels.
fn footprint(mesh: &[Vertex]) -> Vec<Vertex> {
    mesh.iter()
        .map(|vertex| Vertex {
            uv: [SOLID[0], SOLID[1]],
            ..*vertex
        })
        .collect()
}

/// What stands in front, each in the bad order, drawn before the wall behind it: a sign's plate, its corners rounded; a
/// glyph's quad, a large "O"; and a rounded box from the shared shapes, a pill.
fn items() -> Vec<(&'static str, Vec<Vertex>)> {
    let station = Station {
        facing: crate::delegation::HOME_EYES,
        id: "a".into(),
        title: "Corner".into(),
        position: [0.0, 1.2, -2.5],
        state: State::Running,
        opacity: 1.0,
        scale: 1.0,
        more: 0,
        size: None,
        highlighted: false,
    };
    let mut panel = station_panel(&station, station.position, State::Running, None);
    panel.pixel = 0.02;
    let sign = panel.sign.clone().unwrap();
    let mut plate = Vec::new();
    let surface = sign_mesh(&panel, &sign, &mut plate);
    plate.truncate(surface);
    let width = (panel.columns * 9) as f32 * panel.pixel;
    let height = (panel.lines.len() * 11) as f32 * panel.pixel;
    let plate = recoloured(&plate, [-width / 2.0, height / 2.0], -1.0, BLUE);
    let cells: Vec<(char, ())> =
        crate::ui_text::cut(&[crate::ui_text::Span::new("O", ROW_STYLE)], 1)
            .into_iter()
            .map(|(ch, _)| (ch, ()))
            .collect();
    let mut glyph = Vec::new();
    for (quad, ()) in crate::ui_text::quads(&cells, 0.0, 0.0, 11.0, |()| false) {
        let p = 0.12;
        textured(
            &mut glyph,
            [
                quad.x * p - 0.55,
                -quad.y * p + 0.66,
                0.0,
                quad.width * p,
                quad.height * p,
            ],
            BLUE,
            quad.tile,
            quad.weight,
        );
    }
    let glyph = recoloured(&glyph, [0.0, 0.0], -1.0, BLUE);
    let mut pill = Vec::new();
    shape(
        &mut pill,
        &Shape::Rect {
            x: 0.0,
            y: 0.0,
            width: 16.0,
            height: 8.0,
            color: BLUE,
            radius: 4.0,
            outline: None,
        },
        0.1,
        0.0,
    );
    let pill = recoloured(&pill, [-0.8, 0.4], -1.0, BLUE);
    vec![
        ("sign plate", plate),
        ("glyph", glyph),
        ("rounded rect", pill),
    ]
}

#[test]
fn a_transparent_corner_margin_or_outside_never_hides_a_translucent_wall_drawn_after_it() {
    test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let camera_layout = crate::graphics::camera_layout(&device, crate::multiview::Views::One);
        use wgpu::util::DeviceExt;
        let camera = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&crate::graphics::camera(
                glam::camera::rh::proj::directx::orthographic(-1.0, 1.0, -1.0, 1.0, 0.05, 50.0),
                tomori_assets::haze::Haze::NONE,
                1,
            )),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            }],
        });
        let glyphs = crate::glyphs::Glyphs::new(&device);
        let graphics = WorkspaceGraphics::new(&device, format, &camera_layout, &glyphs, 1);
        // As every surface wrote depth before, over its whole quad, whatever its coverage: none is discarded.
        let shader = device.create_shader_module(hazed_wgsl!("../workspace.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&camera_layout), Some(glyphs.layout())],
            immediate_size: 0,
        });
        let whole_quads = crate::occlusion::occluder_at(
            &device,
            "whole quads",
            &layout,
            &shader,
            VERTICES,
            format,
            1,
            0.0,
            crate::multiview::Views::One,
        );
        let items = items();
        let extent = wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let depth = device
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default());
        // Draw each mesh, and then, for those that write it, its depth with `occluder`: the background, then what
        // stands in front, then the wall between them.
        let draw = |meshes: &[(&[Vertex], bool)], occluder: &wgpu::RenderPipeline| {
            glyphs.sync(&device, &queue);
            let glyph_binding = glyphs.binding();
            let buffers: Vec<_> = meshes
                .iter()
                .map(|(mesh, _)| {
                    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: None,
                        contents: bytemuck::cast_slice(mesh),
                        usage: wgpu::BufferUsages::VERTEX,
                    })
                })
                .collect();
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Discard,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                pass.set_bind_group(0, &binding, &[]);
                pass.set_bind_group(1, &glyph_binding, &[]);
                for ((mesh, writes), buffer) in meshes.iter().zip(&buffers) {
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.set_pipeline(&graphics.pipeline);
                    pass.draw(0..mesh.len() as u32, 0..1);
                    if *writes {
                        pass.set_pipeline(occluder);
                        pass.draw(0..mesh.len() as u32, 0..1);
                    }
                }
            }
            queue.submit([encoder.finish()]);
            crate::test_support::read_back(&device, &queue, &texture, (SIZE, SIZE))
        };
        let background = square(-1.0, 1.0, -3.0, 2.0, MAGENTA);
        let wall = square(-1.0, 1.0, -2.0, 2.0, WALL);
        let mut sheet = image::RgbaImage::new(SIZE * 2, SIZE * items.len() as u32);
        for (row, (name, item)) in items.iter().enumerate() {
            // Where it covers, and where its quads reach but it does not.
            let covered = draw(&[(item, false)], &graphics.occluder);
            let reach = draw(&[(&footprint(item), false)], &graphics.occluder);
            let mut drawn = Vec::new();
            for (column, occluder) in [&whole_quads, &graphics.occluder].into_iter().enumerate() {
                let image = draw(
                    &[(&background, true), (item, true), (&wall, false)],
                    occluder,
                );
                image::imageops::replace(
                    &mut sheet,
                    &image,
                    i64::from(SIZE) * column as i64,
                    i64::from(SIZE) * row as i64,
                );
                drawn.push(image);
            }
            let [before, after] = [&drawn[0], &drawn[1]];
            let (mut outside, mut inside, mut halo) = (0, 0, 0);
            for (x, y, pixel) in covered.enumerate_pixels() {
                let (now, was) = (after.get_pixel(x, y), before.get_pixel(x, y));
                if reach.get_pixel(x, y)[2] == 255 && pixel[2] == 0 {
                    // A rounded corner's outside, or a glyph's margin: the wall shows over the background there.
                    outside += 1;
                    assert!(
                        now[1] > 100 && now[0] > 100,
                        "{name}: the background shows through the wall at {x}, {y}: {now:?}"
                    );
                    halo += usize::from(was[1] < 10);
                } else if pixel[2] == 255 {
                    // Where it is opaque, it hides the wall behind it, as it did.
                    inside += 1;
                    assert!(now[1] < 10 && now[2] > 245, "{name}: {x}, {y}: {now:?}");
                    assert_eq!(now, was, "{name}: {x}, {y}");
                }
            }
            assert!(outside > 20 && inside > 20, "{name}: {outside} {inside}");
            // As every quad wrote its depth whole, the background showed through the wall there: the halo.
            assert!(halo > 20, "{name}: {halo}");
        }
        crate::test_support::preview("TOMORI_DEPTH_WRITES_PREVIEW", &sheet);
        assert!(pollster::block_on(scope.pop()).is_none());
    });
}

#[test]
fn a_sign_before_a_translucent_wall_shows_the_wall_about_its_corners_and_blends_over_it() {
    use crate::ui_panel::Canvas;
    test_gpu::with(|test_gpu::Gpu { device, queue, .. }| {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let mut renderer = crate::graphics::Renderer::new(&device, &queue, format, SIZE, SIZE);
        let mut swarm = crate::swarm::Swarm::default();
        swarm.agents.clear();
        renderer.update(&queue, &swarm, None, false);
        // A distinctive background far behind, and a wall's translucent board between it and the sign, a metre behind.
        let board = |color: [f32; 4]| {
            let mut canvas = Canvas::new(0, 0.01);
            canvas.padding = (200, 200);
            canvas.background = color;
            let mut panel = renderer.panel(&device, 64);
            panel.set(&queue, &canvas);
            panel
        };
        let backdrop = board(MAGENTA);
        let wall = board([0.1, 0.9, 0.2, 0.45]);
        let models = [
            glam::Mat4::from_translation(glam::Vec3::new(0.0, 1.6, -5.0)),
            glam::Mat4::from_translation(glam::Vec3::new(0.0, 1.6, -3.5)),
        ];
        let eye = glam::Vec3::new(0.0, 1.6, 0.0);
        let station = Station {
            facing: crate::delegation::HOME_EYES,
            id: "a".into(),
            title: "Corner".into(),
            position: [0.0, 1.2, -2.5],
            state: State::Running,
            opacity: 1.0,
            scale: 1.0,
            more: 0,
            size: None,
            highlighted: false,
        };
        let workspace = Workspace {
            nodes: vec![crate::delegation::Node {
                id: "a".into(),
                parent_id: Some("root".into()),
                title: "Corner".into(),
                state: State::Running,
            }],
            ..Default::default()
        };
        // Its plate's top-left corner, where its rounding starts, and a point just outside the rounding and one inside
        // the plate, in font pixels from it.
        let panel = station_panel(
            &station,
            station.position,
            State::Running,
            Some(eye.to_array()),
        );
        let sign = panel.sign.clone().unwrap();
        let right = glam::Vec3::from_array(sign.right);
        let normal = glam::Vec3::from_array(sign.normal);
        let corner = glam::Vec3::from_array(panel.origin)
            + right * (-PLATE_MARGIN[0] * panel.pixel)
            + glam::Vec3::Y * (PLATE_MARGIN[1] * panel.pixel);
        let at = |x: f32, y: f32| {
            let mid = (panel.columns * 9) as f32 * panel.pixel / 2.0;
            let reach = (SIGN_COLUMNS * 9) as f32 * panel.pixel / 2.0;
            let across = -PLATE_MARGIN[0] * panel.pixel + x * panel.pixel;
            corner
                + right * (x * panel.pixel)
                + glam::Vec3::Y * (-y * panel.pixel)
                + normal * (SIGN_PLATE + SIGN_BOW * ((across - mid) / reach).powi(2))
        };
        let matrix = glam::camera::rh::proj::directx::perspective(0.03, 1.0, 0.05, 50.0)
            * glam::camera::rh::view::look_at_mat4(eye, at(3.0, 3.0), glam::Vec3::Y);
        let pixel_at = |image: &image::RgbaImage, point: glam::Vec3| {
            let clip = matrix * point.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            let x = ((ndc.x + 1.0) / 2.0 * SIZE as f32) as u32;
            let y = ((1.0 - ndc.y) / 2.0 * SIZE as f32) as u32;
            *image.get_pixel(x, y)
        };
        let mut sheet = image::RgbaImage::new(SIZE * 2, SIZE);
        for (column, opacity) in [1.0, 0.5].into_iter().enumerate() {
            let station = Station {
                opacity,
                ..station.clone()
            };
            renderer.update_workspace(
                &device,
                &queue,
                &[],
                std::slice::from_ref(&station),
                &workspace,
                Some(eye),
            );
            renderer.render(
                &device,
                &queue,
                std::slice::from_ref(&view),
                &[matrix],
                glam::Mat4::IDENTITY,
                &[(&backdrop, models[0]), (&wall, models[1])],
            );
            let image = crate::test_support::read_back(&device, &queue, &texture, (SIZE, SIZE));
            image::imageops::replace(&mut sheet, &image, i64::from(SIZE) * column as i64, 0);
            let outside = pixel_at(&image, at(0.4, 0.4));
            let inside = pixel_at(&image, at(8.0, 8.0));
            // Outside its rounded corner, the wall shows over the background, as it does beside the sign, rather than
            // the background alone.
            let beside = pixel_at(&image, at(-3.0, -3.0));
            assert!(outside[1] > 100, "{opacity}: {outside:?}");
            assert!(
                outside
                    .0
                    .iter()
                    .zip(beside.0)
                    .all(|(a, b)| a.abs_diff(b) <= 2),
                "{opacity}: {outside:?} {beside:?}"
            );
            if opacity == 1.0 {
                // Opaque, its dark plate hides the wall.
                assert!(inside.0[..3].iter().all(|&c| c < 80), "{inside:?}");
            } else {
                // Fading, it is drawn after the wall behind it, over it, rather than the wall over it: its dark plate
                // darkens the wall's green by half, where the wall over it would leave it barely darker.
                assert!(inside[1] < 150 && inside[1] > 60, "{inside:?} {beside:?}");
            }
        }
        crate::test_support::preview("TOMORI_SIGN_CORNER_PREVIEW", &sheet);
        assert!(pollster::block_on(scope.pop()).is_none());
    });
}
