//! `aarg experience add|import|list|remove` — record projects, open-source,
//! or other experience that isn't a job, and link the skills it demonstrates.
//!
//! Roles are the spine of a resume, but plenty of real evidence lives
//! outside them: a side project, an open-source contribution, founding
//! something. The dataset has always modeled this as `Project`, and
//! tailoring already renders projects and counts them as skill evidence
//! (`EvidenceRef::Project`) — there was just no way to add one unless it
//! appeared on an ingested resume. This is that way. Thin glue: load,
//! interview (or take flags for a scripted run), save once.
//!
//! Never-fabricate holds the same as everywhere else, though `add` and
//! `import` draw the line in different places. `add` only ever links a
//! project as evidence to a skill the user already recorded; it mints no
//! skills and invents no claims. `import` (`repoimport.rs`) reads a real
//! project (a local folder or a GitHub repo/profile) and proposes new
//! skills an LLM found evidence for there, but never adds one without the
//! user confirming it first — the model proposes, the person decides.

use crate::agent::{Agent, AgentContext, ModelTier};
use crate::commands::{CliError, configured_client};
use crate::dataset::store;
use crate::dataset::types::{EvidenceRef, Project, ProjectId, ResumeDataset, SkillId};
use crate::repoimport::{self, ImportSummary, RepoMaterial, SourceKind};
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

/// `aarg experience import <source>` — read a real project and propose the
/// skills it demonstrates. The source auto-detects: a github.com/owner/repo URL
/// is shallow-cloned, a github.com/owner profile URL lists its public repos for
/// the user to pick from, and anything else is read as a local folder. Unlike
/// `ingest`, nothing lands in the dataset until the user confirms which
/// newly-proposed skills to add — minting a skill is the one new claim here, so
/// it is always gated. Existing skills the project also demonstrates are linked
/// as evidence with no ceremony. The dataset is saved once, at the end.
pub async fn import(source: String) -> Result<(), CliError> {
    let user = auto_user();
    match repoimport::detect_source(&source) {
        SourceKind::Local(path) => {
            let name = folder_name(&path);
            match repoimport::read_repo(&path, &name, None)? {
                Some(material) => run_import(vec![material], user.as_ref()).await,
                None => {
                    eprintln!(
                        "{}",
                        style::info(format!(
                            "found nothing to analyze in {} · nothing added",
                            path.display()
                        ))
                    );
                    Ok(())
                }
            }
        }
        SourceKind::Repo { owner, repo } => {
            eprintln!(
                "{}",
                style::dim(format!("cloning github.com/{owner}/{repo}"))
            );
            match repoimport::fetch_repo_material(&owner, &repo)? {
                Some(material) => run_import(vec![material], user.as_ref()).await,
                None => {
                    eprintln!(
                        "{}",
                        style::info(format!(
                            "found nothing to analyze in {repo} · nothing added"
                        ))
                    );
                    Ok(())
                }
            }
        }
        SourceKind::Profile { user: gh_user } => import_profile(gh_user, user.as_ref()).await,
    }
}

/// List a GitHub profile's public repos, let the user pick which to import,
/// and clone and analyze each chosen one. The pick needs a terminal: a
/// non-interactive run surfaces a typed "needs a terminal" error naming the
/// prompt, rather than guessing which repos to pull.
async fn import_profile(gh_user: String, user: &dyn UserHandle) -> Result<(), CliError> {
    eprintln!(
        "{}",
        style::dim(format!("listing public repos for github.com/{gh_user}"))
    );
    let repos = repoimport::fetch_profile_repos(&gh_user).await?;
    if repos.is_empty() {
        eprintln!(
            "{}",
            style::info(format!(
                "no public repos found for {gh_user} · nothing added"
            ))
        );
        return Ok(());
    }

    let options: Vec<String> = repos.iter().map(repoimport::RepoRef::label).collect();
    let answer = user
        .ask(Question::MultiSelect {
            prompt: "which repos should I import? (space toggles, enter confirms)".into(),
            options,
        })
        .await?;
    let Answer::Choices(indexes) = answer else {
        eprintln!("{}", style::info("nothing selected · nothing added"));
        return Ok(());
    };
    if indexes.is_empty() {
        eprintln!("{}", style::info("nothing selected · nothing added"));
        return Ok(());
    }

    let mut materials = Vec::new();
    for index in indexes {
        let Some(repo) = repos.get(index) else {
            continue;
        };
        eprintln!(
            "{}",
            style::dim(format!("cloning github.com/{}/{}", repo.owner, repo.name))
        );
        match repoimport::fetch_repo_material(&repo.owner, &repo.name)? {
            Some(material) => materials.push(material),
            None => eprintln!(
                "{}",
                style::info(format!(
                    "found nothing to analyze in {} · skipping",
                    repo.name
                ))
            ),
        }
    }
    run_import(materials, user).await
}

/// Analyze each gathered project, run the confirm step, and save once. Loads
/// the dataset a single time and mutates it in memory across every project, so
/// a multi-repo import is one atomic save rather than a write per project.
async fn run_import(materials: Vec<RepoMaterial>, user: &dyn UserHandle) -> Result<(), CliError> {
    if materials.is_empty() {
        eprintln!("{}", style::info("nothing to import · nothing added"));
        return Ok(());
    }

    let (client, config) = configured_client().await?;
    let tracer = super::default_tracer()?;
    let ctx = AgentContext {
        llm: &*client,
        model: config.active_resolver(),
        tracer: &tracer,
        sink: None,
    };

    let mut dataset = store::load()?;
    let mut summaries = Vec::new();
    for material in materials {
        let name = material.name.clone();
        let url = material.url.clone();
        eprintln!(
            "{}",
            style::info(format!(
                "analyzing {name} with {}",
                ctx.model.resolve("project_analysis_v1", ModelTier::Cheap)
            ))
        );
        let analysis = repoimport::ProjectAnalysisAgent
            .run(&ctx, material)
            .await?
            .output;

        // Show what was found before anything is written.
        eprintln!(
            "{}",
            style::section(format!("{} · proposed", analysis.name))
        );
        if !analysis.summary.is_empty() {
            eprintln!("  {}", style::dim(&analysis.summary));
        }
        if analysis.skills.is_empty() {
            eprintln!(
                "  {}",
                style::dim("no skills could be grounded in the material")
            );
        } else {
            for skill in &analysis.skills {
                let reason = if skill.reason.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", skill.reason)
                };
                eprintln!("  {}", style::bullet(format!("{}{reason}", skill.name)));
            }
        }

        let summary = repoimport::apply_import(&mut dataset, analysis, url, user).await?;
        summaries.push(summary);
    }

    dataset.metadata.updated_at = chrono::Utc::now();
    store::save(&dataset)?;

    let mut any_skipped = false;
    for summary in &summaries {
        any_skipped |= !summary.skipped_new.is_empty();
        report_summary(summary);
    }
    if any_skipped && !user.is_interactive() {
        eprintln!(
            "{}",
            style::suggest(
                "new skills were not added because no terminal was available · re-run interactively to add them"
            )
        );
    }
    eprintln!(
        "{}",
        style::success(format!(
            "dataset saved (previous version backed up) · {} project(s) recorded",
            summaries.len()
        ))
    );
    Ok(())
}

/// One line summarizing what an import wrote for a project.
fn report_summary(summary: &ImportSummary) {
    let mut parts = Vec::new();
    if !summary.minted.is_empty() {
        parts.push(format!(
            "minted {} new: {}",
            summary.minted.len(),
            summary.minted.join(", ")
        ));
    }
    if !summary.linked.is_empty() {
        parts.push(format!(
            "linked {} existing: {}",
            summary.linked.len(),
            summary.linked.join(", ")
        ));
    }
    if !summary.skipped_new.is_empty() {
        parts.push(format!(
            "skipped {} new: {}",
            summary.skipped_new.len(),
            summary.skipped_new.join(", ")
        ));
    }
    let detail = if parts.is_empty() {
        "no skills attached".to_string()
    } else {
        parts.join(" · ")
    };
    eprintln!(
        "  {}",
        style::done(format!(
            "{} ({}) · {detail}",
            summary.project_name, summary.project_id.0
        ))
    );
}

/// A display name for a local folder: its final path component, resolving `.`
/// and relative paths to the real directory name, falling back to "project".
fn folder_name(path: &std::path::Path) -> String {
    path.canonicalize()
        .ok()
        .as_deref()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .or_else(|| path.file_name().map(|n| n.to_string_lossy().to_string()))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "project".to_string())
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
