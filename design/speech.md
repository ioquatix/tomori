# Speech

This describes the target design for the Rust server that replaces `bridge/`. The current Node bridge and headset client differ where noted in `readme.md`; changes to them are not reviewed against this document until they move to the new server.

## Requirement

Speech recognition and synthesis run on the PC server.

**Recognition starts as audio arrives.** Audio addressed to the agent is decoded incrementally while the participant is still speaking, with partial transcripts sent back during recording. Stable words are committed as recording proceeds, so finishing an utterance only decodes the uncommitted tail rather than the whole recording. Recognition never silently discards spoken audio; if it cannot produce a transcript, the participant is told and nothing is sent. An utterance whose audio has a gap fails visibly, including a gap before its first frame: audio frames and the utterance's start share one ordered stream and frame sequence numbers increase by one per frame sent, so the first frame must directly follow the last frame sent before the utterance began. Push-to-talk audio that arrives while no utterance is open is relayed to the other participants but not transcribed; the server counts and logs it.

**Synthesis starts before the reply completes.** The server consumes the agent's streaming text and begins synthesizing as soon as a speakable segment is stable, rather than waiting for the message to complete. Following segments are synthesized while earlier ones play.

**Only speakable text is spoken.** Speakable text is selected from Markdown structure: code blocks, tool output, internal reasoning, raw HTML and Markdown syntax are not spoken, and tables are not read cell by cell. Text whose Markdown structure may still change as more text arrives is held back until it is stable.

**Agent speech is not interrupted by participant input.** Each space has one agent speech stream that every participant receives. Recording, sending or queueing input does not stop, pause or skip it. A participant may mute their own playback locally.

Only newly produced assistant text is spoken; existing history is not read aloud when joining a space.

## Rationale

Conversation latency matters most at both ends of a turn: participants should see their words transcribed while speaking, and hear the agent as soon as it has something to say. Participants can queue further input instead of interrupting; see `spaces.md`. Agreed in the October 2026 design discussion.

## Scope

The server's recognition and synthesis pipelines, the speakable-text filter and segmenter, and the speech stream sent to clients and the spectator mixer.

## Validation

- Test that losing an utterance's first frame, as well as a later one, fails the utterance instead of queueing a cut transcript.
- Test that partial transcripts arrive before an utterance ends, and that the final decode covers only the uncommitted tail.
- Test that the first synthesized audio for a streamed reply is produced before that reply's message completes.
- Test the speakable filter with streamed Markdown containing fences, inline code, tables, links, headings and a line that later becomes a table header or setext heading.
- Test that participant recording and queueing during playback leave the speech stream unchanged.

Latency targets for transcript finalization and time to first audio are not yet agreed.
