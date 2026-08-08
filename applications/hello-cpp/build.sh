#!/usr/bin/env bash
#
# Build the Hello C++ application.
#
# tools/os101-c++ adds -fno-exceptions -fno-rtti and links os101-libc's C++
# runtime support; the output goes to target/hello-cpp.elf, which is what
# manifest.txt names and what kernel/build.rs embeds into the kernel image.
#
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${APP_DIR}/../.." && pwd)"

mkdir -p "${APP_DIR}/target"
"${ROOT}/tools/os101-c++" -O2 -Wall -Wextra -std=c++17 \
  -o "${APP_DIR}/target/hello-cpp.elf" \
  "${APP_DIR}/hello.cpp" "$@"
