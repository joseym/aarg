//! Voice-anchored rewriting (FR-3.6). The tailored draft can read like
//! generic AI prose — "leveraging synergies to spearhead..." — even when
//! every fact is the user's. This pass rewrites the lines that smell like
//! that, steering them *toward* the user's captured writing samples and
//! *away* from a deny-list of LLM clichés.
//!
//! Two hard boundaries keep it honest, matching the PRD:
//!
//! - It changes phrasing, never facts. Every rewrite passes the same
//!   `digit_runs` guard tailoring uses: a rewrite that introduces a
//!   number the source line didn't have is reverted. (Non-numeric
//!   inflation is held by the prompt, the same bar as a tailoring
//!   rewrite.)
//! - It makes **no AI-detection claims**. Detectors are unreliable and
//!   self-grading would be circular; the job here is voice *fidelity*,
//!   which is checkable against the samples — not "undetectability".
//!
//! Only `summary` and bullet lines are touched; the skills section and
//! every structural field are left exactly as tailoring produced them.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, ModelTier};
use crate::llm::{LlmError, TokenUsage};
use crate::tailor::{TailoredResume, digit_runs};

/// LLM tells worth steering away from. Word/phrase level only — the
/// structural tics (tricolon stacks, em-dash chains, uniform rhythm) are
/// the rewrite prompt's job, not a substring scan's.
const DENY_LIST: &[&str] = &[
    "leverage",
    "leveraging",
    "spearhead",
    "synergy",
    "synergies",
    "results-driven",
    "passionate about",
    "track record of",
    "proven track record",
    "best-in-class",
    "world-class",
    "cutting-edge",
    "game-chang",
    "move the needle",
    "wheelhouse",
    "circle back",
    "low-hanging fruit",
    "think outside the box",
    "deep dive",
    "delve",
    "tapestry",
    "in today's fast-paced",
    "seamless",
    "robust solution",
    "utilize",
];

/// One line of the draft, by id (`"summary"` or a bullet's source id).
/// Used both for the lines sent to the agent and the rewrites it returns.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Line {
    pub id: String,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the voice rewriter's reply was not the expected JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },
}

/// What a voice pass did. `reverted` counts rewrites dropped for drifting
/// from the facts — a number that should never be nonzero if the model
/// behaved, but the guard is there regardless.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct VoiceStats {
    pub rewritten: usize,
    pub reverted: usize,
    pub usage: TokenUsage,
}

/// What the agent needs: the flagged lines and the user's samples.
#[derive(Serialize)]
pub struct VoiceRewriteInput {
    pub lines: Vec<Line>,
    pub samples: Vec<String>,
}

/// Rewrites flagged lines toward the user's voice.
pub struct VoiceRewriteAgent;

#[async_trait]
impl Agent for VoiceRewriteAgent {
    type Input = VoiceRewriteInput;
    type Wire = RawRewrites;
    type Output = Vec<Line>;
    type Error = VoiceError;

    fn id(&self) -> &'static str {
        "voice_rewrite_v1"
    }
    fn model_tier(&self) -> ModelTier {
        // Matching the user's register while strengthening wording without
        // inflating the claim is a fine-grained writing task — smart tier.
        ModelTier::Smart
    }
    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        2048
    }
    fn user_message(&self, input: &VoiceRewriteInput) -> String {
        let mut text = String::from("The candidate's own writing, for voice:\n");
        if input.samples.is_empty() {
            text.push_str("(none provided)\n");
        } else {
            for (i, sample) in input.samples.iter().enumerate() {
                text.push_str(&format!("[sample {}] {}\n", i + 1, sample));
            }
        }
        text.push_str("\nRewrite each of these lines in that voice, keeping every fact:\n");
        for line in &input.lines {
            text.push_str(&format!("[{}] {}\n", line.id, line.text));
        }
        text.push_str("\nReturn one rewrite per line, keyed by the same id.");
        text
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> VoiceError {
        VoiceError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawRewrites,
        _input: VoiceRewriteInput,
    ) -> Result<Vec<Line>, VoiceError> {
        Ok(wire
            .rewrites
            .into_iter()
            .map(|r| Line {
                id: r.id,
                text: r.text,
            })
            .collect())
    }
}

const SYSTEM_PROMPT: &str = r#"You rewrite resume lines so they read like the candidate wrote them — the strongest version of THEIR writing, not generic AI output. You are given samples of the candidate's own writing and a set of lines to rewrite.

Do:
- Match the cadence, directness, and word choice of the samples. Plain and specific over polished and vague.
- Tighten for impact, in their voice: lead with the action and any result the line already states, replace vague verbs with precise, concrete ones ("built", "shipped", "cut" instead of "worked on things", "did", "was involved in"), and cut throat-clearing and filler ("In my role I was tasked with..."). Make their writing the strongest version of itself.
- Never escalate their role or scope to sound stronger. The degree of ownership stays exactly what the source states: "helped with" stays a contribution, it does not become "led" or "owned"; "contributed to" does not become "drove". Strengthen the wording, never the claim.
- Strip LLM tells: "leveraging", "spearheaded", "synergies", "results-driven", tricolon stacking ("X, Y, and Z" piled on), em-dashes ("—") of any kind (rewrite with a comma, "and", or a second sentence), and the eerily uniform sentence rhythm that gives AI away. Vary it the way a person does.
- Keep EVERY fact exactly: numbers, names, employers, dates, skills, scope. You are changing how it sounds, never what it claims. If a line has no number, do not add one.

Do not:
- Reach for inflated, buzzword-y, or generic resume-speak to sound impressive — a crisp line in the candidate's own register beats an impressive-sounding one they would never say and could not defend in an interview.
- Invent, inflate, or round any metric or achievement.
- Make any claim about AI detection or "undetectability" — that is not your job and the claim would be false.

Reply with exactly one JSON object and nothing else — no markdown fences:
{"rewrites": [{"id": "summary", "text": "..."}, {"id": "bullet-3", "text": "..."}]}"#;

#[derive(Debug, Deserialize)]
pub struct RawRewrites {
    #[serde(default)]
    rewrites: Vec<RawLine>,
}

#[derive(Debug, Deserialize)]
struct RawLine {
    #[serde(default)]
    id: String,
    #[serde(default)]
    text: String,
}

const SUMMARY_ID: &str = "summary";

/// The draft lines that need a voice pass: the summary and any bullet
/// that either trips the cliché deny-list *or* reads raw — a verbatim,
/// un-bullet-like line that slipped onto the page (a floor-topped-up
/// source bullet, or a raw enrichment answer). Deterministic, so a
/// keyless caller still knows whether a rewrite pass is worth a model
/// call.
pub fn flagged_lines(resume: &TailoredResume) -> Vec<Line> {
    let mut lines = Vec::new();
    if needs_rewrite(&resume.summary) {
        lines.push(Line {
            id: SUMMARY_ID.to_string(),
            text: resume.summary.clone(),
        });
    }
    for role in &resume.roles {
        for bullet in &role.bullets {
            if needs_rewrite(&bullet.text) {
                lines.push(Line {
                    id: bullet.source_id.0.clone(),
                    text: bullet.text.clone(),
                });
            }
        }
    }
    lines
}

fn needs_rewrite(text: &str) -> bool {
    has_cliche(text) || looks_raw(text)
}

fn has_cliche(text: &str) -> bool {
    let lower = text.to_lowercase();
    DENY_LIST.iter().any(|tell| lower.contains(tell))
}

/// A line that reads like a raw note rather than a polished résumé bullet:
/// it starts lowercase, or opens with a first-person subject ("I", "we",
/// "my", "our"). A real bullet leads with a capitalized action verb. This
/// is what catches a verbatim line the model never reworded.
fn looks_raw(text: &str) -> bool {
    let trimmed = text.trim_start();
    let starts_lowercase = trimmed
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() && c.is_lowercase());
    let lower = trimmed.to_lowercase();
    let first_person = ["i ", "we ", "my ", "our "]
        .iter()
        .any(|opener| lower.starts_with(opener));
    starts_lowercase || first_person
}

/// Run a voice pass over a draft: flag the AI-sounding lines, rewrite them
/// against the samples, and fold back only the rewrites that keep the
/// facts (the `digit_runs` guard). Returns the new draft and what changed.
/// A draft with nothing flagged costs no model call.
pub async fn rewrite_to_voice(
    ctx: &crate::agent::AgentContext<'_>,
    resume: &TailoredResume,
    samples: &[String],
) -> Result<(TailoredResume, VoiceStats), VoiceError> {
    let flagged = flagged_lines(resume);
    if flagged.is_empty() {
        return Ok((resume.clone(), VoiceStats::default()));
    }

    let run = VoiceRewriteAgent
        .run(
            ctx,
            VoiceRewriteInput {
                lines: flagged,
                samples: samples.to_vec(),
            },
        )
        .await?;

    let mut out = resume.clone();
    let mut stats = VoiceStats {
        usage: run.usage,
        ..VoiceStats::default()
    };
    for rewrite in run.output {
        let Some(original) = line_text(&out, &rewrite.id) else {
            continue; // a line id the model invented
        };
        let new = rewrite.text.trim();
        if new.is_empty() || new == original.trim() {
            continue;
        }
        // The fact guard: a rewrite may drop a number, never add one.
        if !digit_runs(new).is_subset(&digit_runs(&original)) {
            stats.reverted += 1;
            continue;
        }
        set_line_text(&mut out, &rewrite.id, new.to_string());
        stats.rewritten += 1;
    }
    Ok((out, stats))
}

/// The current text of a line by id, if it exists in the draft.
fn line_text(resume: &TailoredResume, id: &str) -> Option<String> {
    if id == SUMMARY_ID {
        return Some(resume.summary.clone());
    }
    resume
        .roles
        .iter()
        .flat_map(|role| &role.bullets)
        .find(|bullet| bullet.source_id.0 == id)
        .map(|bullet| bullet.text.clone())
}

/// Replace a line's text in place.
fn set_line_text(resume: &mut TailoredResume, id: &str, text: String) {
    if id == SUMMARY_ID {
        resume.summary = text;
        return;
    }
    for role in &mut resume.roles {
        for bullet in &mut role.bullets {
            if bullet.source_id.0 == id {
                bullet.text = text;
                return;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::agent::AgentContext;
    use crate::dataset::types::{BulletId, Contact, RoleId, YearMonth};
    use crate::llm::MockLlmClient;
    use crate::tailor::{BuildId, JdId, SkillsSection, TailoredBullet, TailoredRole};
    use crate::trace::Tracer;

    fn draft(summary: &str, bullets: &[(&str, &str)]) -> TailoredResume {
        TailoredResume {
            build_id: BuildId("001".into()),
            jd_id: JdId("x".into()),
            generated_at: chrono::Utc::now(),
            contact: Contact {
                full_name: "Ada".into(),
                email: "ada@example.com".into(),
                phone: None,
                location: None,
                links: Vec::new(),
            },
            target_title: Some("Engineer".into()),
            summary: summary.into(),
            roles: vec![TailoredRole {
                id: RoleId("role-1".into()),
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
                        source_id: BulletId((*id).into()),
                        text: (*text).into(),
                    })
                    .collect(),
            }],
            education: Vec::new(),
            skills_section: SkillsSection { skills: Vec::new() },
            projects: Vec::new(),
            achievements: Vec::new(),
            certifications: Vec::new(),
        }
    }

    fn ctx<'a>(mock: &'a MockLlmClient) -> AgentContext<'a> {
        AgentContext {
            llm: mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        }
    }

    #[test]
    fn flagging_catches_cliches_in_summary_and_bullets() {
        let resume = draft(
            "Results-driven leader leveraging synergies.",
            &[
                ("bullet-1", "Spearheaded the migration"),
                ("bullet-2", "Built the settlement pipeline"),
            ],
        );
        let flagged = flagged_lines(&resume);
        let ids: Vec<&str> = flagged.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, vec!["summary", "bullet-1"]); // bullet-2 is clean
    }

    #[test]
    fn flagging_catches_raw_un_bullet_like_lines() {
        let resume = draft(
            "Built and scaled the team.", // clean summary
            &[
                ("bullet-1", "we had weekly check in calls with customers"), // lowercase start
                ("bullet-2", "I built the trading UI"),                      // first-person
                ("bullet-3", "Owned the release process"),                   // clean bullet
            ],
        );
        let flagged = flagged_lines(&resume);
        let ids: Vec<&str> = flagged.iter().map(|l| l.id.as_str()).collect();
        // The two raw lines are flagged; the proper bullet and clean
        // summary are left alone.
        assert_eq!(ids, vec!["bullet-1", "bullet-2"]);
    }

    #[tokio::test]
    async fn a_clean_draft_makes_no_model_call() {
        let resume = draft(
            "Built and shipped the thing.",
            &[("bullet-1", "Ran the team")],
        );
        let mock = MockLlmClient::default();

        let (out, stats) = rewrite_to_voice(&ctx(&mock), &resume, &["plain words".into()])
            .await
            .unwrap();

        assert_eq!(stats, VoiceStats::default());
        assert_eq!(out, resume);
        assert!(mock.requests().is_empty());
    }

    #[tokio::test]
    async fn a_rewrite_lands_but_an_invented_number_is_reverted() {
        let resume = draft(
            "Leveraging best-in-class delivery.",
            &[("bullet-1", "Spearheaded the rollout")],
        );
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"rewrites": [
                {"id": "summary", "text": "I ship reliable releases."},
                {"id": "bullet-1", "text": "Ran the rollout across 5 teams"}
            ]}"#,
        );

        let (out, stats) = rewrite_to_voice(&ctx(&mock), &resume, &["plain".into()])
            .await
            .unwrap();

        // The summary rewrite (no new number) is kept...
        assert_eq!(out.summary, "I ship reliable releases.");
        assert_eq!(stats.rewritten, 1);
        // ...but the bullet rewrite invented "5" — reverted to the original.
        assert_eq!(out.roles[0].bullets[0].text, "Spearheaded the rollout");
        assert_eq!(stats.reverted, 1);
    }

    #[tokio::test]
    async fn the_prompt_asks_for_impact_while_holding_the_guards() {
        let resume = draft(
            "Leveraging synergies.",
            &[("bullet-1", "Helped with the rollout")],
        );
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"rewrites": []}"#); // we only inspect the prompt sent
        rewrite_to_voice(&ctx(&mock), &resume, &["plain".into()])
            .await
            .unwrap();

        let requests = mock.requests();
        let system = requests[0].system.as_deref().unwrap();
        // It now asks to tighten for impact...
        assert!(system.contains("Tighten for impact"));
        // ...but strengthening is precision, never promotion: ownership
        // and scope are held exactly.
        assert!(system.contains("Never escalate their role or scope"));
        assert!(system.contains("Strengthen the wording, never the claim"));
        // ...and no other guard is dropped: facts held, anti-generic
        // boundary, and no detection claims.
        assert!(system.contains("Keep EVERY fact"));
        assert!(system.contains("generic resume-speak"));
        assert!(system.contains("could not defend in an interview"));
        assert!(system.contains("AI detection"));
    }
}
