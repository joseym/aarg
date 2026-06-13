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
    Bullet, BulletId, EvidenceRef, Proficiency, ResumeDataset, Skill, SkillCategory, SkillId,
    Strength,
};
use crate::gap::GapReport;
use crate::guide::{GuideInput, GuideTurn, VerificationGuideAgent};
use crate::jd::JobRequirements;
use crate::user::{Answer, AskError, Question, UserHandle};

/// How many guide exchanges before nudging the user back to the
/// question — a confused user shouldn't loop forever, and each exchange
/// is an LLM call.
const MAX_GUIDE_EXCHANGES: usize = 4;

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
                let (role_index, years, sentence) =
                    collect_evidence(name, role_options, user).await?;
                return Ok(Verdict::Have {
                    role_index,
                    years,
                    sentence,
                });
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
                "Ask anything about \"{name}\" — what it means, or describe what you did and I'll help you decide honestly."
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
                user.notify("(couldn't reach the guide right now — answer as best you can)");
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

/// The questions behind a "yes": which role best shows the skill,
/// roughly how many years, and an optional one-sentence description.
/// Shared by the per-skill interview and the keyword triage, which
/// reach the same point — "the user has this" — by different routes.
async fn collect_evidence(
    name: &str,
    role_options: &[String],
    user: &dyn UserHandle,
) -> Result<(usize, Option<f32>, Option<String>), AskError> {
    let role_index = match user
        .ask(Question::Select {
            prompt: format!("Which role best demonstrates {name}?"),
            options: role_options.to_vec(),
        })
        .await?
    {
        Answer::Choice(index) if index < role_options.len() => index,
        _ => 0,
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
            prompt: format!("One sentence on what you did with {name} (blank to skip)"),
        })
        .await?
    {
        Answer::Text(text) => {
            let text = text.trim().to_string();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    };
    Ok((role_index, years, sentence))
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
        let bullet_id = next_bullet_id(dataset);
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
        user.notify("the dataset has no roles to attach evidence to — ingest a resume first");
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
/// a settled "no" isn't a candidate — and duplicates are collapsed
/// case-insensitively (the JD often lists a skill and a near-identical
/// phrase). This is what the triage checklist offers.
pub fn unbacked_keywords(
    dataset: &ResumeDataset,
    jd: &JobRequirements,
    gap: &GapReport,
) -> Vec<KeywordCandidate> {
    let declined = &dataset.metadata.declined_skills;
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<KeywordCandidate> = Vec::new();

    let mut consider = |name: &str, category: SkillCategory| {
        let key = name.to_lowercase();
        if declined.contains(&key) || seen.contains(&key) {
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
        user.notify("the dataset has no roles to attach evidence to — ingest a resume first");
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
    // help pass); whatever's left at the end is declined.
    let role_options = role_options(dataset);
    let mut recorded = vec![false; candidates.len()];
    for (index, candidate) in candidates.iter().enumerate() {
        if checked.contains(&index) {
            record_keyword(dataset, candidate, &role_options, user, &mut outcome).await?;
            recorded[index] = true;
        }
    }

    // The "help me decide" pass over the keywords left unchecked; it
    // flips `recorded` for anything the user rescues.
    if let Some(ctx) = guide {
        clarify_unchecked(
            dataset,
            candidates,
            &mut recorded,
            &role_options,
            user,
            ctx,
            &mut outcome,
        )
        .await?;
    }

    // Whatever is still unrecorded — unchecked and not rescued — is a
    // "no" worth remembering.
    for (index, candidate) in candidates.iter().enumerate() {
        if !recorded[index] {
            decline_keyword(dataset, candidate, &mut outcome);
        }
    }

    Ok(outcome)
}

/// Run the evidence interview for one claimed keyword and add it as a
/// recorded skill. Shared by the checklist pass and the help pass.
async fn record_keyword(
    dataset: &mut ResumeDataset,
    candidate: &KeywordCandidate,
    role_options: &[String],
    user: &dyn UserHandle,
    outcome: &mut VerifyOutcome,
) -> Result<(), AskError> {
    let (role_index, years, sentence) =
        collect_evidence(&candidate.name, role_options, user).await?;
    let skill_id = add_skill(dataset, &candidate.name, candidate.category);
    attach_evidence(dataset, &skill_id, role_index, years, sentence, outcome);
    user.notify(&format!("added {}", candidate.name));
    outcome.verified += 1;
    Ok(())
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

/// The "help me decide" pass: let the user talk through any unchecked
/// keyword with the guide and pull it in if it turns out they have it.
/// Touches only the keywords left unchecked, so confident ticks are
/// undisturbed. Flips `recorded[i]` for anything rescued.
async fn clarify_unchecked(
    dataset: &mut ResumeDataset,
    candidates: &[KeywordCandidate],
    recorded: &mut [bool],
    role_options: &[String],
    user: &dyn UserHandle,
    ctx: &AgentContext<'_>,
    outcome: &mut VerifyOutcome,
) -> Result<(), AskError> {
    // Indexes still in play: unchecked, not yet recorded.
    let mut pending: Vec<usize> = (0..candidates.len()).filter(|i| !recorded[*i]).collect();
    if pending.is_empty() {
        return Ok(());
    }
    if !user
        .confirm("talk through any of the ones you left unchecked?", false)
        .await?
    {
        return Ok(());
    }

    loop {
        let mut options: Vec<String> = pending
            .iter()
            .map(|&i| candidates[i].name.clone())
            .collect();
        options.push("Done — set the rest aside".to_string());
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

        let candidate_index = pending[pick];
        clarify(&candidates[candidate_index].name, role_options, user, ctx).await?;
        if user
            .confirm(
                &format!("Add {} now?", candidates[candidate_index].name),
                false,
            )
            .await?
        {
            record_keyword(
                dataset,
                &candidates[candidate_index],
                role_options,
                user,
                outcome,
            )
            .await?;
            // Mark it recorded so the caller's decline sweep skips it,
            // and drop it from the pending menu.
            recorded[candidate_index] = true;
            pending.remove(pick);
        }
        if pending.is_empty() {
            break;
        }
    }
    Ok(())
}

/// "Title — Company" for each role, the menu the interview offers.
fn role_options(dataset: &ResumeDataset) -> Vec<String> {
    dataset
        .roles
        .iter()
        .map(|role| format!("{} — {}", role.title, role.company))
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

/// New bullet IDs continue the existing `bullet-N` sequence.
fn next_bullet_id(dataset: &ResumeDataset) -> BulletId {
    let highest = dataset
        .roles
        .iter()
        .flat_map(|role| role.bullets.iter())
        .filter_map(|bullet| bullet.id.0.strip_prefix("bullet-")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    BulletId(format!("bullet-{}", highest + 1))
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{
        Contact, EmploymentType, Proficiency, Role, RoleId, Skill, SkillCategory, YearMonth,
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
    /// phrases (the function reads skills from the gap, not the JD).
    fn jd_with_phrases(phrases: &[&str]) -> JobRequirements {
        use crate::jd::{RemotePolicy, Seniority};
        JobRequirements {
            company: "amplo".into(),
            title: "Senior Engineering Manager".into(),
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
        // The guide's opener description for the keyword being discussed.
        mock.enqueue(
            r#"{"reply": "SOC 2 Type 2 is an audited security/compliance attestation; if you owned or drove one, that counts."}"#,
        );
        let ctx = AgentContext {
            llm: &mock,
            model: "m",
            tracer: &Tracer::DISABLED,
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
        // The guide was consulted once (the opener) and its reply shown.
        assert_eq!(mock.requests().len(), 1);
        assert!(user.notices().iter().any(|n| n.contains("attestation")));
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
            model: "m",
            tracer: &Tracer::DISABLED,
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
}
