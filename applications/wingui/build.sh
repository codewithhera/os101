#!/usr/bin/env bash
set -euo pipefail
APP="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$APP/../.." && pwd)"
mkdir -p "$APP/target"
"$ROOT/tools/os101-cc" -O2 -o "$APP/target/wingui.elf" "$APP/main.c"
