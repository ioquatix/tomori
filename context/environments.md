# Environments

How Tomori environments are built: a proposed framework, with a computer environment first. This is a design proposal for discussion, from November 2026. Nothing here is a requirement until it is agreed. Once agreed, its parts become binding `design/*.md` changes, listed under [Changes to existing documents](#changes-to-existing-documents). Each part is tracked by an issue.

## Summary

- An **environment** is a crate compiled into both the server and the clients, with a protocol version both sides agree on (#75).
  - Its **server half** is a full program in the server. It can query anything the server can reach, owns the environment's authoritative state, and handles participants' intents.
  - Tomori **synchronizes that state** to clients with generic primitives (#76).
  - Its **client half** presents the state with custom rendering, derived locally each frame.
  - A project that needs its own environments builds its own server and headset through builders (#73), without changing Tomori's source.
- Alongside its environment, each space has a **generic scene**: entities built from Tomori's own vocabulary of shapes, materials, standard components and declarative animation (#77). Every client presents it without environment code. The agent's projections (#82) and environments without a client half use it.
- No per-frame geometry crosses the network. Video and audio travel as **media streams** that never delay the conversation or speech (#84).
- Tomori keeps its **own wgpu renderer**, grown into a shared crate, and adopts no game engine (#77). It provides a **default physics engine** for interaction, Rapier, run with the server as authority (#97).
- What participants point at reaches the agent with their words (#81). The agent can **project charts, diagrams and highlights** into the space through Tomori's MCP tools (#82), with live data from data sources (#83).
- The agent receives layered **instructions**: Tomori's own, the environment's and the space's (#72).
- The first concrete environment is **`environment-computer`**: the computer's windows as panels in the space, operated with a virtual keyboard and mouse, playing their audio (#85).
- Tomori gains CI and Base conventions and is **released as a framework** (#73).

## Where things stand

**Choosing an environment.** Each space is bound to one environment when it is created (`design/environments.md`). The server offers environments in `Welcome` (`crates/server/src/environments.rs`), today only Sky. The headset's New space rings include an environment ring only when the server offers more than one (`crates/client-quest/src/arrival.rs`). The client maps an identifier to a built-in `Environment` (`crates/client-quest/src/environment.rs`), and shows the Minimal grid, naming the environment, when it doesn't know one. Nothing changes a space's environment after creation (#74). The desktop window, which has no server, switches with **E**.

**Rendering.** `crates/client-quest` renders with wgpu 30, OpenXR 0.22 and ash 0.38, one 2D swapchain per eye and no MSAA. Sky (`crates/environment-sky`) is a separate crate with per-eye `prepare_eye` and `draw` hooks into the client's pass. Text uses `font8x8`. There is no artificial locomotion and no hardware video decoding.

**Audio.** Clients mix participant streams and agent speech without position (`design/live-audio.md`). An experimental spectator output publishes PipeWire video (`crates/client-quest/src/pipewire_video.rs`), and `docs/slack-huddle.md` matches PipeWire audio streams to processes.

**Agent integration.** A prompt is text only (`backend::Prompt`). Tomori sets no instructions and hosts no tools.

**Repository.** There is no CI. Crate names are too generic to publish.

**OpenCode V2 offers the hooks needed** (from its OpenAPI document), all experimental and scoped by directory rather than session:

- Per-session instruction entries: `PUT/DELETE /api/experimental/session/{id}/instructions/entries/{key}`.
- Prompt `metadata` and file attachments, and `POST /api/session/{id}/synthetic`.
- A client-hosted MCP server over a WebSocket: `GET /api/experimental/client-mcp?name=…`.
- Runtime MCP servers: `PUT /api/experimental/mcp/{server}`.

## Proposal

### 1. What an environment is (#75)

An environment is a Rust crate in three parts, behind Cargo features, so that each binary links only what it needs:

- **Shared protocol:** the types of its replicated state, the intents clients may send and its transient events, plus an integer protocol version.
- **Server half:** linked into the server.
- **Client half:** linked into each client that presents the environment.

**The server half is a program, not only a scene producer.** It runs in the server process with the server's reach, and can query anything the server can: data sources, APIs, the space's directory, version control, commands, or the operating system. It provides:

- identity: identifier, title and description;
- the authoritative state and its handling of intents (section 2);
- descriptions of the resources it presents (section 7);
- anchors (section 6) and a locomotion profile (section 5);
- agent instructions (section 10), and tools and data sources it adds (sections 9 and 11);
- parameters chosen when the space is created.

Secrets, endpoints and credentials stay in server configuration.

Tomori gives the server half a context for its space:

- identity, directory and parameters;
- participants with their roles, and their attention (section 7);
- its state store, events, intents and media streams;
- the server's configuration and data sources;
- a task scope cancelled when the space closes or the server stops.

It runs on its own tasks and never blocks the hub, other spaces or the conversation. When it fails, its space shows the error and keeps its conversation. Being compiled in, it is trusted like the rest of the server. Server halves loaded at runtime, as `design/scene-modules.md` envisages for modules an agent writes, are a later step and would need capability limits.

**The client half presents.** It provides:

- optional scenery, drawn natively as Sky is;
- a presentation derived each frame from its state replica, local view state and the shared clock, drawn with Tomori's toolkit or its own pipelines;
- picking: given a ray, the nearest hit as a distance and a resource identity;
- local view state, and intents for anything shared.

It holds no credentials, accesses no data, and opens no connections.

**Tomori keeps the boundaries:**

- devices, rays, thresholds, hover feedback, locomotion and the hand menu;
- the render pass, camera and per-eye contract;
- the network;
- what the agent is told: a pick reports only an identity, and the server half supplies the description.

**Agreeing on the environment.** A client lists, in `Hello`, the environments it presents with their versions. The server offers each client only the environments it can present. A client joining a space whose environment it lacks, or has at another version, shows the Minimal grid and the space's generic scene, and names the version it needs. Changes a peer can ignore keep the version: new optional fields with defaults, and new variants read as unknown. Any other change raises it.

**Builds.** Environments are compiled in; the headset cannot load native code. Tomori's own builds include Tomori's environments. Agent projection uses only the generic scene, so it needs no client build.

### 2. State and scene replication (#76, #77)

Each space replicates its environment's state and its generic scene on channels Tomori carries, with one synchronization mechanism.

**Primitives:**

- **Collections:** keyed records with revisions.
- **Values:** singletons.
- **Series:** bounded, append-only.
- **Events:** transient.
- **Intents:** client to server half. Participants only; validated, and refused with a reason.
- **Interest:** a client declares interest in an expensive part of the state. It is kept current and sent only to interested clients, and expires unless renewed.

**Delivery:**

- A snapshot on join and reconnect, then ordered deltas; a revision gap is repaired by a snapshot.
- Its own coalesced lane: a newer value of a record replaces an unsent older one, and environment traffic never displaces conversation or speech.
- Snapshots are chunked within the 1 MiB frame limit, with their own budget.
- Per-space limits, enforced with a visible error.
- Times on the server's clock, which clients estimate from keepalive.

Local view state, such as a scroll position, stays on the client. Anything shared is an intent.

**The generic scene** splits what crosses the network by rate:

| What | When it is sent | Example |
|---|---|---|
| Assets | Once, cached by content hash | A mesh, a texture |
| Structure | When entities appear, disappear or change kind | A chart opens |
| Parameters | At the data cadence, coalesced | A value, a colour, a label |
| Animation | Never per frame; evaluated on clients | Flows, pulses, tweens |

Entities have an identifier, parent, owner (the agent stage or an environment's server half), revision and components:

- Transform, and Shape (parametric, or a mesh asset).
- Material: a fixed set of shaders with typed parameters.
- Instances, Series and Curve.
- Text, Panel and Surface: flat, cylindrical or spherical, with exact ray intersection.
- Interactable, Detail (shown by distance) and Resource.
- Body: a physics body and collider (#97).

Animation is declarative: tweens, drivers, time inside shaders, and keyframe tracks. Unknown components show as labelled placeholders. Client halves use the same vocabulary locally as a toolkit.

### 3. Media streams (#84)

A server half may publish video and audio streams, which clients subscribe to through interest. Media never delays control messages, conversation or speech: late media is dropped, keyframes are sent on subscribe and loss, and bitrate adapts. Timestamps are on the shared clock.

The transport is to be chosen: a separate stream, QUIC/WebTransport, or RTP over UDP alongside the UDP audio `design/server.md` plans. On the headset, `AMediaCodec` decodes in hardware into `AHardwareBuffer`, imported into wgpu as a texture. On the server, hardware encoding is used where available.

### 4. Rendering (#77)

**Keep Tomori's own wgpu renderer; adopt no engine.**

- The stack is proven on Quest 3.
- An engine would mean rewriting the client.
- `design/clients.md` anticipates a native visionOS client, where Bevy is unavailable.

A shared crate, `tomori-render`, provides:

- indexed, instanced meshes;
- signed-distance-field text with Unicode;
- curved surfaces used for both drawing and picking;
- ordered-dither transparency with alpha-to-coverage;
- video textures;
- the per-eye contract.

Sceneries and client halves use the same contract. Still to measure and decide: 4× MSAA for the pass, and a frame-time budget.

### 5. Interaction, movement and physics (#78, #97)

**Tomori owns devices; environments receive meaning.** Picks go to the client half or to the generic scene. A client half turns choices into intents; a generic entity sends input to its owner only. Pinch has hysteresis. While grabbing, the dragging client owns the entity's transform until release.

**Locomotion profiles:** `none` (default), `teleport` (onto declared walkable surfaces, with 30° snap turn), and `fly`. Travel is a per-participant rig carried in presence.

**A default physics engine: Rapier (#97).** Rapier is pure Rust and built on `parry3d`, so physics, picking and teleport queries share one set of collider shapes. Environments and the generic scene get it by default, and an environment may opt out.

- **Server authoritative.** Each space with physical content has a physics world on the server, stepped at a fixed rate on its own task, with bounded bodies and step time.
- **Bodies.** A `Body` component in the generic scene: static, kinematic or dynamic, with a collider, mass, friction, restitution and damping. Sceneries declare static colliders.
- **Hands and controllers.** Participants' hand joints and controller poses from presence become kinematic colliders, so anyone can push, poke and press, and everyone sees the same result.
- **Grabbing and throwing.** The grabbing client takes authority over the body and moves it locally with no latency. On release it sends the release velocity, and the server continues the throw.
- **Replication.** Transforms and velocities of awake bodies travel on the synchronization lane at a bounded rate, and sleeping bodies send nothing. Clients interpolate, and may later predict bodies near their hands.
- **Client-only physics** is limited to cosmetic effects.

### 6. Placing the agent (#79)

Environments declare anchors: `agent`, `workers`, `stage` (agent projections), `presentation` (result cards) and `arrival`. Each has a default when omitted.

### 7. Shared attention and agent context (#81)

Resource descriptions (identity URI, kind, label, summary, attributes, context and links) come from whoever presents the resource. Clients report only identities, which the server resolves. Hover travels in presence. Selections and highlights are shared space state, labelled with who made them.

When an utterance begins, the server records the speaker's hovered and selected resources, and prepends a bounded context block to the delivered prompt. The queue shows only the transcript. This relaxes `design/scene-modules.md`'s separation from agent interactions only at the space layer.

### 8. Standard UI kit (#80)

Panel, Text and Label, Button and Toggle, List, Table, Chart, Meter, Diagram, Flow, Legend and Breadcrumb. Feedback is the same everywhere, and everything is anchored in the world. The hand menu gains an **Environment** section for controls the server half publishes.

### 9. Agent projection (#82)

An MCP server named `tomori` for each space's agent provides:

- `show_chart` and `show_diagram`, which also accepts a Mermaid flowchart subset laid out in 3D;
- `show_table`, `show_card` and `place`;
- `highlight`, `selection` and `inspect`;
- `list`, `remove` and `clear`.

Its content belongs to the space's agent stage, which has limits, can be dismissed by anyone, and never changes an environment's state. The transport is OpenCode's client-hosted MCP. Sessions in one directory may reach each other's spaces; these are shared tools.

### 10. Instructions (#72)

Three entries: `tomori` (generic: spoken, brief replies; delegating to sub-agents; showing rather than telling), `tomori-environment` and `tomori-space`. They are set as OpenCode session instruction entries and shown in the hand menu's Agent section. Tomori never writes agent configuration files. This aligns with the opt-in brief speech setting in #65.

### 11. Data sources (#83)

Data sources are server-side connections configured with credentials that never reach clients. Tomori provides a generic Prometheus source. A chart may bind to a query that the server keeps current, shared among charts and only while someone is interested.

### 12. The computer environment (#85)

`environment-computer` brings the computer running the Tomori server into the space.

| Part | What it does |
|---|---|
| Server half | Uses the desktop's ScreenCast and RemoteDesktop portals (`xdg-desktop-portal`, Linux first), encodes window streams, injects validated input |
| State | Shared windows (application, title, size, placement, focus), who is controlling, whether sharing is on |
| Intents | Pointer and key input; moving and resizing panels; taking and releasing control |
| Media | Window video (#86) and window audio played from its panel (#88) |
| Client half | Window panels with video textures and a crisp cursor; ray-to-window mapping |
| Input | Ray pointer, virtual keyboard, Bluetooth keyboards, and a dictation mode (#87) |
| Resources | Each window, described by application and title |
| Locomotion | `none` |

Sharing starts only with the computer's consent, and is visible in the space and on the computer; either side can stop it. Only participants control, by default only owners, one at a time. By default, visitors don't see shared windows.

It is the first environment because it needs every part of the framework and no external services.

### 13. Repositories and packaging (#73)

- Base conventions and CI.
- `tomori-*` crates released together.
- The server and headset become libraries with builders that register environments, data sources and tools.
- Reusable Android packaging, and the default headset APK as a release artifact.

## Decisions

Decided by the user in November 2026:

- Environments are compiled into both the server and the clients.
  - Client and server agree on an environment's protocol version.
  - The environment has its own state synchronization.
  - Its server half is not limited to rendering: it can query anything.
- Sessions in one directory may share the `tomori` tools across spaces.
- Tomori provides a default physics engine for interaction (#97).
- Tomori sets per-session instructions.

Still open, discussed in the issues:

- the synchronization encoding (#76);
- the server half's reach into the agent (#75);
- changing a space's environment (#74);
- the media transport (#84);
- locomotion profiles (#78);
- physics rates, limits and client prediction (#97);
- prompt context limits (#81);
- agent content persistence (#82);
- instruction lifetime and wording (#72);
- the first publication (#73).

## Proposed order

1. Instructions (#72) and the framework release work (#73), which are independent.
2. Environment crates, synchronization and the generic scene (#75, #76, #77): Sky as the first environment crate, plus a test environment.
3. Agent projection and shared attention (#82, #81), then data sources (#83).
4. Interaction and physics, anchors and the UI kit (#78, #97, #79, #80) as environments need them.
5. Media streams (#84), then `environment-computer` (#85–#88).

## Changes to existing documents

On acceptance:

- `design/environments.md`: version agreement, and answers to its open questions.
- `design/scene-modules.md`: "clients need no feature-specific code" narrows to the generic scene and agent content; the agent separation is relaxed at the space layer; the physics question is answered: a default engine, with the server as authority (#97).
- `design/agent-backends.md`: instruction entries and tool hosting become backend capabilities.
- `design/spaces.md`: highlights and the agent stage join shared state, and the rig joins presence.
- `design/clients.md`: the shared renderer and client builders.
- `design/hand-menu.md`: the Environment and Agent sections.
- `design/live-audio.md`: positioned sources.
- `design/server.md`: the media transport.
- `design/security.md`: tool routing, data-source credentials, and environment media for visitors.
