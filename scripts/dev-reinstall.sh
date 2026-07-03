#!/usr/bin/env bash
# Reinstall the aarg binary and, on macOS, re-sign it with a stable identity
# so the keychain "Always Allow" grant survives the reinstall.
#
# The problem this solves: `cargo install` ad-hoc-signs the binary, whose
# designated requirement is a raw code hash. The macOS keychain ties an
# app's access grant to that requirement, so every rebuild produces a new
# hash, drops the grant, and re-prompts (or fails in a non-GUI context).
# Signing each build with one self-signed certificate and a fixed identifier
# keeps the designated requirement constant, so the grant is given once and
# then holds.
#
# One-time setup (see scripts/dev-reinstall.sh --help):
#   1. Create a self-signed code-signing certificate named "aarg-dev".
#   2. Run this script; approve the keychain prompt once with "Always Allow".
#
# The identity name is read from AARG_SIGN_IDENTITY (default "aarg-dev").
# Without a matching identity the script still installs, warns, and leaves
# the binary ad-hoc signed (the pre-existing behavior), so it is safe on a
# machine that has not done the one-time setup.

set -euo pipefail

IDENTITY="${AARG_SIGN_IDENTITY:-aarg-dev}"
BUNDLE_ID="com.joseym.aarg"

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  cat <<'HELP'
Usage: scripts/dev-reinstall.sh

Reinstalls aarg (cargo install --path . --force) and re-signs it on macOS
with a stable identity so keychain access persists across rebuilds.

One-time setup on macOS:

  1. Open Keychain Access > Certificate Assistant > Create a Certificate.
     Name: aarg-dev
     Identity Type: Self Signed Root
     Certificate Type: Code Signing
     Create it in the login keychain.

  2. Run this script. On the first aarg run that reads a key, macOS prompts
     once; choose "Always Allow". Every later reinstall re-signs with the
     same identity, so the grant holds and you are never prompted again.

Set AARG_SIGN_IDENTITY to use a certificate name other than "aarg-dev".
HELP
  exit 0
fi

cd "$(dirname "$0")/.."

echo "Installing aarg (release)..."
cargo install --path . --force

BIN="$(command -v aarg || echo "$HOME/.cargo/bin/aarg")"

if [ "$(uname)" != "Darwin" ]; then
  echo "Not macOS; skipping code signing."
  exit 0
fi

if security find-identity -v -p codesigning 2>/dev/null | grep -q "$IDENTITY"; then
  echo "Signing $BIN with identity '$IDENTITY' (identifier $BUNDLE_ID)..."
  codesign --force --sign "$IDENTITY" --identifier "$BUNDLE_ID" "$BIN"
  echo "Signed. Designated requirement:"
  codesign -d -r- "$BIN" 2>&1 | grep -i "designated" || true
else
  echo "WARNING: no code-signing identity '$IDENTITY' found." >&2
  echo "The binary is ad-hoc signed, so a keychain grant will not survive" >&2
  echo "the next reinstall. Run 'scripts/dev-reinstall.sh --help' for the" >&2
  echo "one-time setup that fixes this." >&2
fi
