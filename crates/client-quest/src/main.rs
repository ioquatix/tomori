// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

#[cfg(not(target_os = "android"))]
fn main() -> anyhow::Result<()> {
    use anyhow::Context;

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut overrides = tomori::Overrides {
        server: std::env::var("TOMORI_SERVER").ok(),
        name: None,
    };
    let mut mode = None;
    let mut overridden = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--server" => {
                overrides.server = Some(arguments.next().context("--server needs an address")?);
                overridden = true;
            }
            "--name" => {
                overrides.name = Some(arguments.next().context("--name needs a name")?);
                overridden = true;
            }
            _ if mode.is_none() => mode = Some(argument),
            _ => anyhow::bail!("Unexpected argument {argument:?}; use --help"),
        }
    }
    if overridden && !matches!(mode.as_deref(), Some("--xr" | "--console")) {
        // Only the headset's panel connects to a server.
        anyhow::bail!("--server and --name apply only to --xr and --console; use --help");
    }
    match mode.as_deref() {
        None => tomori::run(),
        Some("--xr") => tomori::run_xr(overrides),
        Some("--console") => tomori::run_console(overrides),
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
                "tomori [--xr|--console|--spectator|--spectator-pipewire|--spectator-pipewire-preview|--licenses] [--server ADDRESS] [--name NAME]\n\nDefault: desktop window (no headset needed).\n--xr: use the desktop OpenXR runtime.\n--console: show the headset's panel in the terminal, driven by typed commands.\n--spectator: show frames published to the PC bridge in a window; the headset does not publish them yet.\n--spectator-pipewire: headless PipeWire video source (Linux; Ctrl-C to stop).\n--spectator-pipewire-preview: PipeWire video source with preview window.\n--licenses: show the bundled controller assets' license.\n--server: with --xr or --console, connect to this Tomori server (host:port) instead of choosing one; TOMORI_SERVER does the same.\n--name: with --xr or --console, the display name other participants see (default: this computer's name).\n\nDesktop controls: left mouse attract, right drag orbit, wheel zoom, E environment, R reset, Space pause, Esc quit.\nTOMORI_ENVIRONMENT=sky|minimal selects the initial environment (default sky)."
            );
            Ok(())
        }
        Some(argument) => anyhow::bail!("Unknown option {argument:?}; use --help"),
    }
}

#[cfg(target_os = "android")]
fn main() {}
