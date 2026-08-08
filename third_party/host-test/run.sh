#!/usr/bin/env bash
#
# Build and run the host harness. Object files go to /tmp; nothing is written
# into the repository.
#
# The host build uses the shim's headers and the shim's printf, string and
# stdlib, exactly as the kernel build does. Only the four things a hosted process
# has to borrow — the real allocator, the real clock, a real write() and abort —
# come from the platform, and they come through host_platform.c. -U__APPLE__ is
# what keeps quickjs.c off the Darwin-specific malloc_size path so that it uses
# the shim's exact-size malloc_usable_size instead, which is what makes the
# memory numbers below transfer to the kernel.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$here/.."
build="${TMPDIR:-/tmp}/os101-quickjs-host-test"
mkdir -p "$build"

# Takes a target triple, because the stack figures the harness prints are
# architecture-specific and the kernel's architecture is x86_64: on an Apple
# Silicon machine `./run.sh x86_64-apple-darwin` runs the same harness under
# Rosetta and reports the numbers that actually apply to the kernel.
triple="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
version="$(tr -d '[:space:]' < "$root/quickjs/VERSION")"
build="$build/$triple"
mkdir -p "$build"

shim_flags=(
    --target="$triple"
    -ffreestanding
    -U__APPLE__
    -std=gnu11
    -O2
    -fwrapv
    -fno-builtin
    -fno-strict-aliasing
    -DCONFIG_VERSION="\"$version\""
    -I"$root/libc-shim/include"
    -I"$root/quickjs/src"
)

echo "==> compiling QuickJS $version and the shim for $triple"
objects=()
for unit in quickjs dtoa libregexp libunicode cutils; do
    clang "${shim_flags[@]}" -c "$root/quickjs/src/$unit.c" -o "$build/$unit.o"
    objects+=("$build/$unit.o")
done
for unit in printf string stdlib; do
    clang "${shim_flags[@]}" -c "$root/libc-shim/src/$unit.c" -o "$build/$unit.o"
    objects+=("$build/$unit.o")
done
for unit in host_glue driver; do
    clang "${shim_flags[@]}" -c "$here/$unit.c" -o "$build/$unit.o"
    objects+=("$build/$unit.o")
done

echo "==> compiling the platform layer against the system headers"
clang --target="$triple" -std=gnu11 -O2 -c "$here/host_platform.c" \
    -o "$build/host_platform.o"
objects+=("$build/host_platform.o")

clang --target="$triple" -o "$build/harness" "${objects[@]}"

echo "==> running"
exec "$build/harness"
