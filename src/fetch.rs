//! Fetching job descriptions from supported job boards (FR-1.4).
//!
//! Greenhouse, Lever, and Ashby publish documented, auth-free JSON APIs
//! for public postings, and LinkedIn serves a guest HTML fragment; those
//! four are fetched. Reading a known shape beats scraping pages that can
//! change any day. Any other URL is a typed error telling the user to
//! paste the text; an allowlist that fails honestly is worth more than a
//! generic fetcher that returns nav-bar soup. Indeed gets its own error:
//! a Cloudflare challenge blocks every non-browser client, so aarg names
//! the workaround instead of pretending the host is unknown.
//!
//! Fetched JDs are cached by URL hash (`~/.cache/aarg/jd_cache`), so
//! re-running `gap` or `tailor` against the same posting is free and
//! works offline.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::agent::{Tool, ToolError};

/// Everything that can go wrong while fetching a JD.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error(
        "aarg can only fetch JDs from Greenhouse, Lever, LinkedIn, or Ashby job boards; {url} is none of those"
    )]
    UnsupportedUrl { url: String },

    #[error(
        "Indeed blocks automated fetching; open {url} in your browser and paste the posting text instead"
    )]
    IndeedBlocked { url: String },

    #[error("could not determine this user's home directory")]
    NoHomeDir,

    #[error("could not build the HTTP client for fetching {url}")]
    Client {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("aarg couldn't read LinkedIn's posting page at {url}; its layout may have changed")]
    LinkedIn { url: String },

    #[error(
        "Ashby's board for {org} no longer lists posting {job_id}; the posting may have been taken down"
    )]
    Ashby { org: String, job_id: String },

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
                "description": "A Greenhouse (boards.greenhouse.io), Lever (jobs.lever.co), LinkedIn (linkedin.com/jobs/view), or Ashby (jobs.ashbyhq.com) posting URL"
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
        "Fetch the text of a job posting from a Greenhouse, Lever, LinkedIn, or Ashby URL"
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
    LinkedIn { job_id: String },
    Ashby { org: String, job_id: String },
}

/// Fetch (or recall from cache) the text of a job posting.
pub async fn fetch_jd(url: &str) -> Result<String, FetchError> {
    let cache = cache_dir()?;
    let http = http_client(url)?;
    fetch_jd_with(&http, &cache, url).await
}

/// The module's HTTP client: a 20-second timeout so a job board that hangs
/// can't stall the whole run, rather than the default (no timeout at all).
/// `build` can fail if the runtime's TLS backend won't initialize, so it
/// returns a `Result` we surface as a typed error instead of unwrapping.
fn http_client(url: &str) -> Result<reqwest::Client, FetchError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|source| FetchError::Client {
            url: url.to_string(),
            source,
        })
}

/// The testable core: HTTP client and cache directory are parameters.
async fn fetch_jd_with(
    http: &reqwest::Client,
    cache_dir: &Path,
    url: &str,
) -> Result<String, FetchError> {
    // Indeed is recognized but never fetched: its Cloudflare challenge
    // blocks every non-browser client, so fail before any I/O and name
    // the workaround.
    if is_indeed(url) {
        return Err(FetchError::IndeedBlocked {
            url: url.to_string(),
        });
    }
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
        .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
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

/// True when the URL points at Indeed: `indeed.com` or any subdomain
/// (`www.`, `m.`, country hosts like `uk.`), any path. Indeed has no
/// `Board` variant because there is nothing to fetch; this check exists
/// only so `fetch_jd_with` can explain why instead of calling the host
/// unknown.
fn is_indeed(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    host == "indeed.com" || host.ends_with(".indeed.com")
}

/// Recognize a posting URL. Hand-rolled rather than pulling a URL crate:
/// the accepted shapes are rigid enough that scheme-strip + segment
/// split is the whole job.
///
/// - `https://boards.greenhouse.io/<company>/jobs/<id>`
///   (also the newer `job-boards.greenhouse.io`)
/// - `https://jobs.lever.co/<company>/<posting-id>`
/// - `https://www.linkedin.com/jobs/view/<slug-or-id>` where the last
///   path segment ends in the numeric posting id (a bare id, or a
///   hyphenated slug like `...-at-prepass-4395937732`)
/// - `https://jobs.ashbyhq.com/<org>/<posting-uuid>`
// EXERCISE(EX-021)
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
        "jobs.ashbyhq.com" => match segments.as_slice() {
            [org, job_id] => Some(Board::Ashby {
                org: (*org).to_string(),
                job_id: (*job_id).to_string(),
            }),
            _ => None,
        },
        host if host == "linkedin.com" || host.ends_with(".linkedin.com") => {
            // Any LinkedIn subdomain works: phones share m.linkedin.com
            // links, Google serves country ones (uk.linkedin.com), and
            // email notifications add a /comm prefix to the same path.
            match segments.as_slice() {
                // The last segment is either the bare numeric id or a slug
                // that ends in it; pull the trailing digits and reject if
                // there are none (some other LinkedIn page, not a posting).
                ["jobs", "view", segment] | ["comm", "jobs", "view", segment] => {
                    linkedin_job_id(segment).map(|job_id| Board::LinkedIn { job_id })
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Pull the trailing run of digits off a LinkedIn `jobs/view` segment.
/// Works for a bare id (`4395937732`) and for a slug that ends in one
/// (`director-of-software-engineering-at-prepass-4395937732`). Returns
/// `None` when the segment has no trailing digits at all.
fn linkedin_job_id(segment: &str) -> Option<String> {
    // Take digits from the end, then flip them back into reading order.
    let reversed: String = segment
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect();
    if reversed.is_empty() {
        None
    } else {
        Some(reversed.chars().rev().collect())
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
            Board::LinkedIn { job_id } => {
                format!("https://www.linkedin.com/jobs-guest/jobs/api/jobPosting/{job_id}")
            }
            // Ashby has no single-posting endpoint; the board URL lists
            // every open posting for the org, and extract picks by id.
            Board::Ashby { org, .. } => {
                format!("https://api.ashbyhq.com/posting-api/job-board/{org}")
            }
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
            // LinkedIn's guest endpoint answers with an HTML fragment, not
            // JSON, so there's no serde here: we scan the markup by hand.
            Board::LinkedIn { .. } => {
                let linkedin_err = || FetchError::LinkedIn {
                    url: self.api_url(),
                };

                // The description is the one field we require. Its markup
                // div holds nested p/ul/li/strong, so run it through the
                // same HTML-to-text pass the other boards use.
                let markup =
                    div_inner_html(body, "show-more-less-html__markup").ok_or_else(linkedin_err)?;
                // Unlike Greenhouse's double-encoded `content`, this fragment
                // is raw HTML: entities inside it are literal JD text (a
                // requirement like "Vec&lt;String&gt;" must survive). So no
                // unescape pass before tag stripping; html_to_text decodes
                // entities once, at the end.
                let description = html_to_text(markup);

                // The three topcard fields are best-effort: each is plain
                // text sitting directly inside a leaf element, so read from
                // the class to the next tag.
                let company = text_inside_class(body, "topcard__org-name-link");
                let title = text_inside_class(body, "topcard__title");
                let location = text_inside_class(body, "topcard__flavor--bullet");

                // Compose "company - title (location)" from whatever fields
                // came through, then the description below it.
                let mut header = String::new();
                if let Some(company) = &company {
                    header.push_str(company);
                }
                if let Some(title) = &title {
                    if !header.is_empty() {
                        header.push_str(" - ");
                    }
                    header.push_str(title);
                }
                if let Some(location) = &location {
                    header.push_str(&format!(" ({location})"));
                }

                let mut text = String::new();
                if !header.is_empty() {
                    text.push_str(header.trim());
                    text.push_str("\n\n");
                }
                text.push_str(&description);

                // A near-empty body means the markup div was there but the
                // scan came up short (a layout change, an interstitial):
                // fail loudly rather than hand the parser nothing.
                if text.trim().chars().count() < 100 {
                    return Err(linkedin_err());
                }
                Ok(text)
            }
            Board::Ashby { org, job_id } => {
                let board: AshbyBoard = serde_json::from_str(body).map_err(parse_err)?;
                // The response is the whole board, so search it for the
                // posting the URL named. A miss means the posting is no
                // longer listed, which deserves its own error.
                let job = board
                    .jobs
                    .into_iter()
                    .find(|job| job.id == *job_id)
                    .ok_or_else(|| FetchError::Ashby {
                        org: org.clone(),
                        job_id: job_id.clone(),
                    })?;
                let mut text = format!("{org} - {}", job.title);
                if !job.location.is_empty() {
                    text.push_str(&format!(" ({})", job.location));
                }
                text.push_str("\n\n");
                text.push_str(&job.description_plain);
                Ok(text)
            }
        }
    }
}

/// Return the inner HTML of the first element whose opening tag carries
/// `class_marker`, balancing nested `<div>`s so a child div can't end the
/// scan early. `None` if the class isn't found or the tags don't balance.
fn div_inner_html<'a>(html: &'a str, class_marker: &str) -> Option<&'a str> {
    // Find the class, then the `>` that closes its opening tag. The inner
    // HTML starts right after that `>`.
    let marker = html.find(class_marker)?;
    let open_end = marker + html[marker..].find('>')? + 1;

    // Walk forward, counting `<div` openings against `</div` closings, until
    // the matching close brings the depth back to zero.
    let mut depth = 1usize;
    let mut pos = open_end;
    while depth > 0 {
        let next_close = html[pos..].find("</div")?;
        match html[pos..].find("<div") {
            Some(next_open) if next_open < next_close => {
                depth += 1;
                pos += next_open + "<div".len();
            }
            _ => {
                depth -= 1;
                if depth == 0 {
                    return Some(&html[open_end..pos + next_close]);
                }
                pos += next_close + "</div".len();
            }
        }
    }
    None
}

/// Return the plain text sitting directly inside the first element whose
/// opening tag carries `class_marker` — from that tag's `>` to the next `<`.
/// Good enough for LinkedIn's leaf topcard fields, which hold text with no
/// child tags. `None` if the class is absent or the text is blank.
fn text_inside_class(html: &str, class_marker: &str) -> Option<String> {
    let marker = html.find(class_marker)?;
    let open_end = marker + html[marker..].find('>')? + 1;
    let close = html[open_end..].find('<')?;
    let text = unescape(html[open_end..open_end + close].trim());
    if text.is_empty() { None } else { Some(text) }
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

#[derive(Debug, Deserialize)]
struct AshbyBoard {
    #[serde(default)]
    jobs: Vec<AshbyJob>,
}

#[derive(Debug, Deserialize)]
struct AshbyJob {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    location: String,
    /// Ashby ships the description already flattened to plain text, so
    /// no HTML pass is needed.
    #[serde(default, rename = "descriptionPlain")]
    description_plain: String,
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

/// `jd_cache/` under the active workspace's cache directory (the `.aarg/`
/// workspace, else `~/.cache/aarg`). Resolved by the `workspace` module.
fn cache_dir() -> Result<PathBuf, FetchError> {
    crate::workspace::cache_dir()
        .map(|dir| dir.join("jd_cache"))
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
    fn linkedin_urls_are_recognized() {
        // Bare id, with and without a trailing slash.
        assert_eq!(
            classify("https://www.linkedin.com/jobs/view/4395937732"),
            Some(Board::LinkedIn {
                job_id: "4395937732".into()
            })
        );
        assert_eq!(
            classify("https://www.linkedin.com/jobs/view/4395937732/"),
            Some(Board::LinkedIn {
                job_id: "4395937732".into()
            })
        );
        // Phone shares (m.), country subdomains, and email /comm links.
        assert_eq!(
            classify("https://m.linkedin.com/jobs/view/4395937732"),
            Some(Board::LinkedIn {
                job_id: "4395937732".into()
            })
        );
        assert_eq!(
            classify("https://uk.linkedin.com/jobs/view/4395937732"),
            Some(Board::LinkedIn {
                job_id: "4395937732".into()
            })
        );
        assert_eq!(
            classify("https://www.linkedin.com/comm/jobs/view/4395937732"),
            Some(Board::LinkedIn {
                job_id: "4395937732".into()
            })
        );
        // Hyphenated slug ending in the id, and the bare `linkedin.com` host.
        assert_eq!(
            classify(
                "https://www.linkedin.com/jobs/view/director-of-software-engineering-at-prepass-4395937732"
            ),
            Some(Board::LinkedIn {
                job_id: "4395937732".into()
            })
        );
        assert_eq!(
            classify("https://linkedin.com/jobs/view/4395937732?refId=abc&trk=xyz"),
            Some(Board::LinkedIn {
                job_id: "4395937732".into()
            })
        );
    }

    #[test]
    fn everything_else_is_unsupported() {
        for url in [
            "https://example.com/careers/123",
            "https://boards.greenhouse.io/acme",
            "https://boards.greenhouse.io/acme/jobs/1/extra",
            "not a url at all",
            "ftp://boards.greenhouse.io/acme/jobs/1",
            // LinkedIn pages that aren't job postings, or a slug with no id.
            "https://www.linkedin.com/feed/",
            "https://www.linkedin.com/jobs/view/software-engineer",
            "https://www.linkedin.com/in/someone",
            // Ashby pages that aren't a single posting.
            "https://jobs.ashbyhq.com/amplo",
            "https://jobs.ashbyhq.com/amplo/some-id/application",
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
    fn linkedin_api_url_is_the_guest_endpoint() {
        let board = Board::LinkedIn {
            job_id: "4395937732".into(),
        };
        assert_eq!(
            board.api_url(),
            "https://www.linkedin.com/jobs-guest/jobs/api/jobPosting/4395937732"
        );
    }

    // A trimmed but realistic guest-endpoint fragment: the topcard fields
    // plus a description with entities and a list.
    const LINKEDIN_FRAGMENT: &str = r#"
<div class="top-card-layout__card">
  <h2 class="topcard__title">Director of Software Engineering</h2>
  <a class="topcard__org-name-link" href="https://www.linkedin.com/company/prepass">PrePass</a>
  <span class="topcard__flavor topcard__flavor--bullet">Phoenix, Arizona, United States</span>
  <span class="topcard__flavor topcard__flavor--metadata">2 weeks ago</span>
</div>
<div class="show-more-less-html">
  <div class="show-more-less-html__markup show-more-less-html__markup--clamp-after-5">
    <p>We are looking for a <strong>Director of Software Engineering</strong> to lead &amp; scale our platform team.</p>
    <p>You will own architecture, mentoring, and delivery across several squads.</p>
    <ul>
      <li>10+ years building production software</li>
      <li>Deep experience with Rust &amp; distributed systems</li>
      <li>Comfort with Vec&lt;String&gt; generics; &lt;5 years tenure is fine</li>
    </ul>
    <p>Join us at PrePass to keep freight moving.</p>
  </div>
</div>"#;

    #[test]
    fn linkedin_fragment_composes_header_and_description() {
        let board = Board::LinkedIn {
            job_id: "4395937732".into(),
        };
        let text = board.extract(LINKEDIN_FRAGMENT).unwrap();
        assert!(text.starts_with(
            "PrePass - Director of Software Engineering (Phoenix, Arizona, United States)"
        ));
        assert!(text.contains("lead & scale our platform team."));
        assert!(text.contains("- 10+ years building production software"));
        assert!(text.contains("- Deep experience with Rust & distributed systems"));
        assert!(text.contains("Join us at PrePass"));
        // Escaped entities in the fragment are literal JD text and must
        // survive tag stripping intact (the tags themselves must not).
        assert!(text.contains("Vec<String>"));
        assert!(text.contains("<5 years tenure"));
        assert!(!text.contains("<li"));
        assert!(!text.contains("</"));
        // The metadata flavor (posted date) is not the bullet flavor.
        assert!(!text.contains("2 weeks ago"));
    }

    #[test]
    fn linkedin_missing_markup_is_a_loud_error() {
        let board = Board::LinkedIn { job_id: "1".into() };
        let no_markup = r#"<div class="top-card-layout__card">
            <h2 class="topcard__title">Some Role</h2></div>"#;
        assert!(matches!(
            board.extract(no_markup),
            Err(FetchError::LinkedIn { .. })
        ));
    }

    #[test]
    fn linkedin_short_description_is_a_loud_error() {
        let board = Board::LinkedIn { job_id: "1".into() };
        let thin = r#"<div class="show-more-less-html__markup"><p>Apply now.</p></div>"#;
        assert!(matches!(
            board.extract(thin),
            Err(FetchError::LinkedIn { .. })
        ));
    }

    #[test]
    fn a_garbled_response_is_a_typed_parse_error() {
        let greenhouse = Board::Greenhouse {
            company: "acme".into(),
            job_id: "1".into(),
        };
        assert!(matches!(
            greenhouse.extract("<html>503 maintenance</html>"),
            Err(FetchError::Parse { .. })
        ));

        let ashby = Board::Ashby {
            org: "amplo".into(),
            job_id: "1".into(),
        };
        assert!(matches!(
            ashby.extract("<html>503 maintenance</html>"),
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
        assert!(
            err.to_string()
                .contains("Greenhouse, Lever, LinkedIn, or Ashby")
        );
    }

    #[test]
    fn fnv1a_is_stable_forever() {
        // Pinned value: if this ever changes, every cached JD is orphaned.
        assert_eq!(fnv1a(b"hello"), 0xa430_d846_80aa_bd0b);
    }

    #[test]
    fn ex_013_ashby_urls_are_recognized() {
        // Plain, with a query string, and with a fragment: none of the
        // decorations identify the posting.
        for url in [
            "https://jobs.ashbyhq.com/amplo/aabbccdd-1122-3344-5566-77889900aabb",
            "https://jobs.ashbyhq.com/amplo/aabbccdd-1122-3344-5566-77889900aabb?utm_source=x",
            "https://jobs.ashbyhq.com/amplo/aabbccdd-1122-3344-5566-77889900aabb#content",
        ] {
            assert_eq!(
                classify(url),
                Some(Board::Ashby {
                    org: "amplo".into(),
                    job_id: "aabbccdd-1122-3344-5566-77889900aabb".into()
                }),
                "{url:?} should classify as Ashby"
            );
        }
    }

    #[test]
    fn ashby_api_url_is_the_org_board_endpoint() {
        let board = Board::Ashby {
            org: "amplo".into(),
            job_id: "aabbccdd-1122-3344-5566-77889900aabb".into(),
        };
        assert_eq!(
            board.api_url(),
            "https://api.ashbyhq.com/posting-api/job-board/amplo"
        );
    }

    // A trimmed board response: Ashby's posting API returns every open
    // posting for the org, so the fixture carries two and extract must
    // pick the one the URL named.
    const ASHBY_FRAGMENT: &str = r#"{"jobs": [
        {"id": "11111111-aaaa-bbbb-cccc-222222222222",
         "title": "Platform Engineer",
         "location": "Remote",
         "descriptionPlain": "Run the platform.",
         "descriptionHtml": "<p>Run the platform.</p>",
         "jobUrl": "https://jobs.ashbyhq.com/amplo/11111111-aaaa-bbbb-cccc-222222222222"},
        {"id": "33333333-dddd-eeee-ffff-444444444444",
         "title": "Founding Engineer",
         "location": "Utrecht",
         "descriptionPlain": "Build the product end to end.",
         "descriptionHtml": "<p>Build the product end to end.</p>",
         "jobUrl": "https://jobs.ashbyhq.com/amplo/33333333-dddd-eeee-ffff-444444444444"}
    ]}"#;

    #[test]
    fn ashby_responses_pick_the_posting_by_id() {
        let board = Board::Ashby {
            org: "amplo".into(),
            job_id: "33333333-dddd-eeee-ffff-444444444444".into(),
        };
        let text = board.extract(ASHBY_FRAGMENT).unwrap();
        assert_eq!(
            text,
            "amplo - Founding Engineer (Utrecht)\n\nBuild the product end to end."
        );
    }

    #[test]
    fn an_ashby_posting_missing_from_the_board_is_a_loud_error() {
        // The URL was valid and the response parsed; the posting just
        // isn't on the board anymore.
        let board = Board::Ashby {
            org: "amplo".into(),
            job_id: "99999999-0000-1111-2222-333333333333".into(),
        };
        assert!(matches!(
            board.extract(ASHBY_FRAGMENT),
            Err(FetchError::Ashby { .. })
        ));
    }

    #[tokio::test]
    async fn indeed_urls_fail_before_any_io_with_the_blocked_error() {
        // No HTTP server exists and the cache is empty, so anything but
        // an early typed error would surface as a connection failure.
        let cache = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        for url in [
            "https://www.indeed.com/viewjob?jk=abc123",
            "https://m.indeed.com/viewjob?jk=abc123",
            "https://uk.indeed.com/viewjob?jk=abc123",
            "https://indeed.com/jobs?q=rust",
            "http://de.indeed.com/rc/clk?jk=abc123",
        ] {
            let err = fetch_jd_with(&http, cache.path(), url).await.unwrap_err();
            assert!(
                matches!(err, FetchError::IndeedBlocked { .. }),
                "{url:?} should be recognized as Indeed"
            );
            assert!(err.to_string().contains("paste the posting text"));
        }

        // A lookalike host is not Indeed and stays plainly unsupported.
        let err = fetch_jd_with(&http, cache.path(), "https://notindeed.com/viewjob")
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::UnsupportedUrl { .. }));
    }

    #[test]
    #[ignore = "exercise: add SmartRecruiters (careers.smartrecruiters.com/{company}/{id-slug}, API https://api.smartrecruiters.com/v1/companies/{company}/postings/{id}) as a board, then finish this test"]
    fn ex_021_smartrecruiters_urls_are_recognized() {
        // Once SmartRecruiters support exists: classify a
        // careers.smartrecruiters.com posting URL, assert the variant
        // and its API URL, and extract a fixture response into text
        // containing the title and description.
        let smartrecruiters_implemented = false;
        assert!(smartrecruiters_implemented);
    }
}
