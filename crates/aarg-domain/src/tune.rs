//! Conversational tuning of a finished tailored resume (the "fix it, in your
//! own words" surface). After the adversarial loop settles on a draft, the
//! reviewer can leave objections the autonomous loop is not allowed to act on:
//! rephrasing a line stronger than the truth allows is exactly the inflation
//! never-fabricate forbids. So the person drives instead. They ask, in plain
//! language, for a change ("drop the intern bullet", "make the summary more
//! conversational", "this line should lead with the migration"), and a router
//! maps that request onto a small set of *grounded* operations.
//!
//! The router authors no resume content. Every operation it dispatches to is
//! one of:
//!
//! - **removal** — pure deletion, which takes a claim off the page and adds
//!   none, so it needs no evidence check;
//! - **strengthening** — the existing evidence interview, where the new words
//!   are the user's own and a digit guard reverts any invented number;
//! - **tone** — the voice rewrite, which changes phrasing, never facts, and is
//!   re-reviewed before it is kept;
//! - **a captured metric** — the metric interview, whose number traces to the
//!   user.
//!
//! So "conversational" never means "free-text editor". It means natural
//! language dispatched to operations that each keep the three never-fabricate
//! guards. This module owns the pure draft-edit primitives (locating and
//! removing a bullet, outlining the draft) and the router that classifies a
//! request into one of those operations.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentContext, ModelTier};
use crate::dataset::types::BulletId;
use crate::llm::{LlmError, TokenUsage};
use crate::tailor::{TailoredBullet, TailoredResume};
use crate::user::{Answer, AskError, Question, UserHandle};
use crate::voice::{self, VoiceError};

/// How to style the session's notifications — the seam that keeps the terminal
/// styler out of this portable crate.
///
/// The tune session reports each change through `user.notify` using the CLI's
/// semantic vocabulary: a green "✓ removed…", a blue "ℹ nothing to change", a
/// dim before/after diff, a yellow "⚠ couldn't read that". Building those means
/// reaching for `owo-colors`, which a crate that must compile to `wasm32` can't
/// depend on — the same wall `metric`'s [`crate::metric::AnchorStyle`] hits. So
/// the domain builds the message text and defers the glyph-and-color to the
/// caller: the CLI passes its `style`-backed functions, tests and the wasm host
/// pass [`SessionStyle::PLAIN`] (identity). Same messages, no terminal
/// dependency crossing the crate line.
///
/// It's a set of `fn(&str) -> String` pointers rather than closures because
/// nothing here captures state — a plain function per tier is all the caller
/// needs to hand across the seam, and `Copy` lets it pass by value freely. Each
/// tier maps to one of `style`'s helpers, so the CLI output stays byte-identical
/// to before the extraction.
#[derive(Clone, Copy)]
pub struct SessionStyle {
    /// Dimmed detail — the quoted "was:" line and a "left as is" note.
    pub dim: fn(&str) -> String,
    /// Bold — the "now:" label over a reworded line.
    pub bold: fn(&str) -> String,
    /// Yellow warn (`⚠`) — a request that couldn't be read or applied.
    pub warn: fn(&str) -> String,
    /// Green done (`✓`) — a removal or a kept rewrite.
    pub done: fn(&str) -> String,
    /// Blue info (`ℹ`) — a tone pass that found nothing to change.
    pub info: fn(&str) -> String,
    /// Cyan suggest (`→`) — an unsupported request's explanation.
    pub suggest: fn(&str) -> String,
}

/// Identity styling: return the text unchanged. What every
/// [`SessionStyle::PLAIN`] tier points at — for tests and any host without a
/// terminal (the wasm build), where glyphs and color would be noise.
fn plain(s: &str) -> String {
    s.to_string()
}

impl SessionStyle {
    /// Every part passes through unchanged. The default for tests and for any
    /// host without a terminal (the wasm build).
    pub const PLAIN: SessionStyle = SessionStyle {
        dim: plain,
        bold: plain,
        warn: plain,
        done: plain,
        info: plain,
        suggest: plain,
    };
}

/// Where a bullet sits in a draft, with a snapshot of it. Returned by
/// [`locate_bullet`] so a caller can show the user exactly which line a
/// request resolved to before changing it, and describe the change after.
#[derive(Debug, Clone, PartialEq)]
pub struct BulletLocation {
    pub role_index: usize,
    pub bullet_index: usize,
    pub company: String,
    pub source_id: BulletId,
    pub text: String,
}

/// Find a bullet in `resume` by its `source_id` — the handle the router
/// returns once the user names a line to act on. Returns the first match in
/// presentation order (role order, then bullet order), or `None` when no
/// bullet carries that id.
pub fn locate_bullet(resume: &TailoredResume, id: &BulletId) -> Option<BulletLocation> {
    for (role_index, role) in resume.roles.iter().enumerate() {
        for (bullet_index, bullet) in role.bullets.iter().enumerate() {
            if &bullet.source_id == id {
                return Some(BulletLocation {
                    role_index,
                    bullet_index,
                    company: role.company.clone(),
                    source_id: bullet.source_id.clone(),
                    text: bullet.text.clone(),
                });
            }
        }
    }
    None
}

/// Remove a bullet from `resume` by `source_id`, returning the removed bullet
/// (so a caller can confirm or undo) or `None` when nothing matched.
///
/// Pure deletion: it takes a line off the page and adds nothing, so it carries
/// no fabrication risk and needs no evidence check — the worst a wrong removal
/// can do is drop a true line, which the user sees and can re-add. A role left
/// with no bullets is kept: work history has value in its own right, and an
/// empty role is the caller's presentation call, not this function's.
pub fn remove_bullet(resume: &mut TailoredResume, id: &BulletId) -> Option<TailoredBullet> {
    let location = locate_bullet(resume, id)?;
    let role = resume.roles.get_mut(location.role_index)?;
    if location.bullet_index < role.bullets.len() {
        Some(role.bullets.remove(location.bullet_index))
    } else {
        None
    }
}

/// A compact outline of the draft: each role with its bullets, every bullet
/// prefixed by the `source_id` handle a request resolves to. Used both to show
/// the user the current draft and to give the router the ids it returns. Plain
/// text, no glyphs, so it reads the same in a terminal and in a model prompt.
pub fn draft_outline(resume: &TailoredResume) -> String {
    let mut out = String::new();
    if !resume.summary.is_empty() {
        out.push_str("SUMMARY\n");
        out.push_str(&format!("  {}\n", resume.summary));
    }
    for role in &resume.roles {
        out.push_str(&format!("\n{} / {}\n", role.company, role.title));
        for bullet in &role.bullets {
            out.push_str(&format!("  [{}] {}\n", bullet.source_id.0, bullet.text));
        }
    }
    out
}

// ---------------------------------------------------------------------
// The router: classify a free-text request into a grounded operation
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum TuneError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the tune router's reply was not the expected JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },

    #[error(transparent)]
    Voice(#[from] VoiceError),

    #[error(transparent)]
    Ask(#[from] AskError),
}

/// A request resolved to one grounded operation. The router only ever produces
/// one of these, and none of them carries model-authored resume text — so a
/// tune request, whatever its words, can never introduce a claim. Removal
/// deletes; tone rephrases through the guarded voice rewrite; an unsupported
/// request changes nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum TuneIntent {
    /// Remove the located bullet. The snapshot rides along for the confirm
    /// prompt, so the user sees the exact line before it goes.
    Remove(BulletLocation),
    /// Rewrite these line ids toward `register` (an empty `ids` means the whole
    /// draft). Phrasing only — it runs through the `digit_runs`-guarded voice
    /// rewrite, so no fact can change.
    Tone { ids: Vec<String>, register: String },
    /// The request is not something tune can do: adding a new accomplishment,
    /// number, or skill, or changing an employer, title, or date. `note`
    /// explains why, for the user. New facts go through the guided interview,
    /// never a free-text edit here.
    Unsupported { note: String },
}

/// What applying an intent did — for the session to report, and to decide
/// whether the draft needs re-rendering and re-review.
#[derive(Debug, Clone, PartialEq)]
pub enum TuneOutcome {
    Removed {
        line: String,
    },
    Retoned {
        rewritten: usize,
        reverted: usize,
    },
    /// The user declined a confirm (e.g. a removal); nothing changed.
    Declined,
    /// An unsupported request; `note` is shown to the user.
    NothingToDo {
        note: String,
    },
}

impl TuneOutcome {
    /// Whether the draft actually changed, so the caller knows to re-render and
    /// re-review. A tone pass that reverted every rewrite (or changed nothing)
    /// did not.
    pub fn changed_draft(&self) -> bool {
        match self {
            TuneOutcome::Removed { .. } => true,
            TuneOutcome::Retoned { rewritten, .. } => *rewritten > 0,
            TuneOutcome::Declined | TuneOutcome::NothingToDo { .. } => false,
        }
    }
}

/// What the router sees: the draft outline (every bullet carrying its `[id]`)
/// and the user's free-text request.
#[derive(Serialize)]
pub struct TuneClassifyInput {
    pub outline: String,
    pub request: String,
}

/// Maps a free-text request onto one supported operation, naming the target by
/// id. It chooses an action; it never writes resume content.
pub struct TuneRouterAgent;

#[async_trait]
impl Agent for TuneRouterAgent {
    type Input = TuneClassifyInput;
    type Wire = RawIntent;
    type Output = RawIntent;
    type Error = TuneError;

    fn id(&self) -> &'static str {
        "tune_router_v1"
    }
    fn model_tier(&self) -> ModelTier {
        // Picking which operation a request maps to is structured
        // classification, not writing, so the cheap tier handles it.
        ModelTier::Cheap
    }
    fn system_prompt(&self) -> &str {
        ROUTER_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        256
    }
    fn user_message(&self, input: &TuneClassifyInput) -> String {
        format!(
            "The resume, with each bullet's [id]:\n{}\n\nThe user's request:\n{}\n",
            input.outline, input.request
        )
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> TuneError {
        TuneError::BadReply { snippet, source }
    }
    fn assemble(&self, wire: RawIntent, _input: TuneClassifyInput) -> Result<RawIntent, TuneError> {
        Ok(wire)
    }
}

const ROUTER_PROMPT: &str = r#"You are the router for a resume "tune" assistant. The user describes, in plain language, a change to their already-tailored resume. You decide which supported operation the request maps to and name the target by id. You NEVER write resume content yourself.

Supported operations:
- "remove": the user wants a specific bullet gone ("drop the intern line", "cut the bullet about the migration"). Set "bullet_id" to that bullet's [id] from the outline.
- "tone": the user wants wording to read differently WITHOUT changing any fact, more conversational, warmer, plainer, more direct ("make it more conversational", "the summary sounds stiff"). Set "line_ids" to the [id]s to adjust, or leave it empty for the whole resume, and set "register" to a short word for the target tone ("conversational", "warmer", "direct"). This only changes how lines read, never what they claim.
- "unsupported": anything else, ESPECIALLY adding a new accomplishment, number, metric, or skill, or changing an employer, title, or date. Briefly say why in "note". Adding new facts is done through a guided interview elsewhere, not here, so route those here as unsupported.

Use only [id]s that appear in the outline. If you are unsure which bullet a removal means, choose "unsupported" and ask the user to be more specific in the note rather than guessing at a line.

Reply with exactly one JSON object and nothing else, no markdown fences:
{"action": "remove" | "tone" | "unsupported", "bullet_id": "<id, for remove>", "line_ids": ["<id>", ...], "register": "<short word, for tone>", "note": "<why, for unsupported>"}"#;

#[derive(Debug, Default, Deserialize)]
pub struct RawIntent {
    #[serde(default)]
    action: String,
    #[serde(default)]
    bullet_id: String,
    #[serde(default)]
    line_ids: Vec<String>,
    #[serde(default)]
    register: String,
    #[serde(default)]
    note: String,
}

/// Default note when the router can't map a request, or returns "unsupported"
/// with no explanation of its own.
const UNSUPPORTED_NOTE: &str = "That isn't something I can change here. I can remove a bullet or adjust how the wording reads; adding new facts happens through the guided interview during a tailor run.";

/// Classify a free-text `request` against `resume`. Runs the router, then
/// validates its choice against the draft so a bad id can't reach an
/// operation: a removal must name a bullet that exists (else the request is
/// reported unsupported), and a tone request with no specific line falls back
/// to the whole draft. Returns the resolved intent and what the call cost.
pub async fn classify(
    ctx: &AgentContext<'_>,
    resume: &TailoredResume,
    request: &str,
) -> Result<(TuneIntent, TokenUsage), TuneError> {
    let run = TuneRouterAgent
        .run(
            ctx,
            TuneClassifyInput {
                outline: draft_outline(resume),
                request: request.to_string(),
            },
        )
        .await?;
    let raw = run.output;
    let intent = match raw.action.trim().to_lowercase().as_str() {
        "remove" => {
            let id = BulletId(raw.bullet_id.trim().to_string());
            match locate_bullet(resume, &id) {
                Some(location) => TuneIntent::Remove(location),
                None => TuneIntent::Unsupported {
                    note: "I couldn't tell which bullet you meant. Name the line a bit more \
                           specifically."
                        .to_string(),
                },
            }
        }
        "tone" => {
            let named: Vec<String> = raw
                .line_ids
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let ids = if named.is_empty() {
                voice::all_line_ids(resume)
            } else {
                named
            };
            let register = match raw.register.trim() {
                "" => "more conversational".to_string(),
                r => r.to_string(),
            };
            TuneIntent::Tone { ids, register }
        }
        _ => TuneIntent::Unsupported {
            note: match raw.note.trim() {
                "" => UNSUPPORTED_NOTE.to_string(),
                n => n.to_string(),
            },
        },
    };
    Ok((intent, run.usage))
}

/// Apply a resolved intent to the draft in place, returning what happened and
/// any tokens it cost. A removal is confirmed first, so the user sees the exact
/// line before it goes; a tone change runs the guarded voice rewrite, which
/// keeps every fact. Nothing here writes a model-authored claim: removal only
/// deletes, tone only rephrases, unsupported does nothing.
pub async fn apply(
    ctx: &AgentContext<'_>,
    resume: &mut TailoredResume,
    intent: TuneIntent,
    user: &dyn UserHandle,
    samples: &[String],
    style: SessionStyle,
) -> Result<(TuneOutcome, TokenUsage), TuneError> {
    match intent {
        TuneIntent::Remove(location) => {
            let ok = user
                .confirm(
                    &format!(
                        "remove this line from {}? \"{}\"",
                        location.company, location.text
                    ),
                    true,
                )
                .await?;
            if !ok {
                return Ok((TuneOutcome::Declined, TokenUsage::default()));
            }
            remove_bullet(resume, &location.source_id);
            Ok((
                TuneOutcome::Removed {
                    line: location.text,
                },
                TokenUsage::default(),
            ))
        }
        TuneIntent::Tone { ids, register } => {
            let (out, stats) =
                voice::rewrite_lines(ctx, resume, &ids, samples, Some(register)).await?;
            if stats.rewritten == 0 {
                // Nothing came back changed (the guard reverted any drift, or
                // the lines were already fine), so there's nothing to confirm.
                return Ok((
                    TuneOutcome::Retoned {
                        rewritten: 0,
                        reverted: stats.reverted,
                    },
                    stats.usage,
                ));
            }
            // A tone rewrite is a model rewrite of the user's prose. The prompt
            // and the digit guard already held the facts, but the person is the
            // final gate (the same leg the strengthen flow stands on): show what
            // changed, line by line, and keep it only if they say so. Declining
            // leaves the draft exactly as it was.
            show_tone_changes(user, resume, &out, &ids, style);
            let keep = user.confirm("keep these wording changes?", true).await?;
            if keep {
                *resume = out;
                Ok((
                    TuneOutcome::Retoned {
                        rewritten: stats.rewritten,
                        reverted: stats.reverted,
                    },
                    stats.usage,
                ))
            } else {
                Ok((TuneOutcome::Declined, stats.usage))
            }
        }
        TuneIntent::Unsupported { note } => {
            Ok((TuneOutcome::NothingToDo { note }, TokenUsage::default()))
        }
    }
}

/// The current text of a tunable line (the summary or a bullet) by id, used to
/// show a before/after when a tone rewrite changes a line.
fn current_line(resume: &TailoredResume, id: &str) -> Option<String> {
    if id == voice::SUMMARY_ID {
        return Some(resume.summary.clone());
    }
    locate_bullet(resume, &BulletId(id.to_string())).map(|location| location.text)
}

/// Show the user each line a tone rewrite changed, the old text then the new,
/// so they can judge the rewrite before keeping it. Only lines that actually
/// differ are shown.
fn show_tone_changes(
    user: &dyn UserHandle,
    before: &TailoredResume,
    after: &TailoredResume,
    ids: &[String],
    style: SessionStyle,
) {
    for id in ids {
        let (Some(old), Some(new)) = (current_line(before, id), current_line(after, id)) else {
            continue;
        };
        if old.trim() == new.trim() {
            continue;
        }
        user.notify(&format!(
            "  {} {}\n  {} {}",
            (style.dim)("was:"),
            (style.dim)(&old),
            (style.bold)("now:"),
            new
        ));
    }
}

/// Drive the conversational tuning loop over a finished draft. Offer the user a
/// chance to change it in plain words, then read requests, route and apply each
/// (reporting through `user`), until they finish. Returns whether the draft
/// changed (so the caller re-renders and re-scores) and the tokens it spent.
///
/// Shared by the inline pass at the end of a tailor run and the standalone
/// `tune` command, so both behave identically. Reporting goes through
/// `user.notify`, so a scripted run records it and the never-interactive path
/// never reaches here (callers gate on `is_interactive`). Every word that lands
/// is the user's own: the router only maps a request onto a removal or the
/// guarded voice rewrite, so the loop cannot introduce a claim. A no to the
/// offer, or a blank request, ends it; a request it can't read or apply warns
/// and the loop continues.
pub async fn run_session(
    ctx: &AgentContext<'_>,
    resume: &mut TailoredResume,
    user: &dyn UserHandle,
    samples: &[String],
    style: SessionStyle,
) -> (bool, TokenUsage) {
    let mut changed = false;
    let mut total = TokenUsage::default();
    let wants = user
        .confirm(
            "want to change anything in your own words? (remove a bullet, make a part read differently)",
            false,
        )
        .await
        .unwrap_or(false);
    if !wants {
        return (changed, total);
    }
    loop {
        let request = match user
            .ask(Question::Text {
                prompt: "what would you like to change? (blank to finish)".into(),
            })
            .await
        {
            Ok(Answer::Text(t)) if !t.trim().is_empty() => t.trim().to_string(),
            _ => break, // a blank or non-text answer, or a read error, ends it
        };

        let (intent, usage) = match classify(ctx, resume, &request).await {
            Ok(pair) => pair,
            Err(e) => {
                user.notify(&(style.warn)(&format!("couldn't read that request ({e})")));
                continue;
            }
        };
        accumulate(&mut total, usage);

        let (outcome, usage) = match apply(ctx, resume, intent, user, samples, style).await {
            Ok(pair) => pair,
            Err(e) => {
                user.notify(&(style.warn)(&format!("couldn't make that change ({e})")));
                continue;
            }
        };
        accumulate(&mut total, usage);

        report(user, &outcome, style);
        if outcome.changed_draft() {
            changed = true;
        }
    }
    (changed, total)
}

/// Add one call's token usage into a running total.
fn accumulate(total: &mut TokenUsage, other: TokenUsage) {
    total.input_tokens += other.input_tokens;
    total.output_tokens += other.output_tokens;
}

/// Report one applied outcome through the user handle.
fn report(user: &dyn UserHandle, outcome: &TuneOutcome, style: SessionStyle) {
    match outcome {
        TuneOutcome::Removed { line } => {
            user.notify(&(style.done)(&format!("removed: \"{line}\"")));
        }
        TuneOutcome::Retoned {
            rewritten,
            reverted,
        } => {
            if *rewritten > 0 {
                let note = if *reverted > 0 {
                    format!(" ({reverted} reverted for drifting from the facts)")
                } else {
                    String::new()
                };
                user.notify(&(style.done)(&format!("rewrote {rewritten} line(s){note}")));
            } else {
                user.notify(&(style.info)("nothing there needed changing"));
            }
        }
        TuneOutcome::Declined => user.notify(&(style.dim)("left as is")),
        TuneOutcome::NothingToDo { note } => user.notify(&(style.suggest)(note)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::agent::AgentContext;
    use crate::dataset::types::{Contact, RoleId, YearMonth};
    use crate::llm::MockLlmClient;
    use crate::tailor::{
        BuildId, JdId, SkillsSection, TailoredBullet, TailoredResume, TailoredRole,
    };
    use crate::trace::Tracer;
    use crate::user::ScriptedUser;

    fn ctx(mock: &MockLlmClient) -> AgentContext<'_> {
        AgentContext {
            llm: mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        }
    }

    fn draft() -> TailoredResume {
        TailoredResume {
            build_id: BuildId("001".into()),
            jd_id: JdId("acme".into()),
            generated_at: chrono::Utc::now(),
            contact: Contact {
                full_name: "Ada".into(),
                email: "ada@example.com".into(),
                phone: None,
                location: None,
                links: Vec::new(),
            },
            target_title: Some("Engineering Manager".into()),
            summary: "Engineering leader.".into(),
            roles: vec![
                TailoredRole {
                    id: RoleId("role-1".into()),
                    company: "Acme".into(),
                    title: "Engineer".into(),
                    start: YearMonth {
                        year: 2020,
                        month: 1,
                    },
                    end: None,
                    location: None,
                    bullets: vec![
                        TailoredBullet {
                            source_id: BulletId("bullet-1".into()),
                            text: "Helped with the platform".into(),
                        },
                        TailoredBullet {
                            source_id: BulletId("bullet-2".into()),
                            text: "Cut deploy time from 45 to 8 minutes".into(),
                        },
                    ],
                },
                TailoredRole {
                    id: RoleId("role-2".into()),
                    company: "Globex".into(),
                    title: "Intern".into(),
                    start: YearMonth {
                        year: 2018,
                        month: 6,
                    },
                    end: Some(YearMonth {
                        year: 2019,
                        month: 8,
                    }),
                    location: None,
                    bullets: vec![TailoredBullet {
                        source_id: BulletId("bullet-9".into()),
                        text: "Ran the intern mentoring program".into(),
                    }],
                },
            ],
            education: Vec::new(),
            skills_section: SkillsSection {
                skills: vec!["Rust".into()],
            },
            projects: Vec::new(),
            achievements: Vec::new(),
            certifications: Vec::new(),
        }
    }

    #[test]
    fn locate_finds_a_bullet_by_id_with_its_position_and_company() {
        let resume = draft();
        let found = locate_bullet(&resume, &BulletId("bullet-9".into())).unwrap();
        assert_eq!(found.role_index, 1);
        assert_eq!(found.bullet_index, 0);
        assert_eq!(found.company, "Globex");
        assert_eq!(found.text, "Ran the intern mentoring program");
        // An id no bullet carries resolves to nothing.
        assert!(locate_bullet(&resume, &BulletId("nope".into())).is_none());
    }

    #[test]
    fn remove_drops_the_named_bullet_and_returns_it_leaving_the_rest() {
        let mut resume = draft();
        let removed = remove_bullet(&mut resume, &BulletId("bullet-1".into())).unwrap();
        assert_eq!(removed.text, "Helped with the platform");
        // Acme keeps its other bullet; Globex is untouched.
        assert_eq!(resume.roles[0].bullets.len(), 1);
        assert_eq!(resume.roles[0].bullets[0].source_id.0, "bullet-2");
        assert_eq!(resume.roles[1].bullets.len(), 1);
        // The removed id no longer resolves.
        assert!(locate_bullet(&resume, &BulletId("bullet-1".into())).is_none());
    }

    #[test]
    fn removing_the_only_bullet_keeps_the_role_as_history() {
        let mut resume = draft();
        remove_bullet(&mut resume, &BulletId("bullet-9".into())).unwrap();
        // Globex stays on the page with no bullets; history is not erased.
        assert_eq!(resume.roles.len(), 2);
        assert!(resume.roles[1].bullets.is_empty());
    }

    #[test]
    fn removing_an_unknown_id_changes_nothing() {
        let mut resume = draft();
        let before = resume.clone();
        assert!(remove_bullet(&mut resume, &BulletId("ghost".into())).is_none());
        assert_eq!(resume, before);
    }

    #[test]
    fn the_outline_lists_the_summary_roles_and_bullet_handles() {
        let outline = draft_outline(&draft());
        assert!(outline.contains("SUMMARY"));
        assert!(outline.contains("Engineering leader."));
        assert!(outline.contains("Acme / Engineer"));
        assert!(outline.contains("[bullet-2] Cut deploy time from 45 to 8 minutes"));
        assert!(outline.contains("[bullet-9] Ran the intern mentoring program"));
    }

    #[tokio::test]
    async fn classify_routes_a_removal_to_the_named_bullet() {
        let resume = draft();
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"action": "remove", "bullet_id": "bullet-9"}"#);

        let (intent, _) = classify(&ctx(&mock), &resume, "drop the intern bullet")
            .await
            .unwrap();
        match intent {
            TuneIntent::Remove(location) => {
                assert_eq!(location.company, "Globex");
                assert_eq!(location.source_id.0, "bullet-9");
            }
            other => panic!("expected a removal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn classify_reports_unsupported_when_a_remove_id_does_not_exist() {
        let resume = draft();
        let mock = MockLlmClient::default();
        // The model named a line the draft doesn't carry — never act on it.
        mock.enqueue(r#"{"action": "remove", "bullet_id": "ghost"}"#);

        let (intent, _) = classify(&ctx(&mock), &resume, "remove the made up one")
            .await
            .unwrap();
        assert!(matches!(intent, TuneIntent::Unsupported { .. }));
    }

    #[tokio::test]
    async fn classify_tones_the_whole_draft_when_no_line_is_named() {
        let resume = draft();
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"action": "tone", "register": "conversational"}"#);

        let (intent, _) = classify(&ctx(&mock), &resume, "make it more conversational")
            .await
            .unwrap();
        match intent {
            TuneIntent::Tone { ids, register } => {
                assert_eq!(register, "conversational");
                // Empty line_ids falls back to every rewritable line.
                assert_eq!(ids, voice::all_line_ids(&resume));
            }
            other => panic!("expected a tone change, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn classify_tones_only_the_named_lines() {
        let resume = draft();
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"action": "tone", "line_ids": ["summary"], "register": "warmer"}"#);

        let (intent, _) = classify(&ctx(&mock), &resume, "the summary sounds stiff")
            .await
            .unwrap();
        match intent {
            TuneIntent::Tone { ids, register } => {
                assert_eq!(ids, vec!["summary"]);
                assert_eq!(register, "warmer");
            }
            other => panic!("expected a tone change, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn classify_marks_an_add_facts_request_unsupported() {
        let resume = draft();
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"action": "unsupported", "note": "Adding a new metric needs the guided interview."}"#,
        );

        let (intent, _) = classify(&ctx(&mock), &resume, "say I cut costs 40%")
            .await
            .unwrap();
        match intent {
            TuneIntent::Unsupported { note } => assert!(note.contains("guided interview")),
            other => panic!("expected unsupported, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_removes_a_bullet_once_confirmed() {
        let mut resume = draft();
        let location = locate_bullet(&resume, &BulletId("bullet-1".into())).unwrap();
        let mock = MockLlmClient::default();
        let user = ScriptedUser::new();
        user.confirm_with(true);

        let (outcome, _) = apply(
            &ctx(&mock),
            &mut resume,
            TuneIntent::Remove(location),
            &user,
            &[],
            SessionStyle::PLAIN,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, TuneOutcome::Removed { .. }));
        assert!(outcome.changed_draft());
        assert!(locate_bullet(&resume, &BulletId("bullet-1".into())).is_none());
        // No model call: removal is pure deletion.
        assert!(mock.requests().is_empty());
    }

    #[tokio::test]
    async fn apply_keeps_the_bullet_when_the_removal_is_declined() {
        let mut resume = draft();
        let before = resume.clone();
        let location = locate_bullet(&resume, &BulletId("bullet-1".into())).unwrap();
        let mock = MockLlmClient::default();
        let user = ScriptedUser::new();
        user.confirm_with(false);

        let (outcome, _) = apply(
            &ctx(&mock),
            &mut resume,
            TuneIntent::Remove(location),
            &user,
            &[],
            SessionStyle::PLAIN,
        )
        .await
        .unwrap();

        assert_eq!(outcome, TuneOutcome::Declined);
        assert!(!outcome.changed_draft());
        assert_eq!(resume, before);
    }

    #[tokio::test]
    async fn apply_tone_runs_the_guarded_rewrite_on_the_draft() {
        let mut resume = draft();
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"rewrites": [{"id": "summary", "text": "I lead engineering teams."}]}"#);
        let user = ScriptedUser::new();

        let intent = TuneIntent::Tone {
            ids: vec!["summary".to_string()],
            register: "conversational".to_string(),
        };
        let (outcome, _) = apply(
            &ctx(&mock),
            &mut resume,
            intent,
            &user,
            &[],
            SessionStyle::PLAIN,
        )
        .await
        .unwrap();

        assert_eq!(
            outcome,
            TuneOutcome::Retoned {
                rewritten: 1,
                reverted: 0
            }
        );
        assert!(outcome.changed_draft());
        assert_eq!(resume.summary, "I lead engineering teams.");
    }

    #[tokio::test]
    async fn apply_tone_reverts_when_the_user_declines_to_keep_it() {
        let mut resume = draft();
        let before = resume.clone();
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"rewrites": [{"id": "summary", "text": "I lead engineering teams."}]}"#);
        let user = ScriptedUser::new();
        user.confirm_with(false); // shown the change, the user says no

        let intent = TuneIntent::Tone {
            ids: vec!["summary".to_string()],
            register: "conversational".to_string(),
        };
        let (outcome, _) = apply(
            &ctx(&mock),
            &mut resume,
            intent,
            &user,
            &[],
            SessionStyle::PLAIN,
        )
        .await
        .unwrap();

        // Declined: the draft is exactly as it was, even though the model
        // produced a clean rewrite.
        assert_eq!(outcome, TuneOutcome::Declined);
        assert!(!outcome.changed_draft());
        assert_eq!(resume, before);
        // The before/after was shown so the user could judge it.
        assert!(user.notices().iter().any(|n| n.contains("was:")));
    }

    #[tokio::test]
    async fn run_session_applies_a_request_then_finishes_on_a_blank_line() {
        let mut resume = draft();
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"action": "remove", "bullet_id": "bullet-9"}"#);
        let user = ScriptedUser::new();
        user.confirm_with(true); // yes to the opening offer
        user.answer(Answer::Text("drop the intern bullet".into()));
        user.confirm_with(true); // yes to the removal confirm
        user.answer(Answer::Text("".into())); // a blank line ends the loop

        let (changed, _usage) =
            run_session(&ctx(&mock), &mut resume, &user, &[], SessionStyle::PLAIN).await;

        assert!(changed);
        assert!(locate_bullet(&resume, &BulletId("bullet-9".into())).is_none());
        assert!(user.notices().iter().any(|n| n.contains("removed")));
    }

    #[tokio::test]
    async fn run_session_does_nothing_when_the_offer_is_declined() {
        let mut resume = draft();
        let before = resume.clone();
        let mock = MockLlmClient::default();
        let user = ScriptedUser::new();
        user.confirm_with(false); // no to the offer

        let (changed, _usage) =
            run_session(&ctx(&mock), &mut resume, &user, &[], SessionStyle::PLAIN).await;

        assert!(!changed);
        assert_eq!(resume, before);
        // Declining the offer never reaches the router.
        assert!(mock.requests().is_empty());
    }
}
