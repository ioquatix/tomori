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
    // With the GPU's timestamps where it has them, as lavapipe does, so that the frame profiler is tested timing.
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: adapter.features() & crate::profiler::FEATURES,
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

/// Parses and validates WGSL `source` with naga, as wgpu does when it creates a shader module, so that a shader is checked where no adapter can compile it: after the shared haze functions, `tomori_assets::haze::shader`, as every pipeline that draws in the world compiles its own, unless it begins with them already or is them.
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
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string_with_path(source, label)));
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::default(),
    )
    .validate(&module)
    .unwrap_or_else(|error| panic!("{}", error.emit_to_string_with_path(source, label)));
    if let Some((function, uniform)) = whole_uniform_value(&module, &info) {
        panic!(
            "{label}: {function} reads a struct or an array of the uniform `{uniform}` whole, as a value, which the \
             Quest's driver cannot always compile: read the scalar, vector and matrix fields it uses where it uses them"
        );
    }
}

/// The name of a function or an entry point of `module`, of any stage, that reads a struct or an array of a uniform
/// whole, as a value, and the uniform's name; or none. Such a value is the uniform itself, an element of a uniform
/// array or a member of a uniform struct, however deeply nested, such as an eye's camera, loaded whole: to be given to
/// a function, returned, bound by `let` or `var`, assigned, or put in another value. Reading its scalar, vector and
/// matrix fields through the uniform loads only those. The Quest 3's Adreno driver fails to make a pipeline drawing
/// both eyes by multiview of a vertex shader giving a function an eye's camera so, answering `vkCreateGraphicsPipelines`
/// with `VK_ERROR_UNKNOWN`, which wgpu takes, silently, as the device lost.
fn whole_uniform_value(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
) -> Option<(String, String)> {
    let functions = module
        .functions
        .iter()
        .map(|(handle, function)| (function, &info[handle]));
    let entry_points = module
        .entry_points
        .iter()
        .enumerate()
        .map(|(index, entry)| (&entry.function, info.get_entry_point(index)));
    functions.chain(entry_points).find_map(|(function, info)| {
        function
            .expressions
            .iter()
            .find_map(|(handle, expression)| match *expression {
                naga::Expression::Load { pointer }
                    if matches!(
                        info[handle].ty.inner_with(&module.types),
                        naga::TypeInner::Struct { .. } | naga::TypeInner::Array { .. }
                    ) =>
                {
                    uniform(module, function, pointer)
                        .map(|uniform| (function.name.clone().unwrap_or_default(), uniform))
                }
                _ => None,
            })
    })
}

/// The name of the uniform `pointer` of `function` points into, through elements and members however deeply, or none
/// if it points elsewhere.
fn uniform(
    module: &naga::Module,
    function: &naga::Function,
    pointer: naga::Handle<naga::Expression>,
) -> Option<String> {
    match function.expressions[pointer] {
        naga::Expression::GlobalVariable(global) => {
            let global = &module.global_variables[global];
            (global.space == naga::AddressSpace::Uniform)
                .then(|| global.name.clone().unwrap_or_default())
        }
        naga::Expression::Access { base, .. } | naga::Expression::AccessIndex { base, .. } => {
            uniform(module, function, base)
        }
        _ => None,
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
    #[should_panic(expected = "twice reads a struct or an array of the uniform `camera` whole")]
    fn a_vertex_shader_passing_its_uniform_by_value_is_refused() {
        validate_wgsl(
            "camera.wgsl",
            "struct Camera { matrix: mat4x4<f32>, pixels: f32 }
@group(0) @binding(0) var<uniform> camera: Camera;
fn grown(camera: Camera) -> f32 { return camera.pixels; }
fn twice() -> f32 { return grown(camera) * 2.0; }
@vertex
fn vs_main(@location(0) at: vec3<f32>) -> @builtin(position) vec4<f32> {
    return camera.matrix * vec4<f32>(at * twice(), 1.0);
}
",
        );
    }

    #[test]
    #[should_panic(expected = "vs_main reads a struct or an array of the uniform `eyes` whole")]
    fn a_vertex_shader_passing_an_element_of_a_uniform_array_by_value_is_refused() {
        validate_wgsl(
            "eyes.wgsl",
            "struct Eye { matrix: mat4x4<f32>, pixels: f32 }
@group(0) @binding(0) var<uniform> eyes: array<Eye, 2>;
fn grown(eye: Eye) -> f32 { return eye.pixels; }
@vertex
fn vs_main(@builtin(instance_index) view: u32, @location(0) at: vec3<f32>) -> @builtin(position) vec4<f32> {
    return eyes[view % 2u].matrix * vec4<f32>(at * grown(eyes[view % 2u]), 1.0);
}
",
        );
    }

    #[test]
    #[should_panic(expected = "fs_main reads a struct or an array of the uniform `globals` whole")]
    fn a_fragment_shader_binding_a_struct_nested_in_a_uniform_by_let_is_refused() {
        validate_wgsl(
            "nested.wgsl",
            "struct Light { colour: vec4<f32>, strength: f32 }
struct Globals { time: f32, light: Light }
@group(0) @binding(0) var<uniform> globals: Globals;
@fragment
fn fs_main() -> @location(0) vec4<f32> {
    let light = globals.light;
    return light.colour * light.strength;
}
",
        );
    }

    #[test]
    #[should_panic(expected = "light reads a struct or an array of the uniform `lights` whole")]
    fn a_fragment_shader_returning_an_element_of_nested_uniform_arrays_is_refused() {
        validate_wgsl(
            "arrays.wgsl",
            "struct Light { colour: vec4<f32>, strength: f32 }
struct Lights { all: array<array<Light, 2>, 2> }
@group(0) @binding(0) var<uniform> lights: Lights;
fn light(index: u32) -> Light { return lights.all[index % 2u][1]; }
@fragment
fn fs_main(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    return light(u32(at.x)).colour;
}
",
        );
    }

    #[test]
    #[should_panic(expected = "cs_main reads a struct or an array of the uniform `settings` whole")]
    fn a_compute_shader_copying_its_uniform_into_a_variable_is_refused() {
        validate_wgsl(
            "copy.wgsl",
            "struct Settings { scale: f32, offsets: array<vec4<f32>, 4> }
@group(0) @binding(0) var<uniform> settings: Settings;
@group(0) @binding(1) var<storage, read_write> out: array<f32>;
@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    var copy: Settings;
    copy = settings;
    out[id.x] = copy.scale;
}
",
        );
    }

    #[test]
    #[should_panic(expected = "cs_main reads a struct or an array of the uniform `settings` whole")]
    fn a_compute_shader_binding_a_uniform_array_by_var_is_refused() {
        validate_wgsl(
            "offsets.wgsl",
            "struct Settings { scale: f32, offsets: array<vec4<f32>, 4> }
@group(0) @binding(0) var<uniform> settings: Settings;
@group(0) @binding(1) var<storage, read_write> out: array<f32>;
@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    var offsets = settings.offsets;
    out[id.x] = offsets[id.x % 4u].x;
}
",
        );
    }

    #[test]
    fn shaders_reading_a_uniform_s_fields_or_their_own_values_are_valid() {
        // A scalar uniform, a uniform's scalar, vector and matrix fields, and an element of a uniform array of vectors
        // are read as they should be, in every stage; a struct made in the shader, or a copy of one, an argument passed
        // on, and a struct read whole from a storage buffer are not read from a uniform.
        validate_wgsl(
            "fields.wgsl",
            "struct Camera { matrix: mat4x4<f32>, pixels: f32, eye: vec4<f32>, offsets: array<vec4<f32>, 2> }
@group(0) @binding(0) var<uniform> time: f32;
@group(0) @binding(1) var<uniform> camera: Camera;
@group(0) @binding(2) var<storage, read_write> stored: Camera;
fn twice(value: f32) -> f32 { return value * 2.0; }
fn placed(matrix: mat4x4<f32>, eye: vec3<f32>, at: vec3<f32>) -> vec4<f32> { return matrix * vec4<f32>(at - eye, 1.0); }
fn grown(camera: Camera) -> f32 { return camera.pixels; }
fn passed(camera: Camera) -> f32 { return grown(camera); }
@vertex
fn vs_main(@builtin(instance_index) index: u32, @location(0) at: vec3<f32>) -> @builtin(position) vec4<f32> {
    let made = Camera(camera.matrix, twice(time), camera.eye, array(camera.offsets[0], camera.offsets[index % 2u]));
    var copy = made;
    let grown = passed(made) + grown(copy) + grown(stored);
    return placed(camera.matrix, camera.eye.xyz, at * twice(camera.pixels) * grown);
}
@fragment
fn fs_main() -> @location(0) vec4<f32> {
    let matrix = camera.matrix;
    return matrix[0] * camera.pixels + camera.offsets[1];
}
@compute @workgroup_size(1)
fn cs_main() {
    stored.pixels = camera.pixels * time;
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
