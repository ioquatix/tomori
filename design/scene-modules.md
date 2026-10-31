# Scene Modules

This is a long-term requirement. It is not part of the initial server, but the initial design must not prevent it.

## Requirement

Each space has a scene graph owned by the server. The scene graph belongs to the space, not to its agent session. The server replicates scene graph nodes and generic state to every client in the space, and clients render them. Clients do not need feature-specific code for each new kind of visualization.

Scene content can be produced by modules running on the server. An agent can write a module in Rust while the server is running, and the server can load, replace and unload it without restarting and without disturbing other spaces. Modules visualize information, states and running systems. Examples:

- Prometheus metrics shown as graphs.
- Interactive 3D system diagrams.
- Web pages or HTML shown on 2D panels.
- Source code shown as interactive, hierarchical 3D objects that show interactions and boundaries.

Scene content can be interactive. Participant input on a node is delivered to the module that owns it.

At least initially, the scene graph and modules are independent of agent interactions. They do not depend on agent backends, sessions or the conversation, and agent features do not depend on them. Agents author modules through ordinary tools; the scene graph does not need to know which agent wrote a module.

The scene model is meant to grow better lighting and physics over time.

Open questions, not yet decided:

- How modules are isolated and linked: WebAssembly components or native libraries.
- Which capabilities modules get, such as network access to a metrics endpoint, and who approves them.
- Whether physics runs on the server, on clients, or both.
- How HTML panels are rendered.
- Resource limits per module.

## Rationale

Agents should be able to build new spatial visualizations during a conversation instead of each visualization being hand-written into the headset. Running modules on the server keeps every participant's view consistent, and the headset cannot load native code at runtime. Stated by the user in the October 2026 design discussion.

## Scope

The space state model, the client protocol, the client renderer and a future module host in the server.

## Validation

Until modules are implemented: review protocol and renderer changes to confirm that shared visual state comes from the server and that new non-agent visualizations do not require hard-coding a new message type for each feature. Agent-specific state such as workers and presentation cards may keep its own messages.

Once implemented:

- Load, replace and unload a module in a running server, and confirm that other spaces are unaffected.
- Confirm that clients render a new module's output without a client update.
- Confirm that input on a node reaches only the module that owns it.
