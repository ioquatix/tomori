# Live Audio

This describes the target design for the Rust server that replaces `bridge/`. The current Node bridge and headset client differ where noted in `readme.md`; changes to them are not reviewed against this document until they move to the new server.

## Requirement

Participants in a space can talk to each other. Each participant sends one microphone stream, and each audio frame carries a gate:

- `off`: nothing is sent.
- `live`: heard by the other participants and the spectator feed.
- `agent` (push-to-talk): heard by the other participants and the spectator feed, and also transcribed for the agent.

Push-to-talk audio is not a separate upload path. Only `agent` audio is transcribed; open-microphone conversation between participants never reaches the agent.

Each client mixes every incoming participant stream together with the space's agent speech stream into a single output. A participant does not hear their own microphone. A client that shows who is speaking takes it from the audio its mixer actually plays, measured before local volume, so muting agent speech locally silences it without hiding that the agent is speaking. The server mixes the same sources once for the spectator feed.

Playback and mixing are real time: audio that arrives too late to play is dropped from playback rather than delayed or replayed. This does not apply to transcription: every `agent` frame reaches recognition, in order, and a gap in `agent` audio fails the utterance with a visible error, as `speech.md` requires.

The agent speech stream is real time too. Synthesis runs ahead of playback, but the server sends each segment's audio at the rate it plays, only slightly ahead to absorb network jitter, to whoever is in the space at that moment; a participant who joins mid-segment hears it from that point. On each connection, participant audio never displaces agent speech: agent frames are queued separately and dropped only when they could no longer play in time, which receivers see as a gap in the agent stream's sequence numbers.

Echo cancellation happens on the capturing device, so that a participant's playback does not feed back into their microphone stream.

## Rationale

The existing audience microphone and push-to-talk recording are separate paths that only reach OBS and the transcription bridge. A shared workspace needs people to hear each other, including when someone addresses the agent. Agreed in the October 2026 design discussion.

## Scope

Client audio capture and playback, the microphone gate, server routing and the spectator mixer.

## Validation

- Test that `agent` frames are both forwarded and transcribed, `live` frames are only forwarded, and `off` produces no frames.
- Test that a client's output contains other participants and agent speech but not its own microphone, and that muted agent speech plays silently while still reporting a level.
- Test that frames arriving too late for playback are dropped from playback and mixing, while the same `agent` frames still reach recognition.
- Test that a missing `agent` frame fails the utterance visibly instead of producing a partial transcript.
- Test that agent speech is sent at its playback rate rather than as fast as it is synthesized, that a participant joining mid-segment receives the rest of it, and that a backlog of participant audio on a connection does not displace agent frames.
- On a headset, confirm that agent speech and other participants playing through the speakers are not transcribed from the local microphone.

Latency targets for participant-to-participant audio are not yet agreed.
