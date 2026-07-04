# Contributing

Thanks for looking at aarg. This page covers getting a working build, running
the tests, and what a pull request needs.

## Building the CLI

You need Rust 1.89 or newer and [Typst](https://github.com/typst/typst) on
your `PATH` (rendering shells out to it, and a handful of tests exercise real
renders; those tests skip themselves when Typst is missing).

```sh
git clone https://github.com/joseym/aarg.git
cd aarg
cargo build
cargo test --workspace
```

Both commands work on a fresh clone with nothing else installed. To run your
build against your own data, `cargo install --path .` puts it at
`~/.cargo/bin/aarg`.

## Building the browser workspace

The web app is an Angular project that runs the same Rust pipeline in the
page through WebAssembly. Working on it needs Node with npm and
[wasm-pack](https://rustwasm.github.io/wasm-pack/):

```sh
wasm-pack build crates/aarg-wasm --target web --out-dir ../../web/src/wasm/pkg --out-name aarg_wasm
cd web && npm install && npm run build && cd ..
aarg serve --dir web/dist/aarg/browser
```

The `--dir` flag serves your build output instead of the app embedded in the
binary, so a rebuild shows up on the next page load without reinstalling
anything. Skip all of this if your change stays in the CLI.

## How the workspace is laid out

- `crates/aarg-core` holds the agent runtime and the LLM clients.
- `crates/aarg-domain` holds the résumé pipeline: parsing, gap analysis,
  tailoring, review, and the deterministic checks. Pure code, no IO.
- `crates/aarg-wasm` wraps the pipeline for the browser.
- The root crate is the `aarg` binary: the CLI, the REPL, the MCP server, and
  `aarg serve`.
- `web/` is the Angular front end.

No résumé claim may reach output without tracing to evidence in the user's
dataset; much of the domain crate exists to enforce that. If your change
touches tailoring, validation, or prompts, expect review attention on that
path.

## Commits and pull requests

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org)
and a hook checks them. Install it once after cloning:

```sh
cog install-hook --all
```

(`cog` is [cocogitto](https://docs.cocogitto.io); `cargo install cocogitto`
works if your package manager lacks it.)

Before opening a PR, run what CI runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Keep each commit to one concern. Release versions are computed from commit
types, so choose `feat:` and `fix:` accurately.

Releases themselves are cut by the maintainer; nothing in a normal PR needs
to touch versions, tags, or the release workflow.
