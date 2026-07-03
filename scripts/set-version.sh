#!/usr/bin/env bash
#
# set-version.sh <version>: stamp a new version across the workspace.
#
# It rewrites two kinds of `version = "..."` fields and nothing else:
#   1. the [package] version in each of the four Cargo.tomls (the leading
#      top-level `version =` line, never a dependency's), and
#   2. the version requirement on each in-workspace path dependency (root's
#      aarg-core and aarg-domain deps, and aarg-domain's aarg-core dep), so a
#      published crate pins the exact versions it was released with.
# Third-party dependency versions (chrono, reqwest, ...) are left untouched.
# Finally it runs `cargo update --workspace` so Cargo.lock matches.
#
# Idempotent: stamping the current version rewrites the same bytes and leaves no
# diff. Called by cocogitto's pre_bump_hooks (see cog.toml).

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: set-version.sh <version>

Set the workspace version (e.g. 0.2.1 or 0.3.0-rc.1) in the four Cargo.tomls
and the in-workspace path-dependency requirements, then refresh Cargo.lock.
EOF
}

case "${1:-}" in
  -h | --help)
    usage
    exit 0
    ;;
esac

if [ "$#" -ne 1 ]; then
  usage >&2
  exit 2
fi

VERSION=$1

# major.minor.patch with an optional -prerelease and/or +build suffix.
if ! printf '%s' "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'; then
  echo "set-version.sh: not a semver version: '$VERSION'" >&2
  exit 2
fi

ROOT=$(cd "$(dirname "$0")/.." && pwd)

MANIFESTS=(
  "$ROOT/Cargo.toml"
  "$ROOT/crates/aarg-core/Cargo.toml"
  "$ROOT/crates/aarg-domain/Cargo.toml"
  "$ROOT/crates/aarg-wasm/Cargo.toml"
)

# Clean up any scratch file if a rewrite fails partway through.
trap 'rm -f "${MANIFESTS[@]/%/.tmp.$$}"' EXIT

for manifest in "${MANIFESTS[@]}"; do
  tmp="$manifest.tmp.$$"
  # First expression: the [package] version. It is the only `version = ` that
  # starts at column zero; every dependency version sits inside an inline table
  # (`name = { ... version = ... }`) or after a `name = ` key, so `^version`
  # never matches one.
  # Second expression: rewrite the version requirement on lines that also carry
  # `path = `, i.e. the workspace path dependencies. Files without such a line
  # (aarg-core) or whose path dep has no version key (aarg-wasm's aarg-domain
  # dep) simply match nothing.
  sed -E \
    -e "s/^version = \"[0-9][^\"]*\"/version = \"$VERSION\"/" \
    -e "/path = /s/version = \"[0-9][^\"]*\"/version = \"$VERSION\"/" \
    "$manifest" >"$tmp"
  mv "$tmp" "$manifest"
done

# Refresh the lockfile's workspace-member entries to the new version.
( cd "$ROOT" && cargo update --workspace )

echo "set workspace version to $VERSION"
