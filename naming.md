# Naming

**Tomori** (`ともり`, with `灯` as a light-associated name spelling) is the
name of both the system and its conversational coordinator. One cohesive name
connects the shared workspace to the presence you speak with inside it.

The lamplight association suggests a steady central presence: a light to gather
around. The intended spelling is **Tomori**, not "Tamori".

## Theme

A Japanese-inspired visual direction places a small shrine near the central
coordinator, with floating clouds, a night sky and a distant moon. The initial
**Sky** environment implements a procedural blockout of that direction with a
fixed moonlit sky. Detailed architecture and a cycle in a chosen timezone remain
future design options, not implemented features.

## crates.io

Read-only checks of the crates.io API on 2026-10-28 returned HTTP 404 for
[`tomori`](https://crates.io/api/v1/crates/tomori): no registered crate was
found at the time of checking. The name has not been reserved or published there.
Recheck before publication; this is not a trademark or wider availability check.

## Scope

The public repository uses `ioquatix/tomori`. Technical names now match:

- Rust package: `client-quest` (in `crates/client-quest`); library and executable: `tomori`.
- Android application ID: `io.github.ioquatix.tomori`; label: **Tomori**;
  APK: `tomori.apk`.
- Node bridge package: `tomori-opencode-bridge`.
- Environment variables: `TOMORI_*`.
- PipeWire video: `tomori-spectator` (**Tomori-Spectator**).
- PipeWire audio: `tomori-quest-microphone` and `tomori-assistant-voice`.

### Migrating from the prototype name

Earlier builds used `base-agent-swarm` and Android ID `dev.base.agentswarm`.
The new Android ID installs alongside the old app rather than updating it;
app data and permissions are separate. Remove the old app only when ready.

Rename any local `BASE_SWARM_*` environment exports to `TOMORI_*` (including
those in `.android-env` or service definitions); the old prefix is no longer
read. Stop old services normally, then rebuild/restart with the new names.
Ports and HTTP routes are unchanged.

Reselect the renamed video/audio sources in OBS or other PipeWire consumers.
The bundled OBS compatibility patch now matches `tomori-spectator`; a plugin
using the earlier exact-name patch needs rebuilding, or use the documented
manual target/link method. The separate plugin fork's generic **Video/Source**
discovery option needs no name-specific change.

Name-reading reference: [Weblio's entry for 灯](https://ejje.weblio.jp/content/灯).
