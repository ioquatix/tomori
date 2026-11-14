// The campsite's bonfire: low-poly flame tongues, each a card turned towards the eye about the upright, swaying and
// flickering by the time alone, as big as the fire is; embers rising from them and fading, as many and as high as the
// fire is big; and a little flame on a marshmallow that burns. All of it glows, added to what is behind it, so that
// nothing of it needs sorting, hazed as everything is, and it is drawn only inside the reveal sphere while the farmyard
// grows.

struct Camera {
    inverse_view_projection: mat4x4<f32>,
    // The centre and radius of the sphere the farmyard is drawn inside while it grows; a negative radius fills the world.
    reveal: vec4<f32>,
    view_projection: mat4x4<f32>,
    // Where the eye is.
    eye: vec4<f32>,
    // The haze's colour, and its density in its fourth component.
    haze: vec4<f32>,
    // The time in seconds, in its first component; how big the bonfire's flames are, as a scale of how they were first
    // drawn, in its second; how many of its embers rise, of all of them, in its third; and, in its fourth, how often the
    // time wraps, which the embers' trails read.
    time: vec4<f32>,
}
// Each eye's camera, indexed by the view drawn: see `tomori_render::multiview`.
@group(0) @binding(0) var<uniform> cameras: array<Camera, 1 /* views */>;

// A corner of a flame or an ember: the foot of what it belongs to; the corner, across it from -1 to 1 and up it from 0
// to 1; its colour, with how brightly it glows; and how it moves: its phase, its height, its width, and what it is, a
// flame of the bonfire 0, an ember 1, or a marshmallow's flame 2.
struct Corner {
    @location(0) foot: vec3<f32>,
    @location(1) corner: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) motion: vec4<f32>,
}

struct Shaded {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) color: vec3<f32>,
}

@vertex
fn fire_vertex(/* view_index */ corner: Corner) -> Shaded {
    let camera = cameras[0u /* view */];
    let t = camera.time.x;
    let phase = corner.motion.x;
    // Turned towards the eye about the upright.
    var towards = camera.eye.xyz - corner.foot;
    towards.y = 0.0;
    let along = select(vec3<f32>(1.0, 0.0, 0.0), normalize(vec3<f32>(towards.z, 0.0, -towards.x)), length(towards) > 1e-4);
    var world: vec3<f32>;
    var glow = corner.color.a;
    let ember = corner.motion.w > 0.5 && corner.motion.w < 1.5;
    if !ember {
        // A flame: taller and shorter by turns, its tip swaying, more the higher it is; the bonfire's as big as the fire
        // is, a little wider as it grows taller, and a marshmallow's as it is.
        let bonfire = corner.motion.w < 0.5;
        let scale = select(1.0, camera.time.y, bonfire);
        let wide = select(1.0, sqrt(camera.time.y), bonfire);
        let flicker = 0.8 + 0.12 * sin(t * 9.1 + phase * 7.0) + 0.08 * sin(t * 23.7 + phase * 13.0);
        let up = corner.corner.y;
        let sway = (sin(t * 3.3 + phase * 5.0) * 0.05 + sin(t * 7.9 + phase * 3.0) * 0.025) * up * up * scale;
        world = corner.foot
            + along * (corner.corner.x * corner.motion.z * wide * (1.0 - 0.35 * up) + sway)
            + vec3<f32>(sway * 0.4, up * corner.motion.y * scale * flicker, 0.0);
        glow = glow * (0.85 + 0.15 * flicker);
    } else {
        // An ember: rising from the flames over its life, drifting, shrinking and fading as it goes, as high as the fire
        // is big; and only as many of them rise as the fire is big.
        let scale = camera.time.y;
        let life = fract(t * (0.35 + fract(phase * 3.7) * 0.25) + phase);
        let drift = vec3<f32>(sin(t * 1.7 + phase * 11.0), 0.0, cos(t * 1.3 + phase * 7.0)) * 0.12 * life * scale;
        let size = corner.motion.z * (1.0 - life);
        let rise = vec3<f32>(0.0, (0.25 + life * corner.motion.y) * scale, 0.0);
        world = corner.foot + rise + drift + along * corner.corner.x * size
            + vec3<f32>(0.0, (corner.corner.y - 0.5) * 2.0 * size, 0.0);
        let rising = select(0.0, 1.0, fract(phase * 7.31) < camera.time.z);
        glow = glow * (1.0 - life) * (1.0 - life) * rising;
    }
    var out: Shaded;
    out.position = camera.view_projection * vec4<f32>(world, 1.0);
    out.world = world;
    out.color = corner.color.rgb * glow;
    return out;
}

@fragment
fn fire_fragment(/* view_index */ in: Shaded) -> @location(0) vec4<f32> {
    let camera = cameras[0u /* view */];
    if camera.reveal.w >= 0.0 && distance(in.world, camera.reveal.xyz) > camera.reveal.w {
        discard;
    }
    // Its glow is added to what is behind it, so the haze takes as much of it as it hides.
    return vec4<f32>(hazed_glow(in.color, camera.haze, distance(in.world, camera.eye.xyz)), 1.0);
}
