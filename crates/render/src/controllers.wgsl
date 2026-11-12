struct Camera {
    view_projection: mat4x4<f32>,
    // Where the eye is in the scene, in `xyz`.
    eye: vec4<f32>,
    // The scene's haze, the environment's: its colour, and its density in `w`, for `hazed`.
    haze: vec4<f32>,
    // The rest of the renderer's camera, which Tomori's cubes read, so that each eye's is as far from the next as there.
    pixels: f32,
}
// Each eye's camera, indexed by the view drawn: see `tomori_render::multiview`.
@group(0) @binding(0) var<uniform> cameras: array<Camera, 1 /* views */>;
@group(1) @binding(0) var controller_texture: texture_2d<f32>;
@group(1) @binding(1) var controller_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) world: vec3<f32>,
    @location(3) uv: vec2<f32>,
}

@vertex
fn vs_main(
    /* view_index */
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) uv: vec2<f32>,
    @location(4) model_0: vec4<f32>,
    @location(5) model_1: vec4<f32>,
    @location(6) model_2: vec4<f32>,
    @location(7) model_3: vec4<f32>,
) -> VertexOutput {
    let camera = cameras[0u /* view */];
    let model = mat4x4<f32>(model_0, model_1, model_2, model_3);
    var out: VertexOutput;
    out.world = (model * vec4<f32>(position, 1.0)).xyz;
    out.clip_position = camera.view_projection * vec4<f32>(out.world, 1.0);
    out.normal = (model * vec4<f32>(normal, 0.0)).xyz;
    out.color = color;
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(/* view_index */ in: VertexOutput) -> @location(0) vec4<f32> {
    let camera = cameras[0u /* view */];
    let light = 0.5 + 0.5 * max(dot(normalize(in.normal), normalize(vec3<f32>(0.4, 0.8, 0.5))), 0.0);
    let color = textureSample(controller_texture, controller_sampler, in.uv).rgb * in.color;
    return vec4<f32>(hazed(color * light, camera.haze, distance(in.world, camera.eye.xyz)), 1.0);
}
