# environment-sky

Tomori's first concrete environment: a stationary moonlit sky, sparse stars,
cloud banks, a platform, an open shrine blockout and warm emissive lanterns.
Original procedural geometry and shaders; no external assets or downloads.
Clouds are a bounded translucent density volume, not opaque surface meshes.

## Boundary

This crate depends only on wgpu, glam and bytemuck. It knows nothing about
OpenXR sessions, agent simulation, worker layout, chat, controller input or
networking. `Sky` owns static GPU resources, not the application state.

The application owns the Sky/Minimal selector and transmits the selected preset
to spectators. This first preset is fixed and deterministic: no device-local
clock, random seed or animation phase needs synchronizing yet.

## Rendering contract

1. Create `Sky::new(device, queue, color_format, width, height)` once per
   device/target format. Call `resize` when the render target dimensions change.
2. Before drawing, call `prepare_eye(queue, eye, scene_to_clip)` with the full
   projection × view × scene-anchor transform. Eye indices are 0 or 1.
3. Call `encode_clouds(encoder, eye)` to render that eye's low-resolution cloud
   texture, before beginning the main render pass.
4. Call `draw(pass, eye)` before application content in a pass with a cleared
   `Depth32Float` depth attachment, matching color format and sample count 1.
5. Rebind application pipelines/bindings/buffers and render opaque content.
6. Call `draw_clouds(pass, eye)` to composite premultiplied-alpha clouds against
   full-resolution depth. Draw transparent workspace labels afterwards.

Each eye has its own camera buffer. The sky uses reconstructed directions,
not eye position, so head translation does not drag the moon/stars. Scenery uses
ordinary perspective/depth and follows the application's recenter transform.
The sky/clouds do not write depth; the platform and shrine do. Cloud composition
tests the volume entry against opaque depth. This works for the existing scene,
whose opaque objects are above the cloud slab. Placing opaque objects inside
the volume would require sampled scene depth to terminate each density ray.

Coordinates are in metres, matching Tomori's existing scene: floor `y=0`,
coordinator work volume centred near `(0, 1.35, -2.5)`. The deck top is just below
the floor; the shrine stands behind the coordinator; clouds stay below the
working area. These are visual surfaces, not collision or locomotion geometry.

## Cloud technique and budget

The static scenery mesh is built/uploaded once. A periodic 64³ shape/detail
noise texture (Perlin-style value noise plus Worley noise) is **baked at build
time**, not generated on the headset. Both eyes sample the same world-space
density field below `y=-0.8`; there is no per-eye randomness or animation clock.

Each eye integrates Beer–Lambert extinction and approximate moonlit scattering
with at most **32 ray steps**, a bounded 85 m ray segment and early termination
for dense clouds. Lighting uses one additional density sample per occupied step,
not a nested shadow march. The volume is rendered at half resolution, capped
at 960 pixels per axis, then linearly upsampled and alpha blended. Each eye has
separate camera/image resources. No temporal jitter/history or reprojection is
used, avoiding temporal ghosting at the cost of softer detail.

The environment uses four draw calls per eye (sky, scenery, low-resolution
volume, cloud composite). This is a bounded prototype, not a claim of a measured
Quest frame-time budget. No dynamic shadow maps, day/night simulation, plugin
loading or fly-through-clouds locomotion is implemented.

## Moon shadows

The static scenery casts and receives moon shadows using a single **1024²
Depth32Float shadow map** (4 MiB). `Sky::new` renders it once from a fixed,
scene-space orthographic light camera. Both eyes share the same map. Camera
motion, recentering, environment switches and cloud-target resize do not rebuild
it; changing the immutable geometry or moon would require regenerating the map.

Scenery shading uses a 3×3 percentage-closer filter with a linear comparison
sampler, plus small raster/receiver bias and receiver-plane slope compensation
to reduce self-shadow artifacts without pulling shadows away from their bases. This softens
edges with a fixed filter width; it is not a physically varying penumbra model.
Only direct moonlight is occluded: ambient lighting and emissive lanterns remain.
The cache adds one initialization pass, not a shadow-generation pass per frame.

This does not add dynamic-agent shadows, runtime lantern shadow maps,
cloud-cast/received scene shadows or changes to status/label rendering.

## Baked AO and lantern illumination

`build.rs` bakes a **1024² RGBA8 linear lightmap** (4 MiB): RGB stores occluded
warm lantern irradiance; alpha stores local ambient visibility. Each cuboid face
has a unique UV chart and two-texel gutters. The deck has a 508² interior chart
(~2 cm texels), baked at ~4 cm sample spacing and upsampled; other faces use 44²
charts baked from 22² samples.

The baker traces against the exact oriented cuboids used by the mesh. AO uses
24 cosine-weighted hemisphere rays within 0.9 m; lantern lighting uses distance
falloff, surface orientation and 16 emitter samples with visibility tests.
This is static direct illumination plus AO, not multi-bounce global illumination.
It rebuilds when geometry or bake parameters change, with no ray queries on Quest.

At runtime one filtered lightmap lookup adds lantern illumination and attenuates
ambient light. Direct moonlight retains its shadow map; lantern emission is not
dimmed by AO. The brighter lanterns illuminate nearby static deck/posts, not moving
hands, agents or workspace labels. No extra per-frame light/shadow draw passes
are needed. Regenerate the bake if the lanterns/scenery become movable.

Posts and lantern pedestals meet the deck at `y=-0.01`; AO is not used to conceal
a geometry gap. Sky/horizon/cloud lighting is darker to preserve the warm-light
contrast; Minimal and application status/label styling are unchanged.

## Tests

```sh
cargo test -p environment-sky --locked
cargo test -p environment-sky --locked -- --ignored
```

CPU tests check bounded, finite, deterministic geometry and an unobstructed
central work volume, periodic noise and render-target size bounds. A Vulkan
cloud regression checks partial alpha, premultiplication, per-eye parallax,
repeatability, foreground depth occlusion and resize. Tomori additionally renders stereo,
switches Sky/Minimal without changing the swarm, and checks recenter compensation
and translation-invariant sky rendering; see the root README.
Shadow tests additionally check caster/frustum coverage, light-depth direction,
actual localized cast-shadow darkening, unchanged lantern emission and stable
results after camera movement/resize, plus visible lantern illumination and AO
versus a neutral bake. CPU tests cover chart separation, exact base/deck contact,
reproducible bakes, local occlusion and visibility-blocked lantern illumination.
For an optional unshadowed/shadowed preview:

```sh
TOMORI_SHADOW_PREVIEW=/tmp/opencode/tomori-shadows.png cargo test -p environment-sky \
  --locked cached_moon_shadows -- --ignored
```
