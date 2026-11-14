// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

/// A PNG of a synthetic picture `side` pixels square, of noise seeded by `seed`, which does not compress.
fn png(side: u32, seed: u32) -> Arc<[u8]> {
    let mut state = seed.max(1);
    let picture = image::RgbImage::from_fn(side, side, |_, _| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let [red, green, blue, _] = state.to_le_bytes();
        image::Rgb([red, green, blue])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    picture
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner().into()
}

#[test]
fn mipmaps_average_in_linear_light_down_to_a_pixel() {
    // Black and white, side by side, average to a mid grey as the eye sees it, which is 188 in sRGB, not 128.
    let level = Level {
        width: 2,
        height: 2,
        pixels: [[0, 0, 0, 255], [255, 255, 255, 255]].repeat(2).concat(),
    };
    let levels = mipmaps(level);
    assert_eq!(levels.len(), 2);
    assert_eq!(levels[1].pixels, [188, 188, 188, 255]);
    // An odd, long and thin image halves each side to a pixel, never to none.
    let levels = mipmaps(Level {
        width: 5,
        height: 1,
        pixels: vec![200; 20],
    });
    assert_eq!(
        levels
            .iter()
            .map(|level| (level.width, level.height))
            .collect::<Vec<_>>(),
        [(5, 1), (2, 1), (1, 1)]
    );
    assert!(
        levels
            .iter()
            .all(|level| level.pixels.len() == (level.width * level.height * 4) as usize)
    );
    // A flat colour stays the same at every level.
    assert!(
        levels
            .iter()
            .all(|level| level.pixels.iter().all(|value| *value == 200))
    );
    assert_eq!(encode(linear_table()[77]), 77);
}

#[test]
fn an_odd_level_keeps_its_last_column_and_row_as_it_does_its_first() {
    // Black, edged one pixel wide in white on every side, at sizes whose levels are odd, as 768's become below 6.
    for (width, height) in [(7, 5), (125, 3), (1, 9), (24, 12)] {
        let pixels = (0..height)
            .flat_map(|y| {
                (0..width).flat_map(move |x| {
                    let edge = x == 0 || y == 0 || x == width - 1 || y == height - 1;
                    let value = if edge { 255 } else { 0 };
                    [value, value, value, 255]
                })
            })
            .collect();
        let levels = mipmaps(Level {
            width,
            height,
            pixels,
        });
        for level in &levels {
            let at = |x: u32, y: u32| level.pixels[((y * level.width + x) * 4) as usize];
            let (right, bottom) = (level.width - 1, level.height - 1);
            // The right edge as bright as the left, and the bottom as the top, at every level: nothing shifted or lost.
            for y in 0..level.height {
                assert!(
                    at(0, y).abs_diff(at(right, y)) <= 1,
                    "{width} × {height}, level {} × {}, row {y}",
                    level.width,
                    level.height
                );
            }
            for x in 0..level.width {
                assert!(at(x, 0).abs_diff(at(x, bottom)) <= 1);
            }
            assert!(at(right, bottom) > 0 && level.pixels[3] == 255);
        }
    }
    // Five across, the last white, halve to two: the second covers the last two and a half, the white one 2/5 of it.
    let levels = mipmaps(Level {
        width: 5,
        height: 1,
        pixels: [[0, 0, 0, 255]; 4]
            .into_iter()
            .chain([[255; 4]])
            .flatten()
            .collect(),
    });
    assert_eq!(levels[1].pixels[..4], [0, 0, 0, 255]);
    assert_eq!(levels[1].pixels[4], encode(0.4));
}

#[test]
fn only_a_png_within_the_largest_side_is_decoded() {
    let levels = decode(&png(64, 3)).unwrap();
    assert_eq!(
        (levels[0].width, levels[0].height, levels.len()),
        (64, 64, 7)
    );
    assert!(decode(b"not a picture").is_err());
    // A JPEG is not what the server sends, so it is not decoded.
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::RgbImage::new(8, 8)
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    assert!(decode(&jpeg.into_inner()).is_err());
    // Nor one larger than the server sends.
    let mut wide = std::io::Cursor::new(Vec::new());
    image::GrayImage::new(SIDE + 1, 1)
        .write_to(&mut wide, image::ImageFormat::Png)
        .unwrap();
    assert!(decode(&wide.into_inner()).is_err());
}

/// The images' pipeline, drawing into `format`, for panels' transforms bound as a panel's are.
fn images_into(device: &wgpu::Device, format: wgpu::TextureFormat) -> PanelImages {
    let transforms = crate::ui_panel::transforms_layout(device, crate::multiview::Views::One);
    PanelImages::new(device, format, 1, &transforms)
}

/// The images' pipeline, drawing into an sRGB target, as the headset's is.
fn images(device: &wgpu::Device) -> PanelImages {
    images_into(device, wgpu::TextureFormat::Rgba8UnormSrgb)
}

/// Show `shown` frame after frame until each has a texture, or for a few seconds.
fn settle(
    images: &mut PanelImages,
    gpu: &crate::test_gpu::Gpu,
    shown: &[(String, Arc<[u8]>)],
    until: impl Fn(&PanelImages) -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !until(images) {
        assert!(
            std::time::Instant::now() < deadline,
            "the images never settled"
        );
        images.update(&gpu.device, &gpu.queue, shown);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[test]
fn textures_stay_within_their_budget_letting_go_of_those_not_shown_and_cutting_new_ones_down() {
    crate::test_gpu::with(|gpu| {
        let mut images = images(&gpu.device);
        let image = |name: &str, seed| (name.to_owned(), png(512, seed));
        let (a, b, c) = (image("a", 1), image("b", 2), image("c", 3));
        // 512 by 512 with every level below it is 1,398,100 bytes.
        let whole = 1_398_100;
        images.set_budget(2 * whole + 400_000);
        settle(&mut images, &gpu, std::slice::from_ref(&a), |images| {
            images.binding("a").is_some()
        });
        assert_eq!(images.levels("a"), Some((0, 10)));
        assert_eq!(images.used(), whole);
        settle(&mut images, &gpu, std::slice::from_ref(&b), |images| {
            images.binding("b").is_some()
        });
        // c does not fit beside them: a, shown least recently, goes.
        settle(&mut images, &gpu, &[b.clone(), c.clone()], |images| {
            images.binding("c").is_some()
        });
        assert!(images.binding("a").is_none() && images.binding("b").is_some());
        assert_eq!(images.used(), 2 * whole);
        // Shown with both, a fits only without its largest level, rather than not at all.
        let all = [a.clone(), b.clone(), c.clone()];
        settle(&mut images, &gpu, &all, |images| {
            images.binding("a").is_some()
        });
        assert_eq!(images.levels("a"), Some((1, 9)));
        assert!(images.used() <= 2 * whole + 400_000);
        // With no room even for the smallest it may be cut to, a fourth waits, decoded, for room.
        let d = image("d", 4);
        let mut four = all.to_vec();
        four.push(d.clone());
        for _ in 0..200 {
            images.update(&gpu.device, &gpu.queue, &four);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(images.binding("d").is_none());
        // Once the others are no longer shown, it is uploaded whole in their place.
        settle(&mut images, &gpu, std::slice::from_ref(&d), |images| {
            images.binding("d").is_some()
        });
        assert_eq!(images.levels("d"), Some((0, 10)));
        // One no panel shows for long enough is let go.
        for _ in 0..=EXPIRE_FRAMES {
            images.update(&gpu.device, &gpu.queue, &[]);
        }
        assert_eq!(images.used(), 0);
        assert!(images.binding("d").is_none());
        // One that cannot be decoded is not tried again.
        let broken = ("broken".to_owned(), Arc::<[u8]>::from(&b"broken"[..]));
        settle(&mut images, &gpu, std::slice::from_ref(&broken), |images| {
            images.failed.contains("broken")
        });
        images.update(&gpu.device, &gpu.queue, &[broken]);
        assert!(images.decoding.is_empty());
    });
}

#[test]
fn into_a_target_that_is_not_srgb_textures_are_sampled_as_stored() {
    crate::test_gpu::with(|gpu| {
        let mut images = images_into(&gpu.device, wgpu::TextureFormat::Rgba8Unorm);
        assert!(!images.srgb);
        let shown = [("a".to_owned(), png(16, 5))];
        settle(&mut images, &gpu, &shown, |images| {
            images.binding("a").is_some()
        });
        assert_eq!(images.levels("a"), Some((0, 5)));
    });
}
