# Connection Liveness

## Requirement

Both ends of a client connection (`server.md`) detect a peer that has gone silent, without waiting for the transport to report an error.

- Any data received on the connection counts as liveness: a control message, an audio frame or part of either. Keepalive traffic is needed only while the connection is otherwise quiet, so a steady audio stream needs none.
- A side that has received nothing for the keepalive interval sends `ping` with a nonce, and repeats it each interval while the quiet continues. The peer answers every `ping` with a `pong` carrying the same nonce as soon as it is received, including before Hello. Both message types exist in both directions of the protocol.
- Keepalive messages are sent ahead of all queued control messages and audio, so congestion from queued audio cannot delay them.
- A side that has received nothing for the keepalive timeout ends the connection. Ending it must not depend on the peer or on a pending write completing: a write blocked on a peer that is gone is abandoned.
- The server also ends a connection whose outgoing data has not been accepted for the keepalive timeout, that is, a write that has made no progress for that long, even if the peer is still sending. A peer that does not read cannot receive audio or space state, so it is not a working participant. The server does not read a connection while one of its writes is blocked, so data the peer sends meanwhile is only seen once the write completes.
- The defaults are a 5 second interval and a 15 second timeout. With these defaults, each side ends a silent connection within 20 seconds of the last data it received. Both values are configurable on the client and on the server.
- When the server ends a silent connection, it releases the participant exactly as for any other disconnection: the participant leaves its space but keeps its identity and audio source, and rejoins that space on reconnection, within the participant retention (`spaces.md`).
- When the client ends a silent connection, it reports the disconnection as a timeout and reconnects with its usual backoff.

## Rationale

A headset that walks out of Wi-Fi range or goes to sleep leaves a half-open TCP connection. Without application-level liveness, the server keeps a ghost participant in the space and the client believes it is connected, until a write fails. A write to a peer that is gone may not fail for many minutes, and when the send buffer is full it blocks rather than fails. Counting all received data as liveness keeps keepalive traffic off connections that are already busy. Requested in the October 2026 keepalive work.

## Scope

The protocol crate's `Ping`, `Pong` and `Keepalive`, the client connection loop and its options, and the server's connection handling and keepalive sweep.

## Validation

- Inspect the protocol for `ping` and `pong` in both `ClientMessage` and `ServerMessage`, and test their serialization.
- Inspect both outboxes: keepalive messages are taken before control messages and audio.
- Test with short intervals that the server releases a raw client which says Hello and then sends nothing, that the released participant reconnects with the same identity and space, and that a peer which only answers pings stays connected.
- Test with short intervals that the client reports a timeout and reconnects when the server welcomes it and then sends nothing, and that a quiet connection to a server that answers pings stays open for several timeouts.
- Inspect how the server ends a silent connection: it must cancel that connection's exchange rather than wait for a write or for the peer.
- Test with short intervals that the server releases a raw client which keeps sending but never reads while the server has audio to send it, and test the sweep directly: a connection whose write never completes is released after the timeout although data keeps arriving, while one whose writes complete is not.
- Review the defaults and the server's sweep period against the 20 second bound.
