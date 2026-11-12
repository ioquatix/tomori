struct Camera {
    // From the panel's plane to clip space.
    transform: mat4x4<f32>,
    // From the panel's plane to the space the eye is in.
    model: mat4x4<f32>,
    // The scene's haze: its colour, and its density in its fourth component.
    haze: vec4<f32>,
    // Where the eye is.
    eye: vec3<f32>,
}
// Each eye's camera, indexed by the view drawn: see `tomori_render::multiview`.
@group(0) @binding(0) var<uniform> cameras: array<Camera, 1 /* views */>;
@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;
struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    // The same at every vertex of a quad, so not interpolated, which could take a join's 1 to just under it.
    @location(2) @interpolate(flat) weight: f32,
    @location(3) world: vec3<f32>,
}
@vertex
fn vs_main(
    /* view_index */
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) weight: f32,
) -> Output {
    let camera = cameras[0u /* view */];
    var out: Output;
    out.position = camera.transform * vec4<f32>(position, 0.0, 1.0);
    out.world = (camera.model * vec4<f32>(position, 0.0, 1.0)).xyz;
    out.color = color;
    out.uv = uv;
    out.weight = weight;
    return out;
}
// How much of the pixel a glyph covers: its distance field crosses one half, less its weight, at its edge, and the field's change across the pixel antialiases the edge at any distance. A weight of 1 or more, `ui_text::JOIN`, told apart from bold, which is at most a half, at three quarters, marks box drawing and blocks, grown by half a pixel so that where two meet no seam shows. Where a pixel spans more than a texel and a half of the atlas, as on a distant sign, samples on a grid across the pixel are averaged, so that thin strokes neither vanish between samples nor shimmer as the head moves; such pixels are few, as text that far away is small. Filled quads sample the atlas's solid block, which is wholly inside.
fn coverage(uv: vec2<f32>, weight: f32) -> f32 {
    let size = vec2<f32>(textureDimensions(atlas));
    let at = uv / size;
    let distance = textureSample(atlas, atlas_sampler, at).r;
    let width = max(fwidth(distance), 1e-4);
    let joins = weight >= 0.75;
    let edge = 0.5 - select(weight, weight - 1.0, joins);
    let grow = select(0.5, 1.0, joins);
    let across = dpdx(at);
    let down = dpdy(at);
    // Up to a texel and a half to a pixel, one sample is enough; past that, a grid of up to four by four, each sample spanning no more.
    let texels = max(length(across * size), length(down * size));
    let n = min(i32(ceil(texels / 1.5)), 4);
    if n <= 1 {
        return clamp((distance - edge) / width + grow, 0.0, 1.0);
    }
    // Each sample's edge is as wide as its share of the pixel, in a field that changes by an eighth a texel, from its spread of four; it grows by as many of its widths as there are samples across.
    let sharpest = texels / (8.0 * f32(n));
    let grown = f32(n) * (grow - 0.5) + 0.5;
    var sum = 0.0;
    for (var j = 0; j < n; j++) {
        for (var i = 0; i < n; i++) {
            let offset = (vec2<f32>(f32(i), f32(j)) + 0.5) / f32(n) - 0.5;
            let sample = textureSampleLevel(atlas, atlas_sampler, at + across * offset.x + down * offset.y, 0.0).r;
            sum += clamp((sample - edge) / sharpest + grown, 0.0, 1.0);
        }
    }
    return sum / f32(n * n);
}
// The exponent that turns coverage into opacity: `glyphs::GAMMA` where the target is sRGB, and so blends in linear light, and 1 where it blends the values it stores.
override GAMMA: f32 = 1.0;
// How opaque a pixel `covered` this much is drawn in `color`. Blended in linear light, a partly covered pixel of light text on a dark background shows far brighter than its share, so that small text, whose strokes and counters are all partly covered, grows bolder as it shrinks, and the counters of "e" and "o" fill in; dark text on a light background thins the same way. Light text's coverage is raised to `GAMMA`, and dark text's complement, mixed by the colour's luminance, so that text keeps one weight at every size.
fn opacity(covered: f32, color: vec3<f32>) -> f32 {
    let light = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    return mix(1.0 - pow(1.0 - covered, GAMMA), pow(covered, GAMMA), light);
}
@fragment
fn fs_main(/* view_index */ in: Output) -> @location(0) vec4<f32> {
    let camera = cameras[0u /* view */];
    // Hazed as everything in the scene is; its coverage is made opacity by its own colour, so that text keeps its weight.
    let color = hazed(in.color.rgb, camera.haze, distance(in.world, camera.eye));
    return vec4<f32>(color, in.color.a * opacity(coverage(in.uv, in.weight), in.color.rgb));
}
// How far back a backing's depth is written, in the depth buffer's units, so that a surface drawn later just behind it, as near as a panel carried over a wall's slot, is still drawn over it. Set from `occlusion::TIE`.
override TIE: f32 = 0.0;
// How opaque a fragment of a backing must be to write its depth, from 0 to 1: a rounded corner's outside, a glyph's or shape's margin, or an antialiased edge more transparent than this writes none, so that what is drawn later behind it still shows through it. Set from `occlusion::OCCLUDING`.
override OCCLUDING: f32 = 0.6;
// A backing drawn into depth alone, once its surface is drawn, so that what is behind it and drawn later is hidden; its colour is not written, and only where it is opaque enough. See `occlusion.rs`.
@vertex
fn vs_occlude(
    /* view_index */
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) weight: f32,
) -> Output {
    let camera = cameras[0u /* view */];
    var out: Output;
    let at = camera.transform * vec4<f32>(position, 0.0, 1.0);
    out.position = vec4<f32>(at.xy, min(at.z + TIE * at.w, at.w), at.w);
    out.color = color;
    out.uv = uv;
    out.weight = weight;
    return out;
}
@fragment
fn fs_occlude(input: Output) -> @location(0) vec4<f32> {
    if input.color.a * opacity(coverage(input.uv, input.weight), input.color.rgb) < OCCLUDING {
        discard;
    }
    return vec4<f32>(0.0);
}
