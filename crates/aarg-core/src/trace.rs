//! Agent run traces: one JSON file per run, the runtime's flight
//! recorder (FR-2.4, PRD §8/§9.5).
//!
//! A trace records what an agent was asked (the serialized input and
//! the full conversation, retry turns included), what the model said
//! (the raw final reply), and what it cost (token usage, duration) —
//! enough to replay an argument with a model after the fact. Files
//! land in the per-OS data directory under `traces/`, named
//! `YYYY-MM-DDTHH-MM-SS_<agent>_<short>.json` so a directory listing
//! sorts chronologically.
//!
//! Recording is **best-effort by design**: a run that cost real tokens
//! must never fail because the trace directory is read-only. Reading
//! traces back (for `aarg trace last|show`) reports errors normally —
//! a missing or corrupt trace is worth telling the user about.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
#[cfg(feature = "native")]
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::llm::{Message, TokenUsage};

/// Everything that can go wrong while *reading* traces. (Writing
/// swallows errors — see the module docs.)
#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("could not determine this user's home directory")]
    NoHomeDir,

    #[error("no traces recorded yet")]
    Empty,

    #[error("no trace named {id:?}")]
    NotFound { id: String },

    #[error("could not read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a valid trace file")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// A trace's identity — also its filename stem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TraceId(pub String);

/// One recorded agent run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub trace_id: TraceId,
    /// The agent's stable identifier (`Agent::id()`).
    pub agent: String,
    pub started_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub model: String,
    /// The agent's typed input, serialized.
    pub input: serde_json::Value,
    /// The system prompt the run used.
    pub system: String,
    /// The conversation as last sent — retry turns appear here as
    /// assistant/user pairs.
    pub messages: Vec<Message>,
    /// The model's final raw reply, if one arrived.
    pub reply: Option<String>,
    pub usage: TokenUsage,
    pub outcome: TraceOutcome,
}

/// How the run ended. Failures are traced too — a run that died on a
/// parse error is exactly the one worth replaying.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TraceOutcome {
    Succeeded,
    Failed { error: String },
}

/// Where traces go. `DISABLED` (for tests and library callers that
/// don't want files) records nothing.
#[derive(Debug)]
pub struct Tracer {
    dir: Option<PathBuf>,
}

impl Tracer {
    /// A tracer that records nothing. A constant so test contexts can
    /// borrow it without a local binding.
    pub const DISABLED: Tracer = Tracer { dir: None };

    /// Record into the standard per-OS location
    /// (`~/.local/share/aarg/traces` on Linux). Native-only: a wasm build has
    /// no per-OS data directory and uses [`Tracer::DISABLED`] or [`Tracer::to_dir`].
    #[cfg(feature = "native")]
    pub fn to_default_dir() -> Result<Self, TraceError> {
        Ok(Self {
            dir: Some(default_dir()?),
        })
    }

    /// Record into a specific directory (tests use a tempdir).
    pub fn to_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
        }
    }

    /// Write one trace. Best-effort: failures are swallowed because
    /// tracing must never sink the run it describes.
    pub fn record(&self, trace: &Trace) {
        let Some(dir) = &self.dir else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        if let Ok(json) = serde_json::to_vec_pretty(trace) {
            let _ = std::fs::write(dir.join(format!("{}.json", trace.trace_id.0)), json);
        }
    }
}

/// Build a trace's identity from its agent and start time. The short
/// suffix (sub-second micros) keeps two runs in the same second apart.
pub fn trace_id(agent: &str, started_at: DateTime<Utc>) -> TraceId {
    TraceId(format!(
        "{}_{}_{:05x}",
        started_at.format("%Y-%m-%dT%H-%M-%S"),
        agent,
        started_at.timestamp_subsec_micros()
    ))
}

/// The standard traces directory. Native-only (resolves a per-OS data dir).
#[cfg(feature = "native")]
pub fn default_dir() -> Result<PathBuf, TraceError> {
    ProjectDirs::from("", "", "aarg")
        .map(|dirs| dirs.data_dir().join("traces"))
        .ok_or(TraceError::NoHomeDir)
}

/// The most recent trace in a directory. Filenames start with a
/// zero-padded timestamp, so lexicographic max is chronological max.
pub fn latest_in(dir: &Path) -> Result<Trace, TraceError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(TraceError::Empty),
        Err(source) => {
            return Err(TraceError::Read {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    let newest = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .max();
    match newest {
        Some(path) => load_path(&path),
        None => Err(TraceError::Empty),
    }
}

/// Load one trace by its ID (the filename stem; a trailing `.json` is
/// tolerated because users will paste filenames).
pub fn load_from(dir: &Path, id: &str) -> Result<Trace, TraceError> {
    let stem = id.strip_suffix(".json").unwrap_or(id);
    let path = dir.join(format!("{stem}.json"));
    if !path.exists() {
        return Err(TraceError::NotFound {
            id: stem.to_string(),
        });
    }
    load_path(&path)
}

fn load_path(path: &Path) -> Result<Trace, TraceError> {
    let text = std::fs::read_to_string(path).map_err(|source| TraceError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| TraceError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_trace(agent: &str, started_at: DateTime<Utc>) -> Trace {
        Trace {
            trace_id: trace_id(agent, started_at),
            agent: agent.to_string(),
            started_at,
            duration_ms: 1200,
            model: "test-model".into(),
            input: serde_json::json!({"text": "hello"}),
            system: "You do things.".into(),
            messages: vec![Message::user("hello")],
            reply: Some("{\"ok\": true}".into()),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
            },
            outcome: TraceOutcome::Succeeded,
        }
    }

    fn at(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 12, h, m, s).unwrap()
    }

    #[test]
    fn record_writes_a_chronologically_named_file() {
        let dir = tempfile::tempdir().unwrap();
        let tracer = Tracer::to_dir(dir.path());
        let trace = sample_trace("jd_parser_v1", at(9, 30, 0));

        tracer.record(&trace);

        let path = dir.path().join(format!("{}.json", trace.trace_id.0));
        assert!(path.exists());
        assert!(
            trace
                .trace_id
                .0
                .starts_with("2026-06-12T09-30-00_jd_parser_v1_"),
            "got {:?}",
            trace.trace_id.0
        );
    }

    #[test]
    fn latest_picks_the_newest_by_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let tracer = Tracer::to_dir(dir.path());
        tracer.record(&sample_trace("a", at(9, 0, 0)));
        tracer.record(&sample_trace("b", at(11, 0, 0)));
        tracer.record(&sample_trace("c", at(10, 0, 0)));

        let latest = latest_in(dir.path()).unwrap();
        assert_eq!(latest.agent, "b");
    }

    #[test]
    fn load_by_id_tolerates_a_pasted_filename() {
        let dir = tempfile::tempdir().unwrap();
        let tracer = Tracer::to_dir(dir.path());
        let trace = sample_trace("a", at(9, 0, 0));
        tracer.record(&trace);

        let by_stem = load_from(dir.path(), &trace.trace_id.0).unwrap();
        let by_file = load_from(dir.path(), &format!("{}.json", trace.trace_id.0)).unwrap();
        assert_eq!(by_stem, trace);
        assert_eq!(by_file, trace);

        assert!(matches!(
            load_from(dir.path(), "no-such-trace"),
            Err(TraceError::NotFound { .. })
        ));
    }

    #[test]
    fn an_empty_or_missing_directory_is_a_typed_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(latest_in(dir.path()), Err(TraceError::Empty)));
        assert!(matches!(
            latest_in(&dir.path().join("never-created")),
            Err(TraceError::Empty)
        ));
    }

    #[test]
    fn the_disabled_tracer_writes_nothing_and_never_fails() {
        // No directory involved at all; recording is a no-op.
        Tracer::DISABLED.record(&sample_trace("a", at(9, 0, 0)));
    }

    // EXERCISE(EX-015)
    #[test]
    #[ignore = "exercise: traces accumulate forever; make record() prune the directory to the newest 200 files after writing, then finish this test"]
    fn ex_015_old_traces_are_pruned() {
        // Once pruning exists: record 205 traces with distinct
        // timestamps, assert exactly 200 files remain, and assert the
        // five oldest are the ones gone. Watch the edge: pruning
        // failures must be swallowed like every other tracing failure.
        let pruning_implemented = false;
        assert!(pruning_implemented);
    }
}
