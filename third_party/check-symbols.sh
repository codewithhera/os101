#!/usr/bin/env bash
#
# Prove that the freestanding QuickJS build has nothing left to resolve.
#
# The interesting failure mode for this library is not a compile error, it is a
# symbol that used to come for free and quietly stopped: thirty of the libm
# functions QuickJS needs are weak exports from compiler_builtins rather than
# anything we wrote, and the `mem` group behind them is gated on a Cargo feature
# that a `-Z build-std` invocation has to remember to pass. Both would surface as
# a wall of undefined symbols at the end of a long kernel link, a long way from
# the cause. This script moves that check to the front.
#
# Usage:
#   ./check-symbols.sh                       # the kernel's own target
#   ./check-symbols.sh path/to/target.json   # some other one
#
# Exits non-zero if any symbol is unaccounted for. It also exits non-zero for a
# soft-float target such as the stock x86_64-unknown-none, on purpose — see
# require_hardware_float in libc-shim/rust/build.rs.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
crate="$here/libc-shim/rust"
target="${1:-$here/../kernel/x86_64-os101.json}"

# Cargo runs from the crate directory, so a relative path to a target JSON would
# be resolved against the wrong place.
if [[ "$target" == *.json ]]; then
    target="$(cd "$(dirname "$target")" && pwd)/$(basename "$target")"
fi

toolchain_bin="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin"
nm="$toolchain_bin/llvm-nm"
if [[ ! -x "$nm" ]]; then
    echo "llvm-nm not found at $nm; install the llvm-tools-preview component" >&2
    exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "==> building the shim for $target"
build_args=(--offline --release --target "$target")
if [[ "$target" == *.json ]]; then
    # A custom target has no prebuilt core, so the sysroot has to be built too —
    # and compiler-builtins-mem is what puts memcpy and friends in it.
    build_args+=(
        -Z json-target-spec
        -Z build-std=core,alloc,compiler_builtins
        -Z build-std-features=compiler-builtins-mem
    )
fi
( cd "$crate" && CARGO_TARGET_DIR="$work/target" cargo build "${build_args[@]}" )

target_dir_name="$(basename "$target" .json)"
rlib="$work/target/$target_dir_name/release/libos101_libc_shim.rlib"
[[ -f "$rlib" ]] || { echo "no rlib at $rlib" >&2; exit 1; }

# The rlib has the QuickJS archive bundled into it, so one file holds everything
# this library contributes to the kernel's link.
"$nm" --defined-only --extern-only "$rlib" 2>/dev/null | awk 'NF>=3 {print $NF}' | sort -u > "$work/defined"
"$nm" -u "$rlib" 2>/dev/null | awk 'NF==2 {print $NF}' | sort -u > "$work/undefined"
comm -23 "$work/undefined" "$work/defined" > "$work/outstanding"

# Everything core, alloc and compiler_builtins define. For a custom target these
# live in the sysroot that build-std just produced.
sysroot_libs=("$work/target/$target_dir_name/release/deps")
if [[ "$target" != *.json ]]; then
    sysroot_libs+=("$(rustc --print sysroot)/lib/rustlib/$target/lib")
fi
: > "$work/sysroot-defined"
for dir in "${sysroot_libs[@]}"; do
    for lib in "$dir"/lib{core,alloc,compiler_builtins}-*.rlib; do
        [[ -f "$lib" ]] || continue
        "$nm" --defined-only --extern-only "$lib" 2>/dev/null | awk 'NF>=3 {print $NF}'
    done
done | sort -u > "$work/sysroot-defined"

# Symbols only the kernel can define, so they are resolved in the final kernel
# link and never here. The allocator ones come from `#[global_allocator]` in
# kernel/src/allocator.rs; `os101_qjs_native_dispatch` is the Rust end of the
# trampoline in libc-shim/src/quickjs_glue.c, and lives in kernel/src/quickjs/
# because it is the embedder's registry that it looks a function up in. They are
# matched as substrings because Rust's v0 mangling wraps the allocator ones in a
# per-compilation hash and a length prefix.
kernel_provided=(
    __rust_alloc
    __rust_alloc_zeroed
    __rust_dealloc
    __rust_no_alloc_shim_is_unstable_v2
    __rust_realloc
    os101_qjs_native_dispatch
)

fail=0
while read -r symbol; do
    [[ -n "$symbol" ]] || continue
    if grep -qxF "$symbol" "$work/sysroot-defined"; then
        continue
    fi
    kernel=0
    for provided in "${kernel_provided[@]}"; do
        if [[ "$symbol" == *"$provided"* ]]; then
            kernel=1
            break
        fi
    done
    if [[ $kernel -eq 1 ]]; then
        echo "provided by the kernel: $symbol"
        continue
    fi
    echo "UNRESOLVED: $symbol"
    fail=1
done < "$work/outstanding"

if [[ $fail -ne 0 ]]; then
    echo
    echo "The symbols above are provided by neither this library, nor core/alloc/"
    echo "compiler_builtins, nor the kernel's global allocator. Something that"
    echo "used to be free has gone away; see libc-shim/rust/src/math.rs."
    exit 1
fi

echo "==> every symbol accounted for"
echo "    library size: $(
    find "$work/target/$target_dir_name/release/build" -name libquickjs.a -exec ls -l {} \; |
    awk '{printf "%d bytes (%.1f MiB)", $5, $5/1048576}'
)"
