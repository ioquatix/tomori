// An image panel's image, drawn over its panel's face in the panel's plane, from a texture with every mipmap level,
// sampled trilinearly, so that it is sharp near and does not shimmer far. Opaque: the server lays transparency over white.
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
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world: vec3<f32>,
}
@vertex
fn vs_main(/* view_index */ @location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> Output {
    let camera = cameras[0u /* view */];
    var out: Output;
    out.position = camera.transform * vec4<f32>(position, 0.0, 1.0);
    out.world = (camera.model * vec4<f32>(position, 0.0, 1.0)).xyz;
    out.uv = uv;
    return out;
}
@fragment
fn fs_main(/* view_index */ in: Output) -> @location(0) vec4<f32> {
    let camera = cameras[0u /* view */];
    let color = textureSample(image, image_sampler, in.uv).rgb;
    return vec4<f32>(hazed(color, camera.haze, distance(in.world, camera.eye)), 1.0);
}
