//! Build script: embed the compiled browser workspace into the `aarg` binary.
//!
//! The Angular app builds to `web/dist/aarg/browser`. When that directory
//! exists at compile time, this script walks it and writes a generated Rust
//! source file (`$OUT_DIR/embedded_assets.rs`) that names every file as a
//! `(relative path, bytes)` pair via `include_bytes!`. `src/commands/serve`
//! includes that file, so `aarg serve` can hand the app out of memory with no
//! `--dir` on disk. When the directory is absent (a fresh clone that has not
//! built the web app yet), the same static is written empty, so the build
//! still succeeds and `aarg serve` simply runs API-only.

use std::fs;
use std::path::{Path, PathBuf};

fn main() -> std::io::Result<()> {
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "CARGO_MANIFEST_DIR not set")
        })?;
    let out_dir = std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "OUT_DIR not set"))?;

    let dist = manifest_dir.join("web/dist/aarg/browser");

    // Re-run when the built app changes. The path is relative to
    // CARGO_MANIFEST_DIR. When the directory does not exist, cargo has nothing
    // to stat and re-runs this script on every build — an acceptable readdir,
    // and it means the first `npm run build` after a clone gets picked up
    // without a manual `cargo clean`.
    println!("cargo:rerun-if-changed=web/dist/aarg/browser");

    // Collect (relative path with forward slashes, absolute path) pairs. The
    // dist is flat today, but Angular can emit subdirectories (e.g. `media/`),
    // so the walk recurses.
    let mut assets: Vec<(String, PathBuf)> = Vec::new();
    if dist.is_dir() {
        collect(&dist, &dist, &mut assets)?;
    }
    // Sort by relative path so the generated table is deterministic regardless
    // of the order the filesystem hands entries back.
    assets.sort_by(|a, b| a.0.cmp(&b.0));

    // Both strings are escaped with `{:?}`, which produces a valid Rust string
    // literal (quotes plus escaping). An empty `assets` yields `&[]`.
    let mut generated = String::new();
    generated.push_str("pub static EMBEDDED_ASSETS: &[(&str, &[u8])] = &[\n");
    for (relative, absolute) in &assets {
        let absolute = absolute.to_string_lossy();
        generated.push_str(&format!(
            "    ({relative:?}, include_bytes!({absolute:?})),\n"
        ));
    }
    generated.push_str("];\n");

    fs::write(out_dir.join("embedded_assets.rs"), generated)
}

/// Recursively collect every regular file under `dir`, recording each as a
/// `(path relative to `root` with forward slashes, absolute path)` pair.
fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect(root, &path, out)?;
        } else {
            let relative = path.strip_prefix(root).map_err(std::io::Error::other)?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            out.push((relative, path));
        }
    }
    Ok(())
}
