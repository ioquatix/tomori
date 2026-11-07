// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Tomori's farmyard, its client half: the scenery Tomori's clients draw, an evening sky behind the yard and the coop,
//! built in code and drawn with `tomori-assets`, and the teleport profile, which a client registers with
//! [`register`]. What its server and clients share is `tomori-environment-farmyard`. See `design/farmyard.md`.
pub mod scenery;

pub use tomori_environment_farmyard::{ID, LABEL, VERSION, coop, layout};

/// Where participants may travel in the farmyard: teleporting onto the yard's ground, clear of what stands in it, and
/// turning in snaps. See Tomori's `design/locomotion.md`.
pub fn locomotion() -> tomori_scene::locomotion::Locomotion {
    use tomori_scene::locomotion::{Locomotion, Surface};
    Locomotion::teleport(
        layout::walkable()
            .into_iter()
            .map(|(centre, size)| Surface::rectangle(centre, size))
            .collect(),
    )
}

/// Register the farmyard with a client's `sceneries`: its scenery, at the identity in `tomori-environment-farmyard`,
/// with its coop built in code, and its teleport profile. Tomori's own app and spectator do so; a build of another
/// project's can too.
///
/// ```
/// let mut sceneries = tomori_render::scenery::Sceneries::empty();
/// tomori_environment_farmyard_client::register(&mut sceneries);
/// assert!(sceneries.contains(tomori_environment_farmyard_client::ID));
/// ```
pub fn register(sceneries: &mut tomori_render::scenery::Sceneries) {
    register_with(sceneries, coop::Coop::Built);
}

/// As [`register`], with `coop`, such as one modelled in Blender and bundled as an asset.
pub fn register_with(sceneries: &mut tomori_render::scenery::Sceneries, coop: coop::Coop) {
    sceneries.register_versioned(ID, VERSION, move |target| {
        Box::new(scenery::Farmyard::new(
            target,
            &coop,
            tomori_assets::Library::shared(),
        ))
    });
    sceneries.set_locomotion(ID, locomotion());
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_farmyard_registers_its_scenery_at_its_version_and_lets_participants_teleport_in_the_yard()
     {
        use tomori_scene::environment::Environment;
        let mut sceneries = tomori_render::scenery::Sceneries::empty();
        super::register(&mut sceneries);
        assert!(sceneries.contains("farmyard"));
        assert!(
            sceneries
                .environments()
                .contains(&Environment::new(super::ID).with_version(super::VERSION))
        );
        let declared = sceneries.locomotion();
        assert_eq!(declared.len(), 1);
        assert_eq!(
            declared[0],
            (Environment::new("farmyard"), super::locomotion())
        );
        assert!(super::locomotion().travels());
    }
}
