// The farmyard's water beyond the fence: the river and the pool, their ripples carried downstream and glinting in the
// moonlight, the waterfall streaking down its cliff, foam churning and mist billowing about its foot, moved by the time
// alone, hazed with distance as the models are, and drawn only inside the reveal sphere while the farmyard grows.

struct Camera {
    inverse_view_projection: mat4x4<f32>,
    // The centre and radius of the sphere the farmyard is drawn inside while it grows; a negative radius fills the world.
    reveal: vec4<f32>,
    view_projection: mat4x4<f32>,
    // Where the eye is.
    eye: vec4<f32>,
    // The haze's colour, and its density in its fourth component.
    haze: vec4<f32>,
    // The time in seconds that moves the water, in its first component.
    time: vec4<f32>,
}
// Each eye's camera, indexed by the view drawn: see `tomori_render::multiview`.
@group(0) @binding(0) var<uniform> cameras: array<Camera, 1 /* views */>;

// A point of the water: where it is, its colour, with what it is in the fourth component, the surface 0, the fall 1,
// foam 2 and mist 3, and where it is in its flow, metres across and along it.
struct Point {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) flow: vec2<f32>,
}

struct Shaded {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) flow: vec2<f32>,
}

@vertex
fn water_vertex(/* view_index */ point: Point) -> Shaded {
    let camera = cameras[0u /* view */];
    var out: Shaded;
    out.position = camera.view_projection * vec4<f32>(point.position, 1.0);
    out.world = point.position;
    out.color = point.color;
    out.flow = point.flow;
    return out;
}

const MOONLIT: vec3<f32> = vec3<f32>(0.16, 0.18, 0.24);
const DEEP: vec3<f32> = vec3<f32>(0.02, 0.04, 0.055);

fn hash(value: f32) -> f32 {
    return fract(sin(value * 12.9898) * 43758.5453);
}

@fragment
fn water_fragment(/* view_index */ in: Shaded) -> @location(0) vec4<f32> {
    let camera = cameras[0u /* view */];
    if camera.reveal.w >= 0.0 && distance(in.world, camera.reveal.xyz) > camera.reveal.w {
        discard;
    }
    let time = camera.time.x;
    var color = in.color.rgb;
    if in.color.a < 0.5 {
        // Ripples carried downstream, glinting where they catch the moon.
        let along = in.flow.y - time * 0.8;
        let wave = sin(along * 2.2 + sin(in.flow.x * 1.7 + along * 0.6) * 1.8)
            * sin(in.flow.x * 3.1 - along * 0.9 + 1.3);
        color += MOONLIT * smoothstep(0.5, 0.95, wave) * 0.5;
    } else if in.color.a < 1.5 {
        // Streaks down the fall, each lane at its own pace.
        let lane = floor(in.flow.x * 5.0);
        let pace = 1.1 + hash(lane) * 0.6;
        let streak = fract(in.flow.y * 0.45 - time * pace + hash(lane + 7.0));
        color = mix(color * 0.7, color * 1.6, smoothstep(0.3, 0.95, streak));
    } else if in.color.a < 2.5 {
        // Foam churning about the fall's foot, thinning into the pool outwards.
        let churn = sin(in.flow.x * 4.0 + time * 2.3) * sin(in.flow.y * 7.0 - time * 3.1) * 0.5 + 0.5;
        color = mix(DEEP, color, (0.45 + 0.55 * churn) * (1.0 - smoothstep(0.5, 1.9, in.flow.y)));
    } else {
        // Mist billowing slowly upwards.
        let billow = sin(in.flow.x * 3.0 + time * 0.9) * sin(in.flow.y * 5.0 - time * 1.4) * 0.5 + 0.5;
        color = color * (0.8 + 0.3 * billow);
    }
    return vec4<f32>(hazed(color, camera.haze, distance(in.world, camera.eye.xyz)), 1.0);
}
