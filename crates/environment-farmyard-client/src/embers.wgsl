// The light a burning marshmallow paints in the air: a ribbon along the path it took, turned towards the eye about its
// length, thin and yellow-white when fresh, widening and cooling through orange to dull red as it fades, with sparks
// rising and embers falling off it, each a little disc turned towards the eye. Every quad is written once, with when it
// was made, and aged here by the time; all of it glows, added to what is behind it, unlit, hazed with distance as
// everything is, and drawn only inside the reveal sphere while the farmyard grows. See `embers.rs`, whose `LIFE`, widths
// and colours these follow.

struct Camera {
    inverse_view_projection: mat4x4<f32>,
    // The centre and radius of the sphere the farmyard is drawn inside while it grows; a negative radius fills the world.
    reveal: vec4<f32>,
    view_projection: mat4x4<f32>,
    // Where the eye is.
    eye: vec4<f32>,
    // The haze's colour, and its density in its fourth component.
    haze: vec4<f32>,
    // The time in seconds, in its first component, which wraps every fourth component's seconds; its second and third
    // are the bonfire's.
    time: vec4<f32>,
}
// Each eye's camera, indexed by the view drawn: see `tomori_render::multiview`.
@group(0) @binding(0) var<uniform> cameras: array<Camera, 1 /* views */>;

const LIFE: f32 = 3.0;
const FRESH_WIDTH: f32 = 0.014;
const FADED_WIDTH: f32 = 0.03;
const LEAST_WIDTH: f32 = 0.004;
// How brightly the fresh ribbon glows, its core past white.
const BRIGHT: f32 = 2.0;

// A corner of a quad: a ribbon's point, or where a spark or an ember began; the way the ribbon runs there, or the
// spark's velocity as it began; across it, up a spark, when it was made and what it is, a ribbon 0, a spark 1 or an
// ember 2; and a spark's life, size, phase and rise.
struct Corner {
    @location(0) at: vec3<f32>,
    @location(1) way: vec3<f32>,
    @location(2) shape: vec4<f32>,
    @location(3) motion: vec4<f32>,
}

struct Shaded {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) across: vec2<f32>,
}

// How long ago `made` was, on the wrapping clock, the camera's `time`. Given the time rather than the camera: see
// `tomori_render::multiview` on a vertex shader passing an eye's uniform by value.
fn age_of(time: vec4<f32>, made: f32) -> f32 {
    var age = time.x - made;
    if age < 0.0 {
        age = age + time.w;
    }
    return age;
}

// Yellow-white, through orange, to dull red, as `fresh` goes from 0 to 1.
fn cooling(fresh: f32) -> vec3<f32> {
    let at = fresh * 3.0;
    if at < 1.0 {
        return mix(vec3<f32>(1.0, 0.92, 0.7), vec3<f32>(1.0, 0.62, 0.22), at);
    }
    if at < 2.0 {
        return mix(vec3<f32>(1.0, 0.62, 0.22), vec3<f32>(0.95, 0.28, 0.05), at - 1.0);
    }
    return mix(vec3<f32>(0.95, 0.28, 0.05), vec3<f32>(0.55, 0.06, 0.02), min(at - 2.0, 1.0));
}

// A unit vector square to `way` and `towards`, or to `towards` alone where they run together.
fn square(way: vec3<f32>, towards: vec3<f32>) -> vec3<f32> {
    var across = cross(way, towards);
    if dot(across, across) < 1e-12 {
        across = cross(vec3<f32>(0.0, 1.0, 0.0), towards);
    }
    if dot(across, across) < 1e-12 {
        across = vec3<f32>(1.0, 0.0, 0.0);
    }
    return normalize(across);
}

@vertex
fn ember_vertex(/* view_index */ corner: Corner) -> Shaded {
    let camera = cameras[0u /* view */];
    let age = age_of(camera.time, corner.shape.z);
    var world: vec3<f32>;
    var color: vec3<f32>;
    if corner.shape.w < 0.5 {
        // The ribbon, turned towards the eye about its length, at least a pixel or so across however far away.
        let fresh = clamp(age / LIFE, 0.0, 1.0);
        let towards = camera.eye.xyz - corner.at;
        let width = max(mix(FRESH_WIDTH, FADED_WIDTH, sqrt(fresh)), LEAST_WIDTH * length(towards));
        world = corner.at + square(corner.way, towards) * width * 0.5 * corner.shape.x;
        let glow = (1.0 - fresh) * (1.0 - fresh);
        color = cooling(fresh) * glow * BRIGHT;
    } else {
        // A spark drifting up, slowed by the air, or an ember falling, flickering, shrinking and fading over its life;
        // gone once it has lived, its corners together.
        let life = corner.motion.x;
        let lived = clamp(age / life, 0.0, 1.0);
        let phase = corner.motion.z;
        let t = min(age, life);
        let drift = corner.way * (1.0 - exp(-3.0 * t)) / 3.0 + vec3<f32>(0.0, 0.5 * corner.motion.w * t * t, 0.0)
            + vec3<f32>(sin(t * 7.0 + phase), 0.0, cos(t * 5.0 + phase)) * 0.01 * t;
        let centre = corner.at + drift;
        let towards = normalize(camera.eye.xyz - centre);
        let right = square(vec3<f32>(0.0, 1.0, 0.0), towards);
        let up = cross(towards, right);
        let size = corner.motion.y * (1.0 - 0.5 * lived) * select(0.0, 1.0, age < life);
        world = centre + (right * corner.shape.x + up * corner.shape.y) * size;
        let flicker = 0.75 + 0.25 * sin(age * 31.0 + phase * 5.0);
        let fade = (1.0 - lived) * (1.0 - lived);
        if corner.shape.w < 1.5 {
            color = vec3<f32>(1.0, 0.7, 0.3) * fade * flicker * 1.6;
        } else {
            color = vec3<f32>(1.0, 0.36, 0.08) * fade * flicker;
        }
    }
    var out: Shaded;
    out.position = camera.view_projection * vec4<f32>(world, 1.0);
    out.world = world;
    out.color = hazed_glow(color, camera.haze, distance(world, camera.eye.xyz));
    out.across = corner.shape.xy;
    return out;
}

@fragment
fn ember_fragment(/* view_index */ in: Shaded) -> @location(0) vec4<f32> {
    let camera = cameras[0u /* view */];
    if camera.reveal.w >= 0.0 && distance(in.world, camera.reveal.xyz) > camera.reveal.w {
        discard;
    }
    // A thin hot core, a third of its width, in a soft halo, as a long exposure blooms about a moving light.
    let off = dot(in.across, in.across);
    let halo = clamp(1.0 - off, 0.0, 1.0);
    return vec4<f32>(in.color * (exp(-off * 14.0) + 0.3 * halo * halo), 1.0);
}
