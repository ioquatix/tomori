# Agent Backends

This describes the target design for the Rust server that replaces `bridge/`. The current Node bridge and headset client differ where noted in `readme.md`; changes to them are not reviewed against this document until they move to the new server.

## Requirement

Each agent integration (currently OpenCode and Claude Code) is a separate workspace crate implementing a backend-neutral interface. Backends report structured facts: messages and their parts, text deltas, tool activity, permission requests, questions with structured fields, delegated workers and busy/idle state. A delegated worker names the worker that started it as its parent, or no parent when the session itself started it, so that a change of the agent's own session identifier cannot leave workers pointing at it. They do not produce headset UI text, prompts or voice-answer parsing; the server derives those from the structured facts.

Session identities are opaque and namespaced by backend. Clients and the wire protocol do not depend on a backend's identifier format.

Backends observe their agents through persistent connections or processes, such as an event-stream subscription or a long-lived child process, not by periodically polling for snapshots. A snapshot operation may exist for resynchronization when a participant joins.

The Claude Code backend drives sessions itself: Tomori starts and owns the `claude` process for a space and communicates over its structured stream interface. Sharing a live session with an interactive terminal is not supported.

Tomori never approves a tool permission automatically and never changes an agent's model, agent selection or project configuration.

Sessions run under the user's own agent configuration. Tomori does not override permission modes or rules, so a tool the user's configuration already allows runs without a Tomori approval. Every permission request that does reach Tomori is presented to the space and resolved only by an explicit participant decision.

Tomori changes an agent's permissions only when a participant chooses a standing option, such as "don't ask again", that the agent's own interface offers for the same request. Tomori offers exactly the standing options that interface would offer, labelled with everything each grants and where it is kept, and nothing else. Choosing one has the same effect as choosing it in that interface: the same rules, directories and mode changes, written to the same destination (the session, the project's local or shared settings, or the user's settings) by the agent itself. Tomori never writes agent settings files, and never invents, widens, combines or edits a grant. An option whose grants cannot all be described in its label is not offered. For Claude Code, the standing option is the one standing row of the terminal permission dialog for that tool, such as "Yes, and don't ask again for …" or "Yes, and switch to accept edits for this session". It is built from the request's `permission_suggestions` as that dialog builds it, filtering them by type, destination, tool and size, and answered with exactly the updates the row displays as `updatedPermissions`. It is not offered when the terminal would show no such row, such as when the request sets `suppress_always_allow_rule`, or when the row is one the terminal computes or lets the user edit in ways Tomori cannot reproduce, except that a Bash command prefix row is offered with the single prefix Claude Code suggests.

## Rationale

Tomori must support multiple agent systems without coupling the protocol, server or headset to one of them. Streaming text from the backend is required to start speech before a reply completes; see `speech.md`. Agreed in the October 2026 design discussion.

Answering the same request repeatedly from a headset is tedious, so participants may grant standing permissions, but only in the way the agent's own interface does: a user who knows what "don't ask again" means in the terminal gets the same result in Tomori, and Tomori adds no permission path of its own to review. Agreed with the user in October 2026, on the condition that it works the same way as the terminal interface.

## Scope

The backend interface crate, every backend crate, and the server code that consumes them. The standing option requirement also covers the protocol and clients that present approval options and send decisions.

## Validation

- Inspect the dependency graph: the protocol, client and headset crates do not depend on any backend crate, and backends do not depend on the server.
- Inspect backend outputs: no user-facing prompt strings or answer parsing in backend crates.
- Test each backend against a recorded or simulated agent stream, including permissions, questions, workers and a turn that streams text deltas.
- Confirm that no backend issues periodic snapshot requests while a session is attached.
- Confirm that permission decisions only reach a backend in response to an explicit participant action.
- Inspect the server and backends: no code writes agent settings files, and a standing grant reaches the agent only as the agent's own answer to the request, such as Claude Code's `updatedPermissions`.
- Test each backend's standing options against recorded requests: the options offered and the grants they describe match what the agent's interface offers for that request, and choosing one sends exactly the corresponding answer. For Claude Code, compare with the installed CLI's permission dialog (for example by searching its bundled source for the dialog's rows) and probe in a scratch directory that the answer is applied and saved to the suggested destination. Re-check when the supported Claude Code version changes. Last compared with Claude Code 2.1.311 in October 2026. The comparison covered the Bash, edit, `Read`, `WebFetch` and generic dialogs in its bundled source. A scratch probe recorded the suggestions for Bash (single prefix, compound command and outside path), `Write` inside and outside the project, `Edit`, `Read` outside the project, `WebFetch` and `WebSearch`. It confirmed that granting a Bash prefix saved `Bash(npm run *)` to the scratch project's `.claude/settings.local.json` and stopped a later prompt, and that granting `Read` session rules and the `Write` mode and directories stopped later prompts.
- Test that the server refuses a grant naming an option the pending request does not offer, and that every offered option's label names each grant and its scope.
