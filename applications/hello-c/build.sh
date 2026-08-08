#!/usr/bin/env bash
#
# Build the Hello C application.
#
# The output goes to target/hello-c.elf, which is what manifest.txt names and
# what kernel/build.rs embeds into the kernel image. It is under target/ because
# that is the directory the repository's .gitignore already keeps out of git for
# every application.
#
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${APP_DIR}/../.." && pwd)"

mkdir -p "${APP_DIR}/target"
"${ROOT}/tools/os101-cc" -O2 -Wall -Wextra \
  -o "${APP_DIR}/target/hello-c.elf" \
  "${APP_DIR}/hello.c" "$@"
