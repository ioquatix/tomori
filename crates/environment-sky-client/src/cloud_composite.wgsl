// Each eye's clouds, a layer for each view drawn: see `tomori_render::multiview`.
@group(1) @binding(0) var clouds: texture_2d_array<f32>;
@group(1) @binding(1) var cloud_sampler: sampler;

struct Composite {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn composite_fragment(/* view_index */ in: RayVertex) -> Composite {
    let camera = cameras[0u /* view */];
    let direction = normalize(in.far - in.near);
    let interval = cloud_interval(in.near, direction);
    if interval.y <= interval.x || !sees_reveal(camera, in.near, direction) { discard; }
    let color = textureSampleLevel(clouds, cloud_sampler, in.uv, 0u /* view */, 0.0);
    if color.a < 0.002 { discard; }
    let entry = in.near + direction * interval.x;
    let clip = camera.view_projection * vec4<f32>(entry, 1.0);
    var out: Composite;
    out.color = color;
    // Test the volume entry against full-resolution opaque depth. Because all
    // application scenery is above this cloud slab, it cannot cut a ray inside
    // the volume. A future object inside clouds would need depth-limited marching.
    out.depth = clip.z / clip.w;
    return out;
}
