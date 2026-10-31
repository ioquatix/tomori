# Speech

This describes the target design for the Rust server that replaces `bridge/`, and for the headset client, which uses it. The current Node bridge differs; changes to it are not reviewed against this document. The headset client sends push-to-talk (`agent`) audio and plays other participants and agent speech, but does not offer an open (`live`) microphone yet; that gap is tracked in `readme.md`.

## Requirement

Speech recognition and synthesis run on the PC server.

**Recognition starts as audio arrives.** Audio addressed to the agent is decoded incrementally while the participant is still speaking, with partial transcripts sent back during recording. Stable words are committed as recording proceeds, so finishing an utterance only decodes the uncommitted tail rather than the whole recording. Recognition never silently discards spoken audio; if it cannot produce a transcript, the participant is told and nothing is sent. The server closes an utterance when it reports the failure, even mid-recording, so the client forgets it without cancelling and the failure is not reported twice. An utterance whose agent audio has a gap fails visibly: sequence numbers skipped between two of its `agent` frames. Frames of another gate within an utterance are not recognized, so numbers skipped next to them do not fail it. The client never drops frames of an open utterance, of any gate, and sends them in order with the utterance's start and end, so its first frame is the first one after the start; the client may skip sequence numbers before an utterance, by not sending audio while the gate is off and by dropping congested live audio, so the first frame is not checked against earlier ones. A client that cannot deliver an utterance's audio cancels the utterance and tells the participant. Push-to-talk audio that arrives while no utterance is open is relayed to the other participants but not transcribed; the server counts and logs it.

**Synthesis starts before the reply completes.** The server consumes the agent's streaming text and begins synthesizing as soon as a speakable segment is stable, rather than waiting for the message to complete. Following segments are synthesized while earlier ones play.

**Only speakable text is spoken.** Speakable text is selected from Markdown structure: code blocks, tool output, internal reasoning, raw HTML and Markdown syntax are not spoken, and tables are not read cell by cell. Text whose Markdown structure may still change as more text arrives is held back until it is stable.

**Agent speech is not interrupted by participant input.** Each space has one agent speech stream that every participant receives. Recording, sending or queueing input does not stop, pause or skip it. A participant may mute their own playback locally. A client silences its own playback while its participant records push-to-talk, as `live-audio.md` requires, so that the reply is not transcribed as their input; the stream continues meanwhile, and they hear it again, live, when they stop recording.

Only newly produced assistant text is spoken; existing history is not read aloud when joining a space.

## Rationale

Conversation latency matters most at both ends of a turn: participants should see their words transcribed while speaking, and hear the agent as soon as it has something to say. Participants can queue further input instead of interrupting; see `spaces.md`. Agreed in the October 2026 design discussion.

## Scope

The server's recognition and synthesis pipelines, the speakable-text filter and segmenter, and the speech stream sent to clients and the spectator mixer.

## Validation

- Test that losing a frame within an utterance fails the utterance instead of queueing a cut transcript, that sequence numbers skipped before an utterance, or next to its `live` frames, do not, and that the client queues the `live` frames of an open utterance without dropping them under congestion.
- Test that partial transcripts arrive before an utterance ends, and that the final decode covers only the uncommitted tail.
- Test that the first synthesized audio for a streamed reply is produced before that reply's message completes.
- Test the speakable filter with streamed Markdown containing fences, inline code, tables, links, headings and a line that later becomes a table header or setext heading.
- Test that participant recording and queueing during playback leave the speech stream unchanged.

Latency targets for transcript finalization and time to first audio are not yet agreed.
