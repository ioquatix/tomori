// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Where spaces may run, and how many may run at once. See `design/workspaces.md`.
//!
//! There is no authentication yet, so every directory a client names is resolved, with symbolic links followed, and must be one of the server's workspace roots or lie beneath one. Every refusal looks the same to the client, so that the reply does not reveal whether a path exists or why it was refused.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// The only reply a client receives when a directory is refused, whatever the reason.
pub const DIRECTORY_UNAVAILABLE: &str = "That directory is not available.";

/// The default number of spaces whose sessions may run at once.
pub const DEFAULT_MAX_SPACES: usize = 8;

/// The most directory checks that may run at once on their own threads. A check that never finishes, such as one on a hung network mount, keeps its place, so a client cannot leave more threads than this blocked.
pub const MAX_RESOLVING: usize = 8;

/// How long a directory check may take before the directory is refused.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// The canonical directories under which spaces may run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRoots {
    roots: Vec<PathBuf>,
    /// Each root as configured and in canonical form, which may differ, such as `/tmp` and `/private/tmp` on macOS, for the check made before touching the file system.
    prefixes: Vec<PathBuf>,
}

/// A directory a client asked for that is not available. The details are logged by [`WorkspaceRoots::resolve`]; the client sees only [`DIRECTORY_UNAVAILABLE`].
#[derive(Debug, PartialEq, Eq)]
pub struct Unavailable;

impl fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(DIRECTORY_UNAVAILABLE)
    }
}

impl WorkspaceRoots {
    /// Canonicalize each root. A root that does not exist or is not a directory is an error naming it, for the operator.
    pub fn new<P: AsRef<Path>>(roots: impl IntoIterator<Item = P>) -> Result<Self, String> {
        let mut canonical = Vec::new();
        let mut prefixes = Vec::new();
        for root in roots {
            let root = root.as_ref();
            let resolved = root
                .canonicalize()
                .map_err(|error| format!("workspace root {}: {error}", root.display()))?;
            if !resolved.is_dir() {
                return Err(format!(
                    "workspace root {} is not a directory",
                    root.display()
                ));
            }
            for prefix in [normalize(root), Some(resolved.clone())]
                .into_iter()
                .flatten()
            {
                if !prefixes.contains(&prefix) {
                    prefixes.push(prefix);
                }
            }
            if !canonical.contains(&resolved) {
                canonical.push(resolved);
            }
        }
        Ok(Self {
            roots: canonical,
            prefixes,
        })
    }

    /// The default: the user's home directory alone.
    pub fn home() -> Result<Self, String> {
        let home = std::env::home_dir()
            .filter(|home| !home.as_os_str().is_empty())
            .ok_or("could not determine the home directory; give --workspace-root")?;
        Self::new([home])
    }

    /// No roots at all, refusing every directory.
    pub fn none() -> Self {
        Self {
            roots: Vec::new(),
            prefixes: Vec::new(),
        }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Refuse `directory` without touching the file system unless, with `.` and `..` applied to the path as written, it is a root or lies beneath one, as configured or in canonical form. This keeps the file system calls of [`resolve`](Self::resolve) within the roots, so that a client cannot make the server wait on an arbitrary path, such as a hung network mount. It only narrows what is checked: [`resolve`](Self::resolve) still decides.
    pub fn check(&self, directory: &str) -> Result<(), Unavailable> {
        let Some(normalized) = normalize(Path::new(directory)) else {
            log::warn!("Refused directory {directory:?}: not an absolute path.");
            return Err(Unavailable);
        };
        if !self
            .prefixes
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
        {
            log::warn!("Refused directory {directory:?}: outside the workspace roots.");
            return Err(Unavailable);
        }
        Ok(())
    }

    /// The canonical form of `directory`, if it is an existing directory that is a root or lies beneath one. Containment is checked component by component on the canonical path, so `/home/sam2` is not beneath `/home/sam` and neither `..` nor a symbolic link can leave a root. Why a directory was refused is logged, never returned. This touches the file system and may block; see [`resolve_in_background`](Self::resolve_in_background).
    pub fn resolve(&self, directory: &str) -> Result<PathBuf, Unavailable> {
        self.check(directory)?;
        let requested = Path::new(directory);
        let resolved = match requested.canonicalize() {
            Ok(resolved) => resolved,
            Err(error) => {
                log::warn!("Refused directory {directory:?}: {error}.");
                return Err(Unavailable);
            }
        };
        if !self.roots.iter().any(|root| resolved.starts_with(root)) {
            log::warn!(
                "Refused directory {directory:?}: {} is outside the workspace roots.",
                resolved.display()
            );
            return Err(Unavailable);
        }
        if !resolved.is_dir() {
            log::warn!("Refused directory {directory:?}: not a directory.");
            return Err(Unavailable);
        }
        Ok(resolved)
    }

    /// [`resolve`](Self::resolve), as the UTF-8 string that backends and the protocol carry. A canonical path that is not UTF-8 is refused.
    pub fn resolve_str(&self, directory: &str) -> Result<String, Unavailable> {
        let resolved = self.resolve(directory)?;
        resolved.into_os_string().into_string().map_err(|path| {
            log::warn!("Refused directory {directory:?}: {path:?} is not UTF-8.");
            Unavailable
        })
    }

    /// [`resolve_str`](Self::resolve_str) on its own thread, so that a slow file system cannot stall the server. The directory is first [`check`](Self::check)ed, and is refused at once if [`MAX_RESOLVING`] checks are already running. The caller waits on the receiver for at most [`RESOLVE_TIMEOUT`].
    pub fn resolve_in_background(
        &self,
        directory: &str,
        resolving: &Arc<Resolving>,
    ) -> Result<async_channel::Receiver<Result<String, Unavailable>>, Unavailable> {
        self.check(directory)?;
        let Some(place) = resolving.acquire() else {
            log::warn!(
                "Refused directory {directory:?}: {MAX_RESOLVING} directory checks are already running."
            );
            return Err(Unavailable);
        };
        let (sender, receiver) = async_channel::bounded(1);
        let roots = self.clone();
        let requested = directory.to_owned();
        // If the thread cannot start, the closure and the place it holds are dropped.
        std::thread::Builder::new()
            .name("tomori-directory".to_owned())
            .spawn(move || {
                let _place = place;
                let _ = sender.try_send(roots.resolve_str(&requested));
            })
            .map_err(|error| {
                log::warn!("Refused directory {directory:?}: could not start a check: {error}.");
                Unavailable
            })?;
        Ok(receiver)
    }
}

/// `path` with `.` and `..` applied as written, without following symbolic links, or `None` if it is not absolute.
fn normalize(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component)
            }
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
        }
    }
    Some(normalized)
}

/// Counts directory checks running on their own threads, against [`MAX_RESOLVING`].
#[derive(Debug, Default)]
pub struct Resolving(AtomicUsize);

/// One running check's place, freed when its thread finishes.
struct ResolvingPlace(Arc<Resolving>);

impl Resolving {
    fn acquire(self: &Arc<Self>) -> Option<ResolvingPlace> {
        // The place is taken first, so concurrent checks cannot pass the limit together.
        let place = ResolvingPlace(self.clone());
        (self.0.fetch_add(1, Ordering::AcqRel) < MAX_RESOLVING).then_some(place)
    }
}

impl Drop for ResolvingPlace {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Counts the spaces whose sessions are starting or running, so that no more than the limit run at once.
#[derive(Debug)]
pub struct SpaceSlots {
    running: AtomicUsize,
    limit: AtomicUsize,
}

/// One space's place under the limit, from when its directory has been accepted until its session has closed or failed to start. Dropping it frees the place.
#[derive(Debug)]
pub struct SpaceSlot {
    slots: Arc<SpaceSlots>,
}

impl SpaceSlots {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            running: AtomicUsize::new(0),
            limit: AtomicUsize::new(limit),
        })
    }

    pub fn limit(&self) -> usize {
        self.limit.load(Ordering::Acquire)
    }

    pub fn set_limit(&self, limit: usize) {
        self.limit.store(limit, Ordering::Release);
    }

    /// Whether the limit is reached now, so that a request can be refused before its directory is checked. Only [`acquire`](Self::acquire) takes a place.
    pub fn is_full(&self) -> bool {
        self.running.load(Ordering::Acquire) >= self.limit()
    }

    /// The error a request beyond the limit receives.
    pub fn full_error(&self) -> String {
        format!(
            "This server already runs its limit of {} spaces; try again when one closes.",
            self.limit()
        )
    }

    /// Take a place, or `None` if the limit is reached.
    pub fn acquire(self: &Arc<Self>) -> Option<SpaceSlot> {
        let limit = self.limit();
        let mut running = self.running.load(Ordering::Acquire);
        while running < limit {
            match self.running.compare_exchange_weak(
                running,
                running + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(SpaceSlot {
                        slots: self.clone(),
                    });
                }
                Err(actual) => running = actual,
            }
        }
        None
    }

    #[cfg(test)]
    fn running(&self) -> usize {
        self.running.load(Ordering::Acquire)
    }
}

impl Drop for SpaceSlot {
    fn drop(&mut self) {
        self.slots.running.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("tomori-workspace-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn directory(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn text(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    #[test]
    fn a_root_and_directories_beneath_it_are_available() {
        let scratch = Scratch::new("inside");
        let root = scratch.directory("root");
        let nested = scratch.directory("root/project/src");
        let roots = WorkspaceRoots::new([&root]).unwrap();
        assert_eq!(roots.resolve(text(&root)), Ok(root.clone()));
        assert_eq!(roots.resolve(text(&nested)), Ok(nested.clone()));
        // A trailing slash and `.` resolve to the same directory.
        assert_eq!(roots.resolve(&format!("{}/./", text(&nested))), Ok(nested));
    }

    #[test]
    fn a_directory_outside_every_root_is_refused() {
        let scratch = Scratch::new("outside");
        let root = scratch.directory("root");
        let other = scratch.directory("other");
        let roots = WorkspaceRoots::new([&root]).unwrap();
        assert_eq!(roots.resolve(text(&other)), Err(Unavailable));
        assert_eq!(roots.resolve("/"), Err(Unavailable));
        assert_eq!(roots.resolve(text(&scratch.0)), Err(Unavailable));
    }

    #[test]
    fn a_sibling_sharing_a_prefix_is_not_beneath_the_root() {
        let scratch = Scratch::new("sibling");
        let root = scratch.directory("root");
        let sibling = scratch.directory("root2");
        let roots = WorkspaceRoots::new([&root]).unwrap();
        assert_eq!(roots.resolve(text(&sibling)), Err(Unavailable));
    }

    #[test]
    fn parent_traversal_cannot_leave_a_root() {
        let scratch = Scratch::new("traversal");
        let root = scratch.directory("root");
        scratch.directory("root/project");
        let other = scratch.directory("other");
        let roots = WorkspaceRoots::new([&root]).unwrap();
        let escape = format!("{}/project/../../other", text(&root));
        assert_eq!(roots.resolve(&escape), Err(Unavailable));
        // Traversal that stays within the root is fine.
        let within = format!("{}/project/..", text(&root));
        assert_eq!(roots.resolve(&within), Ok(root));
        assert!(other.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_out_of_a_root_is_refused() {
        let scratch = Scratch::new("symlink");
        let root = scratch.directory("root");
        let other = scratch.directory("other");
        let inner = scratch.directory("root/inner");
        std::os::unix::fs::symlink(&other, root.join("escape")).unwrap();
        std::os::unix::fs::symlink(&inner, root.join("alias")).unwrap();
        let roots = WorkspaceRoots::new([&root]).unwrap();
        assert_eq!(roots.resolve(text(&root.join("escape"))), Err(Unavailable));
        // A link that stays within the root resolves to its target.
        assert_eq!(roots.resolve(text(&root.join("alias"))), Ok(inner));
    }

    #[cfg(unix)]
    #[test]
    fn a_root_given_through_a_symbolic_link_is_canonicalized() {
        let scratch = Scratch::new("linked-root");
        let root = scratch.directory("root");
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let roots = WorkspaceRoots::new([&link]).unwrap();
        assert_eq!(roots.roots(), std::slice::from_ref(&root));
        assert_eq!(
            roots.resolve(text(&link.join("."))),
            Ok(root),
            "a directory named through the link is beneath the canonical root"
        );
    }

    #[test]
    fn a_missing_path_is_refused_like_a_forbidden_one() {
        let scratch = Scratch::new("missing");
        let root = scratch.directory("root");
        std::fs::write(root.join("file"), "").unwrap();
        let roots = WorkspaceRoots::new([&root]).unwrap();
        for directory in [
            format!("{}/missing", text(&root)),
            format!("{}/file", text(&root)),
            "/does/not/exist".to_owned(),
            "relative".to_owned(),
            String::new(),
        ] {
            assert_eq!(roots.resolve(&directory), Err(Unavailable), "{directory}");
        }
        assert_eq!(Unavailable.to_string(), DIRECTORY_UNAVAILABLE);
    }

    #[test]
    fn a_missing_root_is_refused_at_startup() {
        let scratch = Scratch::new("missing-root");
        let missing = scratch.0.join("missing");
        let error = WorkspaceRoots::new([&missing]).unwrap_err();
        assert!(error.contains("missing"), "{error}");
        std::fs::write(scratch.0.join("file"), "").unwrap();
        assert!(WorkspaceRoots::new([scratch.0.join("file")]).is_err());
    }

    #[test]
    fn paths_outside_the_roots_as_written_are_refused_before_touching_the_file_system() {
        let scratch = Scratch::new("lexical");
        let root = scratch.directory("root");
        let roots = WorkspaceRoots::new([&root]).unwrap();
        let root_text = text(&root);
        // These need no file system access to refuse, even if they would hang.
        for directory in [
            "/net/unreachable/share".to_owned(),
            "/".to_owned(),
            format!("{root_text}2/project"),
            format!("{root_text}/project/../../other"),
            "relative".to_owned(),
        ] {
            assert_eq!(roots.check(&directory), Err(Unavailable), "{directory}");
        }
        // Missing paths beneath a root pass this check; resolving them refuses them.
        for directory in [
            root_text.to_owned(),
            format!("{root_text}/missing"),
            format!("{root_text}/project/./.."),
        ] {
            assert_eq!(roots.check(&directory), Ok(()), "{directory}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_root_is_matched_as_configured_and_in_canonical_form() {
        let scratch = Scratch::new("configured");
        let root = scratch.directory("root");
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let roots = WorkspaceRoots::new([&link]).unwrap();
        // As on macOS, where `/tmp` is `/private/tmp`, a client may name either form.
        assert_eq!(roots.check(&format!("{}/project", text(&link))), Ok(()));
        assert_eq!(roots.check(&format!("{}/project", text(&root))), Ok(()));
    }

    #[test]
    fn directories_resolve_in_the_background() {
        let scratch = Scratch::new("background");
        let root = scratch.directory("root");
        let roots = WorkspaceRoots::new([&root]).unwrap();
        let resolving = Arc::new(Resolving::default());
        let resolved = roots
            .resolve_in_background(&format!("{}/.", text(&root)), &resolving)
            .unwrap();
        assert_eq!(
            resolved.recv_blocking().unwrap(),
            Ok(text(&root).to_owned())
        );
        let missing = roots
            .resolve_in_background(&format!("{}/missing", text(&root)), &resolving)
            .unwrap();
        assert_eq!(missing.recv_blocking().unwrap(), Err(Unavailable));
        // Outside the roots as written: refused without starting a check.
        assert!(roots.resolve_in_background("/", &resolving).is_err());
    }

    #[test]
    fn running_checks_are_limited() {
        let resolving = Arc::new(Resolving::default());
        let places: Vec<_> = (0..MAX_RESOLVING)
            .map(|_| resolving.acquire().unwrap())
            .collect();
        assert!(resolving.acquire().is_none());
        drop(places);
        assert!(resolving.acquire().is_some());
        assert_eq!(resolving.0.load(Ordering::Acquire), 0);
    }

    #[test]
    fn no_roots_refuse_everything() {
        assert_eq!(WorkspaceRoots::none().resolve("/"), Err(Unavailable));
    }

    #[test]
    fn slots_are_limited_and_freed_when_dropped() {
        let slots = SpaceSlots::new(2);
        let first = slots.acquire().unwrap();
        let second = slots.acquire().unwrap();
        assert!(slots.acquire().is_none());
        assert_eq!(slots.running(), 2);
        drop(first);
        let third = slots.acquire().unwrap();
        assert!(slots.acquire().is_none());
        drop((second, third));
        assert_eq!(slots.running(), 0);
    }

    #[test]
    fn concurrent_acquires_never_exceed_the_limit() {
        let slots = SpaceSlots::new(3);
        let taken: Vec<Option<SpaceSlot>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16).map(|_| scope.spawn(|| slots.acquire())).collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        assert_eq!(taken.iter().filter(|slot| slot.is_some()).count(), 3);
        assert_eq!(slots.running(), 3);
    }
}
