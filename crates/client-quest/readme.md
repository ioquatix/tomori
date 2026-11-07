# `tomori-client-quest`

[Tomori](https://github.com/ioquatix/tomori)'s headset, for the Meta Quest with OpenXR, and the `tomori` library and executable that run it, its desktop modes and the spectator. The library is also what another project builds a headset of its own on, through `tomori::Builder`, registering sceneries for environments of its own, and how participants travel in them, without changing Tomori.

## Usage

Tomori's own headset is an APK, `com.codeotaku.tomori`, attached to each [GitHub Release](https://github.com/ioquatix/tomori/releases) as `tomori-VERSION.apk`; [`scripts/android.sh`](https://github.com/ioquatix/tomori/blob/main/scripts/android.sh) builds it from Tomori's repository too. On the desktop:

```sh
cargo install tomori-client-quest
tomori                                       # the server-less window
tomori --desktop --server 192.168.1.10:8765  # take part in a server's spaces
tomori --xr --server 192.168.1.10:8765       # a desktop OpenXR runtime
```

`tomori --help` lists every mode. `tomori::Builder::default()` draws nothing but the built-in grid; Tomori's own build, `tomori::app::builder()`, behind the default `app` feature, registers Tomori's own environments on it, each from its own crate. A headset of your own registers its sceneries, at version 1 with `Builder::scenery`, or at the environment's protocol version with `Builder::versioned_scenery`, and those of other environments from their client crates with `Builder::register`, such as Sky's. The build's `Hello` lists them with their versions, so that a server offers it only environments it presents at the same version:

```rust
fn builder() -> tomori::Builder {
    tomori::Builder::default()
        // Sky, as Tomori's own build draws it.
        .register(tomori_environment_sky_client::register)
        .scenery("observe", my_scenery)
        // Teleport onto a floor 40 m across, and turn in snaps of 30°; of Tomori's own environments, only the farmyard declares one. Provisional, until environments have server sides to declare it.
        .locomotion(
            "observe",
            tomori::Locomotion::teleport(vec![tomori::Surface::rectangle(
                glam::Vec3::ZERO,
                glam::Vec2::splat(40.0),
            )]),
        )
}

fn main() -> anyhow::Result<()> {
    tomori::main(builder())
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: tomori::AndroidApp) {
    builder().run_android(app)
}
```

Depend on it without default features, which hold Tomori's own app, its `android_main` and its environments' crates:

```toml
tomori-client-quest = { version = "0.1.0", default-features = false }
```

[Packaging a Headset Build for the Quest](context/android.md) shows how to package it as an APK of its own, with its own application ID, label, icon and assets, and [examples/headset](https://github.com/ioquatix/tomori/tree/main/examples/headset) is a complete example.

Its interfaces change with the environment framework ([#75](https://github.com/ioquatix/tomori/issues/75), [#77](https://github.com/ioquatix/tomori/issues/77)).

## See Also

- [Tomori](https://github.com/ioquatix/tomori), the conversational spatial workspace these crates belong to.
- [`tomori-render`](https://crates.io/crates/tomori-render), for the `Scenery` a build draws.
- [`tomori-server`](https://crates.io/crates/tomori-server), whose builder adds the server's half of an environment.
