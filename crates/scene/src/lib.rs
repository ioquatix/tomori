// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The platform-neutral Tomori client: the frame step that the headset and the desktop window share, with each platform's own logic, the conversation with a server, the panels' layout, and audio and haptic logic behind traits that each client's devices implement.
//!
//! The conversation is the [`tomori_client::connection::Session`] that a [`tomori_client::connection::Connection`] carries out, so the connection, the device identity, the microphone and speaker traits and the feedback model are `tomori-client`'s; see `design/clients.md`.
//!
//! Nothing here depends on a rendering, windowing or platform audio crate, as `design/clients.md` requires, so it builds for every client target. A client turns its devices' input into [`scene::Buttons`] and [`scene::Senses`], runs the frame step, and draws the [`scene::Frame`] it returns.
//!
//! Its interfaces change with the environment framework ([#75](https://github.com/ioquatix/tomori/issues/75), [#77](https://github.com/ioquatix/tomori/issues/77)).
pub mod arrival;
pub mod arrival_motion;
pub mod board;
pub mod canvas;
pub mod caption;
pub mod capture;
pub mod cards;
pub mod channel;
pub mod client_side;
pub mod controller;
pub mod controller_ui;
pub mod conversation;
pub mod delegation;
pub mod environment;
pub mod hand_menu;
pub mod hands;
pub mod haptics;
pub mod interaction;
pub mod locomotion;
pub mod log_cadence;
mod markdown_lite;
pub mod microphone_request;
pub mod panel;
mod panel_anchor;
pub mod pill;
pub mod playback;
pub mod pointing;
pub mod recording;
pub mod scene;
pub mod spatial_audio;
mod speakers;
pub mod swarm;
/// Devices and connections for tests, shared with the other client crates' tests through the `testing` feature.
#[cfg(all(any(test, feature = "testing"), not(target_os = "android")))]
pub mod test_support;
pub mod transcript;
pub mod ui_text;
