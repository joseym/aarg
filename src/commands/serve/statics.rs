//! Static-file serving: the browser app's own bytes (`index.html`, the JS
//! glue, the `.wasm` module, CSS) handed out at `/` when `aarg serve --dir`
//! was given.
//!
//! Two things matter here and both are load-bearing:
//!
//! - **No directory traversal.** A request path is joined onto the static
//!   root, then *canonicalized* (symlinks and `..` resolved by the OS) and
//!   checked to still live under the root. A path like `/../../etc/passwd`
//!   canonicalizes to somewhere outside the root and is refused. This is the
//!   same defense the MCP resources route uses, applied to a filesystem tree.
//! - **The right `Content-Type`.** `application/wasm` in particular is not
//!   cosmetic: browsers only take the streaming-compilation fast path for a
//!   `.wasm` served with that exact type, so getting it wrong quietly slows
//!   the app's startup.

use std::path::{Path, PathBuf};

use super::{AppState, Resp, bytes_response, error_response};

/// Serve a static file for a non-`/api` GET. `path` is the request path
/// (e.g. `/`, `/index.html`, `/assets/app.wasm`). A `404` when no `--dir` was
/// given, when the file doesn't exist, or when the path tried to escape the
/// root.
pub(super) fn serve(state: &AppState, path: &str) -> Resp {
    let Some(root) = state.static_root.as_deref() else {
        return error_response(
            404,
            "not_found",
            "static serving is off; start the server with `--dir <path>` to serve a browser app",
        );
    };

    let file = match resolve(root, path) {
        Some(file) => file,
        // SPA fallback: a request for a client-side route (no file extension,
        // e.g. `/build/051/tailor`) that matches no real file serves
        // `index.html`, so the browser app's router takes over on a deep link
        // or a refresh. A request that *looks* like an asset (has an extension)
        // still 404s, so a broken script/style/wasm reference stays a real
        // error rather than silently returning HTML.
        None if is_spa_route(path) => match resolve(root, "index.html") {
            Some(index) => index,
            None => return error_response(404, "not_found", "no index.html to serve"),
        },
        None => return error_response(404, "not_found", format!("no static file for {path}")),
    };

    match std::fs::read(&file) {
        Ok(bytes) => bytes_response(200, content_type_for(&file), bytes),
        // Resolve already confirmed it's a file under the root, so a read error
        // here is a genuine IO fault, not a missing/forbidden path.
        Err(error) => error_response(500, "internal", format!("could not read the file: {error}")),
    }
}

/// Map a request path to the file on disk to serve, or `None` when there is
/// nothing safe to serve. `root` must already be canonical (done once at
/// startup). The rules:
///
/// - `/` (the root path *only*) falls back to `index.html`. The wider
///   client-route fallback (a missing extension-less path also serving
///   `index.html`) lives in [`serve`], not here — `resolve` stays a strict
///   file-or-nothing lookup.
/// - The joined path is canonicalized and must still start with `root`, so no
///   `..` or symlink can reach outside the served directory.
/// - It must resolve to a regular file (not a directory).
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let relative = path.trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };

    // Canonicalize the *candidate*: this resolves any `..`/symlinks against the
    // real filesystem, so the prefix check below sees where the path truly
    // lands, not the literal text. A non-existent path fails here (→ None → 404).
    let candidate = root.join(relative).canonicalize().ok()?;
    if !candidate.starts_with(root) {
        return None; // escaped the served directory
    }
    candidate.is_file().then_some(candidate)
}

/// Whether a path is a client-side route (which should fall back to
/// `index.html`) rather than an asset request. The test is simple and
/// sufficient: an asset has a file extension in its last segment
/// (`/main-ABC.js`, `/aarg_wasm_bg.wasm`), a route does not (`/build/051/tailor`,
/// `/`). A missing asset stays a 404; a missing route serves the app.
fn is_spa_route(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or("");
    !last.contains('.')
}

/// The `Content-Type` for a file, by extension. Covers the web-app kinds a
/// browser cares about — `.wasm` especially, which needs `application/wasm`
/// for streaming compilation — and falls back to a generic binary type for
/// anything else. Text kinds carry `; charset=utf-8` so a browser decodes
/// them correctly.
pub(super) fn content_type_for(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("wasm") => "application/wasm",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn spa_routes_fall_back_but_missing_assets_do_not() {
        // Client routes (no extension in the last segment) fall back to the app.
        assert!(is_spa_route("/build/051/tailor"));
        assert!(is_spa_route("/"));
        assert!(is_spa_route("/build/051/")); // trailing slash → route
        // Asset requests (extension present) must 404 when missing, not serve HTML.
        assert!(!is_spa_route("/main-ABC123.js"));
        assert!(!is_spa_route("/aarg_wasm_bg.wasm"));
        assert!(!is_spa_route("/styles-XQ.css"));
    }

    #[test]
    fn content_types_cover_the_web_app_kinds() {
        assert_eq!(
            content_type_for(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type_for(Path::new("app.js")),
            "text/javascript; charset=utf-8"
        );
        // `.wasm` must be exactly application/wasm for streaming compilation.
        assert_eq!(
            content_type_for(Path::new("aarg_bg.wasm")),
            "application/wasm"
        );
        assert_eq!(
            content_type_for(Path::new("style.css")),
            "text/css; charset=utf-8"
        );
        // An unknown or extensionless file is a generic binary blob.
        assert_eq!(
            content_type_for(Path::new("LICENSE")),
            "application/octet-stream"
        );
        assert_eq!(
            content_type_for(Path::new("data.bin")),
            "application/octet-stream"
        );
    }

    #[test]
    fn resolve_serves_a_real_file_and_defaults_root_to_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("index.html"), "hi").unwrap();
        std::fs::create_dir(root.join("assets")).unwrap();
        std::fs::write(root.join("assets").join("app.wasm"), b"\0asm").unwrap();

        // The root path resolves to index.html.
        assert_eq!(resolve(&root, "/"), Some(root.join("index.html")));
        // A named file under a subdir resolves normally.
        assert_eq!(
            resolve(&root, "/assets/app.wasm"),
            Some(root.join("assets").join("app.wasm"))
        );
        // A missing file is None (→ 404), not an error.
        assert_eq!(resolve(&root, "/missing.js"), None);
    }

    #[test]
    fn resolve_refuses_directory_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("web");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("index.html"), "hi").unwrap();
        // A sibling secret outside the served root.
        std::fs::write(dir.path().join("secret.txt"), "nope").unwrap();

        // `..` that would climb out of the root canonicalizes outside it and is
        // refused, whether spelled as a traversal or an absolute-looking path.
        assert_eq!(resolve(&root, "/../secret.txt"), None);
        assert_eq!(resolve(&root, "/assets/../../secret.txt"), None);
        // A directory is not a servable file.
        assert_eq!(resolve(&root, "/"), Some(root.join("index.html")));
    }
}
