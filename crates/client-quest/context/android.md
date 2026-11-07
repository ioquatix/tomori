# Packaging a Headset Build for the Quest

A headset build of your own, made with `tomori::Builder` (see `design/builders.md` in Tomori's repository), is packaged as an APK of its own with [`cargo-apk`](https://crates.io/crates/cargo-apk) 0.10, which Tomori uses for its own. It has its own application ID, label, icon and assets, and installs beside Tomori's own app, `com.codeotaku.tomori`, without changing Tomori's source. [`examples/headset`](https://github.com/ioquatix/tomori/tree/main/examples/headset) is such a build, and Tomori's CI packages it.

## The crate

The build is a library crate, compiled as a shared library for the APK, that depends on `tomori-client-quest` without its default `app` feature, which holds Tomori's own app, its `android_main` and its environments' crates, and writes its own:

```toml
[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
tomori-client-quest = { version = "0.1.0", default-features = false }
```

```rust
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: tomori::AndroidApp) {
    tomori::Builder::default().scenery("observe", my_scenery).run_android(app)
}
```

`tomori::Builder::default()` draws nothing but the built-in grid, so the build registers every environment it presents, its own and others' from their client crates, such as Sky's with `.register(tomori_environment_sky_client::register)`. A scenery registered with `scenery` presents its environment at protocol version 1; `versioned_scenery("observe", 2, my_scenery)` presents it at another, which must match the version the server offers it at (see `design/environments.md`). `tomori::AndroidApp` is `android-activity`'s, re-exported so that the build's version matches Tomori's. The `rlib` is for the build's tests and any desktop executable, which runs the same builder with `tomori::main(builder)`.

## The manifest

`cargo-apk` reads `[package.metadata.android]` only from the package it builds, so the build has a copy of [`tomori-client-quest`'s](https://github.com/ioquatix/tomori/blob/main/crates/client-quest/Cargo.toml), from `[package.metadata.android]` to the end, and changes only what is its own:

```toml
[package.metadata.android]
# Its own application ID. The app's identity, the server it last joined, its certificates and tokens, its settings and its permissions are kept per application ID, so a new one pairs again.
package = "com.example.observe"
# The APK: target/debug/apk/observe.apk.
apk_name = "observe"
build_targets = ["aarch64-linux-android"]
# The Khronos OpenXR loader: its arm64-v8a/libopenxr_loader.so, relative to this manifest.
runtime_libs = "android/libs"
# Its own assets, with the licence notices of what tomori-render compiles in.
assets = "assets"
# Its own Android resources, such as its launcher icon.
resources = "res"
strip = "strip"

[package.metadata.android.application]
label = "Observe"
icon = "@mipmap/icon"
# …the rest as tomori-client-quest's.
```

Keep the rest as Tomori's: the OpenXR and Oculus features, permissions and queries, the `NativeActivity` with its VR intent category, `extract_native_libs` and `uses_cleartext_traffic`. The headset needs them to start, to find the OpenXR runtime, to record audio, to find servers with Bonjour and to show the controllers and hands.

- **Application ID:** each application ID is a separate app on the headset, with its own private storage: its participant identity (`participant-2`), the server it last joined (`server`), each server's pinned certificate and token (`credentials.json`) and its feedback setting (`feedback.json`), and its own granted permissions. A build with a new ID starts without them and is paired with its server again.
- **Icon:** `resources` names a folder of Android resources, compiled with `aapt`; `icon` names one, such as `res/mipmap/icon.png` as `@mipmap/icon`. Tomori's own APK has none.
- **Assets:** `assets` names one folder, packaged as the APK's `assets/`, which the build reads on the headset through `AndroidApp::asset_manager`. The APK must carry the licence notices of what `tomori-render` compiles in, the controller models, the Basis Universal transcoder and the fonts: copy its `assets/controllers` and `assets/licenses` into the build's folder, from the `tomori-render` package it depends on. A build that registers an environment with models of its own carries them too, at the environment's identifier, where the app reads them on the headset, as environments' crates embed them everywhere else (see `design/assets.md` in Tomori's repository); the environment's client crate says which folder, as the farmyard's does. Tomori's own APK's folder is `android/assets` in its repository, and the example links to the same assets instead, as it is in Tomori's repository.
- **The OpenXR loader:** `runtime_libs` names the folder holding `arm64-v8a/libopenxr_loader.so`, which is not committed.
- **Version:** `cargo-apk` sets `versionName` and `versionCode` from the package's version.
- **Intent extras and log tag:** these belong to the framework, not the app, so they are the same for every build: `tomori.server`, `tomori.name`, `tomori.token`, `tomori.msaa` and `tomori.echo_test`, and the logcat tag `tomori`.

## Building

Tomori's [`scripts/android.sh`](https://github.com/ioquatix/tomori/blob/main/scripts/android.sh) builds, installs and runs any package with such a manifest. It reads the application ID, the APK's name and the loader's folder from `cargo metadata`, so it needs `jq`, and fetches the loader into that folder, checked against its published SHA-256, with [`scripts/fetch-loader.sh`](https://github.com/ioquatix/tomori/blob/main/scripts/fetch-loader.sh), when it is missing:

```sh
bash scripts/android.sh --package tomori-example-headset build
bash scripts/android.sh --package tomori-example-headset run
bash scripts/android.sh --package tomori-example-headset path
```

`path` prints where `build` writes the APK, such as `target/debug/apk/tomori-example-headset.apk`. Without `--package` it builds Tomori's own, `tomori-client-quest`. A project of its own copies both scripts into its own `scripts/`, run from the repository's root, or runs the same steps:

```sh
bash scripts/fetch-loader.sh android/libs
cargo apk build --package observe --lib
adb install -r target/debug/apk/observe.apk
adb shell am start -n com.example.observe/android.app.NativeActivity
```

Debug builds are signed with the local debug key, `~/.android/debug.keystore`, which `cargo-apk` creates if needed.

## Releasing

`--release` builds the release profile, which `cargo-apk` signs with the keystore that `CARGO_APK_RELEASE_KEYSTORE` names, with the password in `CARGO_APK_RELEASE_KEYSTORE_PASSWORD`; it never uses the debug key for a release. The APK is then `target/release/apk/NAME.apk`, and is not debuggable. Android identifies an app's updates by that key, so keep it, and a backup, out of the repository, and check what you publish with the SDK's build tools: `apksigner verify --print-certs` prints the signer's certificate digest, and `aapt2 dump badging` must name your application ID and print no `application-debuggable`:

```sh
CARGO_APK_RELEASE_KEYSTORE=… CARGO_APK_RELEASE_KEYSTORE_PASSWORD=… bash scripts/android.sh --package observe --release build
apksigner verify --print-certs "$(bash scripts/android.sh --package observe --release path)"
```

Tomori's own release builds its APK unsigned, so that the build never has the key, signs it in a step of its own, and checks it against its committed certificate, with Bake tasks in its repository, `cargo bake android:apk:build`, `sign` and `verify`; its `design/android-release.md` and `scripts/release-artifacts/` show how.
