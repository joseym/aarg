//! WebAssembly bindings for AARG's deterministic, model-free domain logic.
//!
//! These four functions are the no-LLM, no-filesystem core of the pipeline, so
//! a web UI can run them entirely in the browser, offline: validate a dataset,
//! preview how it covers a job description, project the canonical draft into
//! the ATS payload, and — the point of AARG — check that a variant makes no
//! claim the canonical draft doesn't (the never-fabricate backstop, running
//! client-side).
//!
//! Everything crosses the JS boundary as JSON, the same shape the CLI reads
//! and writes on disk. The logic lives in plain `*_impl` functions
//! (`Result<String, String>`) so it is testable as ordinary Rust on the host;
//! the thin `#[wasm_bindgen]` wrappers just turn the error string into a thrown
//! JS error. The model-driven pipeline (tailoring) is not bound here — it needs
//! a host-provided LLM client and is a later milestone.

use wasm_bindgen::prelude::*;

use aarg_domain::dataset::types::ResumeDataset;
use aarg_domain::jd::JobRequirements;
use aarg_domain::tailor::TailoredResume;
use aarg_domain::variant::VariantPayload;

/// Parse a JSON argument into a domain type, naming what was malformed.
fn parse<T: serde::de::DeserializeOwned>(json: &str, what: &str) -> Result<T, String> {
    serde_json::from_str(json).map_err(|e| format!("invalid {what}: {e}"))
}

/// Serialize a result to JSON.
fn dump<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("could not serialize result: {e}"))
}

fn validate_impl(dataset_json: &str) -> Result<String, String> {
    let dataset: ResumeDataset = parse(dataset_json, "dataset")?;
    dump(&aarg_domain::dataset::validate::validate(&dataset))
}

fn analyze_gap_impl(jd_json: &str, dataset_json: &str) -> Result<String, String> {
    let jd: JobRequirements = parse(jd_json, "job requirements")?;
    let dataset: ResumeDataset = parse(dataset_json, "dataset")?;
    dump(&aarg_domain::gap::deterministic_gap(&jd, &dataset))
}

fn project_ats_impl(canonical_json: &str) -> Result<String, String> {
    let draft: TailoredResume = parse(canonical_json, "canonical draft")?;
    dump(&aarg_domain::variant::ats_payload(&draft))
}

fn check_claims_impl(canonical_json: &str, payload_json: &str) -> Result<String, String> {
    let canonical: TailoredResume = parse(canonical_json, "canonical draft")?;
    let payload: VariantPayload = parse(payload_json, "variant payload")?;
    let (ok, divergences) = match aarg_domain::variant::check_claims(&canonical, &payload) {
        Ok(()) => (true, Vec::new()),
        Err(divergence) => (false, divergence.divergences),
    };
    dump(&serde_json::json!({ "ok": ok, "divergences": divergences }))
}

/// Validate a dataset, returning a `ValidationReport` (problems + notes). The
/// never-fabricate invariant starts here: a skill with no evidence is a
/// problem the report flags.
#[wasm_bindgen]
pub fn validate(dataset_json: &str) -> Result<String, JsValue> {
    validate_impl(dataset_json).map_err(|e| JsValue::from_str(&e))
}

/// Preview, with no model call, how a dataset covers a job description: which
/// JD skills resolve against recorded experience and which don't. The parsed
/// `JobRequirements` is produced by the host (parsing is a model call); this is
/// the deterministic coverage half.
#[wasm_bindgen]
pub fn analyze_gap(jd_json: &str, dataset_json: &str) -> Result<String, JsValue> {
    analyze_gap_impl(jd_json, dataset_json).map_err(|e| JsValue::from_str(&e))
}

/// Project a canonical `TailoredResume` into the ATS variant payload — the
/// deterministic, same-facts projection (the human variant's reword needs the
/// model and is not bound here).
#[wasm_bindgen]
pub fn project_ats(canonical_json: &str) -> Result<String, JsValue> {
    project_ats_impl(canonical_json).map_err(|e| JsValue::from_str(&e))
}

/// Check that a variant payload makes no claim the canonical draft doesn't —
/// the never-fabricate backstop, running client-side. Returns
/// `{ "ok": true, "divergences": [] }` when the variant is faithful, or
/// `{ "ok": false, "divergences": [...] }` listing each claim that diverged.
#[wasm_bindgen]
pub fn check_claims(canonical_json: &str, payload_json: &str) -> Result<String, JsValue> {
    check_claims_impl(canonical_json, payload_json).map_err(|e| JsValue::from_str(&e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use aarg_domain::dataset::types::Contact;

    // Build a well-formed empty dataset via the domain constructor, so the
    // round-trip test doesn't hand-encode (and drift from) the schema.
    fn empty_dataset_json() -> String {
        let dataset = ResumeDataset::new(Contact {
            full_name: "Test Person".into(),
            email: "t@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        serde_json::to_string(&dataset).unwrap()
    }

    #[test]
    fn validate_round_trips_a_dataset_through_json() {
        let out = validate_impl(&empty_dataset_json())
            .expect("validate should accept a well-formed dataset");
        let report: serde_json::Value = serde_json::from_str(&out).unwrap();
        // An empty dataset has no skills, so no evidence problems.
        assert!(report["problems"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_malformed_dataset_is_an_error_not_a_panic() {
        let err = validate_impl("{ not json").unwrap_err();
        assert!(err.contains("invalid dataset"), "got {err:?}");
    }
}
