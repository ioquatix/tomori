// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Whether the headset draws both eyes in one render pass, by multiview, as asked: by default each eye in a pass of its
//! own, the left's first, which the Quest 3's GPU draws quicker; both in one where asked for and the GPU and every
//! scenery can; or each in its own, the right's first, to tell a first pass's costs from the left eye's own. See
//! `design/rendering.md`.
use tomori_render::multiview::Views;

/// How the eyes are asked to be drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Both in one pass, by multiview, where the GPU and every scenery can.
    On,
    /// Each in a pass of its own, the left's first, as before multiview.
    Off,
    /// Each in a pass of its own, the right's first, so that the log's `left`, the first pass timed, is the right eye's.
    RightFirst,
}

impl Request {
    /// What `value` asks for, as the headset's `tomori.multiview` intent extra or the desktop's `TOMORI_MULTIVIEW`:
    /// `on`, `off` or `right-first`, or their numbers 1 and 0; off when absent or not understood.
    pub fn parse(value: Option<&str>) -> Self {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Self::Off;
        };
        match value.to_ascii_lowercase().as_str() {
            "on" | "1" => Self::On,
            "off" | "0" => Self::Off,
            "right-first" => Self::RightFirst,
            _ => {
                log::warn!("Ignored tomori.multiview {value:?}: use on, off or right-first");
                Self::Off
            }
        }
    }

    /// The views a pass is asked to draw.
    pub fn views(self) -> Views {
        match self {
            Self::On => Views::Both,
            Self::Off | Self::RightFirst => Views::One,
        }
    }

    /// The eyes in the order they are drawn, each a pass of its own: the left's first, unless the right's is asked for.
    pub fn order(self) -> [usize; 2] {
        match self {
            Self::RightFirst => [1, 0],
            Self::On | Self::Off => [0, 1],
        }
    }

    /// Whether the device is asked for multiview.
    pub fn asks(self) -> bool {
        self.views() == Views::Both
    }
}

/// What the log says of how the eyes were drawn, beside the GPU's frame times: `views` a pass, as asked by `request`.
pub fn summary(request: Request, views: Views) -> &'static str {
    match (views, request) {
        (Views::Both, _) => "both eyes in one pass",
        (Views::One, Request::On) => "each eye a pass, multiview unavailable",
        (Views::One, Request::Off) => "each eye a pass",
        (Views::One, Request::RightFirst) => {
            "each eye a pass, the right first: left is the first pass"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiview_is_asked_for_only_when_turned_on() {
        let parse = |value| Request::parse(Some(value));
        assert_eq!(Request::parse(None), Request::Off);
        assert_eq!(parse(" "), Request::Off);
        assert_eq!(parse("on"), Request::On);
        assert_eq!(parse("1"), Request::On);
        assert_eq!(parse("OFF"), Request::Off);
        assert_eq!(parse("0"), Request::Off);
        assert_eq!(parse("right-first"), Request::RightFirst);
        assert_eq!(parse("sideways"), Request::Off);
        assert_eq!(Request::On.views(), Views::Both);
        assert!(Request::On.asks());
        for request in [Request::Off, Request::RightFirst] {
            assert_eq!(request.views(), Views::One);
            assert!(!request.asks());
        }
        assert_eq!(Request::On.order(), [0, 1]);
        assert_eq!(Request::Off.order(), [0, 1]);
        assert_eq!(Request::RightFirst.order(), [1, 0]);
    }

    #[test]
    fn the_log_says_how_the_eyes_were_drawn() {
        assert_eq!(summary(Request::On, Views::Both), "both eyes in one pass");
        assert_eq!(
            summary(Request::On, Views::One),
            "each eye a pass, multiview unavailable"
        );
        assert_eq!(summary(Request::Off, Views::One), "each eye a pass");
        assert!(summary(Request::RightFirst, Views::One).contains("the right first"));
    }
}
