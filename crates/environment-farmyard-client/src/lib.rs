// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Tomori's farmyard, its client half: the scenery Tomori's clients draw, an evening sky behind the yard and the coop,
//! built in code and drawn with `tomori-assets`, with the chickens; the client side that shows the chickens from what
//! the farmyard's server side shares; and the teleport profile, which a client registers with [`register`]. What its
//! server and clients share is `tomori-environment-farmyard`. See `design/farmyard.md`.
pub mod client;
pub mod scenery;

pub use tomori_environment_farmyard::{ID, LABEL, LICENSE, VERSION, chicken, coop, layout};

/// The heading the chicken's licence notice, [`LICENSE`], is shown under with a build's other bundled assets'.
pub const LICENSE_TITLE: &str = "The farmyard's chicken, by Hinoki Maple Williams (CC BY 4.0)";

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
/// with its coop built in code, its teleport profile, its client side, which shows the chickens, and the licence of
/// the chicken it bundles. Tomori's own app and spectator do so; a build of another project's can too.
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
    sceneries.set_client_side(ID, |space| {
        Box::new(client::FarmyardClient::new(
            space,
            chicken::Chicken::shared(),
            std::time::Instant::now(),
        ))
    });
    sceneries.add_license(LICENSE_TITLE, LICENSE);
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_farmyard_registers_its_scenery_client_side_and_licence_at_its_identity_and_lets_participants_teleport()
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
        // Its client side, made for a space, shows nothing until the server side shares the flock.
        let client_sides = sceneries.client_sides();
        assert_eq!(client_sides[0].0.id(), super::ID);
        let mut client = (client_sides[0].1)("space-1");
        let output = client.update(tomori_scene::client_side::Input::new(
            Vec::new(),
            std::time::Instant::now(),
        ));
        assert!(output.drawn.is_none());
        // It carries the chicken's credit and licence, for the build to show with its own.
        assert_eq!(
            sceneries.licenses(),
            [(super::LICENSE_TITLE, super::LICENSE)]
        );
        assert!(super::LICENSE.contains("Hinoki Maple Williams"));
        assert!(super::LICENSE.contains("https://creativecommons.org/licenses/by/4.0/"));
    }
}
