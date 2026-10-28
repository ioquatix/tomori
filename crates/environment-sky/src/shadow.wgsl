struct Shadow {
    world_to_light: mat4x4<f32>,
    parameters: vec4<f32>,
}
@group(0) @binding(0) var<uniform> shadow: Shadow;

@vertex
fn shadow_vertex(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return shadow.world_to_light * vec4<f32>(position, 1.0);
}
