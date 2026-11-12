// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! How many views an eye's render pass draws: one, each eye in a pass of its own, or both at once by multiview, the
//! eye then chosen in each shader by its view index. GPU-free, so that the server and tests can name it; see
//! `design/rendering.md`.
//!
//! A shader drawn in an eye's pass is written for one view, as valid WGSL, with three marks that [`Views::shader`]
//! rewrites for both:
//!
//! - its eyes' uniform an array of `1 /* views */`, which becomes 2;
//! - each entry point that reads it taking `/* view_index */` as its first parameter, which becomes the view index;
//! - each read indexing the array by `0u /* view */`, which becomes that view index.
//!
//! ```wgsl
//! @group(0) @binding(0) var<uniform> cameras: array<Camera, 1 /* views */>;
//!
//! @vertex
//! fn vs_main(/* view_index */ @location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
//!     return cameras[0u /* view */].view_projection * vec4<f32>(position, 1.0);
//! }
//! ```
use std::num::NonZeroU32;

/// The mark of the number of views an eye's uniform holds.
pub const VIEWS_MARK: &str = "1 /* views */";
/// The mark of an entry point's view index, as its first parameter.
pub const VIEW_INDEX_MARK: &str = "/* view_index */";
/// The mark of where a shader reads the view it draws.
pub const VIEW_MARK: &str = "0u /* view */";

/// How many views an eye's render pass draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Views {
    /// One: each eye in a pass of its own, as the desktop window and the spectator draw their one view, and the
    /// headset its two where multiview is off or cannot be used.
    #[default]
    One,
    /// Both eyes in one pass, by multiview, into two-layer targets, each eye's layer drawn with its own view.
    Both,
}

impl Views {
    /// How many views each pass draws, and how many of an eye's uniforms each binding holds.
    pub const fn count(self) -> usize {
        match self {
            Self::One => 1,
            Self::Both => 2,
        }
    }

    /// The views each pass draws, as a pipeline and a render pass name them: none for one, or the first two layers.
    pub const fn mask(self) -> Option<NonZeroU32> {
        match self {
            Self::One => None,
            Self::Both => NonZeroU32::new(0b11),
        }
    }

    /// How many passes draw `eyes` eyes: one for each, or one for both.
    pub fn passes(self, eyes: usize) -> usize {
        match self {
            Self::One => eyes,
            Self::Both => eyes.min(1),
        }
    }

    /// The pass that draws `eye`: its own, or the one that draws both.
    pub fn pass(self, eye: usize) -> usize {
        match self {
            Self::One => eye,
            Self::Both => 0,
        }
    }

    /// `source`, a shader written for one view with the marks the module describes, for these views: as it is for one,
    /// and for both with its eyes' uniforms arrays of two, indexed by each entry point's view index.
    pub fn shader(self, source: &str) -> String {
        match self {
            Self::One => source.to_owned(),
            Self::Both => source
                .replace(VIEWS_MARK, "2")
                .replace(VIEW_INDEX_MARK, "@builtin(view_index) view_index: u32,")
                .replace(VIEW_MARK, "view_index"),
        }
    }

    /// The name the log gives them.
    pub fn name(self) -> &'static str {
        match self {
            Self::One => "one view a pass",
            Self::Both => "multiview",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str =
        "@group(0) @binding(0) var<uniform> cameras: array<mat4x4<f32>, 1 /* views */>;
@vertex
fn vs_main(/* view_index */ @location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return cameras[0u /* view */] * vec4<f32>(position, 1.0);
}
";

    #[test]
    fn one_view_draws_each_eye_in_its_own_pass_and_both_draw_them_in_one() {
        assert_eq!(Views::default(), Views::One);
        assert_eq!((Views::One.count(), Views::Both.count()), (1, 2));
        assert_eq!(Views::One.mask(), None);
        assert_eq!(Views::Both.mask().map(NonZeroU32::get), Some(0b11));
        assert_eq!((Views::One.passes(2), Views::Both.passes(2)), (2, 1));
        assert_eq!((Views::One.passes(1), Views::Both.passes(0)), (1, 0));
        assert_eq!((Views::One.pass(1), Views::Both.pass(1)), (1, 0));
        assert_eq!(Views::One.name(), "one view a pass");
        assert_eq!(Views::Both.name(), "multiview");
    }

    #[test]
    fn a_shader_for_one_view_is_rewritten_for_both() {
        assert_eq!(Views::One.shader(SOURCE), SOURCE);
        let both = Views::Both.shader(SOURCE);
        assert!(both.contains("array<mat4x4<f32>, 2>"));
        assert!(both.contains("fn vs_main(@builtin(view_index) view_index: u32, @location(0)"));
        assert!(both.contains("cameras[view_index] *"));
        assert!(!both.contains("/*"));
    }
}
