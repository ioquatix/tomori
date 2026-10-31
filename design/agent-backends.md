# Agent Backends

This describes the target design for the Rust server that replaces `bridge/`. The current Node bridge and headset client differ where noted in `readme.md`; changes to them are not reviewed against this document until they move to the new server.

## Requirement

Each agent integration (currently OpenCode and Claude Code) is a separate workspace crate implementing a backend-neutral interface. Backends report structured facts: messages and their parts, text deltas, tool activity, permission requests, questions with structured fields, delegated workers and busy/idle state. They do not produce headset UI text, prompts or voice-answer parsing; the server derives those from the structured facts.

Session identities are opaque and namespaced by backend. Clients and the wire protocol do not depend on a backend's identifier format.

Backends observe their agents through persistent connections or processes, such as an event-stream subscription or a long-lived child process, not by periodically polling for snapshots. A snapshot operation may exist for resynchronization when a participant joins.

The Claude Code backend drives sessions itself: Tomori starts and owns the `claude` process for a space and communicates over its structured stream interface. Sharing a live session with an interactive terminal is not supported.

Tomori never approves a tool permission automatically and never changes an agent's persistent permissions, model, agent selection or project configuration.

## Rationale

Tomori must support multiple agent systems without coupling the protocol, server or headset to one of them. Streaming text from the backend is required to start speech before a reply completes; see `speech.md`. Agreed in the October 2026 design discussion.

## Scope

The backend interface crate, every backend crate, and the server code that consumes them.

## Validation

- Inspect the dependency graph: the protocol, client and headset crates do not depend on any backend crate, and backends do not depend on the server.
- Inspect backend outputs: no user-facing prompt strings or answer parsing in backend crates.
- Test each backend against a recorded or simulated agent stream, including permissions, questions, workers and a turn that streams text deltas.
- Confirm that no backend issues periodic snapshot requests while a session is attached.
- Confirm that permission decisions only reach a backend in response to an explicit participant action.
