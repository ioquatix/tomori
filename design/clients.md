# Clients

This describes the layout of Tomori clients. The headset and desktop application is `crates/client-quest`; it connects to the new server, and no longer uses `bridge/`.

## Requirement

Each kind of hardware or output has its own client implementation in a separate workspace crate named `client-<target>`, such as `client-quest` for Meta Quest headsets and `client-spectator` for the Linux spectator/OBS output. Clients share a platform-neutral core:

- `crates/client`: the server connection, reconnection, outgoing audio, per-source jitter buffers and the playback mixer. It contains no rendering, windowing or device code, and builds for every client target.
- A shared scene model derived from space state and participant presence, also free of rendering code, so that clients with different renderers present the same scene.
- Rendering code shared by clients that use the same engine lives in its own crate, not in one client that others depend on.

The protocol stays device-neutral. A client reports optional capabilities, such as hand tracking or controllers, through its messages; the server never branches on which hardware a participant uses.

The root manifest only defines the workspace and its build profiles; it has no package.

## Rationale

Tomori should support several kinds of hardware and outputs, each with its own platform constraints, without duplicating connection, audio or scene logic, and without one client's platform dependencies leaking into another. Agreed in the October 2026 design discussion.

A visionOS client is a possible later target. Bevy and OpenXR are unavailable there, so it would render natively and use the core through a generated Swift binding. It is not planned, and the core is not required to provide that binding yet.

## Scope

Workspace layout and dependencies of client crates, the client core, the shared scene model, and the protocol's treatment of device differences.

## Validation

- Inspect the workspace: each client is a separate `client-<target>` crate, and the core and scene crates depend on no rendering, windowing or platform audio crate.
- Check that the core builds for every client target, including `aarch64-linux-android`.
- Inspect the server for logic that depends on a client's hardware rather than its reported capabilities.

Whether the spectator client mixes audio itself, instead of the server mixing the spectator feed as `live-audio.md` currently requires, is a pending follow-up.
