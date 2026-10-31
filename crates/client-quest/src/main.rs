// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

#[cfg(not(target_os = "android"))]
fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    match std::env::args().nth(1).as_deref() {
        None => tomori::run(),
        Some("--xr") => tomori::run_xr(),
        Some("--spectator") => tomori::run_spectator(),
        Some("--spectator-pipewire") => tomori::run_spectator_pipewire(),
        Some("--spectator-pipewire-preview") => tomori::run_spectator_pipewire_preview(),
        Some("--licenses") => {
            println!(
                "Bundled WebXR Input Profiles controller assets:\n\n{}\n\nBasis Universal transcoder:\n\n{}",
                include_str!("../assets/controllers/LICENSE.md"),
                include_str!("../assets/licenses/basisu-LICENSE.txt")
            );
            Ok(())
        }
        Some("--help" | "-h") => {
            println!(
                "tomori [--xr|--spectator|--spectator-pipewire|--spectator-pipewire-preview|--licenses]\n\nDefault: desktop window (no headset needed).\n--xr: use the desktop OpenXR runtime.\n--spectator: follow the connected Quest in a window.\n--spectator-pipewire: headless PipeWire video source (Linux; Ctrl-C to stop).\n--spectator-pipewire-preview: PipeWire video source with preview window.\n--licenses: show the bundled controller assets' license.\n\nDesktop controls: left mouse attract, right drag orbit, wheel zoom, E environment, R reset, Space pause, Esc quit.\nTOMORI_ENVIRONMENT=sky|minimal selects the initial environment (default sky). Spectators follow the headset."
            );
            Ok(())
        }
        Some(arg) => anyhow::bail!("Unknown option {arg:?}; use --help"),
    }
}

#[cfg(target_os = "android")]
fn main() {}
