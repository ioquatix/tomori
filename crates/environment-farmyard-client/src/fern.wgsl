// The farmyard's ferns: every instance of each kind's mesh, placed at its foot, turned and sized, swaying in the breeze
// by the time alone, its frond tips most and its trunk not at all, each fern at its own phase and each frond fluttering
// a little of its own accord; lit as the farmyard's models are, by the sky's and the ground's light, the moon, a little
// of the moon through a frond lit from behind, and the few lamps listed for it, its underside's own colour where the
// back of a frond is seen, ponga's silver; hazed with distance, and drawn only inside the reveal sphere while the farmyard grows.

struct Lamp {
    // Where it is, and how far it reaches.
    position: vec4<f32>,
    // Its colour, as bright as it is at its centre.
    color: vec4<f32>,
}

struct Camera {
    inverse_view_projection: mat4x4<f32>,
    // The centre and radius of the sphere the farmyard is drawn inside while it grows; a negative radius fills the world.
    reveal: vec4<f32>,
    view_projection: mat4x4<f32>,
    // Where the eye is.
    eye: vec4<f32>,
    // The haze's colour, and its density in its fourth component.
    haze: vec4<f32>,
    // The time in seconds that sways the ferns, in its first component.
    time: vec4<f32>,
}
// Each eye's camera, indexed by the view drawn: see `tomori_render::multiview`.
@group(0) @binding(0) var<uniform> cameras: array<Camera, 1 /* views */>;

// How the ferns are lit, the same for both eyes: the way towards the moon, its light, and the sky's and the ground's;
// and the lamps, an unlit one black, and how many there are.
struct Lighting {
    moon: vec4<f32>,
    moonlight: vec4<f32>,
    sky: vec4<f32>,
    ground: vec4<f32>,
    lamps: array<Lamp, 64>,
    counts: vec4<u32>,
}
@group(0) @binding(1) var<uniform> lighting: Lighting;

// How much of a lamp's light it gives a fern: the `index`th byte of its weights, 255 for all of it.
fn weight(lamps: vec4<u32>, index: u32) -> f32 {
    let word = select(lamps.z, lamps.w, index >= 4u);
    let byte = (word >> (8u * (index % 4u))) & 0xffu;
    return select(f32(byte) / 255.0, 1.0, byte == 255u);
}

// A point of a fern's mesh at its own size: where it is, the way its upper side faces, its colour above and beneath,
// and how far it sways at most; and its instance: its foot and how it is turned, and how large it is and its phase.
struct Point {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) underside: vec3<f32>,
    @location(4) sway: f32,
    @location(5) foot: vec4<f32>,
    @location(6) size: vec4<f32>,
    // A mask of the lamps that light it, low then high, and how much of its light each gives, a byte each in the mask's
    // order, 255 for all of it and past the last; or every lamp in the mask for every lamp.
    @location(7) lamps: vec4<u32>,
}

struct Shaded {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) underside: vec3<f32>,
    @location(4) @interpolate(flat) lamps: vec4<u32>,
}

// The way the breeze blows across the ground, and across it.
const BREEZE: vec3<f32> = vec3<f32>(0.8, 0.0, 0.6);
const ACROSS: vec3<f32> = vec3<f32>(-0.6, 0.0, 0.8);

// `value` turned about the upright by `turn`, as the instance's rotation about +Y turns it.
fn turned(value: vec3<f32>, turn: f32) -> vec3<f32> {
    let c = cos(turn);
    let s = sin(turn);
    return vec3<f32>(value.x * c + value.z * s, value.y, value.z * c - value.x * s);
}

@vertex
fn fern_vertex(/* view_index */ point: Point) -> Shaded {
    let camera = cameras[0u /* view */];
    let scale = point.size.x;
    let phase = point.size.y;
    let t = camera.time.x;
    // A slow swell and a quicker stir, the fern's own, leaning it a little with the breeze, and a flutter of each frond,
    // by where it is on its fern.
    let swell = sin(t * 0.9 + phase) * 0.6 + sin(t * 2.1 + phase * 1.7) * 0.25 + 0.15;
    let flutter = sin(t * 4.3 + phase + dot(point.position.xz, vec2<f32>(2.7, 1.9))) * 0.3;
    let sway = point.sway * scale;
    var world = point.foot.xyz + turned(point.position * scale, point.foot.w);
    world += (BREEZE * swell + ACROSS * flutter * 0.5 - vec3<f32>(0.0, abs(flutter) * 0.3, 0.0)) * sway;
    var out: Shaded;
    out.position = camera.view_projection * vec4<f32>(world, 1.0);
    out.world = world;
    out.normal = turned(point.normal, point.foot.w);
    out.color = point.color;
    out.underside = point.underside;
    out.lamps = point.lamps;
    return out;
}

// The light `lamp` gives a point at `world` facing `normal`, as much of it as `weight` says.
fn lamp_light(lamp: Lamp, weight: f32, world: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    let offset = lamp.position.xyz - world;
    let distance = length(offset);
    let reach = max(lamp.position.w, 0.001);
    let falloff = clamp(1.0 - distance / reach, 0.0, 1.0);
    let towards = offset / max(distance, 0.0001);
    return lamp.color.rgb * weight * falloff * falloff * (0.35 + 0.65 * max(dot(normal, towards), 0.0));
}

@fragment
fn fern_fragment(/* view_index */ in: Shaded, @builtin(front_facing) facing: bool) -> @location(0) vec4<f32> {
    let camera = cameras[0u /* view */];
    if camera.reveal.w >= 0.0 && distance(in.world, camera.reveal.xyz) > camera.reveal.w {
        discard;
    }
    // Its upper side, or its underside, in its own colour, facing the eye.
    var normal = normalize(in.normal);
    var albedo = in.color;
    if !facing {
        normal = -normal;
        albedo = in.underside;
    }
    let moon = dot(normal, lighting.moon.xyz);
    var light = mix(lighting.ground.rgb, lighting.sky.rgb, normal.y * 0.5 + 0.5)
        + lighting.moonlight.rgb * (max(moon, 0.0) + max(-moon, 0.0) * 0.25);
    // Its own lamps, in the lamps' order, or every lamp.
    if all(in.lamps.xy == vec2<u32>(0xffffffffu)) {
        for (var index = 0u; index < min(lighting.counts.x, 64u); index += 1u) {
            light += lamp_light(lighting.lamps[index], 1.0, in.world, normal);
        }
    } else if all(in.lamps.zw == vec2<u32>(0xffffffffu)) {
        // Each giving all its light.
        var low = in.lamps.x;
        while low != 0u {
            let index = firstTrailingBit(low);
            low &= low - 1u;
            light += lamp_light(lighting.lamps[index], 1.0, in.world, normal);
        }
        var high = in.lamps.y;
        while high != 0u {
            let index = firstTrailingBit(high);
            high &= high - 1u;
            light += lamp_light(lighting.lamps[32u + index], 1.0, in.world, normal);
        }
    } else {
        // Some fading, each by its byte.
        var lit = 0u;
        var low = in.lamps.x;
        while low != 0u {
            let index = firstTrailingBit(low);
            low &= low - 1u;
            light += lamp_light(lighting.lamps[index], weight(in.lamps, lit), in.world, normal);
            lit += 1u;
        }
        var high = in.lamps.y;
        while high != 0u {
            let index = firstTrailingBit(high);
            high &= high - 1u;
            light += lamp_light(lighting.lamps[32u + index], weight(in.lamps, lit), in.world, normal);
            lit += 1u;
        }
    }
    return vec4<f32>(hazed(albedo * light, camera.haze, distance(in.world, camera.eye.xyz)), 1.0);
}
