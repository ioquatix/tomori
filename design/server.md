# Server

This describes the target design for the Rust server that replaces `bridge/`, and for the headset client, which uses it. The current Node bridge differs; changes to it are not reviewed against this document.

## Requirement

The PC server is a Rust program built on `base-http-server`. It replaces the Node bridge and owns spaces, agent backends, speech, live audio routing and the spectator feed.

Each client keeps one long-lived, bidirectional streaming connection to the server. Control messages, events, microphone audio, speech audio and presence are framed over that connection; the server does not require per-action request routes for interactive traffic. Audio frames are self-contained (source, sequence and timing) so that their transport can change without affecting the layers above. Both ends detect a silent peer as described in `connection-liveness.md`.

Initially, audio travels over the same HTTP streaming connection. Later, audio moves to UDP datagrams so that a lost packet is dropped instead of stalling the audio behind it. Over UDP, audio is Opus-encoded with in-band forward error correction, so isolated losses are recovered without a round trip. Transcribed audio must still meet `speech.md`: the server requests any `agent` frame that error correction could not recover, and fails the utterance visibly if the frame does not arrive in time. Live playback never waits for a retransmission.

Whose view the spectator feed shows when a space has several participants is not yet decided.

The server advertises itself on the local network with DNS service discovery over multicast DNS (Bonjour), as service type `_tomori._tcp`, with TXT records for the protocol version and a display name. When a client starts, it browses for that service and lists the servers it finds, so a participant can choose one without typing an address; entering an address manually remains possible. Advertising can be turned off.

Wire types are defined once in a shared protocol crate used by both the server and native clients. A client reads a space or worker state that it does not know, such as one added by a newer server, as unknown rather than rejecting the message, so adding such a state does not change the protocol version.

Shutting the server down closes every space's session and ends every connection, and completes even while a space is being created: a session that finishes starting after shutdown has begun is closed at once rather than registered.

The server serves a bounded number of connections at once, so a connection that is not doing useful work must not keep its slot. A connection that has not completed a request head within the request timeout of being accepted, or of finishing its previous response, is closed; so is a `/connect` exchange that has not said Hello within the request timeout. The request timeout defaults to 10 seconds and is configurable, as are the keepalive interval and timeout, with `tomori-server` options. Once a participant has said Hello, `connection-liveness.md` applies instead.

No frame exceeds the protocol's frame limit (`MAX_FRAME_BYTES`, 1 MiB), which every receiver enforces. Encoding a larger frame is an error rather than a corrupt or oversized frame, and the server never panics on it.

- The server bounds the strings that clients supply when they arrive: participant identity and display name, space title, directory and session, utterance identifiers, the request, option, question and field identifiers of decisions and answers, answer text and queued transcripts. It also bounds how many entries wait in a space's queue. It keeps at most 32 closed spaces that nobody is in (see `spaces.md`), so closed spaces cannot grow its memory without bound. A value over its limit is rejected with a short error that names the field but does not repeat the value. A display name must not be empty or only whitespace, and must not start with `/`, because it begins each attributed prompt where a leading slash could read as a command to the agent; the error replies of the server never repeat unbounded client input, and are themselves cut to a fixed length.
- Space snapshots and the lobby listing are kept within a byte budget of a quarter of the frame limit (256 KiB of JSON), because they also carry agent output. A snapshot over budget drops its oldest messages first, then cuts the newest message text with a visible marker, then drops finished queue entries, cuts approval and question text, their titles and the space error, and drops the oldest workers. A question field whose key is longer than 1 KiB, such as a long question used as its own key, is named by its position (`field 1`), and answers name it the same way. Approval and question identifiers are not cut, because clients name them in replies. A listing over budget shortens long directories to their final part, then leaves out closed spaces, then the oldest spaces. What remains is bounded by the input limits and the connection limit.

What waits to be sent on one connection is bounded too, so that a slow or careless client cannot grow the server's memory or crowd out agent speech:

- A relayed pose replaces any pose from the same participant that has not been sent yet, and a partial transcript replaces the unsent partial transcript of the same utterance; each keeps the place of the one it replaces. At most a few error replies wait at once, and further errors are dropped until they are sent. A client that keeps making requests without reading the replies until its control queue reaches a fixed limit is disconnected.
- Space snapshots are full states, coalesced so that one waits at a time. Changes caused by agent events, such as a streaming reply, send a snapshot at most every 100 milliseconds per space: the first change is sent at once, and later changes within the interval are sent together at its end, so the latest state always arrives. Each space's deferred snapshot is sent at the end of its own interval, however many other spaces have one pending. Changes made by participants are sent at once.

Clients connect over the local network; a USB connection is not required. Authentication is not an initial requirement. It will be added later, likely as tokens carried in cookies or authorization headers; until then, anyone who can reach the server on the local network can join a space, so sessions run only beneath the operator's workspace roots and only a limited number at once, as described in `workspaces.md`. Whether local network traffic is encrypted is not yet decided. The server exposes no generic filesystem, shell or session-deletion operations and never sends model credentials to clients.

## Rationale

A single duplex connection per participant keeps the server simple and supports real-time audio in both directions. Sharing one protocol crate prevents the client and server definitions from drifting apart. Local network connections let several headsets share a space without USB cables, and discovery lets a headset find the server without typing an address. Deferring authentication keeps the first server simple; because space participants can approve agent tool use, it must be added before use on untrusted networks. Agreed in the October 2026 design discussion.

## Scope

The server crate, the protocol crate, the shared client crate and network configuration.

## Validation

- Inspect dependencies: the server uses `base-http-server`; client and server wire types come from the protocol crate.
- Test a client session over one connection carrying control messages, events and audio concurrently.
- Confirm that a headset can connect over the local network without USB.
- Test that a space or worker state a client does not know is read as unknown.
- Test that a running server is found by browsing for `_tomori._tcp`, with its protocol version and name, and that it disappears from the list when it stops. On a Quest, confirm that the server list populates on the same Wi-Fi network.
- Over UDP, test that an isolated lost packet is recovered by error correction, that an unrecovered `agent` frame is requested again, and that playback continues without waiting for it.
- Review the protocol for operations outside the listed responsibilities.
- Test that shutdown completes when a session finishes starting after shutdown began, and that the session is closed.
- Test with a short request timeout that more silent sockets than the connection limit do not prevent a new `GET /health` from being served, and that an idle keep-alive connection, an incomplete request head and a `/connect` without Hello are closed.
- Test that queued poses and partial transcripts keep only the latest, that error replies and the control queue are bounded, and that a fast streaming reply sends about ten snapshots a second ending with its final text, and that a snapshot deferred in one space is sent when it is due while a later one is pending in another.
- Test that a Hello with an empty, blank or slash-prefixed name is refused, and that the server's timeout options are parsed and validated.
- Test that oversized client strings are rejected without being repeated, that a large conversation, a large lobby listing and an enormous question and error arrive within the snapshot budget and the question can still be answered, and that encoding an oversized frame fails without producing it.
