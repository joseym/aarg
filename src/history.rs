//! Build history and diff (FR-4.3), read straight from the build
//! directories.
//!
//! Every `aarg tailor` run already writes a self-contained folder under
//! `builds/<id>/` — `meta.json`, `canonical.json`, the reviewer and ATS
//! reports. That's the source of truth, so `history` and `diff` just read
//! it back: list the runs, or compare two of them field by field. The PRD
//! plans a SQLite index here, but at a few dozen builds a directory scan is
//! instant and dependency-free; the index can be added when it earns its
//! place (see the decisions log).
//!
//! Everything is read-only except `remove`, which deletes one build's
//! folder — the one way to clear an item from the history.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::ats::AtsReport;
use crate::builds::{BuildError, BuildMeta, builds_root};
use crate::jd::JobRequirements;
use crate::review::AdversarialReport;
use crate::tailor::TailoredResume;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error(transparent)]
    Build(#[from] BuildError),

    #[error("could not read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("no build {id:?} in the history")]
    NotFound { id: String },

    #[error("build {id:?} is missing or has an unreadable {artifact}")]
    ReadArtifact { id: String, artifact: String },
}

/// Read one artifact of a build by id, with a typed error naming what was
/// missing — for callers (like `attack`) that need a specific file rather
/// than a whole summary.
pub fn read_artifact<T: DeserializeOwned>(id: &str, artifact: &str) -> Result<T, HistoryError> {
    let dir = builds_root()?.join(id);
    read_json(&dir, artifact).ok_or_else(|| HistoryError::ReadArtifact {
        id: id.to_string(),
        artifact: artifact.to_string(),
    })
}

/// The combined score the loop optimizes — mirrors `tailor::combined_score`
/// so the history shows the same headline number a run printed. The weights
/// are a product decision; keep them in step with that function.
fn combined(overall_score: f32, coverage: f32) -> f32 {
    0.6 * overall_score + 0.4 * coverage
}

/// A one-line summary of one build, for `aarg history`.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildSummary {
    pub id: String,
    pub created_at: String,
    pub target: String,
    pub model: String,
    pub score: f32,
    pub review_score: f32,
    pub coverage: f32,
    pub objections: usize,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// The run was on a Claude plan, so its cost is covered by the flat fee
    /// and a dollar estimate would mislead.
    pub subscription: bool,
}

/// Every build with a complete-enough set of artifacts, newest first. A
/// half-written or hand-deleted build (missing a report) is skipped rather
/// than failing the whole listing.
pub fn list() -> Result<Vec<BuildSummary>, HistoryError> {
    list_in(&builds_root()?)
}

fn list_in(root: &Path) -> Result<Vec<BuildSummary>, HistoryError> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        // No builds directory yet just means no history.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(HistoryError::Io {
                path: root.to_path_buf(),
                source,
            });
        }
    };

    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| HistoryError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let name = entry.file_name();
        // Only numbered build dirs; strays and partials are ignored.
        let Some(id) = name.to_str().filter(|s| s.parse::<u32>().is_ok()) else {
            continue;
        };
        if let Some(summary) = summarize(&entry.path(), id) {
            out.push(summary);
        }
    }
    // Ids are zero-padded (`001`..`999`), so a string sort is chronological.
    out.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(out)
}

/// Read a JSON artifact, or `None` if it's absent or unreadable — a missing
/// piece means an incomplete build, not an error.
fn read_json<T: DeserializeOwned>(dir: &Path, name: &str) -> Option<T> {
    let text = std::fs::read_to_string(dir.join(name)).ok()?;
    serde_json::from_str(&text).ok()
}

fn summarize(dir: &Path, id: &str) -> Option<BuildSummary> {
    let meta: BuildMeta = read_json(dir, "meta.json")?;
    let report: AdversarialReport = read_json(dir, "adversarial_report.json")?;
    let ats: AtsReport = read_json(dir, "ats_report.json")?;
    let resume: TailoredResume = read_json(dir, "canonical.json")?;

    // The JD it was tailored for, if its `jd.json` is still around; else
    // fall back to the headline title or the candidate's name.
    let target = match read_json::<JobRequirements>(dir, "jd.json") {
        Some(jd) => format!("{} @ {}", jd.title, jd.company),
        None => resume
            .target_title
            .clone()
            .unwrap_or_else(|| resume.contact.full_name.clone()),
    };

    Some(BuildSummary {
        id: id.to_string(),
        created_at: meta.created_at.format("%Y-%m-%d %H:%M").to_string(),
        target,
        model: meta.model,
        score: combined(report.overall_score, ats.coverage),
        review_score: report.overall_score,
        coverage: ats.coverage,
        objections: report.objections.len(),
        tokens_in: meta.tailor_usage.input_tokens,
        tokens_out: meta.tailor_usage.output_tokens,
        subscription: meta.subscription,
    })
}

/// The artifacts a diff compares, loaded for one build.
pub struct LoadedBuild {
    pub id: String,
    pub resume: TailoredResume,
    pub report: AdversarialReport,
    pub coverage: f32,
}

/// Load one build by id, or `NotFound` if it isn't there (or is too
/// incomplete to compare).
pub fn load(id: &str) -> Result<LoadedBuild, HistoryError> {
    load_in(&builds_root()?, id)
}

fn load_in(root: &Path, id: &str) -> Result<LoadedBuild, HistoryError> {
    let dir = root.join(id);
    let not_found = || HistoryError::NotFound { id: id.to_string() };
    let resume: TailoredResume = read_json(&dir, "canonical.json").ok_or_else(not_found)?;
    let report: AdversarialReport =
        read_json(&dir, "adversarial_report.json").ok_or_else(not_found)?;
    let ats: AtsReport = read_json(&dir, "ats_report.json").ok_or_else(not_found)?;
    Ok(LoadedBuild {
        id: id.to_string(),
        resume,
        report,
        coverage: ats.coverage,
    })
}

/// What changed between two builds.
pub struct BuildDiff {
    pub from: String,
    pub to: String,
    pub score_from: f32,
    pub score_to: f32,
    pub coverage_from: f32,
    pub coverage_to: f32,
    pub objections_from: usize,
    pub objections_to: usize,
    pub skills_added: Vec<String>,
    pub skills_removed: Vec<String>,
    pub bullets_added: Vec<String>,
    pub bullets_removed: Vec<String>,
    pub bullets_changed: Vec<BulletChange>,
}

/// A bullet present in both builds whose text was reworded between them.
pub struct BulletChange {
    pub id: String,
    pub from: String,
    pub to: String,
}

/// Compare two builds by id.
pub fn diff(from_id: &str, to_id: &str) -> Result<BuildDiff, HistoryError> {
    let from = load(from_id)?;
    let to = load(to_id)?;
    Ok(compute_diff(&from, &to))
}

fn compute_diff(from: &LoadedBuild, to: &LoadedBuild) -> BuildDiff {
    let from_skills: Vec<&String> = from.resume.skills_section.skills.iter().collect();
    let to_skills: Vec<&String> = to.resume.skills_section.skills.iter().collect();

    let skills_added = to_skills
        .iter()
        .filter(|s| !from_skills.contains(s))
        .map(|s| (*s).clone())
        .collect();
    let skills_removed = from_skills
        .iter()
        .filter(|s| !to_skills.contains(s))
        .map(|s| (*s).clone())
        .collect();

    // Bullets keyed by their source id, across every role.
    let from_bullets = bullets_by_id(&from.resume);
    let to_bullets = bullets_by_id(&to.resume);

    let mut bullets_added = Vec::new();
    let mut bullets_changed = Vec::new();
    for (id, text) in &to_bullets {
        match from_bullets.iter().find(|(fid, _)| fid == id) {
            None => bullets_added.push(id.clone()),
            Some((_, from_text)) if from_text != text => bullets_changed.push(BulletChange {
                id: id.clone(),
                from: from_text.clone(),
                to: text.clone(),
            }),
            Some(_) => {}
        }
    }
    let bullets_removed = from_bullets
        .iter()
        .filter(|(id, _)| !to_bullets.iter().any(|(tid, _)| tid == id))
        .map(|(id, _)| id.clone())
        .collect();

    BuildDiff {
        from: from.id.clone(),
        to: to.id.clone(),
        score_from: combined(from.report.overall_score, from.coverage),
        score_to: combined(to.report.overall_score, to.coverage),
        coverage_from: from.coverage,
        coverage_to: to.coverage,
        objections_from: from.report.objections.len(),
        objections_to: to.report.objections.len(),
        skills_added,
        skills_removed,
        bullets_added,
        bullets_removed,
        bullets_changed,
    }
}

/// (source_id, text) for every bullet on the page, in order.
fn bullets_by_id(resume: &TailoredResume) -> Vec<(String, String)> {
    resume
        .roles
        .iter()
        .flat_map(|role| &role.bullets)
        .map(|bullet| (bullet.source_id.0.clone(), bullet.text.clone()))
        .collect()
}

/// Delete one build's directory — the way to clear an item from the
/// history. Returns `NotFound` if no such build, so the caller can report
/// it; only removes a numbered directory under the builds root.
pub fn remove(id: &str) -> Result<(), HistoryError> {
    remove_in(&builds_root()?, id)
}

fn remove_in(root: &Path, id: &str) -> Result<(), HistoryError> {
    // Guard: only ever a bare build number, never a path that could escape
    // the builds root.
    if id.parse::<u32>().is_err() {
        return Err(HistoryError::NotFound { id: id.to_string() });
    }
    let dir = root.join(id);
    if !dir.is_dir() {
        return Err(HistoryError::NotFound { id: id.to_string() });
    }
    std::fs::remove_dir_all(&dir).map_err(|source| HistoryError::Io { path: dir, source })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{Contact, YearMonth};
    use crate::llm::TokenUsage;
    use crate::review::{ObjectionScope, Severity};
    use crate::tailor::{
        BuildId, JdId, SkillsSection, TailoredBullet, TailoredResume, TailoredRole,
    };
    use chrono::Utc;

    fn resume(skills: &[&str], bullets: &[(&str, &str)]) -> TailoredResume {
        TailoredResume {
            build_id: BuildId("000".into()),
            jd_id: JdId("acme".into()),
            generated_at: Utc::now(),
            contact: Contact {
                full_name: "Ada".into(),
                email: "ada@example.com".into(),
                phone: None,
                location: None,
                links: Vec::new(),
            },
            target_title: Some("Engineer".into()),
            summary: "s".into(),
            roles: vec![TailoredRole {
                id: crate::dataset::types::RoleId("role-1".into()),
                company: "Acme".into(),
                title: "Engineer".into(),
                start: YearMonth {
                    year: 2020,
                    month: 1,
                },
                end: None,
                location: None,
                bullets: bullets
                    .iter()
                    .map(|(id, text)| TailoredBullet {
                        source_id: crate::dataset::types::BulletId((*id).into()),
                        text: (*text).into(),
                    })
                    .collect(),
            }],
            education: Vec::new(),
            skills_section: SkillsSection {
                skills: skills.iter().map(|s| (*s).to_string()).collect(),
            },
            projects: Vec::new(),
            achievements: Vec::new(),
            certifications: Vec::new(),
        }
    }

    fn report(score: f32, objections: usize) -> AdversarialReport {
        use crate::review::{Objection, ObjectionKind, ObjectionTarget};
        AdversarialReport {
            objections: (0..objections)
                .map(|_| Objection {
                    target: ObjectionTarget::Overall,
                    severity: Severity::Minor,
                    kind: ObjectionKind::Other,
                    scope: ObjectionScope::Canonical,
                    message: "m".into(),
                    suggestion: None,
                })
                .collect(),
            overall_score: score,
            persona_notes: "ok".into(),
        }
    }

    fn write_build(
        root: &Path,
        id: &str,
        skills: &[&str],
        bullets: &[(&str, &str)],
        score: f32,
        objections: usize,
        coverage: f32,
    ) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let meta = BuildMeta {
            created_at: Utc::now(),
            model: "m".into(),
            template: "ats/classic".into(),
            tailor_usage: TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
            },
            subscription: false,
        };
        let ats = crate::ats::AtsReport {
            keyword_hits: Vec::new(),
            keyword_misses: Vec::new(),
            coverage,
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
        std::fs::write(
            dir.join("canonical.json"),
            serde_json::to_vec(&resume(skills, bullets)).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("adversarial_report.json"),
            serde_json::to_vec(&report(score, objections)).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("ats_report.json"),
            serde_json::to_vec(&ats).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn list_returns_complete_builds_newest_first() {
        let root = tempfile::tempdir().unwrap();
        write_build(root.path(), "001", &["Rust"], &[("b1", "x")], 0.7, 3, 1.0);
        write_build(root.path(), "002", &["Rust"], &[("b1", "x")], 0.8, 2, 1.0);
        // A stray dir and an incomplete build are both ignored.
        std::fs::create_dir_all(root.path().join("not-a-build")).unwrap();
        std::fs::create_dir_all(root.path().join("003")).unwrap();

        let list = list_in(root.path()).unwrap();
        let ids: Vec<&str> = list.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["002", "001"]);
        // Combined score = 0.6*review + 0.4*coverage.
        assert!((list[0].score - (0.6 * 0.8 + 0.4)).abs() < 1e-6);
    }

    #[test]
    fn diff_reports_skill_and_bullet_changes() {
        let root = tempfile::tempdir().unwrap();
        write_build(
            root.path(),
            "001",
            &["Rust", "Go"],
            &[("b1", "old text"), ("b2", "kept")],
            0.7,
            5,
            0.8,
        );
        write_build(
            root.path(),
            "002",
            &["Rust", "Kubernetes"],
            &[("b1", "new text"), ("b3", "added")],
            0.8,
            3,
            1.0,
        );

        let from = load_in(root.path(), "001").unwrap();
        let to = load_in(root.path(), "002").unwrap();
        let d = compute_diff(&from, &to);

        assert_eq!(d.skills_added, vec!["Kubernetes"]);
        assert_eq!(d.skills_removed, vec!["Go"]);
        assert_eq!(d.bullets_added, vec!["b3"]);
        assert_eq!(d.bullets_removed, vec!["b2"]);
        assert_eq!(d.bullets_changed.len(), 1);
        assert_eq!(d.bullets_changed[0].id, "b1");
        assert_eq!(d.objections_from, 5);
        assert_eq!(d.objections_to, 3);
    }

    #[test]
    fn remove_deletes_a_build_and_rejects_a_stray_id() {
        let root = tempfile::tempdir().unwrap();
        write_build(root.path(), "001", &["Rust"], &[("b1", "x")], 0.7, 1, 1.0);

        remove_in(root.path(), "001").unwrap();
        assert!(!root.path().join("001").is_dir());

        // A non-numeric id never resolves to a path.
        assert!(matches!(
            remove_in(root.path(), "../etc"),
            Err(HistoryError::NotFound { .. })
        ));
        // A missing build is NotFound, not an IO error.
        assert!(matches!(
            remove_in(root.path(), "999"),
            Err(HistoryError::NotFound { .. })
        ));
    }
}
