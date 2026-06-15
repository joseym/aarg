//! Where aarg keeps its files.
//!
//! By default aarg works out of a **local workspace**: a `.aarg/` directory
//! holding the config, dataset, builds, traces, and cache for one project.
//! Commands find it the way `git` finds `.git` — by walking up from the
//! current directory — so you can run aarg from anywhere inside a project.
//! When no `.aarg/` is found, aarg falls back to the per-OS home
//! directories (`~/.config/aarg`, `~/.local/share/aarg`, `~/.cache/aarg`),
//! which is where setups made before workspaces live.
//!
//! API keys are the one thing that never lands in a workspace: they stay in
//! the OS keychain (see the `secrets` module). A local config still records
//! which key *labels* to use, so a project can pin its own active key while
//! the secrets remain shared.
//!
//! Every storage root in the codebase resolves through here, so "where do
//! my files live" has a single answer. Resolution precedence:
//!   1. the `AARG_DIR` environment variable (its `.aarg/` subdir), then
//!   2. the nearest `.aarg/` walking up from the current directory, then
//!   3. the per-OS home directories.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

/// The marker directory that identifies a local workspace.
pub const MARKER: &str = ".aarg";

/// The environment variable that forces a specific project directory,
/// overriding discovery. aarg uses its `.aarg/` subdirectory.
pub const DIR_ENV: &str = "AARG_DIR";

/// Where aarg's files live for the current invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A project's `.aarg/` directory. Config, data, and cache all live
    /// under it (the path stored here is the `.aarg/` directory itself).
    Local(PathBuf),
    /// The per-OS home directories: config, data, and cache are separate.
    Global,
}

impl Location {
    /// The directory holding `config.toml` and `dataset.json`.
    pub fn config_dir(&self) -> Option<PathBuf> {
        match self {
            Location::Local(root) => Some(root.clone()),
            Location::Global => {
                ProjectDirs::from("", "", "aarg").map(|dirs| dirs.config_dir().to_path_buf())
            }
        }
    }

    /// The directory under which `builds/` and `traces/` live.
    pub fn data_dir(&self) -> Option<PathBuf> {
        match self {
            Location::Local(root) => Some(root.clone()),
            Location::Global => {
                ProjectDirs::from("", "", "aarg").map(|dirs| dirs.data_dir().to_path_buf())
            }
        }
    }

    /// The directory under which transient caches (e.g. fetched JDs) live.
    pub fn cache_dir(&self) -> Option<PathBuf> {
        match self {
            // Group caches under the workspace so a `.aarg/` stays tidy.
            Location::Local(root) => Some(root.join("cache")),
            Location::Global => {
                ProjectDirs::from("", "", "aarg").map(|dirs| dirs.cache_dir().to_path_buf())
            }
        }
    }

    /// A short human description of where files are coming from, for status
    /// output. The path is the workspace dir (local) or `home config`.
    pub fn describe(&self) -> String {
        match self {
            Location::Local(root) => format!("local workspace ({})", root.display()),
            Location::Global => "global (per-user home directories)".to_string(),
        }
    }
}

/// Resolve the active location from an explicit env override and a starting
/// directory. Split out from [`locate`] so the precedence is unit-testable
/// without touching the real environment or current directory.
fn resolve(env_dir: Option<PathBuf>, start_dir: &Path) -> Location {
    // 1. An explicit project directory wins outright.
    if let Some(project) = env_dir {
        return Location::Local(project.join(MARKER));
    }
    // 2. Walk up from the start directory looking for a `.aarg/`.
    let mut here = Some(start_dir);
    while let Some(dir) = here {
        let candidate = dir.join(MARKER);
        if candidate.is_dir() {
            return Location::Local(candidate);
        }
        here = dir.parent();
    }
    // 3. Nothing local: fall back to the per-OS home directories.
    Location::Global
}

/// The active location for this invocation: `AARG_DIR`, else the nearest
/// `.aarg/` above the current directory, else the home directories.
pub fn locate() -> Location {
    let env_dir = std::env::var_os(DIR_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    // A missing current directory (deleted out from under us) just means no
    // local workspace can be discovered — fall back to global.
    let cwd = std::env::current_dir().unwrap_or_default();
    resolve(env_dir, &cwd)
}

/// The config/dataset directory for the active location.
pub fn config_dir() -> Option<PathBuf> {
    locate().config_dir()
}

/// The data directory (parent of `builds/` and `traces/`) for the active
/// location.
pub fn data_dir() -> Option<PathBuf> {
    locate().data_dir()
}

/// The cache directory for the active location.
pub fn cache_dir() -> Option<PathBuf> {
    locate().cache_dir()
}

/// The traces directory for the active location.
pub fn traces_dir() -> Option<PathBuf> {
    locate().data_dir().map(|dir| dir.join("traces"))
}

/// The `.aarg/` directory for an explicit project directory — used by
/// `init` to target a workspace it is about to create, rather than the one
/// discovery would currently find (which doesn't exist yet).
pub fn local_root(project_dir: &Path) -> PathBuf {
    project_dir.join(MARKER)
}

/// The per-OS home config directory, ignoring any local workspace — used by
/// `aarg init --global` to target the home config explicitly.
pub fn global_config_dir() -> Option<PathBuf> {
    Location::Global.config_dir()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn env_override_points_at_its_marker_subdir() {
        let location = resolve(Some(PathBuf::from("/tmp/proj")), Path::new("/anywhere"));
        assert_eq!(location, Location::Local(PathBuf::from("/tmp/proj/.aarg")));
    }

    #[test]
    fn discovery_finds_the_nearest_marker_walking_up() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir(root.join(MARKER)).unwrap();
        let deep = root.join("src").join("nested");
        std::fs::create_dir_all(&deep).unwrap();

        // From deep inside the project, discovery climbs to the marker.
        assert_eq!(
            resolve(None, &deep),
            Location::Local(root.join(MARKER)),
            "should find the .aarg above the start dir"
        );
    }

    #[test]
    fn no_marker_falls_back_to_global() {
        let temp = tempfile::tempdir().unwrap();
        // A directory tree with no `.aarg/` anywhere above the start dir.
        let deep = temp.path().join("empty").join("tree");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(resolve(None, &deep), Location::Global);
    }

    #[test]
    fn local_dirs_collapse_to_one_root_but_keep_subfolders() {
        let location = Location::Local(PathBuf::from("/p/.aarg"));
        assert_eq!(location.config_dir().unwrap(), PathBuf::from("/p/.aarg"));
        assert_eq!(location.data_dir().unwrap(), PathBuf::from("/p/.aarg"));
        assert_eq!(
            location.cache_dir().unwrap(),
            PathBuf::from("/p/.aarg/cache")
        );
        assert_eq!(
            traces_dir_of(&location).unwrap(),
            PathBuf::from("/p/.aarg/traces")
        );
    }

    // Mirror of `traces_dir` against an explicit location (the public one
    // reads the environment).
    fn traces_dir_of(location: &Location) -> Option<PathBuf> {
        location.data_dir().map(|dir| dir.join("traces"))
    }
}
