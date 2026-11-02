# Environments

This describes how a space's environment relates to the space. The first part is a current requirement; the rest is long-term direction, and the initial design must not prevent it.

## Requirement

Each space is bound to exactly one environment. The environment is chosen when the space is created, is part of the space's shared state, and is the same for every participant in it. A participant does not switch a space's environment on their own device; the per-device environment choice, such as squeezing the grip to switch between Sky and Minimal, does not apply inside a space. The server offers the environments a space can use, as it offers backends and directories; today that is only Sky (`crates/environment-sky`). A client that doesn't know a space's environment shows the Minimal grid and says which environment it lacks, rather than substituting another environment silently.

Arrival, outside any space, uses the Minimal grid as its waiting area (see `arrival.md`).

Long-term direction, not yet requirements:

- An environment is much more than scenery. It can be dynamic and data-backed, with its own interaction, such as Fantail Observe's spatial view of a running system's topology and telemetry, a separate project. Environments are expected to be built on the server-owned scene graph and modules in `scene-modules.md`.
- The space's agent is aware of its environment: the information and data it presents, and what each participant is pointing at or has selected in it. Pointing and selection identify concrete resources, with their identity and context such as the time window, not only positions or pixels.
- A participant can refer to what they are pointing at in speech, such as "Tomori, can you investigate why this server is overloaded?". The agent receives the identified resource with the request, and uses the tools available in that space, such as its MCP servers, to investigate and answer.

## Rationale

The user intends to build specific environments for different kinds of work, so a space represents much more than a cosmetic place: it pulls in the information and interaction that work needs. Tying the environment to the space keeps every participant looking at the same thing, which shared pointing and the agent's awareness of the environment depend on. Stated by the user, speaking from the headset, in November 2026, with Fantail Observe as the example to connect eventually.

## Scope

The space's shared state and creation request in `crates/protocol` and the server, the headset client's environment selection, and, long term, the scene module host and how participant pointing reaches the agent.

## Validation

- Create a space and confirm that its state names its environment, that a second participant sees the same environment, and that squeezing the grip inside the space doesn't change it.
- Confirm that the creation choices offer only the environments the server lists.
- Join a space whose environment the client doesn't know, for example with a test server that names an unknown one, and confirm that the client shows the grid and names the missing environment.

Open questions, not yet decided:

- Whether a space's environment can be changed after it is created, and by whom.
- Whether an environment is a scene module, a set of modules, or something that hosts them.
- How participant pointing and selection are delivered to the agent, and how that relaxes `scene-modules.md`'s current separation of the scene graph from agent interactions.
- How an environment's data sources, such as a telemetry service, relate to the tools available to the space's agent.
