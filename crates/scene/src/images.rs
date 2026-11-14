// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The images of the joined space's image panels, as the client fetches and holds them: each is fetched once, by its
//! SHA-256, however many panels show it, a few at a time, so that a wall of images never queues more than the server
//! takes, and held, as the PNG the server made of it, across reconnecting and changing space. Only the renderer decodes
//! them. See `design/projection.md`.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tomori_protocol::{Image, Projection};

/// The most bytes of images held: those beyond it that no panel shows are forgotten, the least recently shown first.
pub const HELD_BYTES: usize = 64 * 1024 * 1024;

/// The most images asked for and not yet arrived.
pub const ASKING: usize = 2;

/// How long an image asked for may take to arrive before it is asked for again.
pub const RETRY: Duration = Duration::from_secs(20);

/// How many times an image is asked for before it is given up on, until the client reconnects.
pub const TRIES: u32 = 3;

/// An image asked for: when last, and how many times.
#[derive(Clone, Copy, Debug)]
struct Asked {
    at: Instant,
    tries: u32,
}

/// The images held and asked for.
#[derive(Clone, Debug, Default)]
pub struct Images {
    /// By hash, each with when it was last shown, in [`Images::wanted`]'s count.
    held: HashMap<String, (Arc<[u8]>, u64)>,
    bytes: usize,
    asked: HashMap<String, Asked>,
    /// How many times [`Images::wanted`] has been asked, by which the least recently shown is found.
    count: u64,
}

/// The images `projection`'s panels show, valid ones only, each once.
pub fn shown(projection: &Projection) -> Vec<&Image> {
    let mut images: Vec<&Image> = Vec::new();
    for image in projection
        .panels
        .iter()
        .filter_map(|panel| panel.image.as_ref())
        .filter(|image| image.is_valid())
    {
        if !images.iter().any(|known| known.hash == image.hash) {
            images.push(image);
        }
    }
    images
}

impl Images {
    /// The images the faces of `sheets` show, of those held, each with its PNG, for the renderer to decode and draw over
    /// them: as the frame step and the spectator hand them over.
    pub fn drawn(&self, sheets: &[crate::controller_ui::Sheet]) -> Vec<(String, Arc<[u8]>)> {
        sheets
            .iter()
            .filter_map(|sheet| {
                sheet
                    .canvas
                    .as_deref()
                    .and_then(|canvas| canvas.image.as_ref())
            })
            .filter_map(|image| {
                self.png(&image.hash)
                    .map(|png| (image.hash.clone(), png.clone()))
            })
            .collect()
    }

    /// The image `hash`, if it is held.
    pub fn png(&self, hash: &str) -> Option<&Arc<[u8]>> {
        self.held.get(hash).map(|(png, _)| png)
    }

    /// The images to ask for at `now`, of those `shown`: each neither held nor asked for, or asked for more than
    /// [`RETRY`] ago fewer than [`TRIES`] times, while fewer than [`ASKING`] are on their way.
    pub fn wanted(&mut self, shown: &[&Image], now: Instant) -> Vec<String> {
        self.count += 1;
        let count = self.count;
        for image in shown {
            if let Some((_, used)) = self.held.get_mut(&image.hash) {
                *used = count;
            }
        }
        let late = |asked: &Asked| now.saturating_duration_since(asked.at) >= RETRY;
        let mut asking = self.asked.values().filter(|asked| !late(asked)).count();
        let mut wanted = Vec::new();
        for image in shown {
            if asking >= ASKING {
                break;
            }
            if self.held.contains_key(&image.hash) {
                continue;
            }
            let asked = self.asked.get(&image.hash).copied();
            if asked.is_some_and(|asked| !late(&asked) || asked.tries >= TRIES) {
                continue;
            }
            let tries = asked.map_or(0, |asked| asked.tries) + 1;
            self.asked
                .insert(image.hash.clone(), Asked { at: now, tries });
            asking += 1;
            wanted.push(image.hash.clone());
        }
        wanted
    }

    /// Hold `png`, the image `hash` that arrived, forgetting those least recently shown beyond [`HELD_BYTES`], but never
    /// one `shown` now.
    pub fn arrived(&mut self, hash: String, png: Arc<[u8]>, shown: &[&Image]) {
        if !self.asked.contains_key(&hash) {
            // Not asked for, as from before reconnecting: kept all the same, since it was checked.
            log::debug!("Holding an image that arrived without being asked for.");
        }
        self.asked.remove(&hash);
        if let Some((old, _)) = self.held.remove(&hash) {
            self.bytes -= old.len();
        }
        self.bytes += png.len();
        self.held.insert(hash, (png, self.count));
        while self.bytes > HELD_BYTES {
            let oldest = self
                .held
                .iter()
                .filter(|(hash, _)| !shown.iter().any(|image| image.hash == **hash))
                .min_by_key(|(_, (_, used))| *used)
                .map(|(hash, _)| hash.clone());
            let Some(oldest) = oldest else {
                break;
            };
            let (png, _) = self.held.remove(&oldest).expect("found above");
            self.bytes -= png.len();
        }
    }

    /// The connection went: what was asked for will not arrive, and is asked for again, as many times, once connected.
    pub fn disconnected(&mut self) {
        self.asked.clear();
    }

    /// How many bytes of images are held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tomori_protocol::{Panel, Place};

    fn image(digit: char, bytes: u32) -> Image {
        Image::new(digit.to_string().repeat(64), 640, 480, bytes)
    }

    #[test]
    fn images_are_asked_for_a_few_at_a_time_each_once_and_again_when_late() {
        let start = Instant::now();
        let (a, b, c) = (image('a', 10), image('b', 10), image('c', 10));
        let mut images = Images::default();
        let shown = [&a, &b, &c, &a];
        assert_eq!(
            images.wanted(&shown, start),
            [a.hash.clone(), b.hash.clone()]
        );
        // Nothing more while two are on their way.
        assert!(images.wanted(&shown, start).is_empty());
        images.arrived(a.hash.clone(), vec![1; 10].into(), &shown);
        assert_eq!(images.png(&a.hash).unwrap()[..], [1; 10]);
        assert_eq!(images.wanted(&shown, start), std::slice::from_ref(&c.hash));
        // One that does not arrive is asked for again once late, as many as TRIES times.
        let mut at = start;
        for _ in 1..TRIES {
            at += RETRY;
            assert_eq!(images.wanted(&[&b], at), std::slice::from_ref(&b.hash));
        }
        at += RETRY;
        assert!(images.wanted(&[&b], at).is_empty());
        // Reconnected, it is asked for again.
        images.disconnected();
        assert_eq!(images.wanted(&[&b], at), std::slice::from_ref(&b.hash));
    }

    #[test]
    fn images_beyond_what_is_held_are_forgotten_least_recently_shown_first_but_never_one_shown() {
        crate::test_support::log_everything();
        let size = (HELD_BYTES / 2) as u32;
        let (a, b, c) = (image('a', size), image('b', size), image('c', size));
        let start = Instant::now();
        let mut images = Images::default();
        let png = || -> Arc<[u8]> { vec![0; HELD_BYTES / 2].into() };
        images.wanted(&[&a, &b], start);
        images.arrived(a.hash.clone(), png(), &[&a, &b]);
        images.arrived(b.hash.clone(), png(), &[&a, &b]);
        assert_eq!(images.bytes(), HELD_BYTES);
        // Only b is shown now; a, shown least recently, goes to make room for c.
        images.wanted(&[&b, &c], start);
        images.arrived(c.hash.clone(), png(), &[&b, &c]);
        assert!(images.png(&a.hash).is_none());
        assert!(images.png(&b.hash).is_some() && images.png(&c.hash).is_some());
        // When every one held is shown, none is forgotten.
        images.arrived(a.hash.clone(), png(), &[&a, &b, &c]);
        assert_eq!(images.bytes(), HELD_BYTES * 3 / 2);
        // Arriving again replaces what was held.
        images.arrived(a.hash.clone(), vec![0; 4].into(), &[&a, &b, &c]);
        assert_eq!(images.bytes(), HELD_BYTES + 4);
    }

    #[test]
    fn a_projection_shows_its_valid_images_once_each() {
        let place = Place::Unknown;
        let mut projection = Projection::new("s", "sky");
        projection.panels = vec![
            Panel::new("1", "", "", place.clone()).with_image(image('a', 10)),
            Panel::new("2", "", "", place.clone()).with_image(image('a', 10)),
            Panel::new("3", "", "", place.clone()),
            Panel::new("4", "", "", place.clone()).with_image(Image::new("nope", 1, 1, 1)),
            Panel::new("5", "", "", place).with_image(image('b', 10)),
        ];
        let shown: Vec<&str> = shown(&projection)
            .into_iter()
            .map(|image| image.hash.as_str())
            .collect();
        assert_eq!(shown, ["a".repeat(64), "b".repeat(64)]);
    }
}
