//! `aarg serve` — the HTTP companion server for a browser UI.
//!
//! The wasm build (`crates/aarg-wasm`) runs AARG's whole domain pipeline in
//! the browser, but four things a browser page genuinely cannot do stay on
//! this side of a localhost socket:
//!
//! 1. **Hold the API key.** The key lives in the OS keychain; a page can't
//!    read it, and shipping it to JS would leak it. `POST /api/llm` proxies a
//!    single completion through the same credential resolution the CLI uses.
//! 2. **Run typst.** Rendering shells out to the `typst` binary, which wasm
//!    has no way to invoke. `POST /api/render` stages a payload + template and
//!    returns the PDF bytes.
//! 3. **Read/write the workspace.** The dataset and past builds are on disk.
//!    `/api/dataset` and `/api/builds/...` read and write them.
//! 4. **Fetch a cross-origin JD.** A browser can't fetch a Greenhouse/Lever
//!    posting (CORS); `POST /api/fetch-jd` does it server-side.
//!
//! **Hand-rolled, framework-free.** This is the same portfolio move as the
//! MCP server ([`crate::mcp`]): the wire handling is written directly against
//! `hyper` — no axum/warp/actix — one tokio task per connection, typed errors,
//! and every log line on stderr. `hyper` is already a transitive dependency
//! (via `reqwest`), so promoting it to a direct one adds no new crates.
//!
//! **Security model (v1): same-origin, enforced.** The listener binds
//! `127.0.0.1` and nothing else, so only processes on this machine can reach
//! the socket at all — but a loopback bind alone doesn't stop a *browser*
//! from being tricked into talking to it, so [`handle`] enforces same-origin
//! itself, before any route runs:
//! - **A `Host` allowlist.** Every request's `Host` header must name this
//!   server (`127.0.0.1`/`localhost`, with or without the actual bound
//!   port) or it's rejected with `403`. This is what actually stops DNS
//!   rebinding: an attacker's page served from `attacker.com`, pointed at a
//!   DNS name that resolves to `127.0.0.1`, would otherwise look
//!   same-origin to the browser and same-loopback to us — the `Host` header
//!   is the one thing that still reveals the browser thinks it's talking to
//!   `attacker.com`.
//! - **A `Content-Type` gate on JSON routes.** `POST /api/llm`,
//!   `POST /api/render`, `PUT /api/dataset`, and `POST /api/fetch-jd` all
//!   require `Content-Type: application/json` (a `;charset=` suffix is
//!   fine) or they're rejected with `415`. A cross-origin form or `fetch`
//!   using a "simple" content type (`text/plain`, form-urlencoded,
//!   multipart) is sent by the browser *without* a CORS preflight, so a
//!   handler that happily parsed JSON out of such a body could be driven
//!   blind by any web page the victim has open. Demanding
//!   `application/json` forces a preflight, and since this server sends no
//!   CORS headers back, the preflight fails and the real request never
//!   goes out.
//!
//! Together these close the two holes a bind-only story leaves open: a
//! drive-by cross-origin `POST` (blocked by the content-type gate) and DNS
//! rebinding (blocked by the `Host` check). There is still deliberately no
//! CORS header — the browser app is expected to be served by *this* server
//! (`--dir`), same-origin, so no other web page can read the dataset or
//! spend the key. Broadening beyond localhost is a later slice.
//!
//! **Streaming.** `POST /api/llm` returns the whole completion in one buffered
//! response by default, but a client that sends `Accept: text/event-stream`
//! (and whose request carries no tools) gets a server-sent-events stream
//! instead, one frame per token as the model produces it. A tool-bearing
//! request always takes the buffered path, because the SSE parser drops
//! non-text content deltas and would lose the tool calls. Every other route is
//! buffered.
//!
//! The submodules split the surface the way [`crate::mcp`] does: [`routes`]
//! holds the JSON API handlers (thin adapters over the same library services
//! the CLI commands call), [`statics`] serves the browser app's files.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::commands::CliError;
use crate::style;

mod embedded;
mod routes;
mod statics;

/// The default port when `--port` is omitted. A high, unregistered port so it
/// rarely collides with something already running.
const DEFAULT_PORT: u16 = 8787;

/// The response body type every handler produces. Most handlers return a whole
/// buffered blob of bytes with a known length (a `Full`), so `hyper` sets
/// `Content-Length` for us; the one exception is `POST /api/llm` in SSE mode,
/// which returns a `StreamBody` that emits one frame per token as the model
/// produces it. Both erase into this boxed body so every handler shares one
/// return type. `UnsyncBoxBody` (not the `Sync`-bound `BoxBody`) because the
/// LLM `TokenStream` the SSE path wraps is `Send` but not `Sync`; hyper only
/// needs the response body to be `Send`, so nothing is lost.
type Bytes = hyper::body::Bytes;
type Body = http_body_util::combinators::UnsyncBoxBody<Bytes, std::convert::Infallible>;
type Resp = Response<Body>;

/// What the server can fail with. Only *bootstrap* failures surface as a
/// `ServeError` — binding the socket, or a bad `--dir`. Once the accept loop
/// is running, a per-connection or per-request failure is turned into an HTTP
/// error response (or logged and skipped), never propagated out, so one bad
/// client can't take the server down.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("could not bind the server to {addr} (is the port already in use?)")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },

    #[error("--dir {path} is not a readable directory")]
    StaticDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Where `aarg serve` gets the browser app's files, resolved once at startup.
#[derive(Clone)]
pub(super) enum StaticSource {
    /// A directory on disk, canonicalized, from an explicit `--dir`. Its path
    /// is the prefix every served file is checked against for traversal.
    Dir(Arc<PathBuf>),
    /// The app baked into this binary at build time (see [`embedded`]). The
    /// default when no `--dir` is given and a web dist was embedded.
    Embedded,
    /// Nothing to serve: API-only. No `--dir`, and no app was embedded (a
    /// clone built before the web app was compiled).
    None,
}

/// Everything a request handler needs, cheap to `clone` into each connection
/// task: an `Arc`-shared static source and a write-serializing lock.
#[derive(Clone)]
struct AppState {
    /// Where static files come from: an on-disk `--dir`, the embedded app, or
    /// nothing (API-only).
    source: StaticSource,
    /// Serializes `PUT /api/dataset` writes. The on-disk store already takes an
    /// advisory file lock (so it can never *corrupt* on a race — a second
    /// writer fails fast with `Locked`), but two concurrent browser saves
    /// racing that lock would make one spuriously 500. Holding this async mutex
    /// across validate-then-save turns a race into an orderly queue instead.
    dataset_write: Arc<tokio::sync::Mutex<()>>,
    /// Serializes `POST /api/builds/:id/edits` writes. Unlike the dataset store,
    /// build directories take no file lock at all: the handler read-modifies-
    /// writes `canonical.json` + `edit_log.json`, so two concurrent saves or
    /// reverts (two tabs on the same build) would silently lose the loser's
    /// edit and log entries — last write wins, and both clients see a 200.
    /// Holding this mutex across the whole read-apply-write-render sequence
    /// turns that race into an orderly queue. One mutex for ALL builds (not a
    /// per-id map) is deliberate: this is a single-user localhost server where
    /// build edits are rare and brief, so cross-build contention is negligible
    /// and one lock keeps the state trivially simple.
    build_write: Arc<tokio::sync::Mutex<()>>,
    /// The port the listener actually bound to. Read from `TcpListener::local_addr`
    /// rather than trusted from the `--port` argument, so the `Host` allowlist
    /// (see [`host_is_allowed`]) is correct even when the OS picks the port —
    /// as an ephemeral-port test does.
    bound_port: u16,
    /// Extra `Host` names the allowlist accepts beyond `127.0.0.1`/`localhost`,
    /// populated only when the server is bound past loopback (`--bind` +
    /// `--allow-host`, plus this machine's own hostname). Empty on a loopback
    /// bind, so the default posture is unchanged.
    allowed_hosts: Arc<Vec<String>>,
}

/// Run the server until the process is interrupted. Binds `127.0.0.1:<port>`,
/// prints where it's listening, and serves connections one tokio task each.
/// This does not return on its own — the accept loop runs forever — so the
/// caller (the CLI dispatch) blocks here until Ctrl-C.
pub async fn run(
    bind: Option<std::net::IpAddr>,
    port: Option<u16>,
    allow_hosts: Vec<String>,
    dir: Option<PathBuf>,
) -> Result<(), CliError> {
    let port = port.unwrap_or(DEFAULT_PORT);
    // Default stays loopback: only an explicit `--bind` opens the server to the
    // network. `0.0.0.0` binds every interface (the usual way to reach it from
    // another device); a specific LAN IP binds just that one.
    let bind = bind.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let addr = SocketAddr::new(bind, port);

    // Resolve where the browser app comes from. An explicit `--dir` wins: it's
    // canonicalized once up front (it's the prefix every served path is checked
    // against to refuse directory traversal, so it must be absolute and real; a
    // missing/unreadable dir is a clear bootstrap error, not a 404 per request
    // later). With no `--dir`, serve the app baked into the binary when one was
    // embedded at build time, otherwise run API-only.
    let source = match dir {
        Some(dir) => {
            let canonical = dir
                .canonicalize()
                .map_err(|source| ServeError::StaticDir { path: dir, source })?;
            StaticSource::Dir(Arc::new(canonical))
        }
        None if embedded::available() => StaticSource::Embedded,
        None => StaticSource::None,
    };

    let listener = TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Bind { addr, source })?;
    // Ask the socket what it actually bound, rather than trusting `port`
    // verbatim: today they always agree (we never pass port 0), but reading it
    // back here is what keeps the `Host` allowlist correct if that ever
    // changes, and it's exactly what the ephemeral-port test below relies on.
    let bound_port = listener
        .local_addr()
        .map_err(|source| ServeError::Bind { addr, source })?
        .port();

    // The Host allowlist: 127.0.0.1/localhost are always in (they still work on
    // this machine). When bound beyond loopback, add the names a LAN client will
    // address the server by — the caller's `--allow-host` values plus this
    // machine's own hostname (and its `.local` mDNS form), looked up best-effort
    // so `http://<hostname>.local:<port>` just works from a phone.
    let allowed_hosts = if bind.is_loopback() {
        // A loopback bind keeps the invariant allowlist (127.0.0.1/localhost are
        // built into `host_is_allowed`). `--allow-host` is deliberately ignored
        // here: widening a loopback server's allowlist would re-open the very
        // DNS-rebinding hole the Host check exists to close.
        Vec::new()
    } else {
        let mut hosts: Vec<String> = allow_hosts
            .into_iter()
            .map(|host| host.trim().to_string())
            .filter(|host| !host.is_empty())
            .collect();
        // The literal bound IP, when a specific interface was named — an
        // IP-literal Host can't be produced by DNS rebinding, so it's safe, and
        // it's the obvious thing to type. `0.0.0.0`/`::` name no single address.
        if !bind.is_unspecified() {
            hosts.push(bind.to_string());
        }
        // This machine's own name, so `http://<hostname>.local:<port>` just works.
        if let Some(name) = system_hostname() {
            hosts.push(name.clone());
            if !name.contains('.') {
                hosts.push(format!("{name}.local"));
            }
        }
        hosts
    };

    let state = AppState {
        source,
        dataset_write: Arc::new(tokio::sync::Mutex::new(())),
        build_write: Arc::new(tokio::sync::Mutex::new(())),
        bound_port,
        allowed_hosts: Arc::new(allowed_hosts.clone()),
    };

    // The startup banner: plain stderr lines, no spinner/animation (the
    // scriptability rule), and the `style` helpers make it NO_COLOR-safe.
    let url = format!("http://{addr}");
    eprintln!("{}", style::info(format!("aarg serve listening on {url}")));
    if bind.is_loopback() {
        eprintln!(
            "{}",
            style::dim("bound to 127.0.0.1 only — localhost is the security default")
        );
    } else {
        // Binding beyond loopback exposes the dataset and the key-spending LLM
        // proxy to everything that can reach this address. Say so, loudly, and
        // print exactly which host names the Host allowlist will answer to.
        eprintln!(
            "{}",
            style::warn(format!(
                "bound to {bind} — the dataset, the LLM proxy (which spends your key), and build deletion are reachable by anything on this network"
            ))
        );
        for host in allowed_hosts.iter() {
            eprintln!(
                "{}",
                style::bullet(format!("reachable at http://{host}:{port}"))
            );
        }
    }
    match &state.source {
        StaticSource::Dir(root) => eprintln!(
            "{}",
            style::bullet(format!("serving {} at {url}/", root.display()))
        ),
        StaticSource::Embedded => eprintln!(
            "{}",
            style::bullet(format!(
                "serving the built-in browser workspace at {url}/ (pass --dir <path> to serve a different build)"
            ))
        ),
        StaticSource::None => eprintln!(
            "{}",
            style::bullet(format!(
                "API only at {url}/api (this build has no embedded browser app; pass --dir <path> to serve one)"
            ))
        ),
    }
    eprintln!("{}", style::dim("press Ctrl-C to stop"));

    serve_listener(listener, state).await;
    Ok(())
}

/// The accept loop: one tokio task per connection, each running an HTTP/1
/// connection whose service is [`handle`]. Runs until the task is dropped
/// (process exit). An accept error is logged and skipped rather than fatal —
/// a transient failure (e.g. fd exhaustion) shouldn't kill a long-lived
/// server. Split from [`run`] so a test can drive it on an ephemeral port.
async fn serve_listener(listener: TcpListener, state: AppState) {
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                log(&format!("accept failed: {error}"));
                // A transient blip is fine to retry immediately, but a
                // *persistent* accept() failure — most commonly fd exhaustion
                // (EMFILE/ENFILE) from a burst of connections — would otherwise
                // hot-spin this loop at full CPU, forever failing the same way.
                // A short pause gives the system a chance to free a descriptor
                // (or the operator a chance to notice) before the next attempt.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let io = TokioIo::new(stream);
        let state = state.clone();
        tokio::task::spawn(async move {
            let service = service_fn(move |req| {
                let state = state.clone();
                // Every request resolves to a response; a handler never errors
                // out of the service (it maps its own failures to HTTP), so the
                // service's error type is `Infallible`.
                async move { Ok::<Resp, std::convert::Infallible>(handle(req, state).await) }
            });
            if let Err(error) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                // A dropped/reset connection is routine; log at debug volume.
                log(&format!("connection error: {error}"));
            }
        });
    }
}

/// Route one request to its handler and always produce a response. Reads only
/// the method, path, and headers here; a handler that needs the request body
/// consumes it itself.
///
/// The same-origin checks run before any handler sees the request, so a
/// rejected request never reaches library code (a stolen API key or a spent
/// typst invocation would already be too late): first the `Host` allowlist
/// (kills DNS rebinding), checked before routing at all; then, once routing
/// has identified a route that accepts a JSON body, the `Content-Type` gate
/// (kills drive-by cross-origin POSTs). See the module doc for why each one
/// matters.
async fn handle(req: Request<Incoming>, state: AppState) -> Resp {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    if let Some(resp) = check_host(req.headers(), state.bound_port, &state.allowed_hosts) {
        return resp;
    }

    let route = match_route(&method, &path);
    if let Some(resp) = json_content_type_gate(&route, req.headers()) {
        return resp;
    }

    match route {
        Match::Api(route) => match route {
            ApiRoute::Llm => routes::llm(req).await,
            ApiRoute::Render => routes::render(req).await,
            ApiRoute::GetDataset => routes::get_dataset().await,
            ApiRoute::PutDataset => routes::put_dataset(req, &state).await,
            ApiRoute::ListBuilds => routes::list_builds().await,
            ApiRoute::CreateBuild => routes::create_build(req).await,
            ApiRoute::Models => routes::models().await,
            ApiRoute::Templates => routes::templates().await,
            ApiRoute::GetBuild(id) => routes::get_build(&id).await,
            ApiRoute::DeleteBuild(id) => routes::delete_build(&id, &state).await,
            ApiRoute::SaveBuildEdits(id) => routes::save_build_edits(req, &id, &state).await,
            ApiRoute::SaveBuildTriage(id) => routes::save_build_triage(req, &id, &state).await,
            ApiRoute::GenerateBuildCover(id) => {
                routes::generate_build_cover(req, &id, &state).await
            }
            ApiRoute::GetBuildFile(id, name) => routes::get_build_file(&id, &name).await,
            ApiRoute::FetchJd => routes::fetch_jd(req).await,
            ApiRoute::Cost => routes::cost(req).await,
        },
        Match::Static => statics::serve(&state, &path),
        Match::NotFound => {
            error_response(404, "not_found", format!("no route for {method} {path}"))
        }
    }
}

/// Reject any request whose `Host` header doesn't name this server. A loopback
/// *bind* only restricts who can open the socket; it says nothing about which
/// browser tab a same-socket request came from. DNS rebinding exploits exactly
/// that gap: `attacker.com` can be pointed at a DNS record for `127.0.0.1`, so
/// a page served from `attacker.com` becomes, from the browser's perspective,
/// same-origin with itself — but the request it sends still carries
/// `Host: attacker.com`, because that's what the page's own origin is. Checking
/// `Host` here is what tells the two apart. A missing header is rejected too:
/// every real HTTP/1.1 client sends one.
///
/// Returns the rejection response directly (`Option<Resp>`, not
/// `Result<(), Resp>`) — there's no success *value* here, only "let it
/// through" or "answer with this instead", and `Option` says that without
/// clippy's `result_large_err` objecting to carrying a whole `Resp` in an
/// error slot it isn't one.
fn check_host(headers: &hyper::HeaderMap, bound_port: u16, allowed: &[String]) -> Option<Resp> {
    let host = headers
        .get(hyper::header::HOST)
        .and_then(|value| value.to_str().ok());
    match host {
        Some(host) if host_is_allowed(host, bound_port, allowed) => None,
        _ => Some(error_response(
            403,
            "forbidden_host",
            "this server does not answer requests with that Host header",
        )),
    }
}

/// Whether a `Host` header value names this server: the bare loopback names,
/// any extra `allowed` name (from `--bind`/`--allow-host` and the machine's own
/// hostname when bound past loopback), each accepted bare or with the *actual
/// bound* port appended (what a browser sends whenever the port isn't the
/// scheme default). Comparing against the bound port rather than the requested
/// one keeps this correct even when the OS chose the port, as the ephemeral-port
/// test does. `allowed` is empty on a loopback bind, so the default posture is
/// exactly the loopback-only allowlist.
fn host_is_allowed(host: &str, bound_port: u16, allowed: &[String]) -> bool {
    let names = ["127.0.0.1", "localhost"]
        .iter()
        .map(|s| s.to_string())
        .chain(allowed.iter().cloned());
    names.into_iter().any(|name| {
        host.eq_ignore_ascii_case(&name)
            || host.eq_ignore_ascii_case(&format!("{name}:{bound_port}"))
    })
}

/// This machine's hostname, best-effort, for the `Host` allowlist when bound to
/// the network. Shelled out (like the typst resolution) rather than pulling a
/// crate for one string; a failure just means the caller must name the host via
/// `--allow-host` instead.
fn system_hostname() -> Option<String> {
    let output = std::process::Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Which API routes accept a JSON request body — the ones the content-type
/// gate below protects. The other routes (`GET`s, and the build-file fetch)
/// have no body to gate.
fn requires_json_body(route: &ApiRoute) -> bool {
    matches!(
        route,
        ApiRoute::Llm
            | ApiRoute::Render
            | ApiRoute::PutDataset
            | ApiRoute::CreateBuild
            | ApiRoute::SaveBuildEdits(_)
            | ApiRoute::SaveBuildTriage(_)
            | ApiRoute::GenerateBuildCover(_)
            | ApiRoute::FetchJd
    )
}

/// Runs [`check_json_content_type`] only for a matched route that
/// [`requires_json_body`], and lets everything else (a static path, a
/// `GET`-only API route, an already-unmatched path) through untouched. Kept
/// as its own function, rather than an `if`-inside-an-`if` at the call site,
/// so there's exactly one place in [`handle`] that decides whether to bail —
/// `if let Some(resp) = json_content_type_gate(...) { return resp; }`.
fn json_content_type_gate(route: &Match, headers: &hyper::HeaderMap) -> Option<Resp> {
    match route {
        Match::Api(api_route) if requires_json_body(api_route) => check_json_content_type(headers),
        _ => None,
    }
}

/// Reject a JSON-body route whose `Content-Type` isn't `application/json`
/// (a `;charset=...` suffix is fine). This is what forces a cross-origin
/// browser request onto the preflighted path: `text/plain`,
/// `application/x-www-form-urlencoded`, and `multipart/form-data` are the
/// three "simple" content types a browser will send *without* first asking
/// permission via CORS preflight — so a handler that parsed JSON out of one of
/// those would be reachable by any page the victim has open, no matter what
/// origin it came from. Requiring `application/json` takes that request out of
/// the simple-request category; since this server never sends the
/// `Access-Control-Allow-*` headers a preflight checks for, the preflight
/// fails closed and the real request never leaves the browser.
///
/// Like [`check_host`], returns `Option<Resp>` rather than `Result<(), Resp>`
/// for the same reason: "let it through" or "answer with this instead", not a
/// success value worth a `Result`.
fn check_json_content_type(headers: &hyper::HeaderMap) -> Option<Resp> {
    let content_type = headers
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    match content_type {
        Some(value) if is_json_content_type(value) => None,
        _ => Some(error_response(
            415,
            "unsupported_media_type",
            "this endpoint requires Content-Type: application/json",
        )),
    }
}

/// Whether a `Content-Type` header value is JSON, ignoring a trailing
/// `;charset=...` (or any other) parameter — only the part before the first
/// `;` is the media type.
fn is_json_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

/// One matched API endpoint. The path parameters (a build id, a filename) are
/// carried as owned strings so the pure [`match_route`] stays free of the
/// request's lifetime.
#[derive(Debug, PartialEq, Eq)]
enum ApiRoute {
    Llm,
    Render,
    GetDataset,
    PutDataset,
    ListBuilds,
    CreateBuild,
    Models,
    Templates,
    GetBuild(String),
    DeleteBuild(String),
    SaveBuildEdits(String),
    SaveBuildTriage(String),
    GenerateBuildCover(String),
    GetBuildFile(String, String),
    FetchJd,
    Cost,
}

/// What a `(method, path)` resolved to.
#[derive(Debug, PartialEq, Eq)]
enum Match {
    /// A JSON API endpoint.
    Api(ApiRoute),
    /// A non-`/api` GET: try to serve it as a static file.
    Static,
    /// Nothing matched — an unknown path, or a method the route doesn't accept.
    NotFound,
}

/// Resolve a request line to a route. Pure (no IO, no request body), so the
/// whole routing table is unit-testable without binding a socket.
fn match_route(method: &Method, path: &str) -> Match {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    // `/api/...` is the JSON surface. Matching on the method string keeps the
    // table readable and sidesteps `Method`'s non-`Copy` pattern ergonomics.
    if segments.first() == Some(&"api") {
        let rest = &segments[1..];
        let route = match (method.as_str(), rest) {
            ("POST", ["llm"]) => Some(ApiRoute::Llm),
            ("POST", ["render"]) => Some(ApiRoute::Render),
            ("GET", ["dataset"]) => Some(ApiRoute::GetDataset),
            ("PUT", ["dataset"]) => Some(ApiRoute::PutDataset),
            ("GET", ["builds"]) => Some(ApiRoute::ListBuilds),
            ("POST", ["builds"]) => Some(ApiRoute::CreateBuild),
            ("GET", ["models"]) => Some(ApiRoute::Models),
            ("GET", ["templates"]) => Some(ApiRoute::Templates),
            ("GET", ["builds", id]) => Some(ApiRoute::GetBuild((*id).to_string())),
            ("DELETE", ["builds", id]) => Some(ApiRoute::DeleteBuild((*id).to_string())),
            ("POST", ["builds", id, "edits"]) => Some(ApiRoute::SaveBuildEdits((*id).to_string())),
            ("POST", ["builds", id, "triage"]) => {
                Some(ApiRoute::SaveBuildTriage((*id).to_string()))
            }
            ("POST", ["builds", id, "cover"]) => {
                Some(ApiRoute::GenerateBuildCover((*id).to_string()))
            }
            ("GET", ["builds", id, "files", name]) => Some(ApiRoute::GetBuildFile(
                (*id).to_string(),
                (*name).to_string(),
            )),
            ("POST", ["fetch-jd"]) => Some(ApiRoute::FetchJd),
            ("GET", ["cost"]) => Some(ApiRoute::Cost),
            _ => None,
        };
        return route.map(Match::Api).unwrap_or(Match::NotFound);
    }

    // Everything else is a static-file GET (the browser app). `HEAD` is routed
    // the same as `GET` — [`statics::serve`] builds the same full response
    // either way, and hyper's connection layer (not this code) omits the body
    // bytes on the wire for a `HEAD` response while still writing the correct
    // headers, so `curl -I` reports real sizes instead of a bare 404. Any
    // other method on a non-API path has nothing to serve.
    if method == Method::GET || method == Method::HEAD {
        Match::Static
    } else {
        Match::NotFound
    }
}

// ---------------------------------------------------------------------
// Shared response builders — one JSON envelope, one error envelope, used by
// every route so a browser client sees a consistent shape.
// ---------------------------------------------------------------------

/// A `200`-family JSON response serialized from any `Serialize` value. A
/// serialization failure (practically impossible for our own types) becomes a
/// plain 500 rather than a panic — production code may not `unwrap`.
fn json_response<T: serde::Serialize>(status: u16, value: &T) -> Resp {
    match serde_json::to_vec(value) {
        Ok(bytes) => bytes_response(status, "application/json", bytes),
        Err(error) => error_response(
            500,
            "internal",
            format!("could not serialize the response: {error}"),
        ),
    }
}

/// A JSON error response with the envelope every handler shares:
/// `{"error": {"kind": ..., "message": ...}}`. `kind` is a stable machine
/// token; `message` is human-readable. Never carries secret material — callers
/// pass an error's `Display`, which is written to be safe to show.
fn error_response(status: u16, kind: &str, message: impl Into<String>) -> Resp {
    let body = serde_json::json!({
        "error": { "kind": kind, "message": message.into() }
    });
    // Build the bytes directly; if even this can't serialize, fall back to a
    // fixed byte string so the function is total.
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| {
        br#"{"error":{"kind":"internal","message":"error serialization failed"}}"#.to_vec()
    });
    bytes_response(status, "application/json", bytes)
}

/// A raw-bytes response with an explicit content type. The single place a
/// `Resp` is constructed, so status/header/body handling lives once. A
/// builder failure (an invalid header, which our fixed inputs never are)
/// degrades to a bare 500 rather than panicking.
fn bytes_response(status: u16, content_type: &str, bytes: Vec<u8>) -> Resp {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .body(Full::new(Bytes::from(bytes)).boxed_unsync())
        .unwrap_or_else(|_| {
            let mut fallback =
                Response::new(Full::new(Bytes::from_static(b"internal error")).boxed_unsync());
            *fallback.status_mut() = hyper::StatusCode::INTERNAL_SERVER_ERROR;
            fallback
        })
}

/// A `200 text/event-stream` response wrapping a streaming body — the SSE mode
/// of `POST /api/llm`. `cache-control: no-cache` keeps an intermediary from
/// buffering or replaying the token stream. Built here, next to
/// [`bytes_response`], so the one place a streaming `Resp` is constructed sits
/// beside the one place a buffered one is. A builder failure (our fixed headers
/// never are invalid) degrades to a bare 500 rather than panicking.
pub(super) fn event_stream_response(body: Body) -> Resp {
    Response::builder()
        .status(200)
        .header(hyper::header::CONTENT_TYPE, "text/event-stream")
        .header(hyper::header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap_or_else(|_| {
            error_response(500, "internal", "could not build the streaming response")
        })
}

/// The largest request body any handler will read. A dataset (even with a
/// long history) is well under 1 MB, and an LLM request carrying a few
/// attachments is at most a few MB, so 8 MB is generous headroom without
/// leaving the door open to a client that just keeps sending bytes and
/// parking a task's worth of memory (or a blocking-task worth, since some of
/// what reads this body — `store::save`, `typst` staging — runs synchronously;
/// see the note below).
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Read a request body into bytes, capped at [`MAX_BODY_BYTES`] and mapping a
/// transport failure or an over-limit body to an error response the caller
/// returns as-is. Returns `Err(Resp)` so a handler reads
/// `let body = match read_body(req).await { Ok(b) => b, Err(resp) => return resp };`.
///
/// Note for readers of this file, `routes.rs`, and `statics.rs`: several
/// handlers do their real work with *synchronous* IO on this async worker —
/// `std::fs::read`/`write` for the dataset store, config, and static files,
/// and (via `spawn_blocking`, at least) the `typst` subprocess. On localhost,
/// serving one user, that's an acceptable v1 trade — it keeps the handlers
/// simple to read — but it does mean a slow disk or a large file blocks the
/// worker thread it runs on. Revisit alongside the streaming slice mentioned
/// in the module doc.
async fn read_body(req: Request<Incoming>) -> Result<Bytes, Resp> {
    // A fast precheck: if the client declared a `Content-Length` over the
    // limit, reject before reading a single byte rather than waiting for the
    // stream to prove it. This is a courtesy, not the enforcement — a chunked
    // body carries no `Content-Length` at all — the `Limited` wrapper below is
    // what actually enforces the cap regardless of how the body is framed.
    match content_length(req.headers()) {
        Some(declared) if declared > MAX_BODY_BYTES => {
            return Err(body_too_large_response(declared));
        }
        _ => {}
    }

    let limited = Limited::new(req.into_body(), MAX_BODY_BYTES);
    match limited.collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(error) => {
            // `Limited`'s error type is a boxed `dyn Error` covering both "the
            // real transport failed" and "the limit was hit"; downcasting
            // tells the two apart so the response says which one happened.
            if error.downcast_ref::<LengthLimitError>().is_some() {
                Err(body_too_large_response(MAX_BODY_BYTES))
            } else {
                Err(error_response(
                    400,
                    "bad_request",
                    format!("could not read the request body: {error}"),
                ))
            }
        }
    }
}

/// The `Content-Length` header as a byte count, if present and parseable. A
/// missing or unparseable header just means "no fast precheck" — the
/// `Limited` body still enforces the real cap.
fn content_length(headers: &hyper::HeaderMap) -> Option<usize> {
    headers
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
}

/// A `413` error response for a request body that exceeds [`MAX_BODY_BYTES`].
fn body_too_large_response(size: usize) -> Resp {
    error_response(
        413,
        "payload_too_large",
        format!("request body of {size} bytes exceeds the {MAX_BODY_BYTES}-byte limit"),
    )
}

/// Every server log line goes to stderr — the scriptability rule, and the
/// same discipline as the MCP server (there, stdout is a wire; here, stdout
/// simply carries no machine output at all).
fn log(message: &str) {
    eprintln!("{}", style::dim(format!("serve: {message}")));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn route(method: &str, path: &str) -> Match {
        match_route(&Method::from_bytes(method.as_bytes()).unwrap(), path)
    }

    #[test]
    fn the_api_routes_match_by_method_and_shape() {
        assert_eq!(route("POST", "/api/llm"), Match::Api(ApiRoute::Llm));
        assert_eq!(route("POST", "/api/render"), Match::Api(ApiRoute::Render));
        assert_eq!(
            route("GET", "/api/dataset"),
            Match::Api(ApiRoute::GetDataset)
        );
        assert_eq!(
            route("PUT", "/api/dataset"),
            Match::Api(ApiRoute::PutDataset)
        );
        assert_eq!(
            route("GET", "/api/builds"),
            Match::Api(ApiRoute::ListBuilds)
        );
        assert_eq!(
            route("POST", "/api/builds"),
            Match::Api(ApiRoute::CreateBuild)
        );
        assert_eq!(route("GET", "/api/models"), Match::Api(ApiRoute::Models));
        assert_eq!(
            route("GET", "/api/templates"),
            Match::Api(ApiRoute::Templates)
        );
        assert_eq!(
            route("GET", "/api/builds/041"),
            Match::Api(ApiRoute::GetBuild("041".into()))
        );
        assert_eq!(
            route("DELETE", "/api/builds/041"),
            Match::Api(ApiRoute::DeleteBuild("041".into()))
        );
        assert_eq!(
            route("POST", "/api/builds/041/edits"),
            Match::Api(ApiRoute::SaveBuildEdits("041".into()))
        );
        assert_eq!(
            route("POST", "/api/builds/041/triage"),
            Match::Api(ApiRoute::SaveBuildTriage("041".into()))
        );
        assert_eq!(
            route("POST", "/api/builds/041/cover"),
            Match::Api(ApiRoute::GenerateBuildCover("041".into()))
        );
        assert_eq!(
            route("GET", "/api/builds/041/files/resume.ats.pdf"),
            Match::Api(ApiRoute::GetBuildFile(
                "041".into(),
                "resume.ats.pdf".into()
            ))
        );
        assert_eq!(
            route("POST", "/api/fetch-jd"),
            Match::Api(ApiRoute::FetchJd)
        );
        // The query string isn't part of the path `match_route` sees (hyper
        // splits it off), so a `?model=...` suffix still resolves.
        assert_eq!(route("GET", "/api/cost"), Match::Api(ApiRoute::Cost));
    }

    #[test]
    fn a_wrong_method_on_a_known_api_path_is_not_found() {
        // `GET /api/llm` (llm is POST-only) matches no route.
        assert_eq!(route("GET", "/api/llm"), Match::NotFound);
        // An unknown `/api` path is likewise NotFound, never a static attempt.
        assert_eq!(route("GET", "/api/nope"), Match::NotFound);
        assert_eq!(route("DELETE", "/api/dataset"), Match::NotFound);
    }

    #[test]
    fn non_api_gets_are_static_and_other_methods_are_not_found() {
        assert_eq!(route("GET", "/"), Match::Static);
        assert_eq!(route("GET", "/index.html"), Match::Static);
        assert_eq!(route("GET", "/assets/app.wasm"), Match::Static);
        // A non-GET on a static path has nothing to serve.
        assert_eq!(route("POST", "/index.html"), Match::NotFound);
    }

    #[test]
    fn the_error_envelope_is_stable() {
        let resp = error_response(422, "invalid_dataset", "2 problems");
        assert_eq!(resp.status(), 422);
        assert_eq!(
            resp.headers()
                .get(hyper::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
    }

    /// End-to-end: bind an ephemeral port, drive a few routes with `reqwest`
    /// (a main dependency, so no dev-dep is needed), then drop the server task.
    /// Covers three routes with no keychain, no typst, and no network:
    /// `GET /api/builds`, the `PUT /api/dataset` validation gate, and a static
    /// file — plus a 404 for good measure.
    #[tokio::test]
    async fn the_server_answers_over_a_real_socket() {
        use crate::dataset::types::{
            Contact, Proficiency, ResumeDataset, Skill, SkillCategory, SkillId,
        };

        // A static root with one file to serve.
        let web = tempfile::tempdir().unwrap();
        std::fs::write(web.path().join("index.html"), "<h1>aarg</h1>").unwrap();
        let root = web.path().canonicalize().unwrap();

        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let state = AppState {
            source: StaticSource::Dir(Arc::new(root)),
            dataset_write: Arc::new(tokio::sync::Mutex::new(())),
            build_write: Arc::new(tokio::sync::Mutex::new(())),
            bound_port: addr.port(),
            allowed_hosts: Arc::new(Vec::new()),
        };
        let server = tokio::spawn(serve_listener(listener, state));

        let base = format!("http://{addr}");
        let http = reqwest::Client::new();

        // 1) GET /api/builds — read-only, works against any workspace (even an
        //    empty one); we only assert the shape, not the contents.
        let resp = http.get(format!("{base}/api/builds")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["builds"].is_array());

        // 2) PUT /api/dataset with a dataset that fails validation (a skill with
        //    no evidence) — the never-fabricate gate must reject it with 422
        //    *before* any write, so the real workspace is untouched.
        let mut bad = ResumeDataset::new(Contact {
            full_name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        });
        bad.skills.skills.push(Skill {
            id: SkillId("skill-1".into()),
            canonical_name: "Rust".into(),
            aliases: Vec::new(),
            category: SkillCategory::Language,
            proficiency: Proficiency::Working,
            years: None,
            last_used: None,
            evidence: Vec::new(), // unbacked → a validation problem
            verified: false,
            verified_at: None,
        });
        let resp = http
            .put(format!("{base}/api/dataset"))
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 422);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["kind"], "invalid_dataset");
        assert!(!body["problems"].as_array().unwrap().is_empty());

        // 3) A static file from --dir, with the right content type.
        let resp = http.get(format!("{base}/index.html")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        assert!(
            resp.headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap()
                .starts_with("text/html")
        );
        assert_eq!(resp.text().await.unwrap(), "<h1>aarg</h1>");

        // 4) An unknown path is a JSON 404.
        let resp = http.get(format!("{base}/api/nope")).send().await.unwrap();
        assert_eq!(resp.status(), 404);

        server.abort();
    }

    #[test]
    fn host_is_allowed_covers_the_documented_forms() {
        // Loopback bind: no extra allowed hosts, only the built-in loopback names.
        let none: &[String] = &[];
        // The bare loopback names, with no port, are accepted (some non-browser
        // clients send exactly these).
        assert!(host_is_allowed("127.0.0.1", 8787, none));
        assert!(host_is_allowed("localhost", 8787, none));
        // What a browser actually sends: the name plus the *bound* port.
        assert!(host_is_allowed("127.0.0.1:8787", 8787, none));
        assert!(host_is_allowed("localhost:8787", 8787, none));
        // Anything naming another origin — including the right name at the
        // *wrong* port, which is a different origin as far as a browser is
        // concerned — is refused.
        assert!(!host_is_allowed("attacker.com", 8787, none));
        assert!(!host_is_allowed("127.0.0.1:9999", 8787, none));
        assert!(!host_is_allowed("127.0.0.1.attacker.com", 8787, none));

        // LAN bind: an explicitly-allowed host is accepted bare and with the
        // bound port (case-insensitively), but an unlisted one is still refused.
        let lan = vec!["MortM5.local".to_string()];
        assert!(host_is_allowed("mortm5.local", 8787, &lan));
        assert!(host_is_allowed("MortM5.local:8787", 8787, &lan));
        assert!(!host_is_allowed("mortm5.local:9999", 8787, &lan));
        assert!(!host_is_allowed("someone-else.local", 8787, &lan));
    }

    #[test]
    fn is_json_content_type_ignores_a_charset_suffix_but_nothing_else() {
        assert!(is_json_content_type("application/json"));
        assert!(is_json_content_type("application/json; charset=utf-8"));
        assert!(!is_json_content_type("text/plain"));
        assert!(!is_json_content_type("application/x-www-form-urlencoded"));
        assert!(!is_json_content_type(""));
    }

    /// Spawn a real server on an ephemeral port, API-only (no `--dir`), for
    /// the raw-socket tests below — they need to control headers (`Host`,
    /// `Content-Type`, a lying `Content-Length`) that a high-level HTTP client
    /// either sets itself or won't let a caller mismatch from the connection's
    /// real target.
    async fn spawn_test_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let state = AppState {
            source: StaticSource::None,
            dataset_write: Arc::new(tokio::sync::Mutex::new(())),
            build_write: Arc::new(tokio::sync::Mutex::new(())),
            bound_port: addr.port(),
            allowed_hosts: Arc::new(Vec::new()),
        };
        (addr, tokio::spawn(serve_listener(listener, state)))
    }

    /// Send a raw, hand-written HTTP/1.1 request over a fresh connection and
    /// return the response's status code. `request` must be the complete
    /// request (request line, headers, blank line, and any body) with `\r\n`
    /// line endings.
    async fn raw_status(addr: SocketAddr, request: &str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        // Every request below sends `Connection: close`, so the server closes
        // its write side once the response is flushed and `read_to_end`
        // returns — no need (and, per hyper's http1 io loop, actively
        // harmful) to half-close our own write side first, which can race the
        // server's in-flight response write and abort the connection instead.
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let text = String::from_utf8_lossy(&response);
        let status_line = text.lines().next().unwrap();
        // A status line looks like "HTTP/1.1 403 Forbidden".
        status_line
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    }

    #[tokio::test]
    async fn an_unrecognized_host_header_is_rejected_before_routing() {
        let (addr, server) = spawn_test_server().await;

        // A request that would otherwise be a perfectly good `GET
        // /api/builds` is refused with 403 because `Host` names some other
        // origin — this is what actually stops DNS rebinding (a page served
        // from `attacker.com`, resolved to 127.0.0.1, still sends
        // `Host: attacker.com`).
        let status = raw_status(
            addr,
            "GET /api/builds HTTP/1.1\r\nHost: attacker.com\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(status, 403);

        // A request with no `Host` header at all is refused the same way.
        let status = raw_status(
            addr,
            "GET /api/builds HTTP/1.1\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(status, 403);

        server.abort();
    }

    #[tokio::test]
    async fn a_non_json_content_type_on_a_json_route_is_rejected() {
        let (addr, server) = spawn_test_server().await;

        // `Host` is correct, but a cross-origin `fetch`/`<form>` using a
        // "simple" content type like `text/plain` never triggers a CORS
        // preflight — so if this handler parsed JSON out of it, any web page
        // could drive `/api/llm` blind. The gate rejects it with 415 before
        // the handler (and its keychain read) ever runs.
        let request = format!(
            "POST /api/llm HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}",
            addr.port()
        );
        let status = raw_status(addr, &request).await;
        assert_eq!(status, 415);

        server.abort();
    }

    #[tokio::test]
    async fn an_oversized_declared_body_is_rejected_before_reading() {
        let (addr, server) = spawn_test_server().await;

        // `Host` and `Content-Type` are both fine, but the declared
        // `Content-Length` is over the cap. `read_body`'s precheck rejects it
        // from the header alone — note there's no actual 9 MB following in
        // this request, which is the point: the fast path never tries to read
        // it.
        let request = format!(
            "POST /api/fetch-jd HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: 9000000\r\nConnection: close\r\n\r\n",
            addr.port()
        );
        let status = raw_status(addr, &request).await;
        assert_eq!(status, 413);

        server.abort();
    }

    #[tokio::test]
    async fn the_happy_path_host_forms_are_all_accepted() {
        let (addr, server) = spawn_test_server().await;

        for host in [
            "127.0.0.1".to_string(),
            format!("127.0.0.1:{}", addr.port()),
            "localhost".to_string(),
            format!("localhost:{}", addr.port()),
        ] {
            let request =
                format!("GET /api/builds HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
            let status = raw_status(addr, &request).await;
            assert_eq!(status, 200, "expected Host: {host} to be accepted");
        }

        server.abort();
    }
}
