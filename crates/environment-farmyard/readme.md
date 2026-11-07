# `tomori-environment-farmyard`

[Tomori](https://github.com/ioquatix/tomori)'s farmyard: an evening yard, fenced, with a red chicken coop raised on legs, its ramp and little door, a window to peek in at the nest boxes, a water trough, a feeder with grain scattered about it, hay bales and lanterns, under a rising moon with pines beyond.

This crate is the farmyard's common crate, what its server and its clients share, building no renderer or GPU stack: its identity, `ID`, `VERSION` and `LABEL`, where everything stands in the yard (`layout`), and its yard and coop, built in code as `tomori-assets` models (`props`, `coop`). A server offers it by registering its identity:

```rust
use tomori_server::{Environment, Server};

let builder = Server::builder().environment(
    Environment::new(tomori_environment_farmyard::ID, tomori_environment_farmyard::LABEL)
        .with_version(tomori_environment_farmyard::VERSION),
);
```

Its client half, the scenery Tomori's clients draw, is [`tomori-environment-farmyard-client`](https://crates.io/crates/tomori-environment-farmyard-client).

It is an environment built as another project would build one, from Tomori's published crates and their extension points, and nothing in Tomori's own crates knows about it: the proof that a downstream environment crate can customise everything about its environment without changing Tomori.

The coop is a model of its own, which a coop modelled in Blender can replace, as [`context/coop.md`](context/coop.md) describes. The interface is 0.x. Its requirements are in Tomori's [`design/farmyard.md`](https://github.com/ioquatix/tomori/blob/main/design/farmyard.md).

## See Also

- [Tomori](https://github.com/ioquatix/tomori), the conversational spatial workspace these crates belong to.
- [`tomori-assets`](https://crates.io/crates/tomori-assets), which loads and draws its models.
