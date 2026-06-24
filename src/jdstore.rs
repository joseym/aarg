//! The JD store: every job description aarg parses, remembered so it can be
//! reused without re-entering it.
//!
//! Until now a JD only persisted as a side effect of `tailor` (each build
//! writes its `jd.json`), so a JD entered for `gap` — or pasted and then
//! abandoned — vanished and never showed up in the reuse picker. This module
//! is the fix: a single `jds.json` under the workspace data directory holding
//! the JDs you've entered, newest first. Every command that parses a JD
//! (`gap`, `tailor`, `jd parse`, the interactive paste) records it here, so
//! the picker can offer it everywhere.
//!
//! It is a convenience cache, not the source of truth: an unreadable or
//! corrupt file is treated as empty (and the next write heals it), and a
//! write failure is non-fatal to the command that triggered it. The builds
//! remain the authoritative record of what was actually tailored.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::jd::JobRequirements;

/// How many entered JDs to keep. The picker is a convenience, not an
/// archive; the oldest entries fall off once this many accumulate.
const MAX_REMEMBERED: usize = 50;

/// Everything that can go wrong reading or writing the JD store.
#[derive(Debug, thiserror::Error)]
pub enum JdStoreError {
    #[error("could not determine where to store job descriptions")]
    NoHomeDir,

    #[error("could not read or write {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not serialize the job-description store")]
    Serialize(#[from] serde_json::Error),
}

/// One remembered JD: the requirements plus when they were last entered
/// (which orders the picker and labels each entry).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredJd {
    pub saved_at: DateTime<Utc>,
    pub requirements: JobRequirements,
}

/// Where the store lives: `jds.json` under the active workspace's data dir.
fn store_path() -> Result<PathBuf, JdStoreError> {
    crate::workspace::data_dir()
        .map(|dir| dir.join("jds.json"))
        .ok_or(JdStoreError::NoHomeDir)
}

/// The remembered JDs, newest first. A missing store is simply empty; a
/// corrupt one is treated as empty too (the next `remember` overwrites it),
/// so a damaged cache never blocks a command.
pub fn recent() -> Result<Vec<StoredJd>, JdStoreError> {
    recent_in(&store_path()?)
}

/// Remember a JD, moving it to the front. Re-entering the same posting
/// (by [`JobRequirements::identity_key`]) updates its timestamp rather than
/// duplicating it, and the list is capped at the most recent
/// [`MAX_REMEMBERED`].
pub fn remember(jd: &JobRequirements) -> Result<(), JdStoreError> {
    remember_at(&store_path()?, jd, Utc::now())
}

/// [`recent`] against an explicit file, so the read path is testable without
/// a real workspace.
fn recent_in(path: &Path) -> Result<Vec<StoredJd>, JdStoreError> {
    match std::fs::read_to_string(path) {
        // A corrupt file reads as empty — the store is a rebuildable cache,
        // so self-heal rather than fail every command that reads it.
        Ok(text) => Ok(serde_json::from_str(&text).unwrap_or_default()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(JdStoreError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// [`remember`] against an explicit file and clock, so the dedup/cap logic is
/// testable deterministically.
fn remember_at(path: &Path, jd: &JobRequirements, now: DateTime<Utc>) -> Result<(), JdStoreError> {
    let mut list = recent_in(path)?;
    // Drop any earlier copy of the same posting, then push the fresh one to
    // the front — re-entering a JD bumps it up, it doesn't pile up.
    let key = jd.identity_key();
    list.retain(|stored| stored.requirements.identity_key() != key);
    list.insert(
        0,
        StoredJd {
            saved_at: now,
            requirements: jd.clone(),
        },
    );
    list.truncate(MAX_REMEMBERED);

    write_list(path, &list)
}

/// Overwrite the store with `list` (already newest-first). The shared write
/// path behind both [`remember`] and [`save`]: ensure the directory exists,
/// then atomically replace the file with the serialized list.
fn write_list(path: &Path, list: &[StoredJd]) -> Result<(), JdStoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| JdStoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let json = serde_json::to_vec_pretty(list)?;
    std::fs::write(path, json).map_err(|source| JdStoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Replace the remembered JDs with exactly `list` (newest first) — the write
/// half of `aarg jd rm`, which reads [`recent`], drops the entries the user
/// picked, and saves what's left.
pub fn save(list: &[StoredJd]) -> Result<(), JdStoreError> {
    write_list(&store_path()?, list)
}

/// Forget every remembered JD by removing the store file. A missing file is
/// already empty, so that counts as success. The builds keep their own
/// `jd.json`, so nothing authoritative is lost — only the reuse cache.
pub fn clear() -> Result<(), JdStoreError> {
    let path = store_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(JdStoreError::Io { path, source }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::jd::{RemotePolicy, Seniority};

    fn req(company: &str, title: &str, source: Option<&str>) -> JobRequirements {
        JobRequirements {
            company: company.to_string(),
            title: title.to_string(),
            seniority: Seniority::Unspecified,
            location: None,
            remote: RemotePolicy::Unspecified,
            domain_keywords: Vec::new(),
            required_skills: Vec::new(),
            preferred_skills: Vec::new(),
            responsibilities: Vec::new(),
            ats_phrases: Vec::new(),
            raw_text: String::new(),
            source_url: source.map(str::to_string),
        }
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn a_missing_store_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jds.json");
        assert!(recent_in(&path).unwrap().is_empty());
    }

    #[test]
    fn remember_then_recent_round_trips_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jds.json");
        remember_at(&path, &req("Acme", "Engineer", None), at(1)).unwrap();
        remember_at(&path, &req("Globex", "Manager", None), at(2)).unwrap();

        let list = recent_in(&path).unwrap();
        assert_eq!(list.len(), 2);
        // Most-recently remembered is first.
        assert_eq!(list[0].requirements.company, "Globex");
        assert_eq!(list[1].requirements.company, "Acme");
    }

    #[test]
    fn re_entering_the_same_jd_bumps_it_without_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jds.json");
        remember_at(&path, &req("Acme", "Engineer", None), at(1)).unwrap();
        remember_at(&path, &req("Globex", "Manager", None), at(2)).unwrap();
        // Same posting as the first, entered again later.
        remember_at(&path, &req("Acme", "Engineer", None), at(3)).unwrap();

        let list = recent_in(&path).unwrap();
        assert_eq!(list.len(), 2, "the re-entered JD must not duplicate");
        assert_eq!(list[0].requirements.company, "Acme");
        assert_eq!(list[0].saved_at, at(3), "its timestamp is refreshed");
    }

    #[test]
    fn the_same_role_from_two_postings_stays_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jds.json");
        remember_at(&path, &req("Acme", "Engineer", Some("https://a/1")), at(1)).unwrap();
        remember_at(&path, &req("Acme", "Engineer", Some("https://a/2")), at(2)).unwrap();
        assert_eq!(recent_in(&path).unwrap().len(), 2);
    }

    #[test]
    fn write_list_overwrites_the_store_so_a_prune_can_drop_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jds.json");
        remember_at(&path, &req("Acme", "Engineer", None), at(1)).unwrap();
        remember_at(&path, &req("Globex", "Manager", None), at(2)).unwrap();

        // Keep only one (what `jd rm` does after the user picks the other).
        let kept = vec![StoredJd {
            saved_at: at(2),
            requirements: req("Globex", "Manager", None),
        }];
        write_list(&path, &kept).unwrap();

        let list = recent_in(&path).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].requirements.company, "Globex");
    }

    #[test]
    fn a_corrupt_store_reads_as_empty_and_heals_on_the_next_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jds.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(recent_in(&path).unwrap().is_empty());

        remember_at(&path, &req("Acme", "Engineer", None), at(1)).unwrap();
        assert_eq!(recent_in(&path).unwrap().len(), 1);
    }
}
