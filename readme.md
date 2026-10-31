# Tomori

A conversational spatial workspace for people and agents, currently a native
**Rust + OpenXR + wgpu/Vulkan** prototype for **Meta Quest 3**.
128 agents inhabit a selectable moonlit Sky environment or minimal floor grid.
Real stereo views use the headset's predicted poses and asymmetric eye projections. The Quest
renders the interactive scene and joins conversations through a Tomori server on
the PC, which runs the agent sessions.

Tomori is the name of both the system and its conversational coordinator; see
[naming.md](naming.md). The Rust package and executable are named `tomori`.
The headset finds servers with Bonjour or over USB; several participants can share
a space. Voice is moving from the PC bridge to the server, so the headset is
text-only for now. Sky is an early environment blockout, not finished artwork.
Private workspace memory and session state are not included in this public
source repository.

- **Right trigger:** attract the swarm to the right controller (orange cube).
- **A:** connect, join a space, or confirm an approval; otherwise reset the swarm.
- **Squeeze the right grip:** switch between Sky and Minimal environments.
- **Hold Meta button:** Quest's system recenter brings the swarm in front of you.
  A short press opens the system menu. No artificial camera movement.

## Environments

**Sky** is the default: a fixed moonlit night, sparse stars, static cloud banks,
a stable platform and an open shrine blockout with warm lanterns. The concrete
renderer lives in [`crates/environment-sky`](crates/environment-sky/README.md),
separate from sessions, worker simulation and application input.

- **Quest / desktop XR:** squeeze and release the right grip to switch presets.
- **Desktop window:** press **E** to switch presets; or launch with
  `TOMORI_ENVIRONMENT=minimal cargo run -p client-quest` for the original grid.
- **Spectator:** followed the headset's selection; the headset doesn't publish
  to the spectator for now.

Switching does not reset agents, conversations or worker positions. Selection
is not persisted across launches. Sky uses the same scene/recenter transform as
the workspace; it does not move the camera. Clouds use a bounded low-resolution
volumetric density pass with translucent edges and approximate moonlighting;
lanterns have brighter emissive faces and cast warm, occluded illumination onto
the nearby static scenery through a build-time lightmap, not runtime point-light
shadow maps. The same lightmap adds local ambient occlusion at contacts.
The shrine and platform cast/receive softly filtered moon shadows from a cached
static shadow map. Agent/status visuals and labels are not shadowed.
There is no day/night cycle, scenery collision or new locomotion system.

The scene has been exercised on Quest 3 and tested offscreen in Vulkan; the
on-device frame-time budget still needs measurement. To run
the stereo/switch/recenter regression and optionally save a preview sheet:

```sh
TOMORI_SKY_PREVIEW=/tmp/opencode/tomori-sky.png cargo test --locked -p client-quest \
  sky_environment_renders_stereo_switches_and_preserves_recenter -- --ignored
```

## Join a space on a Tomori server

Start the server on the PC with the Claude Code backend (see
[design/server.md](design/server.md)). Spaces may run in the directory the
server starts in or beneath it; give `--workspace-root PATH`, repeated as
needed, to allow other directories instead:

```sh
cargo run -p server --bin tomori-server -- --backend claude-code
```

The headset joins existing spaces but can't create them yet, so create one from
the PC with the example client: give it the server's address, then type
`create` with the backend and a directory beneath a workspace root.

```sh
cargo run -p client --example connect -- 127.0.0.1:8765
create claude-code /home/sam/project
```

The space stays on the server after the example quits.

It advertises itself with Bonjour, so the headset lists it when both are on the
same network. Over USB, `bash scripts/android.sh run` forwards the headset's
port 8765 to the PC with `adb reverse`, and the list's **USB** entry connects
through it; set `TOMORI_SERVER_PORT` when the server listens on another port.
To skip the list, give an address:
`TOMORI_SERVER=192.168.1.10:8765 bash scripts/android.sh run`, or pass the
intent extras `tomori.server` and `tomori.name` to `am start`. Your name
defaults to the headset's model name. The headset remembers the last server it
connected to, and connects to it again when it finds it, unless you move the
selection first.

### Controls

- **Servers:** flick the left stick up/down to choose; **A** connects; **Y**
  searches again. A server with an incompatible protocol is listed with its
  version but can't be chosen.
- **Spaces:** flick the left stick up/down to choose; **A** joins; **B** returns
  to the servers; **Y** lists again.
- **In a space:** flick the left stick up/down to page through text and
  left/right to move between messages; the queue follows the last message.
  **Y** fetches the space again.
- **Pending approval:** flick the right stick left/right to choose *allow once*
  or a standing option, then press **A**; **B** denies. Nothing is decided
  without a press.
- **Pending question:** answer it on the PC for now.
- **B:** deny a pending approval, otherwise cancel your newest queued entry,
  otherwise leave the space.

The server keeps the conversation: after a dropped connection the headset
reconnects and rejoins the space by itself. If the space is gone, as after the
server restarts, the headset returns to the space list.

### Delegated workers and result cards

The headset doesn't show workers or result cards for now: the PC bridge
provided them, and they return through the server.

The coordinator can use OpenCode's existing subagent tool. Child sessions of the
open root conversation appear as smaller swarms: cubes travel from the coordinator
or parent worker to an automatically placed station. Stations form a stable arc
around the observer's reference position, with inward-facing signposts. Existing
history appears settled rather than masquerading as newly delegated work. On
observed completion a worker settles into one solid, slowly rotating cube; on
error, separate fragments with a gentle wave.
Their labels and symbols show observed running, waiting, idle, completed, error,
or unknown states; stale or absent data is not treated as completion. Partial observation
errors do not erase independently observed worker statuses. This is a visual
projection of actual OpenCode sessions, not a simulator or invented progress.

The coordinator can explicitly publish a reviewed result, linked to a source
session and message. Workers do not need spatial interfaces and their outputs
are not automatically displayed. Use `node scripts/workspace.mjs help` for the
`list`, `present`, and `remove` commands. One focused card is displayed at a
time, with card and content page counters. Tables use a column/row record view;
long headers and cells continue across pages without clipping. The bitmap font
displays unsupported Unicode and control characters as `\u{HEX}` code-point
escapes. Titles and source identifiers may be shortened in the header. Cards are stored in
`.space/presentations.json` in this space (one bridge writer). The current limits
are 64 worker nodes, up to 128 visible stations including retained/departing
workers, and 16 cards per root. Every supported station gets a complete label;
long titles wrap instead of being hidden or shortened. Stable cells around the
observer separate labels; large workspaces require looking around and up.
Labels remain world-space and can be physically occluded, rather than rendering
through opaque objects. Upstream discovery limits are indicated separately.
Updated spectators accept old frames, defaulting missing cube rotation to identity.

Button actions are processed independently of controller poses and render
availability; losing a controller's tracked pose must not discard X presses or
releases. Real session focus loss still cancels recording. Headset validation
is needed to distinguish missing tracking from Quest runtime/profile changes
that deactivate the button actions themselves.

Worker titles, statuses and published cards also appear in the spectator/OBS
view: publish only content appropriate for that audience. Worker access to the
coordinator's memory is currently restricted by instructions, **not** by a
security sandbox. Permissions are never automatically approved; pending child
permissions or forms still need to be handled in OpenCode. OpenCode's active
status covers foreground session drains and is not proof that all background
work everywhere has stopped.

After reviewing a terminal worker result, the coordinator can use
`node scripts/workspace.mjs acknowledge --session ROOT --worker CHILD` to clear
its visual presence without deleting or archiving the session. `restore` brings
it back; clearing a station sends its cubes back toward the coordinator. New
activity or a changed outcome also makes it visible again. Active
or waiting workers cannot be acknowledged, and visible children keep their
ancestors visible. Visibility decisions persist in `.space/worker-visibility.json`.

### Swarm conversation signals (prototype)

Join a space to give the swarm an observable conversational state:

| Appearance | Meaning |
| --- | --- |
| Tight violet circulation, rotating broken halo | The space's agent is working, or input is queued |
| Open, calm mint swarm with a steady complete halo | The agent is idle and nothing is queued |
| Amber, gently pulsing broken halo | An approval or question needs an answer |
| Dim fragmented halo / red fragmented halo | Status unknown / the server is unreachable |

The ready halo persists; you don't need to catch an end-of-turn flash. An
active violet arc represents observed work in the space, **not** all
delegated/background work. This is status telemetry, not a visualization of
hidden reasoning. Outside a space the swarm keeps its original flocking
behavior. Controller attraction still works. While in a space, the text console
sits lower so it doesn't obscure the swarm.

### Voice

Speech recognition and spoken replies are moving from the PC bridge to the
server (see [design/speech.md](design/speech.md)). Until they arrive, **X** shows
a notice instead of recording, and replies are not spoken.

## PC spectator and live audio (prototype)

The headset doesn't publish its scene or audio for now; this section describes
the PC side, which shows “Waiting for Quest” until publishing returns through
the server.

The Quest remains the simulation authority for now. It sends its head/view,
controller, agent positions, chat panel, and recording indicator to the PC bridge
at up to **20 updates per second**. The desktop renders a read-only, headset-
following mono view; it is a separate render, not a pixel-perfect Quest cast.
It shows “Waiting for Quest” after updates stop. No OBS or YouTube stream starts
automatically.

Both tracked controller grip poses are rendered using the bundled, MIT-licensed
WebXR Quest Touch Plus models (see `crates/client-quest/assets/controllers/`). If the Quest runtime
exposes `XR_FB_render_model`, the headset attempts to load its models on a
background thread and falls back to the bundled meshes if loading fails. The PC
spectator currently uses the bundled meshes rather than copying runtime assets.
Controller textures are supported for embedded PNG/JPEG and Basis Universal
KTX2; button animation is not yet implemented. When `XR_EXT_hand_tracking` is
available and hand tracking is enabled on the Quest, tracked hands are shown
as a simple joint-and-bone skeleton rather than a skinned hand mesh. Hand
gestures do not replace the existing controller bindings. The local bridge
forwards controller poses and tracked joint positions to the spectator at the
same up-to-20-Hz scene-state rate; it does not persist them.

```sh
bash scripts/opencode-bridge.sh       # keep running; requires USB/adb reverse
cargo run -p client-quest -- --spectator # PC window; capture this in OBS
```

The spectator requests a **1920×1080 physical-pixel** content area at startup,
with a 1080p minimum and no window decorations, to match a 1080p OBS canvas.
Press **Esc** to close it. Resizing larger changes the render resolution. On a
1080-pixel-high display the desktop panel can cover part of the borderless
window; the compositor may also constrain sizes on smaller or scaled displays.
Check the actual size in the startup log and OBS preview.

In OBS, add the spectator window as a window/screen capture. Add a **Media
Source** for `http://127.0.0.1:8765/spectator/audio.wav`: uncheck “Local File”,
use the URL as the input, and specify `wav` as the input format if required.
The live 16 kHz mono stream mixes the headset microphone and assistant speech;
it emits silence when neither is active. Avoid capturing the same audio a second
time through desktop audio. Preview and adjust OBS audio/video sync before going
live. The first stream connection may buffer; reconnect the Media Source after
restarting the bridge.

### Experimental direct PipeWire video (Linux)

Instead of capturing a window, run `cargo run -p client-quest -- --spectator-pipewire`. It
publishes `Tomori-Spectator` (`tomori-spectator`) **without
creating a window or display surface**. Press **Ctrl-C** to stop it. For an
optional window showing the same direct output, use
`cargo run -p client-quest -- --spectator-pipewire-preview` instead; Escape closes that window
and stops its output. Run only one direct-output spectator at a time.

Both modes render to a fixed **3840×2160 RGBA** texture at up to 30 fps. The
optional preview window requests 1920×1080 and scales the 4K image; the
`--spectator` window-capture fallback still renders at its actual window size.
The Quest still provides scene updates at
up to 20 Hz. Audio continues to use the bridge URLs above; video alone is sent
through PipeWire. Any local client with PipeWire access may be able to view it.

This prototype requires `gst-launch-1.0`, GStreamer's `fdsrc` and
`rawvideoparse` elements, and the PipeWire GStreamer plugin. It performs an
asynchronous GPU-to-CPU readback and copies through a GStreamer subprocess; it is
**not zero-copy**. Readback and the outgoing queue are bounded, so frames can be
dropped under load. A slow or absent consumer may briefly see older queued frames
when it reconnects. If GStreamer fails, the headless process exits with an error;
in preview mode the window stays open but the video source stops. Restart the
spectator to retry.
Each uncompressed 4K frame is about 31.6 MiB, so readback and memory-copy costs
are significantly higher than at 1080p. A sustained 30 fps is not guaranteed;
check performance and latency in the real OBS preview.

The built-in OBS PipeWire screen capture is not the same as a generic PipeWire
video input. The [ioquatix fork of `obs-pwvideo`](https://github.com/ioquatix/obs-pwvideo/tree/discover-video-source-nodes)
adds an opt-in **Show video devices and other sources** checkbox to the PipeWire
Video source; enable it to select **Tomori-Spectator**. The change is
under review in [upstream PR #8](https://github.com/hoshinolina/obs-pwvideo/pull/8).
To build and install the fork as a Pacman-managed package on Arch:

```sh
cd packaging/obs-pwvideo-ioquatix-git
makepkg -si
```

The fork package conflicts with and replaces the standard `obs-pwvideo` package;
don't install both. Review the PKGBUILD before building. OBS must be restarted
to load the installed plugin. Rebuild this local VCS package to update the fork;
it is not itself an AUR package.

Alternatively, the [AUR `obs-pwvideo` package](https://aur.archlinux.org/packages/obs-pwvideo)
works with this stream **when the OBS source already has its target saved** as
`tomori-spectator`. The plugin does not discover `Video/Source` nodes
in its picker, although it can connect to a saved target of that class. Avoid
changing the source's target to “No autoconnect”, which clears it. For a new
source, either manually link the PipeWire ports (`pw-link` or a graph editor),
or, with OBS closed and the scene collection backed up, set the source's
`settings.target` to `tomori-spectator` in its scene collection JSON.
Check that the `id` is `pipewire-video-source` and preserve all other settings.
An isolated libobs test with the unmodified AUR package and a saved target
received frames from the earlier 1080p spectator; check the real OBS preview too.

Alternatively, the supplied compatibility patch makes the spectator selectable
in the plugin's picker. To build that patched plugin in a new directory (requires
OBS, PipeWire and DRM development headers, CMake and Ninja):

```sh
git clone https://github.com/hoshinolina/obs-pwvideo.git /tmp/opencode/obs-pwvideo-build
git -C /tmp/opencode/obs-pwvideo-build checkout --detach ba64bf322ad450f8fe8c43236878e83669fb677d
git -C /tmp/opencode/obs-pwvideo-build apply "$PWD/patches/obs-pwvideo-spectator.patch"
cmake -S /tmp/opencode/obs-pwvideo-build -B /tmp/opencode/obs-pwvideo-build/build -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
cmake --build /tmp/opencode/obs-pwvideo-build/build
```

Run these commands from the project directory; the clone directory must not
already exist. The plugin is **not installed automatically**. When ready to
change OBS, follow [the plugin's installation instructions](https://github.com/hoshinolina/obs-pwvideo#how-to-build-from-source), using the patched
build and its `data` directory. Check for an existing installation before
copying anything. Do not install both patched and AUR versions at once. OBS must
be restarted to load a different plugin; do not restart during a stream. Verify
the live preview, performance and audio/video sync before going live. Keep
`--spectator` and window capture as the fallback.

For separate OBS volume controls, use `/spectator/microphone.wav` and
`/spectator/agent.wav` as two Media Sources instead of the mixed `audio.wav`.
All three are live 16 kHz mono streams on the same loopback bridge; don't add
both the mixed stream and the individual streams or the sound will be doubled.

### Experimental PipeWire/PulseAudio inputs (Linux)

As an alternative to the OBS Media Sources, keep the bridge running and start
the audio helper in a separate terminal:

```sh
node bridge/pipewire-audio.mjs
```

It exposes **Tomori-Quest-Microphone** and **Tomori-Assistant-Voice** as
two audio inputs. In OBS, add an **Audio Input Capture (PulseAudio)** source for
each and choose the matching device. Start the helper before adding or selecting
the OBS sources; leave the existing OBS scene unchanged until you are ready to
test. Do not simultaneously add the WAV Media Sources for the same audio.

The helper subscribes to the bridge's existing local HTTP/WAV feeds and publishes
them through PipeWire's PulseAudio compatibility layer; the headset-to-PC
transport is unchanged. It creates two dedicated silent output sinks and two
remapped inputs in the current user session, without deliberately changing the
default devices or sending audio to the speakers. The internal sinks and their
monitor sources can also appear in device lists. Only one helper can run at a
time. It requires `pactl`, `gst-launch-1.0`, GStreamer's `rawaudioparse`, `queue`
and `pulsesink` elements, and a PulseAudio-compatible server supporting
`module-null-sink` and `module-remap-source`.

When the bridge disconnects, the helper emits silence and retries; it can stay
running across a bridge restart. **The OBS PulseAudio input did not reconnect
automatically after the helper itself was stopped and restarted** in an isolated
test. In that case, mute the affected OBS sources and reselect their devices or
recreate the sources when safe. Ctrl-C or SIGTERM stops the helper and removes
the virtual devices; an uncatchable termination or Pulse server failure can leave
modules behind. If the helper reports an existing device, inspect `pactl list
short modules` and unload only the matching stale `module-null-sink` and
`module-remap-source` IDs after confirming they belong to this helper.

The original audio remains 16 kHz mono even if PipeWire exposes the remapped
inputs at 48 kHz. Small buffers mean late audio may be dropped, and the pipeline
can retain a short tail after muting; verify the headset's `MIC OFF` state and
audio/video sync before broadcasting.

## Build and sideload (Linux)

Enable Quest developer mode, connect a USB data cable, and accept **Allow USB
debugging** inside the headset. `adb devices -l` must show `device`, not
`unauthorized`. Keep your boundary enabled and stay seated or stationary.
Wake the Touch controllers before launching: Quest may otherwise show a
**controllers required** dialog and block the application from starting.

Install a **user-local rustup** toolchain (Rust 1.93+) and Android tools:

```sh
rustup target add aarch64-linux-android
cargo install cargo-apk --version 0.10.0 --locked
```

With Android SDK command-line tools installed, install these SDK components
(review/accept Google's licenses when prompted):

```sh
sdkmanager 'platform-tools' 'platforms;android-32' 'build-tools;35.0.0' 'ndk;29.0.14206865'
export ANDROID_HOME="$HOME/Android/Sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/29.0.14206865"
bash scripts/android.sh run
```

The script downloads the official Khronos **1.1.65** OpenXR loader AAR from Maven
Central, checks its published SHA-256, and packages the arm64 loader in the APK.
The Quest APK does not contain a transcription model.
`cargo-apk` creates a local debug signing key if needed. Build output:
`target/debug/apk/tomori.apk` (package `io.github.ioquatix.tomori`).
Launch from **Library → Unknown Sources → Tomori** if not auto-started.

```sh
bash scripts/android.sh devices
bash scripts/android.sh logs
bash scripts/android.sh build  # build without deploying
```

You can save machine-local exports in `.android-env` (gitignored). The SDK,
NDK, JDK (`java`/`keytool`), `cargo-apk`, and ADB must be available. Debug builds
use optimization level 1; the example is for local sideloading, not store submission.
This machine uses isolated tools in the ignored `.tools/` directory, referenced
by `.android-env`; system Rust is not modified.

## Desktop window

```sh
cargo run -p client-quest
```

Runs the same simulation and wgpu renderer in a resizable desktop window;
**no headset or OpenXR runtime needed**. Uses Vulkan on Linux (Wayland or X11).

- **Hold left mouse:** attract agents to the cursor on a plane through the swarm.
- **Right-drag:** orbit the camera; **mouse wheel:** zoom.
- **R:** reset agents; **Space:** pause/resume; **Esc:** quit.

The window doesn't connect to a server. To use a server without a headset or a
GPU, show the headset's panel in the terminal:

```sh
cargo run -p client-quest -- --console [--server HOST:PORT] [--name NAME]
```

Type the controller buttons as commands, such as `a`, `b`, `y`, `u` and `d`
for the left stick, and `<` `>` for the right stick. `--server` (or
`TOMORI_SERVER`) and `--name` also apply to `--xr`.

## Tests / desktop OpenXR

```sh
cargo test --workspace                   # simulation, projection, desktop camera, server and client tests
cargo test -p client-quest graphics::tests -- --ignored  # offscreen stereo captions / mono rendering
cargo run -p client-quest -- --xr        # use an active desktop OpenXR Vulkan runtime
npm test --prefix bridge          # session routing, questions and permissions
```

For the PC streaming-recognition test (model + public sample required):

```sh
bash scripts/setup-pc-transcription.sh
mkdir -p target/downloads
curl -fL https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav -o target/downloads/jfk.wav
(cd bridge && node --test asr.test.mjs)
```

It checks partial results, finalization, out-of-order rejection, and a recording
longer than a minute.

A USB-connected Quest does **not** provide a Linux desktop OpenXR runtime:
build the Android APK for standalone Quest use. Desktop execution requires its
own OpenXR loader/runtime (for example Monado).

## Layout / interop

- `crates/client-quest/src/desktop.rs`: desktop window, orbit camera, mouse attraction.
- `crates/client-quest/src/xr.rs`: Android/desktop OpenXR lifecycle, predicted views, controller actions.
- `crates/client-quest/src/vulkan.rs`: XR-created Vulkan instance/device imported into wgpu; borrowed
  two-layer XR swapchain images, one layer per eye.
- `crates/client-quest/src/graphics.rs`, `crates/client-quest/src/swarm.wgsl`: instanced cube rendering with per-eye depth.
- `crates/client-quest/src/swarm.rs`: deterministic CPU flocking, separation/alignment/cohesion.
- `bridge/asr.mjs`, `bridge/asr_worker.py`: GPU/CPU Whisper rolling transcription.
- `crates/client-quest/src/caption.rs`, `crates/client-quest/src/caption.wgsl`: head-relative, stereo text panel.
- `crates/client-quest/src/conversation.rs`: server list, space list and space panel, driven by server events.
- `crates/client-quest/src/connection.rs`: the server connection and Bonjour browsing.
- `bridge/`: official OpenCode V2 client adapter, no longer used by the headset.

The Vulkan device uses the runtime-selected physical GPU and enables the full
extension/feature set required by wgpu. Swapchain images remain runtime-owned;
wgpu has no permission to destroy them. Each frame waits for submitted GPU work
before releasing the XR image, leaving it in `COLOR_ATTACHMENT_OPTIMAL`. This
simple conservative synchronization favors correctness over maximum throughput.
Tracking loss and non-rendering frames submit no layer; sessions handle
ready/stopping/exit events. The scene initially faces the headset; the app uses
`LOCAL_FLOOR` when available so the Quest's system Meta-button recenter moves the
scene, falling back to `LOCAL` with an approximate 1.6 m floor offset. Reference
space changes take effect at the runtime's specified frame time. Recentering
doesn't reset the agents, conversation, or interaction state. This is opaque VR, not
passthrough; optional tracked-hand skeletons are described above.

## Third-Party Components

Original project code is released under the MIT License; see [license.md](license.md).
Third-party components retain their own licenses:

- Bundled controller assets: [MIT](crates/client-quest/assets/controllers/LICENSE.md).
- Basis Universal notices and test fixtures:
  [Apache-2.0](crates/client-quest/assets/licenses/basisu-LICENSE.txt), with fixture provenance in
  [crates/client-quest/tests/fixtures/README.md](crates/client-quest/tests/fixtures/README.md).
- The optional OBS plugin compatibility patch follows the upstream plugin's
  GPL-2.0-or-later licensing; its packaging metadata is under `packaging/`.
- Speech models and native dependencies are downloaded separately, not bundled
  in this repository. Their licensing requirements are noted in the relevant
  setup sections above.

## Releases

<!-- bake-readme:releases:start -->
See [releases.md](releases.md) for the full release history.
<!-- bake-readme:releases:end -->

## See Also

- [tomori](https://github.com/ioquatix/tomori) — A conversational spatial workspace for people and agents, built with OpenXR and wgpu<!-- bake-readme:package -->
