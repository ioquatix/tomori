// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Tomori's own spectator, which the `tomori-spectator` executable runs: the sceneries of Tomori's own environments, registered from each environment's own crate, as Tomori's own app registers them. The library registers none itself, so that it depends on no environment's crate; this module is the default `app` feature. See `design/builders.md`.
use tomori_render::scenery::Sceneries;

/// What Tomori's own spectator draws: the grid, Sky from `tomori-environment-sky-client`, at Sky's version, the farmyard from `tomori-environment-farmyard-client`, and the GitHub prototype from `tomori-environment-github-client`, presented over the grid, whose cards the spectator does not show yet.
pub fn sceneries() -> Sceneries {
    let mut sceneries = Sceneries::empty();
    tomori_environment_sky_client::register(&mut sceneries);
    tomori_environment_farmyard_client::register(&mut sceneries);
    tomori_environment_github_client::register(&mut sceneries);
    sceneries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tomoris_own_spectator_presents_the_grid_sky_the_farmyard_and_the_github_prototype() {
        let presented: Vec<(&str, u32)> = sceneries()
            .environments()
            .into_iter()
            .chain(sceneries().over_grid())
            .map(|environment| (environment.id(), environment.version()))
            .collect();
        assert_eq!(
            presented,
            [("minimal", 1), ("sky", 1), ("farmyard", 1), ("github", 1)]
        );
        // The farmyard's teleport profile, which a spectator, following a participant, never uses itself.
        assert_eq!(sceneries().locomotion().len(), 1);
    }
}
