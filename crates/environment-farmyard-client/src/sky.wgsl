// The farmyard's evening sky: deep blue overhead, a dusky band and the last warmth of sunset along the horizon in the
// west, a rising moon in the east, and a few stars, drawn behind everything, through the reveal sphere as the farmyard
// grows out of a space; and the ranges at its edge, beyond the far plane, at the sky's depth, each faded into the sky's
// colour that way as far as its distance hazes it.

struct Camera {
    inverse_view_projection: mat4x4<f32>,
    // The centre and radius of the sphere the farmyard is drawn inside while it grows; a negative radius fills the world.
    reveal: vec4<f32>,
    view_projection: mat4x4<f32>,
    // Where the eye is.
    eye: vec4<f32>,
    // The haze's colour, and its density in its fourth component, which the water is hazed by.
    haze: vec4<f32>,
    // The time in seconds that moves the water, in its first component.
    time: vec4<f32>,
}
// Each eye's camera, indexed by the view drawn: see `tomori_render::multiview`.
@group(0) @binding(0) var<uniform> cameras: array<Camera, 1 /* views */>;

struct Background {
    @builtin(position) position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
}

@vertex
fn sky_vertex(@builtin(vertex_index) index: u32) -> Background {
    let points = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var out: Background;
    out.position = vec4<f32>(points[index], 1.0, 1.0);
    out.ndc = points[index];
    return out;
}

// Whether a ray from `origin` along unit `direction` meets the reveal sphere, as the sky is seen through it.
fn sees_reveal(camera: Camera, origin: vec3<f32>, direction: vec3<f32>) -> bool {
    if camera.reveal.w < 0.0 { return true; }
    let offset = origin - camera.reveal.xyz;
    let b = dot(offset, direction);
    let c = dot(offset, offset) - camera.reveal.w * camera.reveal.w;
    return c <= 0.0 || (b <= 0.0 && b * b >= c);
}

const MOON: vec3<f32> = vec3<f32>(0.62, 0.36, -0.7);
const DUSK: vec3<f32> = vec3<f32>(0.05, 0.055, 0.1);

// The sky's colour along unit `direction`, without the moon's disc or the stars: what the ranges fade into.
fn horizon(direction: vec3<f32>) -> vec3<f32> {
    let up = direction.y;
    let zenith = vec3<f32>(0.008, 0.012, 0.035);
    var color = mix(DUSK, zenith, smoothstep(-0.05, 0.6, up));
    // The last of the sunset, low in the west.
    let west = max(dot(normalize(vec3<f32>(direction.x, 0.0, direction.z) + vec3<f32>(0.0001)), vec3<f32>(-0.8, 0.0, -0.6)), 0.0);
    color += vec3<f32>(0.16, 0.06, 0.025) * pow(west, 3.0) * exp(-max(up, 0.0) * 9.0);
    // The moon's halo.
    let separation = length(direction - normalize(MOON));
    color += vec3<f32>(0.25, 0.27, 0.32) * 0.08 * exp(-separation * separation * 40.0);
    // Below the horizon, the far meadow fades into the same dusk.
    return mix(color, DUSK * 0.6, smoothstep(0.0, -0.08, up));
}

@fragment
fn sky_fragment(/* view_index */ in: Background) -> @location(0) vec4<f32> {
    let camera = cameras[0u /* view */];
    let near_h = camera.inverse_view_projection * vec4<f32>(in.ndc, 0.0, 1.0);
    let far_h = camera.inverse_view_projection * vec4<f32>(in.ndc, 0.99, 1.0);
    let origin = near_h.xyz / near_h.w;
    let direction = normalize(far_h.xyz / far_h.w - origin);
    if !sees_reveal(camera, origin, direction) { discard; }
    let up = direction.y;
    var color = horizon(direction);
    // The moon's disc.
    let disc = 1.0 - smoothstep(0.04, 0.043, length(direction - normalize(MOON)));
    color = mix(color, vec3<f32>(0.78, 0.8, 0.82), disc);
    // A few fixed stars, on a direction grid, fading out towards the dusky horizon.
    let star_uv = vec2<f32>(atan2(direction.x, -direction.z), asin(clamp(up, -1.0, 1.0))) * 90.0;
    let cell = floor(star_uv);
    let random = fract(sin(dot(cell, vec2<f32>(127.1, 311.7))) * 43758.5453);
    let star = (1.0 - smoothstep(0.02, 0.12, length(fract(star_uv) - vec2<f32>(0.5))))
        * step(0.99, random) * smoothstep(0.2, 0.55, up);
    color += vec3<f32>(0.2, 0.22, 0.28) * star * (1.0 - disc);
    return vec4<f32>(color, 1.0);
}

// A point of the ranges: the way it lies, and its colour, lit, with how far it fades into the sky in its fourth
// component.
struct Far {
    @location(0) direction: vec3<f32>,
    @location(1) color: vec4<f32>,
}

struct Distant {
    @builtin(position) position: vec4<f32>,
    @location(0) direction: vec3<f32>,
    @location(1) color: vec4<f32>,
}

// Infinitely far, so that the ranges stay where they are as the eye moves, at the sky's depth, so that everything
// nearer is drawn over them.
@vertex
fn range_vertex(/* view_index */ far: Far) -> Distant {
    let camera = cameras[0u /* view */];
    let clip = camera.view_projection * vec4<f32>(far.direction, 0.0);
    var out: Distant;
    out.position = vec4<f32>(clip.xy, clip.w, clip.w);
    out.direction = far.direction;
    out.color = far.color;
    return out;
}

@fragment
fn range_fragment(/* view_index */ in: Distant) -> @location(0) vec4<f32> {
    let camera = cameras[0u /* view */];
    let direction = normalize(in.direction);
    if !sees_reveal(camera, camera.eye.xyz, direction) { discard; }
    return vec4<f32>(mix(in.color.rgb, horizon(direction), in.color.a), 1.0);
}
