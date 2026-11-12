// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Assets for Tomori's environments, loaded at runtime: content-addressed references to them, the sources and library
//! that load them, a reader of binary glTF that treats every file as untrusted, the models it reads, instances of them
//! with poses, a small deterministic animator, and, with the `gpu` feature, drawing them skinned with wgpu.
//!
//! The layers, each usable without the ones after it:
//!
//! - **Assets:** an [`AssetRef`] names an asset by its environment, version, name and SHA-256 digest; a [`Library`]
//!   reads it from its [`Source`]s, refusing bytes whose digest differs, and reads it, within [`Limits`], into a
//!   [`Model`], which it keeps by digest and shares.
//! - **Models and instances:** a [`Model`] is validated data: nodes, meshes, materials, textures, skins and
//!   animations; geometry built in code is checked the same way with [`Model::built`], and a large one split into
//!   chunks across the ground with [`Model::chunked`]. An [`Instance`] places a model in a scene, with a [`Pose`] and
//!   nodes it shows or hides.
//! - **Animation:** an [`animation::Animator`] moves through an [`animation::Graph`] of states, each a clip of an
//!   animation, on its parameters, cross-fading, firing events on clips' timelines and measuring root motion, the same
//!   way however its time is divided, so that a server and its clients compute the same from the same inputs.
//! - **Drawing** (`gpu`): `gpu::Renderer` uploads models once and draws their instances, skinned on the GPU from each
//!   instance's joint matrices, with one draw for every instance of a primitive.
//!
//! See `design/assets.md`, `design/animation.md` and `context/usage.md`.
pub mod animation;
pub mod bounds;
mod chunks;
mod gltf;
pub mod haze;
mod instance;
mod library;
mod model;
mod pose;
mod reference;
pub mod views;

#[cfg(feature = "gpu")]
pub mod gpu;

use std::fmt;

pub use gltf::read;
pub use instance::{Draw, Instance};
pub use library::{Directory, Embedded, Library, Source};
pub use model::{
    Animation, Channel, INFLUENCES, Interpolation, Limits, Material, Mesh, Model, Node, Primitive,
    Property, Skin, Texture, Transform,
};
pub use pose::{Pose, deformed, joint_matrices};
pub use reference::{AssetRef, Digest};
pub use views::Views;

/// What kind of failure an [`Error`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// No source has the asset.
    Missing,
    /// A source has bytes for the asset that are not what its digest names.
    Mismatch,
    /// The asset is broken: something in it is out of range, or does not agree with the rest.
    Invalid,
    /// The asset uses something this crate does not read.
    Unsupported,
    /// The asset is larger than the limits allow.
    TooLarge,
}

/// Why an asset could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn missing(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Missing, message)
    }

    pub(crate) fn mismatch(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Mismatch, message)
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Invalid, message)
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, message)
    }

    pub(crate) fn too_large(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::TooLarge, message)
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
