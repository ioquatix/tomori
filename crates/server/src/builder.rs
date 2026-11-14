// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::process::ExitCode;
use std::time::Instant;

use tomori_backend::Backend;

use crate::command::{self, Arguments, ENGINE_STOP, Engines};
use crate::environments::{Environment, Environments};
use crate::{AnyBackend, CommandOption, Server};

/// Makes the backend registered under a name, from the command line.
type Constructor = Box<dyn Fn(&Arguments) -> Result<AnyBackend, String>>;

/// Changes a server once the command line has configured it.
type Hook = Box<dyn FnOnce(Server) -> Server>;

/// Builds and runs a Tomori server from the `tomori-server` command line, with the backends and environments registered on it.
///
/// [`Server::builder`] starts with Tomori's own backends, `fake` and `opencode`, as their Cargo features allow, and no environments: every environment registers itself, each by the identity in its own crate, as the `tomori-server` program, in the `tomori-server-app` package, registers Sky and the Minimal grid. Another project builds its own server the same way, registering its own, without changing Tomori. Its executable then takes the same command line as `tomori-server`, including `auth`, serves the same way, with token authentication, TLS, Bonjour and speech, and offers what it registered: `--backend NAME` names a registered backend, and clients are offered every registered environment.
///
/// ```no_run
/// use std::process::ExitCode;
///
/// use tomori_server::{Environment, Server};
///
/// fn main() -> ExitCode {
///     Server::builder()
///         .environment(
///             Environment::new("observe", "Observe")
///                 .with_instructions("The space shows the service's topology."),
///         )
///         .configure(|server| server.with_max_visitors(8))
///         .main()
/// }
/// ```
///
/// Environments here are what the server knows of them today: an identifier, label and version offered to clients, their layer of a space's instructions, and their server sides. Their data sources and tools, planned in [#83](https://github.com/base-rs/tomori/issues/83) and [#82](https://github.com/base-rs/tomori/issues/82), will be registered here too once Tomori has them.
pub struct Builder {
    /// The program's name, which its usage gives: `tomori-server` unless [`Builder::program_name`] gives another.
    program: String,
    /// Each backend's name and how to make it, in the order `--help` lists them.
    backends: Vec<(String, Constructor)>,
    environments: Environments,
    /// The application's own command-line options, in the order `--help` lists them and they are applied.
    options: Vec<CommandOption>,
    hooks: Vec<Hook>,
}

impl Default for Builder {
    /// Tomori's own backends, and no environments.
    fn default() -> Self {
        let builder = Self {
            program: "tomori-server".to_owned(),
            backends: Vec::new(),
            environments: Environments::default(),
            options: Vec::new(),
            hooks: Vec::new(),
        };
        #[cfg(feature = "fake")]
        let builder = builder.with_constructor(
            "fake",
            Box::new(|_| Ok(AnyBackend::new(crate::fake::FakeBackend::new()))),
        );
        builder.with_constructor("opencode", Box::new(opencode))
    }
}

/// The OpenCode backend, reaching `opencode serve` where the command line says.
#[cfg(feature = "opencode")]
fn opencode(arguments: &Arguments) -> Result<AnyBackend, String> {
    Ok(AnyBackend::new(tomori_backend_opencode::OpenCode::new(
        tomori_backend_opencode::Config {
            url: arguments.opencode_url.clone(),
        },
    )))
}

/// Named, so that `--backend opencode` says what is missing rather than that it is unknown.
#[cfg(not(feature = "opencode"))]
fn opencode(_arguments: &Arguments) -> Result<AnyBackend, String> {
    Err("the opencode backend needs a server built with the opencode feature".to_owned())
}

impl Server {
    /// A builder for a server with Tomori's own backends and no environments, to register environments and more backends on and run; see [`Builder`].
    pub fn builder() -> Builder {
        Builder::default()
    }
}

impl Builder {
    /// Offer the backend `name`, made by `create` when the command line names it with `--backend`, in place of one already registered under that name. `create` reads any configuration of its own, such as environment variables, and fails with a message that the server reports as it stops. The backend's [`Backend::name`] must be `name`, which clients name it by.
    ///
    /// ```
    /// use tomori_server::Server;
    /// use tomori_server::fake::FakeBackend;
    ///
    /// // `tomori-server --backend fake` offers the scripted backend, which tests use.
    /// let builder = Server::builder().backend("fake", || Ok(FakeBackend::new()));
    /// ```
    pub fn backend<B: Backend + 'static>(
        self,
        name: impl Into<String>,
        create: impl Fn() -> Result<B, String> + 'static,
    ) -> Self {
        self.with_constructor(name, Box::new(move |_| create().map(AnyBackend::new)))
    }

    fn with_constructor(mut self, name: impl Into<String>, constructor: Constructor) -> Self {
        let name = name.into();
        match self
            .backends
            .iter_mut()
            .find(|(registered, _)| *registered == name)
        {
            Some((_, registered)) => *registered = constructor,
            None => self.backends.push((name, constructor)),
        }
        self
    }

    /// Name the program `name` in its usage, as `--help` and `auth --help` show it and as the server says to run `auth`, in place of `tomori-server`, such as an application's own executable's name. A name must be a plain executable name: one that is empty or holds whitespace, a control character or a zero-width character is not used, with a warning, and the program keeps the name it had.
    ///
    /// ```
    /// use tomori_server::Server;
    ///
    /// // `observe-server --help` starts `Usage: observe-server`.
    /// let builder = Server::builder().program_name("observe-server");
    /// ```
    pub fn program_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        match unusable(&name) {
            None => self.program = name,
            Some(why) => log::warn!("The program name {name:?} is ignored, since {why}."),
        }
        self
    }

    /// The program's name, which its usage gives: `tomori-server` unless [`Builder::program_name`] gave another.
    pub(crate) fn program(&self) -> &str {
        &self.program
    }

    /// Offer `environment` for spaces, in place of one with the same identifier, or else after those already offered. The first offered is the one a space whose creator names none is bound to. See [`Environment`].
    pub fn environment(mut self, environment: Environment) -> Self {
        self.environments = self.environments.with(environment);
        self
    }

    /// The environments registered so far, in the order they are offered.
    ///
    /// ```
    /// use tomori_server::{Environment, Server};
    ///
    /// let builder = Server::builder().environment(Environment::new("observe", "Observe"));
    /// assert_eq!(builder.environments()[0].id(), "observe");
    /// assert!(Server::builder().environments().is_empty());
    /// ```
    pub fn environments(&self) -> &[Environment] {
        self.environments.as_slice()
    }

    /// Add `option` to the command line, in place of one with the same flag, or else after those already added: an option of the application's own, such as an environment's configuration, which `--help` lists after Tomori's own, and which changes the server once the command line has configured it, before the hooks run. See [`CommandOption`].
    pub fn option(mut self, option: CommandOption) -> Self {
        match self
            .options
            .iter_mut()
            .find(|added| added.flag() == option.flag())
        {
            Some(added) => *added = option,
            None => self.options.push(option),
        }
        self
    }

    /// The application's own command-line options, in order.
    pub(crate) fn options(&self) -> &[CommandOption] {
        &self.options
    }

    /// Change the server once the command line has configured it, and before it serves, with `hook`, such as to set a limit with one of [`Server`]'s `with_` methods. Hooks run in the order they were given. Serving replaces the server's [`Server::with_counts`] callback with the one that advertises them.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use tomori_server::Server;
    ///
    /// let builder = Server::builder()
    ///     .configure(|server| server.with_participant_retention(Duration::from_secs(60 * 60)));
    /// ```
    pub fn configure(mut self, hook: impl FnOnce(Server) -> Server + 'static) -> Self {
        self.hooks.push(Box::new(hook));
        self
    }

    /// Run the command line `command`, the arguments after the program's name, as `tomori-server` does: serve until SIGINT or SIGTERM, or manage tokens with `auth`. Problems are reported on standard error, and the exit code says whether it succeeded. Once the server has drained, its speech engines are stopped, and if they do not stop in time, the process exits at once, as `design/server.md` describes.
    pub fn run(self, command: impl IntoIterator<Item = String>) -> ExitCode {
        let mut engines = Engines::default();
        let code = command::run(
            self,
            command.into_iter(),
            command::default_state_directory(),
            &mut engines,
        );
        let stopped = engines.stop(Instant::now() + ENGINE_STOP);
        #[cfg(unix)]
        let exit = crate::exit::now;
        // Elsewhere, the server waits for the engines.
        #[cfg(not(unix))]
        let exit = |_| ();
        command::finish(code, stopped, exit)
    }

    /// Run the process's command line, as [`run`](Self::run) does, logging to standard error at `info` and above unless `RUST_LOG` says otherwise, unless the program has already set up logging. `tomori-server`'s `main` runs this, on a builder registering Tomori's own environments.
    pub fn main(self) -> ExitCode {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init();
        self.run(std::env::args().skip(1))
    }

    /// The command line's usage, naming the program and the registered backends, and listing the registered environments, in the order they are offered.
    pub(crate) fn usage(&self) -> String {
        let names: Vec<&str> = self
            .backends
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        command::usage(
            &self.program,
            &names.join(", "),
            self.environments(),
            &self.options,
            cfg!(feature = "speech"),
        )
    }

    /// The backend registered as `name`, made as `arguments` say.
    pub(crate) fn backend_named(
        &self,
        name: &str,
        arguments: &Arguments,
    ) -> Result<AnyBackend, String> {
        let (_, constructor) = self
            .backends
            .iter()
            .find(|(registered, _)| registered == name)
            .ok_or_else(|| format!("unknown backend {name:?}"))?;
        let backend = constructor(arguments)?;
        if backend.name() != name {
            return Err(format!(
                "the backend registered as {name:?} is named {:?}",
                backend.name()
            ));
        }
        Ok(backend)
    }

    /// The environments to offer, the hooks to run and the options that may change the server, once the backends are made.
    pub(crate) fn into_parts(self) -> (Vec<Environment>, Vec<Hook>, Vec<CommandOption>) {
        (self.environments.into_vec(), self.hooks, self.options)
    }
}

/// Why `name` is not a plain executable name, which [`Builder::program_name`] takes, or `None` if it is one.
fn unusable(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("it is empty");
    }
    name.chars().find_map(|character| {
        if character.is_whitespace() {
            Some("it holds whitespace")
        } else if character.is_control() {
            Some("it holds a control character")
        } else if matches!(
            character,
            '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
        ) {
            Some("it holds a zero-width character")
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use super::*;
    use crate::fake::FakeBackend;

    /// The command line `list` parsed for `builder`.
    fn arguments(builder: &Builder, list: &[&str]) -> Arguments {
        command::parse(
            list.iter().map(|argument| argument.to_string()),
            &builder.usage(),
            builder.options(),
        )
        .unwrap()
    }

    /// An empty directory of its own for the test named `name`.
    fn directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "tomori-server-builder-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn registered_backends_are_offered_by_name_after_tomoris() {
        let builder = Server::builder()
            .backend("scripted", || Ok(FakeBackend::new()))
            .backend("broken", || {
                Err::<FakeBackend, _>("No agent here.".to_owned())
            })
            // In place of Tomori's own, keeping its place.
            .backend("fake", || Ok(FakeBackend::new()));
        assert!(
            builder
                .usage()
                .contains("Available: fake, opencode, scripted, broken.")
        );
        let parsed = arguments(&builder, &[]);
        assert_eq!(
            builder.backend_named("fake", &parsed).unwrap().name(),
            "fake"
        );
        assert_eq!(
            builder.backend_named("broken", &parsed).err().as_deref(),
            Some("No agent here.")
        );
        // A backend must be registered under its own name, which clients name it by.
        assert_eq!(
            builder.backend_named("scripted", &parsed).err().as_deref(),
            Some("the backend registered as \"scripted\" is named \"fake\"")
        );
        assert_eq!(
            builder.backend_named("claude", &parsed).err().as_deref(),
            Some("unknown backend \"claude\"")
        );
        // Tomori's own build lists its own.
        assert!(Server::builder().usage().contains(
            "--backend NAME       Offer a backend; repeat for several. Available: fake, opencode.\n"
        ));
    }

    #[test]
    fn an_environment_registered_again_has_the_server_side_of_the_last() {
        struct Idle;
        impl tomori_environment::ServerSide for Idle {
            fn run(&self, _: &tomori_environment::Space) -> tomori_environment::Result<()> {
                Ok(())
            }
        }
        let first = Environment::new("observe", "Observe").with_server_side(Idle);
        let second = Environment::new("observe", "Observe").with_server_side(Idle);
        let (environments, _, _) = Server::builder()
            .environment(first)
            .environment(second.clone())
            .into_parts();
        let offered = environments.last().expect("offered");
        assert!(std::sync::Arc::ptr_eq(
            offered.server_side().expect("it has a side"),
            second.server_side().expect("it has a side")
        ));
        let space = tomori_environment::Space::new("space-1", "Fleet", "/");
        assert_eq!(
            offered.server_side().expect("it has a side").run(&space),
            Ok(())
        );

        // Registered again without one, it has none.
        let (environments, _, _) = Server::builder()
            .environment(second)
            .environment(Environment::new("observe", "Observe"))
            .into_parts();
        assert!(
            environments
                .last()
                .expect("offered")
                .server_side()
                .is_none()
        );
    }

    #[test]
    fn registered_environments_are_offered_in_order_and_replace_those_they_name() {
        // None until they are registered.
        assert!(Server::builder().into_parts().0.is_empty());
        assert!(Server::builder().environments().is_empty());
        let observe =
            Environment::new("observe", "Observe").with_instructions("Mind the topology.");
        let grid = Environment::new(tomori_protocol::GRID, "Grid");
        let night = Environment::new("observe", "Night");
        let (environments, _, _) = Server::builder()
            .environment(observe)
            .environment(grid.clone())
            .environment(night.clone())
            .into_parts();
        assert_eq!(environments, [night.clone(), grid.clone()]);
        let builder = Server::builder().environment(grid.clone());
        assert_eq!(builder.environments(), [grid]);
    }

    #[test]
    fn the_help_lists_the_registered_environments_in_the_order_offered() {
        // None until they are registered, so that every space creation is refused.
        assert!(Server::builder().usage().ends_with(
            "  --help               Show this help.\n\nEnvironments: none, so every space creation is refused."
        ));
        let usage = Server::builder()
            .environment(Environment::new("observe", "Observe"))
            .environment(Environment::new(tomori_protocol::GRID, "Grid").with_version(2))
            // In place of the first, keeping its place.
            .environment(Environment::new("observe", "Night").with_version(3))
            .usage();
        assert!(
            usage.ends_with(
                "  --help               Show this help.\n\nEnvironments, offered in this order, the first by default:\n  observe              Night, version 3.\n  minimal              Grid, version 2."
            ),
            "{usage}"
        );
    }

    #[test]
    fn the_help_names_the_program_it_is_given() {
        crate::test_log::record();
        // Whether or not the server is built with speech, whose options the synopsis starts with.
        let second = |usage: &str| usage.lines().nth(1).unwrap().to_owned();
        let usage = Server::builder().usage();
        assert!(usage.starts_with("Usage: tomori-server [--bind ADDRESS] [--backend NAME]..."));
        assert!(second(&usage).starts_with(&format!("{}[--", " ".repeat(21))));
        assert!(usage.contains(
            "\n       tomori-server auth (mint|list|revoke) ...   (see tomori-server auth --help)\n"
        ));
        // Another program's, its synopsis lined up after its name, its options' too.
        let usage = Server::builder()
            .program_name("observe-server")
            .option(fleet())
            .usage();
        assert!(
            usage.starts_with("Usage: observe-server [--bind ADDRESS] [--backend NAME]..."),
            "{usage}"
        );
        assert!(second(&usage).starts_with(&format!("{}[--", " ".repeat(22))));
        assert!(usage.contains(
            "\n                      [--no-mcp] [--open-pairing]\n                      [--fleet NAME]\n       observe-server auth (mint|list|revoke) ...   (see observe-server auth --help)\n"
        ));
        assert!(!usage.contains("tomori-server"), "{usage}");
        // Lined up by characters, not bytes.
        let usage = Server::builder().program_name("bühne").usage();
        assert!(usage.starts_with("Usage: bühne [--bind"));
        assert!(second(&usage).starts_with(&format!("{}[--", " ".repeat(13))));
        // A name that is not a plain executable name is not used.
        // Each with why it is not used, which the warning gives.
        for (name, why) in [
            ("", "it is empty"),
            ("observe x", "it holds whitespace"),
            ("observe\u{1b}[2J", "it holds a control character"),
            ("observe\u{200b}x", "it holds a zero-width character"),
            ("observe\u{200c}x", "it holds a zero-width character"),
            ("observe\u{200d}x", "it holds a zero-width character"),
            ("observe\u{2060}x", "it holds a zero-width character"),
            ("observe\u{feff}x", "it holds a zero-width character"),
        ] {
            assert_eq!(unusable(name), Some(why), "{name:?}");
        }
        assert_eq!(unusable("observe-ü"), None);
        for name in [
            "",
            "observe server",
            "observe\nserver",
            "observe\u{1b}[2J",
            "observe\u{200b}x",
        ] {
            assert_eq!(
                Server::builder().program_name(name).program(),
                "tomori-server",
                "{name:?}"
            );
        }
        assert_eq!(
            Server::builder()
                .program_name("observe-server")
                .program_name("observe server")
                .program(),
            "observe-server"
        );
        assert!(!crate::test_log::logged("is ignored, since").is_empty());
    }

    #[test]
    fn the_help_keeps_each_environment_to_its_line() {
        let usage = Server::builder()
            .environment(Environment::new("a-very-long-environment-id", "Long"))
            .environment(Environment::new("night\nshift", "Ob\tserve\u{1b}[2J"))
            .usage();
        assert!(
            usage.ends_with(
                "\n  a-very-long-environment-id Long, version 1.\n  night?shift          Ob?serve?[2J, version 1."
            ),
            "{usage}"
        );
    }

    /// An option of an application's own, `--fleet NAME`, offering an environment named for the fleet, and refusing the fleet `nowhere` as it configures the server.
    fn fleet() -> CommandOption {
        fleet_named("NAME")
    }

    /// `--fleet`, its value named `value` in `--help`.
    fn fleet_named(value: &str) -> CommandOption {
        CommandOption::new(
            "--fleet",
            value,
            "Offer the Observe environment, showing this fleet.\nThe fleet must exist.",
            |value| match value {
                "" => Err("--fleet needs a name, not \"\"".to_owned()),
                name => Ok(name.to_owned()),
            },
            |server, fleet| match fleet.as_str() {
                "nowhere" => Err("There is no fleet nowhere.".to_owned()),
                fleet => Ok(server.with_environments([Environment::new("observe", fleet)])),
            },
        )
    }

    #[test]
    fn an_applications_options_are_listed_read_and_applied_after_tomoris() {
        let builder = Server::builder().option(fleet());
        assert_eq!(
            (
                builder.options()[0].flag(),
                builder.options()[0].value(),
                builder.options()[0].help()
            ),
            (
                "--fleet",
                "NAME",
                "Offer the Observe environment, showing this fleet.\nThe fleet must exist."
            )
        );
        // `--help` lists it after Tomori's own, in the synopsis and among the options, each line of its help indented.
        let usage = builder.usage();
        assert!(usage.contains(
            "[--max-visitors COUNT] [--state-directory PATH] [--mcp-bind ADDRESS]\n                     [--no-mcp] [--open-pairing]\n                     [--fleet NAME]\n       tomori-server auth"
        ));
        assert!(usage.contains(
            "  --fleet NAME\n                       Offer the Observe environment, showing this fleet.\n                       The fleet must exist.\n  --help               Show this help."
        ));
        assert!(!Server::builder().usage().contains("--fleet"));
        // It needs its value, which it reads as the command line is read, and the last given is used.
        let parse = |list: &[&str]| {
            command::parse(
                list.iter().map(|argument| argument.to_string()),
                &builder.usage(),
                builder.options(),
            )
        };
        assert_eq!(
            parse(&["--fleet"]).err().as_deref(),
            Some("--fleet needs NAME")
        );
        assert_eq!(
            parse(&["--fleet", ""]).err().as_deref(),
            Some("--fleet needs a name, not \"\"")
        );
        // It changes the server once the command line has configured it, and its failure stops the server.
        let directory = directory("options");
        let root = directory.to_str().unwrap();
        let local = [
            "--auth",
            "open",
            "--state-directory",
            root,
            "--workspace-root",
            root,
        ];
        let configure = |extra: &[&str]| {
            let builder = Server::builder().option(fleet());
            let list: Vec<&str> = local.iter().chain(extra).copied().collect();
            let parsed = arguments(&builder, &list);
            let mut engines = Engines::default();
            let configured = command::configure(builder, &parsed, None, &mut engines)
                .map(|(server, _)| server.environment_labels());
            assert!(engines.stop(Instant::now()));
            configured
        };
        assert_eq!(
            configure(&["--fleet", "alpha", "--fleet", "beta"]),
            Ok(vec!["beta".to_owned()])
        );
        assert_eq!(configure(&[]), Ok(Vec::new()));
        assert_eq!(
            configure(&["--fleet", "nowhere"]).err().as_deref(),
            Some("There is no fleet nowhere.")
        );
        std::fs::remove_dir_all(directory).unwrap();
        // Added again under its flag, an option replaces the one added before, in its place.
        let replaced = Server::builder()
            .option(fleet())
            .option(fleet_named("FLEET"));
        assert_eq!(replaced.options().len(), 1);
        assert_eq!(replaced.options()[0].value(), "FLEET");
    }

    #[test]
    fn an_option_replaces_one_registered_environment_and_keeps_the_others() {
        struct Live;
        impl tomori_environment::ServerSide for Live {
            fn run(&self, _: &tomori_environment::Space) -> tomori_environment::Result<()> {
                Ok(())
            }
        }
        // `--observe-config PATH` swaps the registered Observe environment's server side for a live one, keeping the rest of it, without naming the others.
        let observe_config = || {
            CommandOption::new(
                "--observe-config",
                "PATH",
                "Run Observe against this configuration.",
                |value| Ok(value.to_owned()),
                |server, _path| {
                    let registered = server
                        .environments()
                        .into_iter()
                        .find(|environment| environment.id() == "observe")
                        .ok_or_else(|| "Observe is not registered.".to_owned())?;
                    Ok(server.with_environment(registered.with_server_side(Live)))
                },
            )
        };
        let directory = directory("environment-option");
        let root = directory.to_str().unwrap();
        let local = [
            "--auth",
            "open",
            "--state-directory",
            root,
            "--workspace-root",
            root,
        ];
        let configure = |builder: Builder, extra: &[&str]| {
            let builder = builder.option(observe_config());
            let list: Vec<&str> = local.iter().chain(extra).copied().collect();
            let parsed = arguments(&builder, &list);
            let mut engines = Engines::default();
            let configured = command::configure(builder, &parsed, None, &mut engines)
                .map(|(server, _)| server.environments());
            assert!(engines.stop(Instant::now()));
            configured
        };
        let sky = Environment::new("sky", "Sky");
        let observe = Environment::new("observe", "Observe")
            .with_version(2)
            .with_instructions("Mind the topology.");
        let grid = Environment::new(tomori_protocol::GRID, "Grid");
        let registered = || {
            Server::builder()
                .environment(sky.clone())
                .environment(observe.clone())
                .environment(grid.clone())
        };
        // Without the option, the environments are those registered.
        assert_eq!(
            configure(registered(), &[]),
            Ok(vec![sky.clone(), observe.clone(), grid.clone()])
        );
        let offered = configure(registered(), &["--observe-config", "observe.toml"]).unwrap();
        // Sky stays first, the default, and the grid last; Observe keeps its place, label, version and instructions, now with its live server side.
        assert_eq!(offered[0], sky);
        assert_eq!(offered[2], grid);
        assert_eq!(offered.len(), 3);
        assert_eq!(
            (
                offered[1].id(),
                offered[1].label(),
                offered[1].version(),
                offered[1].instructions()
            ),
            ("observe", "Observe", 2, "Mind the topology.")
        );
        let space = tomori_environment::Space::new("space-1", "Fleet", "/");
        assert_eq!(
            offered[1].server_side().expect("it is live").run(&space),
            Ok(())
        );
        // Without Observe registered, the option stops the server.
        assert_eq!(
            configure(
                Server::builder().environment(sky),
                &["--observe-config", "observe.toml"]
            )
            .err()
            .as_deref(),
            Some("Observe is not registered.")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn hooks_change_the_configured_server_in_order() {
        let directory = directory("hooks");
        let root = directory.to_str().unwrap();
        let ran = Rc::new(RefCell::new(Vec::new()));
        let builder = Server::builder()
            .configure({
                let ran = ran.clone();
                move |server| {
                    ran.borrow_mut().push("first");
                    server.with_max_visitors(1)
                }
            })
            .configure({
                let ran = ran.clone();
                move |server| {
                    ran.borrow_mut().push("second");
                    server
                }
            });
        let parsed = arguments(
            &builder,
            &[
                "--auth",
                "open",
                "--state-directory",
                root,
                "--workspace-root",
                root,
            ],
        );
        let mut engines = Engines::default();
        assert!(command::configure(builder, &parsed, None, &mut engines).is_ok());
        assert!(engines.stop(Instant::now()));
        assert_eq!(*ran.borrow(), ["first", "second"]);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_command_line_that_cannot_serve_ends_with_a_failure() {
        assert_eq!(
            Server::builder().run(["--help".to_owned()]),
            ExitCode::FAILURE
        );
        assert_eq!(
            Server::builder().run(["--backend".to_owned(), "claude".to_owned()]),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn engines_that_do_not_stop_end_the_process_at_once_with_its_status() {
        crate::test_log::record();
        let exited = RefCell::new(Vec::new());
        let exit = |code| exited.borrow_mut().push(code);
        assert_eq!(
            command::finish(ExitCode::SUCCESS, true, exit),
            ExitCode::SUCCESS
        );
        assert_eq!(
            command::finish(ExitCode::FAILURE, true, exit),
            ExitCode::FAILURE
        );
        assert!(exited.borrow().is_empty());
        assert_eq!(
            command::finish(ExitCode::SUCCESS, false, exit),
            ExitCode::SUCCESS
        );
        assert_eq!(
            command::finish(ExitCode::FAILURE, false, exit),
            ExitCode::FAILURE
        );
        assert_eq!(*exited.borrow(), [0, 1]);
    }
}
