# Tomori Crates

Tomori is a conversational spatial workspace for people and agents: a headset or desktop client joins a space on a Tomori server, which runs an agent session for that space, transcribes what participants say to it, and speaks its replies. Use this guide to choose which Tomori crates a project needs.

## Published crates

The crates share one version and are released together, so depend on the same version of each.

| Crate | Purpose |
| --- | --- |
| `tomori-protocol` | The wire protocol: control messages, audio frames and the framing of a connection. Every other crate builds on it. See [Using the Tomori Protocol](protocol.md). |
| `tomori-client` | A client's connection to a server, with reconnection, keepalive, microphone audio and playback, and the `Connection` that carries out a client's session with a server as Tomori's own clients do. Start here to build a client. |
| `tomori-discovery` | Bonjour advertising and browsing of servers (`_tomori._tcp`). `tomori-client` re-exports it as `tomori_client::discovery`. |
| `tomori-security` | Tokens, pairing codes and certificate pinning, shared by clients and the server. The `issue` feature adds the server's token store and certificate. |
| `tomori-backend` | The backend-neutral interface between the server and coding agents: implement `Backend` and `Session` to drive another agent. |
| `tomori-backend-opencode` | The backend that drives OpenCode sessions on a running `opencode serve`. |
| `tomori-environment` | The interface of an environment's server side: implement `ServerSide` to run a program in the server for each space bound to the environment. Its Making an Environment guide describes an environment's common, server and client crates. |
| `tomori-assets` | Environments' assets, loaded at runtime: content-addressed `.glb` models read as untrusted input, their instances and poses, and drawing them skinned with wgpu (the default `gpu` feature). See its Using Tomori's Assets guide. |
| `tomori-speech` | Streaming speech recognition, speakable text selection and synthesis, through prebuilt sherpa-onnx libraries. |
| `tomori-environment-sky` | The identity of the Sky environment, its identifier and version, which its server and client halves share. |
| `tomori-environment-sky-client` | The renderer of the Sky environment, a moonlit sky, clouds, a platform and a shrine, drawn with wgpu, and its registration as a scenery. Tomori's own app and spectator register it; its interface changes when environments get a shared interface. |
| `tomori-environment-farmyard` | The farmyard environment, an evening yard with a chicken coop and a flock of chickens: its identity, layout, models, the chickens' behaviour, controller and animator graph, and its server side, which its server and client halves share. |
| `tomori-environment-farmyard-client` | The farmyard's client half: its scenery, the chickens' client side and its teleport profile, registered with a client's sceneries. Tomori's own app and spectator register it. |
| `tomori-server` | The server library, whose builder registers backends and environments, starting with none, of Tomori's own or another project's. See its Building a Tomori Server guide. |
| `tomori-server-app` | Tomori's own server, the `tomori-server` executable, which registers Tomori's own environments on the library's builder. |

## Unpublished crates

- `tomori-client-quest`, the headset and desktop application, whose library is `tomori`, is released as an Android APK rather than to crates.io.

## Choosing crates

- **A client of an existing server:** depend on `tomori-client` and `tomori-protocol`. The client presents tokens and pins certificates, and re-exports discovery; the application supplies the token, and stores the certificate fingerprint to pin.
- **Another implementation of the protocol, in Rust:** depend on `tomori-protocol` alone, and follow its framing, versioning and keepalive rules.
- **A backend for another agent:** depend on `tomori-backend`, and report structured events rather than text for display or speech.
- **A server:** run `tomori-server`, from `tomori-server-app`, or build one of your own on the `tomori-server` library, registering your own backends and environments through `tomori_server::Server::builder`; its Building a Tomori Server guide explains how. Its interface is not stable yet.
- **An environment:** make a common crate for its identity, and server and client crates beside it, which applications register; `tomori-environment`'s Making an Environment guide explains how.

The design documents in the [Tomori repository](https://github.com/ioquatix/tomori/tree/main/design) state the requirements these crates follow, such as `server.md` for the connection and `security.md` for authentication.
