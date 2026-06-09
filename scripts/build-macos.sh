#!/usr/bin/env bash
# Build a signed + notarized Universal (Apple Silicon + Intel) Moonshell on your Mac.
#
# Prereqs (one-time):
#   - Apple Developer Program membership
#   - "Developer ID Application" certificate installed in your login keychain
#   - rustup target add aarch64-apple-darwin x86_64-apple-darwin
#
# Credentials: copy scripts/signing.env.example -> scripts/signing.env, fill it in.
# scripts/signing.env is gitignored. Then run:  ./scripts/build-macos.sh
set -euo pipefail
cd "$(dirname "$0")/.."

ENV_FILE="scripts/signing.env"
if [[ -f "$ENV_FILE" ]]; then
  set -a; source "$ENV_FILE"; set +a
else
  echo "warn: $ENV_FILE not found — building unsigned unless env vars are already set." >&2
fi

# Confirm a signing identity is available (set APPLE_SIGNING_IDENTITY or pick first Developer ID).
if [[ -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  echo "note: APPLE_SIGNING_IDENTITY unset; codesign will use the only matching identity if present." >&2
  security find-identity -v -p codesigning | grep "Developer ID Application" || true
fi

echo "==> Building Universal binary (this compiles Rust for both arches)…"
pnpm install --frozen-lockfile
pnpm tauri build --target universal-apple-darwin

echo "==> Done. Artifacts:"
ls -1 src-tauri/target/universal-apple-darwin/release/bundle/dmg/*.dmg 2>/dev/null || true
ls -d  src-tauri/target/universal-apple-darwin/release/bundle/macos/*.app 2>/dev/null || true
