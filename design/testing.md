# Testing

This describes the test coverage Tomori's workspace must keep.

## Requirement

Every measured source region in the workspace runs under its tests: `cargo bake test:coverage --all-targets true` passes on current stable Rust, on CI's Linux runner with lavapipe and the speech models. The Test workflow's `test-result` job requires it for every pull request and push to main, and the main branch requires `test-result`. The merge queue, which tests each pull request again merged with those ahead of it, runs the tests without coverage instrumentation, as the pull request has passed the gate on its own. The target is 100%, never a lower floor.

Uncovered code gets a test, or is removed if nothing needs it. A branch that only a race could take is restructured, so that whether it runs does not depend on timing. Nor does coverage depend on which tests share a process or which runs first: a log line's arguments are evaluated only when the log level allows them, so each test that reaches one raises the level itself, with `test_support::log_everything` in the client and the scene, `log::set_max_level` elsewhere, or `test_log::record` in the server, where a test that causes a warning an operator relies on also checks it with `test_log::logged`. Bake does not count regions inside `unreachable!`, which is left only where the types or an immediately preceding check rule the branch out, with a comment saying why.

A failure that the system causes only rarely, such as a thread or scheduler it cannot start, is handled as it was before coverage was required: the clients report it and carry on or try again, and the server refuses the one request it affects. A private `#[cfg(test)]` shim refuses the start in tests, so that the handling runs; production code calls the system directly. Threads the server cannot run without, such as its speech engines', are still expected to start. State that only the code's own invariants keep consistent, such as the server hub's, is looked up without panicking where input from the network, OpenCode, files or the system leads to it, and the hub's lock recovers if a request ever panics holding it, so that one mistake costs one request rather than the server.

The only other exception is a file of raw device or platform calls that cannot run in CI, listed with its reason under `[[package.metadata.bake.test.coverage.exclude]]` in its crate's `Cargo.toml`. Such a file is thin. It makes the calls and passes their results on, and decisions around them belong in covered modules. Two still make some that could move out: `xr.rs` decides which controller models to load, decode and retry, how often to try again, how hand tracking falls back, and when to log poses; `desktop_window.rs` translates window events into the window's input and decides what each surface status does. The exclusions are:

| File | What cannot run in CI |
| --- | --- |
| `crates/client-quest/src/xr.rs` | The OpenXR instance, session, swapchain and actions, and the frame loop that drives them, which need an OpenXR runtime and a headset. |
| `crates/client-quest/src/vulkan.rs` | The Vulkan instance and device created through OpenXR, the runtime's swapchain images imported into wgpu, and the eyes' density maps: the device functions wrapped to attach them, and Tomori's own map's image, which need the headset's GPU; which passes and framebuffers get a map, and the map itself, are decided in `foveation.rs`. |
| `crates/client-desktop/src/desktop_window.rs` | winit's event loop and window, and the window's Vulkan or Metal surface, which need a display. |
| `crates/client-desktop/src/desktop_audio.rs` | The default microphone and speaker, opened through cpal. |
| `crates/client-spectator/src/child_process.rs` | What a forked child runs before it executes its program, whose coverage counters the child discards. |
| `crates/server/src/exit.rs` | Ending the server process at once with `_exit`, after which it writes no coverage. |
| `crates/discovery/src/daemon.rs` | Starting the multicast DNS responder and querier, whose failures come only from the system. |

Coverage is not achieved through `#[coverage(off)]` or `cfg(coverage)`, nor by measuring less of the workspace.

Every test in the workspace, including those in `tests/` directories and the examples', is meant to pass on macOS as well as on Linux; CI checks only Linux, so this is kept by hand, as best effort, by whoever runs the suite on a Mac. A test that needs what one platform cannot give it, such as a file name that is not UTF-8, PipeWire, an OpenXR runtime or a machine without a display, says so: it is compiled only where it can run, with `#[cfg(target_os = …)]`, or ignored elsewhere with `#[cfg_attr(…, ignore = "…")]` and its reason, so that it adds no region that CI does not run. A test that runs a window mode to see it fail without a display runs on Linux alone, where leaving out `DISPLAY` and `WAYLAND_DISPLAY` hides the display; on macOS the window opens and waits to be closed. A test that compares a path the code canonicalises compares it with the canonical form of the path it gave, since on macOS `/tmp` is a link to `/private/tmp`. A computed float is compared within a tolerance where platforms' maths libraries round differently, as macOS's `atan2f` and glibc's do.

## Rationale

The last regions find as many bugs as the first ones, and they expose dead code. A lower floor makes people test code they don't care about whenever someone else's change lowers the total. With 100%, each change brings its own tests. Some code can't run in GitHub CI, such as the headset's OpenXR calls. An exclusion that names its file and reason, in the crate it belongs to, keeps that visible and reviewable, where scattered markers would not. Agreed with the maintainer in November 2026 in [#55](https://github.com/base-rs/tomori/issues/55); see the proposal in [#111](https://github.com/base-rs/tomori/pull/111).

Instrumentation takes most of the Test workflow's time, and every pull request has passed the gate before it is queued, so the merge queue only checks that the merged workspace builds and passes; the push to main that follows runs the gate again, so that a combination of pull requests that leaves a region uncovered shows there. The maintainer chose this in November 2026.

CI runs on Linux, but the maintainers also develop on Macs, where a test that fails or hangs for a reason of the platform's hides the failures that matter.

## Scope

Every workspace member's code, including its inline tests and the `bake/` package, as `cargo llvm-cov --workspace --all-targets` measures it on Linux. The examples, and files in `tests/` directories, are not measured. Code compiled only for Android is not measured either. It belongs in the headset's thin platform files.

## Validation

- `cargo bake test:coverage --all-targets true` passes in CI, for every pull request and push to main. Its report lists each excluded file with its reason and its size, in regions and lines.
- A change that adds or widens an exclusion is reviewed as lowering the target would be. Its file must hold only device or platform calls, its reason must say what cannot run, and no logic may have moved into it.
- Search the source for `coverage(off)` and `cfg(coverage)`: there is neither.
- `cargo test --workspace` on macOS passes, opening no window, when run by hand (CI does not check it). Every `ignore` in a test names its reason, and none applies on Linux unless the test needs something CI does not have.
