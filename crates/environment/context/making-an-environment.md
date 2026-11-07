# Making an Environment

A Tomori environment is what a space is bound to: what its participants see and interact with, and what its agent is told about it. Tomori's core crates, the server library and the client libraries, know no environment but the Minimal grid, which every client draws built in. Every other environment, Tomori's own Sky included, is made in crates of its own and registered by the applications that offer and present it, so that adding one changes nothing in Tomori. Use this guide to make one.

## Its crates

An environment is up to three crates, named for it:

- **Common**, `NAME`, such as `tomori-environment-sky`: what its halves share. At least its identity: the identifier spaces name it by, its protocol version and its label. Later, its message formats and any simulation both halves run. It must not depend on a renderer or the GPU stack, such as `tomori-render` or wgpu, so that a server builds it.
- **Server**, `NAME-server`: what runs only in the server, such as its server side, an implementation of this crate's `ServerSide`, its data sources and its credentials. A credential, such as an API token, lives here and nowhere else: never in the common crate, which clients build, nor in what the server sends. It must not depend on a renderer either.
- **Client**, `NAME-client`, such as `tomori-environment-sky-client`: how a client presents it, such as a scenery, an implementation of [`tomori-render`](https://crates.io/crates/tomori-render)'s `Scenery`, with a `register` function that registers it on a client's `Sceneries` at the common crate's identity. It may depend on the renderer.

Leave out a crate an environment has no use for: Sky has no server half. The GitHub prototype has all three: `tomori-environment-github`, its identity; `tomori-environment-github-server`, which holds its GitHub client and token and adds its `--github-repository` option to a server's builder; and `tomori-environment-github-client`, which presents it over the grid.

```rust
// NAME: the common crate.
pub const ID: &str = "observe";
pub const VERSION: u32 = 1;
pub const LABEL: &str = "Observe";
```

```rust
// NAME-client: its scenery, registered at its identity.
pub fn register(sceneries: &mut tomori_render::scenery::Sceneries) {
    sceneries.register_versioned(observe::ID, observe::VERSION, my_scenery);
}
```

Raise the version in the common crate, and change both halves together, when a client of the earlier version could not present the environment as the server now offers it; a change an older client can ignore keeps it.

## Registering it

An application registers each environment it offers or presents, through the same calls Tomori's own programs make:

```rust
// A server, on tomori-server's builder.
let server = tomori_server::Server::builder().environment(
    tomori_server::Environment::new(observe::ID, observe::LABEL)
        .with_version(observe::VERSION)
        .with_server_side(observe_server::Side::new()),
);
```

```rust
// A headset, on tomori-client-quest's builder.
let headset = tomori::Builder::default().register(observe_client::register);
```

The server offers a client only the environments it presents at the same version, and a client in a space whose environment it lacks shows the grid and names it. Tomori's own server program, `tomori-server-app`, registers Sky, the grid and the farmyard, and Tomori's own app, `tomori-client-quest`'s `app` feature, registers Sky's and the farmyard's client halves, in the same way.

## The dependency rule

No core crate depends on an environment's crate, so environments can build on the core, and on one another, without a cycle; Tomori checks this, and that an environment's common and server crates build no renderer, with a test that reads `cargo metadata`. Tomori's design document, [`design/environments.md`](https://github.com/ioquatix/tomori/blob/main/design/environments.md), states the requirements, and [`design/builders.md`](https://github.com/ioquatix/tomori/blob/main/design/builders.md) the builders'.
