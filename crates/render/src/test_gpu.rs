// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A device for tests that render offscreen, and WGSL validation for every shader, shared by this crate's tests, Sky's and the clients' through the `test-gpu` feature.
//!
//! A real GPU is preferred, and a software adapter such as Mesa's lavapipe is used when there is none, as on CI runners. Where there is no adapter at all, the test is skipped with a message on standard error, unless `TOMORI_REQUIRE_GPU` is set to anything but `0` or the empty string, as CI sets it, so that a missing driver fails the tests rather than silently skipping them.
//!
//! Every test in a test binary shares one instance, adapter and device, which are created by the first test to ask and are never dropped. Tests run on parallel threads, and before version 1.4.346 the Vulkan loader looked up a device, when one thread named an object, without the lock that another thread held while creating or destroying a device, and so read freed memory. wgpu names objects through `VK_EXT_debug_utils` in debug builds, and Ubuntu 26.04's loader, 1.4.341, crashed CI's rendering tests with SIGSEGV this way about half of the time. See <https://github.com/KhronosGroup/Vulkan-Loader/issues/1863>, fixed by <https://github.com/KhronosGroup/Vulkan-Loader/pull/1866>. With one device, nothing creates or destroys a device while tests run, and nothing is torn down at process exit. Error scopes are per thread, so tests on the same device do not see each other's errors.
use std::io::Write;
use std::sync::OnceLock;

/// The environment variable that makes a missing adapter fail the test instead of skipping it.
pub const REQUIRE_GPU: &str = "TOMORI_REQUIRE_GPU";

/// The adapter a test renders with, and a device and queue created on it with default limits, and the features of the frame profiler it has, shared by every test in the binary.
#[derive(Clone)]
pub struct Gpu {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// Fails if `required`, as [`REQUIRE_GPU`] says, because no adapter was found, with the `error` wgpu gave; or else returns the message that says the calling test is skipped.
fn missing(error: &dyn std::fmt::Display, required: Option<&str>) -> String {
    assert!(
        matches!(required, None | Some("" | "0")),
        "{REQUIRE_GPU} is set but no GPU adapter was found ({error}); install a Vulkan driver, such as Mesa's lavapipe (mesa-vulkan-drivers)"
    );
    format!(
        "Skipping a GPU test: no GPU adapter was found ({error}). Install a Vulkan driver, such as Mesa's lavapipe, to run it, and set {REQUIRE_GPU}=1 to fail instead."
    )
}

/// Writes to standard error directly, bypassing the test harness's capture, so that the message shows for a passing test too.
fn report(message: impl std::fmt::Display) {
    let _ = writeln!(std::io::stderr(), "{message}");
}

/// The backends tests render with: Vulkan, and Metal only when built with `--features wgpu/metal`, to preview on a Mac.
const BACKENDS: wgpu::Backends = wgpu::Backends::VULKAN.union(wgpu::Backends::METAL);

/// Opens the preferred adapter of `backends` and a device on it, or returns why there is no adapter.
///
/// # Panics
///
/// If the adapter refuses a device.
fn create(backends: wgpu::Backends) -> Result<Gpu, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    // High performance orders discrete and integrated GPUs first and software (`DeviceType::Cpu`) adapters last, so a software adapter is only chosen when it is the only one.
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|error| error.to_string())?;
    let info = adapter.get_info();
    report(format_args!(
        "GPU tests use {} ({:?}, {}).",
        info.name, info.device_type, info.driver
    ));
    // With the GPU's timestamps where it has them, as lavapipe does, so that the frame profiler is tested timing, and
    // multiview, so that both eyes are drawn in one pass as the headset draws them.
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: adapter.features()
            & (crate::profiler::FEATURES | crate::multiview::FEATURES),
        required_limits: crate::multiview::limits(&adapter, wgpu::Limits::default()),
        ..Default::default()
    }))
    .expect("the adapter must provide a device");
    Ok(Gpu {
        adapter,
        device,
        queue,
    })
}

/// The binary's shared adapter, device and queue, created on first use, or `None` after saying why the calling test is skipped.
///
/// # Panics
///
/// If no adapter is found while [`REQUIRE_GPU`] is set, or if the adapter refuses a device.
pub fn open() -> Option<Gpu> {
    static GPU: OnceLock<Result<Gpu, String>> = OnceLock::new();
    found(
        GPU.get_or_init(|| create(BACKENDS)),
        std::env::var(REQUIRE_GPU).ok().as_deref(),
    )
}

/// The shared GPU if there is one, or `None` after saying why the calling test is skipped; fails if `required`, as [`REQUIRE_GPU`] says.
fn found(gpu: &Result<Gpu, String>, required: Option<&str>) -> Option<Gpu> {
    match gpu {
        Ok(gpu) => Some(gpu.clone()),
        Err(error) => {
            report(missing(error, required));
            None
        }
    }
}

/// Runs `test` with the binary's shared GPU, as [`open`] gives it, and returns what it returns; or skips it, returning `None`, where there is no adapter.
///
/// # Panics
///
/// As [`open`] does.
pub fn with<T>(test: impl FnOnce(Gpu) -> T) -> Option<T> {
    open().map(test)
}

/// The `.wgsl` files directly in `directory`, by file name and in name order, with their source.
pub fn wgsl_files(directory: impl AsRef<std::path::Path>) -> Vec<(String, String)> {
    let mut files: Vec<_> = std::fs::read_dir(directory)
        .expect("the shader directory must be readable")
        .map(|entry| entry.expect("the shader directory must be readable").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "wgsl")
        })
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let source = std::fs::read_to_string(&path).expect("a shader must be readable");
            (name, source)
        })
        .collect();
    files.sort();
    files
}

/// Whether `gpu`'s device draws both eyes in one pass with four samples a pixel, as every Vulkan device the tests run on
/// does, lavapipe, as CI's, and desktop drivers; or, after saying the calling test's multiview half is skipped, not, as
/// Metal on a Mac does not (see [`crate::multiview::supported`]).
///
/// # Panics
///
/// If a Vulkan device cannot.
pub fn multiview(gpu: &Gpu) -> bool {
    draws_both(
        crate::multiview::supported(&gpu.device, 4),
        gpu.adapter.get_info().backend,
    )
}

/// Runs `test` with the binary's shared GPU, as [`with`] does, where its device draws both eyes in one pass, as
/// [`multiview`] says; or skips it, returning `None`.
pub fn with_multiview<T>(test: impl FnOnce(Gpu) -> T) -> Option<T> {
    with(|gpu| multiview(&gpu).then(|| test(gpu))).flatten()
}

fn draws_both(supported: bool, backend: wgpu::Backend) -> bool {
    assert!(
        supported || backend != wgpu::Backend::Vulkan,
        "every Vulkan device the tests run on draws both eyes in one pass"
    );
    if !supported {
        report(format_args!(
            "Skipping a multiview test: {backend} draws each eye in a pass of its own."
        ));
    }
    supported
}

/// What layer `layer` of `texture`, `width` by `height`, holds once drawn.
pub fn read_layer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    layer: u32,
    (width, height): (u32, u32),
) -> image::RgbaImage {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test layer readback"),
        size: u64::from(width * height * 4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    receiver.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
    buffer.unmap();
    image::RgbaImage::from_raw(width, height, data).unwrap()
}

/// Parses and validates WGSL `source` with naga, as wgpu does when it creates a shader module, so that a shader is checked where no adapter can compile it: after the shared haze functions, `tomori_assets::haze::shader`, as every pipeline that draws in the world compiles its own, unless it begins with them already or is them; as it is, for one view a pass, and as rewritten for both, by multiview (see [`crate::multiview`]).
///
/// # Panics
///
/// With the error, located in the source and labelled with `label`, if the source does not parse or validate.
pub fn validate_wgsl(label: &str, source: &str) {
    let hazed;
    let source = if source.starts_with(tomori_assets::haze::WGSL) {
        source
    } else {
        hazed = tomori_assets::haze::shader(source);
        &hazed
    };
    use crate::multiview::Views;
    for (views, capabilities) in [
        (Views::One, naga::valid::Capabilities::default()),
        (
            Views::Both,
            naga::valid::Capabilities::default() | naga::valid::Capabilities::MULTIVIEW,
        ),
    ] {
        let source = views.shader(source);
        let label = format!("{label} ({})", views.name());
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string_with_path(&source, &label)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), capabilities)
            .validate(&module)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string_with_path(&source, &label)));
        if let Some(function) = passes_a_uniform_by_value(&module) {
            panic!(
                "{label}: {function} is given a uniform by value in a vertex shader, which the Quest's driver cannot \
                 compile for both eyes by multiview: give it the fields it reads"
            );
        }
    }
}

/// The name of a function a vertex entry point of `module` calls, directly or not, that is given by value a struct or an
/// array read from a uniform: the uniform itself, an element of a uniform array, or a member of a uniform struct, however
/// deeply nested, such as an eye's camera; or none. Scalars, vectors and matrices read from a uniform, which such a
/// function is given instead, are not. The Quest 3's Adreno driver fails to make a pipeline drawing both eyes by multiview
/// of such a shader, answering `vkCreateGraphicsPipelines` with `VK_ERROR_UNKNOWN`, which wgpu takes, silently, as the
/// device lost.
fn passes_a_uniform_by_value(module: &naga::Module) -> Option<String> {
    let mut called = Vec::new();
    let mut found = None;
    for entry in &module.entry_points {
        if entry.stage == naga::ShaderStage::Vertex {
            calls(
                module,
                &entry.function,
                &entry.function.body,
                &mut called,
                &mut found,
            );
        }
    }
    found
}

/// Every function `block` of `caller`, in `module`, calls, directly or not, into `called`, and the first given a struct
/// or array read from a uniform into `found`.
fn calls(
    module: &naga::Module,
    caller: &naga::Function,
    block: &naga::Block,
    called: &mut Vec<naga::Handle<naga::Function>>,
    found: &mut Option<String>,
) {
    for statement in block.iter() {
        match statement {
            naga::Statement::Call {
                function,
                arguments,
                ..
            } => {
                let callee = &module.functions[*function];
                if found.is_none()
                    && arguments
                        .iter()
                        .zip(&callee.arguments)
                        .any(|(&argument, parameter)| {
                            composite(module, parameter.ty)
                                && from_uniform(module, caller, argument)
                        })
                {
                    *found = Some(callee.name.clone().unwrap_or_default());
                }
                if !called.contains(function) {
                    called.push(*function);
                    calls(module, callee, &callee.body, called, found);
                }
            }
            naga::Statement::Block(inner) => calls(module, caller, inner, called, found),
            naga::Statement::If { accept, reject, .. } => {
                calls(module, caller, accept, called, found);
                calls(module, caller, reject, called, found);
            }
            naga::Statement::Switch { cases, .. } => {
                for case in cases {
                    calls(module, caller, &case.body, called, found);
                }
            }
            naga::Statement::Loop {
                body, continuing, ..
            } => {
                calls(module, caller, body, called, found);
                calls(module, caller, continuing, called, found);
            }
            _ => {}
        }
    }
}

/// Whether `ty` is a struct or an array, which a function is given whole.
fn composite(module: &naga::Module, ty: naga::Handle<naga::Type>) -> bool {
    matches!(
        module.types[ty].inner,
        naga::TypeInner::Struct { .. } | naga::TypeInner::Array { .. }
    )
}

/// Whether `expression` of `function` is read from a uniform: loaded from it, or from an element or member of it, or
/// from a variable it initialised, however deeply.
fn from_uniform(
    module: &naga::Module,
    function: &naga::Function,
    expression: naga::Handle<naga::Expression>,
) -> bool {
    match function.expressions[expression] {
        naga::Expression::GlobalVariable(global) => {
            module.global_variables[global].space == naga::AddressSpace::Uniform
        }
        naga::Expression::Load { pointer: base }
        | naga::Expression::Access { base, .. }
        | naga::Expression::AccessIndex { base, .. } => from_uniform(module, function, base),
        naga::Expression::LocalVariable(local) => {
            let mut values = Vec::from_iter(function.local_variables[local].init);
            stored(function, &function.body, local, &mut values);
            values
                .into_iter()
                .any(|value| from_uniform(module, function, value))
        }
        _ => false,
    }
}

/// What `block` of `function` stores in its variable `local`, however deeply, into `values`.
fn stored(
    function: &naga::Function,
    block: &naga::Block,
    local: naga::Handle<naga::LocalVariable>,
    values: &mut Vec<naga::Handle<naga::Expression>>,
) {
    for statement in block.iter() {
        match statement {
            naga::Statement::Store { pointer, value } => {
                if matches!(function.expressions[*pointer], naga::Expression::LocalVariable(stored) if stored == local)
                {
                    values.push(*value);
                }
            }
            naga::Statement::Block(inner) => stored(function, inner, local, values),
            naga::Statement::If { accept, reject, .. } => {
                stored(function, accept, local, values);
                stored(function, reject, local, values);
            }
            naga::Statement::Switch { cases, .. } => {
                for case in cases {
                    stored(function, &case.body, local, values);
                }
            }
            naga::Statement::Loop {
                body, continuing, ..
            } => {
                stored(function, body, local, values);
                stored(function, continuing, local, values);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_adapter_is_skipped_unless_required() {
        for required in [None, Some(""), Some("0")] {
            assert!(
                missing(&"no adapter", required)
                    .starts_with("Skipping a GPU test: no GPU adapter was found (no adapter).")
            );
        }
    }

    #[test]
    #[should_panic(
        expected = "TOMORI_REQUIRE_GPU is set but no GPU adapter was found (no adapter)"
    )]
    fn a_missing_adapter_fails_when_required() {
        missing(&"no adapter", Some("1"));
    }

    #[test]
    fn a_test_is_skipped_without_an_adapter() {
        let gpu = create(wgpu::Backends::empty());
        assert!(gpu.is_err());
        assert!(found(&gpu, None).is_none());
    }

    #[test]
    fn a_test_runs_with_the_shared_gpu() {
        with(|gpu| {
            let name = gpu.adapter.get_info().name;
            let again = found(&Ok(gpu), Some("1")).expect("the GPU found");
            assert_eq!(again.adapter.get_info().name, name);
        });
    }

    #[test]
    fn a_multiview_test_is_skipped_only_where_the_device_is_not_vulkan() {
        assert!(draws_both(true, wgpu::Backend::Vulkan));
        assert!(!draws_both(false, wgpu::Backend::Metal));
    }

    #[test]
    #[should_panic(expected = "every Vulkan device the tests run on draws both eyes in one pass")]
    fn a_vulkan_device_the_tests_run_on_draws_both_eyes_at_once() {
        draws_both(false, wgpu::Backend::Vulkan);
    }

    #[test]
    #[should_panic(expected = "scaled is given a uniform by value in a vertex shader")]
    fn a_vertex_shader_passing_a_uniform_by_value_is_refused() {
        validate_wgsl(
            "eye.wgsl",
            "struct Eye { matrix: mat4x4<f32>, pixels: f32 }
@group(0) @binding(0) var<uniform> eyes: array<Eye, 2>;
fn grown(eye: Eye) -> f32 { return eye.pixels; }
fn scaled(eye: Eye, by: f32) -> f32 {
    var total = 0.0;
    if by > 0.0 { total = grown(eye); } else { total = 1.0; }
    loop { if total > 0.0 { break; } continuing { total = total + 1.0; } }
    switch u32(by) { default: { total = total * by; } }
    { total = total + 0.0; }
    return total * by;
}
@vertex
fn vs_main(@builtin(instance_index) view: u32, @location(0) at: vec3<f32>) -> @builtin(position) vec4<f32> {
    let eye = eyes[view % 2u];
    return eye.matrix * vec4<f32>(at * scaled(eye, 2.0) * scaled(eye, 1.0), 1.0);
}
",
        );
    }

    #[test]
    #[should_panic(expected = "grown is given a uniform by value in a vertex shader")]
    fn a_vertex_shader_passing_its_uniform_by_value_is_refused() {
        validate_wgsl(
            "camera.wgsl",
            "struct Camera { matrix: mat4x4<f32>, pixels: f32 }
@group(0) @binding(0) var<uniform> camera: Camera;
fn grown(camera: Camera) -> f32 { return camera.pixels; }
@vertex
fn vs_main(@location(0) at: vec3<f32>) -> @builtin(position) vec4<f32> {
    return camera.matrix * vec4<f32>(at * grown(camera), 1.0);
}
",
        );
    }

    #[test]
    #[should_panic(expected = "grown is given a uniform by value in a vertex shader")]
    fn a_vertex_shader_passing_a_struct_nested_in_a_uniform_by_value_is_refused() {
        validate_wgsl(
            "nested.wgsl",
            "struct Camera { matrix: mat4x4<f32>, pixels: f32 }
struct Globals { time: f32, camera: Camera }
@group(0) @binding(0) var<uniform> globals: Globals;
fn grown(camera: Camera) -> f32 { return camera.pixels; }
@vertex
fn vs_main(@location(0) at: vec3<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(at * grown(globals.camera), 1.0);
}
",
        );
    }

    #[test]
    #[should_panic(expected = "grown is given a uniform by value in a vertex shader")]
    fn a_vertex_shader_passing_an_element_of_nested_uniform_arrays_by_value_is_refused() {
        validate_wgsl(
            "arrays.wgsl",
            "struct Camera { matrix: mat4x4<f32>, pixels: f32 }
struct Globals { eyes: array<array<Camera, 2>, 2> }
@group(0) @binding(0) var<uniform> globals: Globals;
fn grown(camera: Camera) -> f32 { return camera.pixels; }
@vertex
fn vs_main(@builtin(instance_index) index: u32, @location(0) at: vec3<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(at * grown(globals.eyes[index % 2u][1]), 1.0);
}
",
        );
    }

    #[test]
    #[should_panic(expected = "grown is given a uniform by value in a vertex shader")]
    fn a_vertex_shader_passing_a_copy_of_its_uniform_by_value_is_refused() {
        validate_wgsl(
            "copy.wgsl",
            "struct Camera { matrix: mat4x4<f32>, pixels: f32 }
@group(0) @binding(0) var<uniform> camera: Camera;
fn grown(camera: Camera) -> f32 { return camera.pixels; }
@vertex
fn vs_main(@builtin(instance_index) index: u32, @location(0) at: vec3<f32>) -> @builtin(position) vec4<f32> {
    var copy: Camera;
    var unused = 1.0;
    if index > 0u { unused = 2.0; } else { copy = camera; }
    switch index { default: { unused = 3.0; } }
    loop { if unused > 0.0 { break; } continuing { unused = 1.0; } }
    { unused = unused + 0.0; }
    return vec4<f32>(at * grown(copy) * unused, 1.0);
}
",
        );
    }

    #[test]
    fn a_vertex_shader_passing_a_uniform_s_fields_or_its_own_values_is_valid() {
        // A scalar uniform and a uniform's scalar, vector and matrix fields are passed as they should be; a struct made
        // in the shader, or a copy of one, and an argument passed on, are not read from a uniform.
        validate_wgsl(
            "fields.wgsl",
            "struct Camera { matrix: mat4x4<f32>, pixels: f32, eye: vec4<f32> }
@group(0) @binding(0) var<uniform> time: f32;
@group(0) @binding(1) var<uniform> camera: Camera;
fn twice(value: f32) -> f32 { return value * 2.0; }
fn placed(matrix: mat4x4<f32>, eye: vec3<f32>, at: vec3<f32>) -> vec4<f32> { return matrix * vec4<f32>(at - eye, 1.0); }
fn grown(camera: Camera) -> f32 { return camera.pixels; }
fn passed(camera: Camera) -> f32 { return grown(camera); }
@vertex
fn vs_main(@location(0) at: vec3<f32>) -> @builtin(position) vec4<f32> {
    let made = Camera(camera.matrix, twice(time), camera.eye);
    var copy = made;
    let grown = passed(made) + grown(copy);
    return placed(camera.matrix, camera.eye.xyz, at * twice(camera.pixels) * grown);
}
",
        );
    }

    #[test]
    #[should_panic(expected = "broken.wgsl")]
    fn invalid_wgsl_names_the_shader() {
        validate_wgsl("broken.wgsl", "fn main() -> f32 { return true; }");
    }

    #[test]
    #[should_panic(expected = "unparsable.wgsl")]
    fn unparsable_wgsl_names_the_shader() {
        validate_wgsl("unparsable.wgsl", "fn main( {");
    }
}
