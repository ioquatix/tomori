struct Camera {
    view_projection: mat4x4<f32>,
    inverse_view_projection: mat4x4<f32>,
}
@group(0) @binding(0) var<uniform> camera: Camera;

struct RayVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) near: vec3<f32>,
    @location(2) far: vec3<f32>,
}

@vertex
fn ray_vertex(@builtin(vertex_index) index: u32) -> RayVertex {
    let points = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    let ndc = points[index];
    let near = camera.inverse_view_projection * vec4<f32>(ndc, 0.0, 1.0);
    let far = camera.inverse_view_projection * vec4<f32>(ndc, 0.99, 1.0);
    var out: RayVertex;
    out.position = vec4<f32>(ndc, 1.0, 1.0);
    out.uv = ndc * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    out.near = near.xyz / near.w;
    out.far = far.xyz / far.w;
    return out;
}

// All density is below y=-0.8, outside the application's working volume.
// This also bounds work for horizon rays, instead of marching into infinity.
fn cloud_interval(origin: vec3<f32>, direction: vec3<f32>) -> vec2<f32> {
    let epsilon = select(vec3<f32>(-0.00001), vec3<f32>(0.00001), direction >= vec3<f32>(0.0));
    let inverse = 1.0 / select(epsilon, direction, abs(direction) > vec3<f32>(0.00001));
    let a = (vec3<f32>(-54.0, -11.0, -56.5) - origin) * inverse;
    let b = (vec3<f32>(54.0, -0.8, 51.5) - origin) * inverse;
    let lo = min(a, b);
    let hi = max(a, b);
    return vec2<f32>(max(max(max(lo.x, lo.y), lo.z), 0.0), min(min(min(hi.x, hi.y), hi.z), 85.0));
}
