# `tomori-environment-farmyard`

[Tomori](https://github.com/ioquatix/tomori)'s farmyard: an evening yard, fenced, with a red chicken coop raised on legs, its ramp and little door, a window to peek in at the nest boxes, a water trough, a feeder with grain scattered about it, hay bales and lanterns, under a rising moon with pines beyond, where a small flock of chickens wanders, pecks, roosts in the coop, and looks up puzzled when someone points at one.

This crate is the farmyard's common crate, what its server and its clients share, building no renderer or GPU stack: its identity, `ID`, `VERSION` and `LABEL`; where everything stands in the yard (`layout`); its yard and coop, built in code as `tomori-assets` models (`props`, `coop`); the chicken, its asset, its animator graph and its controller (`chicken`, `controller`); what each chicken does and where that puts it (`behaviour`); the flock (`flock`); its clucks (`cluck`); and its server side, `side::FarmyardSide`. The farmyard has no server crate, as its server side holds nothing that must stay in the server: no credentials, data sources or options, only the flock's simulation, which its clients replay. A server offers it by registering its identity with its server side, as Tomori's own server, `tomori-server-app`, does:

```rust
use tomori_environment_farmyard::{ID, LABEL, VERSION, side::FarmyardSide};
use tomori_server::{Environment, Server};

let builder = Server::builder().environment(
    Environment::new(ID, LABEL)
        .with_version(VERSION)
        .with_server_side(FarmyardSide),
);
```

Its client half, the scenery Tomori's clients draw, the chickens' client side and its teleport profile, is [`tomori-environment-farmyard-client`](https://crates.io/crates/tomori-environment-farmyard-client).

It is an environment built as another project would build one, from Tomori's published crates and their extension points, and nothing in Tomori's own crates knows about it: the proof that a downstream environment crate can customise everything about its environment without changing Tomori.

How the chickens work is in [`context/chickens.md`](context/chickens.md). The chicken, its model, textures and animations, is by **Hinoki Maple Williams**, and is licensed under the [Creative Commons Attribution 4.0 International licence](https://creativecommons.org/licenses/by/4.0/) (CC BY 4.0), as [`assets/models/LICENSE.md`](assets/models/LICENSE.md) says; the code is MIT, so the crate is `MIT AND CC-BY-4.0`. The coop is a model of its own, which a coop modelled in Blender can replace, as [`context/coop.md`](context/coop.md) describes. The interface is 0.x. Its requirements are in Tomori's [`design/farmyard.md`](https://github.com/ioquatix/tomori/blob/main/design/farmyard.md).

## See Also

- [Tomori](https://github.com/ioquatix/tomori), the conversational spatial workspace these crates belong to.
- [`tomori-assets`](https://crates.io/crates/tomori-assets), which loads and draws its models.
