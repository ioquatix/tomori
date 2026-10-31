# Server

This describes the target design for the Rust server that replaces `bridge/`. The current Node bridge and headset client differ where noted in `readme.md`; changes to them are not reviewed against this document until they move to the new server.

## Requirement

The PC server is a Rust program built on `base-http-server`. It replaces the Node bridge and owns spaces, agent backends, speech, live audio routing and the spectator feed.

Each client keeps one long-lived, bidirectional streaming connection to the server. Control messages, events, microphone audio, speech audio and presence are framed over that connection; the server does not require per-action request routes for interactive traffic. Audio frames are self-contained (source, sequence and timing) so that their transport can change without affecting the layers above. Both ends detect a silent peer as described in `connection-liveness.md`.

Initially, audio travels over the same HTTP streaming connection. Later, audio moves to UDP datagrams so that a lost packet is dropped instead of stalling the audio behind it. Over UDP, audio is Opus-encoded with in-band forward error correction, so isolated losses are recovered without a round trip. Transcribed audio must still meet `speech.md`: the server requests any `agent` frame that error correction could not recover, and fails the utterance visibly if the frame does not arrive in time. Live playback never waits for a retransmission.

Whose view the spectator feed shows when a space has several participants is not yet decided.

The server advertises itself on the local network with DNS service discovery over multicast DNS (Bonjour), as service type `_tomori._tcp`, with TXT records for the protocol version and a display name. When a client starts, it browses for that service and lists the servers it finds, so a participant can choose one without typing an address; entering an address manually remains possible. Advertising can be turned off.

Wire types are defined once in a shared protocol crate used by both the server and native clients.

No frame exceeds the protocol's frame limit (`MAX_FRAME_BYTES`, 1 MiB), which every receiver enforces. Encoding a larger frame is an error rather than a corrupt or oversized frame, and the server never panics on it.

- The server bounds the strings that clients supply when they arrive: participant identity and display name, space title, directory and session, utterance identifiers, answer text and queued transcripts. It also bounds how many entries wait in a space's queue. A value over its limit is rejected with a short error that names the field but does not repeat the value; the error replies of the server never repeat unbounded client input, and are themselves cut to a fixed length.
- Space snapshots and the lobby listing are kept within a byte budget of a quarter of the frame limit (256 KiB of JSON), because they also carry agent output. A snapshot over budget drops its oldest messages first, then cuts the newest message text with a visible marker, then drops finished queue entries, cuts approval and question text and drops the oldest workers. A listing over budget shortens long directories to their final part, then leaves out closed spaces, then the oldest spaces. What remains is bounded by the input limits and the connection limit.

Clients connect over the local network; a USB connection is not required. Authentication is not an initial requirement. It will be added later, likely as tokens carried in cookies or authorization headers; until then, anyone who can reach the server on the local network can join a space. Whether local network traffic is encrypted is not yet decided. The server exposes no generic filesystem, shell or session-deletion operations and never sends model credentials to clients.

## Rationale

A single duplex connection per participant keeps the server simple and supports real-time audio in both directions. Sharing one protocol crate prevents the client and server definitions from drifting apart. Local network connections let several headsets share a space without USB cables, and discovery lets a headset find the server without typing an address. Deferring authentication keeps the first server simple; because space participants can approve agent tool use, it must be added before use on untrusted networks. Agreed in the October 2026 design discussion.

## Scope

The server crate, the protocol crate, the shared client crate and network configuration.

## Validation

- Inspect dependencies: the server uses `base-http-server`; client and server wire types come from the protocol crate.
- Test a client session over one connection carrying control messages, events and audio concurrently.
- Confirm that a headset can connect over the local network without USB.
- Test that a running server is found by browsing for `_tomori._tcp`, with its protocol version and name, and that it disappears from the list when it stops. On a Quest, confirm that the server list populates on the same Wi-Fi network.
- Over UDP, test that an isolated lost packet is recovered by error correction, that an unrecovered `agent` frame is requested again, and that playback continues without waiting for it.
- Review the protocol for operations outside the listed responsibilities.
- Test that oversized client strings are rejected without being repeated, that a large conversation and a large lobby listing arrive within the snapshot budget, and that encoding an oversized frame fails without producing it.
