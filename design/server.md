# Server

## Requirement

The PC server is a Rust program built on `base-http-server`. It replaces the Node bridge and owns spaces, agent backends, speech, live audio routing and the spectator feed.

Each client keeps one long-lived, bidirectional streaming connection to the server. Control messages, events, microphone audio, speech audio and presence are framed over that connection; the server does not require per-action request routes for interactive traffic. Audio frames are self-contained (source, sequence and timing) so that their transport can change without affecting the layers above.

Wire types are defined once in a shared protocol crate used by both the server and native clients.

The server listens on loopback only by default. Any non-loopback listener is explicitly enabled and requires participant pairing. The server exposes no generic filesystem, shell or session-deletion operations and never sends model credentials to clients.

## Rationale

A single duplex connection per participant keeps the server simple and supports real-time audio in both directions. Sharing one protocol crate prevents the client and server definitions from drifting apart. The loopback default preserves the existing security model while allowing multiple clients later. Agreed in the October 2026 design discussion.

## Scope

The server crate, the protocol crate, the shared client crate and network configuration.

## Validation

- Inspect dependencies: the server uses `base-http-server`; client and server wire types come from the protocol crate.
- Test a client session over one connection carrying control messages, events and audio concurrently.
- Confirm the default listener address is loopback and that a non-loopback listener refuses unpaired clients.
- Review the protocol for operations outside the listed responsibilities.
