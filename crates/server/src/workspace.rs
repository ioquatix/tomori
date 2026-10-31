// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Where spaces may run, and how many may run at once. See `design/workspaces.md`.
//!
//! There is no authentication yet, so every directory a client names is resolved, with symbolic links followed, and must be one of the server's workspace roots or lie beneath one. Every refusal looks the same to the client, so that the reply does not reveal whether a path exists or why it was refused.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The only reply a client receives when a directory is refused, whatever the reason.
pub const DIRECTORY_UNAVAILABLE: &str = "That directory is not available.";

/// The default number of spaces whose sessions may run at once.
pub const DEFAULT_MAX_SPACES: usize = 8;

/// The canonical directories under which spaces may run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRoots {
    roots: Vec<PathBuf>,
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
            if !canonical.contains(&resolved) {
                canonical.push(resolved);
            }
        }
        Ok(Self { roots: canonical })
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
        Self { roots: Vec::new() }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The canonical form of `directory`, if it is an existing directory that is a root or lies beneath one. Containment is checked component by component on the canonical path, so `/home/sam2` is not beneath `/home/sam` and neither `..` nor a symbolic link can leave a root. Why a directory was refused is logged, never returned.
    pub fn resolve(&self, directory: &str) -> Result<PathBuf, Unavailable> {
        let requested = Path::new(directory);
        if !requested.is_absolute() {
            log::warn!("Refused directory {directory:?}: not an absolute path.");
            return Err(Unavailable);
        }
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
}

/// Counts the spaces whose sessions are starting or running, so that no more than the limit run at once.
#[derive(Debug)]
pub struct SpaceSlots {
    running: AtomicUsize,
    limit: AtomicUsize,
}

/// One space's place under the limit, from when its creation is accepted until its session has closed or failed to start. Dropping it frees the place.
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
