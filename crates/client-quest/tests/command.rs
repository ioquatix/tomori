// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The `tomori` executable: its command line, and each mode as far as it gets without a display, an OpenXR runtime or PipeWire, as on a CI runner.
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::RwLock;

/// Held to start a process, and exclusively to write a program that a test then runs: a child forked while the program is open for writing keeps it open until it executes its own, and running the program meanwhile fails with "Text file busy".
static SPAWN: RwLock<()> = RwLock::new(());

/// `tomori` with `arguments`, in an environment without a display, an OpenXR runtime, a server or the user's PipeWire settings; with `path` for its `PATH`, if given.
fn tomori(arguments: &[&str], path: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tomori"));
    command
        .args(arguments)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("TOMORI_SERVER")
        .env_remove("TOMORI_TOKEN")
        .env_remove("PIPEWIRE_ALSA")
        .env_remove("PIPEWIRE_NODE")
        .env("XR_RUNTIME_JSON", "/nonexistent/openxr_runtime.json")
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let child = {
        let _spawning = SPAWN.read().unwrap();
        command.spawn().expect("tomori starts")
    };
    child.wait_with_output().expect("tomori runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A new directory under the target directory, for this test alone.
fn directory(name: &str) -> PathBuf {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn help_lists_the_modes_and_the_desktop_controls() {
    let output = tomori(&["--help"], None);
    assert!(output.status.success());
    assert!(stdout(&output).starts_with("tomori [MODE]"));
    assert!(stdout(&output).contains("--desktop controls:"));
}

#[test]
fn licenses_name_the_bundled_assets() {
    let output = tomori(&["--licenses"], None);
    assert!(output.status.success());
    assert!(stdout(&output).contains("Basis Universal transcoder:"));
    assert!(stdout(&output).contains("SIL OPEN FONT LICENSE Version 1.1"));
    // And the notices of what Tomori's own app's environments bundle, as their registration added them.
    let bundled = tomori::app::builder().sceneries().licenses();
    assert!(!bundled.is_empty());
    for (what, notice) in bundled {
        assert!(
            stdout(&output).contains(&format!("\n\n{what}:\n\n{notice}")),
            "{what}"
        );
    }
}

#[test]
fn a_mistaken_command_line_is_refused() {
    let output = tomori(&["--fly"], None);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("Unknown option \"--fly\"; use --help"));
}

#[test]
fn the_window_modes_need_a_display() {
    for arguments in [
        &[][..],
        &["--desktop", "--name", "Ada"],
        &["--spectator", "--audio-output", "nowhere"],
        &["--spectator-pipewire-preview"],
    ] {
        let output = tomori(arguments, None);
        assert!(!output.status.success(), "{arguments:?}");
    }
}

#[test]
fn the_xr_mode_needs_an_openxr_runtime() {
    let output = tomori(&["--xr"], None);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("OpenXR"));
}

#[test]
fn the_console_shows_its_commands_and_ends_with_its_input() {
    let output = tomori(&["--console"], None);
    assert!(output.status.success());
    assert!(stdout(&output).starts_with("Commands, one or more per line:"));
}

#[test]
fn discover_browses_the_network_for_servers() {
    let output = tomori(&["discover"], None);
    assert!(output.status.success());
    let stdout = stdout(&output);
    assert!(stdout.starts_with("No servers found") || stdout.starts_with("Servers:"));
}

#[cfg(target_os = "linux")]
#[test]
fn the_pipewire_source_stops_when_gstreamer_does() {
    use std::os::unix::fs::PermissionsExt;
    // The spectator opens a headless Vulkan adapter before it starts GStreamer, so this is a GPU test: skipped where there is no adapter, unless `TOMORI_REQUIRE_GPU` is set.
    tomori_render::test_gpu::with(|_| {
        // GStreamer that takes the video for a moment, then stops, as when PipeWire goes away.
        let bin = directory("gstreamer-stops");
        let gstreamer = bin.join("gst-launch-1.0");
        {
            let _writing = SPAWN.write().unwrap();
            std::fs::write(&gstreamer, "#!/bin/sh\nexec /bin/sleep 1\n").unwrap();
            std::fs::set_permissions(&gstreamer, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = tomori(&["--spectator-pipewire"], Some(&bin));
        assert!(!output.status.success());
        let stderr = stderr(&output);
        assert!(stderr.contains("GStreamer exited"), "{stderr}");
    });
}

#[test]
fn discover_lists_the_spaces_on_a_server_given_to_it() {
    let server = tomori_server::test_support::TestServer::start();
    let mut ada = server.connect("ada", "Ada");
    let space = ada.create_space("Studio");
    let output = tomori(&["discover", "--server", &server.address.to_string()], None);
    assert!(output.status.success());
    assert!(stdout(&output).contains(&format!("{:<12} Studio", space.id)));
    assert!(
        stderr(&output)
            .contains("Join one with: tomori-spectator --server ADDRESS --space SPACE_ID")
    );
}
