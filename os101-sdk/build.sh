#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <app-crate-dir>"
  exit 1
fi

APP_DIR="$1"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Apps are built for the same custom hardware-SSE2 target as the kernel, and
# cargo names the output directory after the spec file.
TARGET="x86_64-os101"
TARGET_SPEC="$REPO_ROOT/kernel/${TARGET}.json"

if [ ! -f "${APP_DIR}/Cargo.toml" ]; then
  echo "error: ${APP_DIR}/Cargo.toml not found"
  exit 1
fi

# Build from inside the app directory. Cargo resolves .cargo/config.toml
# against the working directory rather than the manifest, and that config is
# what applies `-Tuser.ld`; building via --manifest-path from elsewhere links
# the app at the wrong address without any warning.
(cd "${APP_DIR}" && cargo +nightly build --target "${TARGET_SPEC}" --release)

CRATE_NAME="$(awk -F'=' '/^name[[:space:]]*=/{gsub(/[ "]/,"",$2); print $2; exit}' "${APP_DIR}/Cargo.toml")"
OUT="${APP_DIR}/target/${TARGET}/release/${CRATE_NAME}"
echo "built: ${OUT}"
