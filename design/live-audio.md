# Live Audio

## Requirement

Participants in a space can talk to each other. Each participant sends one microphone stream, and each audio frame carries a gate:

- `off`: nothing is sent.
- `live`: heard by the other participants and the spectator feed.
- `agent` (push-to-talk): heard by the other participants and the spectator feed, and also transcribed for the agent.

Push-to-talk audio is not a separate upload path. Only `agent` audio is transcribed; open-microphone conversation between participants never reaches the agent.

Each client mixes every incoming participant stream together with the space's agent speech stream into a single output. A participant does not hear their own microphone. The server mixes the same sources once for the spectator feed.

Live audio is real time: late audio is dropped rather than delayed or replayed.

Echo cancellation happens on the capturing device, so that a participant's playback does not feed back into their microphone stream.

## Rationale

The existing audience microphone and push-to-talk recording are separate paths that only reach OBS and the transcription bridge. A shared workspace needs people to hear each other, including when someone addresses the agent. Agreed in the October 2026 design discussion.

## Scope

Client audio capture and playback, the microphone gate, server routing and the spectator mixer.

## Validation

- Test that `agent` frames are both forwarded and transcribed, `live` frames are only forwarded, and `off` produces no frames.
- Test that a client's output contains other participants and agent speech but not its own microphone.
- Test that delayed frames are dropped rather than queued indefinitely.
- On a headset, confirm that agent speech and other participants playing through the speakers are not transcribed from the local microphone.

Latency targets for participant-to-participant audio are not yet agreed.
