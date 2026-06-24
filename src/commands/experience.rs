//! `aarg experience add|list|remove` — record projects, open-source, or
//! other experience that isn't a job, and link the skills it demonstrates.
//!
//! Roles are the spine of a resume, but plenty of real evidence lives
//! outside them: a side project, an open-source contribution, founding
//! something. The dataset has always modeled this as `Project`, and
//! tailoring already renders projects and counts them as skill evidence
//! (`EvidenceRef::Project`) — there was just no way to add one unless it
//! appeared on an ingested resume. This is that way. Thin glue: load,
//! interview (or take flags for a scripted run), save once.
//!
//! Never-fabricate holds the same as everywhere else: the user names the
//! project, writes its summary, and chooses which of *their own* recorded
//! skills it backs. Linking only ever adds a real project as evidence to a
//! skill that already exists; it mints no skills and invents no claims.

use crate::commands::CliError;
use crate::dataset::store;
use crate::dataset::types::{EvidenceRef, Project, ProjectId, ResumeDataset, SkillId};
use crate::style;
use crate::terminal::auto_user;
use crate::user::{Answer, Question, UserHandle};

/// `aarg experience add [name]` — record one project / non-job experience
/// and link the recorded skills it demonstrates. The name comes from the
/// argument or a prompt (blank cancels); `--summary` and `--url` skip their
/// prompts, and `--skill <name>` (repeatable) links skills without the
/// interactive picker, so a scripted run works end to end. Saves once.
pub async fn add(
    name: Option<String>,
    summary: Option<String>,
    url: Option<String>,
    skill_flags: Vec<String>,
) -> Result<(), CliError> {
    let mut dataset = store::load()?;
    let user = auto_user();

    let Some(name) = resolve_text(
        name,
        "project or experience name (e.g. \"aarg\", \"OSS: serde\")",
        user.as_ref(),
    )
    .await?
    else {
        eprintln!("{}", style::dim("no name given · nothing added"));
        return Ok(());
    };

    // Summary: the flag, or a prompt when a person is driving. A scripted
    // run that gave no --summary records an empty one rather than failing.
    let summary = match summary {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ if user.is_interactive() => match user
            .ask(Question::Text {
                prompt: "one line: what was it?".into(),
            })
            .await?
        {
            Answer::Text(t) => t.trim().to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    };

    let url = url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());

    // Which recorded skills this demonstrates: explicit `--skill` names win
    // (scriptable); otherwise offer a picker when a person is driving.
    let linked = if !skill_flags.is_empty() {
        resolve_skill_names(&dataset, &skill_flags, user.as_ref())
    } else if user.is_interactive() && !dataset.skills.skills.is_empty() {
        pick_skills(&dataset, user.as_ref()).await?
    } else {
        Vec::new()
    };

    let id = next_project_id(&dataset);
    dataset.projects.push(Project {
        id: id.clone(),
        name: name.clone(),
        summary,
        url,
        skill_ids: linked.clone(),
    });
    attach_project_evidence(&mut dataset, &id, &linked);

    dataset.metadata.updated_at = chrono::Utc::now();
    store::save(&dataset)?;
    eprintln!(
        "{}",
        style::success(format!(
            "recorded {name} ({}) {}",
            id.0,
            style::dim(format!(
                "· backs {} skill(s) · dataset saved (previous version backed up)",
                linked.len()
            ))
        ))
    );
    Ok(())
}

/// `aarg experience list` — show the recorded projects / non-job experience.
pub async fn list() -> Result<(), CliError> {
    let dataset = store::load()?;
    if dataset.projects.is_empty() {
        eprintln!(
            "{}",
            style::info("no projects recorded · add one with `aarg experience add`")
        );
        return Ok(());
    }
    eprintln!(
        "{}",
        style::section(format!("Projects ({})", dataset.projects.len()))
    );
    for p in &dataset.projects {
        let url = p
            .url
            .as_deref()
            .map(|u| format!(" · {u}"))
            .unwrap_or_default();
        eprintln!(
            "  {} {}",
            style::bold(format!("{} ({})", p.name, p.id.0)),
            style::dim(format!("· backs {} skill(s){url}", p.skill_ids.len()))
        );
        if !p.summary.is_empty() {
            eprintln!("    {}", style::dim(p.summary.clone()));
        }
    }
    Ok(())
}

/// `aarg experience remove <id>` — drop a project and stop it backing any
/// skill. A skill left with no other evidence becomes unbacked (excluded
/// from tailoring until re-backed), which `dataset validate` reports — the
/// honest consequence of removing the thing that justified it.
pub async fn remove(id: String) -> Result<(), CliError> {
    let mut dataset = store::load()?;
    let pid = ProjectId(id.clone());
    if !dataset.projects.iter().any(|p| p.id == pid) {
        eprintln!(
            "{}",
            style::warn(format!("no project {id:?} · see `aarg experience list`"))
        );
        return Ok(());
    }
    dataset.projects.retain(|p| p.id != pid);
    detach_project_evidence(&mut dataset, &pid);
    dataset.metadata.updated_at = chrono::Utc::now();
    store::save(&dataset)?;
    eprintln!(
        "{}",
        style::success(format!(
            "removed {id} {}",
            style::dim("· dataset saved (previous version backed up)")
        ))
    );
    Ok(())
}

/// The argument if non-blank, else a prompt; `Ok(None)` means the user gave
/// nothing (an empty prompt cancels). A non-interactive run with no argument
/// surfaces a typed `NotInteractive` error rather than hanging — the
/// scriptable-everywhere contract.
async fn resolve_text(
    arg: Option<String>,
    prompt: &str,
    user: &dyn UserHandle,
) -> Result<Option<String>, CliError> {
    if let Some(v) = arg
        && !v.trim().is_empty()
    {
        return Ok(Some(v.trim().to_string()));
    }
    match user
        .ask(Question::Text {
            prompt: prompt.into(),
        })
        .await?
    {
        Answer::Text(t) if !t.trim().is_empty() => Ok(Some(t.trim().to_string())),
        _ => Ok(None),
    }
}

/// Offer the recorded skills and return the ids the user marks as
/// demonstrated by this project.
async fn pick_skills(
    dataset: &ResumeDataset,
    user: &dyn UserHandle,
) -> Result<Vec<SkillId>, CliError> {
    let names: Vec<String> = dataset
        .skills
        .skills
        .iter()
        .map(|s| s.canonical_name.clone())
        .collect();
    let answer = user
        .ask(Question::MultiSelect {
            prompt: "which of your skills did this demonstrate? (space toggles, enter confirms)"
                .into(),
            options: names,
        })
        .await?;
    let Answer::Choices(indexes) = answer else {
        return Ok(Vec::new());
    };
    Ok(indexes
        .iter()
        .filter_map(|&i| dataset.skills.skills.get(i).map(|s| s.id.clone()))
        .collect())
}

/// Resolve `--skill` names to ids through the alias map (so "k8s" finds
/// "Kubernetes"), warning on any that don't resolve rather than minting
/// one — a project links existing skills, it never invents them.
fn resolve_skill_names(
    dataset: &ResumeDataset,
    names: &[String],
    user: &dyn UserHandle,
) -> Vec<SkillId> {
    let mut ids = Vec::new();
    for name in names {
        match dataset.skills.aliases.get(&name.to_lowercase()) {
            Some(id) if !ids.contains(id) => ids.push(id.clone()),
            Some(_) => {}
            None => user.notify(&format!(
                "no recorded skill matches {name:?} · add it with `aarg skills add` first; skipping"
            )),
        }
    }
    ids
}

/// Attach `project_id` as evidence to each listed skill, skipping any that
/// already cite it.
fn attach_project_evidence(
    dataset: &mut ResumeDataset,
    project_id: &ProjectId,
    skill_ids: &[SkillId],
) {
    let ev = EvidenceRef::Project(project_id.clone());
    for sid in skill_ids {
        if let Some(skill) = dataset.skills.skills.iter_mut().find(|s| &s.id == sid)
            && !skill.evidence.contains(&ev)
        {
            skill.evidence.push(ev.clone());
        }
    }
}

/// Remove every reference to a project's evidence — the mirror of
/// `attach_project_evidence`, run when the project is deleted.
fn detach_project_evidence(dataset: &mut ResumeDataset, project_id: &ProjectId) {
    let ev = EvidenceRef::Project(project_id.clone());
    for skill in &mut dataset.skills.skills {
        skill.evidence.retain(|e| e != &ev);
    }
}

/// The next `project-N` id, continuing the highest already used.
fn next_project_id(dataset: &ResumeDataset) -> ProjectId {
    let highest = dataset
        .projects
        .iter()
        .filter_map(|p| p.id.0.strip_prefix("project-")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    ProjectId(format!("project-{}", highest + 1))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::dataset::types::{Contact, Proficiency, RoleId, Skill, SkillCategory, SkillId};

    fn skill(id: &str) -> Skill {
        Skill {
            id: SkillId(id.into()),
            canonical_name: id.into(),
            aliases: Vec::new(),
            category: SkillCategory::Tool,
            proficiency: Proficiency::Working,
            years: None,
            last_used: None,
            evidence: vec![EvidenceRef::Role(RoleId("role-1".into()))],
            verified: false,
            verified_at: None,
        }
    }

    fn dataset() -> ResumeDataset {
        let mut d = ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        d.skills.skills = vec![skill("Rust"), skill("Typst")];
        d
    }

    #[test]
    fn project_ids_continue_the_sequence() {
        let mut d = dataset();
        assert_eq!(next_project_id(&d).0, "project-1");
        d.projects.push(Project {
            id: ProjectId("project-4".into()),
            name: "x".into(),
            summary: String::new(),
            url: None,
            skill_ids: Vec::new(),
        });
        assert_eq!(next_project_id(&d).0, "project-5");
    }

    #[test]
    fn linking_a_project_backs_its_skills_without_duplicates() {
        let mut d = dataset();
        let pid = ProjectId("project-1".into());
        let rust = SkillId("Rust".into());
        attach_project_evidence(&mut d, &pid, std::slice::from_ref(&rust));
        // Idempotent: a second attach adds no duplicate.
        attach_project_evidence(&mut d, &pid, std::slice::from_ref(&rust));
        let backed = d.skills.skills.iter().find(|s| s.id == rust).unwrap();
        assert_eq!(
            backed
                .evidence
                .iter()
                .filter(|e| **e == EvidenceRef::Project(pid.clone()))
                .count(),
            1
        );
    }

    #[test]
    fn removing_a_project_detaches_it_as_evidence() {
        let mut d = dataset();
        let pid = ProjectId("project-1".into());
        let rust = SkillId("Rust".into());
        attach_project_evidence(&mut d, &pid, std::slice::from_ref(&rust));
        detach_project_evidence(&mut d, &pid);
        let backed = d.skills.skills.iter().find(|s| s.id == rust).unwrap();
        assert!(
            !backed
                .evidence
                .iter()
                .any(|e| *e == EvidenceRef::Project(pid.clone())),
            "the project should no longer back the skill"
        );
        // Its original role evidence is untouched.
        assert!(
            backed
                .evidence
                .contains(&EvidenceRef::Role(RoleId("role-1".into())))
        );
    }
}
