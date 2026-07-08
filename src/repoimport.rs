//! `aarg experience import <source>` core: read a real project and turn what
//! it concretely ships with into a proposed `Project` plus the skills it
//! demonstrates.
//!
//! Three source kinds resolve through one command: a local folder, a single
//! GitHub repo URL (shallow-cloned to a scratch dir), or a GitHub profile URL
//! (its public repos listed over the REST API, then the user picks which to
//! import). Whichever the source, the material read from disk is the same and
//! deliberately bounded:
//!
//! - the README, for the project's own description of what it does;
//! - the dependency manifests present (`Cargo.toml`, `package.json`, ...),
//!   read for their dependency lists as concrete technology evidence;
//! - a language breakdown by file extension, as evidence of what the code is
//!   actually written in, distinct from what a README claims.
//!
//! We never feed whole source files to the model: the manifest + README +
//! language signal is enough to name technologies honestly and keeps the
//! prompt cheap.
//!
//! This is the SECOND place in the codebase allowed to mint a new `Skill`
//! (ingest is the first). Minting is the one capability here that invents a
//! dataset entity, so it is gated: `apply_import` shows the user every
//! proposed skill with the concrete reason it was proposed, and only mints the
//! ones they confirm. A minted skill is born already backed by this project
//! (`EvidenceRef::Project`), so it satisfies the never-fabricate invariant the
//! same way ingest's assembly does. A skill the project only *links* (one
//! already in the dataset) needs no confirmation: linking recorded evidence to
//! an existing skill invents nothing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{Agent, ModelTier};
use crate::dataset::types::{
    EvidenceRef, Proficiency, Project, ProjectId, ResumeDataset, Skill, SkillCategory, SkillId,
};
use crate::llm::LlmError;
use crate::user::{Answer, AskError, Question, UserHandle};

/// The README, manifests, and language breakdown fit comfortably here.
const REPLY_BUDGET: u32 = 2048;

/// How much of a README to hand the model: enough to describe the project,
/// bounded so a book-length README can't blow up the prompt.
const README_CAP: usize = 6000;

/// How many dependency names to keep per manifest — a huge lockfile-style list
/// is noise past the first few dozen.
const MAX_DEPS: usize = 60;

/// How many languages to report, most files first.
const TOP_LANGUAGES: usize = 8;

/// A ceiling on the tree walk so a pathological checkout can't spin forever.
const MAX_FILES_WALKED: usize = 50_000;

/// Everything that can go wrong importing experience from a source.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error(transparent)]
    Llm(#[from] LlmError),

    #[error("the analysis reply was not the expected JSON (reply began {snippet:?})")]
    BadReply {
        snippet: String,
        #[source]
        source: serde_json::Error,
    },

    #[error(
        "the `git` binary was not found on PATH - aarg shells out to it to clone a repo; install git, or pass a local folder path instead"
    )]
    GitMissing,

    #[error("could not run git")]
    GitSpawn(#[source] std::io::Error),

    #[error("git could not clone {url}:\n{stderr}")]
    CloneFailed { url: String, stderr: String },

    #[error("could not read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not create a scratch directory for the clone")]
    Scratch(#[source] std::io::Error),

    #[error("could not build the HTTP client for reaching GitHub")]
    Client {
        #[source]
        source: reqwest::Error,
    },

    #[error("could not reach the GitHub API at {url}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error(
        "GitHub answered HTTP {status} for {url} - the user may not exist, or the API rate limit was hit (it is low for unauthenticated requests)"
    )]
    Api { status: u16, url: String },

    #[error("could not make sense of GitHub's response for {url}")]
    ParseApi {
        url: String,
        #[source]
        source: serde_json::Error,
    },
}

// ---------------------------------------------------------------------
// Source detection
// ---------------------------------------------------------------------

/// What the `<source>` argument resolves to. A GitHub URL with an owner and a
/// repo is one repo; a GitHub URL with only an owner is a profile to browse;
/// anything else is treated as a local folder path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// A single GitHub repository to shallow-clone.
    Repo { owner: String, repo: String },
    /// A GitHub user/org whose public repos we list for the user to pick from.
    Profile { user: String },
    /// A folder already on disk; read directly, no network.
    Local(PathBuf),
}

/// Classify the `<source>` argument. GitHub URLs (with or without a scheme,
/// with or without `www.`, and the `git@github.com:owner/repo.git` SSH form)
/// resolve to a repo or a profile; everything else is a local path — including
/// something that merely looks path-like, since that is exactly the folder case.
///
/// One rough edge this leaves: a URL-shaped source naming some other host
/// (`https://gitlab.com/user/repo`) falls through to `SourceKind::Local` too,
/// which reads as a bogus path and surfaces as a confusing "found nothing to
/// analyze" rather than naming the host as unsupported.
// EXERCISE(EX-020)
pub fn detect_source(input: &str) -> SourceKind {
    let trimmed = input.trim();
    parse_github(trimmed).unwrap_or_else(|| SourceKind::Local(PathBuf::from(trimmed)))
}

/// Recognize the GitHub URL shapes we accept and split off owner (+ repo).
/// Returns `None` for anything that isn't a github.com URL, so the caller
/// falls back to treating it as a local path.
fn parse_github(input: &str) -> Option<SourceKind> {
    // The SSH remote form: git@github.com:owner/repo(.git).
    let host_rest = if let Some(ssh) = input.strip_prefix("git@github.com:") {
        ssh
    } else {
        let no_scheme = input
            .strip_prefix("https://")
            .or_else(|| input.strip_prefix("http://"))
            .unwrap_or(input);
        let no_www = no_scheme.strip_prefix("www.").unwrap_or(no_scheme);
        no_www.strip_prefix("github.com/")?
    };

    // Drop any query string or fragment before splitting into path segments.
    let path = host_rest.split(['?', '#']).next()?;
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let owner = segments.next()?.to_string();
    match segments.next() {
        Some(repo) => {
            let repo = repo.strip_suffix(".git").unwrap_or(repo).to_string();
            Some(SourceKind::Repo { owner, repo })
        }
        None => Some(SourceKind::Profile { user: owner }),
    }
}

/// The HTTPS clone URL for an owner/repo pair.
pub fn clone_url(owner: &str, repo: &str) -> String {
    format!("https://github.com/{owner}/{repo}.git")
}

// ---------------------------------------------------------------------
// The material read from a repo
// ---------------------------------------------------------------------

/// One dependency manifest and the dependency names read from it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Manifest {
    /// The manifest's filename, e.g. "Cargo.toml".
    pub file: String,
    /// Dependency names read from it (bounded; empty if none parsed).
    pub dependencies: Vec<String>,
}

/// One language and how many files carry its extension.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LanguageCount {
    pub language: String,
    pub files: usize,
}

/// Everything the analysis agent gets to see about one project: bounded,
/// concrete, and read straight off disk. `Serialize` because the agent runtime
/// records its input in the trace.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepoMaterial {
    /// The project name to fall back to (the folder or repo name).
    pub name: String,
    /// The source URL, when there is one (a clone or profile import).
    pub url: Option<String>,
    /// The README's text, truncated to `README_CAP`; `None` if none was found.
    pub readme: Option<String>,
    pub manifests: Vec<Manifest>,
    pub languages: Vec<LanguageCount>,
}

/// Read the bounded material from a project directory. Returns `Ok(None)` when
/// the directory has nothing to analyze at all — no README, no manifest, and no
/// recognizable source files — which is an empty or non-code folder, not a bug.
pub fn read_repo(
    root: &Path,
    name: &str,
    url: Option<String>,
) -> Result<Option<RepoMaterial>, ImportError> {
    let readme = read_readme(root);
    let manifests = read_manifests(root)?;
    let languages = language_breakdown(root)?;

    if readme.is_none() && manifests.is_empty() && languages.is_empty() {
        return Ok(None);
    }
    Ok(Some(RepoMaterial {
        name: name.to_string(),
        url,
        readme,
        manifests,
        languages,
    }))
}

/// Read the first root-level file whose name looks like a README (any case,
/// any extension). Best-effort: an unreadable README is treated as absent
/// rather than failing the whole import over one file.
fn read_readme(root: &Path) -> Option<String> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.to_lowercase().starts_with("readme") && entry.path().is_file() {
            let text = std::fs::read_to_string(entry.path()).ok()?;
            return Some(truncate(&text, README_CAP));
        }
    }
    None
}

/// Read every known manifest present at the repo root and parse its dependency
/// names. A manifest that parses to no dependencies is still returned — its
/// mere presence is evidence (a `go.mod` means Go), which the model can use.
fn read_manifests(root: &Path) -> Result<Vec<Manifest>, ImportError> {
    let mut manifests = Vec::new();
    for (file, parse) in MANIFEST_PARSERS {
        let path = root.join(file);
        if !path.is_file() {
            continue;
        }
        let content = std::fs::read_to_string(&path).map_err(|source| ImportError::Read {
            path: path.clone(),
            source,
        })?;
        let mut dependencies = parse(&content);
        dependencies.truncate(MAX_DEPS);
        manifests.push(Manifest {
            file: (*file).to_string(),
            dependencies,
        });
    }
    Ok(manifests)
}

/// Reads dependency names out of one manifest's text.
type ManifestParser = fn(&str) -> Vec<String>;

/// The manifest filenames aarg reads, each paired with its dependency-name
/// parser. Order is the display order in the prompt.
const MANIFEST_PARSERS: &[(&str, ManifestParser)] = &[
    ("Cargo.toml", parse_cargo_toml),
    ("package.json", parse_package_json),
    ("requirements.txt", parse_requirements_txt),
    ("pyproject.toml", parse_pyproject_toml),
    ("go.mod", parse_go_mod),
    ("Gemfile", parse_gemfile),
];

/// Dependency names from a `Cargo.toml`'s `[dependencies]` and
/// `[build-dependencies]` tables. `[dev-dependencies]` is skipped: test-only
/// crates aren't what the project ships.
fn parse_cargo_toml(content: &str) -> Vec<String> {
    let Ok(value) = toml::from_str::<toml::Value>(content) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for table in ["dependencies", "build-dependencies"] {
        if let Some(deps) = value.get(table).and_then(toml::Value::as_table) {
            names.extend(deps.keys().cloned());
        }
    }
    names
}

/// Dependency names from a `package.json`'s `dependencies` and
/// `devDependencies` (dev tooling like a bundler or TypeScript is genuine tech
/// evidence, unlike a Rust dev-dependency).
fn parse_package_json(content: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for table in ["dependencies", "devDependencies"] {
        if let Some(deps) = value.get(table).and_then(serde_json::Value::as_object) {
            names.extend(deps.keys().cloned());
        }
    }
    names
}

/// Package names from a `requirements.txt`: the leading name of each non-blank,
/// non-comment, non-flag line, cut before any version or extras marker.
fn parse_requirements_txt(content: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        let name = line
            .split(|c: char| "=<>!~;[ \t".contains(c))
            .next()
            .unwrap_or("")
            .trim();
        if !name.is_empty() {
            names.push(name.to_string());
        }
    }
    names
}

/// Package names from a `pyproject.toml`: PEP 621 `[project].dependencies`
/// (each a PEP 508 spec whose leading name we keep) plus any Poetry
/// `[tool.poetry.dependencies]` table keys (dropping the `python` pin).
fn parse_pyproject_toml(content: &str) -> Vec<String> {
    let Ok(value) = toml::from_str::<toml::Value>(content) else {
        return Vec::new();
    };
    let mut names = Vec::new();

    if let Some(deps) = value
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(toml::Value::as_array)
    {
        for spec in deps.iter().filter_map(toml::Value::as_str) {
            let name = spec
                .split(|c: char| "=<>!~;[ \t".contains(c))
                .next()
                .unwrap_or("")
                .trim();
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }
    }

    if let Some(deps) = value
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(toml::Value::as_table)
    {
        names.extend(deps.keys().filter(|k| *k != "python").cloned());
    }
    names
}

/// Module paths a `go.mod` requires, from both the single-line `require x v`
/// form and the `require ( ... )` block.
fn parse_go_mod(content: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        let line = line.trim();
        if in_block {
            if line.starts_with(')') {
                in_block = false;
                continue;
            }
            if let Some(path) = line.split_whitespace().next()
                && !path.is_empty()
            {
                names.push(path.to_string());
            }
        } else if line == "require (" {
            in_block = true;
        } else if let Some(rest) = line.strip_prefix("require ")
            && let Some(path) = rest.split_whitespace().next()
        {
            names.push(path.to_string());
        }
    }
    names
}

/// Gem names from a `Gemfile`: the quoted first argument of each `gem` line.
fn parse_gemfile(content: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("gem ") else {
            continue;
        };
        let rest = rest.trim_start();
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
        if let Some(quote) = quote
            && let Some(name) = rest[1..].split(quote).next()
            && !name.is_empty()
        {
            names.push(name.to_string());
        }
    }
    names
}

/// Count files by language across the tree, skipping the usual build- and
/// dependency-output directories (and every dotfile directory), and return the
/// most-common `TOP_LANGUAGES` languages, most files first.
fn language_breakdown(root: &Path) -> Result<Vec<LanguageCount>, ImportError> {
    use std::collections::HashMap;

    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    let mut walked = 0usize;
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // A directory we can't read is skipped, not fatal — a stray
            // permission problem shouldn't sink a whole import.
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if file_type.is_dir() {
                if is_noise_dir(&name) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() {
                walked += 1;
                if walked > MAX_FILES_WALKED {
                    return Ok(top_languages(&counts));
                }
                if let Some(ext) = path.extension().and_then(|e| e.to_str())
                    && let Some(language) = language_for_ext(&ext.to_lowercase())
                {
                    *counts.entry(language).or_insert(0) += 1;
                }
            }
        }
    }
    Ok(top_languages(&counts))
}

/// Sort a language-count map into a bounded, deterministic top-N list.
fn top_languages(counts: &std::collections::HashMap<&'static str, usize>) -> Vec<LanguageCount> {
    let mut list: Vec<LanguageCount> = counts
        .iter()
        .map(|(language, files)| LanguageCount {
            language: (*language).to_string(),
            files: *files,
        })
        .collect();
    // Most files first; ties broken by name so the order is stable.
    list.sort_by(|a, b| {
        b.files
            .cmp(&a.files)
            .then_with(|| a.language.cmp(&b.language))
    });
    list.truncate(TOP_LANGUAGES);
    list
}

/// Directories whose contents are build output or vendored dependencies, not
/// the project's own code. Any dotfile directory (`.git`, `.venv`, ...) is also
/// skipped by the caller.
fn is_noise_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "node_modules"
                | "target"
                | "dist"
                | "build"
                | "out"
                | "vendor"
                | "venv"
                | "__pycache__"
                | "coverage"
        )
}

/// Map a lowercased file extension to a language label, or `None` for
/// extensions that aren't a programming language (data, docs, lockfiles).
fn language_for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "Rust",
        "ts" | "mts" | "cts" => "TypeScript",
        "tsx" => "TypeScript (JSX)",
        "js" | "mjs" | "cjs" => "JavaScript",
        "jsx" => "JavaScript (JSX)",
        "py" => "Python",
        "go" => "Go",
        "rb" => "Ruby",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "C++",
        "cs" => "C#",
        "php" => "PHP",
        "swift" => "Swift",
        "scala" => "Scala",
        "rs.in" => "Rust",
        "sh" | "bash" | "zsh" => "Shell",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" | "sass" => "Sass",
        "sql" => "SQL",
        "ex" | "exs" => "Elixir",
        "erl" => "Erlang",
        "hs" => "Haskell",
        "clj" | "cljs" => "Clojure",
        "dart" => "Dart",
        "lua" => "Lua",
        "r" => "R",
        "m" => "Objective-C",
        "typ" => "Typst",
        _ => return None,
    })
}

/// Truncate `text` to at most `max` characters, on a character boundary,
/// appending a marker when it was cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}\n[... README truncated ...]")
}

// ---------------------------------------------------------------------
// The analysis agent
// ---------------------------------------------------------------------

/// One skill the project analysis proposes, tagged with the concrete evidence
/// that grounds it so the confirm step can show the user why.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposedSkill {
    pub name: String,
    /// Why it was proposed, e.g. "sqlx dependency" or "42 .rs files".
    pub reason: String,
    pub category: SkillCategory,
}

/// What the analysis produced: a project name and summary, plus the proposed
/// skills. Nothing here is written to the dataset until `apply_import` runs the
/// confirm step.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectAnalysis {
    pub name: String,
    pub summary: String,
    pub skills: Vec<ProposedSkill>,
}

/// Reads a project's bounded material and proposes a name, summary, and skills.
/// Cheap tier: this is pattern-matching manifests and READMEs against known
/// technology names, the same "structured parsing, not judgment" as ingest,
/// not deep interpretation.
pub struct ProjectAnalysisAgent;

#[async_trait]
impl Agent for ProjectAnalysisAgent {
    type Input = RepoMaterial;
    type Wire = RawAnalysis;
    type Output = ProjectAnalysis;
    type Error = ImportError;

    fn id(&self) -> &'static str {
        "project_analysis_v1"
    }
    fn model_tier(&self) -> ModelTier {
        ModelTier::Cheap
    }
    fn system_prompt(&self) -> &str {
        SYSTEM_PROMPT
    }
    fn reply_budget(&self) -> u32 {
        REPLY_BUDGET
    }
    fn user_message(&self, input: &RepoMaterial) -> String {
        render_material(input)
    }
    fn bad_reply(&self, snippet: String, source: serde_json::Error) -> ImportError {
        ImportError::BadReply { snippet, source }
    }
    fn assemble(
        &self,
        wire: RawAnalysis,
        input: RepoMaterial,
    ) -> Result<ProjectAnalysis, ImportError> {
        Ok(assemble_analysis(wire, &input))
    }
}

/// Render the bounded material into the user message. Kept a free function so
/// tests can assert exactly what the model is (and isn't) shown.
fn render_material(m: &RepoMaterial) -> String {
    let mut text = format!("Project directory name: {}\n", m.name);
    if let Some(url) = &m.url {
        text.push_str(&format!("Source URL: {url}\n"));
    }
    text.push('\n');

    match &m.readme {
        Some(readme) => {
            text.push_str("README (verbatim, possibly truncated):\n");
            text.push_str(readme);
            text.push_str("\n\n");
        }
        None => text.push_str("No README was found.\n\n"),
    }

    if m.manifests.is_empty() {
        text.push_str("No dependency manifests were found.\n\n");
    } else {
        text.push_str("Dependency manifests found:\n");
        for manifest in &m.manifests {
            let deps = if manifest.dependencies.is_empty() {
                "(no dependencies parsed)".to_string()
            } else {
                manifest.dependencies.join(", ")
            };
            text.push_str(&format!("- {} lists: {}\n", manifest.file, deps));
        }
        text.push('\n');
    }

    if m.languages.is_empty() {
        text.push_str("No recognizable source files were found.\n");
    } else {
        text.push_str("Language breakdown by file count:\n");
        for language in &m.languages {
            text.push_str(&format!(
                "- {}: {} file(s)\n",
                language.language, language.files
            ));
        }
    }
    text
}

const SYSTEM_PROMPT: &str = r#"You analyze one software project from the concrete material it ships with: its README, its dependency manifests, and a breakdown of the languages its files are written in. From that material you propose a short project name, a one or two sentence summary of what the project does, and the skills the project demonstrates.

Rules, all of them binding:
- Use only what the material actually shows. Never name a technology, framework, language, achievement, or scope that the README, a manifest, or the language breakdown does not evidence. If the material is thin, propose fewer skills, not invented ones.
- Not all evidence carries the same weight. A dependency manifest entry and the language breakdown are primary evidence: structural facts about the codebase itself, hard to fake. The README is the project's own description of itself, which is secondary evidence: its prose can overstate ("production-grade", "enterprise-ready") or claim a technology the manifests and language breakdown never back.
- Every proposed skill carries a short "reason" naming the concrete evidence for it: a specific dependency ("sqlx dependency", "reads as the Postgres driver"), the language breakdown ("42 .rs files"), or the README's own words. A skill you cannot ground in the material this way must not appear at all.
- When a skill rests only on the README, with nothing in the manifests or language breakdown backing it, say so plainly in the reason instead of stating it with manifest-level confidence, e.g. "README states this; not found in dependencies". When a manifest or the language breakdown corroborates a README claim, cite that corroboration instead - it is now primary evidence.
- Propose a skill for a real language or technology, not for a generic soft skill the material cannot show. "Rust" grounded in .rs files is good; "teamwork" is not something a repo evidences.
- The summary states only what the README and manifests say the project does. Do not inflate its scale, its users, or its outcomes.
- category is one of "hard", "soft", "domain", "tool", "language", or "framework".
- Do not use em-dashes anywhere in your reply.
- Reply with exactly one JSON object and nothing else: no markdown fences, no commentary.

The JSON object:
{"name": "short project name", "summary": "one or two sentences", "skills": [{"name": "Rust", "reason": "language breakdown: 42 .rs files", "category": "language"}]}"#;

/// The lenient wire shape the model replies with.
#[derive(Debug, Deserialize)]
pub struct RawAnalysis {
    #[serde(default)]
    name: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    skills: Vec<RawProposedSkill>,
}

#[derive(Debug, Deserialize)]
struct RawProposedSkill {
    name: String,
    #[serde(default)]
    reason: String,
    #[serde(default = "default_category")]
    category: SkillCategory,
}

fn default_category() -> SkillCategory {
    SkillCategory::Hard
}

/// Turn the wire reply into the typed analysis: fall back to the folder name,
/// drop nameless skills, and dedupe by lowercased name so the same technology
/// isn't proposed twice.
fn assemble_analysis(wire: RawAnalysis, input: &RepoMaterial) -> ProjectAnalysis {
    let name = if wire.name.trim().is_empty() {
        input.name.clone()
    } else {
        wire.name.trim().to_string()
    };

    let mut seen: Vec<String> = Vec::new();
    let mut skills = Vec::new();
    for raw in wire.skills {
        let name = raw.name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let key = name.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        skills.push(ProposedSkill {
            name,
            reason: raw.reason.trim().to_string(),
            category: raw.category,
        });
    }

    ProjectAnalysis {
        name,
        summary: wire.summary.unwrap_or_default().trim().to_string(),
        skills,
    }
}

// ---------------------------------------------------------------------
// The confirm step: the honesty gate
// ---------------------------------------------------------------------

/// What one import wrote to the dataset, for the command to report.
#[derive(Debug, PartialEq)]
pub struct ImportSummary {
    pub project_id: ProjectId,
    pub project_name: String,
    /// Canonical names of skills newly minted (backed by this project).
    pub minted: Vec<String>,
    /// Names of already-recorded skills this project now backs.
    pub linked: Vec<String>,
    /// Names of proposed-new skills the user (or a non-interactive run) declined.
    pub skipped_new: Vec<String>,
}

/// Record the analyzed project, minting only the new skills the user confirms.
///
/// The dataset is mutated in place and the caller saves once, on success. The
/// design point: a proposed skill that already exists is *linked* as evidence
/// with no ceremony (it invents nothing); a proposed skill that doesn't exist
/// is *minted* only after the user confirms it, because minting a skill is the
/// one thing here that adds a new claim. A non-interactive run mints nothing
/// (see `confirm_new_skills`), so a piped invocation can never silently grow
/// the skill graph.
pub async fn apply_import(
    dataset: &mut ResumeDataset,
    analysis: ProjectAnalysis,
    url: Option<String>,
    user: &dyn UserHandle,
) -> Result<ImportSummary, AskError> {
    // Split the proposals: those that resolve to a recorded skill (link) vs.
    // those that don't (candidates to mint).
    let mut existing: Vec<(SkillId, String)> = Vec::new();
    let mut new_skills: Vec<ProposedSkill> = Vec::new();
    for proposed in &analysis.skills {
        match dataset.skills.aliases.get(&proposed.name.to_lowercase()) {
            Some(id) => {
                if !existing.iter().any(|(existing_id, _)| existing_id == id) {
                    let display = dataset
                        .skills
                        .skills
                        .iter()
                        .find(|s| &s.id == id)
                        .map(|s| s.canonical_name.clone())
                        .unwrap_or_else(|| proposed.name.clone());
                    existing.push((id.clone(), display));
                }
            }
            None => {
                if !new_skills
                    .iter()
                    .any(|n| n.name.eq_ignore_ascii_case(&proposed.name))
                {
                    new_skills.push(proposed.clone());
                }
            }
        }
    }

    // Ask which new skills to mint. Non-interactive runs mint none.
    let chosen = confirm_new_skills(&new_skills, user).await?;
    let skipped_new: Vec<String> = new_skills
        .iter()
        .enumerate()
        .filter(|(index, _)| !chosen.contains(index))
        .map(|(_, s)| s.name.clone())
        .collect();

    // Everything is decided; now write. The project id is fixed first so
    // minted skills can cite it as their evidence.
    let project_id = next_project_id(dataset);
    let mut skill_ids: Vec<SkillId> = existing.iter().map(|(id, _)| id.clone()).collect();
    let mut minted = Vec::new();

    for index in &chosen {
        let Some(skill) = new_skills.get(*index) else {
            continue;
        };
        // Recomputed each pass so ids stay unique as the vec grows.
        let id = next_skill_id(dataset);
        dataset.skills.skills.push(Skill {
            id: id.clone(),
            canonical_name: skill.name.clone(),
            aliases: Vec::new(),
            category: skill.category,
            // The same default ingest assigns a skill it can't rank higher.
            proficiency: Proficiency::Working,
            years: None,
            last_used: None,
            // Born backed by this project: never-fabricate holds structurally.
            evidence: vec![EvidenceRef::Project(project_id.clone())],
            verified: false,
            verified_at: None,
        });
        dataset
            .skills
            .aliases
            .insert(skill.name.to_lowercase(), id.clone());
        skill_ids.push(id);
        minted.push(skill.name.clone());
    }

    dataset.projects.push(Project {
        id: project_id.clone(),
        name: analysis.name.clone(),
        summary: analysis.summary,
        url,
        skill_ids,
    });

    // Link the already-recorded skills (minted ones already carry the evidence).
    let existing_ids: Vec<SkillId> = existing.iter().map(|(id, _)| id.clone()).collect();
    attach_project_evidence(dataset, &project_id, &existing_ids);

    Ok(ImportSummary {
        project_id,
        project_name: analysis.name,
        minted,
        linked: existing.into_iter().map(|(_, name)| name).collect(),
        skipped_new,
    })
}

/// Ask the user which of the proposed-new skills to mint, showing each with
/// the concrete reason it was proposed. Returns the indices (into `new_skills`)
/// to mint.
///
/// A non-interactive run mints none: minting is the one capability here that
/// adds a new claim, so it never happens without a person confirming it. The
/// project is still recorded and existing skills still linked, matching the low
/// ceremony of `experience add` in a script — only the new-claim step is held
/// back.
async fn confirm_new_skills(
    new_skills: &[ProposedSkill],
    user: &dyn UserHandle,
) -> Result<Vec<usize>, AskError> {
    if new_skills.is_empty() || !user.is_interactive() {
        return Ok(Vec::new());
    }

    user.notify(&format!(
        "\n{} new skill(s) the project suggests (not yet in your dataset):",
        new_skills.len()
    ));
    for skill in new_skills {
        let reason = if skill.reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", skill.reason)
        };
        user.notify(&format!("  {}{reason}", skill.name));
    }

    let options = vec![
        format!("Add all {} new skills", new_skills.len()),
        "Choose which to add".to_string(),
        "Add none".to_string(),
    ];
    let choice = match user
        .ask(Question::Select {
            prompt: "these are new skills - add them to your dataset?".to_string(),
            options: options.clone(),
        })
        .await?
    {
        Answer::Choice(index) => options.get(index).map(String::as_str),
        _ => Some("Add none"),
    };

    match choice {
        Some(label) if label.starts_with("Add all") => Ok((0..new_skills.len()).collect()),
        Some("Choose which to add") => {
            let labels: Vec<String> = new_skills
                .iter()
                .map(|s| {
                    if s.reason.is_empty() {
                        s.name.clone()
                    } else {
                        format!("{} ({})", s.name, s.reason)
                    }
                })
                .collect();
            match user
                .ask(Question::MultiSelect {
                    prompt: "pick the skills to add (space toggles, enter confirms)".to_string(),
                    options: labels,
                })
                .await?
            {
                Answer::Choices(indexes) => Ok(indexes
                    .into_iter()
                    .filter(|i| *i < new_skills.len())
                    .collect()),
                _ => Ok(Vec::new()),
            }
        }
        _ => Ok(Vec::new()),
    }
}

/// Attach `project_id` as evidence to each listed skill, skipping any that
/// already cite it. Mirrors `commands::experience`'s own linking step — the
/// same low-ceremony operation, since linking an existing skill invents nothing.
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

/// The next `skill-N` id, continuing the highest already used.
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

// ---------------------------------------------------------------------
// The network / subprocess seam
//
// Deliberately thin and separated from everything above so the testable
// logic (reading, analysis, the confirm flow) never has to touch the real
// network or spawn a real process.
// ---------------------------------------------------------------------

/// A scratch directory that deletes itself when dropped, so a clone is cleaned
/// up on every exit path — success, an error mid-read, or a panic — without a
/// caller remembering to. Hand-rolled (rather than pulling `tempfile` into the
/// runtime deps) so the RAII guard is visible in the source.
pub struct ScratchDir {
    path: PathBuf,
}

/// Distinguishes scratch dirs created in the same nanosecond (a profile import
/// clones several back to back).
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

impl ScratchDir {
    /// Create a fresh, empty scratch directory under the system temp dir.
    pub fn new() -> Result<Self, ImportError> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("aarg-import-{}-{nanos}-{seq}", std::process::id()));
        std::fs::create_dir_all(&path).map_err(ImportError::Scratch)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        // Best-effort: a scratch dir we can't remove is a leak in the temp
        // dir, not a failure worth surfacing after the work is done.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Shallow-clone a repo into `dest` by shelling out to the user's own `git`
/// (the same "shell out to a real tool" pattern as rendering via `typst`). A
/// missing `git` fails with an actionable message, not a panic.
fn git_clone(clone_url: &str, dest: &Path) -> Result<(), ImportError> {
    let dest = dest.to_string_lossy().to_string();
    let output = Command::new("git")
        .args(["clone", "--depth", "1", clone_url, &dest])
        .output();
    match output {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ImportError::GitMissing),
        Err(e) => Err(ImportError::GitSpawn(e)),
        Ok(out) if !out.status.success() => Err(ImportError::CloneFailed {
            url: clone_url.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        }),
        Ok(_) => Ok(()),
    }
}

/// Shallow-clone `owner/repo` to a self-cleaning scratch dir and read its
/// bounded material. The `ScratchDir` guard drops at the end of this function,
/// after the material is fully read into memory, so the checkout is gone by the
/// time we return — on the error paths too.
pub fn fetch_repo_material(owner: &str, repo: &str) -> Result<Option<RepoMaterial>, ImportError> {
    let scratch = ScratchDir::new()?;
    let url = clone_url(owner, repo);
    git_clone(&url, scratch.path())?;
    read_repo(
        scratch.path(),
        repo,
        Some(format!("https://github.com/{owner}/{repo}")),
    )
}

/// One public repo from a GitHub profile listing.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
    pub description: Option<String>,
    pub fork: bool,
}

impl RepoRef {
    /// A one-line label for the pick list: name, fork marker, and description.
    pub fn label(&self) -> String {
        let mut label = self.name.clone();
        if self.fork {
            label.push_str(" [fork]");
        }
        if let Some(description) = &self.description
            && !description.is_empty()
        {
            label.push_str(&format!(" - {description}"));
        }
        label
    }
}

/// The slice of GitHub's repo JSON aarg reads.
#[derive(Debug, Deserialize)]
struct GhRepo {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    fork: bool,
    #[serde(default)]
    owner: GhOwner,
}

#[derive(Debug, Default, Deserialize)]
struct GhOwner {
    #[serde(default)]
    login: String,
}

/// Parse GitHub's `/users/<user>/repos` array into `RepoRef`s. Split out from
/// the network call so a fixture body tests the mapping with no HTTP.
fn parse_repo_list(body: &str, url: &str) -> Result<Vec<RepoRef>, ImportError> {
    let repos: Vec<GhRepo> =
        serde_json::from_str(body).map_err(|source| ImportError::ParseApi {
            url: url.to_string(),
            source,
        })?;
    Ok(repos
        .into_iter()
        .map(|r| RepoRef {
            owner: r.owner.login,
            name: r.name,
            description: r.description.filter(|d| !d.is_empty()),
            fork: r.fork,
        })
        .collect())
}

/// List a GitHub user's public repos over the unauthenticated REST API. Public
/// data needs no token; the low anonymous rate limit is surfaced as a typed
/// `Api` error if it bites.
pub async fn fetch_profile_repos(user: &str) -> Result<Vec<RepoRef>, ImportError> {
    let url = format!("https://api.github.com/users/{user}/repos?per_page=100&sort=updated");
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|source| ImportError::Client { source })?;
    let response = http
        .get(&url)
        .header(reqwest::header::USER_AGENT, "aarg")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|source| ImportError::Http {
            url: url.clone(),
            source,
        })?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(ImportError::Api { status, url });
    }
    let body = response.text().await.map_err(|source| ImportError::Http {
        url: url.clone(),
        source,
    })?;
    parse_repo_list(&body, &url)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::agent::AgentContext;
    use crate::dataset::types::Contact;
    use crate::llm::MockLlmClient;
    use crate::trace::Tracer;
    use crate::user::ScriptedUser;

    // ----- source detection -----

    #[test]
    fn repo_urls_detect_as_a_single_repo() {
        for url in [
            "https://github.com/user/repo",
            "http://github.com/user/repo",
            "https://www.github.com/user/repo/",
            "github.com/user/repo",
            "https://github.com/user/repo.git",
            "https://github.com/user/repo?tab=readme",
            "git@github.com:user/repo.git",
        ] {
            assert_eq!(
                detect_source(url),
                SourceKind::Repo {
                    owner: "user".into(),
                    repo: "repo".into()
                },
                "{url:?} should be a repo"
            );
        }
    }

    #[test]
    fn profile_urls_detect_as_a_profile() {
        for url in [
            "https://github.com/user",
            "github.com/user",
            "https://www.github.com/user/",
            "http://github.com/user?tab=repositories",
        ] {
            assert_eq!(
                detect_source(url),
                SourceKind::Profile {
                    user: "user".into()
                },
                "{url:?} should be a profile"
            );
        }
    }

    #[test]
    fn non_github_sources_detect_as_local_paths() {
        for path in [
            "./my-project",
            "/Users/ada/code/engine",
            "engine",
            "../sibling",
            "github.com", // no owner segment: not a GitHub URL we can use
            "https://gitlab.com/user/repo", // a non-github host: see EX-020
        ] {
            assert_eq!(
                detect_source(path),
                SourceKind::Local(PathBuf::from(path)),
                "{path:?} should be local"
            );
        }
    }

    #[test]
    #[ignore = "exercise: a URL-shaped source naming a host other than github.com (e.g. https://gitlab.com/user/repo) currently detects as SourceKind::Local, which reads as a bogus path and reports a confusing \"found nothing to analyze\"; add a way to recognize this shape and return a clear \"unsupported source\" error instead, then finish this test"]
    fn ex_020_an_unsupported_host_is_a_clear_error_not_a_silent_local_path() {
        let unsupported_hosts_get_a_clear_error = false;
        assert!(unsupported_hosts_get_a_clear_error);
    }

    // ----- manifest parsing -----

    #[test]
    fn cargo_toml_deps_are_read_without_dev_deps() {
        let toml = r#"
            [package]
            name = "demo"
            [dependencies]
            serde = "1"
            tokio = { version = "1", features = ["full"] }
            [build-dependencies]
            cc = "1"
            [dev-dependencies]
            tempfile = "3"
        "#;
        let deps = parse_cargo_toml(toml);
        assert!(deps.contains(&"serde".to_string()));
        assert!(deps.contains(&"tokio".to_string()));
        assert!(deps.contains(&"cc".to_string()));
        assert!(!deps.contains(&"tempfile".to_string()), "dev-deps excluded");
    }

    #[test]
    fn package_json_reads_both_dependency_maps() {
        let json = r#"{
            "name": "demo",
            "dependencies": {"react": "^18", "pg": "^8"},
            "devDependencies": {"typescript": "^5"}
        }"#;
        let deps = parse_package_json(json);
        assert!(deps.contains(&"react".to_string()));
        assert!(deps.contains(&"pg".to_string()));
        assert!(deps.contains(&"typescript".to_string()));
    }

    #[test]
    fn requirements_txt_reads_leading_names() {
        let txt = "# comment\ndjango==4.2\nrequests>=2\npsycopg2-binary\n-e .\n\nnumpy~=1.26";
        let deps = parse_requirements_txt(txt);
        assert_eq!(deps, vec!["django", "requests", "psycopg2-binary", "numpy"]);
    }

    #[test]
    fn pyproject_reads_pep621_and_poetry() {
        let pep621 = r#"
            [project]
            dependencies = ["fastapi>=0.100", "sqlalchemy"]
        "#;
        assert_eq!(parse_pyproject_toml(pep621), vec!["fastapi", "sqlalchemy"]);

        let poetry = r#"
            [tool.poetry.dependencies]
            python = "^3.11"
            flask = "^3"
        "#;
        assert_eq!(parse_pyproject_toml(poetry), vec!["flask"]);
    }

    #[test]
    fn go_mod_reads_single_and_block_requires() {
        let go = "module demo\n\ngo 1.22\n\nrequire github.com/gin-gonic/gin v1.9.1\n\nrequire (\n\tgithub.com/lib/pq v1.10.9\n\tgolang.org/x/sync v0.5.0\n)\n";
        let deps = parse_go_mod(go);
        assert!(deps.contains(&"github.com/gin-gonic/gin".to_string()));
        assert!(deps.contains(&"github.com/lib/pq".to_string()));
        assert!(deps.contains(&"golang.org/x/sync".to_string()));
    }

    #[test]
    fn gemfile_reads_quoted_gem_names() {
        let gemfile =
            "source 'https://rubygems.org'\ngem 'rails', '~> 7'\ngem \"pg\"\n# gem 'commented'";
        let deps = parse_gemfile(gemfile);
        assert_eq!(deps, vec!["rails", "pg"]);
    }

    // ----- reading a constructed repo (no network) -----

    /// Build a small on-disk project fixture in a temp dir.
    fn fixture_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("README.md"),
            "# Engine\nA task engine that schedules jobs.",
        )
        .unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"engine\"\n[dependencies]\nsqlx = \"0.7\"\ntokio = \"1\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/lib.rs"), "// lib").unwrap();
        std::fs::write(root.join("build.rs"), "fn main() {}").unwrap();
        // Noise that must be skipped in the language count.
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("target/debug/gen.rs"), "// generated").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config.rs"), "// not real source").unwrap();
        dir
    }

    #[test]
    fn read_repo_gathers_readme_manifest_and_languages() {
        let dir = fixture_repo();
        let material = read_repo(dir.path(), "engine", None).unwrap().unwrap();

        assert!(material.readme.as_deref().unwrap().contains("task engine"));

        let cargo = material
            .manifests
            .iter()
            .find(|m| m.file == "Cargo.toml")
            .unwrap();
        assert!(cargo.dependencies.contains(&"sqlx".to_string()));
        assert!(cargo.dependencies.contains(&"tokio".to_string()));

        // Three .rs files under src/ and build.rs; the two under target/ and
        // .git/ are skipped, so Rust counts exactly 3.
        let rust = material
            .languages
            .iter()
            .find(|l| l.language == "Rust")
            .unwrap();
        assert_eq!(rust.files, 3, "target/ and .git/ are excluded");
    }

    #[test]
    fn an_empty_folder_yields_nothing_to_analyze() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_repo(dir.path(), "empty", None).unwrap(), None);
    }

    // ----- the analysis agent (MockLlmClient) -----

    fn ctx<'a>(mock: &'a MockLlmClient) -> AgentContext<'a> {
        AgentContext {
            llm: mock,
            model: &"m",
            tracer: &Tracer::DISABLED,
            sink: None,
        }
    }

    #[tokio::test]
    async fn analysis_extracts_only_grounded_skills() {
        let dir = fixture_repo();
        let material = read_repo(dir.path(), "engine", None).unwrap().unwrap();

        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"name": "Engine", "summary": "A task engine that schedules jobs.",
                "skills": [
                    {"name": "Rust", "reason": "42 .rs files", "category": "language"},
                    {"name": "PostgreSQL", "reason": "sqlx dependency", "category": "tool"}
                ]}"#,
        );

        let analysis = ProjectAnalysisAgent
            .run(&ctx(&mock), material)
            .await
            .unwrap()
            .output;

        assert_eq!(analysis.name, "Engine");
        assert_eq!(analysis.skills.len(), 2);
        assert_eq!(analysis.skills[0].name, "Rust");
        assert_eq!(analysis.skills[1].reason, "sqlx dependency");

        // The prompt showed the model the concrete material and nothing else:
        // the manifest deps and the README, never raw source file bodies.
        let sent = &mock.requests()[0].messages[0].content;
        assert!(sent.contains("sqlx"));
        assert!(sent.contains("task engine that schedules jobs"));
        assert!(!sent.contains("fn main"), "raw source is not sent");
    }

    #[tokio::test]
    async fn duplicate_and_nameless_skills_are_dropped() {
        let material = RepoMaterial {
            name: "demo".into(),
            url: None,
            readme: Some("readme".into()),
            manifests: Vec::new(),
            languages: Vec::new(),
        };
        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"name": "", "summary": "s",
                "skills": [
                    {"name": "Rust", "reason": "a"},
                    {"name": "rust", "reason": "dupe"},
                    {"name": "  ", "reason": "empty"}
                ]}"#,
        );
        let analysis = ProjectAnalysisAgent
            .run(&ctx(&mock), material)
            .await
            .unwrap()
            .output;
        // Nameless dropped, case-duplicate collapsed, empty name falls back.
        assert_eq!(analysis.name, "demo");
        assert_eq!(analysis.skills.len(), 1);
        assert_eq!(analysis.skills[0].name, "Rust");
    }

    #[tokio::test]
    async fn the_prompt_tiers_manifest_and_readme_evidence() {
        let material = RepoMaterial {
            name: "demo".into(),
            url: None,
            readme: Some("readme".into()),
            manifests: Vec::new(),
            languages: Vec::new(),
        };
        let mock = MockLlmClient::default();
        mock.enqueue(r#"{"name": "demo", "summary": "s", "skills": []}"#);
        ProjectAnalysisAgent
            .run(&ctx(&mock), material)
            .await
            .unwrap();

        // A manifest entry and the language breakdown are structural facts;
        // the README is the project's own, possibly-inflated self-description
        // - the model is told to weigh them differently, not treat all three
        // as equally solid grounds for a skill.
        let requests = mock.requests();
        let system = requests[0].system.as_deref().unwrap();
        assert!(system.contains("primary evidence"));
        assert!(system.contains("README is the project's own description of itself"));
        assert!(system.contains("not found in dependencies"));
    }

    #[tokio::test]
    async fn a_readme_only_skill_names_its_weaker_evidence_distinctly() {
        let dir = fixture_repo();
        let material = read_repo(dir.path(), "engine", None).unwrap().unwrap();

        let mock = MockLlmClient::default();
        mock.enqueue(
            r#"{"name": "Engine", "summary": "A task engine.",
                "skills": [
                    {"name": "PostgreSQL", "reason": "sqlx dependency", "category": "tool"},
                    {"name": "Kubernetes", "reason": "README states this; not found in dependencies", "category": "tool"}
                ]}"#,
        );
        let analysis = ProjectAnalysisAgent
            .run(&ctx(&mock), material)
            .await
            .unwrap()
            .output;

        let manifest_backed = analysis
            .skills
            .iter()
            .find(|s| s.name == "PostgreSQL")
            .unwrap();
        let readme_only = analysis
            .skills
            .iter()
            .find(|s| s.name == "Kubernetes")
            .unwrap();
        // Same shape (both are just a `reason` string), but the wording
        // carries a visibly different evidentiary weight: one names a
        // concrete manifest fact, the other flags itself as unconfirmed.
        assert_eq!(manifest_backed.reason, "sqlx dependency");
        assert_eq!(
            readme_only.reason,
            "README states this; not found in dependencies"
        );
        assert_ne!(manifest_backed.reason, readme_only.reason);
    }

    // ----- the confirm step (ScriptedUser) -----

    fn base_dataset() -> ResumeDataset {
        ResumeDataset::new(Contact {
            full_name: "Ada".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        })
    }

    /// A dataset that already records "Rust" (skill-1), so an import proposing
    /// Rust links it rather than minting a duplicate.
    fn dataset_with_rust() -> ResumeDataset {
        let mut d = base_dataset();
        d.skills.skills.push(Skill {
            id: SkillId("skill-1".into()),
            canonical_name: "Rust".into(),
            aliases: Vec::new(),
            category: SkillCategory::Language,
            proficiency: Proficiency::Working,
            years: None,
            last_used: None,
            evidence: vec![EvidenceRef::Role(crate::dataset::types::RoleId(
                "role-1".into(),
            ))],
            verified: false,
            verified_at: None,
        });
        d.skills
            .aliases
            .insert("rust".into(), SkillId("skill-1".into()));
        d
    }

    fn analysis_with(skills: &[(&str, SkillCategory)]) -> ProjectAnalysis {
        ProjectAnalysis {
            name: "Engine".into(),
            summary: "A task engine.".into(),
            skills: skills
                .iter()
                .map(|(name, category)| ProposedSkill {
                    name: (*name).to_string(),
                    reason: "evidence".into(),
                    category: *category,
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn confirming_some_new_skills_mints_only_those() {
        let mut dataset = dataset_with_rust();
        let analysis = analysis_with(&[
            ("Rust", SkillCategory::Language),   // existing -> linked
            ("PostgreSQL", SkillCategory::Tool), // new -> offered
            ("Docker", SkillCategory::Tool),     // new -> offered
        ]);
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(1)); // "Choose which to add"
        user.answer(Answer::Choices(vec![0])); // only the first new one (PostgreSQL)

        let summary = apply_import(
            &mut dataset,
            analysis,
            Some("https://github.com/ada/engine".into()),
            &user,
        )
        .await
        .unwrap();

        // PostgreSQL minted; Docker skipped; Rust linked, not re-minted.
        assert_eq!(summary.minted, vec!["PostgreSQL"]);
        assert_eq!(summary.skipped_new, vec!["Docker"]);
        assert_eq!(summary.linked, vec!["Rust"]);

        // The dataset has exactly one new skill (skill-2, PostgreSQL), backed
        // by the new project.
        assert_eq!(dataset.skills.skills.len(), 2);
        let pg = dataset
            .skills
            .skills
            .iter()
            .find(|s| s.canonical_name == "PostgreSQL")
            .unwrap();
        assert_eq!(pg.id, SkillId("skill-2".into()));
        assert_eq!(
            pg.evidence,
            vec![EvidenceRef::Project(ProjectId("project-1".into()))]
        );
        // No unbacked skill was created: never-fabricate holds.
        assert!(dataset.skills.skills.iter().all(|s| !s.evidence.is_empty()));

        // The project links both the linked and the minted skill.
        let project = &dataset.projects[0];
        assert!(project.skill_ids.contains(&SkillId("skill-1".into())));
        assert!(project.skill_ids.contains(&SkillId("skill-2".into())));

        // Rust (existing) now also cites the project as evidence.
        let rust = dataset
            .skills
            .skills
            .iter()
            .find(|s| s.canonical_name == "Rust")
            .unwrap();
        assert!(
            rust.evidence
                .contains(&EvidenceRef::Project(ProjectId("project-1".into())))
        );
    }

    #[tokio::test]
    async fn add_all_mints_every_new_skill() {
        let mut dataset = base_dataset();
        let analysis = analysis_with(&[
            ("Rust", SkillCategory::Language),
            ("Typst", SkillCategory::Tool),
        ]);
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(0)); // "Add all N new skills"

        let summary = apply_import(&mut dataset, analysis, None, &user)
            .await
            .unwrap();

        assert_eq!(summary.minted, vec!["Rust", "Typst"]);
        assert!(summary.linked.is_empty());
        assert_eq!(dataset.skills.skills.len(), 2);
        // Both are backed by the project, and the alias map resolves them.
        assert!(dataset.skills.aliases.contains_key("rust"));
        assert!(dataset.skills.aliases.contains_key("typst"));
    }

    #[tokio::test]
    async fn a_non_interactive_run_mints_nothing_but_still_records_the_project() {
        let mut dataset = dataset_with_rust();
        let analysis = analysis_with(&[
            ("Rust", SkillCategory::Language),   // existing -> still linked
            ("PostgreSQL", SkillCategory::Tool), // new -> skipped (no terminal)
        ]);
        // NonInteractiveUser: is_interactive() is false, so no ask() is made.
        let user = crate::terminal::NonInteractiveUser;

        let summary = apply_import(&mut dataset, analysis, None, &user)
            .await
            .unwrap();

        assert!(summary.minted.is_empty(), "minting needs a person");
        assert_eq!(summary.skipped_new, vec!["PostgreSQL"]);
        assert_eq!(summary.linked, vec!["Rust"]);
        // No new skill was created; the project was still recorded and links Rust.
        assert_eq!(dataset.skills.skills.len(), 1);
        assert_eq!(dataset.projects.len(), 1);
        assert_eq!(
            dataset.projects[0].skill_ids,
            vec![SkillId("skill-1".into())]
        );
    }

    // ----- GitHub profile JSON mapping (no network) -----

    #[test]
    fn repo_list_json_maps_to_repo_refs() {
        let body = r#"[
            {"name": "engine", "description": "a task engine", "fork": false,
             "owner": {"login": "ada"}},
            {"name": "forked-lib", "description": null, "fork": true,
             "owner": {"login": "ada"}}
        ]"#;
        let repos = parse_repo_list(body, "url").unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].name, "engine");
        assert_eq!(repos[0].owner, "ada");
        assert!(!repos[0].fork);
        assert_eq!(repos[0].description.as_deref(), Some("a task engine"));
        assert!(repos[1].fork);
        assert!(repos[1].description.is_none());
        // The label carries the fork marker for the pick list.
        assert!(repos[1].label().contains("[fork]"));
    }

    #[test]
    fn a_garbled_repo_list_is_a_typed_parse_error() {
        assert!(matches!(
            parse_repo_list("<html>rate limited</html>", "url"),
            Err(ImportError::ParseApi { .. })
        ));
    }

    // ----- the scratch-dir guard -----

    #[test]
    fn a_scratch_dir_deletes_itself_on_drop() {
        let path = {
            let scratch = ScratchDir::new().unwrap();
            let path = scratch.path().to_path_buf();
            assert!(path.is_dir());
            path
        };
        assert!(!path.exists(), "the scratch dir is gone after drop");
    }
}
