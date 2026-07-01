//! Deterministic per-line provenance: classify a canonical `TailoredResume`'s
//! role bullets, its summary, and its skills — the three kinds of line that
//! come out of the model — by whether they trace back to the user's recorded
//! `ResumeDataset` (PRD §7's evidence discipline, checked against the
//! finished draft instead of at assembly time). `target_title`, projects,
//! achievements, and certifications aren't classified yet: `tailor::assemble`
//! resolves projects, achievements, and certifications by id straight from
//! the dataset (never rephrased), and sets `target_title` from the JD's own
//! title rather than from anything the model wrote about the user — none of
//! the four is model prose that could drift from its evidence, so there's no
//! rewrite for this module to check yet. That changes if the free-edit UI
//! lets a person hand-edit those sections directly — extending
//! classification to cover them is the follow-up for whenever that lands.
//!
//! A **service**: pure code, no LLM, no filesystem — the same discipline as
//! `mirror.rs`. It exists for the browser UI's free-edit story: once a user
//! hand-edits a line, `check_provenance` re-classifies the whole draft so
//! the UI can show "your words" for a line with no plausible dataset
//! source, versus a line still traceable to something recorded, and a
//! hover can point at exactly where a line came from.
//!
//! Every draft line lands in one of three buckets ([`ProvenanceStatus`]):
//! - **verbatim** — the line's text matches a recorded dataset text
//!   exactly (whitespace-normalized). When the line carries a declared
//!   source id (a bullet's `source_id`), that source is checked first — an
//!   id match with identical text is the strong case.
//! - **grounded** — not identical, but a specific dataset text shares
//!   enough of the line's meaningful words that it's a defensible rewrite
//!   of it. `tailor.rs` mirrors JD vocabulary into bullets, so most
//!   model-tailored lines land here, not in `verbatim`.
//! - **unrecorded** — no dataset text plausibly backs the line: a
//!   hand-typed addition, or a rewrite that drifted too far from its
//!   source to still call "derived".
//!
//! **This module is informational, not enforcement.** It never blocks a
//! build or rejects a draft — the never-fabricate guards that do that live
//! upstream, structurally, in `tailor.rs`'s `assemble` (a rewrite may not
//! introduce a number its source doesn't state) and in the adversarial
//! reviewer. An `unrecorded` line here is not a violation; it's the UI's
//! cue that these are the user's own words, which is allowed —
//! never-fabricate governs what the *model* may claim, never what the user
//! may type.

use serde::{Deserialize, Serialize};

use crate::dataset::types::{Bullet, BulletId, ResumeDataset, RoleId, SkillId};
use crate::keywords::{is_token_subset, keyword_key};
use crate::tailor::{TailoredBullet, TailoredResume, digit_runs};

/// Where one classified line sits in the draft. Tagged so the JSON crossing
/// into JS is self-describing (`{"kind": "role_bullet", ...}`), the same
/// convention `dataset::types::EvidenceRef` uses for its own tagged union.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LineLocation {
    /// The one free-prose summary field.
    Summary,
    /// One bullet under one role, addressed by the role's id and the
    /// bullet's position in that role's (already-selected) list.
    RoleBullet {
        role_id: RoleId,
        bullet_index: usize,
    },
    /// One entry in the skills section, addressed by its position.
    Skill { index: usize },
}

/// Which recorded dataset text a line's `best_match` points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceRef {
    Bullet { id: BulletId },
    Skill { id: SkillId },
    Summary,
}

/// The closest recorded source found for a line, and how close it is.
/// `score` is a normalized-token overlap fraction in `[0.0, 1.0]`; `1.0`
/// means either an exact (whitespace-normalized) text match or, for a
/// skill, that every one of the line's meaningful tokens is present in the
/// matched skill's tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceMatch {
    pub source: SourceRef,
    pub score: f64,
}

/// The three-way call `check_provenance` makes on every line. See the
/// module doc for what each one means and — just as important — what it
/// does *not* mean (this is not the never-fabricate gate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceStatus {
    Verbatim,
    Grounded,
    Unrecorded,
}

/// One classified line: where it sits, its text, the call, and (when the
/// dataset has anything of that kind to compare against) the closest
/// recorded source found — populated even when `status` is `unrecorded`,
/// so a hover can say "closest guess, but not close enough" rather than
/// showing nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineProvenance {
    pub location: LineLocation,
    pub text: String,
    pub status: ProvenanceStatus,
    pub best_match: Option<SourceMatch>,
}

/// The whole draft's provenance, one entry per role bullet, the summary,
/// and each skill — in that order, roles first (in draft order, each
/// role's bullets in draft order), then the summary, then skills.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceReport {
    pub lines: Vec<LineProvenance>,
}

/// A line whose meaningful tokens overlap a single dataset text at or
/// above this fraction is classified `grounded`. The tailoring pipeline
/// rewords bullets to mirror JD language, so most tailored lines keep most
/// of their source's content words while trading a few for the JD's — a
/// bare majority overlap is too weak a claim of "derived from", so the bar
/// sits above half. Below it, the honest call is `unrecorded`: never claim
/// a closer trace than the words actually support.
const GROUNDED_THRESHOLD: f64 = 0.6;

/// Classify every role bullet, the summary, and every skill in `draft`
/// against the recorded material in `dataset`. See the module doc for the
/// three statuses and the (deliberate) absence of any hard failure here —
/// this never returns an `Err`, because there is nothing to reject: it
/// reports, it doesn't gate.
pub fn check_provenance(draft: &TailoredResume, dataset: &ResumeDataset) -> ProvenanceReport {
    let bullets = all_bullets(dataset);

    let mut lines = Vec::new();
    for role in &draft.roles {
        for (bullet_index, bullet) in role.bullets.iter().enumerate() {
            lines.push(classify_bullet(bullet, &role.id, bullet_index, &bullets));
        }
    }
    lines.push(classify_summary(&draft.summary, dataset));
    for (index, skill) in draft.skills_section.skills.iter().enumerate() {
        lines.push(classify_skill(skill, index, dataset));
    }

    ProvenanceReport { lines }
}

/// Every bullet recorded across every role, flattened once and reused for
/// every bullet line in the draft — the dataset is small enough (a career,
/// not a corpus) that an O(draft bullets × dataset bullets) scan is cheap
/// and, unlike an index, needs no upkeep as the dataset changes.
fn all_bullets(dataset: &ResumeDataset) -> Vec<&Bullet> {
    dataset
        .roles
        .iter()
        .flat_map(|role| &role.bullets)
        .collect()
}

/// A bullet's text plus its measured-result `metric`, if it has one,
/// appended the same way the tailoring prompt shows it to the model
/// (`tailor::build_user_message`). A rewrite is allowed to fold a metric's
/// number into the sentence ("...3x faster than the prior process") even
/// though that number never appears in the bullet's own `text` — so the
/// overlap scorer needs the metric in its candidate text too, or a
/// perfectly grounded rewrite would score as if the number came from
/// nowhere. Not used for the `verbatim` check, which compares `text` only:
/// a metric is a separate field, not part of the recorded line's words.
fn bullet_match_text(bullet: &Bullet) -> String {
    match &bullet.metric {
        Some(metric) => format!("{} {}", bullet.text, metric.0),
        None => bullet.text.clone(),
    }
}

/// Collapse runs of whitespace to a single space and trim the ends. The
/// `verbatim` status is whitespace-normalized-but-otherwise-exact text
/// equality — the strong case that a line traces to a specific recorded
/// text, as opposed to merely resembling one.
fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The fraction of `line`'s meaningful, normalized tokens that also appear
/// in `candidate`'s — `keyword_key` does the normalizing (lowercase, drop
/// filler words, light-stem, dedupe), so this reuses the same notion of
/// "meaningful word" the rest of the crate already agreed on rather than
/// inventing a second one. An empty token set (an empty line, or one made
/// entirely of filler words) has nothing meaningful to check, so it scores
/// `0.0` rather than dividing by zero.
fn overlap_score(line: &str, candidate: &str) -> f64 {
    let line_tokens = keyword_key(line);
    if line_tokens.is_empty() {
        return 0.0;
    }
    let candidate_tokens = keyword_key(candidate);
    let shared = line_tokens
        .iter()
        .filter(|t| candidate_tokens.contains(t))
        .count();
    shared as f64 / line_tokens.len() as f64
}

/// Turn an overlap score against one known source into the three-way call a
/// non-verbatim line can land in. Below the threshold it's `unrecorded`,
/// full stop — there's nothing else to check. At or above the threshold, a
/// second, independent test guards against a false `grounded`: word
/// overlap alone can't tell a reworded line from one that kept every word
/// but swapped in a different number ("45 minutes to 3" reads as an 89%
/// match against a recorded "45 minutes to 8" — plenty of shared tokens,
/// wrong figure). So every digit run in `line` (via `tailor::digit_runs`,
/// the fabrication guard's own tokenizer) must also appear in
/// `source_text` — for a bullet, `source_text` already includes its
/// `metric` field (`bullet_match_text`), the same allowed set
/// `tailor::assemble`'s digit guard checks a model rewrite against, applied
/// here informationally instead of as a build-time gate. A line that fails
/// this second test is demoted to `unrecorded`: it read as a defensible
/// rewrite by its words, but states a fact the matched source doesn't
/// back, so `grounded` would be a misleading badge on an altered figure.
/// Exact (whitespace-normalized) equality is handled separately as
/// `verbatim` and never reaches this function.
fn status_for_score(score: f64, line: &str, source_text: &str) -> ProvenanceStatus {
    if score < GROUNDED_THRESHOLD {
        return ProvenanceStatus::Unrecorded;
    }
    if digit_runs(line).is_subset(&digit_runs(source_text)) {
        ProvenanceStatus::Grounded
    } else {
        ProvenanceStatus::Unrecorded
    }
}

fn classify_bullet(
    bullet: &TailoredBullet,
    role_id: &RoleId,
    bullet_index: usize,
    bullets: &[&Bullet],
) -> LineProvenance {
    let location = LineLocation::RoleBullet {
        role_id: role_id.clone(),
        bullet_index,
    };

    // Verify the declared source first: an id match with identical
    // (whitespace-normalized) text is the strong case for `verbatim`. This
    // covers both an untouched dataset bullet copied straight onto the
    // page and the assembly guard's own floor top-up, which does exactly
    // that (`tailor::top_up`).
    if let Some(source) = bullets.iter().find(|b| b.id == bullet.source_id)
        && normalize_whitespace(&bullet.text) == normalize_whitespace(&source.text)
    {
        return LineProvenance {
            location,
            text: bullet.text.clone(),
            status: ProvenanceStatus::Verbatim,
            best_match: Some(SourceMatch {
                source: SourceRef::Bullet {
                    id: bullet.source_id.clone(),
                },
                score: 1.0,
            }),
        };
    }

    // Not an exact match against the declared source (or the id didn't
    // resolve at all). Search every recorded bullet, not only the cited
    // one — an honest check has to consider that the citation could be
    // stale (a hand-edit changed the words but not the id) or simply
    // wrong, and the strongest evidence for a line may live elsewhere.
    match bullets
        .iter()
        .map(|b| {
            let match_text = bullet_match_text(b);
            (
                b.id.clone(),
                overlap_score(&bullet.text, &match_text),
                match_text,
            )
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
    {
        Some((id, score, match_text)) => LineProvenance {
            location,
            text: bullet.text.clone(),
            status: status_for_score(score, &bullet.text, &match_text),
            best_match: Some(SourceMatch {
                source: SourceRef::Bullet { id },
                score,
            }),
        },
        None => LineProvenance {
            location,
            text: bullet.text.clone(),
            status: ProvenanceStatus::Unrecorded,
            best_match: None,
        },
    }
}

/// The summary is the one stretch of free prose (see `tailor.rs`'s module
/// doc): the model writes it from the whole work history rather than
/// rephrasing one recorded line, so token overlap against an arbitrary
/// bullet couldn't honestly be called "derived from that bullet". The one
/// dataset text a summary line can plausibly trace to is the dataset's own
/// `summary` field — when the user has confirmed one (`summary_confirmed`),
/// `tailor::assemble` uses it verbatim, so the common case really is exact
/// equality here.
fn classify_summary(summary: &str, dataset: &ResumeDataset) -> LineProvenance {
    let location = LineLocation::Summary;
    match dataset.summary.as_deref() {
        Some(recorded) => {
            if normalize_whitespace(summary) == normalize_whitespace(recorded) {
                return LineProvenance {
                    location,
                    text: summary.to_string(),
                    status: ProvenanceStatus::Verbatim,
                    best_match: Some(SourceMatch {
                        source: SourceRef::Summary,
                        score: 1.0,
                    }),
                };
            }
            let score = overlap_score(summary, recorded);
            LineProvenance {
                location,
                text: summary.to_string(),
                status: status_for_score(score, summary, recorded),
                best_match: Some(SourceMatch {
                    source: SourceRef::Summary,
                    score,
                }),
            }
        }
        None => LineProvenance {
            location,
            text: summary.to_string(),
            status: ProvenanceStatus::Unrecorded,
            best_match: None,
        },
    }
}

/// A skill line's provenance is a subset check, not a fractional overlap:
/// a mirrored ATS phrase ("managing engineering") reaches the page only
/// when `mirror::backed_phrases` already proved every one of its tokens is
/// present in some recorded skill's tokens — the same gate that licensed
/// it in the first place, so re-checking it here as a subset (score `1.0`)
/// rather than a fraction keeps this module's `grounded` calls consistent
/// with what `tailor::assemble` actually allowed onto the page. A skill
/// name that isn't a full subset of any recorded skill or alias falls back
/// to the same fractional overlap the other line types use.
fn classify_skill(name: &str, index: usize, dataset: &ResumeDataset) -> LineProvenance {
    let location = LineLocation::Skill { index };
    let line_key = keyword_key(name);

    let mut best: Option<(SkillId, f64, String)> = None;
    for skill in &dataset.skills.skills {
        let candidate_names = std::iter::once(skill.canonical_name.as_str())
            .chain(skill.aliases.iter().map(String::as_str));
        for candidate_name in candidate_names {
            if normalize_whitespace(name) == normalize_whitespace(candidate_name) {
                return LineProvenance {
                    location,
                    text: name.to_string(),
                    status: ProvenanceStatus::Verbatim,
                    best_match: Some(SourceMatch {
                        source: SourceRef::Skill {
                            id: skill.id.clone(),
                        },
                        score: 1.0,
                    }),
                };
            }
            let candidate_key = keyword_key(candidate_name);
            let score = if is_token_subset(&line_key, &candidate_key) {
                1.0
            } else {
                overlap_score(name, candidate_name)
            };
            if best
                .as_ref()
                .is_none_or(|(_, best_score, _)| score > *best_score)
            {
                best = Some((skill.id.clone(), score, candidate_name.to_string()));
            }
        }
    }

    match best {
        Some((id, score, candidate_name)) => LineProvenance {
            location,
            text: name.to_string(),
            status: status_for_score(score, name, &candidate_name),
            best_match: Some(SourceMatch {
                source: SourceRef::Skill { id },
                score,
            }),
        },
        None => LineProvenance {
            location,
            text: name.to_string(),
            status: ProvenanceStatus::Unrecorded,
            best_match: None,
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{
        Contact, EmploymentType, EvidenceRef, Metric, Proficiency, Role, Skill, SkillCategory,
        SkillId, Strength, YearMonth,
    };
    use crate::tailor::{BuildId, JdId, SkillsSection, TailoredRole};
    use chrono::Utc;

    fn bullet(id: &str, text: &str, metric: Option<&str>) -> Bullet {
        Bullet {
            id: BulletId(id.into()),
            text: text.into(),
            skill_ids: Vec::new(),
            metric: metric.map(|m| Metric(m.into())),
            theme: Vec::new(),
            strength: Strength::Medium,
            variants: Vec::new(),
        }
    }

    fn dataset_with_one_role() -> ResumeDataset {
        let mut dataset = ResumeDataset::new(Contact {
            full_name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        dataset.roles.push(Role {
            id: RoleId("role-1".into()),
            company: "Analytical Engines Ltd".into(),
            title: "Director of Engineering".into(),
            start: YearMonth {
                year: 2020,
                month: 3,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: vec![
                bullet(
                    "bullet-1",
                    "Led a team of 12 engineers across 3 squads",
                    None,
                ),
                bullet(
                    "bullet-2",
                    "Cut deploy time from 45 minutes to 8",
                    Some("40% fewer breaks"),
                ),
            ],
            skill_ids: Vec::new(),
            context: None,
        });
        dataset.skills.skills.push(Skill {
            id: SkillId("skill-1".into()),
            canonical_name: "Engineering management".into(),
            aliases: vec!["EM".into()],
            category: SkillCategory::Soft,
            proficiency: Proficiency::Proficient,
            years: None,
            last_used: None,
            evidence: vec![EvidenceRef::Role(RoleId("role-1".into()))],
            verified: false,
            verified_at: None,
        });
        dataset
    }

    fn resume_with(role: TailoredRole, summary: &str, skills: Vec<String>) -> TailoredResume {
        TailoredResume {
            build_id: BuildId("b1".into()),
            jd_id: JdId("jd1".into()),
            generated_at: Utc::now(),
            contact: Contact {
                full_name: "Ada Lovelace".into(),
                email: "ada@example.com".into(),
                phone: None,
                location: None,
                links: Vec::new(),
            },
            target_title: None,
            summary: summary.into(),
            roles: vec![role],
            education: Vec::new(),
            skills_section: SkillsSection { skills },
            projects: Vec::new(),
            achievements: Vec::new(),
            certifications: Vec::new(),
        }
    }

    fn role(bullets: Vec<TailoredBullet>) -> TailoredRole {
        TailoredRole {
            id: RoleId("role-1".into()),
            company: "Analytical Engines Ltd".into(),
            title: "Director of Engineering".into(),
            start: YearMonth {
                year: 2020,
                month: 3,
            },
            end: None,
            location: None,
            bullets,
        }
    }

    #[test]
    fn an_untouched_bullet_is_verbatim() {
        let dataset = dataset_with_one_role();
        let draft = resume_with(
            role(vec![TailoredBullet {
                source_id: BulletId("bullet-1".into()),
                text: "Led a team of 12 engineers across 3 squads".into(),
            }]),
            "irrelevant",
            vec![],
        );

        let report = check_provenance(&draft, &dataset);
        let line = &report.lines[0];
        assert_eq!(line.status, ProvenanceStatus::Verbatim);
        assert_eq!(
            line.best_match,
            Some(SourceMatch {
                source: SourceRef::Bullet {
                    id: BulletId("bullet-1".into())
                },
                score: 1.0,
            })
        );
    }

    #[test]
    fn a_jd_mirrored_rewrite_that_folds_in_the_metric_is_grounded() {
        let dataset = dataset_with_one_role();
        // Reworded to mirror a JD phrase, and folds in bullet-2's metric
        // number ("40%") the way tailoring is instructed to. Most content
        // words survive, so this should clear the grounded threshold.
        let draft = resume_with(
            role(vec![TailoredBullet {
                source_id: BulletId("bullet-2".into()),
                text: "Drove engineering excellence, cutting deploy time from 45 minutes to 8, a 40% reduction in breaks".into(),
            }]),
            "irrelevant",
            vec![],
        );

        let report = check_provenance(&draft, &dataset);
        let line = &report.lines[0];
        assert_eq!(line.status, ProvenanceStatus::Grounded);
        let best = line.best_match.as_ref().unwrap();
        assert_eq!(
            best.source,
            SourceRef::Bullet {
                id: BulletId("bullet-2".into())
            }
        );
        assert!(best.score >= GROUNDED_THRESHOLD, "got {}", best.score);
        assert!(best.score < 1.0, "should not be exact: {}", best.score);
    }

    #[test]
    fn a_digit_preserving_reword_stays_grounded() {
        // The digit guard's positive case: the wording changes (verb, order)
        // but every number carries over unchanged, so the guard must not
        // demote an honest rewrite.
        let dataset = dataset_with_one_role();
        let draft = resume_with(
            role(vec![TailoredBullet {
                source_id: BulletId("bullet-1".into()),
                text: "Led a team of 12 engineers across 3 units".into(),
            }]),
            "irrelevant",
            vec![],
        );

        let report = check_provenance(&draft, &dataset);
        let line = &report.lines[0];
        assert_eq!(line.status, ProvenanceStatus::Grounded);
        assert_ne!(
            line.text, "Led a team of 12 engineers across 3 squads",
            "must be a reword, not the verbatim source text"
        );
    }

    #[test]
    fn a_digit_swapped_rewrite_is_demoted_to_unrecorded() {
        // Same wording as the grounded, metric-folding rewrite above, but
        // the deploy-time figure is swapped (8 -> 3). The words
        // still clear the token-overlap bar, but "3" is not a number
        // bullet-2's text or its metric ever states — a changed figure must
        // not read as `grounded`, even though the surrounding words match.
        let dataset = dataset_with_one_role();
        let draft = resume_with(
            role(vec![TailoredBullet {
                source_id: BulletId("bullet-2".into()),
                text: "Cut deploy time from 45 minutes to 3".into(),
            }]),
            "irrelevant",
            vec![],
        );

        let report = check_provenance(&draft, &dataset);
        let line = &report.lines[0];
        assert_eq!(
            line.status,
            ProvenanceStatus::Unrecorded,
            "an unbacked digit swap must demote even with high word overlap"
        );
        // Still the closest match by word overlap — the digit guard changes
        // the verdict, not the informational best-match pointer the UI uses.
        let best = line.best_match.as_ref().unwrap();
        assert_eq!(
            best.source,
            SourceRef::Bullet {
                id: BulletId("bullet-2".into())
            }
        );
        assert!(best.score >= GROUNDED_THRESHOLD, "got {}", best.score);
    }

    #[test]
    fn a_hand_typed_bullet_with_no_plausible_source_is_unrecorded() {
        let dataset = dataset_with_one_role();
        let draft = resume_with(
            role(vec![TailoredBullet {
                source_id: BulletId("bullet-1".into()),
                text: "Volunteered as a weekend chess tournament organizer".into(),
            }]),
            "irrelevant",
            vec![],
        );

        let report = check_provenance(&draft, &dataset);
        let line = &report.lines[0];
        assert_eq!(line.status, ProvenanceStatus::Unrecorded);
        // Still informational: some best guess is reported even though it
        // isn't close enough to call grounded.
        assert!(line.best_match.is_some());
        assert!(line.best_match.as_ref().unwrap().score < GROUNDED_THRESHOLD);
    }

    #[test]
    fn an_empty_dataset_leaves_every_line_unrecorded_with_no_best_match() {
        let dataset = ResumeDataset::new(Contact {
            full_name: "Nobody Yet".into(),
            email: "n@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        let draft = resume_with(
            TailoredRole {
                id: RoleId("role-1".into()),
                company: "Somewhere".into(),
                title: "Engineer".into(),
                start: YearMonth {
                    year: 2020,
                    month: 1,
                },
                end: None,
                location: None,
                bullets: vec![TailoredBullet {
                    source_id: BulletId("bullet-1".into()),
                    text: "Did some work".into(),
                }],
            },
            "A summary with no recorded counterpart",
            vec!["Rust".into()],
        );

        let report = check_provenance(&draft, &dataset);
        assert_eq!(report.lines.len(), 3); // 1 bullet + summary + 1 skill
        for line in &report.lines {
            assert_eq!(line.status, ProvenanceStatus::Unrecorded, "{line:?}");
            assert!(line.best_match.is_none(), "{line:?}");
        }
    }

    #[test]
    fn a_summary_only_draft_matches_the_confirmed_dataset_summary_verbatim() {
        let mut dataset = dataset_with_one_role();
        dataset.summary = Some("Engineering leader with a delivery focus.".into());
        dataset.summary_confirmed = true;
        let draft = resume_with(
            role(vec![]),
            "Engineering leader with a delivery focus.",
            vec![],
        );

        let report = check_provenance(&draft, &dataset);
        let summary_line = report
            .lines
            .iter()
            .find(|l| l.location == LineLocation::Summary)
            .unwrap();
        assert_eq!(summary_line.status, ProvenanceStatus::Verbatim);
        assert_eq!(
            summary_line.best_match,
            Some(SourceMatch {
                source: SourceRef::Summary,
                score: 1.0,
            })
        );
    }

    #[test]
    fn skill_lines_cover_verbatim_a_mirrored_variant_and_unrecorded() {
        let dataset = dataset_with_one_role();
        let draft = resume_with(
            role(vec![]),
            "irrelevant",
            vec![
                "Engineering management".into(), // verbatim canonical name
                "managing engineering".into(),   // word-order mirror, subset of tokens
                "Deep Sea Welding".into(),       // no plausible source
            ],
        );

        let report = check_provenance(&draft, &dataset);
        let skill_lines: Vec<&LineProvenance> = report
            .lines
            .iter()
            .filter(|l| matches!(l.location, LineLocation::Skill { .. }))
            .collect();
        assert_eq!(skill_lines.len(), 3);

        assert_eq!(skill_lines[0].status, ProvenanceStatus::Verbatim);
        assert_eq!(skill_lines[0].best_match.as_ref().unwrap().score, 1.0);

        assert_eq!(skill_lines[1].status, ProvenanceStatus::Grounded);
        assert_eq!(
            skill_lines[1].best_match.as_ref().unwrap().source,
            SourceRef::Skill {
                id: SkillId("skill-1".into())
            }
        );

        assert_eq!(skill_lines[2].status, ProvenanceStatus::Unrecorded);
    }
}
