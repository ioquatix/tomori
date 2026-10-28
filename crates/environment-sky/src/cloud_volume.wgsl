@group(1) @binding(0) var density_noise: texture_3d<f32>;
@group(1) @binding(1) var density_sampler: sampler;

fn density(position: vec3<f32>) -> f32 {
    let noise = textureSampleLevel(density_noise, density_sampler, position * 0.035, 0.0).rgb;
    let height = smoothstep(-10.8, -7.0, position.y) * (1.0 - smoothstep(-4.5, -0.8, position.y));
    let radius = length(position.xz - vec2<f32>(0.0, -2.5));
    let extent = smoothstep(6.0, 10.0, radius) * (1.0 - smoothstep(40.0, 53.0, radius));
    let coverage = mix(0.40, 0.56, noise.b);
    let body = smoothstep(coverage, coverage + 0.18, noise.r);
    return max(body - (1.0 - noise.g) * 0.13, 0.0) * height * extent;
}

@fragment
fn volume_fragment(in: RayVertex) -> @location(0) vec4<f32> {
    let direction = normalize(in.far - in.near);
    let interval = cloud_interval(in.near, direction);
    if interval.y <= interval.x { discard; }
    let step_length = (interval.y - interval.x) / 32.0;
    let moon = normalize(vec3<f32>(0.48, 0.53, -0.8));
    var transmission = 1.0;
    var radiance = vec3<f32>(0.0);
    // Fixed positions: no screen-space jitter or temporal noise swimming per eye.
    for (var step = 0u; step < 32u; step += 1u) {
        let distance = interval.x + (f32(step) + 0.5) * step_length;
        let position = in.near + direction * distance;
        let amount = density(position);
        if amount > 0.005 {
            let opacity = 1.0 - exp(-amount * step_length * 0.85);
            // A bounded, one-sample approximation of light extinction towards
            // the moon, not a second nested ray march or dynamic shadow map.
            let light_transmission = exp(-4.0 * density(position + moon * 3.0));
            let ambient = vec3<f32>(0.055, 0.075, 0.13);
            let moonlight = vec3<f32>(0.32, 0.37, 0.49) * light_transmission;
            let haze = 1.0 - exp(-distance * distance * 0.00024);
            let light = mix(ambient + moonlight, vec3<f32>(0.018, 0.027, 0.06), haze);
            radiance += transmission * opacity * light;
            transmission *= 1.0 - opacity;
            if transmission < 0.02 { break; }
        }
    }
    // Premultiplied radiance + physical transmittance, not an opaque surface.
    return vec4<f32>(radiance, 1.0 - transmission);
}
