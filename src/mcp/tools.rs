//! The tool registry: what `tools/list` advertises and what `tools/call`
//! runs. Each tool is a thin adapter over a library *service* function — the
//! same functions the CLI commands call — so the MCP surface adds no new
//! behavior, only a new way to reach it.
//!
//! Two deliberate shapes here:
//!
//! - **Dispatch is a `match`, not a trait-object registry.** A tools-only
//!   server has a fixed, small set; a `match` on the name is the most legible
//!   form and needs no `dyn` async gymnastics. New tool, new arm.
//! - **Tool failures are in-band.** A handler returns `Result<_, CliError>`;
//!   [`call`] catches the error and turns it into a `CallToolResult` with
//!   `isError: true`, so the client's model reads the reason rather than a
//!   transport fault. Only malformed *arguments* short-circuit to a failure
//!   result early.
//!
//! Never-fabricate is untouched by this layer: the `tailor` tool drives the
//! exact guarded pipeline the CLI does, and `analyze_gap` / `parse_job` are
//! read-only. The only writer is `ingest`, which (like `aarg ingest`) is an
//! input path — it transcribes the user's own resume into the dataset — and
//! it backs the previous dataset up first.

use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::agent::AgentContext;
use crate::ats::AtsReport;
use crate::commands::{CliError, configured_client, default_tracer};
use crate::config::Config;
use crate::dataset::store;
use crate::dataset::types::ResumeDataset;
use crate::llm::AnthropicClient;
use crate::review::AdversarialReport;
use crate::tailor::TailoredResume;
use crate::trace::Tracer;
use crate::variant::Variant;

use super::client::{ElicitationUser, McpClient};
use super::protocol::{
    CallToolResult, Content, ReadResourceResult, Resource, ResourceContents, Tool,
};

/// The tools advertised to the client, in a stable, discovery-friendly
/// order: cheap read-only first, then analysis, then the writers.
pub fn descriptors() -> Vec<Tool> {
    vec![
        tool(
            "dataset_summary",
            "Summarize the user's recorded resume dataset (the source of truth AARG \
             tailors from): contact, counts of roles/skills/projects, and which skills \
             have backing evidence. Read-only.",
            no_args(),
        ),
        tool(
            "list_builds",
            "List past tailoring builds, newest first, with each one's score, keyword \
             coverage, objection count, and the job it targeted. Read-only.",
            no_args(),
        ),
        tool(
            "get_build",
            "Fetch one past build by id: its tailored resume, the adversarial reviewer's \
             report, ATS keyword coverage, and its rendered PDFs (available as MCP \
             resources). Read-only.",
            object(
                &[("build_id", "string", "The build id, e.g. \"041\".")],
                &["build_id"],
            ),
        ),
        tool(
            "parse_job",
            "Parse a job description (paste the text) into structured requirements: title, \
             company, required and preferred skills, responsibilities, and ATS phrases. \
             Changes nothing.",
            object(
                &[(
                    "job_description",
                    "string",
                    "The full job description text.",
                )],
                &["job_description"],
            ),
        ),
        tool(
            "analyze_gap",
            "Compare a job description against the user's recorded experience and return \
             the gap: required skills matched with evidence, weakly supported ones, and \
             unknown ones. Does not fabricate experience or change anything.",
            object(
                &[(
                    "job_description",
                    "string",
                    "The full job description text.",
                )],
                &["job_description"],
            ),
        ),
        tool(
            "tailor",
            "Tailor the user's resume to a job description through AARG's adversarial \
             review loop, then render the PDFs. Pass the posting as job_description, or \
             pass build_id to re-tailor a past build using its stored job description (no \
             re-paste needed). Writes a new build. Never fabricates: every line traces to \
             recorded, evidence-backed material. Returns the build id, score, keyword \
             coverage, the reviewer's report, and the rendered PDFs (available as MCP \
             resources).",
            tailor_schema(),
        ),
        tool(
            "ingest",
            "Replace the user's resume dataset with one parsed from the provided resume \
             text. This OVERWRITES the existing dataset (the previous one is copied to a \
             timestamped backup first). Only what the resume states is recorded; nothing \
             is invented.",
            object(
                &[(
                    "resume_text",
                    "string",
                    "The full text of an existing resume.",
                )],
                &["resume_text"],
            ),
        ),
    ]
}

/// Run a tool by name. Never returns a transport error: an unknown tool or a
/// handler failure becomes a `CallToolResult` with `isError: true`.
pub(super) async fn call(name: &str, arguments: Value, client: &McpClient) -> CallToolResult {
    let outcome = match name {
        "dataset_summary" => dataset_summary().await,
        "list_builds" => list_builds().await,
        "get_build" => get_build(arguments).await,
        "parse_job" => parse_job(arguments).await,
        "analyze_gap" => analyze_gap(arguments).await,
        "tailor" => tailor(arguments, client).await,
        "ingest" => ingest(arguments).await,
        other => return CallToolResult::failure(format!("unknown tool {other:?}")),
    };
    match outcome {
        Ok(result) => result,
        // CliError's Display already carries an actionable message (missing
        // key, typst not installed, model output didn't parse, ...).
        Err(error) => CallToolResult::failure(error.to_string()),
    }
}

// ---------------------------------------------------------------------
// Read-only tools (no model call, no cost)
// ---------------------------------------------------------------------

async fn dataset_summary() -> Result<CallToolResult, CliError> {
    let dataset = store::load()?;
    Ok(CallToolResult::json(summarize_dataset(&dataset)))
}

async fn list_builds() -> Result<CallToolResult, CliError> {
    let builds = crate::history::list()?;
    let items: Vec<Value> = builds
        .iter()
        .map(|b| {
            json!({
                "id": b.id,
                "created_at": b.created_at,
                "target": b.target,
                "model": b.model,
                "score": b.score,
                "review_score": b.review_score,
                "coverage": b.coverage,
                "objections": b.objections,
                "tokens_in": b.tokens_in,
                "tokens_out": b.tokens_out,
                "subscription": b.subscription,
            })
        })
        .collect();
    Ok(CallToolResult::json(json!({ "builds": items })))
}

async fn get_build(arguments: Value) -> Result<CallToolResult, CliError> {
    let args: BuildArgs = match parse_args(arguments) {
        Ok(args) => args,
        Err(failure) => return Ok(failure),
    };
    let resume: TailoredResume = crate::history::read_artifact(&args.build_id, "canonical.json")?;
    let report: AdversarialReport =
        crate::history::read_artifact(&args.build_id, "adversarial_report.json")?;
    let ats: AtsReport = crate::history::read_artifact(&args.build_id, "ats_report.json")?;

    let mut obj = Map::new();
    obj.insert("build_id".into(), json!(args.build_id));
    insert_serialized(&mut obj, "resume", &resume)?;
    insert_serialized(&mut obj, "adversarial_report", &report)?;
    insert_serialized(&mut obj, "ats_report", &ats)?;
    obj.insert("pdfs".into(), json!(build_pdf_paths(&args.build_id)));
    // The PDFs ride along as resource links (the client fetches each via
    // resources/read), never inlined: a binary PDF in a tool result makes the
    // client try to read it as an image and reject the media type.
    Ok(CallToolResult::json(Value::Object(obj))
        .with_resource_links(build_pdf_links(&args.build_id)))
}

// ---------------------------------------------------------------------
// Analysis tools (model-backed, read-only on the dataset)
// ---------------------------------------------------------------------

async fn parse_job(arguments: Value) -> Result<CallToolResult, CliError> {
    let args: JobArgs = match parse_args(arguments) {
        Ok(args) => args,
        Err(failure) => return Ok(failure),
    };
    let pieces = AgentPieces::build().await?;
    let requirements = crate::jd::parse_jd(&pieces.ctx(), &args.job_description).await?;

    let mut obj = Map::new();
    insert_serialized(&mut obj, "requirements", &requirements)?;
    Ok(CallToolResult::json(Value::Object(obj)))
}

async fn analyze_gap(arguments: Value) -> Result<CallToolResult, CliError> {
    let args: JobArgs = match parse_args(arguments) {
        Ok(args) => args,
        Err(failure) => return Ok(failure),
    };
    let dataset = store::load()?;
    let pieces = AgentPieces::build().await?;
    let ctx = pieces.ctx();
    let requirements = crate::jd::parse_jd(&ctx, &args.job_description).await?;
    let gap = crate::gap::analyze_gap(&ctx, &requirements, &dataset).await?;

    let mut obj = Map::new();
    insert_serialized(&mut obj, "requirements", &requirements)?;
    insert_serialized(&mut obj, "gap", &gap)?;
    Ok(CallToolResult::json(Value::Object(obj)))
}

// ---------------------------------------------------------------------
// tailor — the flagship: drive the guarded CLI pipeline non-interactively
// ---------------------------------------------------------------------

async fn tailor(arguments: Value, client: &McpClient) -> Result<CallToolResult, CliError> {
    let args: TailorArgs = match parse_args(arguments) {
        Ok(args) => args,
        Err(failure) => return Ok(failure),
    };
    let variants = match args.variant.as_deref() {
        None | Some("both") => vec![Variant::Ats, Variant::Human],
        Some("ats") => vec![Variant::Ats],
        Some("human") => vec![Variant::Human],
        Some(other) => {
            return Ok(CallToolResult::failure(format!(
                "unknown variant {other:?}; use \"ats\", \"human\", or \"both\""
            )));
        }
    };

    // Resolve the job description to a path the pipeline reads. Two sources,
    // exactly one required: a posting pasted as `job_description` (written to a
    // scratch file), or `build_id` to re-tailor a past build's stored JD — its
    // `jd.json` is the full parsed JobRequirements, which `load_requirements`
    // reads directly, so re-tailoring needs no re-paste, no re-parse, no model
    // call. The never-fabricate-guarded loop then runs verbatim; all output is
    // on stderr, so the JSON-RPC stream on stdout stays clean.
    let job_text = args
        .job_description
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let (jd_path, scratch) = match (job_text, args.build_id.as_deref()) {
        (Some(text), _) => {
            let scratch = store::dir()?.join("mcp.jd.txt");
            std::fs::write(&scratch, text).map_err(|source| CliError::ReadInput {
                path: scratch.clone(),
                source,
            })?;
            (scratch.clone(), Some(scratch))
        }
        (None, Some(build_id)) => {
            let Ok(number) = build_id.parse::<u32>() else {
                return Ok(CallToolResult::failure(format!(
                    "invalid build id {build_id:?}"
                )));
            };
            let path = crate::builds::builds_root()?
                .join(format!("{number:03}"))
                .join("jd.json");
            if !path.exists() {
                return Ok(CallToolResult::failure(format!(
                    "build {build_id:?} has no stored job description to re-tailor"
                )));
            }
            (path, None)
        }
        (None, None) => {
            return Ok(CallToolResult::failure(
                "provide job_description (the posting text) or build_id (to re-tailor a past build's job description)",
            ));
        }
    };

    // Route the copilots through the client: ElicitationUser self-gates on
    // capability, so a client that can't elicit gets the same non-interactive
    // "tailor as-is" run, while one that can sees the copilots as dialogs.
    let user = ElicitationUser::new(client.clone());
    let result =
        crate::commands::tailor::run(Some(jd_path), variants, None, args.cover, &user).await;
    if let Some(scratch) = scratch {
        let _ = std::fs::remove_file(&scratch);
    }
    // A hard failure (no key, typst missing, the model selected nothing) maps
    // to an in-band tool error via `call`.
    result?;

    // The server handles one request at a time, so the newest build is the
    // one we just produced.
    let builds = crate::history::list()?;
    let Some(summary) = builds.into_iter().next() else {
        return Ok(CallToolResult::failure(
            "tailoring finished but no build was recorded",
        ));
    };

    let mut obj = Map::new();
    obj.insert("build_id".into(), json!(summary.id));
    obj.insert("target".into(), json!(summary.target));
    obj.insert("model".into(), json!(summary.model));
    obj.insert("score".into(), json!(summary.score));
    obj.insert("review_score".into(), json!(summary.review_score));
    obj.insert("coverage".into(), json!(summary.coverage));
    obj.insert("objections".into(), json!(summary.objections));
    obj.insert("tokens_in".into(), json!(summary.tokens_in));
    obj.insert("tokens_out".into(), json!(summary.tokens_out));
    obj.insert("subscription".into(), json!(summary.subscription));
    obj.insert("pdfs".into(), json!(build_pdf_paths(&summary.id)));
    // The reviewer's report, so a client can surface the objections that
    // remain on the winning draft.
    if let Ok(report) =
        crate::history::read_artifact::<AdversarialReport>(&summary.id, "adversarial_report.json")
    {
        insert_serialized(&mut obj, "adversarial_report", &report)?;
    }
    // The PDFs ride along as resource links (see get_build); the client fetches
    // each via resources/read rather than receiving a binary blob inline.
    Ok(CallToolResult::json(Value::Object(obj)).with_resource_links(build_pdf_links(&summary.id)))
}

// ---------------------------------------------------------------------
// ingest — the gated writer
// ---------------------------------------------------------------------

async fn ingest(arguments: Value) -> Result<CallToolResult, CliError> {
    let args: IngestArgs = match parse_args(arguments) {
        Ok(args) => args,
        Err(failure) => return Ok(failure),
    };
    if args.resume_text.trim().is_empty() {
        return Ok(CallToolResult::failure("resume_text was empty"));
    }

    // Gate the source-of-truth mutation: if a dataset already exists, copy it
    // to a timestamped backup BEFORE overwriting, so an accidental ingest from
    // a connected client is always recoverable. (`store::save` keeps a single
    // `dataset.json.bak`, but that's overwritten on the next save; this dated
    // copy is kept.)
    let dir = store::dir()?;
    let existing = dir.join("dataset.json");
    let mut backup = None;
    if existing.exists() {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        let path = dir.join(format!("dataset.json.mcp-bak-{stamp}"));
        std::fs::copy(&existing, &path).map_err(|source| CliError::ReadInput {
            path: path.clone(),
            source,
        })?;
        backup = Some(path.display().to_string());
    }

    let pieces = AgentPieces::build().await?;
    let mut outcome = crate::ingest::ingest_resume(&pieces.ctx(), &args.resume_text).await?;
    outcome.dataset.metadata.source_files = vec!["(provided via MCP ingest tool)".to_string()];
    store::save(&outcome.dataset)?;

    let mut obj = Map::new();
    obj.insert("status".into(), json!("dataset replaced"));
    if let Some(backup) = backup {
        obj.insert("previous_dataset_backup".into(), json!(backup));
    }
    obj.insert("warnings".into(), json!(outcome.warnings));
    obj.insert("summary".into(), summarize_dataset(&outcome.dataset));
    Ok(CallToolResult::json(Value::Object(obj)))
}

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

/// The owned pieces an LLM-backed tool borrows into an [`AgentContext`].
/// Built fresh per call (a keychain read is cheap, and a CLI-delegated token
/// is refreshed each time) with no live-cost sink — its output would land on
/// stdout and corrupt the stream.
struct AgentPieces {
    client: AnthropicClient,
    config: Config,
    tracer: Tracer,
}

impl AgentPieces {
    async fn build() -> Result<Self, CliError> {
        let (client, config) = configured_client().await?;
        let tracer = default_tracer()?;
        Ok(Self {
            client,
            config,
            tracer,
        })
    }

    fn ctx(&self) -> AgentContext<'_> {
        AgentContext {
            llm: &self.client,
            model: &self.config.anthropic,
            tracer: &self.tracer,
            sink: None,
        }
    }
}

/// A compact, model-readable view of the dataset.
fn summarize_dataset(d: &ResumeDataset) -> Value {
    let roles: Vec<Value> = d
        .roles
        .iter()
        .map(|r| {
            json!({
                "id": r.id.0,
                "company": r.company,
                "title": r.title,
                "start": r.start.to_string(),
                "end": r.end.as_ref().map(|e| e.to_string()),
                "bullets": r.bullets.len(),
            })
        })
        .collect();
    let skills: Vec<Value> = d
        .skills
        .skills
        .iter()
        .map(|s| {
            json!({
                "name": s.canonical_name,
                "backed": !s.evidence.is_empty(),
            })
        })
        .collect();
    let backed = d
        .skills
        .skills
        .iter()
        .filter(|s| !s.evidence.is_empty())
        .count();
    json!({
        "contact": {
            "name": d.contact.full_name,
            "email": d.contact.email,
            "location": d.contact.location,
        },
        "summary": d.summary,
        "counts": {
            "roles": d.roles.len(),
            "skills": d.skills.skills.len(),
            "skills_backed": backed,
            "projects": d.projects.len(),
            "education": d.education.len(),
            "certifications": d.certifications.len(),
            "achievements": d.achievements.len(),
            "voice_samples": d.voice_samples.len(),
        },
        "roles": roles,
        "skills": skills,
    })
}

/// The rendered PDF files in a build directory, sorted for a stable order.
fn build_pdfs(id: &str) -> Vec<PathBuf> {
    let Ok(root) = crate::builds::builds_root() else {
        return Vec::new();
    };
    let mut pdfs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join(id)) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
            {
                pdfs.push(path);
            }
        }
    }
    pdfs.sort();
    pdfs
}

/// Those PDFs as display paths, for the structured result.
fn build_pdf_paths(id: &str) -> Vec<String> {
    build_pdfs(id)
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

/// Resource-link content blocks for a build's rendered PDFs, so a tool result
/// hands the client openable artifacts (it fetches each via `resources/read`)
/// instead of bare filesystem paths. Same `aarg://` uri scheme as
/// [`list_resources`].
fn build_pdf_links(id: &str) -> Vec<Content> {
    build_pdfs(id)
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            Some(Content::resource_link(
                format!("aarg://build/{id}/{name}"),
                format!("build {id} · {name}"),
                Some("application/pdf".to_string()),
            ))
        })
        .collect()
}

/// Read a file and base64-encode it, or `None` if it can't be read.
fn base64_file(path: &Path) -> Option<String> {
    use base64::Engine as _;
    let bytes = std::fs::read(path).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Every build's rendered PDFs, as MCP resources for `resources/list`.
pub(super) fn list_resources() -> Vec<Resource> {
    let Ok(builds) = crate::history::list() else {
        return Vec::new();
    };
    let mut resources = Vec::new();
    for build in builds {
        for path in build_pdfs(&build.id) {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            resources.push(Resource {
                uri: format!("aarg://build/{}/{}", build.id, name),
                name: format!("build {} · {} · {}", build.id, name, build.target),
                description: None,
                mime_type: Some("application/pdf".to_string()),
            });
        }
    }
    resources
}

/// Read one resource uri (`aarg://build/<id>/<file>.pdf`) into its bytes,
/// guarding against any uri that would escape the builds root.
pub(super) fn read_resource(uri: &str) -> Result<ReadResourceResult, String> {
    let rest = uri
        .strip_prefix("aarg://build/")
        .ok_or_else(|| format!("unknown resource uri {uri:?}"))?;
    let (id, file) = rest
        .split_once('/')
        .ok_or_else(|| format!("malformed resource uri {uri:?}"))?;
    // A bare numeric id and a plain pdf filename — never a path escape.
    if id.parse::<u32>().is_err()
        || file.contains('/')
        || file.contains("..")
        || !file.to_ascii_lowercase().ends_with(".pdf")
    {
        return Err(format!("invalid resource uri {uri:?}"));
    }
    let root = crate::builds::builds_root().map_err(|e| e.to_string())?;
    let blob = base64_file(&root.join(id).join(file))
        .ok_or_else(|| format!("resource not found: {uri}"))?;
    Ok(ReadResourceResult {
        contents: vec![ResourceContents {
            uri: uri.to_string(),
            mime_type: Some("application/pdf".to_string()),
            text: None,
            blob: Some(blob),
        }],
    })
}

/// Deserialize a tool's arguments, mapping a shape mismatch to an in-band
/// failure result the caller returns as `Ok(failure)`.
fn parse_args<T: DeserializeOwned>(arguments: Value) -> Result<T, CallToolResult> {
    serde_json::from_value(arguments)
        .map_err(|e| CallToolResult::failure(format!("invalid arguments: {e}")))
}

/// Serialize a value into a result object, surfacing the (practically
/// impossible) serialization failure as a typed error rather than a panic.
fn insert_serialized<T: serde::Serialize>(
    map: &mut Map<String, Value>,
    key: &str,
    value: &T,
) -> Result<(), CliError> {
    map.insert(
        key.to_string(),
        serde_json::to_value(value).map_err(CliError::OutputJson)?,
    );
    Ok(())
}

// ---- descriptor builders ----

fn tool(name: &str, description: &str, input_schema: Value) -> Tool {
    Tool {
        name: name.to_string(),
        description: Some(description.to_string()),
        input_schema,
    }
}

/// A no-argument tool's input schema: an empty object.
fn no_args() -> Value {
    json!({"type": "object", "properties": {}, "additionalProperties": false})
}

/// Build an object input schema from `(name, type, description)` properties
/// and the names that are required.
fn object(properties: &[(&str, &str, &str)], required: &[&str]) -> Value {
    let mut props = Map::new();
    for (name, ty, description) in properties {
        props.insert(
            (*name).to_string(),
            json!({"type": ty, "description": description}),
        );
    }
    json!({
        "type": "object",
        "properties": Value::Object(props),
        "required": required,
        "additionalProperties": false,
    })
}

/// The `tailor` schema, written by hand because it has an enum and defaults.
fn tailor_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "job_description": {
                "type": "string",
                "description": "The full job description text to tailor toward."
            },
            "variant": {
                "type": "string",
                "enum": ["ats", "human", "both"],
                "description": "Which PDF(s) to render: the parser-safe ATS resume, the \
                                designed human one, or both. Defaults to both."
            },
            "cover": {
                "type": "boolean",
                "description": "Also draft a cover letter from the tailored resume. Defaults to false."
            },
            "build_id": {
                "type": "string",
                "description": "Instead of job_description, re-tailor a past build by id (e.g. \"046\") \
                                using its stored job description. Find ids with list_builds. Provide \
                                exactly one of job_description or build_id."
            }
        },
        "additionalProperties": false
    })
}

// ---- argument shapes ----

#[derive(serde::Deserialize)]
struct BuildArgs {
    build_id: String,
}

#[derive(serde::Deserialize)]
struct JobArgs {
    job_description: String,
}

#[derive(serde::Deserialize)]
struct TailorArgs {
    #[serde(default)]
    job_description: Option<String>,
    #[serde(default)]
    build_id: Option<String>,
    #[serde(default)]
    variant: Option<String>,
    #[serde(default)]
    cover: bool,
}

#[derive(serde::Deserialize)]
struct IngestArgs {
    resume_text: String,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn test_client() -> McpClient {
        let (tx, _rx) = tokio::sync::mpsc::channel::<String>(1);
        let pending =
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        McpClient::new(tx, pending)
    }

    #[test]
    fn every_descriptor_has_an_object_schema_and_a_description() {
        for tool in descriptors() {
            assert!(
                tool.description.is_some(),
                "{} lacks a description",
                tool.name
            );
            assert_eq!(tool.input_schema["type"], "object", "{}", tool.name);
        }
    }

    #[test]
    fn the_tool_set_is_the_expected_seven() {
        let names: Vec<String> = descriptors().into_iter().map(|t| t.name).collect();
        for expected in [
            "dataset_summary",
            "list_builds",
            "get_build",
            "parse_job",
            "analyze_gap",
            "tailor",
            "ingest",
        ] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
    }

    #[test]
    fn the_tailor_schema_constrains_variant_to_an_enum() {
        let schema = tailor_schema();
        let variants = schema["properties"]["variant"]["enum"].as_array().unwrap();
        assert_eq!(variants.len(), 3);
        // Either job_description or build_id works, so neither is hard-required.
        assert!(schema["properties"]["build_id"].is_object());
        assert!(schema.get("required").is_none());
    }

    #[tokio::test]
    async fn an_unknown_tool_is_an_in_band_failure() {
        let result = call("frobnicate", json!({}), &test_client()).await;
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["isError"], json!(true));
        assert!(
            wire["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("frobnicate")
        );
    }

    #[tokio::test]
    async fn a_tool_with_bad_arguments_fails_in_band_without_calling_the_model() {
        // `get_build` needs a `build_id` string; an object missing it is a
        // shape error surfaced as an isError result, not a transport fault.
        let result = call("get_build", json!({"wrong": 1}), &test_client()).await;
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["isError"], json!(true));
        assert!(
            wire["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("invalid arguments")
        );
    }

    #[test]
    fn the_object_builder_marks_required_properties() {
        let schema = object(
            &[("job_description", "string", "the text")],
            &["job_description"],
        );
        assert_eq!(schema["properties"]["job_description"]["type"], "string");
        assert_eq!(schema["required"], json!(["job_description"]));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[tokio::test]
    async fn tailor_without_a_jd_or_build_fails_in_band() {
        // Neither job_description nor build_id: an in-band failure before any
        // model call or pipeline run.
        let result = call("tailor", json!({}), &test_client()).await;
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["isError"], json!(true));
        assert!(
            wire["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("build_id")
        );
    }

    #[test]
    fn read_resource_guards_against_escapes_and_bad_schemes() {
        // Wrong scheme, a non-numeric id, a slash in the filename, and a
        // non-pdf are all rejected before any filesystem access.
        assert!(read_resource("http://evil/x.pdf").is_err());
        assert!(read_resource("aarg://build/../secret.pdf").is_err());
        assert!(read_resource("aarg://build/041/../x.pdf").is_err());
        assert!(read_resource("aarg://build/041/notes.txt").is_err());
        // A well-formed uri for a build that doesn't exist still errs.
        assert!(read_resource("aarg://build/999999/ats.pdf").is_err());
    }
}
