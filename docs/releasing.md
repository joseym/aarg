# Cutting a release

The operator checklist for shipping a new version of AARG. Follow it top to
bottom. Three crates publish to crates.io (`aarg-core`, `aarg-domain`, `aarg`),
and a GitHub release carries the prebuilt binaries and the installer script.

One thing to keep in mind before you touch CI config: any edit to
`.github/build-setup.yml` or `dist-workspace.toml` means rerunning
`dist generate`. CI checks that `.github/workflows/release.yml` is the byte-exact
output of the generator, so a hand-edit to the config without regenerating fails
the job.

## 1. Bump versions

Set the new version in all four manifests: the root `Cargo.toml` and the three
crates under `crates/` (`aarg-core`, `aarg-domain`, `aarg-wasm`). Update the
path-dependency version requirements too, since they must match the versions
being published:

- root `Cargo.toml`: the `version =` on the `aarg-core` and `aarg-domain`
  path deps.
- `crates/aarg-domain/Cargo.toml`: the `version =` on the `aarg-core` path dep.

Then refresh the lockfile:

```sh
cargo build
```

## 2. Build the frontend fresh

The binary embeds `web/dist/aarg/browser` at compile time, so build it before
packaging:

```sh
wasm-pack build crates/aarg-wasm --target web --out-dir ../../web/src/wasm/pkg --out-name aarg_wasm
cd web && npm ci && npm run build
cd ..
```

## 3. Run the checks

```sh
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

## 4. Package sanity

Confirm the published package will carry the embedded workspace and nothing it
should not:

```sh
cargo package -p aarg --list --allow-dirty | grep 'web/dist/aarg/browser/index.html'
cargo package -p aarg --list --allow-dirty | grep 'aarg_wasm_bg.wasm'
cargo package -p aarg --list --allow-dirty | grep -c '\.teach\.md'   # expect 0
cargo package -p aarg --list --allow-dirty | grep -c 'src/bin/'      # expect 0
```

The `include` list in the root `Cargo.toml` reads straight from disk and
overrides `.gitignore`, so `web/dist/aarg/browser/**` lands in the package even
though the built frontend is gitignored. The same reading-from-disk behavior is
why the grep checks matter: the teaching files (`*.teach.md`) and the `evals`
dev binary under `src/bin/` are excluded, and this confirms they stayed out.

## 5. Publish in dependency order

Publish bottom-up, and wait for the crates.io index to propagate each crate
before publishing the one that depends on it:

```sh
cargo publish -p aarg-core
# wait for the index to pick up aarg-core at the new version
cargo publish -p aarg-domain
# wait for the index to pick up aarg-domain at the new version
cargo publish -p aarg
```

`aarg-wasm` is `publish = false`, so it never goes to crates.io.

There is a chicken-and-egg to expect here: `cargo package -p aarg` (and the
verify build that `cargo publish` runs) cannot fully verify until its
dependencies are already on crates.io at the new versions. On a version-bump
release the new `aarg-core` and `aarg-domain` versions do not exist in the index
until you publish them, so a full verify of `aarg` is not possible ahead of
those publishes. That is expected. Publishing in the order above resolves it.

## 6. Tag and push

```sh
git tag v<version>
git push origin v<version>
```

Pushing the tag triggers the release workflow. It builds the three targets
(aarch64 and x86_64 macOS, x86_64 Linux); each runner builds the frontend for
itself, and an assertion in the build fails the job if `web/dist` is missing.
The workflow attaches the platform archives, their checksums, and the installer
script to a GitHub release, and marks a prerelease-suffixed tag as a prerelease.

The tag must match the package version exactly, suffix and all, or dist rejects
it. A `v0.2.0-rc.1` tag requires `0.2.0-rc.1` in the four manifests.

## 7. Post-release smoke test

On a Mac, run the installer script and confirm the workspace loads:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/joseym/aarg/releases/latest/download/aarg-installer.sh | sh
aarg serve   # open the printed URL, confirm the browser workspace loads
```

Then confirm the crates.io path in a scratch home so nothing on your machine
interferes:

```sh
CARGO_HOME=$(mktemp -d) cargo install aarg
```

Check that only the `aarg` binary landed (not `evals`) and that `aarg serve`
carries the embedded app.

## One-time setup

- A crates.io API token, set once with `cargo login`, is needed to publish.
- PR runs of the release workflow are in `pr-run-mode = "upload"` in
  `dist-workspace.toml` right now, which builds and uploads artifacts on pull
  requests for rehearsal. Once the pipeline is proven, flip that to `"plan"` and
  rerun `dist generate` so `release.yml` regenerates to match.
