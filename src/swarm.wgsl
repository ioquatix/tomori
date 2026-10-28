struct Camera { view_projection: mat4x4<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) distance: f32,
}

@vertex
fn vs_main(
    @location(0) vertex: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) position: vec3<f32>,
    @location(3) scale: vec3<f32>,
    @location(4) color: vec3<f32>,
) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = camera.view_projection * vec4<f32>(position + vertex * scale, 1.0);
    out.normal = normal;
    out.color = color;
    out.distance = out.clip_position.w;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let light = 0.5 + 0.5 * max(dot(normalize(in.normal), normalize(vec3<f32>(0.4, 0.8, 0.5))), 0.0);
    let fog = exp(-0.025 * in.distance * in.distance);
    return vec4<f32>(mix(vec3<f32>(0.006, 0.009, 0.025), in.color * light, fog), 1.0);
}
