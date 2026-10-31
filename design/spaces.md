# Spaces

This describes the target design for the Rust server that replaces `bridge/`. The current Node bridge and headset client differ where noted in `readme.md`; changes to them are not reviewed against this document until they move to the new server.

## Requirement

A space is bound to exactly one agent session. Every participant in a space interacts with that same session; a space never fans one participant's input out to several sessions, and a participant never addresses a session outside the space they have joined.

The server hosts multiple spaces concurrently. Clients can list, create, join, leave and move between spaces. Creating a space either starts a new agent session or binds an existing one, where the backend supports binding. A client is a participant in at most one space at a time; moving to another space leaves the previous one. Starting a session can take time; once it has started, the creator joins the new space, unless they have joined another space since asking.

A participant who disconnects leaves their space but keeps their identity and audio source, and rejoins that space when they reconnect, unless they left it explicitly. A client may send `leave` straight after Hello to restore a leave made while it was disconnected; this overrides the rejoin, and is harmless when the participant is in no space. The server forgets a participant who has been disconnected for longer than a generous retention, 12 hours by default; after that they reconnect as a new participant in the lobby.

Shared space state is held by the server and is the same for every participant: the conversation, the input queue, pending approvals and questions (including partially answered forms), delegated workers, presentation cards and worker visibility. Per-participant state is limited to presence, local presentation choices such as mute or paging, and the participant's own in-progress recording.

When participants act concurrently:

- The first decision on a pending approval or question resolves it. Later decisions on the same request are rejected without effect, and every participant observes the resolution.
- An agent may have several approvals or questions pending at once, such as for parallel tool calls. Each is kept until it is resolved; none replaces another. They are presented one at a time, oldest first, and a decision applies only to the presented request. If the backend rejects a decision for a reason other than the request being gone, that request is presented again ahead of later ones. While any request is pending or its decision is being delivered, the space is waiting and no queued input is sent to the agent.
- Finalized utterances enter a per-space queue in finalization order. Each entry records its speaker and state (queued, sending, then delivered or failed). Every participant can see the queue, and any participant can cancel an entry until it is sent to the agent. Finalizing the same utterance again, as a client may after reconnecting, queues nothing: the participant receives the final transcript that was queued the first time. A repeat is the same utterance identifier from the same participant; utterances of different participants are never taken for each other, whatever their identifiers contain. The server recognizes a repeat among a space's 32 most recently queued utterances, more than the queue shows.
- When a space's agent session ends, the space stays listed with its state, marked closed (activity `closed`, with the reason in its error) in both its state and the lobby listing. Entries still queued fail visibly, pending approvals and questions are withdrawn, and new input is refused with a transcript failure instead of being queued. The server keeps at most the 32 most recent closed spaces that nobody is in, and forgets older ones, so that sessions ending repeatedly cannot grow its memory without bound; a closed space someone is still in is kept until they leave.
- Messages delivered to the agent identify their speaker. Attribution is a per-space setting that defaults to on. With attribution on, the delivered text is the speaker's display name, a colon and a space, then the transcript (`Ada: run the tests`); with it off, the transcript is delivered unchanged. The queue shows the transcript without the prefix.

## Rationale

Several people should be able to share one conversational workspace with one coordinating agent, and move between separate workspaces. Agreed in the October 2026 design discussion for replacing the Node bridge.

## Scope

The server's space registry, the client protocol for joining and leaving spaces, the input queue, and approval and question handling. How spaces persist across server restarts is not yet decided.

## Validation

- Inspect the space model: a space has exactly one session binding, and per-space stores are keyed by space.
- Test two participants in one space: both observe the same conversation, queue and pending requests; concurrent decisions on one approval result in exactly one decision reaching the backend. Test two parallel approvals: both are presented in turn and both decisions reach the backend in order.
- Test that finalizing the same utterance twice queues it once and acknowledges the repeat with the text queued first, and that utterances of participants `a` and `a/b` whose identifiers would join to the same text are both queued.
- Test queue cancellation: a queued entry can be cancelled by another participant before sending, and cannot be cancelled after delivery.
- Test attribution on and off for delivered messages.
- Test that reconnecting rejoins the space, that `leave` straight after Hello overrides the rejoin and is harmless in the lobby, and that a participant gone longer than the retention reconnects as a new participant.
- Test that closing a session fails its queued input, marks the space closed in its state and the listing, and refuses new input, and that beyond 32 closed spaces nobody is in, the oldest are forgotten while an occupied closed space is kept.
- Test moving a client between two spaces: it receives the new space's state and no further events from the old one.
