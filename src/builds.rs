//! Build directories: one numbered folder per tailoring run, holding
//! every artifact that produced the final PDF (PRD §8).
//!
//! Lives under the per-OS *data* directory (`~/.local/share/aarg/builds`
//! on Linux), not the config directory — builds are outputs, not
//! settings. Numbering is `001`, `002`, ... so directories sort
//! chronologically in any file manager, and a build's artifacts make the
//! run reproducible: the JD, the gap report, the canonical resume, the
//! exact template, the payload, the PDF, and a `meta.json` with the
//! model and token costs.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::llm::TokenUsage;
use crate::tailor::BuildId;

/// Everything that can go wrong while managing build directories.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("could not determine this user's home directory")]
    NoHomeDir,

    #[error("could not create or read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not serialize a build artifact")]
    Serialize(#[from] serde_json::Error),
}

/// A freshly allocated build: its ID and its directory on disk.
#[derive(Debug)]
pub struct Build {
    pub id: BuildId,
    pub dir: PathBuf,
}

/// Run provenance, written as `meta.json` in every build directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildMeta {
    pub created_at: DateTime<Utc>,
    pub model: String,
    pub template: String,
    pub tailor_usage: TokenUsage,
    /// Whether the run was on a Claude plan (cost covered by the flat fee)
    /// rather than a billed API key. `#[serde(default)]` so builds written
    /// before this field load as `false` and still show a dollar estimate.
    #[serde(default)]
    pub subscription: bool,
}

/// Where builds live: `builds/` under the active workspace's `.aarg/`, else
/// under the per-OS data directory. Resolved by the `workspace` module.
pub fn builds_root() -> Result<PathBuf, BuildError> {
    crate::workspace::data_dir()
        .map(|dir| dir.join("builds"))
        .ok_or(BuildError::NoHomeDir)
}

/// Allocate the next build: scan for the highest numbered directory,
/// create number+1.
pub fn create_next() -> Result<Build, BuildError> {
    create_next_in(&builds_root()?)
}

pub(crate) fn create_next_in(root: &Path) -> Result<Build, BuildError> {
    let io_err = |source| BuildError::Io {
        path: root.to_path_buf(),
        source,
    };
    std::fs::create_dir_all(root).map_err(io_err)?;

    let mut highest = 0u32;
    for entry in std::fs::read_dir(root).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        // Non-numeric names (or stray files) simply don't participate.
        if let Some(n) = entry.file_name().to_str().and_then(|s| s.parse().ok()) {
            highest = highest.max(n);
        }
    }

    let id = BuildId(format!("{:03}", highest + 1));
    let dir = root.join(&id.0);
    std::fs::create_dir(&dir).map_err(|source| BuildError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(Build { id, dir })
}

/// Write one artifact as pretty JSON into a build directory.
pub fn write_json<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<(), BuildError> {
    let path = dir.join(name);
    let json = serde_json::to_vec_pretty(value)?;
    std::fs::write(&path, json).map_err(|source| BuildError::Io { path, source })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_first_build_is_001() {
        let root = tempfile::tempdir().unwrap();
        let build = create_next_in(root.path()).unwrap();
        assert_eq!(build.id, BuildId("001".into()));
        assert!(build.dir.is_dir());
    }

    #[test]
    fn numbering_continues_past_existing_builds_and_ignores_strays() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("001")).unwrap();
        std::fs::create_dir(root.path().join("007")).unwrap();
        std::fs::create_dir(root.path().join("not-a-build")).unwrap();
        std::fs::write(root.path().join("stray.txt"), "x").unwrap();

        let build = create_next_in(root.path()).unwrap();
        assert_eq!(build.id, BuildId("008".into()));
    }

    #[test]
    fn write_json_produces_readable_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let build = create_next_in(root.path()).unwrap();
        let meta = BuildMeta {
            created_at: Utc::now(),
            model: "test-model".into(),
            template: "ats/classic".into(),
            tailor_usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
            },
            subscription: false,
        };

        write_json(&build.dir, "meta.json", &meta).unwrap();

        let text = std::fs::read_to_string(build.dir.join("meta.json")).unwrap();
        let back: BuildMeta = serde_json::from_str(&text).unwrap();
        assert_eq!(back, meta);
    }

    #[test]
    fn meta_without_subscription_defaults_to_false() {
        // A meta.json written before the field existed must still load.
        let json = r#"{"created_at":"2026-06-17T00:00:00Z","model":"m","template":"ats/classic","tailor_usage":{"input_tokens":1,"output_tokens":2}}"#;
        let meta: BuildMeta = serde_json::from_str(json).unwrap();
        assert!(!meta.subscription);
    }
}
