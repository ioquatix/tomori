// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The dependency rule between Tomori's core crates, its environments' crates and its apps, as `design/environments.md` requires: an environment's common and server crates build no renderer or GPU stack, its common and client crates build no server crate, and no core crate depends on an environment's crate or on an app, in any kind of dependency, development dependencies included, or builds an environment's crate with every feature but its `app`, so that environments register themselves with the apps rather than the core knowing them.
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// The workspace's manifest.
fn manifest() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../Cargo.toml"))
}

/// `cargo` with `arguments`, for the workspace, and what it prints.
fn cargo(arguments: &[&str]) -> String {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(arguments)
        .arg("--manifest-path")
        .arg(manifest())
        .output()
        .expect("cargo runs");
    assert!(
        output.status.success(),
        "cargo {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("cargo prints UTF-8")
}

/// The workspace's packages, as `cargo metadata` describes them, without their dependencies' own.
fn packages() -> Vec<Value> {
    let metadata: Value = serde_json::from_str(&cargo(&[
        "metadata",
        "--locked",
        "--format-version",
        "1",
        "--no-deps",
    ]))
    .expect("cargo metadata prints JSON");
    metadata["packages"]
        .as_array()
        .expect("cargo metadata lists packages")
        .clone()
}

/// What a workspace package is, by its name.
#[derive(Debug, PartialEq, Eq)]
enum Role {
    /// An environment's crate: `tomori-environment-NAME`, its common crate, or `…-server` or `…-client`, its halves. `tomori-environment`, the interface of a server side, is core.
    Environment(Half),
    /// One of Tomori's own programs, which registers Tomori's own environments: `tomori-server-app`.
    App,
    /// An example of a downstream project, or the private development tasks, to which the rule does not apply.
    Exempt,
    /// Everything else: the framework, which registers no environment.
    Core,
}

/// Which half of an environment a crate is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Half {
    Common,
    Server,
    Client,
}

fn role(name: &str) -> Role {
    if let Some(environment) = name.strip_prefix("tomori-environment-") {
        Role::Environment(if environment.ends_with("-server") {
            Half::Server
        } else if environment.ends_with("-client") {
            Half::Client
        } else {
            Half::Common
        })
    } else if name == "tomori-server-app" {
        Role::App
    } else if name.starts_with("tomori-example-") || name == "tomori-bake" {
        Role::Exempt
    } else {
        Role::Core
    }
}

/// What builds a renderer or the GPU stack, which an environment's common and server crates must not: Tomori's renderer and the clients built on it, wgpu and what it is built from, and the headset's and windows' platforms.
const RENDERING: &[&str] = &[
    "tomori-render",
    "tomori-client-desktop",
    "tomori-client-spectator",
    "tomori-client-quest",
    "wgpu",
    "wgpu-core",
    "wgpu-hal",
    "naga",
    "ash",
    "openxr",
    "winit",
];

#[test]
fn packages_are_told_apart_by_their_names() {
    assert_eq!(
        role("tomori-environment-sky"),
        Role::Environment(Half::Common)
    );
    assert_eq!(
        role("tomori-environment-sky-client"),
        Role::Environment(Half::Client)
    );
    assert_eq!(
        role("tomori-environment-github-server"),
        Role::Environment(Half::Server)
    );
    assert_eq!(
        role("tomori-environment-farmyard"),
        Role::Environment(Half::Common)
    );
    assert_eq!(
        role("tomori-environment-farmyard-client"),
        Role::Environment(Half::Client)
    );
    assert_eq!(role("tomori-environment"), Role::Core);
    assert_eq!(role("tomori-server"), Role::Core);
    assert_eq!(role("tomori-server-app"), Role::App);
    assert_eq!(role("tomori-example-headset"), Role::Exempt);
}

/// Tomori's own environments' crates, each by its role, which the rule must find among the workspace's packages, so that one renamed out of the convention, which the rule would then take for core or skip, fails here rather than going unchecked.
const ENVIRONMENTS: &[(&str, Half)] = &[
    ("tomori-environment-farmyard", Half::Common),
    ("tomori-environment-farmyard-client", Half::Client),
    ("tomori-environment-github", Half::Common),
    ("tomori-environment-github-client", Half::Client),
    ("tomori-environment-github-server", Half::Server),
    ("tomori-environment-sky", Half::Common),
    ("tomori-environment-sky-client", Half::Client),
];

#[test]
fn the_rule_checks_every_one_of_tomoris_environments_crates() {
    let mut found: Vec<(String, Role)> = packages()
        .iter()
        .map(|package| package["name"].as_str().unwrap().to_owned())
        .map(|name| {
            let role = role(&name);
            (name, role)
        })
        .filter(|(_, role)| matches!(role, Role::Environment(_)))
        .collect();
    found.sort_by(|(left, _), (right, _)| left.cmp(right));
    let expected: Vec<(String, Role)> = ENVIRONMENTS
        .iter()
        .map(|(name, half)| ((*name).to_owned(), Role::Environment(*half)))
        .collect();
    assert_eq!(found, expected);
}

#[test]
fn an_environments_common_and_server_crates_build_no_renderer() {
    let mut checked = 0;
    for package in packages() {
        let name = package["name"].as_str().unwrap();
        if !matches!(role(name), Role::Environment(Half::Common | Half::Server)) {
            continue;
        }
        checked += 1;
        let rendering: Vec<String> = tree(name, "normal,build", &["--all-features"])
            .into_iter()
            .filter(|dependency| RENDERING.contains(&dependency.as_str()))
            .collect();
        assert!(
            rendering.is_empty(),
            "{name} depends on {rendering:?}; an environment's renderer belongs in its client crate"
        );
    }
    assert!(checked > 0, "no environment's common crate was found");
}

#[test]
fn no_core_crate_depends_on_an_environment_or_an_app() {
    let mut checked = 0;
    for package in packages() {
        let name = package["name"].as_str().unwrap();
        if role(name) != Role::Core {
            continue;
        }
        checked += 1;
        // The dependencies named only by the `app` feature, which is Tomori's own app in a core crate whose package also builds it: the `tomori` executable and APK, and the `tomori-spectator` executable.
        let app: Vec<&str> = package["features"]["app"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|enabled| enabled.as_str()?.strip_prefix("dep:"))
            .collect();
        let named_elsewhere = |dependency: &str| {
            package["features"]
                .as_object()
                .unwrap()
                .iter()
                .filter(|(feature, _)| *feature != "app")
                .flat_map(|(_, enabled)| enabled.as_array().unwrap())
                .filter_map(Value::as_str)
                .any(|enabled| {
                    enabled == format!("dep:{dependency}")
                        || enabled.starts_with(&format!("{dependency}/"))
                        || enabled.starts_with(&format!("{dependency}?/"))
                })
        };
        for dependency in package["dependencies"].as_array().unwrap() {
            let dependency_name = dependency["name"].as_str().unwrap();
            let development = dependency["kind"].as_str() == Some("dev");
            match role(dependency_name) {
                Role::Environment(_) => {
                    // Only Tomori's own app may name an environment, optionally, through `app` alone, and never as a development dependency, as the apps' agreement is tested in `tomori-server-app`.
                    let through_app = dependency["optional"].as_bool() == Some(true)
                        && !development
                        && app.contains(&dependency_name)
                        && !named_elsewhere(dependency_name);
                    assert!(
                        through_app,
                        "{name} depends on {dependency_name}; register environments in an app, not in the core"
                    );
                }
                Role::App => panic!(
                    "{name} depends on {dependency_name}, which registers environments; test the apps in the app"
                ),
                Role::Core | Role::Exempt => {}
            }
        }
    }
    assert!(checked > 0, "no core crate was found");
}

/// The packages `name` builds, as `cargo tree` lists them, with `edges` and `options`, for every target.
fn tree(name: &str, edges: &str, options: &[&str]) -> std::collections::BTreeSet<String> {
    let mut arguments = vec![
        "tree",
        "--locked",
        "--package",
        name,
        "--edges",
        edges,
        "--target",
        "all",
    ];
    arguments.extend_from_slice(options);
    arguments.extend_from_slice(&["--prefix", "none", "--format", "{p}"]);
    cargo(&arguments)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// `package`'s features that do not turn on its `app` feature, directly or through another of its features: every feature but Tomori's own app.
fn features_without_app(package: &Value) -> Vec<String> {
    let features = package["features"].as_object().unwrap();
    // Whether `feature` turns on `app`, following the package's own features it turns on, which Cargo keeps free of cycles.
    fn enables_app(features: &serde_json::Map<String, Value>, feature: &str) -> bool {
        feature == "app"
            || features[feature]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .filter(|enabled| features.contains_key(*enabled))
                .any(|enabled| enables_app(features, enabled))
    }
    features
        .keys()
        .filter(|feature| !enables_app(features, feature))
        .cloned()
        .collect()
}

#[test]
fn without_their_app_no_core_crate_builds_an_environment() {
    let mut checked = 0;
    for package in packages() {
        let name = package["name"].as_str().unwrap();
        if role(name) != Role::Core {
            continue;
        }
        checked += 1;
        // Every feature but `app`, and its development dependencies too, so that a core crate reaching an environment through another crate, such as another core crate's own `app`, fails too.
        let features = features_without_app(&package).join(",");
        let mut options = vec!["--no-default-features"];
        if !features.is_empty() {
            options.extend_from_slice(&["--features", &features]);
        }
        let environments: Vec<String> = tree(name, "normal,build,dev", &options)
            .into_iter()
            .filter(|dependency| matches!(role(dependency), Role::Environment(_)))
            .collect();
        assert!(
            environments.is_empty(),
            "{name} without its app builds {environments:?}; register environments in an app, not in the core"
        );
    }
    assert!(checked > 0, "no core crate was found");
}

#[test]
fn an_environments_server_crate_is_built_only_by_servers() {
    let mut checked = 0;
    for package in packages() {
        let name = package["name"].as_str().unwrap();
        let Role::Environment(half @ (Half::Common | Half::Client)) = role(name) else {
            continue;
        };
        checked += 1;
        // A server crate holds what never reaches a client, such as credentials, and a common crate is shared by both halves, so neither builds a server crate, nor a common crate a client's.
        let halves: Vec<String> = tree(name, "normal,build", &["--all-features"])
            .into_iter()
            .filter(|dependency| match role(dependency) {
                Role::Environment(Half::Server) => true,
                Role::Environment(Half::Client) => half == Half::Common,
                _ => false,
            })
            .collect();
        assert!(
            halves.is_empty(),
            "{name} builds {halves:?}; an environment's server half stays in servers, and its client half in clients"
        );
    }
    assert!(
        checked > 0,
        "no environment's common or client crate was found"
    );
}
