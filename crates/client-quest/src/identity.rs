// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Who this device is to the server, and which server it last joined.
use std::{
    collections::hash_map::RandomState,
    fs,
    hash::{BuildHasher, Hasher},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// The server's limit on a display name, in bytes.
const NAME_BYTES: usize = 64;
const PARTICIPANT_FILE: &str = "participant";
const SERVER_FILE: &str = "server";

/// Settings given when the app starts, which take precedence over the defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    /// A server address to connect to straight away.
    pub server: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// A random identity, so the server recognizes a reconnect, or a restart where it is stored, as the same participant.
    pub participant: String,
    /// The display name other participants see and the agent reads before each message.
    pub name: String,
    /// A server address to connect to straight away.
    pub server: Option<String>,
}

impl Identity {
    pub fn new(store: &Store, device: Option<&str>, overrides: Overrides) -> Self {
        let name = overrides
            .name
            .as_deref()
            .and_then(name)
            .or_else(|| device.and_then(name))
            .unwrap_or_else(|| "Tomori".into());
        Self {
            participant: store.participant(),
            name,
            server: overrides
                .server
                .map(|server| server.trim().to_owned())
                .filter(|server| !server.is_empty()),
        }
    }
}

/// A display name the server accepts: not blank, not starting with `/`, and within its length limit.
pub fn name(candidate: &str) -> Option<String> {
    let name = candidate.trim();
    if name.is_empty() || name.starts_with('/') {
        return None;
    }
    let mut end = name.len().min(NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    Some(name[..end].trim_end().to_owned())
}

/// Small values kept across restarts, in the app's private storage. Without a directory nothing is kept, and each run is a new participant.
pub struct Store {
    directory: Option<PathBuf>,
}

impl Store {
    pub fn new(directory: Option<PathBuf>) -> Self {
        Self { directory }
    }

    /// The stored participant identity, created on first use.
    pub fn participant(&self) -> String {
        if let Some(stored) = self
            .read(PARTICIPANT_FILE)
            .filter(|id| valid_participant(id))
        {
            return stored;
        }
        let participant = random_participant();
        self.write(PARTICIPANT_FILE, &participant);
        participant
    }

    /// The instance name of the server last connected to.
    pub fn server(&self) -> Option<String> {
        self.read(SERVER_FILE)
            .filter(|instance| !instance.is_empty())
    }

    pub fn remember_server(&self, instance: &str) {
        if self.server().as_deref() != Some(instance) {
            self.write(SERVER_FILE, instance);
        }
    }

    fn read(&self, file: &str) -> Option<String> {
        let path = self.directory.as_deref()?.join(file);
        Some(fs::read_to_string(path).ok()?.trim().to_owned())
    }

    fn write(&self, file: &str, value: &str) {
        let Some(directory) = self.directory.as_deref() else {
            return;
        };
        if let Err(error) = write(directory, file, value) {
            log::warn!("Could not store {file} in {}: {error}", directory.display());
        }
    }
}

/// Write through a temporary file, so a crash never leaves a partial value.
fn write(directory: &Path, file: &str, value: &str) -> std::io::Result<()> {
    fs::create_dir_all(directory)?;
    let temporary = directory.join(format!("{file}.tmp"));
    fs::write(&temporary, value)?;
    fs::rename(temporary, directory.join(file))
}

fn valid_participant(id: &str) -> bool {
    (16..=64).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 128 random bits as hexadecimal. `RandomState` is seeded from the operating system's random source.
fn random_participant() -> String {
    let mut id = String::new();
    for round in 0..2_u8 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u8(round);
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        hasher.write_u32(std::process::id());
        id.push_str(&format!("{:016x}", hasher.finish()));
    }
    id
}

/// The name of this computer, for a default display name on the desktop.
#[cfg(not(target_os = "android"))]
pub fn host_name() -> Option<String> {
    let output = std::process::Command::new("hostname").output().ok()?;
    let host = String::from_utf8(output.stdout).ok()?;
    // `studio.local` reads better as `studio`.
    let host = host.trim().split('.').next()?.to_owned();
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "tomori-identity-{name}-{}-{}",
            std::process::id(),
            random_participant()
        ));
        let _ = fs::remove_dir_all(&directory);
        directory
    }

    #[test]
    fn names_must_not_be_blank_or_start_with_a_slash() {
        assert_eq!(name("  Ada "), Some("Ada".into()));
        assert_eq!(name("   "), None);
        assert_eq!(name(""), None);
        assert_eq!(name(" /clear"), None);
        assert_eq!(name("Ada/Bea"), Some("Ada/Bea".into()));
    }

    #[test]
    fn long_names_are_cut_at_a_character_boundary() {
        let long = "é".repeat(40);
        let cut = name(&long).unwrap();
        assert!(cut.len() <= NAME_BYTES);
        assert_eq!(cut, "é".repeat(32));
    }

    #[test]
    fn the_name_override_wins_over_the_device_name() {
        let store = Store::new(None);
        let overrides = Overrides {
            server: Some(" 10.0.0.1:8765 ".into()),
            name: Some("Ada".into()),
        };
        let identity = Identity::new(&store, Some("Quest 3"), overrides);
        assert_eq!(identity.name, "Ada");
        assert_eq!(identity.server.as_deref(), Some("10.0.0.1:8765"));
    }

    #[test]
    fn an_invalid_override_falls_back_to_the_device_then_a_default() {
        let store = Store::new(None);
        let overrides = Overrides {
            server: Some("  ".into()),
            name: Some("/me".into()),
        };
        let identity = Identity::new(&store, Some("Quest 3"), overrides.clone());
        assert_eq!(identity.name, "Quest 3");
        assert_eq!(identity.server, None);
        let identity = Identity::new(&store, Some(""), overrides);
        assert_eq!(identity.name, "Tomori");
    }

    #[test]
    fn the_participant_is_kept_where_there_is_storage() {
        let directory = directory("kept");
        let store = Store::new(Some(directory.clone()));
        let first = store.participant();
        assert!(valid_participant(&first), "{first}");
        assert_eq!(Store::new(Some(directory.clone())).participant(), first);
        fs::write(directory.join(PARTICIPANT_FILE), "not hex").unwrap();
        let replaced = store.participant();
        assert_ne!(replaced, first);
        assert!(valid_participant(&replaced));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn without_storage_each_run_is_a_new_participant() {
        let store = Store::new(None);
        assert_ne!(store.participant(), store.participant());
        assert_eq!(store.server(), None);
        store.remember_server("studio");
        assert_eq!(store.server(), None);
    }

    #[test]
    fn remembers_the_last_server() {
        let directory = directory("server");
        let store = Store::new(Some(directory.clone()));
        assert_eq!(store.server(), None);
        store.remember_server("studio");
        assert_eq!(store.server().as_deref(), Some("studio"));
        store.remember_server("attic");
        assert_eq!(
            Store::new(Some(directory.clone())).server().as_deref(),
            Some("attic")
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
