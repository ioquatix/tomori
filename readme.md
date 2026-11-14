# Tomori

A conversational spatial workspace for people and agents, currently a native **Rust + OpenXR + wgpu/Vulkan** prototype for **Meta Quest 3**. 128 agents inhabit a moonlit Sky environment or a minimal floor grid. Real stereo views use the headset's predicted poses and asymmetric eye projections. The Quest renders the interactive scene and joins conversations through a Tomori server on the PC, which runs the agent sessions.

Tomori is the name of both the system and its conversational coordinator; see [docs/naming.md](docs/naming.md). The headset and desktop application is the Rust package `tomori-client-quest`, whose library and executable are named `tomori`, and the server is `tomori-server`, from the package `tomori-server-app`; the other crates are listed under [Crates](#crates). The headset finds servers with Bonjour or over USB; several participants can share a space. Hold **X** to talk to the space's agent: the server transcribes what you say and queues it, and the agent's replies are spoken. Sky is an early environment blockout, not finished artwork. Private workspace memory and session state are not included in this public source repository.

- **Right trigger:** choose what the right controller's ray points at; outside a space, a pull that starts away from every object attracts the swarm to the right controller. In a space it attracts nothing and leaves the agent where it is: call the agent by voice, "come here", or with **Call Tomori here** in the hand menu (see [design/interaction.md](design/interaction.md)).
- **A:** connect, join a space, or confirm an approval; outside a space, otherwise reset the swarm.
- **Hold Meta button:** Quest's system recenter brings the swarm in front of you. A short press opens the system menu. No artificial camera movement.

## Environments

Each space is bound to one environment, chosen when it is created and the same for everyone in it; an owner can change it later, for everyone, from the hand menu's **Space** section (see [design/environments.md](design/environments.md)). The server offers **Sky**, the default for a new space: a fixed moonlit night, sparse stars, static cloud banks, a stable platform and an open shrine blockout with warm lanterns. Sky's renderer lives in [`crates/environment-sky-client`](crates/environment-sky-client/readme.md), separate from sessions, worker simulation and application input, and its identity in [`crates/environment-sky`](crates/environment-sky/readme.md). It also offers **Grid**, the plain floor grid of the **Minimal** environment, which is also the waiting area outside any space, and the **Farmyard**, an evening yard with a red chicken coop, a water trough, a feeder and hay, where a small flock of chickens wanders, pecks and roosts, clucks, and looks up puzzled when you point at one, as everyone in the space sees; you can walk about it by teleporting, peek into the coop's window, and ask the agent about the chicken you point at; it lives in its own crates, [`crates/environment-farmyard`](crates/environment-farmyard/readme.md) and its client half, which Tomori's own programs register as another project's could (see [design/farmyard.md](design/farmyard.md)). Tomori's core crates know no environment but the grid: Tomori's own server and app register Sky and the others from their own crates, as another project's would ([design/environments.md](design/environments.md) describes how to make one).

- **Quest / desktop XR:** choosing a server and a space happens on the grid; joining a space switches to its environment, and leaving returns to the grid. Inside a space, only an owner's change for the whole space switches it. A space whose environment this build doesn't know shows the grid, and its panel names the environment.
- **Desktop window:** it has no server, so press **E** to switch between Sky, the farmyard and the grid (and any scenery a headset build of its own registers; see [examples/headset](examples/headset/readme.md)) (with `--desktop` and a server, **E** does nothing: the space chooses); or launch with `TOMORI_ENVIRONMENT=minimal cargo run -p tomori-client-quest` to start on the grid.
- **Spectator:** shows the environment of the space it watches, as everyone in it does, and follows an owner's change to it.

### GitHub (prototype)

A first experiment of the GitHub environment proposed in [docs/proposals/github-and-code-environments.md](docs/proposals/github-and-code-environments.md): start the server with `--github-repository OWNER/NAME`, and an owner can change a space to **GitHub** from the hand menu's **Space** section. Its cards, the repository's open issues and pull requests, start in an inbox front left of you. Point at one with the right ray to highlight it, squeeze the right grip to pick it up and carry it along the ray, and let go to put it down, or over the inbox to put it back. Everyone in the space sees where cards are put, and the server keeps that with the space. Point at a card as you press **X** to ask the agent about it. The server reads GitHub, read-only, with the token in `GITHUB_TOKEN` or else from `gh auth token`, and never sends it to the headset or the agent:

```sh
cargo run -p tomori-server-app --bin tomori-server -- --backend opencode --github-repository base-rs/tomori
```

Nothing is changed on GitHub, and only participants see the cards; see [design/github.md](design/github.md).

### Projection walls

When something is better read than heard, such as a list, steps or a summary, the agent projects it as a text panel, which everyone in the space sees, by writing a fenced block of language `panel` in its reply:

````text
```panel
title: Build status
source: CI
---
**3 crates** failed.
```
````

The panel goes onto the wall whoever asked was looking at as they began speaking, if it is within 4 m, so that they can read it, or else the nearest in their view, the nearest, or, with none within 4 m, floating in front of them; and otherwise, when nobody asked, onto the **Tomori wall**, which every space has beside the swarm, and which keeps its panels when the space changes environment. The farmyard has a noticeboard of its own, and an owner can add a wall where they stand, or remove the nearest one they added, from the hand menu's **Space** section. Point at a panel to see its label; squeeze the right grip to pull it off and carry it, and let go to leave it floating, or near a wall to snap it back; while you carry it, a hint below it says what each control does, and pressing **B** throws it away for everyone, which B on its own never does; long lines in code panels wrap under a hanging indent; and point at it as you press **X** to ask the agent about it. The agent can also say where a panel goes, with a `place:` header line such as `place: near Ada`, `place: looking Ada`, `place: front` or `place: wall Tomori wall`, or after a code block's language, as `rust place=near:Ada`, and move any panel with `tomori_move_panel` when you ask, even one you put where it is, which stays yours there, with a notice on the Tomori wall saying what moved, though never one you are carrying. Panels are not read aloud, and stay in the transcript; the server keeps walls and panels with the space across restarts, and those of an environment with that environment. See [design/projection.md](design/projection.md); the agent shows images, such as renders, diagrams and screenshots, as image panels, which fit their slot and grow to their own shape when pulled off; to see one rendered, set `TOMORI_IMAGE_PANEL_PREVIEW` to a PNG path and run `cargo test --locked -p tomori-render an_image_panel`; to see a wall rendered, set `TOMORI_PROJECTION_PREVIEW` to a PNG path and run `cargo test --locked -p tomori-render a_projection_wall_and_its_panels_render_over_the_scene`.

Switching does not reset agents, conversations or worker positions. Sky uses the same scene/recenter transform as the workspace; it does not move the camera. Clouds use a bounded low-resolution volumetric density pass with translucent edges and approximate moonlighting; lanterns have brighter emissive faces and cast warm, occluded illumination onto the nearby static scenery through a build-time lightmap, not runtime point-light shadow maps. The same lightmap adds local ambient occlusion at contacts. The shrine and platform cast/receive softly filtered moon shadows from a cached static shadow map. Agent/status visuals and labels are not shadowed. There is no day/night cycle, scenery collision or new locomotion system.

The scene has been exercised on Quest 3 and tested offscreen in Vulkan; the on-device frame-time budget still needs measurement. The stereo/switch/recenter regression runs with the other tests (see [Tests](#tests--desktop-openxr)); to save a preview sheet:

```sh
TOMORI_SKY_PREVIEW=/tmp/opencode/tomori-sky.png cargo test --locked -p tomori-render \
  sky_environment_renders_stereo_switches_and_preserves_recenter
```

## Join a space on a Tomori server

Start the server on the PC with the OpenCode backend (see [design/server.md](design/server.md)), which drives a running `opencode serve`: it finds one started with `opencode serve --service`, or the one `--opencode-url` or `OPENCODE_URL` names. OpenCode can use Claude models, so there is no separate Claude Code backend. Spaces may run in the directory the server starts in or beneath it; give `--workspace-root PATH`, repeated as needed, to allow other directories instead. The server links the sherpa-onnx speech libraries, so fetch and verify them once before the first build:

```sh
bash scripts/fetch-sherpa-onnx.sh
opencode serve --service &
cargo run -p tomori-server-app --bin tomori-server -- --backend opencode
```

Anyone who can reach the server can watch its spaces as a visitor. Creating spaces, talking to the agent and approving its tools need a token (see [design/security.md](design/security.md)). The easiest way to get one is to pair: choose the gold key beside the connected server in the headset, and it shows a 6-digit code. Type that code on the server's console to confirm, or compare it with the code on the status page and confirm there, and the headset then keeps an owner token for that server. The server never writes the code to its console or log, and a wrong code typed declines the pairing, so pair again. A server started with `--no-visitors` still admits a device that asks to pair.

You can also make an owner token on the PC, and give it to the headset or the desktop client once; each keeps it for that server:

```sh
cargo run -p tomori-server-app --bin tomori-server -- auth mint --owner --name "Sam's Quest"
TOMORI_TOKEN=tomori_… bash scripts/android.sh run     # or: tomori --xr --server HOST:8765 --token tomori_…
```

A token given this way goes only to the server given with `TOMORI_SERVER` or `--server`, or over USB, never to a server found on the network.

`tomori-server auth list` lists the tokens, `auth rename ID NAME` renames one, and with it everyone using it, and `auth revoke ID` revokes one. The server keeps its tokens and TLS certificate in `~/.local/state/tomori` (`--state-directory` changes it), serves only TLS, and shows owners its status page at `https://<server>:8765/`, where they sign in with a token. Pass `--no-visitors` to refuse anyone without a token, or `--auth open` to let anyone in as an owner, as before.

The headset creates spaces with its **New space** object (see [Controls](#controls)), shown to owners. To create one from the PC instead, use the example client with an owner token: give it the server's address, then type `create` with the backend and a directory beneath a workspace root.

```sh
TOMORI_TOKEN=tomori_… cargo run -p tomori-client --example connect -- 127.0.0.1:8765
create opencode /home/sam/project
```

The space stays on the server after the example quits.

It advertises itself with Bonjour, so the headset lists it when both are on the same network. Over USB, `bash scripts/android.sh run` forwards the headset's port 8765 to the PC with `adb reverse`, and the list's **USB** entry connects through it; set `TOMORI_SERVER_PORT` when the server listens on another port. To skip the list, give an address: `TOMORI_SERVER=192.168.1.10:8765 bash scripts/android.sh run`, or pass the intent extras `tomori.server` and `tomori.name` to `am start`. Your name defaults to the headset's model name, as a suggestion: once the headset is paired, the server keeps your name with its token, the same on every device using it, and you change it from the hand menu's **People & security**, by choosing **Change your name** in a space and saying the name while holding **X**. An owner can rename any token on the server with `tomori-server auth rename ID NAME`. The headset remembers the last server it connected to, and connects to it again when it finds it, unless you move the selection first. The headset draws with 4× multisample anti-aliasing; pass the intent extra `-e tomori.msaa off` to `am start` to compare without it.

### Controls

Each controller has its own ray, trigger and grip, so either hand can point at, choose and carry its own thing at once. A ray is drawn in full while it meets something, or while its trigger or grip is held, and is otherwise a short stub; it hides while its hand hangs at your side.

Before you join a space, servers, spaces and the **New space** object stand in the world in front of you, each labelled. Point either controller at one and pull its trigger to choose it, or flick the left stick to select one and press **A**. **B** steps back, to the previous ring or the servers, and **Y** searches or lists again. A server with an incompatible protocol is shown grey, with its version, and can't be chosen. **New space** opens rings of choices around it, one at a time: the backend, the folder, then, for a backend that can attach, a new session or an existing one.

In a space:

- **Hold X:** talk to the agent; release to send. While a question waits, what you say answers it. See [Voice](#voice).
- **Left stick click:** turn live audio on or off; it turns on only with the stick at rest.
- **Right stick click:** play the latest spoken reply again, to you only; click again to stop it.
- **Hand menu:** turn your left palm up, or press **Y** to pin it open at its Space section until **Y** or **B**, and choose with a ray and its trigger. Muting spoken replies for you alone is only here, as **Spoken replies** in the Space section (see [design/hand-menu.md](design/hand-menu.md)).
- **Transcript:** turn your right palm up to read the conversation and the queue on your wrist; squeeze the right grip to detach it into the world, and again to put it back. While it shows, the right stick moves its focus and **A** expands a turn, confirms an approval's choice, *allow once*, a standing option or *deny*, or cancels the focused queue entry, with 3 seconds to undo. Nothing is decided without **A**.
- **Sticks:** the two are alike, each for its own hand. Pointing at a panel with more than it shows, with the ray drawn in full, a stick scrolls it for you. Otherwise, where the environment allows travel, as the farmyard does: left or right snap-turns 30°, forward aims a teleport along that hand's ray and letting go lands at the ring, and back steps back a short way, or, held, rises where the environment lets you fly. To stay room scale, turn **Travel** off in the hand menu's Space section (see [design/locomotion.md](design/locomotion.md)).
- **Grips:** squeeze one on a projected panel or a wall to carry it with that hand; that hand's stick pushes it away, pulls it closer and turns it, and the right stick click turns it back to face you. Let a panel go in a wall's outlined slot to dock it, or elsewhere to leave it floating; press **B** while carrying it to throw it away for everyone. Squeeze the other grip on it too to hold it with both hands, which move, turn and stretch it between them. A wall's handles size, shape and tilt it (see [design/projection.md](design/projection.md)).
- **Passing:** squeeze your free hand's grip on a stone or stick the other hand holds to pass it across.
- **Going places:** point at a teleport pad or target and pull the trigger. In the farmyard, from the rock in the waterfall's pool, point at the falling water to reach the terrace above the falls, whose cairn brings you back, and point at the fireside log by the campsite to sit on it.
- **B:** close or back out of what is open, such as the pinned hand menu, the transcript or a selected queue entry, or dismiss the problem shown. **Hold B** for a second to leave the space. A press never decides a request, cancels queued words or leaves.

The server keeps the conversation: after a dropped connection the headset reconnects and rejoins the space by itself. If the space is gone, as after the server restarts, the headset returns to the space list.

### Asking the agent to change the space

The agent can read and change its space through Tomori's own tools, so you can ask it from the headset to "speak replies briefly" or "move us to the farmyard". `tomori-server` serves them over MCP on a free loopback port (`--mcp-bind ADDRESS` to choose one, `--no-mcp` for none), and registers them with OpenCode for each directory where a space is open, as the remote MCP server `tomori`, with a bearer token made for each run; nothing is written to OpenCode's configuration, and OpenCode forgets them when it restarts, after which they are registered again. The tools are `tomori_server`, `tomori_spaces`, `tomori_space`, `tomori_presence`, `tomori_move_panel`, `tomori_show_image`, `tomori_set_speech` and `tomori_set_environment`, and `tomori_pairing`, `tomori_confirm_pairing` and `tomori_decline_pairing`, with which the agent confirms a device pairing with the code a participant reads aloud from the device, or declines it; they never reveal the code, a wrong code declines the pairing, and every decision is shown on the console, the status page and the space's Tomori wall (see `design/security.md`). `tomori_presence` tells the agent where everyone in its space is, in metres in the space's coordinates, what each is looking and pointing at, the wall nearest them, the walls in their view and the space's walls and free slots, so that it can show things where you can see them, and `tomori_move_panel` moves a panel when someone asks; only the space's own agent is told. `tomori_show_image` shows an image file on the server's machine, such as a render, a diagram or a screenshot, as a panel everyone in the space sees. With `tomori_workers` and `tomori_remove_workers`, the agent removes its own finished sub-agents, and only its own space's, rather than calling OpenCode's API. Those that act on a space act on the space bound to the session that calls them, which OpenCode names in each call, so another session in the directory, or a sub-agent, cannot change your space. The agent acts for the space's owners; everyone in the space sees each change, the agent is asked to say what it changed, and the server logs it. See `design/mcp.md`.

### Delegated workers

The coordinator can use OpenCode's existing subagent tool. Child sessions of the open root conversation appear as smaller swarms, each with a sign above it giving its title and state: cubes travel from the coordinator or parent worker to a station placed about the agent's home. An environment gives the agent a home, where the swarm and its workers stand, and where participants arrive and the Tomori wall stands, and its client crate may place the stations by its own algorithm. The agent comes to whoever speaks to it, or to whoever chooses **Call Tomori here** in the hand menu, while its workers stay at home unless it places them where their work is; see [design/environments.md](design/environments.md) and [design/interaction.md](design/interaction.md). Existing history appears settled rather than masquerading as newly delegated work. On observed completion a worker settles into one solid, slowly rotating cube; on error, separate fragments with a gentle wave. Their signs show observed running, waiting, idle, finished, error, stopped or unknown states; stale or absent data is not treated as completion. Partial observation errors do not erase independently observed worker statuses. This is a visual projection of actual OpenCode sessions, not a simulator or invented progress. Point at a worker as you press **X** to ask the agent about it, and ask the agent to clear away finished workers, which it does with `tomori_remove_workers` (see `design/mcp.md`).

Button actions are processed independently of controller poses and render availability; losing a controller's tracked pose must not discard X presses or releases. Real session focus loss still cancels recording. Headset validation is needed to distinguish missing tracking from Quest runtime/profile changes that deactivate the button actions themselves.

Worker titles, statuses and projected panels also appear in the spectator/OBS view: show only content appropriate for that audience. Worker access to the coordinator's memory is currently restricted by instructions, **not** by a security sandbox. Permissions are never automatically approved; pending child permissions or forms still need to be handled in OpenCode. OpenCode's active status covers foreground session drains and is not proof that all background work everywhere has stopped.

### Swarm conversation signals (prototype)

Join a space to give the swarm an observable conversational state:

| Appearance | Meaning |
| --- | --- |
| Tight violet circulation, rotating broken halo | The space's agent is working, or input is queued |
| Open, calm mint swarm with a steady complete halo | The agent is idle and nothing is queued |
| Amber, gently pulsing broken halo | An approval or question needs an answer |
| Dim fragmented halo / red fragmented halo | Status unknown / the server is unreachable |

The ready halo persists; you don't need to catch an end-of-turn flash. An active violet arc represents observed work in the space, **not** all delegated/background work. This is status telemetry, not a visualization of hidden reasoning. Outside a space the swarm keeps its original flocking behavior. Controller attraction still works. While in a space, the text console sits lower so it doesn't obscure the swarm.

### Voice

In a space, click the **left stick** to toggle live audio to the other participants and spectator without addressing the agent. The panel shows **MIC LIVE** while the microphone is open. Click again to mute. A click while you push the stick to travel doesn't turn it on; let the stick go to the middle first. Live audio is off by default and stops on focus loss, taking the headset off, disconnection, departure or microphone failure; toggle it again to resume. Live capture requests the Android voice-communication preset, but echo cancellation is not guaranteed: use headphones or lower the headset volume if there is feedback. The live-microphone toggle is currently available on Quest, not desktop XR.

Hold **X** to talk to the agent: the panel shows **MIC AGENT** with the words recognized so far, and a **REC** timer above it counts up. Release **X** to send; the server finishes the transcript and queues it like typed input, where **B** can still cancel it. Releasing X returns to live audio if it was enabled; otherwise the microphone is off, and the panel says **Mic off**. Tomori asks for microphone access a moment after it starts, once the world is shown, so that the request does not interrupt your first recording; until you answer, the server list, the lobby, the status pill and the hand menu say "Waiting for microphone permission...", unless they have something else to say. If you dismiss it, the first use of the microphone asks again. If it was denied, allow the microphone for Tomori in the headset's Settings.

If anything stops the words reaching the agent intact, such as lost audio, a dropped connection, nothing being heard or a full queue, the panel shows the error and nothing is sent. Losing focus, as when the system menu opens, cancels a recording that is still held; release **X** before holding it again.

The agent's replies are spoken as they stream in, and everyone in the space hears them and each other's push-to-talk audio through the headset or desktop speakers. You never hear your own recording back. While **X** is held, nothing plays, so that the speakers cannot be transcribed as your words; the reply carries on meanwhile and resumes where it has got to when you release (see [design/speech.md](design/speech.md) and [design/live-audio.md](design/live-audio.md)).

Replies are spoken in full. Any participant can switch a space to brief speech from the **Y** list, or with `set_speech`: each message then speaks its first paragraphs, up to about 20 seconds, then says "More on the panel." A space chosen brief stays brief across a server restart.

The server keeps the latest two spoken replies, up to 5 minutes of audio. If you join a space, or put your headset back on, having missed any of the latest reply, you hear it from its start, then whatever followed, until you have caught up with live speech; others in the space are unaffected. Taking the headset off counts as away, and turns the live microphone off. The headset tells the server when it is put on or taken off, from its proximity sensor and the OpenXR session state, and the status line counts who is away.

Recognition and synthesis run inside the server through sherpa-onnx, whose libraries `scripts/fetch-sherpa-onnx.sh` downloads and checks, and the build links. Recognition uses a streaming transducer that shows words while you speak, and a punctuation model adds capitals and punctuation to the finished transcript; synthesis uses Kokoro, given English phonemes that the server works out in Rust with `loqui-g2p`. The models need about 560 MB in `.tools/`. From the repository root:

```sh
bash scripts/fetch-asr-model.sh
bash scripts/fetch-tts-model.sh
cargo run -p tomori-server-app --bin tomori-server -- --backend opencode --speech-root .
```

Speech runs on the CPU, including on macOS; see [crates/speech/readme.md](crates/speech/readme.md) for CUDA on Linux. Without `--speech-root`, or if the recognition model can't load, every utterance fails with an error that says so. Without the punctuation model, which `scripts/fetch-asr-model.sh` also fetches, transcripts are lowercase and unpunctuated, and the server says "Transcript punctuation: unavailable" when it starts. Replies are spoken with Kokoro, which needs another 700 MB; without it the server says "Speech synthesis: off" or "unavailable" when it starts, and replies are only shown.

## PC spectator and live audio (prototype)

The spectator joins the same server and space as the headset and renders a read-only view from a participant's relayed head, controller and hand presence. It renders the shared worker state and simulates its own swarm from the space's activity; it is not a pixel-perfect Quest cast. The headset's HUD is not shown. No OBS or YouTube stream starts automatically.

Both tracked controller grip poses are rendered using the bundled, MIT-licensed WebXR Quest Touch Plus models (see `crates/render/assets/controllers/`). If the Quest runtime exposes `XR_FB_render_model`, the headset attempts to load its models on a background thread and falls back to the bundled meshes if loading fails. The PC spectator currently uses the bundled meshes rather than copying runtime assets. Controller textures are supported for embedded PNG/JPEG and Basis Universal KTX2; button animation is not yet implemented. When `XR_EXT_hand_tracking` is available and hand tracking is enabled on the Quest, tracked hands are shown as a simple joint-and-bone skeleton rather than a skinned hand mesh. Hand gestures do not replace the existing controller bindings. The server relays controller poses and tracked joint positions to the other space participants.

```sh
cargo run -p tomori-client-quest -- discover --server 127.0.0.1:8765
cargo run -p tomori-client-spectator -- --server 127.0.0.1:8765 --space SPACE_ID
```

The spectator is its own executable, `tomori-spectator`, in the `tomori-client-spectator` package; `tomori-spectator --help` lists its options. `tomori --spectator`, `--spectator-pipewire` and `--spectator-pipewire-preview` still run it as before.

The spectator opens a normal window with the platform's title bar, requesting a **1920×1080 physical-pixel** content area to match a 1080p OBS canvas; move it, resize it to any size and aspect, or make it full screen with the window's own controls, and it renders at the window's actual size. Press **Esc** to close it. For a clean capture without a title bar or border, run it with `--borderless` (`tomori --spectator --borderless`), which keeps a 1080p minimum and can't be moved or resized; on a 1080-pixel-high display the desktop panel can cover part of it, and the compositor may also constrain sizes on smaller or scaled displays. Check the actual size in the startup log and OBS preview.

In OBS, add the spectator window as a window/screen capture. The spectator plays participant audio and agent speech through the PC's default audio output; capture that output separately if you want audio. Run it with `--voices agent` to play only the agent's speech, as when demonstrating in a room where your own voice would come back out of the PC's speakers, or `--voices none` for silence; **V** in the window cycles through `all`, `agent` and `none`. Avoid capturing the same sound twice, and preview audio/video synchronization before going live.

### Experimental direct PipeWire video (Linux)

Instead of capturing a window, run `cargo run -p tomori-client-spectator -- --pipewire --server 127.0.0.1:8765 --space SPACE_ID`. It publishes `Tomori-Spectator` (`tomori-spectator`) **without creating a window or display surface**. Press **Ctrl-C** to stop it. For an optional window showing the same direct output, use `cargo run -p tomori-client-spectator -- --pipewire-preview --server 127.0.0.1:8765 --space SPACE_ID` instead; Escape closes that window and stops its output. The preview window is decorated, and borderless with `--borderless`; `--pipewire`, having no window, refuses `--borderless`. Run only one direct-output spectator at a time.

Both modes render to a fixed **1920×1080 RGBA** texture at up to 30 fps. The optional preview window requests 1920×1080 and shows the output whole at any window size, with black bars where the window's aspect differs; the window-capture fallback, without `--pipewire`, still renders at its actual window size. The Quest reports presence through the server. Audio plays through the spectator's desktop output, or the PipeWire output named with `--audio-output NAME` (see `pactl list short sinks`); the PipeWire node carries video only. Any local client with PipeWire access may be able to view it.

For the direct virtual camera and microphone used to share this view in a Slack Huddle without OBS, see [the Linux setup notes](docs/slack-huddle.md).

This prototype requires `gst-launch-1.0`, GStreamer's `fdsrc` and `rawvideoparse` elements, and the PipeWire GStreamer plugin. It performs an asynchronous GPU-to-CPU readback and copies through a GStreamer subprocess; it is **not zero-copy**. Readback and the outgoing queue are bounded, so frames can be dropped under load. A slow or absent consumer may briefly see older queued frames when it reconnects. If GStreamer fails, the headless process exits with an error; in preview mode the window stays open but the video source stops. Restart the spectator to retry. Each uncompressed 1080p frame is about 7.9 MiB. A sustained 30 fps is not guaranteed; check performance and latency in the real preview.

The built-in OBS PipeWire screen capture is not the same as a generic PipeWire video input. The [ioquatix fork of `obs-pwvideo`](https://github.com/ioquatix/obs-pwvideo/tree/discover-video-source-nodes) adds an opt-in **Show video devices and other sources** checkbox to the PipeWire Video source; enable it to select **Tomori-Spectator**. The change is under review in [upstream PR #8](https://github.com/hoshinolina/obs-pwvideo/pull/8). To build and install the fork as a Pacman-managed package on Arch:

```sh
cd packaging/obs-pwvideo-ioquatix-git
makepkg -si
```

The fork package conflicts with and replaces the standard `obs-pwvideo` package; don't install both. Review the PKGBUILD before building. OBS must be restarted to load the installed plugin. Rebuild this local VCS package to update the fork; it is not itself an AUR package.

Alternatively, the [AUR `obs-pwvideo` package](https://aur.archlinux.org/packages/obs-pwvideo) works with this stream **when the OBS source already has its target saved** as `tomori-spectator`. The plugin does not discover `Video/Source` nodes in its picker, although it can connect to a saved target of that class. Avoid changing the source's target to “No autoconnect”, which clears it. For a new source, either manually link the PipeWire ports (`pw-link` or a graph editor), or, with OBS closed and the scene collection backed up, set the source's `settings.target` to `tomori-spectator` in its scene collection JSON. Check that the `id` is `pipewire-video-source` and preserve all other settings. An isolated libobs test with the unmodified AUR package and a saved target received frames from the earlier 1080p spectator; check the real OBS preview too.

Alternatively, the supplied compatibility patch makes the spectator selectable in the plugin's picker. To build that patched plugin in a new directory (requires OBS, PipeWire and DRM development headers, CMake and Ninja):

```sh
git clone https://github.com/hoshinolina/obs-pwvideo.git /tmp/opencode/obs-pwvideo-build
git -C /tmp/opencode/obs-pwvideo-build checkout --detach ba64bf322ad450f8fe8c43236878e83669fb677d
git -C /tmp/opencode/obs-pwvideo-build apply "$PWD/patches/obs-pwvideo-spectator.patch"
cmake -S /tmp/opencode/obs-pwvideo-build -B /tmp/opencode/obs-pwvideo-build/build -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
cmake --build /tmp/opencode/obs-pwvideo-build/build
```

Run these commands from the project directory; the clone directory must not already exist. The plugin is **not installed automatically**. When ready to change OBS, follow [the plugin's installation instructions](https://github.com/hoshinolina/obs-pwvideo#how-to-build-from-source), using the patched build and its `data` directory. Check for an existing installation before copying anything. Do not install both patched and AUR versions at once. OBS must be restarted to load a different plugin; do not restart during a stream. Verify the live preview, performance and audio/video sync before going live. Keep the `tomori-spectator` window and window capture as the fallback.

## Build and sideload (Linux)

Enable Quest developer mode, connect a USB data cable, and accept **Allow USB debugging** inside the headset. `adb devices -l` must show `device`, not `unauthorized`. Keep your boundary enabled and stay seated or stationary. Wake the Touch controllers before launching: Quest may otherwise show a **controllers required** dialog and block the application from starting.

Install a **user-local rustup** toolchain (Rust 1.92+) and Android tools:

```sh
rustup target add aarch64-linux-android
cargo install cargo-apk --version 0.10.0 --locked
```

With Android SDK command-line tools installed, install these SDK components (review/accept Google's licenses when prompted):

```sh
sdkmanager 'platform-tools' 'platforms;android-32' 'build-tools;35.0.0' 'ndk;29.0.14206865'
export ANDROID_HOME="$HOME/Android/Sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/29.0.14206865"
bash scripts/android.sh run
```

The script downloads the official Khronos **1.1.65** OpenXR loader AAR from Maven Central, checks its published SHA-256, and packages the arm64 loader in the APK. The Quest APK does not contain a transcription model. `cargo-apk` creates a local debug signing key if needed. Build output: `target/debug/apk/tomori.apk` (package `com.codeotaku.tomori`), which `bash scripts/android.sh path` prints. Builds before November 2026 were `io.github.ioquatix.tomori`, a separate app that keeps its own pairing; see [Naming](docs/naming.md#migrating-to-comcodeotakutomori). Launch from **Library → Unknown Sources → Tomori** if not auto-started.

```sh
bash scripts/android.sh devices
bash scripts/android.sh logs
bash scripts/android.sh build  # build without deploying
adb shell am start -n com.codeotaku.tomori/android.app.NativeActivity -e tomori.msaa off  # start it with intent extras
```

`--release` builds the release profile, signed with the keystore that `CARGO_APK_RELEASE_KEYSTORE` and `CARGO_APK_RELEASE_KEYSTORE_PASSWORD` name; a release's APK is built, signed and checked by `cargo bake android:apk:build`, `sign` and `verify` instead (see [Releasing](#releasing)). `--package NAME` builds another headset build than Tomori's own, such as `bash scripts/android.sh --package tomori-example-headset run`, with its own application ID, label, icon and assets; see [Packaging a Headset Build for the Quest](crates/client-quest/context/android.md). The script reads them from `cargo metadata`, so it needs `jq`.

You can save machine-local exports in `.android-env` (gitignored). The SDK, NDK, JDK (`java`/`keytool`), `cargo-apk`, and ADB must be available. Debug builds use optimization level 1; the example is for local sideloading, not store submission. This machine uses isolated tools in the ignored `.tools/` directory, referenced by `.android-env`; system Rust is not modified.

## Desktop window

```sh
cargo run -p tomori-client-quest
```

Runs the same simulation and wgpu renderer in a resizable desktop window; **no headset or OpenXR runtime needed**. Uses Vulkan on Linux (Wayland or X11).

- **Hold left mouse:** attract agents to the cursor on a plane through the swarm.
- **Right-drag:** orbit the camera; **mouse wheel:** zoom.
- **R:** reset agents; **Space:** pause/resume; **Esc:** quit.

That window doesn't connect to a server. To take part in a server's spaces from a desktop window instead, as on the headset, use `--desktop` (see [design/desktop.md](design/desktop.md)):

```sh
cargo run -p tomori-client-quest -- --desktop [--server HOST:PORT] [--name NAME] [--token TOKEN]
```

The mouse points as the right controller does: its left button chooses what it points at, and dragging with the right button looks around. **Enter** is A, **Backspace** is B (hold it for a second to leave a space), hold **Space** to talk to the agent (X), **Tab** is Y, the arrow keys are the left stick, so in a space that allows travel, hold **↑**, point the mouse at the floor and release **↑** to land at the ring, and **←** and **→** turn, **\[** and **]** the right stick left and right, and **Page Up**/**Page Down** the right stick up and down. **M** opens the hand menu, **K** knocks on the space you watch, **+** and **-** admit or decline the first knock waiting, **P** pairs, **V** mutes spoken replies and **R** replays the latest. `tomori --help` lists them too. Desktop windows draw with 4× multisample anti-aliasing where the GPU supports it.

To use a server without a headset or a GPU, show the headset's panel in the terminal:

```sh
cargo run -p tomori-client-quest -- --console [--server HOST:PORT] [--name NAME]
```

Type the controller buttons as commands, such as `a`, `b`, `y`, `u` and `d` for the left stick, and `<` `>` for the right stick. A terminal can't tell when a key is released, so `x` starts talking to the agent through the computer's default microphone and `x` again sends it. `--server` (or `TOMORI_SERVER`) and `--name` also apply to `--xr`, where **X** is held as on the headset. On Linux, building the desktop client needs the ALSA development files (`libasound2-dev` or `alsa-lib-devel`) for the microphone.

## Tests / desktop OpenXR

```sh
bash scripts/fetch-sherpa-onnx.sh        # once: the speech libraries the server links
bash scripts/fetch-asr-model.sh whisper tiny.en && bash scripts/fetch-tts-model.sh   # optional: the speech models the speech tests run
cargo test --workspace                   # simulation, projection, rendering, server and client tests
cargo run -p tomori-client-quest -- --xr        # use an active desktop OpenXR Vulkan runtime
```

The rendering tests draw offscreen with Vulkan: on a GPU when there is one, and otherwise on a software driver such as Mesa's lavapipe (`mesa-vulkan-drivers` on Debian and Ubuntu, `vulkan-swrast` on Arch). With no Vulkan adapter at all they are skipped, each saying so; set `TOMORI_REQUIRE_GPU=1` to fail them instead, as CI does. The tests in each test binary share one Vulkan device, created once and never dropped, because Vulkan loaders before 1.4.346 can crash when one thread creates a device while another uses one. To render on lavapipe, as CI does, on a computer with a GPU, name its driver file in `VK_DRIVER_FILES`, such as `VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json`. Every `.wgsl` shader is also parsed and validated with naga, with or without an adapter.

The speech tests that run the real recognition, punctuation, voice activity and synthesis models are skipped, each saying so, when the models aren't installed; set `TOMORI_REQUIRE_MODELS=1` to fail them instead, as CI does. They use Whisper `tiny.en` rather than the server's default, `small.en`; see [crates/speech/readme.md](crates/speech/readme.md#tests).

`cargo bake test` runs the workspace tests too, fetching the speech libraries first. On every pull request, GitHub Actions checks formatting and Clippy, runs the tests, including the rendering tests on lavapipe and the speech tests with their models, on current stable Rust, checks that every target builds on the minimum supported version (the workspace's `rust-version`, 1.92), and builds the client core and the headset APK for `aarch64-linux-android`. On stable Rust the tests run under `cargo bake test:coverage --all-targets true --nextest true`, each in a process of its own under cargo-nextest, which fails unless every measured source region runs: the 100% region coverage that Base projects require. The main branch requires all of these, through the Test workflow's `test-result` job, and the Publish workflow's `check` job. Pull requests merge through GitHub's merge queue, which runs these checks again on the pull request merged with those ahead of it in the queue, with the tests run without coverage instrumentation, which takes most of their time, since each pull request has passed the gate already; pushes to main run the gate again: `gh pr merge N --squash` adds a pull request whose checks have passed to the queue, and `gh pr merge N --squash --auto` waits for its checks and then adds it.

Code that is not covered gets a test, or goes if nothing needs it. Regions inside `unreachable!` are not counted. The only other exceptions are a few thin files of device and platform calls that CI cannot make: the headset's OpenXR session and its Vulkan device, the desktop window and its audio devices, a forked child before it executes its program, the server process's `_exit`, and starting the multicast DNS daemon. Each is excluded in its crate's `Cargo.toml`, with its reason, and the coverage report lists their sizes; [design/testing.md](design/testing.md) lists them, and says which decisions are still in them rather than in covered modules. To measure coverage locally, install `cargo-llvm-cov` and the `llvm-tools` component. With a distribution's Rust whose standard library sources are under `/usr/src`, which `cargo bake test:coverage` cannot read, run `cargo llvm-cov --workspace --all-targets` and then `cargo llvm-cov report --ignore-filename-regex '^/usr/src/'` instead.

A USB-connected Quest does **not** provide a Linux desktop OpenXR runtime: build the Android APK for standalone Quest use. Desktop execution requires its own OpenXR loader/runtime (for example Monado).

## Layout / interop

- `crates/client-desktop/src/desktop.rs`: the server-less window, orbit camera, mouse attraction; `desktop_participant.rs`: the `--desktop` window. `crates/client-spectator/src/spectator_window.rs` is the spectator's window. `window.rs` is what a window gives its contents, and `desktop_window.rs` the winit and surface calls alone.
- `crates/client-quest/src/xr.rs`: Android/desktop OpenXR lifecycle, predicted views, controller actions: the device calls only, with the values read and the choices made in `xr_types.rs`.
- `crates/scene/src/scene.rs`: the frame step the headset and the desktop window share, from plain input; `scene/headset.rs` and `scene/desktop.rs` hold each platform's own logic, and `crates/render/src/scene_graphics.rs` draws a frame.
- `crates/client-quest/src/vulkan.rs`: XR-created Vulkan instance/device imported into wgpu; borrowed two-layer XR swapchain images, one layer per eye.
- `crates/render/src/graphics.rs`, `crates/render/src/swarm.wgsl`: instanced cube rendering with per-eye depth.
- `crates/scene/src/swarm.rs`: deterministic CPU flocking, separation/alignment/cohesion.
- `crates/scene/src/caption.rs`, `crates/render/src/caption.wgsl`: head-relative, stereo text panel.
- `crates/scene/src/conversation.rs`: server list, space list and space panel, driven by server events.
- `crates/client/src/connection.rs`: the connection that carries out the conversation's actions, with the server, the microphone, the speaker and Bonjour browsing.

The Vulkan device uses the runtime-selected physical GPU and enables the full extension/feature set required by wgpu. Swapchain images remain runtime-owned; wgpu has no permission to destroy them. Each frame waits for submitted GPU work before releasing the XR image, leaving it in `COLOR_ATTACHMENT_OPTIMAL`. This simple conservative synchronization favors correctness over maximum throughput. Tracking loss and non-rendering frames submit no layer; sessions handle ready/stopping/exit events. The scene initially faces the headset; the app uses `LOCAL_FLOOR` when available so the Quest's system Meta-button recenter moves the scene, falling back to `LOCAL` with an approximate 1.6 m floor offset. Reference space changes take effect at the runtime's specified frame time. Recentering doesn't reset the agents, conversation, or interaction state. This is opaque VR, not passthrough; optional tracked-hand skeletons are described above.

## Crates

The workspace's crates are named `tomori-` followed by their role, in folders under `crates/` without the prefix. They share one version and one [releases.md](releases.md), and are released together. These are published to crates.io, though none has been published yet:

- [`tomori-protocol`](crates/protocol/readme.md): the wire protocol shared by clients and the server.
- [`tomori-client`](crates/client/readme.md): a client's connection, reconnection, microphone audio and playback, and the `Connection` that carries out a client's session with a server.
- [`tomori-discovery`](crates/discovery/readme.md): Bonjour advertising and browsing of servers.
- [`tomori-security`](crates/security/readme.md): tokens, pairing and certificate pinning.
- [`tomori-backend`](crates/backend/readme.md) and [`tomori-backend-opencode`](crates/backend-opencode/readme.md): the agent backend interface, and its OpenCode implementation.
- [`tomori-environment`](crates/environment/readme.md): the interface of an environment's server side, which the server runs for the spaces bound to its environment.
- [`tomori-physics`](crates/physics/readme.md): rigid-body physics for an environment's loose objects, taken with either hand, carried, thrown and stacked, simulated by its server side on Rapier and mirrored by its clients, which never simulate.
- [`tomori-assets`](crates/assets/readme.md): environments' models, loaded at runtime from content-addressed `.glb` assets, read as untrusted input, and drawn skinned on the GPU.
- [`tomori-world`](crates/world/readme.md): the render world, free of GPU code: what is drawn each frame, with its bounds, layers, lights and views, and what the eyes may see of it.
- [`tomori-speech`](crates/speech/readme.md): speech recognition and synthesis with sherpa-onnx.
- [`tomori-highlight`](crates/highlight/readme.md): the server's syntax highlighting of the agent's code panels, with tree-sitter and syntect, into spans of shared token kinds.
- [`tomori-prometheus`](crates/prometheus/readme.md): a generic Prometheus data source for environments' server sides, with the endpoint and token their application gives it, typed results and label filtering.
- [`tomori-environment-farmyard`](crates/environment-farmyard/readme.md) and [`tomori-environment-farmyard-client`](crates/environment-farmyard-client/readme.md): the farmyard environment's identity, layout and models, which its server and client halves share, and its scenery, which Tomori's own app and spectator register.
- [`tomori-environment-sky`](crates/environment-sky/readme.md) and [`tomori-environment-sky-client`](crates/environment-sky-client/readme.md): the Sky environment's identity, which its server and client halves share, and its renderer, which Tomori's own app and spectator register.
- [`tomori-environment-github`](crates/environment-github/readme.md), [`tomori-environment-github-server`](crates/environment-github-server/readme.md) and [`tomori-environment-github-client`](crates/environment-github-client/readme.md): the GitHub prototype's identity, its server half, with the GitHub client, the token and `--github-repository`, and its client half.
- [`tomori-server`](crates/server/readme.md): the server library, which another project builds a server of its own on, registering its own backends and environments through `Server::builder()`. [examples/server](examples/server/readme.md) is an example.
- [`tomori-server-app`](crates/server-app/readme.md): Tomori's own server, the `tomori-server` executable, installed with `cargo install tomori-server-app`, which registers Tomori's own environments on the library's builder.
- [`tomori-text`](crates/text/readme.md): the bundled fonts and the signed-distance-field glyph atlas that text is drawn from.
- [`tomori-scene`](crates/scene/readme.md): the platform-neutral client, with the frame step, the conversation, and the panels' layout, free of rendering, windowing and device code.
- [`tomori-render`](crates/render/readme.md): the wgpu renderers and shaders that the headset, the desktop window and the spectator share, and the `Scenery` that draws an environment.
- [`tomori-client-desktop`](crates/client-desktop/readme.md): the desktop modes.
- [`tomori-client-spectator`](crates/client-spectator/readme.md): the spectator, installed with `cargo install tomori-client-spectator` for its `tomori-spectator` executable.
- [`tomori-client-quest`](crates/client-quest/readme.md): the headset and desktop application, the `tomori` library and executable, and the library another project builds a headset of its own on, registering its own sceneries through `tomori::Builder`. Tomori's own build, which registers Tomori's own environments, is its default `app` feature. [examples/headset](examples/headset/readme.md) is an example. Tomori's own headset is an APK.

[design/crates.md](design/crates.md) gives the reasons, and [docs/naming.md](docs/naming.md) records that the names were free on crates.io.

## Third-Party Components

Original project code is released under the MIT License; see [license.md](license.md). Third-party components retain their own licenses:

- Bundled controller assets: [MIT](crates/render/assets/controllers/LICENSE.md).
- Bundled fonts, subsets of Noto Sans Mono, Noto Sans Symbols 2, Noto Sans Math and Noto Sans JP: [SIL Open Font License 1.1](crates/text/assets/fonts/readme.md), also in [crates/render/assets/licenses/fonts-LICENSE.txt](crates/render/assets/licenses/fonts-LICENSE.txt) for the APK.
- The server's syntax highlighting builds on tree-sitter, its grammars and syntect, under the MIT License, and syntect embeds Sublime Text's syntax definitions from [sublimehq/Packages](https://github.com/sublimehq/Packages), under that repository's permissive licence; see [crates/highlight/readme.md](crates/highlight/readme.md). None of them is in a client or the APK.
- Basis Universal notices and test fixtures: [Apache-2.0](crates/render/assets/licenses/basisu-LICENSE.txt), with fixture provenance in [crates/render/tests/fixtures/README.md](crates/render/tests/fixtures/README.md).
- The optional OBS plugin compatibility patch follows the upstream plugin's GPL-2.0-or-later licensing; its packaging metadata is under `packaging/`.
- The server's speech stack is permissively licensed. It links sherpa-onnx (Apache-2.0) and the libraries its static archive bundles, among them ONNX Runtime (MIT), OpenFst, Kaldi's feature and decoder libraries and SentencePiece (Apache-2.0), kissfft (BSD-3-Clause) and utf8proc (MIT), which `scripts/fetch-sherpa-onnx.sh` downloads and verifies; and [`loqui-g2p`](https://github.com/Awakened-Labs/loqui) (MIT), which phonemizes English for Kokoro and embeds misaki's lexicons and a grapheme-to-phoneme model, both Apache-2.0, and spaCy's English tokenizer rules, under the MIT License; its `NOTICE` lists them. espeak-ng and piper-phonemize (GPL-3.0), which sherpa-onnx linked before 2.0, are no longer linked; see [crates/speech/readme.md](crates/speech/readme.md).
- Speech models are downloaded separately, not bundled in this repository. Their licensing requirements are noted in the relevant setup sections above. The Kokoro archive that `scripts/fetch-tts-model.sh` fetches also contains espeak-ng's data (GPL-3.0), which the server does not read.

## Releasing

Prepare a release with `cargo bake cargo:release:patch` (or `minor`, `major`, or `bump --version X.Y.Z`) from a clean checkout of `main`; it bumps the version, commits with the `Release: vVERSION` trailer, validates the candidate, and opens the pull request. After review and merge, GitHub Actions publishes the release when the configured `crates-io` environment approves it, then creates or updates the matching GitHub Release from `releases.md`. Nobody edits `releases.md`: the release writes each version's notes from the titles of the pull requests merged since the previous release, so a pull request's title is its release note, and each paragraph of its description that begins `Breaking:`, saying what breaks and how to migrate, is listed under **Breaking changes**. `cargo bake releases:notes Unreleased` previews them. Follow the shared [Releasing skill](https://github.com/base-rs/base-project/blob/main/context/releasing.md) for the standard process.

Each release also attaches Tomori's headset APK to its GitHub Release, as `tomori-VERSION.apk` with its `.sha256`, built from the release's commit on `main` by bake-cargo's release artifacts workflow, `.github/workflows/release-artifacts.yml`, once publishing succeeds, with `scripts/release-artifacts/` and Tomori's `cargo bake android:apk:*` tasks. It is signed with Tomori's release key, held in the protected `android-release` environment, whose certificate's SHA-256 digest is committed in `android/release-certificate.sha256`; see [design/android-release.md](design/android-release.md). To check a download, with `ANDROID_HOME` naming the Android SDK:

```sh
sha256sum -c tomori-0.2.0.apk.sha256
cargo bake android:apk:verify tomori-0.2.0.apk
```

A release APK cannot update a locally built one, or the other way round: they are the same app, `com.codeotaku.tomori`, signed with different keys, so switching between them means uninstalling first, which loses the app's pairing.

Most of Tomori's crates were first published at 0.2.0; `tomori-assets`, `tomori-world`, `tomori-environment`, `tomori-environment-sky-client`, `tomori-environment-github`, `tomori-environment-github-server`, `tomori-environment-github-client`, `tomori-environment-farmyard`, `tomori-environment-farmyard-client`, `tomori-highlight`, `tomori-physics`, `tomori-prometheus` and `tomori-server-app` have not been yet, and need a first upload before the next release. A release publishes the crates listed under [Crates](#crates), which `cargo bake cargo:packages` lists, and `cargo publish --workspace --dry-run --locked` checks that they package and build. Between releases, while the workspace's version is one already on crates.io, it builds each crate against the published copies of the others at that version, so it fails once a crate uses something newer; it passes again once the release bumps the version. Each crate's first upload is `cargo bake cargo:bootstrap PACKAGE`, with an owner token, which also registers `publish.yml` as its trusted publisher, so that later releases publish through GitHub Actions. Upload them in the order of their dependencies, since crates.io needs a crate's dependencies, including dev-dependencies with a version, to exist first:

1. `tomori-protocol`, `tomori-security`, `tomori-backend`, `tomori-environment`, `tomori-assets`, `tomori-world`, after `tomori-assets`, `tomori-physics`, `tomori-environment-sky`, `tomori-environment-github`, `tomori-environment-farmyard`, after `tomori-physics`, `tomori-prometheus` and `tomori-text`;
2. `tomori-discovery`, `tomori-speech`, `tomori-highlight` and `tomori-backend-opencode`;
3. `tomori-client`;
4. `tomori-server`;
5. `tomori-environment-github-server`, then `tomori-server-app`;
6. `tomori-scene`, which depends on `tomori-server` for the test server behind its `testing` feature;
7. `tomori-render`;
8. `tomori-environment-sky-client`, `tomori-environment-github-client` and `tomori-environment-farmyard-client`;
9. `tomori-client-desktop`;
10. `tomori-client-spectator`;
11. `tomori-client-quest`.

## Releases

<!-- bake-readme:releases:start -->

See [releases.md](releases.md) for the full release history.

### v0.2.0

- Text on the headset, the desktop window and the spectator is now drawn from fonts rather than 8 × 8 bitmaps, so it is antialiased and stays crisp and steady as you move your head, and Japanese is shown rather than `?`. The hand menu, the wrist transcript, the recording card, the status pill, arrival's labels and workers' signs draw from a glyph atlas of signed distance fields in the new `tomori-text` crate, which bundles subsets of Noto Sans Mono, Noto Sans Symbols 2, Noto Sans Math and Noto Sans JP, under the SIL Open Font License, adding 2.1 MB to the APK. Accented letters, typographic quotes and dashes, Greek and Cyrillic are drawn as themselves, rather than as their base letter or plain equivalent, and a letter followed by a combining accent is drawn as one accented letter. Japanese kana and the 6,355 kanji of JIS X 0208 take two cells each, and lines of Japanese break between characters, never before "。" or after "「". Emoji, Hangul and rarer kanji still show as `?`. Bold is drawn heavier rather than twice, and box drawing joins up without seams. Every row is measured in cells as drawn, so a Japanese title on a worker's sign or a result card wraps or ends with "…" where it reaches the edge, rather than losing its end, and the queue's names on the wrist line up whatever the script; card titles now end with "…" rather than "...". Lines of Japanese no longer touch, and text on distant signs stays steady rather than shimmering as you move. A first screen of new Japanese fills in over a few frames, rather than dropping frames while its glyphs are drawn. Layouts are otherwise unchanged: every hand menu row still fits its 40 cells. `tomori --licenses` now shows the fonts' licences, which the APK carries. Text is not shaped, so scripts such as Arabic are not drawn correctly yet (part of #146).

- The worker you point at now reaches the agent before your words, after your name, as the first stage of shared attention (#81): `Ada: [Pointing at worker "Review commit 2657a52" (opencode session ses_…, running)] how is this one doing?`, rather than on a line after them. What counts is the worker you point at as you press **X**: pointing elsewhere while you talk no longer changes it, and pointing only while you talk no longer counts. The headset still sends only which worker it is, now as the words begin, and the server describes it from what it knows. The queue still shows only your words. For other clients, `utterance_begin` takes an optional `attention`, a list of resources by identity such as `{"kind": "worker", "worker": "ses_1"}`, of which the server takes the first 4 and keeps the context within 1,000 characters; `Pointed` is now `tomori_protocol::Resource`, which is `#[non_exhaustive]` so that kinds can be added, so make a worker with `Resource::worker` and match it with a wildcard arm. An entry the server can't read, or of a kind it doesn't know, is skipped, and the words are kept, where before one malformed entry lost the whole utterance. Square brackets in your words, such as Whisper's "\[Music]" among them, now reach the agent as parentheses, "(Music)", so that only Tomori's description is in square brackets; the queue still shows your words as heard. A client from before this still sends the worker as its words end, and the server still tells the agent, before the words. An older server ignores the new field, so the agent isn't told what you pointed at. In `tomori-client`, `Action::EndUtterance` no longer carries what was pointed at; make the utterance with `Utterance::attending` instead, and `Microphone::end_pointing` is replaced by `Microphone::begin_attending`.

- The headset no longer starts in a black world when it asks for the microphone. It asked as soon as it had focus, before it had drawn anything and while the headset was still settling the launch, and it did not answer Android's requests to pause while starting XR, which the device's log showed timing out. It now asks once the world has been shown for a second, and answers those requests between the steps of starting, logging how long each took. The request itself never stopped the world being drawn, and still does not. While it is unanswered, the server list, the lobby, the status pill in a space and the hand menu say "Waiting for microphone permission...", unless they have something else to say; the headset checks every half second whether it has been answered, and logs the answer. As before, it asks once per launch until allowed, and the first use of the microphone asks again if it was dismissed.

- A client's connection to a server is now `tomori-client`'s `connection::Connection`, which carries out a `connection::Session`, the client's conversation with the server. Tomori's conversation, in `tomori-scene`, is one, and another project's client can drive the connection with a session of its own, with Tomori's certificate pinning, tokens, pairing, reconnection, microphone and speaker. With it, these moved from `tomori-scene` to `tomori-client`: the device's identity and the certificates and tokens it keeps (`identity`), the server list's entries (`server_choice`), the feedback model (`feedback`), and the microphone and speaker traits, now `capture::Capture` and `speaker::Playback`. The helpers for writing a device adapter stay in `tomori-scene`'s `capture` and `playback` modules. `tomori-client` depends on no other client crate. The connection's tests now drive it through its session alone. Its interface is meant to grow without breaking: its enums, such as `Action`, `Report`, `Event` and `ConnectionState`, take new variants, and structs such as `Options`, `Choice` and `identity::Overrides` take new fields, so match them with a wildcard arm and make them with their constructors; `Action::BeginUtterance` takes a `connection::Utterance`, made with `Utterance::new`. A session can no longer choose the certificate a token is kept with: `Action::Paired` and `Action::Admitted` carry only the token, and the connection keeps it with the certificate that connection presented, or ignores it without one. For people using Tomori, two things change. Leaving a server in the moment that pairing with it completes no longer stops the app; the token is not kept. And the server remembered for next time is always the one that connected, even if another was chosen just as it did (#132).

- Your display name now belongs to you on the server rather than to each device. Once a device is paired, admitted by knocking or given a token, the server calls you by the name kept with that token, in the space's participants, in knocks, on the status page and before each message the agent reads, whatever name the device suggests, so it survives reinstalling, reconnecting and restarting the server, and is the same on every device using the token. A token already issued keeps its name, such as "Quest 3". To change it on the headset or the desktop window, choose **Change your name** in the hand menu's **People & security** while in a space, then hold **X** and say the name; in `--console`, type `name NAME`. Everyone in the space sees the new name at once, and every device using the token is renamed. If the name isn't changed, say because no speech was heard or someone has it already, the headset says "Name not changed:" and why, and the next press of **X** says the name again, until **B** cancels. A name someone else in your space has, or another token has, in any case, is refused with "That name is taken". Names are cleaned on the server before anyone sees them: line breaks become spaces, characters that reverse or hide text, such as bidirectional overrides and zero-width spaces, are removed, and a name may not start with `/`, contain `:`, `[` or `]`, or be longer than 32 characters, so a name can no longer make the agent read words as someone else's. An owner can rename any token on the server's machine with `tomori-server auth rename ID NAME`, which a running server applies within a few seconds; `auth mint --name` follows the same rules. Visitors keep the name their device suggests, for as long as the server keeps them, and can change it too, but it is never written down; the app suggests it again when it reconnects, until it is restarted. A visitor's knock is shown as theirs, such as "Let Ada (visitor) in", since nothing vouches for their name. Pairing another device still gives it a token, and a name, of its own. The protocol adds `set_name`, a `naming` utterance, a `name` in `Welcome` and a `name` message for clients that say they read it, so older clients and servers work as before, and an older client's suggested name is ignored only for a token holder (#99).

- Pointing at a mode in the hand menu's **Space** section no longer makes its highlight flicker, so the trigger chooses it every time rather than about half the time. The description of the mode you point at appeared as extra rows beneath it, which made the menu taller and raised every row, so the ray met another row in the next frame, the description went away, and the menu dropped back again. The descriptions now share a fixed block of rows beneath the modes, as many as the longest description needs. It shows the description of the mode you point at, or otherwise of the current mode, so pointing at a row no longer moves any row. No other row, such as an environment, the speech setting or a level in the **Sound** section, added rows when pointed at, and tests now check that their highlight holds steady too.

- The hand menu's rows can now be pointed at along their whole width, to the panel's right edge, where only their first 20 cm could be before, so the end of a long row highlights and chooses it. Each row is now pointed at by its rectangle in the menu's own plane, the same rectangle its marker and label are drawn within, rather than by a box aligned with the world, so that with the menu held low at the chest and tilted back, a steep ray no longer catches the row above the one you aim at, and rows meet without gaps or overlaps. The menu's labels are now drawn in its plane, rather than each turned to face your head on its own. The hover highlight and the light hover tick are unchanged; they now follow where you point.

- A first prototype of the GitHub environment (#148): start the server with `--github-repository OWNER/NAME`, and an owner can change a space to **GitHub** from the hand menu's Space section. The space then shows the repository's open issues and pull requests as cards, with their number, kind, title, author, age, labels and, for a pull request, whether its checks pass, in an inbox front left of you. Point at a card to highlight it, squeeze the right grip to pick it up, carry it along the ray and let go to put it down, or let go over the inbox to put it back. Everyone in the space sees where cards are put, and the server keeps that with the space across restarts. Pointing at a card as you press **X** tells the agent which issue or pull request you mean, before your words, as pointing at a worker does, so "what's this one about?" works; for other clients, a card is a `Resource` of kind `card`, made with `Resource::card`. The server reads GitHub, read-only, with the token in `GITHUB_TOKEN` or from `gh auth token`, about once a minute while someone watches, waiting for the token's rate limit to reset whenever GitHub says it is limiting requests, even in an answer of 200, and never sends the token to headsets, the agent or its logs. Card titles, authors and labels reach the agent marked as untrusted repository content, quoted so that they cannot pass for anything else. Only participants see cards for now, and nothing is changed on GitHub; `design/github.md` describes the prototype and what is left out, such as triage boxes (#149) and physics (#97). Older clients and servers are unaffected.

- A recording with no speech in it, such as a press of **X** by accident, is no longer sent to the agent as a note like "\[BLANK_AUDIO]" or "(static)", which it then had to answer. With `--speech-backend offline`, what Whisper writes for a short message without speech is now left out as it already was for each part of a long one: notes of noise always, and a word or two such as "you" when the Silero detector hears no speech; words the detector hears, even just "yes", are kept. The recording card then says "Not sent: no speech heard", with the not-sent sound and vibration, as it does for a streaming transcript with no words, and the server logs what it left out.

- The hand menu's text is solid at any wrist angle: its labels, tilted to face the head, drew each pixel row too short, leaving stripes, and a background that fell short of the last row. The menu, arrival's labels and the in-space workers' titles now draw the same characters as the transcript, so that "José" no longer shows as "Jos?", and every panel, label and menu row wraps and cuts text by one rule: at words, breaking a word longer than a row, and ending with "…" where text is cut, rather than "..." in some places. Every row of the hand menu now fits its width, 40 characters, including long names, modes, statuses, pairing codes and notices, where some rows were unbounded and a word longer than a row lost its end; a long name, mode or environment is cut so that the rest of its row stays, such as "away" in "Turn … away" and a participant's role, and the names of those here wrap rather than being cut. Letters the font lacks are drawn as their base letter, such as "Łódź" as "Lódz", and typographic quotes, dashes and spaces as their plain equivalents; `?` is left for what needs a glyph atlas, such as Cyrillic, Japanese and emoji. Combining marks, variation selectors and zero-width spaces no longer take a cell, CRLF is one line break, and a lone carriage return, form feed or Unicode line separator breaks a line as a line feed does rather than drawing as a space or `?`, a non-breaking space no longer breaks a line, and a long word in a console line is no longer split with a space (part of #146).

- After you release **X**, the recording card on the left controller now says "Sending…" only until the server has your words, which is usually at once, since the audio streams while you speak. Then it says "Transcribing…", or, with `--speech-backend offline`, "Transcribing, part 2 of 3" for a message long enough to be transcribed in parts, and "Waiting to transcribe" while other words, someone else's or your own earlier ones, are transcribed first, before saying where your words went as before. With a server that does not say, the card says "Sending…" until then, as before. `tomori-server` sends the new `transcript_progress` message only for an utterance whose `utterance_begin` sets `progress`, as clients built on `tomori-client` now do, and only on the connection that asked, so older clients are unaffected, even one you reconnect with while your words are transcribed. It also logs, for each utterance, how long its transcript took after its end arrived, and how much of that it waited for the recognizer.

- Each release now attaches Tomori's headset APK to its GitHub Release, as `tomori-VERSION.apk` with its SHA-256, built in the release profile from the release's commit on `main` and signed with Tomori's release key, which is held in a protected GitHub environment, `android-release`, that only `main` can use. Its certificate's digest is committed in `android/release-certificate.sha256`, and `cargo bake android:apk:verify` checks a download against it. A release APK and a locally built one are the same app signed with different keys, so switching between them means uninstalling first, and pairing again. Tomori's Bake tasks `android:apk:build`, `sign`, `verify` and `release` build the APK unsigned, sign it and check it locally; a release runs them through bake-cargo's release artifacts workflow, signing on a runner of its own, and every pull request does the same with a throwaway key; see `design/android-release.md` (#73).

- The client crates are now ready to publish to crates.io with the others: `tomori-scene`, `tomori-render`, `tomori-client-desktop`, `tomori-client-spectator` and `tomori-client-quest`, each with a readme. Another project's headset can then depend on `tomori-client-quest` from crates.io, and `cargo install tomori-client-quest` and `cargo install tomori-client-spectator` install the `tomori` and `tomori-spectator` executables. Their interfaces change with the environment framework (#75, #77). `cargo publish --workspace --dry-run --locked` packages and builds all fourteen, and the readme's Releasing section gives the order of their first uploads (#56, #73).

- Tomori's headset app is now `com.codeotaku.tomori`, rather than `io.github.ioquatix.tomori`. It installs beside the old app rather than updating it, so pair it with your server again: the old app's identity, last server, certificates and tokens, feedback setting and permissions stay with the old app. Remove the old app when ready, with `adb uninstall io.github.ioquatix.tomori`; see `docs/naming.md`.

- A headset build of another project's is now packaged as an APK of its own, with its own application ID, APK name, label, icon and assets, without changing Tomori: it has its own copy of `tomori-client-quest`'s `[package.metadata.android]`, and `bash scripts/android.sh --package NAME build` (or `run`) builds it, reading the application ID, the APK's name and the OpenXR loader's folder from its manifest. `scripts/android.sh path` prints where the APK is written, and `scripts/fetch-loader.sh` takes the folder to fetch the loader into. The guide is `crates/client-quest/context/android.md`. `examples/headset` is packaged as `com.example.tomori.headset` on every pull request (#73).

- Releases now start with one command, `cargo bake cargo:release:patch` (or `minor`, `major`, or `bump --version X.Y.Z`), which creates the release branch, bumps the version, writes the release notes and the `Release:` trailer, validates the candidate and opens the pull request; `cargo bake cargo:version:*` no longer exists. A crate's first upload is `cargo bake cargo:bootstrap PACKAGE`. This uses Base's `base-project` 0.1.7 and `bake-cargo` 0.7.0.

- `tomori-environment-sky`, the Sky environment's renderer, is now ready to publish to crates.io with the other crates, as `tomori-render` depends on it to draw Sky as Tomori's own scenery. Nothing changes for people using Tomori (#56).

- Another project can now build a headset of its own on the `tomori` library (package `tomori-client-quest`), without forking Tomori: `tomori::Builder::default()` is Tomori's own build, `.scenery(id, create)` draws a scenery of its own, any implementation of `tomori-render`'s new `Scenery`, for a space bound to the environment `id`, and `.run_android(app)`, `.run_xr(overrides)` or `tomori::main(builder)` runs it on the Quest, a desktop OpenXR runtime or with `tomori`'s command line, whose desktop window's **E** cycles through its sceneries. A build for the Quest writes its own `android_main`, depending on `tomori-client-quest` without its default `app` feature, which holds Tomori's own. The `tomori` executable and APK are that builder's default build, and behave as before. Environments are now named by their identifiers in the client, so a build that draws an environment shows it, where Tomori's own shows the grid and names it. `examples/headset` is an example: it draws a placeholder floor for `examples/server`'s placeholder environment. Environments' client halves will be registered through the same builder once Tomori has them (#73, #75).

- The spectator has its own executable, `tomori-spectator`, in the new `tomori-client-spectator` package: run `tomori-spectator` for its window, `tomori-spectator --pipewire` for the PipeWire video source and `--pipewire-preview` for both, with `--server`, `--token`, `--space`, `--follow` and `--audio-output` as before; `tomori-spectator --help` lists them. `docs/slack-huddle.md` and the readme's OBS instructions use it. `tomori --spectator`, `--spectator-pipewire` and `--spectator-pipewire-preview` still work as before. The desktop modes, the server-less window, `--desktop`, `--console` and `discover`, are now the `tomori-client-desktop` crate, which the `tomori` executable runs unchanged. Neither crate is published yet (#56).

- The renderers are now their own crate, `tomori-render`, in `crates/render`: the swarm, arrival, workers' and panels' renderers and their shaders, the controllers' models with their decoding, and the drawing of each frame, shared by the headset, the desktop window and the spectator. The controller models and their licence notices moved with them, to `crates/render/assets`, and the APK still carries the same notices. Nothing changes for people using Tomori. `tomori-render` is not published yet (#56).

- The headset and desktop application's platform-neutral logic is now its own crate, `tomori-scene`, in `crates/scene`: the frame step that the headset and the desktop window share with each platform's own logic, the conversation and its connection, arrival, the hand menu, the transcript, pointing, the swarm and the workers, feedback and spatial audio logic, and the panels' layout. It depends on no rendering, windowing or platform audio crate, which a test checks for the host and the headset, and CI builds it for Android on its own. The renderers, the desktop's cpal streams (`cpal_audio.rs`) and the headset's AAudio streams (`android_audio.rs`) stay in `tomori-client-quest`. Nothing changes for people using Tomori. `tomori-scene` is not published yet (#56).

- When GStreamer exits while the PipeWire spectator is writing a frame to it, the spectator now says how GStreamer exited, as it does between frames, rather than only that its video writer disconnected.

- The headset and desktop application's package is renamed from `tomori-quest` to `tomori-client-quest`, so that each client is named `tomori-client-<target>` after its folder, as the other crates are. Cargo commands name it, such as `cargo run -p tomori-client-quest` and `cargo apk build -p tomori-client-quest --lib`. Its library and executable are still `tomori`, and the Android application ID and `tomori.apk` are unchanged. This is the first step of splitting it into the scene, rendering and per-client crates planned in #132 (#56).

- Pull requests must now keep the workspace at 100% source-region coverage. The Test workflow checks it on stable Rust, and the main branch requires it through `test-result`. The only exceptions are seven thin files of headset, window, audio, child-process, process-exit and multicast DNS calls that CI cannot make, each excluded in its crate's `Cargo.toml` with its reason. `design/testing.md` records the requirement. Reaching it found and fixed bugs in Markdown hard breaks and task markers, `tomori discover`'s participant counts, the pause between spoken segments, and the fitting of snapshots and listings to their budgets (#55).

- Another project can now build a Tomori server of its own on the `tomori-server` library, without forking Tomori: `tomori_server::Server::builder()` starts with Tomori's own backends and environments, `.backend(name, create)` offers any `tomori_backend::Backend` under `--backend NAME`, `.environment(Environment::new(id, label).with_instructions(text))` offers an environment, with its layer of the agent's instructions, after Sky and Grid, `.configure(hook)` adjusts the configured `Server`, and `.main()` runs the same command line as `tomori-server`, with tokens, TLS, Bonjour and speech. The `tomori-server` executable is now that builder's default build; its command line, output and behaviour are unchanged. `Server::with_environments` offers environments to a server built without the command line, and `Backends::with` takes a backend of any type. `examples/server` is an example, with a placeholder backend and environment, and the Building a Tomori Server guide in `crates/server/context/` explains it. Data sources, tools and environments' server halves will be registered through the same builder once Tomori has them (#73).

- An OpenCode session with a very large or deep family of sub-agents, too many to list within the 5 seconds a space has to read its session, now opens rather than failing every time. If the conversation, approvals and questions were read in time, the space opens with the sub-agents found so far, says that some may be missing, and finds the rest in the background, after which the list is complete again. Changes to sub-agents meanwhile are still shown, and clients do not take a sub-agent missing from the list for one that left. A sub-agent leaving is shown once the list is complete again. If OpenCode stops answering, the space says it could not find every sub-agent and tries again after about 5 seconds, 30 seconds and 2 minutes; if those fail too, the list stays incomplete until the session is read again, as after a reconnection. A conversation that cannot be read in time still stops the space opening, as before (#45).

- A worker that a delegated worker started now returns to that worker's station when it is deleted, as it arrived from there, rather than to the agent at the centre; if its parent leaves at the same time, or is not shown, it returns to the agent as before (#49). When the server has to leave a parent out of a space's worker list to fit its snapshot, the headset and the spectator keep showing the states of that parent's workers, and their new workers, instead of showing them as unknown (#48).

- An owner can change a space's environment after creating it, from the hand menu's **Space** section, under "Environment, for everyone": every environment the server offers is listed, the current one marked, and choosing another moves everyone in the space, participants, visitors and spectators, to it, with the new scenery growing out of the space as it does on joining. Someone joining later arrives in it, and the server keeps it across a restart. Collaborators and visitors see only the environment's name. In `--console`, `environment ID` does the same. The server says in the space state, as `environment_changes`, that it accepts the new `set_environment` message, which it refuses from anyone but an owner and for an environment it does not offer; older servers and clients are unaffected, though an older client shows a changed environment only when it next joins. The server now offers **Grid**, the Minimal floor grid, alongside Sky. Sky stays first, so a client that names no environment still creates a space in it, and the headset's new environment ring, shown on creating a space now that there are two, starts on it. The desktop window's **E** still switches the scenery only without a server.

- A sub-agent deleted while Tomori was reconnecting to OpenCode now leaves its space once the space is read again, rather than staying until the space is attached again. It is removed only when OpenCode confirms that it no longer exists.

- An OpenCode space no longer repeats a reply that finished while it was opening. Its events wait for its initial state and follow it, except those the state already shows. Text from the OpenCode event stream with a character split between two reads, such as Japanese or an emoji, is no longer garbled. An event larger than 16 MiB now makes the backend reconnect and read each space again, rather than growing without limit.

- A request OpenCode is slow to answer no longer holds up every OpenCode space. Each space's own requests still reach OpenCode in order, but other spaces keep receiving the agent's replies and sending their own, and new spaces still open. Closing a space no longer waits for its slow request.

- `tomori-server` no longer presents an approval or question whose identifier is longer than 1 KiB, which no client could name in its reply and which could push a space's snapshot over its budget. The space says to answer it in the agent's own interface instead, and later requests are presented as usual. An approval's "don't ask again" option with such an identifier is not offered (#50).

- When a space has so many workers that its snapshot must leave some out, `tomori-server` now leaves out the oldest, as `design/server.md` says, rather than those whose identifiers sort first: a space keeps its workers in the order it first saw them, and the OpenCode backend lists an attached session's workers in the order they were created. The list is still marked partial, so clients don't take the omission for a departure. The headset and the spectator, which show at most 64 workers, likewise show the newest 64 rather than the first 64 listed (#51).

- `tomori-server --speech-backend offline --speech-offline-model tiny.en` uses Whisper `tiny.en`, which is smaller and faster than the default, `small.en`, and less accurate; fetch it with `bash scripts/fetch-asr-model.sh whisper tiny.en`. The model scripts now check installed models against pinned digests every time they run, and fetch again any that do not match.

- The crates are renamed with a `tomori-` prefix, so that they can be published to crates.io: `tomori-protocol`, `tomori-client`, `tomori-server`, `tomori-backend`, `tomori-backend-opencode`, `tomori-security`, `tomori-discovery`, `tomori-speech` and `tomori-environment-sky`, and the headset and desktop application is `tomori-quest`. Rust paths change to match, such as `tomori_protocol::ClientMessage`, and Cargo commands name the new packages, such as `cargo run -p tomori-quest` and `cargo build -p tomori-server`. The `tomori-server` and `tomori` executables, the `tomori` library, the Android application ID and `tomori.apk` keep their names, and the folders under `crates/` keep theirs. The crates now share one version, set in the root `Cargo.toml`. All but `tomori-quest` and `tomori-environment-sky` are ready to publish, with readmes and agent context for projects that use them, but none has been published yet; see `design/crates.md`.

- Voices now come from where they are. The headset, the desktop window and the spectator play in stereo: the agent's voice comes from the swarm and follows it, and every other participant has a seat on a circle around you, sharing it with the agent ahead, so nobody sounds as if they stood inside your head. When they move, their voice moves with them, within a metre of their seat. Turning your head moves the sound, and a voice moving to a new seat sweeps there. Sound cues come from the hand that acted, or from the swarm when the agent needs you. The agent's voice dips by 8 dB while someone speaks and comes back when they stop. **Mono audio** in the hand menu's Sound section, or `feedback mono on` in `--console`, plays everything in both ears alike.

- While the live microphone is on, the headset now asks Android for its echo canceller, and plays as voice communication in the microphone's audio session, so the canceller can work. Its log says whether the device has one and whether it is enabled. Start it with `-e tomori.echo_test on`, then `canceller-off` and `uncancelled`, to measure how much of the speaker the live microphone hears with the canceller, without it, and with the recognition preset that push-to-talk uses. Until that is measured, use headphones or turn the volume down if you hear feedback.

- The headset now gives feedback you can hear and feel. The controllers vibrate, on the hand that acted: a very light tick as the ray moves onto something, a tap when you choose it, taps on the left controller as **X** starts and stops recording and as the live microphone turns on or off, a ramp while you hold **B** to leave, three taps on the right when an approval or question arrives, and a tap at 30 and two at 10 seconds before the recording limit, as the recording card turns amber and red. Moving the focus on the right wrist's transcript ticks like pointing, and choosing there taps; the transcript opening, or **Y** pinning the hand menu, gives one very light tick. Short, quiet sound cues go with them: a click when you choose, a tick as recording starts, two rising notes when your words are sent, two falling notes when something fails, a chime when a request waits (and once more, quieter, if it still waits a minute later), two soft knocks when someone knocks, and a soft swell when a catch-up starts. The sounds are synthesized, with no audio files. The hand menu's new **Sound** section, after People & security, sets **Sounds** and **Vibration** separately to full, reduced (requests, failures and knocks only) or off, kept on the device; in `--console`, `feedback sounds|vibration full|reduced|off` does the same. The desktop window plays the sounds, and the spectator plays none. The headset's log says whether the runtime offers Meta's richer haptic extensions.

- The panel in front of you in a space is now a small status pill low in your view: what the agent is doing, its mode and the space's title, and, only when something needs you, a second row such as "Approval waiting: turn right wrist", a problem, or that you are offline. It no longer drops when the swarm is busy. Turn it off in the hand menu's new **Space** section.

- The hand menu starts with where you are inside a space: the title, what the agent is doing, its mode and how many are here. Its new **Space** section switches the agent's mode, with each mode's description when you point at it, and the space's speech, mutes spoken replies for you, turns the status pill on or off, and leaves the space once you confirm. People & security shows each person with a swatch of their colour, as in the transcript, with "You" and who is away. Press **Y** in a space to pin the menu open at Space without turning your hand, and **Y** or **B** to let it go. The left stick no longer pages a panel in a space. On the desktop window, **Tab** pins the menu; in `--console`, `s` shows the Space section.

- In a space, turn your right palm up, within about 45° of straight up, to see the conversation on your right wrist: each person's turns under their name with a bar in their colour, the same colour on everyone's headset, the agent in cyan, and "You" for yours; the agent's tools as a row of chips with a tick when done; and replies styled from their Markdown, with headings, bullets, bold and code, and none of the `**`, `#` or backticks. A long turn is cut at 6 rows: press **A** on it to read it all. An approval or question is a card pinned below, with a button for each choice, *Allow once*, each "don't ask again" option in full and *Deny*: move down to one with the right stick and press **A**. The queue is pinned below that: move to an entry and press **A** to remove it, and **A** again within 3 seconds to keep it. **B** backs out, and holding it still leaves. Squeeze the right grip to leave the transcript standing in the world, to read it with your arm down, and squeeze again or press **B** to take it back. A glow on your right wrist says something is waiting there. Approvals and the queue are now handled only there on the headset; the right stick otherwise pages result cards. On the desktop window, **T** turns the wrist up and **G** squeezes the grip; in `--console`, `t` shows the transcript and `tu`, `td`, `tl`, `tr`, `ta` and `tb` use it, and `^` and `v` select a queued entry on the panel.

- While you hold **X** to talk, a small card rises from the left controller, where X is, instead of a timer floating above the panel: it shows how long you have been talking against the server's limit, with a bar, the last words heard, and "Release X to send". In the last 30 seconds before the limit it turns amber and says how long is left, and red in the last 10. When you let go it says "Sending…", then where your words went, such as "Queued, 2nd", or "Not sent" with why until you press **X** again or dismiss it with **B**. While the live microphone is on, a small `LIVE` badge stands on the left controller. Without the controller tracked, the card follows your left wrist, or stands low in your view.

- `tomori-server` tells clients how long a voice message may be, in its `Welcome`, as `utterance_limit` in seconds: 5 minutes with `--speech-backend offline`, and none with the streaming backend.

- The panel and the new controller cards are drawn in the world with the rest of the scene, translucent, so that a hand or controller in front of them covers them. The cards' text can show accented letters, Greek, box drawing and a few symbols, rather than `?`.

- The headset draws with 4× multisample anti-aliasing, as desktop windows do, so edges are smooth instead of jagged. The samples stay in the GPU's tile memory and are resolved as each eye is drawn. Start it with the intent extra `tomori.msaa` set to `off` to compare, or if it costs frames.

- Point at a worker and ask about it, as an early prototype of shared attention (#81). In a space, the right controller's ray, or the desktop's mouse, highlights the worker it points at. The headset draws the ray in a space as in arrival: it reaches the worker it highlights, and is short and plain when it meets none. Point at one as you press **X**, or while you talk, and your words reach the agent with a line naming that worker, its session and its state, such as `[Pointing at worker "Review commit 2657a52" (opencode session ses_…, running)]`; pointing only before you press **X** doesn't count. The server describes the worker from its own state, never from what the headset says. The trigger still attracts the swarm. In `--console`, `point` lists the workers and `point N` points at one. Older servers ignore the pointing, and older clients send none.

- Tomori now gives the agent instructions of its own in each space, so that it answers briefly in plain speech and hands non-trivial work to sub-agents instead of doing it inline. They come in three layers: Tomori's own, the environment's (none for Sky yet) and the space's, which an owner can give when creating a space and change later. With OpenCode they are set as the session's instruction entries, never in your configuration or `AGENTS.md`, and are removed when the server forgets the space. Everyone in a space can read them in the hand menu's new **Agent** section, where an owner can also clear the space's own. In `--console`, `i` shows them, and `instruct TEXT` gives the space you are in, or the next one you create, its own instructions. If OpenCode refuses them, the space still works and says why.

- As the user asked in November 2026, the agent keeps the workspace clean of finished sub-agents: Tomori's instructions ask it to remove a sub-agent once it has finished and its findings are reported, unless someone asks to keep it.

- In a space, **B** never denies an approval, cancels your words or leaves by accident: it closes what is open or dismisses a problem, and you hold it for a second to leave. *Deny* is one of the approval's choices on the right stick, confirmed with **A**. Select any waiting entry in the queue strip with the right stick up and down, and press **A** to cancel it; **A** again within 3 seconds undoes it.

- Errors get their own line on the panel, marked **!** with how long ago they happened, and stay until dismissed or replaced instead of hiding behind "Sending…" or vanishing on the next action. The queue is always shown in its own strip.

- **Y** opens a list of the agent's modes with their descriptions, and the space's brief or full speech, and **A** chooses one directly instead of cycling through them.

- Hold **X** while a question is shown to answer it by voice, with an option's label or number; the words go to the question, not the queue.

- The panel stands in the world in front of you instead of following your head, and glides back in front of you only when you turn or walk away for more than a second.

- A space keeps its spoken replies while nobody is listening, and plays the latest one from its start when you join or put your headset back on, instead of joining mid-sentence; then whatever followed, until you have caught up with live speech. It keeps the latest two replies and at most 5 minutes of audio. Others in the space are unaffected, and nobody hears a reply twice at once. Older clients and the spectator stay with live speech as before.

- Click the right stick in a space to hear the latest reply again from its start, to you only, and again to stop it; a new reply stops it and plays live. Click the left stick to mute spoken replies for you only. In `--console`, `replay` and `mute` do the same.

- The headset tells the server when it is taken off or put back on, from its proximity sensor where the runtime reports it and from the OpenXR session state, and turns the live microphone off when it is taken off. Everyone in the space sees who is away, as "2 here, 1 away".

- Replies are spoken in full by default. A space can be switched to brief speech from the **Y** list or with `set_speech`: each message then speaks whole paragraphs up to about 20 seconds, and a reply cut short says "More on the panel." once. A space chosen brief stays brief across a restart; every other space, including one kept before, speaks in full. A closed space keeps no speech.

- The headset shows which servers and spaces need you. A server whose spaces wait for an approval or answer glows amber, one with a problem nobody saw glows red, and one with a reply nobody heard glows a soft white, and its label says what is waiting, such as "1 needs you, 2 replies". A small jump object above it connects and joins the space that needs you most in one trigger pull: the highest level, then the one waiting longest. If someone dealt with it meanwhile, you stay in the server's lobby and are told so. In the lobby, spaces keep their places, are marked and labelled with what they need, and the selection starts on the most urgent one. The remembered server is now connected a moment, about one and a half seconds, after it is found, and not at all if another server found by then needs you and it doesn't. Attention is for anyone: whoever joins a space or answers its request clears it for everyone, and being in a space when something happens counts as having seen it. The lobby listing carries each space's attention and how long it has waited, in optional fields. `tomori-server` advertises how many spaces need input, have a problem and have a reply in its Bonjour TXT record, as `waiting`, `problems` and `replies`; `--no-advertise-attention` leaves just those out.

- The crates need Rust 1.90 or later rather than 1.93: wgpu-hal's dependency on ordered-float 5.5 needs 1.90, and nothing in Tomori itself needs more than 1.88. GitHub Actions now builds and tests every pull request on stable Rust and on 1.90, and builds the headset APK.

- `tomori-server` keeps serving everyone else, and keeps releasing silent connections, while one client sends faster than the server can apply its input, instead of stalling until that client pauses. The counts it advertises also include a client that connects just as it starts.

- `tomori-server` no longer has a Claude Code backend, nor its `--claude-permission-mode` option: use `--backend opencode`, which can use Claude models too and restores a space's conversation and pending approvals when the server restarts.

- `tomori-server` keeps its spaces across restarts, in `spaces.json` in its state directory: each space is listed again as soon as the server starts and bound to its agent session again, so the headset returns to the same space and collaborators stay admitted. A space whose session cannot be resumed is listed as closed with the reason. Input still queued when the server stops is dropped.

- A visitor can knock to take part in the space they watch, and its participants let them in or turn them away; someone let in becomes a collaborator in that space.

- Turn your left palm up, or roll the left controller palm up, within about 45° of straight up, to show the hand menu above your wrist, as the right palm shows the transcript: its People & security section shows who is here, how many visitors are watching, knocks waiting, pairing and notices, and flashes when someone knocks.

- Pair the headset with a server to make it an owner, without copying a token: choose the gold key beside the connected server, and confirm the 6-digit code it shows by typing it on the server's console, or on the owners' status page. Pairing also restores trust in a server whose certificate changed. In `--console`, `p` pairs.

- `tomori-server` admits by token: holders of its tokens are owners, who create spaces, or collaborators, and anyone else may watch and listen as a visitor, unless `--no-visitors`. Make the first owner token with `tomori-server auth mint --owner --name NAME`, and give it to the headset with `TOMORI_TOKEN` (the `tomori.token` intent extra) or to the desktop with `--token`; `auth list` and `auth revoke` manage them. `--auth open` admits everyone as an owner, as before.

- `tomori-server` serves only TLS when it admits by token, with a self-signed certificate kept in its state directory (`~/.local/state/tomori` by default) and advertised by fingerprint; clients pin it the first time they connect and send their token to no other certificate. Owners sign in with a token at `https://<server>:8765/` to see the server's spaces, who is in them and its tokens, and to revoke one.

- The headset offers **New space** only to owners, and a visitor watches a space without talking, live audio or presence.

- Other participants can no longer take over your place in a space: the server no longer sends anyone's participant identity to other clients, and names participants in a space and its queue by their number instead. The headset chooses a new identity once, since its old one was shared. This changes the protocol to version 2, so update the headset and the server together.

- `tomori-server` survives an OpenCode service restart or a dropped event stream instead of closing every space and refusing new ones until it is restarted. It finds the service again, including a new port and password from `service.json`, and reconnects with a backoff of up to 10 seconds. Requests made while OpenCode is down fail within seconds, and creating a space works again as soon as it returns. Open spaces show that the connection was lost, hold queued input, and refuse decisions and mode changes with a message until each is read again from its session, so a permission or question raised during the gap still appears. Shutting down no longer waits for requests held while OpenCode is unreachable or for one OpenCode has not answered. A reply that was streaming when OpenCode's event stream dropped keeps the text already shown, and is shown in full once it ends. A space whose session was deleted, or that cannot be read again, closes and says why. Nothing said during the gap is spoken, and a worker deleted during the gap stays listed until the space is attached again.

- A space that closes now always shows why, rather than sometimes reporting only that its event channel closed.

- `--speech-backend offline` transcribes voice messages of up to 5 minutes instead of 30 seconds. A message longer than Whisper's 30 second window is split at its pauses, heard by the Silero voice activity detector, or by loudness when it is not installed, and the parts are transcribed in order by the same model, so a long message is as accurate as a short one; what Whisper writes for a long pause is left out: a note such as "\[BLANK_AUDIO]" always, and a word or two such as "you" when the detector is installed and hears no speech there. A message longer than 5 minutes fails as soon as it passes the limit, saying so, and nothing is sent. While one participant's long message is transcribed, another's waits for at most one 28 second part of it. Run `bash scripts/fetch-asr-model.sh whisper` again to fetch the detector.

- The headset logs much less to logcat, so connections, joins, utterances and errors stay in its buffer for many minutes. It logs its first submitted frame and then a frame count and rate once a minute instead of every 720 frames. Swarm interaction modes, pending reference-space changes and controller pose changes are logged only at debug. Repeated recenters are logged at info at most once a minute, and live microphone overruns at most once every 30 seconds, with the overruns counted in between. Speaker underruns on the desktop are logged only at debug.

- Offer only top-level OpenCode sessions when attaching a space, not subagents' child sessions or archived sessions.

- The server, client and OpenCode backend use the Native scheduler instead of the Direct scheduler. An idle server no longer wakes every 2 ms to retry its sockets, and accepting connections can no longer stall on macOS once the Direct blocking pool stops waking its workers.

- Track OpenCode child lifecycle events so deleted workers animate their return without mistaking completion or partial lists for deletion.

- Document the direct 1080p virtual camera and dedicated audio input used to share a Tomori space in a Slack Huddle without OBS.

- Toggle the Quest microphone for live conversation with a click of the left stick; hold X to address the agent and release it to return to the previous live setting. Live capture stops on focus loss or disconnection. Echo cancellation is not guaranteed.

- The spectator renders an unobstructed view without the headset HUD, animates the swarm from the space's activity, and publishes 1080p PipeWire video instead of 4K.

- Release Tomori under the MIT License only, replacing the MIT or Apache-2.0 dual license.

- The headset joins spaces on a Tomori server instead of chatting through the PC bridge: it lists servers found with Bonjour and the USB connection, reconnects to the last server when it finds it again, then lists the server's spaces. It reconnects and rejoins after a dropped connection, says when a space has ended, as after the server restarts, and answers approvals with the right stick and **A**.

- Choose the server and display name with the intent extras `tomori.server` and `tomori.name`, `TOMORI_SERVER=host:port bash scripts/android.sh run` (with `TOMORI_SERVER_PORT` for the port forwarded over USB), or `--server` and `--name` on the desktop.

- Add `--console`, which shows the headset's panel in the terminal for using a server without a headset or a GPU.

- Remove the PC bridge's voice path, spoken replies, voice preview, replay, result cards and spectator publishing from the headset for now.

- Hold **X** in a space to talk to its agent through the server: the headset sends push-to-talk audio over its server connection, shows the words recognized so far and the microphone's state on the panel, and the server transcribes them and queues the text. Lost audio, a dropped connection, nothing heard or a full queue fails visibly instead of sending anything. In `--console`, `x` starts talking through the computer's default microphone and `x` again sends it.

- Speech recognition and synthesis run inside `tomori-server` through sherpa-onnx instead of Python workers, so the server no longer needs Python for speech. Recognition streams: a streaming transducer shows the words recognized so far while you speak, and the transcript is ready about a tenth of a second after you let go. Transcripts are now lowercase, with no punctuation. Fetch the sherpa-onnx libraries, checked against a pinned checksum, with `bash scripts/fetch-sherpa-onnx.sh` before building the server, and the models with `bash scripts/fetch-asr-model.sh` and `bash scripts/fetch-tts-model.sh`, then run `tomori-server --speech-root .`, on macOS or Linux. Recognition that falls more than 5 seconds behind fails the utterance visibly. If recognition or synthesis stops after an internal error, participants are told it is unavailable until the server restarts. `--speech-provider cuda` needs a server built with the `speech-shared` feature. The PC bridge's transcription still uses `scripts/setup-pc-transcription.sh`.

- `tomori-server` keeps serving after transient accept errors, such as running out of file descriptors, instead of silently leaving its port open with nobody accepting. When serving does end it refuses new clients at once, waits at most 5 seconds for open connections, then at most 3 seconds for the speech engines to stop, and exits with a failure status if serving failed. It now logs to standard error, controlled by `RUST_LOG`.

- `tomori-server` speaks each paragraph of a reply once it has ended, rather than sentence by sentence while it is still arriving, and speaks a list once it ends. A paragraph that leads straight into a list or code block without a blank line is spoken once that list or code block has ended. A line that turns into a table header or heading is no longer spoken first as a paragraph.

- Each space is bound to one environment, chosen when it is created and the same for everyone in it. The server offers its environments, today only Sky, and refuses one it doesn't offer. The headset waits on the Minimal grid outside a space, shows the space's environment inside it, and returns to the grid when it leaves; squeezing the right grip no longer switches environments. A space in an environment the headset doesn't know shows the grid and names the environment. `TOMORI_ENVIRONMENT` and **E** now apply only to the desktop window.

<!-- bake-readme:releases:end -->

## See Also

- [tomori](https://github.com/base-rs/tomori) — A conversational spatial workspace for people and agents, built with OpenXR and wgpu<!-- bake-readme:package -->

## Contributing

Please open an issue or pull request on [GitHub](https://github.com/base-rs/tomori).

### Agent Context

Run `cargo bake agent:context:install` to install shared context and skills. Use `cargo bake agent:context:list` and `cargo bake agent:context:show` to discover relevant guides, follow `agents.md` if present, and apply skills under `.agents/skills/`. The installer preserves repository-owned `agents.md`; it does not create or regenerate that file.

Tomori's published crates provide context of their own for projects that depend on them, in [crates/protocol/context/](crates/protocol/context/), [crates/client/context/](crates/client/context/) and [crates/server/context/](crates/server/context/).
