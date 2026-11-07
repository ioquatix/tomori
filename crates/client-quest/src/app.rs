// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Tomori's own headset and desktop app, which the `tomori` executable and APK run: [`crate::Builder`] with Tomori's own environments registered on it, through the same calls another project's build makes, from each environment's own crate. The library registers none itself, so that it depends on no environment's crate; this module is the default `app` feature, which a build of another project's turns off. See `design/builders.md`.
use crate::Builder;

/// Tomori's own build: the grid, Sky from `tomori-environment-sky-client`, at Sky's version, the farmyard from `tomori-environment-farmyard-client`, with its teleport profile, and the GitHub prototype from `tomori-environment-github-client`, presented over the grid, where the shared scene draws its cards. Only the farmyard declares locomotion.
pub fn builder() -> Builder {
    Builder::default()
        .register(tomori_environment_sky_client::register)
        .register(tomori_environment_farmyard_client::register)
        .register(tomori_environment_github_client::register)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tomori_client::connection::Session;
    use tomori_protocol::EnvironmentVersion;

    #[test]
    fn tomoris_own_build_presents_the_grid_sky_the_farmyard_and_the_github_prototype() {
        let (tomori, _connection) = builder().participant(crate::Overrides::default());
        assert_eq!(
            tomori.environments(),
            Some(vec![
                EnvironmentVersion::new(tomori_protocol::GRID, tomori_protocol::GRID_VERSION),
                EnvironmentVersion::new(
                    tomori_environment_sky_client::ID,
                    tomori_environment_sky_client::VERSION
                ),
                EnvironmentVersion::new(
                    tomori_environment_farmyard_client::ID,
                    tomori_environment_farmyard_client::VERSION
                ),
                EnvironmentVersion::new(
                    tomori_environment_github_client::ID,
                    tomori_environment_github_client::VERSION
                ),
            ])
        );
        // Of Tomori's own, the farmyard alone lets participants travel, about its yard.
        let locomotion = builder().sceneries().locomotion();
        assert_eq!(locomotion.len(), 1);
        assert_eq!(locomotion[0].0.id(), tomori_environment_farmyard_client::ID);
        assert_eq!(
            locomotion[0].1,
            tomori_environment_farmyard_client::locomotion()
        );
    }
}
