# Tomori

A conversational spatial workspace for people and agents, currently a native
**Rust + OpenXR + wgpu/Vulkan** prototype for **Meta Quest 3**.
128 agents inhabit a selectable moonlit Sky environment or minimal floor grid.
Real stereo views use the headset's predicted poses and asymmetric eye projections. Voice-to-text
and speech synthesis run locally on the PC; the Quest captures audio and renders
the interactive scene. A PC bridge is required for voice and OpenCode chat.

Tomori is the name of both the system and its conversational coordinator; see
[naming.md](naming.md). The Rust package and executable are named `tomori`.
The current prototype is single-user and USB/loopback-connected;
multi-user workspaces and Bonjour discovery are future directions, not
implemented features. Sky is an early environment blockout, not finished artwork.
Private workspace memory and session state are not included in this public
source repository.

- **Right trigger:** attract the swarm to the right controller (orange cube).
- **A:** open a session / allow a pending tool once; otherwise reset the swarm.
- **Hold X (left controller):** see a live transcription draft; release to send.
- **Squeeze the left grip:** toggle your live audience microphone.
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
  `TOMORI_ENVIRONMENT=minimal cargo run` for the original grid.
- **Spectator:** follows the headset's selection; older headset builds without
  an environment field retain Minimal. Update the PC bridge for new senders.

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
TOMORI_SKY_PREVIEW=/tmp/opencode/tomori-sky.png cargo test --locked \
  sky_environment_renders_stereo_switches_and_preserves_recenter -- --ignored
```

## Chat with existing OpenCode sessions in VR

Requires **OpenCode V2** on the PC (tested with 2.1.4), Node.js, npm, Python,
and the USB-connected Quest. Install the PC transcription model once, then
start the small loopback bridge in another terminal:

```sh
bash scripts/setup-pc-transcription.sh  # installs PC venv and model
bash scripts/opencode-bridge.sh
```

It installs the pinned official `@opencode/client` package if necessary,
discovers/authenticates with the same background service as the OpenCode UI,
and forwards its own loopback port 8765 over USB with `adb reverse`. It does not
create a new conversation or replace existing agents/models/project settings.
Leave this terminal running while chatting. After reconnecting USB, run the
script again (or `adb reverse tcp:8765 tcp:8765` if the bridge is still running).

### Controls

- **Session browser:** flick left stick up/down to choose among the 40 most
  recently updated, unarchived root sessions; **A** opens the selection.
- **Hold X, speak, release:** see a changing live transcript, then send the
  finalized text to the open session. No extra confirmation. If the agent is
  busy, OpenCode queues the message.
- **Click the left thumbstick:** toggle spoken replies; the panel shows
  `[SOUND]` or `[MUTED]`. Holding X interrupts ongoing speech before recording.
  Leaving the conversation or losing XR focus also stops playback.
- **Click the right thumbstick:** replay the selected assistant message (or the
  latest assistant reply if a different message is selected).
- **Flick the right thumbstick left/right:** select a coordinator-published
  result card; **up/down:** page through that card's contents (wrapping at either
  end). Release the stick between flicks. Clicking it still replays speech.
- **Conversation:** flick left stick up/down to page through text; left/right
  to browse recent messages. The panel shows the latest 24 timeline entries,
  omitting internal reasoning and compressing tool-only messages to status lines.
  Replies update progressively by polling about every 700 ms, not an SSE stream.
- **Pending tool permission:** **A** allows once, **B** denies. Nothing is
  auto-approved, and persistent permissions are never changed.
- **Pending question:** hold X and say the answer or numbered option. Ordinary
  string, boolean, numeric, and multiselect fields are supported, including
  sequential multi-field forms. Conditional/external forms must be completed
  in the regular OpenCode UI. Say multiple selections separated by “and”.
- **B:** return to sessions; from sessions, switch to the standalone voice demo.
- **Y:** return to/refresh the session browser.

Speech is bound to the selected session/question **when recording starts**,
so changing sessions while Whisper is processing cannot send it somewhere else.
Network sends are not automatically retried: if delivery is uncertain, check
the session in OpenCode before repeating it. Existing OpenCode permissions still
apply; selected agents may edit files or run tools just as in the regular UI.

The bridge listens on **127.0.0.1 only**, exposes no model credentials to the
headset, and has no generic filesystem/shell/session-deletion endpoints.
Only speech recognition is local/offline: transcripts and replies are exchanged
with OpenCode and its configured model provider. The APK now requests INTERNET
and permits cleartext HTTP for this USB loopback demo, not a production network setup.
The standalone voice-preview mode (B from session browser) also requires the PC
bridge, but does not send its transcript to an OpenCode session.

### Delegated workers and result cards

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

Open a session to give the swarm an observable conversational state:

| Appearance | Meaning |
| --- | --- |
| Tight violet circulation, rotating broken halo | Selected session is working |
| Lavender circulation, rotating broken halo | Speech queued/synthesizing, or settling before ready |
| Cyan ripples and cube-size changes | Headset speech playback; motion follows the written audio level |
| Open, calm mint swarm with a steady complete halo | Selected session idle and speech queue drained |
| Amber, gently pulsing broken halo | A permission or question needs your input |
| Wide cyan swarm | Your push-to-talk recording is active |
| Blue circulation | Finalizing your recording/transcript |
| Dim fragmented halo / red fragmented halo | Status unknown or stale / an interaction error |

The ready halo persists; you don't need to catch an end-of-turn flash. Speech
preparation stays distinct from ready even between chunks. Holding X still
interrupts spoken playback; it does not cancel agent work, and a busy session
may queue your message. An active violet arc during playback represents observed
work in the selected session, **not** all delegated/background work. This is
status telemetry, not a visualization of hidden reasoning. The standalone demo
retains its original flocking behavior. Controller attraction still works.

The same state and audio envelope accompany the Quest scene to updated PC
spectators. Older senders keep the original visual style. OBS audio is separate
and may be delayed relative to these cues. While a session is open, the text
console sits lower so it doesn't obscure the swarm; all its controls remain.

### Spoken replies (PC-side Kokoro)

Install the PC voice model once (~350 MB download):

```sh
bash scripts/fetch-tts-model.sh
```

Restart the bridge after updating dependencies (`npm ci --prefix bridge`),
then run `bash scripts/opencode-bridge.sh`. The bridge lazily starts a separate
CPU worker using Kokoro v1.0 via sherpa-onnx. Its audio goes over the
USB loopback connection to the Quest's speakers. The model stays on the PC;
no speech synthesis cloud service is used. The default is `jf_alpha` (ID 37),
a Japanese-trained voice speaking English via the English text frontend. This
is an experiment, not a clone of any specific voice; English pronunciation may
be imperfect. Change the voice ID (0–53) by setting `TOMORI_TTS_VOICE` when
starting the bridge. Other Japanese-trained voices include 38, 39, and 40;
the prior Michael voice is 16 in this model.

Only **new completed assistant text** is spoken; old session history, internal
reasoning, tool output, and fenced code blocks are not read aloud. Long replies
are sent as short chunks after each assistant message finishes, not as live
word-by-word generation. Speech can lag behind text; the headset can keep
rendering and chatting while synthesis runs. Starting a recording interrupts
speech already playing; new replies completed while X is held wait until X is
released. Muting or losing focus discards queued audio. If the speech worker
is missing or unavailable, the text chat remains usable and shows an error.

Licensing: Kokoro model weights are Apache-2.0; this sherpa-onnx release uses
eSpeak NG for phonemization (GPL-3.0-or-later). Review the model bundle and
dependency licenses before distributing the PC bridge.

## PC spectator and live audio (prototype)

The Quest remains the simulation authority for now. It sends its head/view,
controller, agent positions, chat panel, and recording indicator to the PC bridge
at up to **20 updates per second**. The desktop renders a read-only, headset-
following mono view; it is a separate render, not a pixel-perfect Quest cast.
It shows “Waiting for Quest” after updates stop. No OBS or YouTube stream starts
automatically.

Both tracked controller grip poses are rendered using the bundled, MIT-licensed
WebXR Quest Touch Plus models (see `assets/controllers/`). If the Quest runtime
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
cargo run -- --spectator              # PC window; capture this in OBS
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

Instead of capturing a window, run `cargo run -- --spectator-pipewire`. It
publishes `Tomori-Spectator` (`tomori-spectator`) **without
creating a window or display surface**. Press **Ctrl-C** to stop it. For an
optional window showing the same direct output, use
`cargo run -- --spectator-pipewire-preview` instead; Escape closes that window
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

**The audience microphone starts OFF.** Squeeze the **left grip** to toggle it.
The panel shows `MIC OFF`, `MIC LINKING`, or `MIC LIVE`; press the grip again to
stop. It also turns off when the app loses focus or exits. When enabled, audio
is sent continuously to the PC regardless of whether X is held. X remains the
separate push-to-talk control for the OpenCode prompt, sharing the microphone
with the live stream. The stream may briefly pause while the microphone switches
between continuous and prompt recording. No audio is stored by the app/bridge.
Headphones are recommended: the headset speakers can leak assistant speech back
into the microphone. Video can include chat and other sensitive content; check
your OBS preview before broadcasting. The prototype uses 16 kHz mono (the speech
recognizer's format), not high-fidelity audio, and drops late packets to limit
latency rather than accumulating a backlog.

## PC live voice-to-text

Press **B** from the session browser to enter standalone voice-preview mode.

1. Wait for the floating panel to say **Ready**.
2. Press **X** and allow microphone access in the system permission prompt.
3. Hold **X**. Once `REC ... LIVE ON PC` appears, speak English. The headset
   sends small audio chunks to the PC; the panel shows the most recent part of
   the revisable draft. The header identifies the session bound at button-down.
4. Release **X** to finalize and send the entire transcript to that session.
   There is no 60-second recording cutoff. A timer shows elapsed time, not a
   false progress bar. Partial results may change as more speech arrives.

Recognition uses **faster-whisper `small.en`** on the PC (CUDA on this machine;
CPU fallback when CUDA is unavailable), in a background worker with a bounded
rolling audio window. Long prompts keep their earlier finalized words without
buffering all the audio. The live draft is refreshed approximately every two
seconds of audio. The initial model load may take longer; wait for `REC` before
speaking. The model and CUDA runtime remain on the PC; no external transcription
service is used. Override `TOMORI_ASR_DEVICE=cpu` on bridge startup if needed.

Microphone capture is Android AAudio, 16 kHz mono. Push-to-talk audio is sent to
the PC for recognition **even when the audience microphone is off**; the left
grip separately controls what goes to OBS. Losing focus or quitting cancels the
recording, and a missing/out-of-order audio chunk or saturated connection fails
the utterance rather than silently sending a partial prompt. Audio is not saved
by the app or bridge; OpenCode records the final sent text in its normal session
history. The prompt API has a large text-size guard and the chosen model has a
finite context, so extraordinarily long prompts still have practical limits.

If microphone access was denied, enable it in Quest's app permissions and try
X again. There is no automatic permission grant or background listening.

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
The Quest APK does not contain a transcription model. PC transcription setup
fetches MIT-licensed `Systran/faster-whisper-small.en` at a pinned revision and
verifies the model SHA-256. The prior Quest model files are left untouched.
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
cargo run
```

Runs the same simulation and wgpu renderer in a resizable desktop window;
**no headset or OpenXR runtime needed**. Uses Vulkan on Linux (Wayland or X11).

- **Hold left mouse:** attract agents to the cursor on a plane through the swarm.
- **Right-drag:** orbit the camera; **mouse wheel:** zoom.
- **R:** reset agents; **Space:** pause/resume; **Esc:** quit.

## Tests / desktop OpenXR

```sh
cargo test                       # simulation, projection, desktop camera tests
cargo test graphics::tests -- --ignored  # offscreen stereo captions / mono rendering
cargo run -- --xr                 # use an active desktop OpenXR Vulkan runtime
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
longer than a minute. The legacy local-Quest Whisper model comparison tests
remain optional and require the old `android/assets/` and `android/voice-assets/` models.

A USB-connected Quest does **not** provide a Linux desktop OpenXR runtime:
build the Android APK for standalone Quest use. Desktop execution requires its
own OpenXR loader/runtime (for example Monado).

## Layout / interop

- `src/desktop.rs`: desktop window, orbit camera, mouse attraction.
- `src/xr.rs`: Android/desktop OpenXR lifecycle, predicted views, controller actions.
- `src/vulkan.rs`: XR-created Vulkan instance/device imported into wgpu; borrowed
  two-layer XR swapchain images, one layer per eye.
- `src/graphics.rs`, `src/swarm.wgsl`: instanced cube rendering with per-eye depth.
- `src/swarm.rs`: deterministic CPU flocking, separation/alignment/cohesion.
- `src/voice.rs`, `src/speech.rs`: Android permission/capture and ordered PC audio streaming.
- `bridge/asr.mjs`, `bridge/asr_worker.py`: GPU/CPU Whisper rolling transcription.
- `src/caption.rs`, `src/caption.wgsl`: head-relative, stereo text panel.
- `src/chat.rs`: session browser, reply paging, background loopback HTTP client.
- `bridge/`: official OpenCode V2 client adapter (no alternate AI backend).

An ndk 0.9 workaround is covered by regression tests: positive AAudio read/write
counts are incorrectly reported as errors, so the app normalizes those counts.
The legacy local-Whisper regression tests also cover its abort-closure workaround.

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
doesn't reset the agents, chat, or interaction state. This is opaque VR, not
passthrough; optional tracked-hand skeletons are described above.

## Third-Party Components

Original project code is released under the MIT License; see `license.md`.
Third-party components retain their own licenses:

- Bundled controller assets: [MIT](assets/controllers/LICENSE.md).
- Basis Universal notices and test fixtures:
  [Apache-2.0](assets/licenses/basisu-LICENSE.txt), with fixture provenance in
  [tests/fixtures/README.md](tests/fixtures/README.md).
- The optional OBS plugin compatibility patch follows the upstream plugin's
  GPL-2.0-or-later licensing; its packaging metadata is under `packaging/`.
- Speech models and native dependencies are downloaded separately, not bundled
  in this repository. Their licensing requirements are noted in the relevant
  setup sections above.
