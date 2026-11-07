// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Tomori's farmyard: an evening yard, fenced, with a red chicken coop raised on legs, its ramp and little door, a water
//! trough, a feeder with grain scattered about it, hay bales, lanterns, and pines beyond under a rising moon.
//!
//! It is an environment built as another project would build one, from Tomori's published crates and their extension
//! points, without anything in Tomori's own crates knowing about it. This crate is what its server and its clients
//! share, free of GPU and client code: where everything is in the yard, and its models, built in code as `tomori-assets`
//! models. Its client half, the scenery and teleport profile a client registers, is
//! `tomori-environment-farmyard-client`. See `design/farmyard.md`.
//!
//! ```
//! use tomori_environment_farmyard::{ID, LABEL, VERSION};
//! assert_eq!((ID, VERSION, LABEL), ("farmyard", 1, "Farmyard"));
//! ```
pub mod coop;
pub mod layout;
pub mod props;
mod shapes;

/// The identifier spaces name the farmyard by.
pub const ID: &str = "farmyard";

/// The farmyard's protocol version, which a server offers it at and a client presents it at. Raise it, in this crate
/// alone, when a client of the earlier version could not present the farmyard as the server now offers it.
pub const VERSION: u32 = 1;

/// The label clients show for the farmyard.
pub const LABEL: &str = "Farmyard";

#[cfg(test)]
pub(crate) mod tests {
    /// A binary glTF of one triangle, for a coop that replaces the built one in tests.
    pub fn cube_glb() -> Vec<u8> {
        let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        let mut json = br#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":36}],"bufferViews":[{"buffer":0,"byteLength":36}],"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}],"meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],"nodes":[{"name":"coop","mesh":0}],"scenes":[{"nodes":[0]}]}"#.to_vec();
        json.resize(json.len().next_multiple_of(4), b' ');
        let length = 12 + 8 + json.len() + 8 + positions.len();
        let mut bytes = Vec::new();
        for word in [
            0x4654_6c67u32,
            2,
            length as u32,
            json.len() as u32,
            0x4e4f_534a,
        ] {
            bytes.extend(word.to_le_bytes());
        }
        bytes.extend(json);
        bytes.extend((positions.len() as u32).to_le_bytes());
        bytes.extend(0x004e_4942u32.to_le_bytes());
        bytes.extend(positions);
        bytes
    }
}
