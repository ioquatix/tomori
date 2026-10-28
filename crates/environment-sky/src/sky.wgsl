struct Camera {
    view_projection: mat4x4<f32>,
    inverse_view_projection: mat4x4<f32>,
}
@group(0) @binding(0) var<uniform> camera: Camera;

struct Shadow {
    world_to_light: mat4x4<f32>,
    parameters: vec4<f32>,
}
@group(1) @binding(0) var moon_shadow: texture_depth_2d;
@group(1) @binding(1) var shadow_sampler: sampler_comparison;
@group(1) @binding(2) var<uniform> shadow: Shadow;
@group(2) @binding(0) var baked_lighting: texture_2d<f32>;
@group(2) @binding(1) var lighting_sampler: sampler;

struct SkyVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
}

@vertex
fn sky_vertex(@builtin(vertex_index) index: u32) -> SkyVertex {
    let points = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var out: SkyVertex;
    out.position = vec4<f32>(points[index], 1.0, 1.0);
    out.ndc = points[index];
    return out;
}

fn night(direction: vec3<f32>) -> vec3<f32> {
    let elevation = smoothstep(-0.15, 0.65, direction.y);
    let horizon = vec3<f32>(0.018, 0.027, 0.06);
    let zenith = vec3<f32>(0.003, 0.005, 0.016);
    return mix(horizon, zenith, elevation);
}

@fragment
fn sky_fragment(in: SkyVertex) -> @location(0) vec4<f32> {
    let near_h = camera.inverse_view_projection * vec4<f32>(in.ndc, 0.0, 1.0);
    let far_h = camera.inverse_view_projection * vec4<f32>(in.ndc, 0.99, 1.0);
    let direction = normalize(far_h.xyz / far_h.w - near_h.xyz / near_h.w);
    let moon_direction = normalize(vec3<f32>(0.48, 0.53, -0.8));
    let separation = length(direction - moon_direction);
    let disc = 1.0 - smoothstep(0.039, 0.042, separation);
    let halo = 0.06 * exp(-separation * separation * 70.0);
    var color = night(direction) + vec3<f32>(0.4, 0.5, 0.7) * halo;
    // Subtle deterministic markings; no animated noise or scintillation in VR.
    let local = (direction.xy - moon_direction.xy) / 0.04;
    let crater_a = local - vec2<f32>(0.25, 0.2);
    let crater_b = local - vec2<f32>(-0.3, -0.25);
    let markings = 0.97 - 0.12 * exp(-dot(crater_a, crater_a) * 12.0)
        - 0.09 * exp(-dot(crater_b, crater_b) * 22.0);
    color = mix(color, vec3<f32>(0.75, 0.81, 0.88) * markings, disc);
    // Sparse, fixed stars evaluated on a direction grid, not screen coordinates.
    let star_uv = vec2<f32>(atan2(direction.x, -direction.z), asin(clamp(direction.y, -1.0, 1.0))) * 100.0;
    let cell = floor(star_uv);
    let random = fract(sin(dot(cell, vec2<f32>(127.1, 311.7))) * 43758.5453);
    let star = (1.0 - smoothstep(0.02, 0.12, length(fract(star_uv) - vec2<f32>(0.5))))
        * step(0.987, random) * smoothstep(0.08, 0.35, direction.y);
    color += vec3<f32>(0.22, 0.28, 0.35) * star * (1.0 - disc);
    return vec4<f32>(color, 1.0);
}

struct SceneryVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) emission: f32,
    @location(3) distance: f32,
    @location(4) world_position: vec3<f32>,
    @location(5) light_uv: vec2<f32>,
}

@vertex
fn scenery_vertex(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) emission: f32,
    @location(4) light_uv: vec2<f32>,
) -> SceneryVertex {
    var out: SceneryVertex;
    out.position = camera.view_projection * vec4<f32>(position, 1.0);
    out.normal = normal;
    out.color = color;
    out.emission = emission;
    out.distance = out.position.w;
    out.world_position = position;
    out.light_uv = light_uv;
    return out;
}

fn moon_visibility(position: vec3<f32>, normal: vec3<f32>) -> f32 {
    let clip = shadow.world_to_light * vec4<f32>(position + normal * shadow.parameters.z, 1.0);
    let ndc = clip.xyz / clip.w;
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    // Compare each PCF tap against the receiver plane at that tap, not the
    // centre pixel's depth. This permits small contact-preserving bias without
    // self-shadow stripes across sloped surfaces. Derivatives precede branching.
    let dx = dpdx(vec3<f32>(uv, ndc.z));
    let dy = dpdy(vec3<f32>(uv, ndc.z));
    let determinant = dx.x * dy.y - dx.y * dy.x;
    let safe_determinant = select(1.0, determinant, abs(determinant) > 0.0000000001);
    let gradient = select(vec2<f32>(0.0), vec2<f32>(dx.z * dy.y - dy.z * dx.y, dx.x * dy.z - dy.x * dx.z) / safe_determinant,
        abs(determinant) > 0.0000000001);
    let filter_bias = 0.5 * shadow.parameters.x * (abs(gradient.x) + abs(gradient.y));
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || ndc.z < 0.0 || ndc.z > 1.0 {
        return 1.0;
    }
    var visibility = 0.0;
    // Fixed scene-space PCF: soft edges without temporal jitter or eye-dependent
    // cascades. The comparison sampler also filters each individual lookup.
    for (var y = -1; y <= 1; y += 1) {
        for (var x = -1; x <= 1; x += 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * shadow.parameters.x;
            let receiver_depth = ndc.z + dot(gradient, offset) - shadow.parameters.y - filter_bias;
            visibility += textureSampleCompareLevel(moon_shadow, shadow_sampler, uv + offset, receiver_depth);
        }
    }
    return mix(1.0, visibility / 9.0, shadow.parameters.w);
}

@fragment
fn scenery_fragment(in: SceneryVertex) -> @location(0) vec4<f32> {
    let moon = normalize(vec3<f32>(0.48, 0.53, -0.8));
    let normal = normalize(in.normal);
    let visibility = moon_visibility(in.world_position, normal);
    // Shadows occlude direct moonlight only; ambient and emissive lanterns stay.
    let baked = textureSampleLevel(baked_lighting, lighting_sampler, in.light_uv, 0.0);
    let ambient = 0.18 * mix(0.25, 1.0, baked.a);
    let direct = 0.52 * max(dot(normal, moon), 0.0) * visibility;
    let light = vec3<f32>(ambient + direct) + baked.rgb;
    let color = in.color * mix(light, vec3<f32>(1.0), in.emission);
    let haze = 1.0 - exp(-0.00025 * in.distance * in.distance);
    return vec4<f32>(mix(color, vec3<f32>(0.018, 0.027, 0.06), haze), 1.0);
}
