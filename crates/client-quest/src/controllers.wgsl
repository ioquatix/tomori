struct Camera { view_projection: mat4x4<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var controller_texture: texture_2d<f32>;
@group(1) @binding(1) var controller_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) distance: f32,
    @location(3) uv: vec2<f32>,
}

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) uv: vec2<f32>,
    @location(4) model_0: vec4<f32>,
    @location(5) model_1: vec4<f32>,
    @location(6) model_2: vec4<f32>,
    @location(7) model_3: vec4<f32>,
) -> VertexOutput {
    let model = mat4x4<f32>(model_0, model_1, model_2, model_3);
    var out: VertexOutput;
    out.clip_position = camera.view_projection * model * vec4<f32>(position, 1.0);
    out.normal = (model * vec4<f32>(normal, 0.0)).xyz;
    out.color = color;
    out.distance = out.clip_position.w;
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let light = 0.5 + 0.5 * max(dot(normalize(in.normal), normalize(vec3<f32>(0.4, 0.8, 0.5))), 0.0);
    let fog = exp(-0.025 * in.distance * in.distance);
    let color = textureSample(controller_texture, controller_sampler, in.uv).rgb * in.color;
    return vec4<f32>(mix(vec3<f32>(0.006, 0.009, 0.025), color * light, fog), 1.0);
}
