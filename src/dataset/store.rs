//! Loading and saving `dataset.json` — the dataset's home on disk.
//!
//! The dataset is the only copy of the user's career data, so writes are
//! paranoid (PRD §12 reliability):
//!
//! - **Atomic**: the new JSON is written to a temp file, fsynced, then
//!   renamed over the old one — a crash mid-write leaves the previous
//!   dataset intact, never a half-written file.
//! - **Locked**: an advisory lock on `dataset.lock` makes a concurrent
//!   `aarg` invocation fail fast instead of racing the write.
//! - **Backed up**: the previous `dataset.json` is copied to
//!   `dataset.json.bak` before being replaced.
//!
//! Reads check `schema_version` first, so an old binary refuses a newer
//! file with an upgrade hint instead of misparsing it.
//!
//! IO here is deliberately synchronous `std::fs`: these are small local
//! files read once per command, where async buys nothing.

use std::fs::{self, File, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::Deserialize;

use crate::dataset::types::{ResumeDataset, SCHEMA_VERSION};

/// Everything that can go wrong while reading or writing the dataset.
#[derive(Debug, thiserror::Error)]
pub enum DatasetError {
    #[error("could not determine this user's home directory")]
    NoHomeDir,

    #[error("no dataset yet at {path} — run `aarg ingest <resume>` to create one")]
    NotFound { path: PathBuf },

    #[error("another aarg process is writing the dataset (lock held on {path})")]
    Locked { path: PathBuf },

    #[error(
        "this dataset uses schema version {found}, but this aarg only \
         understands up to {supported} — upgrade aarg to read it"
    )]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("could not read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a valid dataset file")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("could not serialize the dataset to JSON")]
    Serialize(#[source] serde_json::Error),
}

/// The directory the dataset lives in — the same per-OS config directory
/// as `config.toml` (PRD §8).
pub fn dir() -> Result<PathBuf, DatasetError> {
    ProjectDirs::from("", "", "aarg")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .ok_or(DatasetError::NoHomeDir)
}

/// Load the dataset from its default location.
pub fn load() -> Result<ResumeDataset, DatasetError> {
    load_from(&dir()?)
}

/// Save the dataset to its default location: lock, back up, write
/// atomically.
pub fn save(dataset: &ResumeDataset) -> Result<(), DatasetError> {
    save_to(&dir()?, dataset)
}

/// Just the version field, parsed before anything else so a newer file is
/// rejected with a clear message rather than a pile of field errors.
#[derive(Deserialize)]
struct SchemaProbe {
    schema_version: u32,
}

fn load_from(dir: &Path) -> Result<ResumeDataset, DatasetError> {
    let path = dir.join("dataset.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(DatasetError::NotFound { path });
        }
        Err(source) => return Err(DatasetError::Read { path, source }),
    };

    let parse_err = |source| DatasetError::Parse {
        path: path.clone(),
        source,
    };
    let probe: SchemaProbe = serde_json::from_str(&text).map_err(parse_err)?;
    if probe.schema_version > SCHEMA_VERSION {
        return Err(DatasetError::SchemaTooNew {
            found: probe.schema_version,
            supported: SCHEMA_VERSION,
        });
    }

    serde_json::from_str(&text).map_err(parse_err)
}

// EXERCISE(EX-006)
fn save_to(dir: &Path, dataset: &ResumeDataset) -> Result<(), DatasetError> {
    fs::create_dir_all(dir).map_err(|source| DatasetError::Write {
        path: dir.to_path_buf(),
        source,
    })?;

    // Held until `_lock` drops at the end of the function; the file handle
    // closing releases the OS lock.
    let _lock = acquire_lock(dir)?;

    let path = dir.join("dataset.json");
    let backup = dir.join("dataset.json.bak");
    if path.exists() {
        fs::copy(&path, &backup).map_err(|source| DatasetError::Write {
            path: backup,
            source,
        })?;
    }

    let json = serde_json::to_string_pretty(dataset).map_err(DatasetError::Serialize)?;

    // Temp file in the same directory, so the final rename cannot cross a
    // filesystem boundary (rename is only atomic within one filesystem).
    let tmp = dir.join("dataset.json.tmp");
    let write_err = |source| DatasetError::Write {
        path: tmp.clone(),
        source,
    };
    let mut file = File::create(&tmp).map_err(write_err)?;
    file.write_all(json.as_bytes()).map_err(write_err)?;
    // Force the bytes to disk before the rename makes them "the dataset".
    file.sync_all().map_err(write_err)?;
    drop(file);

    fs::rename(&tmp, &path).map_err(|source| DatasetError::Write { path, source })
}

fn acquire_lock(dir: &Path) -> Result<File, DatasetError> {
    let path = dir.join("dataset.lock");
    let file = File::create(&path).map_err(|source| DatasetError::Write {
        path: path.clone(),
        source,
    })?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(DatasetError::Locked { path }),
        Err(TryLockError::Error(source)) => Err(DatasetError::Write { path, source }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::Contact;

    fn sample_dataset() -> ResumeDataset {
        ResumeDataset::new(Contact {
            full_name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        })
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let dataset = sample_dataset();
        save_to(dir.path(), &dataset).unwrap();
        let back = load_from(dir.path()).unwrap();
        assert_eq!(back, dataset);
    }

    #[test]
    fn missing_dataset_is_a_typed_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            load_from(dir.path()),
            Err(DatasetError::NotFound { .. })
        ));
    }

    #[test]
    fn newer_schema_versions_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("dataset.json"),
            r#"{"schema_version": 999}"#,
        )
        .unwrap();
        assert!(matches!(
            load_from(dir.path()),
            Err(DatasetError::SchemaTooNew {
                found: 999,
                supported: SCHEMA_VERSION
            })
        ));
    }

    #[test]
    fn corrupt_json_is_a_parse_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("dataset.json"), "{ not json").unwrap();
        assert!(matches!(
            load_from(dir.path()),
            Err(DatasetError::Parse { .. })
        ));
    }

    #[test]
    fn save_backs_up_the_previous_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let first = sample_dataset();
        save_to(dir.path(), &first).unwrap();

        let mut second = first.clone();
        second.summary = Some("Now with a summary".into());
        save_to(dir.path(), &second).unwrap();

        let backup_text = fs::read_to_string(dir.path().join("dataset.json.bak")).unwrap();
        let backup: ResumeDataset = serde_json::from_str(&backup_text).unwrap();
        assert_eq!(backup, first);
        assert_eq!(load_from(dir.path()).unwrap(), second);
    }

    #[test]
    fn no_temp_file_survives_a_save() {
        let dir = tempfile::tempdir().unwrap();
        save_to(dir.path(), &sample_dataset()).unwrap();
        assert!(!dir.path().join("dataset.json.tmp").exists());
    }

    #[test]
    fn a_held_lock_makes_save_fail_fast() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate another aarg process: hold the lock on a separate file
        // handle for the duration of the save attempt.
        let other = File::create(dir.path().join("dataset.lock")).unwrap();
        other.try_lock().unwrap();

        assert!(matches!(
            save_to(dir.path(), &sample_dataset()),
            Err(DatasetError::Locked { .. })
        ));
    }

    #[test]
    #[ignore = "exercise: save() overwrites the single dataset.json.bak each time; rotate three generations (.bak, .bak.1, .bak.2) oldest-out, then finish this test"]
    fn ex_006_backups_rotate_through_three_generations() {
        // Once rotation exists: save four distinct datasets, then assert
        // .bak holds the 3rd, .bak.1 the 2nd, .bak.2 the 1st.
        let rotation_implemented = false;
        assert!(rotation_implemented);
    }
}
