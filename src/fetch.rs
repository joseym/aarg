//! Fetching job descriptions from supported job boards (FR-1.4).
//!
//! Only Greenhouse and Lever URLs are fetched — both publish documented,
//! auth-free JSON APIs for public postings, which beats scraping HTML
//! that can change shape any day. Any other URL is a typed error telling
//! the user to paste the text; an allowlist that fails honestly is worth
//! more than a generic fetcher that returns nav-bar soup.
//!
//! Fetched JDs are cached by URL hash (`~/.cache/aarg/jd_cache`), so
//! re-running `gap` or `tailor` against the same posting is free and
//! works offline.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use async_trait::async_trait;
use directories::ProjectDirs;
use serde::Deserialize;

use crate::agent::{Tool, ToolError};

/// Everything that can go wrong while fetching a JD.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("aarg can only fetch JDs from Greenhouse or Lever job boards; {url} is neither")]
    UnsupportedUrl { url: String },

    #[error("could not determine this user's home directory")]
    NoHomeDir,

    #[error("could not reach {url}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("the job board answered HTTP {status} for {url} — the posting may be gone")]
    Board { status: u16, url: String },

    #[error("could not make sense of the job board's response for {url}")]
    Parse {
        url: String,
        #[source]
        source: serde_json::Error,
    },
}

/// The fetch capability as a runtime tool — the PRD's `fetch_url`,
/// named for what it actually fetches. Offered to the JD parser so a
/// message that only *references* a posting URL can still be resolved;
/// the deterministic prefetch in the commands stays the primary path.
pub struct FetchJdTool;

static FETCH_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::json!({
        "type": "object",
        "properties": {
            "url": {
                "type": "string",
                "description": "A Greenhouse (boards.greenhouse.io) or Lever (jobs.lever.co) posting URL"
            }
        },
        "required": ["url"]
    })
});

#[async_trait]
impl Tool for FetchJdTool {
    fn name(&self) -> &'static str {
        "fetch_jd"
    }
    fn description(&self) -> &'static str {
        "Fetch the text of a job posting from a Greenhouse or Lever URL"
    }
    fn input_schema(&self) -> &serde_json::Value {
        &FETCH_SCHEMA
    }
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let url = args
            .get("url")
            .and_then(|value| value.as_str())
            .ok_or_else(|| ToolError::Failed {
                message: "fetch_jd needs a string \"url\" argument".into(),
            })?;
        let text = fetch_jd(url).await.map_err(|error| ToolError::Failed {
            message: error.to_string(),
        })?;
        Ok(serde_json::json!({ "text": text }))
    }
}

/// A recognized job-board posting. Which board decides both the API URL
/// and how to read its response.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Board {
    Greenhouse { company: String, job_id: String },
    Lever { company: String, posting_id: String },
}

/// Fetch (or recall from cache) the text of a job posting.
pub async fn fetch_jd(url: &str) -> Result<String, FetchError> {
    let cache = cache_dir()?;
    fetch_jd_with(&reqwest::Client::new(), &cache, url).await
}

/// The testable core: HTTP client and cache directory are parameters.
async fn fetch_jd_with(
    http: &reqwest::Client,
    cache_dir: &Path,
    url: &str,
) -> Result<String, FetchError> {
    let board = classify(url).ok_or_else(|| FetchError::UnsupportedUrl {
        url: url.to_string(),
    })?;

    // Cache first: a hit costs no network and works offline.
    let cache_path = cache_dir.join(format!("{:016x}.txt", fnv1a(url.as_bytes())));
    if let Ok(text) = std::fs::read_to_string(&cache_path) {
        return Ok(text);
    }

    let api_url = board.api_url();
    let http_err = |source| FetchError::Http {
        url: api_url.clone(),
        source,
    };
    let response = http
        .get(&api_url)
        .header(reqwest::header::USER_AGENT, "aarg")
        .send()
        .await
        .map_err(http_err)?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(FetchError::Board {
            status,
            url: api_url,
        });
    }
    let body = response.text().await.map_err(http_err)?;
    let text = board.extract(&body)?;

    // Best-effort cache write: a read-only cache dir shouldn't fail the
    // fetch we just completed.
    if std::fs::create_dir_all(cache_dir).is_ok() {
        let _ = std::fs::write(&cache_path, &text);
    }
    Ok(text)
}

/// Recognize a posting URL. Hand-rolled rather than pulling a URL crate:
/// the two accepted shapes are rigid enough that scheme-strip + segment
/// split is the whole job.
///
/// - `https://boards.greenhouse.io/<company>/jobs/<id>`
///   (also the newer `job-boards.greenhouse.io`)
/// - `https://jobs.lever.co/<company>/<posting-id>`
// EXERCISE(EX-013)
fn classify(url: &str) -> Option<Board> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    // Query strings and fragments don't identify the posting.
    let rest = rest.split(['?', '#']).next()?;
    let mut segments = rest.split('/').filter(|s| !s.is_empty());
    let host = segments.next()?;
    let segments: Vec<&str> = segments.collect();

    match host {
        "boards.greenhouse.io" | "job-boards.greenhouse.io" => match segments.as_slice() {
            [company, "jobs", job_id] => Some(Board::Greenhouse {
                company: (*company).to_string(),
                job_id: (*job_id).to_string(),
            }),
            _ => None,
        },
        "jobs.lever.co" => match segments.as_slice() {
            [company, posting_id] => Some(Board::Lever {
                company: (*company).to_string(),
                posting_id: (*posting_id).to_string(),
            }),
            _ => None,
        },
        _ => None,
    }
}

impl Board {
    /// The documented public API endpoint for this posting.
    fn api_url(&self) -> String {
        match self {
            Board::Greenhouse { company, job_id } => {
                format!("https://boards-api.greenhouse.io/v1/boards/{company}/jobs/{job_id}")
            }
            Board::Lever {
                company,
                posting_id,
            } => format!("https://api.lever.co/v0/postings/{company}/{posting_id}"),
        }
    }

    /// Turn the board's JSON response into plain JD text for the parser.
    fn extract(&self, body: &str) -> Result<String, FetchError> {
        let parse_err = |source| FetchError::Parse {
            url: self.api_url(),
            source,
        };
        match self {
            Board::Greenhouse { company, .. } => {
                let job: GreenhouseJob = serde_json::from_str(body).map_err(parse_err)?;
                let mut text = format!("company: {company}\ntitle: {}\n", job.title);
                if let Some(location) = job.location {
                    text.push_str(&format!("location: {}\n", location.name));
                }
                // `content` is HTML-escaped HTML: unescape, then strip.
                text.push('\n');
                text.push_str(&html_to_text(&unescape(&job.content)));
                Ok(text)
            }
            Board::Lever { company, .. } => {
                let posting: LeverPosting = serde_json::from_str(body).map_err(parse_err)?;
                let mut text = format!("company: {company}\ntitle: {}\n", posting.text);
                if let Some(categories) = posting.categories {
                    for value in [categories.location, categories.team, categories.commitment]
                        .into_iter()
                        .flatten()
                    {
                        text.push_str(&format!("{value}\n"));
                    }
                }
                text.push('\n');
                text.push_str(&posting.description_plain);
                for list in posting.lists {
                    text.push_str(&format!("\n\n{}\n", list.text));
                    text.push_str(&html_to_text(&list.content));
                }
                if !posting.additional_plain.is_empty() {
                    text.push_str("\n\n");
                    text.push_str(&posting.additional_plain);
                }
                Ok(text)
            }
        }
    }
}

// The slices of each API's response that aarg actually reads. Everything
// is lenient (`default`) — boards add fields freely.

#[derive(Debug, Deserialize)]
struct GreenhouseJob {
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
    location: Option<GreenhouseLocation>,
}

#[derive(Debug, Deserialize)]
struct GreenhouseLocation {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct LeverPosting {
    /// Lever calls the posting title "text".
    #[serde(default)]
    text: String,
    categories: Option<LeverCategories>,
    #[serde(default, rename = "descriptionPlain")]
    description_plain: String,
    #[serde(default)]
    lists: Vec<LeverList>,
    #[serde(default, rename = "additionalPlain")]
    additional_plain: String,
}

#[derive(Debug, Deserialize)]
struct LeverCategories {
    location: Option<String>,
    team: Option<String>,
    commitment: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LeverList {
    /// The list's heading, e.g. "Requirements".
    #[serde(default)]
    text: String,
    /// `<li>` items as an HTML fragment.
    #[serde(default)]
    content: String,
}

/// Strip HTML down to readable text: `<li>` becomes a bullet, block-end
/// tags become newlines, everything else inside angle brackets vanishes,
/// and entities are decoded. Not a general HTML parser — just enough for
/// job-board content fields, which is all it ever sees.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            // Unclosed tag: keep the text as-is and stop scanning.
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let tag = rest[start + 1..start + end].trim_start_matches('/');
        let name: String = tag
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        match name.as_str() {
            "li" if !tag.starts_with('/') && !rest[start + 1..].starts_with('/') => {
                out.push_str("\n- ");
            }
            "p" | "br" | "div" | "ul" | "ol" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                out.push('\n');
            }
            _ => {}
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);

    // Collapse runaway blank lines and per-line whitespace.
    let lines: Vec<String> = unescape(&out)
        .lines()
        .map(str::trim)
        .map(str::to_string)
        .collect();
    let mut text = String::new();
    let mut blank_run = 0;
    for line in lines {
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        text.push_str(&line);
        text.push('\n');
    }
    text.trim().to_string()
}

/// Decode the handful of HTML entities job boards actually emit.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// FNV-1a, 64-bit: a tiny, stable hash for cache filenames. Implemented
/// here (10 lines) rather than using `std`'s hasher, whose output may
/// change between Rust versions — a cache key must not.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// `~/.cache/aarg/jd_cache` (per-OS equivalent).
fn cache_dir() -> Result<PathBuf, FetchError> {
    ProjectDirs::from("", "", "aarg")
        .map(|dirs| dirs.cache_dir().join("jd_cache"))
        .ok_or(FetchError::NoHomeDir)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn greenhouse_and_lever_urls_are_recognized() {
        assert_eq!(
            classify("https://boards.greenhouse.io/acme/jobs/12345?gh_src=link"),
            Some(Board::Greenhouse {
                company: "acme".into(),
                job_id: "12345".into()
            })
        );
        assert_eq!(
            classify("http://job-boards.greenhouse.io/acme/jobs/12345"),
            Some(Board::Greenhouse {
                company: "acme".into(),
                job_id: "12345".into()
            })
        );
        assert_eq!(
            classify("https://jobs.lever.co/acme/abc-123-def"),
            Some(Board::Lever {
                company: "acme".into(),
                posting_id: "abc-123-def".into()
            })
        );
    }

    #[test]
    fn everything_else_is_unsupported() {
        for url in [
            "https://jobs.ashbyhq.com/amplo/some-id",
            "https://example.com/careers/123",
            "https://boards.greenhouse.io/acme",
            "https://boards.greenhouse.io/acme/jobs/1/extra",
            "not a url at all",
            "ftp://boards.greenhouse.io/acme/jobs/1",
        ] {
            assert_eq!(classify(url), None, "{url:?} should not classify");
        }
    }

    #[test]
    fn html_becomes_readable_text() {
        let html = "<div><p>We build &amp; ship.</p><ul><li>8+ years</li><li>SaaS &quot;scale&quot;</li></ul></div>";
        let text = html_to_text(html);
        assert_eq!(text, "We build & ship.\n\n- 8+ years\n- SaaS \"scale\"");
    }

    #[test]
    fn greenhouse_responses_unescape_then_strip() {
        let board = Board::Greenhouse {
            company: "acme".into(),
            job_id: "1".into(),
        };
        let body = r#"{"title": "Staff Engineer",
                        "content": "&lt;p&gt;Pay range &amp;amp; benefits&lt;/p&gt;",
                        "location": {"name": "NYC"}}"#;
        let text = board.extract(body).unwrap();
        assert!(text.contains("company: acme"));
        assert!(text.contains("title: Staff Engineer"));
        assert!(text.contains("location: NYC"));
        assert!(text.contains("Pay range & benefits"));
        assert!(!text.contains("&lt;"));
    }

    #[test]
    fn lever_responses_compose_plain_sections_and_lists() {
        let board = Board::Lever {
            company: "acme".into(),
            posting_id: "x".into(),
        };
        let body = r#"{"text": "Platform Engineer",
                        "categories": {"location": "Remote", "team": "Infra", "commitment": "Full-time"},
                        "descriptionPlain": "We run the platform.",
                        "lists": [{"text": "Requirements", "content": "<li>Rust</li><li>Kubernetes</li>"}],
                        "additionalPlain": "Benefits included."}"#;
        let text = board.extract(body).unwrap();
        assert!(text.contains("title: Platform Engineer"));
        assert!(text.contains("Remote"));
        assert!(text.contains("We run the platform."));
        assert!(text.contains("Requirements"));
        assert!(text.contains("- Rust"));
        assert!(text.contains("Benefits included."));
    }

    #[test]
    fn a_garbled_response_is_a_typed_parse_error() {
        let board = Board::Greenhouse {
            company: "acme".into(),
            job_id: "1".into(),
        };
        assert!(matches!(
            board.extract("<html>503 maintenance</html>"),
            Err(FetchError::Parse { .. })
        ));
    }

    #[tokio::test]
    async fn cache_hits_never_touch_the_network() {
        let cache = tempfile::tempdir().unwrap();
        let url = "https://boards.greenhouse.io/acme/jobs/12345";
        std::fs::write(
            cache
                .path()
                .join(format!("{:016x}.txt", fnv1a(url.as_bytes()))),
            "cached jd text",
        )
        .unwrap();

        // No HTTP server exists; only a cache hit can succeed.
        let text = fetch_jd_with(&reqwest::Client::new(), cache.path(), url)
            .await
            .unwrap();
        assert_eq!(text, "cached jd text");
    }

    #[tokio::test]
    async fn unsupported_urls_fail_before_any_io() {
        let cache = tempfile::tempdir().unwrap();
        let err = fetch_jd_with(
            &reqwest::Client::new(),
            cache.path(),
            "https://example.com/jobs/1",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, FetchError::UnsupportedUrl { .. }));
    }

    #[tokio::test]
    async fn the_fetch_tool_rejects_bad_args_and_unsupported_urls() {
        // Both failures come back as ToolError (fed to the model as
        // error results), and neither touches the network.
        let err = FetchJdTool
            .call(serde_json::json!({"link": "oops"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("url"));

        let err = FetchJdTool
            .call(serde_json::json!({"url": "https://example.com/x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Greenhouse or Lever"));
    }

    #[test]
    fn fnv1a_is_stable_forever() {
        // Pinned value: if this ever changes, every cached JD is orphaned.
        assert_eq!(fnv1a(b"hello"), 0xa430_d846_80aa_bd0b);
    }

    #[test]
    #[ignore = "exercise: the amplo demo JD lives on Ashby (jobs.ashbyhq.com), which aarg can't fetch; add Ashby's posting API as a third board, then finish this test"]
    fn ex_013_ashby_urls_are_recognized() {
        // Once Ashby support exists: classify a jobs.ashbyhq.com URL,
        // assert the variant and its API URL, and extract a fixture
        // response into text containing the title and description.
        let ashby_implemented = false;
        assert!(ashby_implemented);
    }
}
