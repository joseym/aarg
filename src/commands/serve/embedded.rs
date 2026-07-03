//! The browser app baked into the binary at build time.
//!
//! `build.rs` walks `web/dist/aarg/browser` and writes
//! `$OUT_DIR/embedded_assets.rs`, a single generated `static`:
//!
//! ```ignore
//! pub static EMBEDDED_ASSETS: &[(&str, &[u8])] =
//!     &[("index.html", include_bytes!("/abs/.../index.html")), …];
//! ```
//!
//! The `include!` below pastes that file in, so `EMBEDDED_ASSETS` is a real
//! symbol here. On a fresh clone that has not built the web app, the generated
//! table is empty (`&[]`) and [`available`] reports `false`, so `aarg serve`
//! falls back to API-only.
//!
//! The lookup logic is written against a table passed in ([`lookup_in`]) rather
//! than reaching for `EMBEDDED_ASSETS` directly. That is what lets the unit
//! tests hand it a small hand-built table and assert real behaviour whether or
//! not a dist happened to be embedded in the test build.

include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));

use super::statics;

/// Find the asset to serve for a request path in a given table, applying the
/// same rules as the on-disk static server: a leading `/` is trimmed, an empty
/// path means `index.html`, an exact filename match wins, and an
/// extension-less path that matches nothing falls back to `index.html` so the
/// single-page app's client-side router can take over on a deep link.
///
/// The table is injected so this stays a pure function the tests can drive with
/// a fixture. The lifetime `'a` ties the returned references to the table; the
/// real `EMBEDDED_ASSETS` is `'static`, which satisfies any `'a`.
fn lookup_in<'a>(
    table: &'a [(&'static str, &'static [u8])],
    path: &str,
) -> Option<(&'a str, &'a [u8])> {
    let trimmed = path.trim_start_matches('/');
    let name = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };

    if let Some(entry) = table.iter().find(|(candidate, _)| *candidate == name) {
        return Some((entry.0, entry.1));
    }

    if statics::is_spa_route(path) {
        return table
            .iter()
            .find(|(candidate, _)| *candidate == "index.html")
            .map(|entry| (entry.0, entry.1));
    }

    None
}

/// Look up an embedded asset for a request path. `None` when nothing matches
/// (a missing real asset, or any path when no app was embedded).
pub(super) fn lookup(path: &str) -> Option<(&'static str, &'static [u8])> {
    lookup_in(EMBEDDED_ASSETS, path)
}

/// Whether this build has a browser app embedded. `false` on a clone built
/// before the web dist existed.
pub(super) fn available() -> bool {
    !EMBEDDED_ASSETS.is_empty()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// A hand-built table so these tests pass whether or not a real dist was
    /// embedded in this build. The bytes stand in for a real index page, a
    /// content-hashed script, and the wasm module.
    fn table() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("index.html", b"<!doctype html>".as_slice()),
            ("main-ABC123.js", b"console.log(1)".as_slice()),
            ("aarg_wasm_bg.wasm", b"\0asm".as_slice()),
        ]
    }

    #[test]
    fn the_root_path_serves_index_html() {
        let table = table();
        assert_eq!(
            lookup_in(&table, "").map(|(name, _)| name),
            Some("index.html")
        );
        assert_eq!(
            lookup_in(&table, "/").map(|(name, _)| name),
            Some("index.html")
        );
    }

    #[test]
    fn an_exact_hashed_asset_name_hits() {
        let table = table();
        let hit = lookup_in(&table, "/main-ABC123.js").unwrap();
        assert_eq!(hit.0, "main-ABC123.js");
        assert_eq!(hit.1, b"console.log(1)");
    }

    #[test]
    fn a_client_route_falls_back_to_index_html() {
        let table = table();
        assert_eq!(
            lookup_in(&table, "/build/051/tailor").map(|(name, _)| name),
            Some("index.html")
        );
    }

    #[test]
    fn a_missing_asset_is_none() {
        let table = table();
        // An extension-ful path that matches nothing stays a real miss, so a
        // broken script reference is a 404 rather than silently served HTML.
        assert_eq!(lookup_in(&table, "/missing.js"), None);
    }

    #[test]
    fn an_empty_table_serves_nothing() {
        let empty: Vec<(&'static str, &'static [u8])> = Vec::new();
        assert_eq!(lookup_in(&empty, "/"), None);
        assert_eq!(lookup_in(&empty, ""), None);
        assert_eq!(lookup_in(&empty, "/index.html"), None);
        assert_eq!(lookup_in(&empty, "/build/051/tailor"), None);
    }
}
