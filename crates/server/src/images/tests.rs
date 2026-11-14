// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::BTreeSet;

use image::{ImageEncoder, Rgb, RgbImage, Rgba, RgbaImage};

use super::*;

/// A folder of its own for one test, removed first, beneath the system's temporary folder.
fn folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!(
        "tomori-images-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

/// A synthetic picture of `width` by `height`: red rising to the right, green rising downwards.
fn picture(width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, y| {
        Rgb([
            (x * 255 / width.max(2).saturating_sub(1).max(1)) as u8,
            (y * 255 / height.max(2).saturating_sub(1).max(1)) as u8,
            96,
        ])
    })
}

fn encoded(image: &RgbImage, format: ImageFormat) -> Vec<u8> {
    let mut bytes = io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}

/// The image clients are sent, decoded.
fn sent(prepared: &Prepared) -> RgbImage {
    let decoded = image::load_from_memory_with_format(&prepared.png, ImageFormat::Png).unwrap();
    assert_eq!(decoded.color(), image::ColorType::Rgb8);
    decoded.into_rgb8()
}

#[test]
fn png_jpeg_webp_and_gif_files_are_sent_as_a_png_named_by_its_hash() {
    let folder = folder("formats");
    let picture = picture(64, 48);
    for (name, format) in [
        ("a.png", ImageFormat::Png),
        ("b.jpg", ImageFormat::Jpeg),
        ("c.webp", ImageFormat::WebP),
        ("d.gif", ImageFormat::Gif),
    ] {
        fs::write(folder.join(name), encoded(&picture, format)).unwrap();
        // A path relative to the space's directory, or absolute.
        for path in [name.to_owned(), folder.join(name).display().to_string()] {
            let prepared = prepare(&folder, &path).unwrap();
            assert_eq!((prepared.image.width, prepared.image.height), (64, 48));
            assert_eq!(prepared.image.bytes as usize, prepared.png.len());
            let hash: [u8; 32] = Sha256::digest(&prepared.png).into();
            assert_eq!(prepared.image.hash, hash_text(&hash));
            assert!(prepared.image.is_valid());
            let sent = sent(&prepared);
            // Lossless formats arrive exactly, and lossy ones nearly.
            let pixel = sent.get_pixel(63, 0).0;
            assert!(pixel[0] > 240 && pixel[1] < 16, "{name}: {pixel:?}");
        }
    }
    // The same picture is the same image, however it was named.
    fs::write(folder.join("copy.png"), encoded(&picture, ImageFormat::Png)).unwrap();
    assert_eq!(
        prepare(&folder, "copy.png").unwrap().image,
        prepare(&folder, "a.png").unwrap().image
    );
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_file_that_is_missing_a_folder_or_not_regular_is_refused() {
    let folder = folder("refused");
    assert_eq!(
        prepare(&folder, "missing.png").unwrap_err(),
        "There is no file at that path."
    );
    assert_eq!(
        prepare(&folder, "").unwrap_err(),
        "Give the path of an image file."
    );
    assert_eq!(
        prepare(&folder, &"a".repeat(PATH_BYTES + 1)).unwrap_err(),
        "That path is too long."
    );
    fs::create_dir(folder.join("inside")).unwrap();
    assert_eq!(
        prepare(&folder, "inside").unwrap_err(),
        "That is a folder, not an image file."
    );
    // Replaced by a folder once its path was checked, what was opened cannot be read, and says so.
    assert!(
        read_file(fs::File::open(folder.join("inside")).unwrap())
            .unwrap_err()
            .starts_with("That file could not be read: ")
    );
    // Replaced by a device, it is read only so far.
    #[cfg(unix)]
    assert_eq!(
        read_file(fs::File::open("/dev/zero").unwrap()).unwrap_err(),
        too_large()
    );
    #[cfg(unix)]
    {
        // A pipe with no writer: opening it waits for one unless it is opened without waiting.
        let fifo = folder.join("fifo.png");
        let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert_eq!(
            prepare(&folder, "fifo.png").unwrap_err(),
            "That is not a regular file, such as a device or a pipe, so Tomori will not read it."
        );
        // A device, by a symbolic link whose name says it is an image: the link is followed and the device refused.
        std::os::unix::fs::symlink("/dev/zero", folder.join("zero.png")).unwrap();
        assert!(prepare(&folder, "zero.png").unwrap_err().contains("device"));
        // A link to a picture is followed.
        fs::write(
            folder.join("real.png"),
            encoded(&picture(4, 4), ImageFormat::Png),
        )
        .unwrap();
        std::os::unix::fs::symlink(folder.join("real.png"), folder.join("link.png")).unwrap();
        assert_eq!(prepare(&folder, "link.png").unwrap().image.width, 4);
        // A file it may not read.
        use std::os::unix::fs::PermissionsExt;
        let secret = folder.join("secret.png");
        fs::write(&secret, encoded(&picture(4, 4), ImageFormat::Png)).unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&secret).is_err() {
            assert_eq!(
                prepare(&folder, "secret.png").unwrap_err(),
                "Tomori may not read that file."
            );
        }
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn an_oversized_file_and_one_that_is_not_an_image_it_reads_are_refused() {
    let folder = folder("oversized");
    let large = fs::File::create(folder.join("large.png")).unwrap();
    large.set_len(SOURCE_BYTES + 1).unwrap();
    assert_eq!(
        prepare(&folder, "large.png").unwrap_err(),
        "That file is larger than 20 MB, the most Tomori reads for an image."
    );
    let unsupported = "That file is not a PNG, JPEG, WebP or GIF image.";
    fs::write(folder.join("notes.png"), "Not a picture at all.").unwrap();
    assert_eq!(prepare(&folder, "notes.png").unwrap_err(), unsupported);
    // A format the `image` crate knows, but Tomori does not read.
    let mut bitmap = b"BM".to_vec();
    bitmap.extend([0; 64]);
    fs::write(folder.join("picture.bmp"), bitmap).unwrap();
    assert_eq!(prepare(&folder, "picture.bmp").unwrap_err(), unsupported);
    // A PNG cut short.
    let mut cut = encoded(&picture(64, 64), ImageFormat::Png);
    cut.truncate(cut.len() / 2);
    fs::write(folder.join("cut.png"), cut).unwrap();
    assert!(
        prepare(&folder, "cut.png")
            .unwrap_err()
            .starts_with("That image could not be read: ")
    );
    fs::remove_dir_all(folder).unwrap();
}

/// The CRC-32 of a PNG chunk, as PNG computes it.
fn crc(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A PNG that claims to be `width` by `height`, of grey, with almost nothing after its header.
fn claiming(width: u32, height: u32) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        png.extend((data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend(data);
        png.extend(&body);
        png.extend(crc(&body).to_be_bytes());
    };
    let mut header = width.to_be_bytes().to_vec();
    header.extend(height.to_be_bytes());
    header.extend([8, 0, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
    chunk(b"IEND", &[]);
    png
}

#[test]
fn a_decompression_bomb_is_refused_before_it_is_decoded() {
    let folder = folder("bomb");
    let refusal =
        "That image is too large to show: at most 50 megapixels, and 16384 pixels on a side.";
    // Too many pixels, though each side is allowed.
    fs::write(folder.join("many.png"), claiming(10_000, 10_000)).unwrap();
    assert_eq!(prepare(&folder, "many.png").unwrap_err(), refusal);
    // A side too long.
    fs::write(folder.join("wide.png"), claiming(SOURCE_SIDE + 1, 1)).unwrap();
    assert_eq!(prepare(&folder, "wide.png").unwrap_err(), refusal);
    // A real one: 56 megapixels of black, which compresses to almost nothing.
    let bomb = image::GrayImage::new(8_000, 7_000);
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut png,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::NoFilter,
    )
    .write_image(bomb.as_raw(), 8_000, 7_000, image::ExtendedColorType::L8)
    .unwrap();
    assert!(png.len() < 1024 * 1024);
    fs::write(folder.join("bomb.png"), png).unwrap();
    assert_eq!(prepare(&folder, "bomb.png").unwrap_err(), refusal);
    // An image of no pixels.
    fs::write(folder.join("empty.png"), claiming(0, 0)).unwrap();
    assert!(prepare(&folder, "empty.png").is_err());
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_large_image_is_scaled_down_keeping_its_shape() {
    let folder = folder("scaled");
    fs::write(
        folder.join("wide.png"),
        encoded(&picture(4_096, 1_024), ImageFormat::Png),
    )
    .unwrap();
    let prepared = prepare(&folder, "wide.png").unwrap();
    assert_eq!(
        (prepared.image.width, prepared.image.height),
        (IMAGE_SIDE, 512)
    );
    assert_eq!(sent(&prepared).dimensions(), (IMAGE_SIDE, 512));
    // A tall one, by its longer side.
    fs::write(
        folder.join("tall.png"),
        encoded(&picture(300, 3_000), ImageFormat::Png),
    )
    .unwrap();
    let tall = prepare(&folder, "tall.png").unwrap();
    assert_eq!((tall.image.width, tall.image.height), (205, IMAGE_SIDE));
    fs::remove_dir_all(folder).unwrap();
}

/// A picture of `width` by `height`, mid grey, edged one pixel wide in red along its top, green along its bottom, blue
/// down its left and yellow down its right.
fn edged(width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, y| match (x, y) {
        (_, 0) => Rgb([255, 0, 0]),
        (_, y) if y == height - 1 => Rgb([0, 255, 0]),
        (0, _) => Rgb([0, 0, 255]),
        (x, _) if x == width - 1 => Rgb([255, 255, 0]),
        _ => Rgb([128, 128, 128]),
    })
}

#[test]
fn an_image_needing_no_scaling_keeps_its_size_and_every_edge_pixel() {
    let folder = folder("edges");
    // The headset's test image's size, a tall one, an odd one, the two renders the maintainer saw as one tall image, and one
    // as large as is sent, opaque and transparent: whatever slot it is shown in, it is sent at its own size.
    for (width, height) in [
        (1_024, 768),
        (768, 1_024),
        (333, 101),
        (960, 1_440),
        (IMAGE_SIDE, 1_536),
    ] {
        let picture = edged(width, height);
        // With an alpha channel, though wholly opaque, it is laid over white, which changes nothing.
        let alpha = RgbaImage::from_fn(width, height, |x, y| {
            let Rgb([red, green, blue]) = *picture.get_pixel(x, y);
            Rgba([red, green, blue, 255])
        });
        let mut bytes = io::Cursor::new(Vec::new());
        alpha.write_to(&mut bytes, ImageFormat::Png).unwrap();
        fs::write(folder.join("alpha.png"), bytes.into_inner()).unwrap();
        fs::write(
            folder.join("opaque.png"),
            encoded(&picture, ImageFormat::Png),
        )
        .unwrap();
        for name in ["opaque.png", "alpha.png"] {
            let prepared = prepare(&folder, name).unwrap();
            assert_eq!(
                (prepared.image.width, prepared.image.height),
                (width, height),
                "{name}"
            );
            // Pixel for pixel what was read, its last row and column included.
            assert_eq!(sent(&prepared), picture, "{name}, {width} × {height}");
        }
    }
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn an_image_whose_png_is_too_large_is_halved_until_it_fits() {
    // Noise does not compress: 2048 by 2048 of it is about 12 MB as a PNG, and 1024 by 1024 about 3 MB.
    let mut state = 0x2545_f491_u32;
    let noise = RgbImage::from_fn(IMAGE_SIDE, IMAGE_SIDE, |_, _| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let [red, green, blue, _] = state.to_le_bytes();
        Rgb([red, green, blue])
    });
    let prepared = encode(noise);
    assert!(prepared.png.len() <= IMAGE_BYTES as usize);
    assert_eq!(
        (prepared.image.width, prepared.image.height),
        (IMAGE_SIDE / 2, IMAGE_SIDE / 2)
    );
}

/// EXIF saying that the picture is to be turned a quarter clockwise to be seen upright, as a phone held upright writes.
fn turned_exif() -> Vec<u8> {
    let mut tiff = b"MM\x00\x2a\x00\x00\x00\x08".to_vec();
    tiff.extend([0x00, 0x01]);
    tiff.extend([
        0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x00, 0x00,
    ]);
    tiff.extend([0x00, 0x00, 0x00, 0x00]);
    tiff
}

#[test]
fn exif_orientation_is_applied_and_metadata_is_not_sent() {
    let folder = folder("orientation");
    // Wider than tall as stored, with its red corner at the top right.
    let stored = picture(80, 40);
    let mut jpeg = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95);
    encoder.set_exif_metadata(turned_exif()).unwrap();
    encoder
        .write_image(stored.as_raw(), 80, 40, image::ExtendedColorType::Rgb8)
        .unwrap();
    assert!(jpeg.windows(4).any(|window| window == b"Exif"));
    fs::write(folder.join("phone.jpg"), jpeg).unwrap();
    let prepared = prepare(&folder, "phone.jpg").unwrap();
    // Turned upright, it is taller than wide, and the top right corner is now at the bottom right.
    assert_eq!((prepared.image.width, prepared.image.height), (40, 80));
    let sent = sent(&prepared);
    let corner = sent.get_pixel(39, 79).0;
    assert!(corner[0] > 200 && corner[1] < 60, "{corner:?}");
    // What clients are sent carries no metadata: only the chunks of its pixels.
    let chunks = png_chunks(&prepared.png);
    assert_eq!(chunks, ["IHDR", "IDAT", "IEND"]);
    fs::remove_dir_all(folder).unwrap();
}

/// The kinds of the chunks of `png`, in order, the image's data once.
fn png_chunks(png: &[u8]) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    let mut at = 8;
    while at + 8 <= png.len() {
        let length = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        let kind = String::from_utf8_lossy(&png[at + 4..at + 8]).into_owned();
        if kinds.last() != Some(&kind) {
            kinds.push(kind);
        }
        at += 12 + length;
    }
    kinds
}

#[test]
fn transparency_is_laid_over_white() {
    let folder = folder("transparent");
    let mut picture = RgbaImage::from_pixel(4, 2, Rgba([0, 0, 0, 0]));
    picture.put_pixel(1, 0, Rgba([0, 0, 0, 255]));
    picture.put_pixel(2, 0, Rgba([255, 0, 0, 128]));
    let mut png = io::Cursor::new(Vec::new());
    picture.write_to(&mut png, ImageFormat::Png).unwrap();
    fs::write(folder.join("diagram.png"), png.into_inner()).unwrap();
    let sent = sent(&prepare(&folder, "diagram.png").unwrap());
    assert_eq!(sent.get_pixel(0, 0).0, [255, 255, 255]);
    assert_eq!(sent.get_pixel(1, 0).0, [0, 0, 0]);
    assert_eq!(sent.get_pixel(2, 0).0, [255, 127, 127]);
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn the_store_keeps_images_by_hash_and_sweeps_those_no_panel_shows() {
    crate::test_log::record();
    let folder = folder("store");
    let source = folder.join("source");
    fs::create_dir(&source).unwrap();
    let make = |width| {
        fs::write(
            source.join("p.png"),
            encoded(&picture(width, 8), ImageFormat::Png),
        )
        .unwrap();
        prepare(&source, "p.png").unwrap()
    };
    let (first, second) = (make(8), make(16));
    // In memory alone, as before the server keeps spaces.
    let store = ImageStore::new();
    store.put(&first).unwrap();
    assert_eq!(store.get(&first.image.hash).unwrap(), first.png);
    assert!(store.get(&second.image.hash).is_none());
    assert!(store.get("not a hash").is_none());
    store.sweep_older(&BTreeSet::new(), Duration::ZERO);
    assert!(store.get(&first.image.hash).is_none());
    // In the state directory.
    let images = folder.join("images");
    let store = ImageStore::new();
    store.keep_in(images.clone()).unwrap();
    store.put(&first).unwrap();
    store.put(&second).unwrap();
    let file = images.join(format!("{}.png", first.image.hash));
    assert_eq!(fs::read(&file).unwrap(), &first.png[..]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // Read again from the directory by a store started afresh, as after a restart.
    let again = ImageStore::new();
    again.keep_in(images.clone()).unwrap();
    assert_eq!(again.get(&first.image.hash).unwrap(), first.png);
    // A file that is not what its name says is not sent.
    fs::write(
        images.join(format!("{}.png", second.image.hash)),
        b"tampered",
    )
    .unwrap();
    assert!(again.get(&second.image.hash).is_none());
    // Sweeping keeps what panels show, what is new, and what is not an image.
    fs::write(images.join("notes.txt"), b"kept").unwrap();
    fs::write(images.join(format!("{}.tmp", second.image.hash)), b"left").unwrap();
    let left = images.join(format!("{}.png.1234.5.tmp", second.image.hash));
    fs::write(&left, b"left").unwrap();
    let shown = BTreeSet::from([first.image.hash.clone()]);
    store.sweep(shown.clone());
    assert!(images.join(format!("{}.png", second.image.hash)).exists());
    store.sweep_older(&shown, Duration::ZERO);
    assert!(file.exists() && images.join("notes.txt").exists());
    assert!(!images.join(format!("{}.png", second.image.hash)).exists());
    assert!(!images.join(format!("{}.tmp", second.image.hash)).exists());
    assert!(!left.exists());
    assert!(store.get(&second.image.hash).is_none());
    // The same images shown again sweep nothing; none shown, everything old goes.
    store.sweep(shown);
    store.sweep_older(&BTreeSet::new(), Duration::ZERO);
    assert!(!file.exists());
    // What cannot be read or removed, such as a folder with an image's name, is neither sent nor removed, and says so.
    let stuck = images.join(format!("{}.png", second.image.hash));
    fs::create_dir(&stuck).unwrap();
    assert!(again.get(&second.image.hash).is_none());
    store.sweep_older(&BTreeSet::new(), Duration::ZERO);
    assert!(stuck.exists());
    // An image that cannot be written is not kept, and says why; a folder that has gone is swept of nothing.
    fs::remove_dir_all(&images).unwrap();
    fs::write(&images, b"not a folder").unwrap();
    assert!(
        store
            .put(&first)
            .unwrap_err()
            .starts_with("Tomori could not keep the image: ")
    );
    store.sweep_older(&BTreeSet::new(), Duration::ZERO);
    // Nor can images be kept in a folder that cannot be made.
    assert!(ImageStore::new().keep_in(images.join("inside")).is_err());
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn memory_keeps_only_the_latest_images_while_a_directory_keeps_them_all() {
    let folder = folder("memory");
    let store = ImageStore::new();
    store.keep_in(folder.clone()).unwrap();
    // Each as large as an image sent may be, so that more than fit in memory are kept.
    let image = |index: u8| {
        let png: Arc<[u8]> = vec![index; IMAGE_BYTES as usize].into();
        let hash: [u8; 32] = Sha256::digest(&png).into();
        Prepared {
            image: Image::new(hash_text(&hash), 1, 1, png.len() as u32),
            png,
        }
    };
    let fit = (MEMORY_BYTES / IMAGE_BYTES as usize) as u8;
    let images: Vec<Prepared> = (0..=fit).map(image).collect();
    for prepared in &images {
        store.put(prepared).unwrap();
    }
    let memory = store.memory.lock().unwrap();
    assert_eq!(memory.images.len(), fit as usize);
    assert!(!memory.images.contains_key(&images[0].image.hash));
    drop(memory);
    // The one forgotten in memory is read again from the directory.
    assert_eq!(store.get(&images[0].image.hash).unwrap(), images[0].png);
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn the_same_image_kept_by_several_spaces_at_once_is_kept_whole_for_each() {
    let folder = folder("together");
    let store = ImageStore::new();
    store.keep_in(folder.clone()).unwrap();
    // Large enough that writing it takes a while, so that the writes overlap.
    let png: Arc<[u8]> = (0..IMAGE_BYTES).map(|byte| byte as u8).collect();
    let hash: [u8; 32] = Sha256::digest(&png).into();
    let prepared = Prepared {
        image: Image::new(hash_text(&hash), 1, 1, png.len() as u32),
        png,
    };
    std::thread::scope(|scope| {
        let puts: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    (0..8)
                        .map(|_| store.put(&prepared))
                        .collect::<Result<Vec<()>, String>>()
                })
            })
            .collect();
        for put in puts {
            assert_eq!(put.join().unwrap(), Ok(vec![(); 8]));
        }
    });
    let file = folder.join(format!("{}.png", prepared.image.hash));
    assert_eq!(fs::read(&file).unwrap(), &prepared.png[..]);
    fs::remove_dir_all(folder).unwrap();
}
