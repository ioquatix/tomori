# Spaces

## Requirement

A space is bound to exactly one agent session. Every participant in a space interacts with that same session; a space never fans one participant's input out to several sessions, and a participant never addresses a session outside the space they have joined.

The server hosts multiple spaces concurrently. Clients can list, create, join, leave and move between spaces. A client is a participant in at most one space at a time; moving to another space leaves the previous one.

Shared space state is held by the server and is the same for every participant: the conversation, the input queue, pending approvals and questions (including partially answered forms), delegated workers, presentation cards and worker visibility. Per-participant state is limited to presence, local presentation choices such as mute or paging, and the participant's own in-progress recording.

When participants act concurrently:

- The first decision on a pending approval or question resolves it. Later decisions on the same request are rejected without effect, and every participant observes the resolution.
- Finalized utterances enter a per-space queue in finalization order. Each entry records its speaker and state (queued, sending, delivered). Every participant can see the queue, and any participant can cancel an entry until it is sent to the agent.
- Messages delivered to the agent identify their speaker. Attribution is a per-space setting that defaults to on.

## Rationale

Several people should be able to share one conversational workspace with one coordinating agent, and move between separate workspaces. Agreed in the October 2026 design discussion for replacing the Node bridge.

## Scope

The server's space registry, the client protocol for joining and leaving spaces, the input queue, approval and question handling, and per-space persistence under `.space/`.

## Validation

- Inspect the space model: a space has exactly one session binding, and per-space stores are keyed by space.
- Test two participants in one space: both observe the same conversation, queue and pending requests; concurrent decisions on one approval result in exactly one decision reaching the backend.
- Test queue cancellation: a queued entry can be cancelled by another participant before sending, and cannot be cancelled after delivery.
- Test attribution on and off for delivered messages.
- Test moving a client between two spaces: it receives the new space's state and no further events from the old one.
