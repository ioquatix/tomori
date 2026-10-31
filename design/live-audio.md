# Live Audio

This describes the target design for the Rust server that replaces `bridge/`, and for the headset client, which uses it. The current Node bridge differs; changes to it are not reviewed against this document. The headset client sends push-to-talk (`agent`) audio and plays other participants and agent speech, but does not offer an open (`live`) microphone yet; that gap is tracked in `readme.md`.

## Requirement

Participants in a space can talk to each other. Each participant sends one microphone stream, and each audio frame carries a gate:

- `off`: nothing is sent.
- `live`: heard by the other participants and the spectator feed.
- `agent` (push-to-talk): heard by the other participants and the spectator feed, and also transcribed for the agent.

Push-to-talk audio is not a separate upload path. Only `agent` audio is transcribed; open-microphone conversation between participants never reaches the agent.

Each client mixes every incoming participant stream together with the space's agent speech stream into a single output.

What a participant records never plays back to them, through any gate. The server never relays a participant's audio to that participant, and a client also discards any audio attributed to its own participant number, in case a server does. The agent's speech is not the participant's own audio: its reply to a question they asked plays to them like everyone else.

A client that shows who is speaking takes it from the audio its mixer actually plays, measured before local volume, so muting agent speech locally silences it without hiding that the agent is speaking. The server mixes the same sources once for the spectator feed.

Playback and mixing are real time: audio that arrives too late to play is dropped from playback rather than delayed or replayed. This does not apply to transcription: every `agent` frame reaches recognition, in order, and a gap in `agent` audio fails the utterance with a visible error, as `speech.md` requires.

The agent speech stream is real time too. Synthesis runs ahead of playback, but the server sends each segment's audio at the rate it plays, only slightly ahead to absorb network jitter, to whoever is in the space at that moment; a participant who joins mid-segment hears it from that point. On each connection, participant audio never displaces agent speech: agent frames are queued separately and dropped only when they could no longer play in time, which receivers see as a gap in the agent stream's sequence numbers.

A participant's playback must not feed back into their microphone stream:

- While a client records push-to-talk, it silences its own playback, fading out over a few milliseconds, so that agent speech and other participants playing through its speakers are not transcribed. Silenced audio is still consumed in real time, so when recording ends playback resumes live rather than replaying what was missed. Silencing is local: the agent speech stream and what other participants hear are unchanged. The client silences playback before it opens the microphone, but audio the output device has already buffered, typically a few tens of milliseconds, can still sound as recording starts; that is accepted for push-to-talk, and an open microphone relies on echo cancellation instead.
- An open (`live`) microphone needs echo cancellation on the capturing device, because silencing playback whenever it is open would stop participants hearing each other. It is required before the client offers an open microphone; no client does yet, and which input preset or canceller provides it on each platform is not yet decided.

## Rationale

The existing audience microphone and push-to-talk recording are separate paths that only reach OBS and the transcription bridge. A shared workspace needs people to hear each other, including when someone addresses the agent. Agreed in the October 2026 design discussion.

A participant hearing their own recording back is disorienting and, through their microphone, feeds back; the user required in November 2026 that what a user records must not play back to that user. Silencing playback during push-to-talk was chosen because the Quest's `VoiceRecognition` capture preset and desktop capture have no echo cancellation, and without it a spoken reply playing while the participant records is transcribed and queued as their input, looping the agent's words back to it.

## Scope

Client audio capture and playback, the microphone gate, server routing and the spectator mixer.

## Validation

- Test that `agent` frames are both forwarded and transcribed, `live` frames are only forwarded, and `off` produces no frames.
- Test that a client's output contains other participants and agent speech but not its own microphone, and that muted agent speech plays silently while still reporting a level.
- Test end to end that a participant who records, through every gate, while the agent answers them receives the agent's reply and no frame of their own; and that a client given audio attributed to its own participant number does not deliver it.
- Test that silenced playback fades out, consumes audio in real time while silenced and resumes live, and inspect the client to confirm that it silences playback before it opens the microphone and until the microphone has closed.
- Test that frames arriving too late for playback are dropped from playback and mixing, while the same `agent` frames still reach recognition.
- Test that a missing `agent` frame fails the utterance visibly instead of producing a partial transcript.
- Test that agent speech is sent at its playback rate rather than as fast as it is synthesized, that a participant joining mid-segment receives the rest of it, and that a backlog of participant audio on a connection does not displace agent frames.
- On a headset and a desktop, confirm that agent speech and other participants playing through the speakers are not transcribed from the local microphone while push-to-talk is held.

Latency targets for participant-to-participant audio are not yet agreed.
