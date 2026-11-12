// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Tomori's wgpu renderers and shaders, shared by the headset, the desktop window and the spectator: the swarm and its workers, arrival, the panels, the controllers' models and the sceneries registered with it, and [`scene_graphics::draw`], which applies a [`tomori_scene::scene::Frame`] to the [`graphics::Renderer`].
//!
//! It holds no window, device or platform code: each client creates its own device and targets, and hands them to the renderer. Its interfaces change with the environment framework ([#75](https://github.com/base-rs/tomori/issues/75), [#77](https://github.com/base-rs/tomori/issues/77)).
/// A shader module of `$file`, one of this crate's, after the shared haze functions, `tomori_assets::haze`, which it
/// fades what it draws by, for the tests' own pipelines of one view; the renderer's are made by [`multiview::shader`]
/// for its views. See `design/rendering.md`.
#[cfg(test)]
macro_rules! hazed_wgsl {
    ($file:literal) => {
        wgpu::ShaderModuleDescriptor {
            label: Some($file),
            source: wgpu::ShaderSource::Wgsl(
                tomori_assets::haze::shader(include_str!($file)).into(),
            ),
        }
    };
}

/// The scene's haze, shared by every pipeline that draws in the world, this crate's and its sceneries'.
pub use tomori_assets::haze;

pub mod arrival_graphics;
pub mod controller;
pub mod controller_graphics;
pub mod glyphs;
pub mod graphics;
pub mod multiview;
pub mod occlusion;
pub mod panel_images;
pub mod profiler;
pub mod scene_graphics;
pub mod scenery;
#[cfg(test)]
mod shared_haze;
pub mod solids;
#[cfg(any(test, feature = "test-gpu"))]
pub mod test_gpu;
#[cfg(test)]
mod test_support;
pub mod ui_panel;
pub mod workspace_graphics;

/// The render world, which the renderer and its sceneries submit what they draw to, and which culls it, so that a
/// scenery need not depend on it itself. See `design/rendering.md`.
pub use tomori_world as world;

// The platform-neutral modules, from `tomori-client` and `tomori-scene`, under the paths this crate's own modules use.
use tomori_client::server_choice;
use tomori_scene::{
    arrival, arrival_motion, avatar, controller_ui, delegation, environment, hand_menu, hands,
    interaction, locomotion, panel, scene, swarm, ui_text,
};

/// The licence of the bundled controller models, which [`controller_graphics`] draws.
pub const CONTROLLER_LICENSE: &str = include_str!("../assets/controllers/LICENSE.md");
/// The licence of the Basis Universal transcoder, which decodes the controllers' KTX2 textures in [`controller`].
pub const BASISU_LICENSE: &str = include_str!("../assets/licenses/basisu-LICENSE.txt");
/// The licences of the fonts that text is drawn with, which `tomori-text` bundles, also in `assets/licenses/fonts-LICENSE.txt` for an APK's assets.
pub const FONT_LICENSES: &str = tomori_text::fonts::LICENSES;
/// The licence notices of what this crate compiles in, which a headset's APK carries in its assets, each by its path beneath the APK's `assets/` and its text: the controller models', the Basis Universal transcoder's and the fonts'. They are this package's own `assets/controllers/LICENSE.md` and `assets/licenses`, so a build of another project's can copy those folders or write these, and check its copies against them in a test; see `crates/client-quest/context/android.md`.
pub const LICENSE_NOTICES: &[(&str, &str)] = &[
    ("controllers/LICENSE.md", CONTROLLER_LICENSE),
    ("licenses/basisu-LICENSE.txt", BASISU_LICENSE),
    ("licenses/fonts-LICENSE.txt", FONT_LICENSES),
];

#[cfg(test)]
mod tests {
    #[test]
    fn the_licence_notices_are_this_packages_assets() {
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        for &(path, text) in super::LICENSE_NOTICES {
            assert_eq!(
                std::fs::read_to_string(assets.join(path)).unwrap(),
                text,
                "{path}"
            );
        }
        // Every licence in the assets is among them.
        let mut licenses: Vec<String> = std::fs::read_dir(assets.join("licenses"))
            .unwrap()
            .map(|entry| format!("licenses/{}", entry.unwrap().file_name().to_str().unwrap()))
            .collect();
        licenses.sort();
        assert_eq!(
            licenses,
            ["licenses/basisu-LICENSE.txt", "licenses/fonts-LICENSE.txt"]
        );
    }

    #[test]
    fn the_font_licences_in_the_assets_are_the_bundled_fonts_own() {
        assert_eq!(
            include_str!("../assets/licenses/fonts-LICENSE.txt"),
            super::FONT_LICENSES
        );
    }
}
