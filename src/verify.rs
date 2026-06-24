//! Skill verification: the interviews that connect skills to evidence
//! (FR-3.1). Two flows reach the same "collect the evidence" point:
//!
//! - `verify_unbacked` — recorded skills with an empty `evidence` list
//!   sit in limbo, barred from tailoring by the never-fabricate rule. A
//!   per-skill interview backs one on "yes" and removes it on "no".
//! - `verify_keywords` — the JD keywords nothing in the dataset backs
//!   yet (unmatched skills plus ATS phrases), gathered by
//!   `unbacked_keywords`. The user ticks the ones they have in a triage
//!   checklist; checked keywords become skills, unchecked ones are
//!   remembered as declined. A "help me decide" pass lets the user talk
//!   an unchecked keyword over with the guide and pull it in if it turns
//!   out they have it. This turns "the job wants Data Engineering, you
//!   didn't record it" into recorded experience — but only if the person
//!   genuinely has it.
//!
//! A "yes" always means a real role plus, optionally, years and a
//! user-written sentence (words the user typed are the one source that
//! needs no further proof — they become a bullet). The interview never
//! invents experience; the person affirms it.
//!
//! Transactionality lives at the save boundary: these functions mutate
//! the in-memory dataset and return a summary; the caller saves once
//! through the store (lock + backup + atomic write) only on success.
//! An interview abandoned halfway — Esc, closed terminal — returns an
//! error and the file on disk never changes.

use chrono::Utc;

use crate::agent::{Agent, AgentContext};
use crate::dataset::types::{
    Bullet, EvidenceRef, Proficiency, ResumeDataset, Skill, SkillCategory, SkillId, Strength,
};
use crate::gap::GapReport;
use crate::guide::{GuideInput, GuideTurn, VerificationGuideAgent};
use crate::jd::JobRequirements;
use crate::keywords::keyword_key;
use crate::user::{Answer, AskError, Question, UserHandle};

/// How many guide exchanges before nudging the user back to the
/// question — a confused user shouldn't loop forever, and each exchange
/// is an LLM call.
const MAX_GUIDE_EXCHANGES: usize = 4;

/// Revise-loop cap when polishing a typed evidence sentence into resume
/// wording. Like strengthen's, it exists only to guarantee the loop ends,
/// so the PRD's 3 is plenty.
const EVIDENCE_REVISES: usize = 3;

/// What an interview session accomplished.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct VerifyOutcome {
    pub verified: usize,
    pub removed: usize,
    pub skipped: usize,
    pub bullets_added: usize,
    /// Unknown skills the user said they don't have — remembered so they
    /// aren't re-offered next time.
    pub declined: usize,
}

impl VerifyOutcome {
    /// Whether anything changed that is worth saving. A recorded decline
    /// counts: persisting it is the whole point of not re-asking.
    pub fn changed(&self) -> bool {
        self.verified > 0 || self.removed > 0 || self.declined > 0
    }
}

/// The user's verdict on one skill. `Have` carries the role they picked
/// and the optional details; the caller decides what to do with it
/// (back an existing skill, or create a new one).
enum Verdict {
    Have {
        role_index: usize,
        years: Option<f32>,
        sentence: Option<String>,
    },
    DontHave,
    Skip,
}

/// The shared per-skill interview: "do you have this, and where?".
/// `no_label` differs per flow ("remove it" vs "I haven't"), but the
/// questions and their order are identical.
// EXERCISE(EX-016)
async fn interview_skill(
    name: &str,
    no_label: &str,
    role_options: &[String],
    user: &dyn UserHandle,
    guide: Option<&AgentContext<'_>>,
) -> Result<Verdict, AskError> {
    // The question can re-appear: choosing "let me explain" runs a
    // guide conversation and then asks again, so the user can decide
    // with help. Every other answer returns.
    loop {
        let mut options = vec![
            "Yes, professionally".to_string(),
            "Yes, in a side project".to_string(),
            no_label.to_string(),
            "Skip for now".to_string(),
        ];
        if guide.is_some() {
            options.push("I'm not sure - let me explain".to_string());
        }

        let answer = user
            .ask(Question::Select {
                prompt: format!("Have you used {name} in any role?"),
                options,
            })
            .await?;

        match answer {
            Answer::Choice(0) | Answer::Choice(1) => {
                match collect_evidence(name, role_options, user, guide).await? {
                    Evidence::Provide {
                        role_index,
                        years,
                        sentence,
                    } => {
                        return Ok(Verdict::Have {
                            role_index,
                            years,
                            sentence,
                        });
                    }
                    // Backed out at the role step — treat as "not now",
                    // leaving the skill untouched to ask again later.
                    Evidence::Skip => return Ok(Verdict::Skip),
                }
            }
            Answer::Choice(2) => return Ok(Verdict::DontHave),
            Answer::Choice(3) => return Ok(Verdict::Skip),
            // The "let me explain" option — present only when a guide is
            // available. Run the conversation, then re-ask.
            _ => match guide {
                Some(ctx) => clarify(name, role_options, user, ctx).await?,
                None => return Ok(Verdict::Skip),
            },
        }
    }
}

/// The clarification conversation: the user asks or explains, an honest
/// guide responds, repeat until they're ready to answer. The guide
/// records nothing — it only helps the user decide. A guide that can't
/// be reached degrades to a note rather than failing the interview.
async fn clarify(
    name: &str,
    role_options: &[String],
    user: &dyn UserHandle,
    ctx: &AgentContext<'_>,
) -> Result<(), AskError> {
    let mut history: Vec<GuideTurn> = Vec::new();

    // Lead with a plain-language description, unprompted: the user's
    // first question is almost always "what even is this?", so answer it
    // before they have to ask. The opener seeds the conversation history
    // so follow-ups build on it. If the guide can't be reached, fall back
    // to a generic invitation and let the user drive.
    let opener = format!(
        "In plain terms, what does \"{name}\" usually mean, and what kind of real experience would count as having it?"
    );
    match VerificationGuideAgent
        .run(
            ctx,
            GuideInput {
                skill: name.to_string(),
                roles: role_options.to_vec(),
                history: Vec::new(),
                message: opener.clone(),
            },
        )
        .await
    {
        Ok(run) => {
            user.notify(&run.output);
            history.push(GuideTurn {
                from_user: true,
                text: opener,
            });
            history.push(GuideTurn {
                from_user: false,
                text: run.output,
            });
        }
        Err(_) => {
            user.notify(&format!(
                "Ask anything about \"{name}\": what it means, or describe what you did and I'll help you decide honestly."
            ));
        }
    }

    loop {
        let message = match user
            .ask(Question::Text {
                prompt: "ask a follow-up, or describe what you did (blank to go back)".to_string(),
            })
            .await?
        {
            Answer::Text(text) if !text.trim().is_empty() => text,
            _ => break,
        };

        let reply = match VerificationGuideAgent
            .run(
                ctx,
                GuideInput {
                    skill: name.to_string(),
                    roles: role_options.to_vec(),
                    history: history.clone(),
                    message: message.clone(),
                },
            )
            .await
        {
            Ok(run) => run.output,
            Err(_) => {
                user.notify("(couldn't reach the guide right now; answer as best you can)");
                break;
            }
        };
        user.notify(&reply);

        history.push(GuideTurn {
            from_user: true,
            text: message,
        });
        history.push(GuideTurn {
            from_user: false,
            text: reply,
        });
        if history.len() >= 2 * MAX_GUIDE_EXCHANGES {
            user.notify("Let's come back to the question.");
            break;
        }
        if !user.confirm("ask something else?", false).await? {
            break;
        }
    }
    Ok(())
}

/// What `collect_evidence` came back with.
enum Evidence {
    /// The user pointed the skill at a role (with optional years/sentence).
    Provide {
        role_index: usize,
        years: Option<f32>,
        sentence: Option<String>,
    },
    /// The user backed out — didn't recognize it, or realized they don't
    /// have it after all. Record nothing; it can be asked again.
    Skip,
}

/// The questions behind a "yes": which role best shows the skill,
/// roughly how many years, and an optional one-sentence description.
/// Shared by the per-skill interview and the keyword triage, which reach
/// the same point — "the user has this" — by different routes. When a
/// `guide` is present the role menu also offers "explain it" (so
/// clarification is reachable *while* populating, not only beforehand)
/// and a skip, for when the explanation reveals it isn't a match.
async fn collect_evidence(
    name: &str,
    role_options: &[String],
    user: &dyn UserHandle,
    guide: Option<&AgentContext<'_>>,
) -> Result<Evidence, AskError> {
    // The role question can re-appear: choosing "explain it" runs the
    // guide and then re-asks, so the user can pick a role with help.
    let role_index = loop {
        let mut options = role_options.to_vec();
        let explain_index = options.len();
        if guide.is_some() {
            options.push("I'm not sure what this is - explain it".to_string());
        }
        let skip_index = options.len();
        options.push("Skip this one".to_string());

        let choice = match user
            .ask(Question::Select {
                prompt: format!("Which role best demonstrates {name}?"),
                options,
            })
            .await?
        {
            Answer::Choice(index) => index,
            _ => 0,
        };

        if choice < role_options.len() {
            break choice;
        }
        if choice == skip_index {
            return Ok(Evidence::Skip);
        }
        if let (true, Some(ctx)) = (choice == explain_index, guide) {
            clarify(name, role_options, user, ctx).await?;
            continue;
        }
        // Any other out-of-range answer: default to the first role.
        break 0;
    };
    let years = match user
        .ask(Question::Text {
            prompt: "Roughly how many years? (blank to skip)".to_string(),
        })
        .await?
    {
        Answer::Text(text) => parse_years(&text),
        _ => None,
    };
    let sentence = match user
        .ask(Question::Text {
            prompt: format!("In a few words, what did you do with {name}? (blank to skip)"),
        })
        .await?
    {
        Answer::Text(text) => {
            let text = text.trim().to_string();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    };
    // Polish the user's plain words into resume wording when a guide is
    // present: they type facts concisely, an agent phrases them well, and
    // they accept, revise, or keep their own. The shared digit-guard rejects
    // a rewrite that invents a number, and their own words are the floor —
    // so this sharpens phrasing without ever inflating the claim. Keyless,
    // the sentence is recorded verbatim (no offer, nothing invented).
    let sentence = match (sentence, guide) {
        (Some(words), Some(ctx)) => {
            Some(crate::strengthen::polish(ctx, "", &words, user, EVIDENCE_REVISES).await?)
        }
        (other, _) => other,
    };
    Ok(Evidence::Provide {
        role_index,
        years,
        sentence,
    })
}

/// Attach evidence for a skill that already exists in the dataset: a
/// role reference, optional years, and (if the user wrote one) a
/// bullet. Used by both flows once a skill id is in hand.
fn attach_evidence(
    dataset: &mut ResumeDataset,
    skill_id: &SkillId,
    role_index: usize,
    years: Option<f32>,
    sentence: Option<String>,
    outcome: &mut VerifyOutcome,
) {
    let role_id = dataset.roles[role_index].id.clone();
    if let Some(text) = sentence {
        let bullet_id = dataset.next_bullet_id();
        dataset.roles[role_index].bullets.push(Bullet {
            id: bullet_id,
            text,
            skill_ids: vec![skill_id.clone()],
            metric: None,
            theme: Vec::new(),
            strength: Strength::Medium,
            variants: Vec::new(),
        });
        if !dataset.roles[role_index].skill_ids.contains(skill_id) {
            dataset.roles[role_index].skill_ids.push(skill_id.clone());
        }
        outcome.bullets_added += 1;
    }
    if let Some(skill) = dataset.skills.skills.iter_mut().find(|s| s.id == *skill_id) {
        skill.evidence.push(EvidenceRef::Role(role_id));
        skill.verified = true;
        skill.verified_at = Some(Utc::now());
        if years.is_some() {
            skill.years = years;
        }
    }
}

/// Interview the user about every skill with no evidence behind it.
/// `guide`, when present, offers an LLM clarification conversation on
/// any question the user is unsure about.
pub async fn verify_unbacked(
    dataset: &mut ResumeDataset,
    user: &dyn UserHandle,
    guide: Option<&AgentContext<'_>>,
) -> Result<VerifyOutcome, AskError> {
    let unbacked: Vec<SkillId> = dataset
        .skills
        .skills
        .iter()
        .filter(|skill| skill.evidence.is_empty())
        .map(|skill| skill.id.clone())
        .collect();

    let mut outcome = VerifyOutcome::default();
    if unbacked.is_empty() {
        user.notify("every recorded skill is already evidence-backed");
        return Ok(outcome);
    }
    if dataset.roles.is_empty() {
        user.notify("the dataset has no roles to attach evidence to; ingest a resume first");
        return Ok(outcome);
    }

    let role_options = role_options(dataset);

    for skill_id in unbacked {
        let Some(skill) = dataset.skills.skills.iter().find(|s| s.id == skill_id) else {
            continue; // removed earlier in this same session
        };
        let name = skill.canonical_name.clone();

        match interview_skill(
            &name,
            "No - remove it from the dataset",
            &role_options,
            user,
            guide,
        )
        .await?
        {
            Verdict::Have {
                role_index,
                years,
                sentence,
            } => {
                attach_evidence(
                    dataset,
                    &skill_id,
                    role_index,
                    years,
                    sentence,
                    &mut outcome,
                );
                user.notify(&format!("added evidence for {name}"));
                outcome.verified += 1;
            }
            Verdict::DontHave => {
                remove_skill(dataset, &skill_id);
                user.notify(&format!("removed {name}"));
                outcome.removed += 1;
            }
            Verdict::Skip => {
                outcome.skipped += 1;
            }
        }
    }

    Ok(outcome)
}

/// A JD keyword nothing in the dataset backs yet — a candidate for the
/// triage checklist. The category is what the skill is created with if
/// the user claims it.
pub struct KeywordCandidate {
    pub name: String,
    pub category: SkillCategory,
}

/// Every JD keyword the dataset can't yet support: the gap's unknown
/// skills, plus any ATS phrase that doesn't already resolve to a
/// recorded skill. Keywords the user has already declined are left out —
/// a settled "no" isn't a candidate — and near-duplicates are collapsed
/// on a normalized key (see `keyword_key`), so the JD listing a skill
/// and three rephrasings of it yields one checklist row. This is what
/// the triage checklist offers.
pub fn unbacked_keywords(
    dataset: &ResumeDataset,
    jd: &JobRequirements,
    gap: &GapReport,
) -> Vec<KeywordCandidate> {
    // Compare on a normalized key so near-duplicates collapse: "people
    // management" / "people manager", "engineering manager" / "sr
    // engineering manager" should be one row, not three. Declined
    // keywords are matched the same way, so declining "people manager"
    // also suppresses "people management".
    let declined: Vec<Vec<String>> = dataset
        .metadata
        .declined_skills
        .iter()
        .map(|d| keyword_key(d))
        .collect();
    let mut seen: Vec<Vec<String>> = Vec::new();
    let mut out: Vec<KeywordCandidate> = Vec::new();

    // The role's own title ("Senior Engineering Manager") is the most
    // common non-skill that ATS phrases smuggle in. It belongs in a
    // target-title headline, not the skills list, so never offer it (or
    // its seniority variants) as a verifiable skill.
    let title_key = keyword_key(&jd.title);

    // Concepts an evidence-backed skill already covers. A candidate whose
    // tokens are a subset of one of these is *already backed* (the mirror
    // surfaces its wording, ats.rs credits it), so offering it would only
    // mint a near-duplicate skill — exactly the bloat the tailor dedup
    // then has to clean up. Same token-subset test the mirror uses.
    let backed_keys: Vec<Vec<String>> = dataset
        .skills
        .skills
        .iter()
        .filter(|s| !s.evidence.is_empty())
        .map(|s| keyword_key(&s.canonical_name))
        .collect();

    let mut consider = |name: &str, category: SkillCategory| {
        let key = keyword_key(name);
        if key.is_empty() || key == title_key || declined.contains(&key) || seen.contains(&key) {
            return;
        }
        // Already covered by a recorded skill's broader wording → not a gap.
        if backed_keys
            .iter()
            .any(|bk| key.iter().all(|t| bk.contains(t)))
        {
            return;
        }
        seen.push(key);
        out.push(KeywordCandidate {
            name: name.to_string(),
            category,
        });
    };

    // Unmatched JD skills carry their own category.
    for skill in &gap.unknown {
        consider(&skill.name, skill.category);
    }
    // ATS phrases are keywords too. Skip any that a recorded skill
    // already covers (by alias or canonical name) — that's backed, not a
    // gap. A bare phrase has no category, so call it domain knowledge.
    for phrase in &jd.ats_phrases {
        let backed = dataset.skills.aliases.contains_key(&phrase.to_lowercase())
            || dataset
                .skills
                .skills
                .iter()
                .any(|s| s.canonical_name.eq_ignore_ascii_case(phrase));
        if !backed {
            consider(phrase, SkillCategory::Domain);
        }
    }
    out
}

/// Triage the unbacked JD keywords: the user checks the ones they
/// genuinely have, and only those get the evidence interview. A checked
/// keyword becomes a recorded skill backed by a real role (and optional
/// user-written sentence); an unchecked one is remembered as declined so
/// it isn't offered again. Like the per-skill interview, this invents
/// nothing — the user affirms each claim and points it at a role.
///
/// `guide`, when present, adds a "help me decide" pass after the
/// checklist: the user can talk through any keyword they left unchecked
/// (a checklist has no room for a per-row "what is this?"), and pull one
/// in if the conversation reveals they do have it.
pub async fn verify_keywords(
    dataset: &mut ResumeDataset,
    candidates: &[KeywordCandidate],
    user: &dyn UserHandle,
    guide: Option<&AgentContext<'_>>,
) -> Result<VerifyOutcome, AskError> {
    let mut outcome = VerifyOutcome::default();
    if candidates.is_empty() {
        return Ok(outcome);
    }
    if dataset.roles.is_empty() {
        user.notify("the dataset has no roles to attach evidence to; ingest a resume first");
        return Ok(outcome);
    }

    let options: Vec<String> = candidates.iter().map(|c| c.name.clone()).collect();
    let checked = match user
        .ask(Question::MultiSelect {
            prompt:
                "Check the job keywords you genuinely have (you can talk through the rest next)"
                    .to_string(),
            options,
        })
        .await?
    {
        Answer::Choices(indexes) => indexes,
        _ => Vec::new(),
    };

    // role_options is owned, so the evidence interview can borrow it
    // while the loop also mutates the dataset to add skills. `recorded`
    // tracks which candidates became skills (checked, or rescued by the
    // help pass).
    let role_options = role_options(dataset);
    let mut recorded = vec![false; candidates.len()];
    for (index, candidate) in candidates.iter().enumerate() {
        if checked.contains(&index) {
            // A checked keyword can still back out at the role step (the
            // "explain it" path may reveal it isn't a match); only a real
            // role makes it `recorded`.
            recorded[index] =
                record_keyword(dataset, candidate, &role_options, user, guide, &mut outcome)
                    .await?;
        }
    }

    // The "help me decide" pass over the keywords left unchecked; it
    // returns the indexes the user rescued, which we mark recorded.
    if let Some(ctx) = guide {
        let pending: Vec<usize> = (0..candidates.len())
            .filter(|i| !checked.contains(i))
            .collect();
        let rescued = clarify_unchecked(
            dataset,
            candidates,
            pending,
            &role_options,
            ctx,
            user,
            &mut outcome,
        )
        .await?;
        for index in rescued {
            recorded[index] = true;
        }
    }

    // An unchecked keyword the user never rescued is a "no" worth
    // remembering. A *checked* one that backed out at the role step is
    // left alone — they engaged with it, so don't bury it as declined.
    for (index, candidate) in candidates.iter().enumerate() {
        if !recorded[index] && !checked.contains(&index) {
            decline_keyword(dataset, candidate, &mut outcome);
        }
    }

    Ok(outcome)
}

/// Run the evidence interview for one claimed keyword. Returns `true`
/// if it became a recorded skill, `false` if the user backed out at the
/// role step. Shared by the checklist pass and the help pass.
async fn record_keyword(
    dataset: &mut ResumeDataset,
    candidate: &KeywordCandidate,
    role_options: &[String],
    user: &dyn UserHandle,
    guide: Option<&AgentContext<'_>>,
    outcome: &mut VerifyOutcome,
) -> Result<bool, AskError> {
    match collect_evidence(&candidate.name, role_options, user, guide).await? {
        Evidence::Provide {
            role_index,
            years,
            sentence,
        } => {
            let skill_id = add_skill(dataset, &candidate.name, candidate.category);
            attach_evidence(dataset, &skill_id, role_index, years, sentence, outcome);
            user.notify(&format!("added {}", candidate.name));
            outcome.verified += 1;
            Ok(true)
        }
        Evidence::Skip => Ok(false),
    }
}

/// `aarg skills add <name>` and the tailor inline pivot: record one skill
/// the user names and back it with evidence in a single interview. If the
/// name already resolves to a recorded skill (by alias or canonical
/// spelling), that one gains the new evidence rather than a duplicate being
/// minted; otherwise a fresh skill is created under `category`. Like every
/// evidence flow here, the user points it at a real role and writes the
/// line — polished into resume wording when a guide is present, never
/// inflated — so nothing is invented. Backing out at the role step records
/// nothing.
pub async fn add_one_skill(
    dataset: &mut ResumeDataset,
    name: &str,
    category: SkillCategory,
    user: &dyn UserHandle,
    guide: Option<&AgentContext<'_>>,
) -> Result<VerifyOutcome, AskError> {
    let mut outcome = VerifyOutcome::default();
    if dataset.roles.is_empty() {
        user.notify("the dataset has no roles to attach evidence to; ingest a resume first");
        return Ok(outcome);
    }

    // Reuse an existing skill if the name already resolves — add evidence to
    // it, never a near-duplicate.
    let existing = dataset
        .skills
        .aliases
        .get(&name.to_lowercase())
        .cloned()
        .or_else(|| {
            dataset
                .skills
                .skills
                .iter()
                .find(|s| s.canonical_name.eq_ignore_ascii_case(name))
                .map(|s| s.id.clone())
        });
    if existing.is_some() {
        user.notify(&format!(
            "{name} is already recorded - adding evidence to it"
        ));
    }

    let role_options = role_options(dataset);
    match collect_evidence(name, &role_options, user, guide).await? {
        Evidence::Provide {
            role_index,
            years,
            sentence,
        } => {
            let skill_id = match existing {
                Some(id) => id,
                None => add_skill(dataset, name, category),
            };
            attach_evidence(
                dataset,
                &skill_id,
                role_index,
                years,
                sentence,
                &mut outcome,
            );
            user.notify(&format!("added evidence for {name}"));
            outcome.verified += 1;
        }
        Evidence::Skip => {}
    }
    Ok(outcome)
}

/// Remember a keyword the user didn't claim, so the checklist shrinks
/// instead of re-offering it every run.
fn decline_keyword(
    dataset: &mut ResumeDataset,
    candidate: &KeywordCandidate,
    outcome: &mut VerifyOutcome,
) {
    let key = candidate.name.to_lowercase();
    if !dataset.metadata.declined_skills.contains(&key) {
        dataset.metadata.declined_skills.push(key);
    }
    outcome.declined += 1;
}

/// The "help me decide" pass: let the user talk through any of the
/// `pending` (unchecked) keywords with the guide and pull one in if it
/// turns out they have it. Touches only those keywords, so confident
/// ticks are undisturbed. Returns the indexes the user rescued.
async fn clarify_unchecked(
    dataset: &mut ResumeDataset,
    candidates: &[KeywordCandidate],
    mut pending: Vec<usize>,
    role_options: &[String],
    ctx: &AgentContext<'_>,
    user: &dyn UserHandle,
    outcome: &mut VerifyOutcome,
) -> Result<Vec<usize>, AskError> {
    let mut rescued: Vec<usize> = Vec::new();
    if pending.is_empty() {
        return Ok(rescued);
    }
    if !user
        .confirm("talk through any of the ones you left unchecked?", false)
        .await?
    {
        return Ok(rescued);
    }

    loop {
        let mut options: Vec<String> = pending
            .iter()
            .map(|&i| candidates[i].name.clone())
            .collect();
        options.push("Done · set the rest aside".to_string());
        let pick = match user
            .ask(Question::Select {
                prompt: "Which one?".to_string(),
                options,
            })
            .await?
        {
            Answer::Choice(index) if index < pending.len() => index,
            _ => break, // "Done", or any out-of-range answer
        };

        // Take it out of the menu — we're handling it now, either way.
        let candidate_index = pending.remove(pick);
        clarify(&candidates[candidate_index].name, role_options, user, ctx).await?;
        if user
            .confirm(
                &format!("Add {} now?", candidates[candidate_index].name),
                false,
            )
            .await?
            && record_keyword(
                dataset,
                &candidates[candidate_index],
                role_options,
                user,
                Some(ctx),
                outcome,
            )
            .await?
        {
            rescued.push(candidate_index);
        }
        if pending.is_empty() {
            break;
        }
    }
    Ok(rescued)
}

/// "Title — Company" for each role, the menu the interview offers.
fn role_options(dataset: &ResumeDataset) -> Vec<String> {
    dataset
        .roles
        .iter()
        .map(|role| format!("{} · {}", role.title, role.company))
        .collect()
}

/// Create a fresh, verified skill (evidence attached separately). The
/// proficiency defaults to working — a sensible middle for something
/// the user just affirmed; it's a self-assessment, not a claim about
/// specific evidence.
fn add_skill(dataset: &mut ResumeDataset, name: &str, category: SkillCategory) -> SkillId {
    let id = next_skill_id(dataset);
    dataset.skills.skills.push(Skill {
        id: id.clone(),
        canonical_name: name.to_string(),
        aliases: Vec::new(),
        category,
        proficiency: Proficiency::Working,
        years: None,
        last_used: None,
        evidence: Vec::new(),
        verified: true,
        verified_at: Some(Utc::now()),
    });
    dataset
        .skills
        .aliases
        .insert(name.to_lowercase(), id.clone());
    id
}

/// New skill IDs continue the `skill-N` sequence.
fn next_skill_id(dataset: &ResumeDataset) -> SkillId {
    let highest = dataset
        .skills
        .skills
        .iter()
        .filter_map(|s| s.id.0.strip_prefix("skill-")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    SkillId(format!("skill-{}", highest + 1))
}

/// Pull a leading number out of free text: "2", "2.5 years", " 3 ".
fn parse_years(text: &str) -> Option<f32> {
    // split_whitespace already ignores leading whitespace.
    text.split_whitespace()
        .next()
        .and_then(|word| word.parse().ok())
}

/// Remove a skill and every reference to it — the alias map and any
/// bullet/role/project/achievement that lists it. Leaving references
/// dangling would just hand `dataset validate` a new problem.
fn remove_skill(dataset: &mut ResumeDataset, id: &SkillId) {
    dataset.skills.skills.retain(|skill| skill.id != *id);
    dataset.skills.aliases.retain(|_, mapped| mapped != id);
    for role in &mut dataset.roles {
        role.skill_ids.retain(|s| s != id);
        for bullet in &mut role.bullets {
            bullet.skill_ids.retain(|s| s != id);
        }
    }
    for project in &mut dataset.projects {
        project.skill_ids.retain(|s| s != id);
    }
    for achievement in &mut dataset.achievements {
        achievement.skill_ids.retain(|s| s != id);
    }
}

/// One redundant skill the dedup dropped, and the kept skill that already
/// covers it — the auditable unit of a prune.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedSkill {
    pub removed: String,
    pub kept: String,
}

/// Remove recorded skills another recorded skill already subsumes: an exact
/// normalized duplicate ("data engineering" / "Data Engineering"), or one
/// whose tokens are a *proper subset* of another's ("remote-first" under
/// "Remote-First Communication"). The more specific phrasing is kept, and
/// only when that kept skill is itself evidence-backed, so the concept
/// stays on the page. References are cleaned for every removal. Returns
/// what was pruned, in removal order.
///
/// Deterministic and conservative on purpose: it never touches synonym
/// pairs that aren't token-subsets ("operational excellence" vs
/// "engineering excellence") — judging those needs a person, which is what
/// the `skills dedup` command's manual pass is for.
pub fn dedup_skills(dataset: &mut ResumeDataset) -> Vec<PrunedSkill> {
    // Snapshot each skill's id, normalized key, name, and whether it's
    // backed — so the comparison reads from a stable view while we decide.
    let snapshot: Vec<(SkillId, Vec<String>, String, bool)> = dataset
        .skills
        .skills
        .iter()
        .map(|s| {
            (
                s.id.clone(),
                keyword_key(&s.canonical_name),
                s.canonical_name.clone(),
                !s.evidence.is_empty(),
            )
        })
        .collect();

    let mut removed_ids: Vec<SkillId> = Vec::new();
    let mut pruned: Vec<PrunedSkill> = Vec::new();
    for (i, (id_a, key_a, name_a, _)) in snapshot.iter().enumerate() {
        if key_a.is_empty() || removed_ids.contains(id_a) {
            continue;
        }
        for (j, (id_b, key_b, name_b, backed_b)) in snapshot.iter().enumerate() {
            if i == j || !backed_b || removed_ids.contains(id_b) {
                continue;
            }
            let subset = key_a.iter().all(|t| key_b.contains(t));
            let removable = (subset && key_a.len() < key_b.len()) // proper subset
                || (*key_a == *key_b && i > j); // exact dup: keep the first
            if removable {
                removed_ids.push(id_a.clone());
                pruned.push(PrunedSkill {
                    removed: name_a.clone(),
                    kept: name_b.clone(),
                });
                break;
            }
        }
    }
    for id in &removed_ids {
        remove_skill(dataset, id);
    }
    pruned
}

/// Remove the named skills (by id) with full reference cleanup — the
/// manual half of `skills dedup`, where a person picks redundant synonyms
/// the deterministic pass can't safely judge. Unknown ids are ignored.
pub fn remove_skills(dataset: &mut ResumeDataset, ids: &[SkillId]) {
    for id in ids {
        remove_skill(dataset, id);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{
        BulletId, Contact, EmploymentType, Proficiency, Role, RoleId, Skill, SkillCategory,
        YearMonth,
    };
    use crate::llm::MockLlmClient;
    use crate::trace::Tracer;
    use crate::user::ScriptedUser;

    fn dataset_with_unbacked(name: &str) -> ResumeDataset {
        let mut dataset = ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        dataset.roles.push(Role {
            id: RoleId("role-1".into()),
            company: "Acme".into(),
            title: "Director".into(),
            start: YearMonth {
                year: 2019,
                month: 4,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: vec![Bullet {
                id: BulletId("bullet-7".into()),
                text: "Did things".into(),
                skill_ids: Vec::new(),
                metric: None,
                theme: Vec::new(),
                strength: Strength::Medium,
                variants: Vec::new(),
            }],
            skill_ids: Vec::new(),
            context: None,
        });
        dataset.skills.skills.push(Skill {
            id: SkillId("skill-1".into()),
            canonical_name: name.into(),
            aliases: vec!["ts".into()],
            category: SkillCategory::Language,
            proficiency: Proficiency::Proficient,
            years: None,
            last_used: None,
            evidence: Vec::new(),
            verified: false,
            verified_at: None,
        });
        dataset
            .skills
            .aliases
            .insert(name.to_lowercase(), SkillId("skill-1".into()));
        dataset
            .skills
            .aliases
            .insert("ts".into(), SkillId("skill-1".into()));
        dataset
    }

    /// A guide context whose only LLM call is the rewrite agent's, fed one
    /// scripted reply. Mirrors the strengthen tests' setup.
    fn rewrite_ctx(mock: &MockLlmClient) -> AgentContext<'_> {
        AgentContext {
            llm: mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        }
    }

    #[tokio::test]
    async fn add_one_skill_creates_a_new_skill_backed_by_a_role() {
        // "Kubernetes" isn't in the dataset yet; adding it mints a skill.
        let mut dataset = dataset_with_unbacked("TypeScript");
        let before = dataset.skills.skills.len();
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // role-1 (no guide: menu is [role, skip])
        user.answer(Answer::Text("3".into())); // years
        user.answer(Answer::Text("Ran the cluster".into())); // sentence

        let outcome = add_one_skill(&mut dataset, "Kubernetes", SkillCategory::Tool, &user, None)
            .await
            .unwrap();

        assert_eq!(outcome.verified, 1);
        assert_eq!(dataset.skills.skills.len(), before + 1);
        let added = dataset
            .skills
            .skills
            .iter()
            .find(|s| s.canonical_name == "Kubernetes")
            .unwrap();
        assert!(added.verified);
        assert_eq!(
            added.evidence,
            vec![EvidenceRef::Role(RoleId("role-1".into()))]
        );
        // The alias map resolves the JD spelling to the new skill.
        assert!(dataset.skills.aliases.contains_key("kubernetes"));
        // Keyless: the sentence is recorded verbatim.
        assert!(
            dataset.roles[0]
                .bullets
                .iter()
                .any(|b| b.text == "Ran the cluster")
        );
    }

    #[tokio::test]
    async fn add_one_skill_backs_an_existing_skill_without_duplicating() {
        // "TypeScript" is already recorded (unbacked); adding it must reuse
        // that skill, not create a second one.
        let mut dataset = dataset_with_unbacked("TypeScript");
        let before = dataset.skills.skills.len();
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // role-1
        user.answer(Answer::Text("".into())); // no years
        user.answer(Answer::Text("Built the trading UI".into())); // sentence

        let outcome = add_one_skill(
            &mut dataset,
            "typescript", // different case: resolves via the alias map
            SkillCategory::Language,
            &user,
            None,
        )
        .await
        .unwrap();

        assert_eq!(outcome.verified, 1);
        assert_eq!(dataset.skills.skills.len(), before); // no duplicate
        let skill = &dataset.skills.skills[0];
        assert_eq!(
            skill.evidence,
            vec![EvidenceRef::Role(RoleId("role-1".into()))]
        );
    }

    #[tokio::test]
    async fn a_typed_sentence_is_polished_when_a_guide_is_present() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let mock = MockLlmClient::default();
        // The rewrite agent's polished line (no invented numbers).
        mock.enqueue(r#"{"bullet": "Built the trading dashboard in TypeScript"}"#);
        let ctx = rewrite_ctx(&mock);
        let user = ScriptedUser::new();
        // With a guide, the role menu is [role-1, "explain it", "skip"].
        user.answer(Answer::Choice(0)); // role-1
        user.answer(Answer::Text("".into())); // years
        user.answer(Answer::Text(
            "built the trading dashboard, lots of ts".into(),
        ));
        user.answer(Answer::Choice(0)); // polish: "Use this wording"

        let outcome = add_one_skill(
            &mut dataset,
            "Kubernetes",
            SkillCategory::Tool,
            &user,
            Some(&ctx),
        )
        .await
        .unwrap();

        assert_eq!(outcome.verified, 1);
        assert_eq!(mock.requests().len(), 1); // only the rewrite call
        // The recorded bullet is the polished wording, not the raw answer.
        assert!(
            dataset.roles[0]
                .bullets
                .iter()
                .any(|b| b.text == "Built the trading dashboard in TypeScript")
        );
    }

    #[tokio::test]
    async fn a_polished_sentence_that_invents_a_number_falls_back_to_the_users_words() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let mock = MockLlmClient::default();
        // The rewrite slips in "40%", a number the answer never stated — the
        // digit-guard rejects it and the user's own words stand.
        mock.enqueue(r#"{"bullet": "Cut load times 40% with a TypeScript rewrite"}"#);
        let ctx = rewrite_ctx(&mock);
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // role-1
        user.answer(Answer::Text("".into())); // years
        user.answer(Answer::Text("rewrote the dashboard in typescript".into()));
        // No accept/revise prompt is shown: the guarded rewrite was discarded.

        let outcome = add_one_skill(
            &mut dataset,
            "Kubernetes",
            SkillCategory::Tool,
            &user,
            Some(&ctx),
        )
        .await
        .unwrap();

        assert_eq!(outcome.verified, 1);
        assert!(
            dataset.roles[0]
                .bullets
                .iter()
                .any(|b| b.text == "rewrote the dashboard in typescript")
        );
        // No invented figure reached the dataset.
        assert!(
            !dataset.roles[0]
                .bullets
                .iter()
                .any(|b| b.text.contains("40%"))
        );
    }

    #[tokio::test]
    async fn a_yes_adds_evidence_years_and_a_user_written_bullet() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // yes, professionally
        user.answer(Answer::Choice(0)); // role-1
        user.answer(Answer::Text("4 years".into()));
        user.answer(Answer::Text("Built the trading UI in TypeScript".into()));

        let outcome = verify_unbacked(&mut dataset, &user, None).await.unwrap();

        assert_eq!(outcome.verified, 1);
        assert_eq!(outcome.bullets_added, 1);
        assert!(outcome.changed());
        let skill = &dataset.skills.skills[0];
        assert_eq!(
            skill.evidence,
            vec![EvidenceRef::Role(RoleId("role-1".into()))]
        );
        assert!(skill.verified);
        assert!(skill.verified_at.is_some());
        assert_eq!(skill.years, Some(4.0));
        // The sentence became a real, skill-linked bullet with the next
        // ID in sequence (existing bullet-7 -> new bullet-8).
        let bullet = dataset.roles[0].bullets.last().unwrap();
        assert_eq!(bullet.id, BulletId("bullet-8".into()));
        assert_eq!(bullet.text, "Built the trading UI in TypeScript");
        assert_eq!(bullet.skill_ids, vec![SkillId("skill-1".into())]);
        assert!(
            dataset.roles[0]
                .skill_ids
                .contains(&SkillId("skill-1".into()))
        );
    }

    #[tokio::test]
    async fn a_no_removes_the_skill_and_every_reference() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        // Seed references that must not dangle afterwards.
        dataset.roles[0].skill_ids.push(SkillId("skill-1".into()));
        dataset.roles[0].bullets[0]
            .skill_ids
            .push(SkillId("skill-1".into()));
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(2)); // no - remove

        let outcome = verify_unbacked(&mut dataset, &user, None).await.unwrap();

        assert_eq!(outcome.removed, 1);
        assert!(dataset.skills.skills.is_empty());
        assert!(dataset.skills.aliases.is_empty());
        assert!(dataset.roles[0].skill_ids.is_empty());
        assert!(dataset.roles[0].bullets[0].skill_ids.is_empty());
    }

    #[tokio::test]
    async fn skips_change_nothing() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let before = dataset.clone();
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(3)); // skip

        let outcome = verify_unbacked(&mut dataset, &user, None).await.unwrap();

        assert_eq!(outcome.skipped, 1);
        assert!(!outcome.changed());
        assert_eq!(dataset, before);
    }

    #[tokio::test]
    async fn an_aborted_interview_is_an_error_the_caller_must_not_save() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // yes...
        // ...but the script ends before "which role" — like Esc mid-flow.

        let result = verify_unbacked(&mut dataset, &user, None).await;
        assert!(matches!(result, Err(AskError::NotInteractive { .. })));
    }

    #[tokio::test]
    async fn fully_backed_datasets_are_left_alone() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        dataset.skills.skills[0]
            .evidence
            .push(EvidenceRef::Role(RoleId("role-1".into())));
        let user = ScriptedUser::new();

        let outcome = verify_unbacked(&mut dataset, &user, None).await.unwrap();

        assert_eq!(outcome, VerifyOutcome::default());
        assert!(
            user.notices()
                .iter()
                .any(|n| n.contains("already evidence-backed"))
        );
    }

    fn gap_wanting(name: &str) -> GapReport {
        use crate::jd::{Importance, JdSkill};
        GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: vec![JdSkill {
                name: name.into(),
                category: SkillCategory::Hard,
                importance: Importance::Required,
                context_phrases: Vec::new(),
            }],
        }
    }

    /// A JD whose only meaningful field for keyword candidacy is its ATS
    /// phrases (the function reads skills from the gap, not the JD). The
    /// title is neutral so it doesn't accidentally filter a test phrase.
    fn jd_with_phrases(phrases: &[&str]) -> JobRequirements {
        jd_titled("Staff Architect", phrases)
    }

    fn jd_titled(title: &str, phrases: &[&str]) -> JobRequirements {
        use crate::jd::{RemotePolicy, Seniority};
        JobRequirements {
            company: "amplo".into(),
            title: title.into(),
            seniority: Seniority::Senior,
            location: None,
            remote: RemotePolicy::Unspecified,
            domain_keywords: Vec::new(),
            required_skills: Vec::new(),
            preferred_skills: Vec::new(),
            responsibilities: Vec::new(),
            ats_phrases: phrases.iter().map(|p| (*p).to_string()).collect(),
            raw_text: String::new(),
            source_url: None,
        }
    }

    #[test]
    fn unbacked_keywords_gathers_skills_and_phrases_minus_backed_and_declined() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        // The user already said "no" to insurtech once.
        dataset.metadata.declined_skills.push("insurtech".into());
        let gap = gap_wanting("Kafka"); // unmatched JD skill
        // "TypeScript" resolves to a recorded skill (backed); "insurtech"
        // is declined; "team management" is a genuine candidate.
        let jd = jd_with_phrases(&["TypeScript", "team management", "insurtech"]);

        let names: Vec<String> = unbacked_keywords(&dataset, &jd, &gap)
            .into_iter()
            .map(|c| c.name)
            .collect();

        assert_eq!(names, vec!["Kafka", "team management"]);
    }

    #[tokio::test]
    async fn verify_keywords_records_checked_and_declines_the_rest() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let before_skills = dataset.skills.skills.len();
        let candidates = vec![
            KeywordCandidate {
                name: "Data Engineering".into(),
                category: SkillCategory::Hard,
            },
            KeywordCandidate {
                name: "people manager".into(),
                category: SkillCategory::Domain,
            },
        ];
        let user = ScriptedUser::new();
        user.answer(Answer::Choices(vec![0])); // only the first is "me"
        user.answer(Answer::Choice(0)); // role-1 for Data Engineering
        user.answer(Answer::Text("3".into()));
        user.answer(Answer::Text(
            "Built the trade-settlement data pipeline".into(),
        ));

        let outcome = verify_keywords(&mut dataset, &candidates, &user, None)
            .await
            .unwrap();

        assert_eq!(outcome.verified, 1);
        assert_eq!(outcome.declined, 1);
        assert!(outcome.changed());
        // The checked keyword became a real, verified, role-backed skill.
        assert_eq!(dataset.skills.skills.len(), before_skills + 1);
        let added = dataset
            .skills
            .skills
            .iter()
            .find(|s| s.canonical_name == "Data Engineering")
            .unwrap();
        assert!(added.verified);
        assert_eq!(
            added.evidence,
            vec![EvidenceRef::Role(RoleId("role-1".into()))]
        );
        assert!(
            dataset.roles[0]
                .bullets
                .iter()
                .any(|b| b.text.contains("data pipeline"))
        );
        // The unchecked keyword is remembered so it isn't re-offered.
        assert_eq!(
            dataset.metadata.declined_skills,
            vec!["people manager".to_string()]
        );
    }

    #[tokio::test]
    async fn verify_keywords_with_nothing_checked_declines_everything() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let candidates = vec![
            KeywordCandidate {
                name: "SaaS environment".into(),
                category: SkillCategory::Domain,
            },
            KeywordCandidate {
                name: "engineering excellence".into(),
                category: SkillCategory::Domain,
            },
        ];
        let user = ScriptedUser::new();
        user.answer(Answer::Choices(Vec::new())); // checked nothing

        let outcome = verify_keywords(&mut dataset, &candidates, &user, None)
            .await
            .unwrap();

        assert_eq!(outcome.verified, 0);
        assert_eq!(outcome.declined, 2);
        assert!(outcome.changed());
        assert_eq!(
            dataset.metadata.declined_skills,
            vec![
                "saas environment".to_string(),
                "engineering excellence".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn verify_keywords_on_no_candidates_changes_nothing() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let user = ScriptedUser::new();

        let outcome = verify_keywords(&mut dataset, &[], &user, None)
            .await
            .unwrap();

        assert_eq!(outcome, VerifyOutcome::default());
    }

    #[tokio::test]
    async fn the_help_pass_rescues_an_unchecked_keyword() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let candidates = vec![
            KeywordCandidate {
                name: "Data Engineering".into(),
                category: SkillCategory::Hard,
            },
            KeywordCandidate {
                name: "SOC 2 Type 2".into(),
                category: SkillCategory::Domain,
            },
        ];
        let mock = MockLlmClient::default();
        // The guide's opener description for the keyword being discussed,
        // then the rewrite that polishes the evidence sentence.
        mock.enqueue(
            r#"{"reply": "SOC 2 Type 2 is an audited security/compliance attestation; if you owned or drove one, that counts."}"#,
        );
        mock.enqueue(r#"{"bullet": "Drove the SOC 2 Type 2 audit to completion"}"#);
        let ctx = AgentContext {
            llm: &mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        };
        let user = ScriptedUser::new();
        user.answer(Answer::Choices(Vec::new())); // checklist: nothing checked
        user.confirm_with(true); // yes, talk through the unchecked
        user.answer(Answer::Choice(1)); // discuss "SOC 2 Type 2"
        user.answer(Answer::Text(String::new())); // opener shown; no follow-up
        user.confirm_with(true); // "Add SOC 2 Type 2 now?" -> yes
        user.answer(Answer::Choice(0)); // role-1
        user.answer(Answer::Text("2".into())); // years
        user.answer(Answer::Text("Drove the SOC 2 Type 2 audit".into())); // sentence
        user.answer(Answer::Choice(0)); // polish: use the suggested wording
        user.answer(Answer::Choice(1)); // "Done" — set the rest aside

        let outcome = verify_keywords(&mut dataset, &candidates, &user, Some(&ctx))
            .await
            .unwrap();

        // The discussed keyword was rescued and recorded; the other,
        // never touched, is declined.
        assert_eq!(outcome.verified, 1);
        assert_eq!(outcome.declined, 1);
        let added = dataset
            .skills
            .skills
            .iter()
            .find(|s| s.canonical_name == "SOC 2 Type 2")
            .unwrap();
        assert!(added.verified);
        assert_eq!(
            added.evidence,
            vec![EvidenceRef::Role(RoleId("role-1".into()))]
        );
        assert_eq!(
            dataset.metadata.declined_skills,
            vec!["data engineering".to_string()]
        );
        // The guide ran twice: the opener, then the rewrite that polished
        // the evidence sentence into resume wording.
        assert_eq!(mock.requests().len(), 2);
        assert!(user.notices().iter().any(|n| n.contains("attestation")));
    }

    #[test]
    fn keyword_key_collapses_rewordings_but_keeps_distinct_concepts() {
        // Inflection and seniority/filler words don't distinguish.
        assert_eq!(
            keyword_key("people management"),
            keyword_key("people manager")
        );
        assert_eq!(
            keyword_key("engineering manager"),
            keyword_key("Sr Engineering Manager")
        );
        assert_eq!(
            keyword_key("Senior Engineering Manager"),
            keyword_key("engineering management")
        );
        assert_eq!(
            keyword_key("Insurtech Industry Experience"),
            keyword_key("insurtech")
        );
        // Genuinely different concepts stay apart.
        assert_ne!(
            keyword_key("team management"),
            keyword_key("people management")
        );
        assert_ne!(
            keyword_key("backend engineering"),
            keyword_key("engineering")
        );
    }

    #[test]
    fn unbacked_keywords_collapses_reworded_phrases_to_one_row() {
        let dataset = dataset_with_unbacked("TypeScript");
        let gap = GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: Vec::new(),
        };
        let jd = jd_with_phrases(&[
            "people management",
            "people manager",
            "Senior Engineering Manager",
            "engineering manager",
        ]);

        let names: Vec<String> = unbacked_keywords(&dataset, &jd, &gap)
            .into_iter()
            .map(|c| c.name)
            .collect();

        // Four phrases, two concepts; the first wording of each survives.
        assert_eq!(
            names,
            vec!["people management", "Senior Engineering Manager"]
        );
    }

    #[test]
    fn unbacked_keywords_never_offers_the_job_title_as_a_skill() {
        let dataset = dataset_with_unbacked("TypeScript");
        let gap = GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: Vec::new(),
        };
        let jd = jd_titled(
            "Senior Engineering Manager",
            &[
                "Senior Engineering Manager",
                "Sr. Engineering Manager",
                "SaaS environment",
            ],
        );

        let names: Vec<String> = unbacked_keywords(&dataset, &jd, &gap)
            .into_iter()
            .map(|c| c.name)
            .collect();

        // The title and its seniority variant are filtered; the real
        // keyword stays.
        assert_eq!(names, vec!["SaaS environment"]);
    }

    #[tokio::test]
    async fn explain_is_reachable_while_populating_a_checked_keyword() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let candidates = vec![KeywordCandidate {
            name: "Backend Engineering".into(),
            category: SkillCategory::Hard,
        }];
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"reply": "Backend engineering is server-side systems work — APIs, databases, services."}"#,
        );
        mock.enqueue(r#"{"bullet": "Ran the backend engineering team"}"#);
        let ctx = AgentContext {
            llm: &mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        };
        let user = ScriptedUser::new();
        user.answer(Answer::Choices(vec![0])); // check it
        // Role menu is [role-1, "explain", "skip"]; ask to explain first.
        user.answer(Answer::Choice(1));
        user.answer(Answer::Text(String::new())); // opener shown; no follow-up
        // Re-asked, now pick the real role and finish.
        user.answer(Answer::Choice(0));
        user.answer(Answer::Text(String::new()));
        user.answer(Answer::Text("Ran the backend team".into()));
        user.answer(Answer::Choice(0)); // polish: use the suggested wording

        let outcome = verify_keywords(&mut dataset, &candidates, &user, Some(&ctx))
            .await
            .unwrap();

        assert_eq!(outcome.verified, 1);
        // Two guide calls: the explain opener, then the rewrite that polished
        // the evidence sentence.
        assert_eq!(mock.requests().len(), 2);
        assert!(user.notices().iter().any(|n| n.contains("server-side")));
        let added = dataset
            .skills
            .skills
            .iter()
            .find(|s| s.canonical_name == "Backend Engineering")
            .unwrap();
        assert_eq!(
            added.evidence,
            vec![EvidenceRef::Role(RoleId("role-1".into()))]
        );
    }

    #[tokio::test]
    async fn a_confused_user_consults_the_guide_then_answers() {
        let mut dataset = dataset_with_unbacked("TypeScript");
        let mock = MockLlmClient::default();
        // Reply 1 is the unprompted opener description; reply 2 answers
        // the user's follow-up.
        mock.enqueue(
            r#"{"reply": "TypeScript is a typed superset of JavaScript; you write JS with type annotations."}"#,
        );
        mock.enqueue(r#"{"reply": "Yes — typed front-end code counts."}"#);
        let ctx = AgentContext {
            llm: &mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        };
        let user = ScriptedUser::new();
        // With a guide present, "let me explain" is option 4. Choosing it
        // shows the description right away, before any question is typed.
        user.answer(Answer::Choice(4));
        user.answer(Answer::Text("does typed front-end code count?".into()));
        user.confirm_with(false); // done explaining
        // Re-asked, the user now answers yes and completes the interview.
        user.answer(Answer::Choice(0));
        user.answer(Answer::Choice(0));
        user.answer(Answer::Text(String::new()));
        user.answer(Answer::Text(String::new()));

        let outcome = verify_unbacked(&mut dataset, &user, Some(&ctx))
            .await
            .unwrap();

        assert_eq!(outcome.verified, 1);
        // The guide ran twice: the auto-description, then the follow-up.
        assert_eq!(mock.requests().len(), 2);
        // The description was shown without the user asking for it.
        assert!(user.notices().iter().any(|n| n.contains("typed superset")));
        assert!(
            user.notices()
                .iter()
                .any(|n| n.contains("front-end code counts"))
        );
    }

    #[tokio::test]
    #[ignore = "exercise: a verified skill takes evidence from exactly one role; let the user keep adding roles (confirm-driven loop, no duplicate evidence), then finish this test"]
    async fn ex_016_multiple_roles_can_back_one_skill() {
        // Once the loop exists: dataset with two roles, script a yes,
        // the first role, a confirmed second pick, the other role, then
        // years/sentence; assert two distinct Role evidence entries.
        let multi_role_implemented = false;
        assert!(multi_role_implemented);
    }

    #[test]
    fn years_parse_leniently() {
        assert_eq!(parse_years("4"), Some(4.0));
        assert_eq!(parse_years(" 2.5 years "), Some(2.5));
        assert_eq!(parse_years(""), None);
        assert_eq!(parse_years("a while"), None);
    }

    /// A dataset with one role and the given skills (name, evidence-backed?).
    fn dataset_with_skill_list(skills: &[(&str, bool)]) -> ResumeDataset {
        let mut dataset = ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        dataset.roles.push(Role {
            id: RoleId("role-1".into()),
            company: "Acme".into(),
            title: "Director".into(),
            start: YearMonth {
                year: 2019,
                month: 4,
            },
            end: None,
            location: None,
            employment_type: EmploymentType::FullTime,
            bullets: Vec::new(),
            skill_ids: Vec::new(),
            context: None,
        });
        for (i, (name, evidenced)) in skills.iter().enumerate() {
            let id = SkillId(format!("skill-{}", i + 1));
            dataset.skills.skills.push(Skill {
                id: id.clone(),
                canonical_name: (*name).into(),
                aliases: Vec::new(),
                category: SkillCategory::Domain,
                proficiency: Proficiency::Proficient,
                years: None,
                last_used: None,
                evidence: if *evidenced {
                    vec![EvidenceRef::Role(RoleId("role-1".into()))]
                } else {
                    Vec::new()
                },
                verified: false,
                verified_at: None,
            });
            dataset.skills.aliases.insert(name.to_lowercase(), id);
        }
        dataset
    }

    #[test]
    fn unbacked_keywords_skips_a_concept_a_backed_skill_already_covers() {
        // A recorded, evidence-backed skill whose tokens cover "remote-first".
        let dataset = dataset_with_skill_list(&[("Remote-First Communication", true)]);
        let gap = GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: Vec::new(),
        };
        // "remote-first" is already covered (subset of the backed skill);
        // "Kubernetes" is a genuine gap.
        let jd = jd_with_phrases(&["remote-first", "Kubernetes"]);
        let names: Vec<String> = unbacked_keywords(&dataset, &jd, &gap)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, vec!["Kubernetes"]);
    }

    #[test]
    fn an_unbacked_skill_does_not_cover_a_concept() {
        // The covering skill has NO evidence, so it can't suppress the
        // candidate — the user still gets to back the concept.
        let dataset = dataset_with_skill_list(&[("Remote-First Communication", false)]);
        let gap = GapReport {
            matched: Vec::new(),
            weak: Vec::new(),
            unknown: Vec::new(),
        };
        let jd = jd_with_phrases(&["remote-first"]);
        let names: Vec<String> = unbacked_keywords(&dataset, &jd, &gap)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, vec!["remote-first"]);
    }

    #[test]
    fn dedup_removes_a_token_subset_keeping_the_backed_superset() {
        let mut dataset = dataset_with_skill_list(&[
            ("Remote-First Communication", true),
            ("remote-first", true),
        ]);
        let pruned = dedup_skills(&mut dataset);

        let names: Vec<String> = dataset
            .skills
            .skills
            .iter()
            .map(|s| s.canonical_name.clone())
            .collect();
        assert_eq!(names, vec!["Remote-First Communication"]);
        assert_eq!(pruned.len(), 1);
        assert_eq!(pruned[0].removed, "remote-first");
        assert_eq!(pruned[0].kept, "Remote-First Communication");
        // The alias for the removed skill is cleaned up too.
        assert!(!dataset.skills.aliases.contains_key("remote-first"));
    }

    #[test]
    fn dedup_collapses_an_exact_normalized_duplicate() {
        let mut dataset =
            dataset_with_skill_list(&[("Data Engineering", true), ("data engineering", true)]);
        let pruned = dedup_skills(&mut dataset);
        // Exact key dup: the first (kept) survives, the later one goes.
        assert_eq!(dataset.skills.skills.len(), 1);
        assert_eq!(dataset.skills.skills[0].canonical_name, "Data Engineering");
        assert_eq!(pruned.len(), 1);
    }

    #[test]
    fn dedup_leaves_non_subset_synonyms_alone() {
        // Share only "excellence" — neither subsets the other, so the
        // deterministic pass keeps both (a person decides via the manual pass).
        let mut dataset = dataset_with_skill_list(&[
            ("operational excellence", true),
            ("engineering excellence", true),
        ]);
        let pruned = dedup_skills(&mut dataset);
        assert!(pruned.is_empty());
        assert_eq!(dataset.skills.skills.len(), 2);
    }

    #[test]
    fn dedup_keeps_a_subset_when_the_only_superset_is_unbacked() {
        // The superset has no evidence, so removing the subset would drop
        // the concept off the page entirely — keep both instead.
        let mut dataset = dataset_with_skill_list(&[
            ("Remote-First Communication", false),
            ("remote-first", true),
        ]);
        let pruned = dedup_skills(&mut dataset);
        assert!(pruned.is_empty());
        assert_eq!(dataset.skills.skills.len(), 2);
    }
}
