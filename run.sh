#!/usr/bin/env bash
set -euo pipefail

# OS101 — build everything and show the OS.
#
# Builds the userspace apps, the kernel and the host tools, produces a
# bootable disk image, then puts the running machine on your screen using
# whatever display this host can actually provide:
#
#   * native qemu-system-x86_64 installed -> a normal QEMU window
#   * otherwise, Docker                   -> QEMU in a container, viewed
#                                            over VNC (macOS opens the
#                                            built-in Screen Sharing app)
#
# Usage:
#   ./run.sh                build and show the OS
#   ./run.sh --headless     build and boot with serial on this terminal
#   ./run.sh --build-only   build the image, do not boot
#   ./run.sh stop           stop a running containerised session

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO_ROOT"

# A custom target rather than one of rustup's: the kernel and the apps are
# built with hardware SSE2, which the stock x86_64-unknown-none forbids. Cargo
# names the output directory after the spec file, so TARGET is that name.
TARGET="x86_64-os101"
TARGET_SPEC="$REPO_ROOT/kernel/${TARGET}.json"
KERNEL_ELF="kernel/target/${TARGET}/release/os101-kernel"
IMAGE_PATH="build/os101-bios.img"
# The second disk holds whatever the user saves. It is deliberately not part
# of the build: the boot image is rebuilt from source every time, and anything
# kept on it would be thrown away with it.
DATA_PATH="build/os101-data.img"
DATA_MIB=64
# A virtual USB flash drive, attached over emulated USB mass storage — same
# deal as the data disk: created once, FAT32-formatted, never rebuilt.
USB_PATH="build/os101-usb.img"
USB_MIB=64
# Must match process::USER_BASE; ELF apps are linked to start here.
USER_BASE=$((0x8010000000))

MODE="show"
case "${1:-}" in
    stop)         exec ./tools/vnc.sh stop ;;
    --headless)   MODE="headless" ;;
    --build-only) MODE="build" ;;
    "")           ;;
    *)
        echo "usage: $0 [--headless | --build-only | stop]" >&2
        exit 2
        ;;
esac

# ── Preflight ───────────────────────────────────────────────────────────────

if ! command -v cargo &> /dev/null; then
    echo "❌ cargo not found. Install Rust: https://rustup.rs" >&2
    exit 1
fi

if [ ! -f "$TARGET_SPEC" ]; then
    echo "❌ Target spec '$TARGET_SPEC' not found." >&2
    exit 1
fi

# There is no `rustup target add` for a JSON target — `core` is compiled from
# source on every fresh checkout instead, and that needs the rust-src component.
if ! rustup component list --toolchain nightly 2>/dev/null | grep -q "^rust-src.*installed"; then
    echo "❌ rust-src missing for nightly, and building $TARGET needs it. Run:" >&2
    echo "   rustup component add rust-src --toolchain nightly" >&2
    exit 1
fi

mkdir -p build
BUILD_LOG="build/build.log"
: > "$BUILD_LOG"

# Cargo's own output is mostly warnings we already know about, so keep it in
# the log and only surface it when something actually breaks.
step() {
    local dir="$1"; shift
    if ! (cd "$dir" && "$@") >> "$BUILD_LOG" 2>&1; then
        echo >&2
        echo "❌ Failed in $dir: $*" >&2
        echo "── last 40 lines of $BUILD_LOG ──" >&2
        tail -40 "$BUILD_LOG" >&2
        exit 1
    fi
}

# ── Build ───────────────────────────────────────────────────────────────────

echo "🧪 Testing the package format..."
step os101-package cargo +nightly test --quiet

echo "📦 Building ELF userspace apps..."
# Each app must be built from inside its own directory. Cargo reads
# .cargo/config.toml relative to the working directory, not to the manifest,
# and the app's config is what applies `-Tuser.ld`. Building with
# --manifest-path from the repo root silently skips the linker script and
# produces an app linked at the wrong address.
manifests=()
while IFS= read -r line; do
    manifests+=("$line")
done < <(find applications -mindepth 2 -maxdepth 2 -name manifest.txt | sort)

for manifest in "${manifests[@]}"; do
    dir=$(dirname "$manifest")
    kind=$(grep -E '^\s*kind\s*=\s*' "$manifest" | tail -n1 | cut -d'=' -f2- | tr -d ' "')
    [ "$kind" = "elf" ] || continue

    # An ELF app is either a Rust crate built by cargo, or a C/C++ program
    # built by its own build.sh through tools/os101-cc. Both end up as a
    # static ELF linked at USER_BASE, which is all the kernel cares about.
    echo "   - $dir"
    if [ -f "$dir/Cargo.toml" ]; then
        step "$dir" cargo +nightly build --target "$TARGET_SPEC" --release --quiet
    elif [ -x "$dir/build.sh" ]; then
        step "$dir" ./build.sh
    else
        echo "❌ ELF app in $dir has neither Cargo.toml nor an executable build.sh" >&2
        exit 1
    fi

    # Catch the failure mode above: without user.ld the app still links, just
    # at a low address the kernel would have to rebase.
    binary=$(grep -E '^\s*binary\s*=\s*' "$manifest" | tail -n1 | cut -d'=' -f2- | tr -d ' "')
    if [ -n "$binary" ]; then
        entry=$(python3 -c \
            "import struct,sys; print(struct.unpack_from('<Q', open(sys.argv[1],'rb').read(), 24)[0])" \
            "$dir/$binary")
        if [ "$entry" -lt "$USER_BASE" ]; then
            printf '❌ %s linked at %#x, expected >= %#x (user.ld was not applied)\n' \
                "$dir" "$entry" "$USER_BASE" >&2
            exit 1
        fi
    fi
done

echo "📦 Building kernel..."
step kernel cargo +nightly build --release --quiet

echo "🔧 Building host tools..."
step tools cargo +nightly build --release --quiet

echo "💾 Creating bootable disk image..."
if [ ! -f "$KERNEL_ELF" ]; then
    echo "❌ Kernel ELF not found at $KERNEL_ELF" >&2
    exit 1
fi
./tools/target/release/os101-tools "$KERNEL_ELF" "$IMAGE_PATH"

echo "✅ Bootable image: $IMAGE_PATH"
echo "   also: build/os101-uefi.img  (UEFI GPT)"
echo "   also: build/os101.iso       (hybrid USB install medium)"

./tools/data-disk.sh "$DATA_PATH" "$DATA_MIB"
./tools/usb-disk.sh "$USB_PATH" "$USB_MIB"

if [ "$MODE" = "build" ]; then
    exit 0
fi

# ── Boot ────────────────────────────────────────────────────────────────────

# QEMU takes a write lock on the disk image, so a second instance fails with
# an opaque locking error. Say what is actually wrong instead.
if pgrep -f "qemu-system-x86_64.*os101-bios.img" > /dev/null 2>&1; then
    echo "❌ OS101 is already running." >&2
    echo "   Close its window, or stop it with: pkill -f qemu-system-x86_64" >&2
    echo "   (containerised session: ./run.sh stop)" >&2
    exit 1
fi

if [ "$MODE" = "headless" ]; then
    echo "🚀 Booting headless (serial on this terminal, Ctrl-C to stop)..."
    exec ./tools/qemu.sh \
        -drive format=raw,file="$IMAGE_PATH" \
        -drive format=raw,file="$DATA_PATH",if=ide,index=1,media=disk \
        -m 512M \
        -netdev user,id=n0 -device e1000,netdev=n0 \
        -usb -device usb-kbd -device usb-mouse \
        -drive if=none,id=stick,format=raw,file="$USB_PATH" \
        -device usb-storage,drive=stick \
        -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
        -display none \
        -serial stdio \
        -no-reboot
fi

# A container has no display, so it is the only case that needs VNC. If
# there is no QEMU at all, install one first — it needs no password, and a
# real window beats a VNC viewer.
if [ "$(./tools/qemu.sh --backend)" = "none" ]; then
    echo "ℹ️  No QEMU on this host yet — installing one (no password needed)."
    ./tools/install-qemu.sh
fi

if [ "$(./tools/qemu.sh --backend)" = "docker" ]; then
    echo "ℹ️  Only a containerised QEMU is available, and it has no display,"
    echo "   so the screen is exported over VNC instead."
    echo "   For a real window: ./tools/install-qemu.sh"
    exec ./tools/vnc.sh
fi

echo "🚀 Starting OS101..."
echo "   Try: help | pkg install /fat/demo.opk | gui   (F2 = launcher, ESC = shell)"
echo "   Quit: close the window, or Ctrl-C here."
echo
# Window size follows the guest framebuffer, which the kernel sets to ~90% of a
# Retina panel. No fullscreen — leave the host menu bar and dock visible.
exec ./tools/qemu.sh \
    -drive format=raw,file="$IMAGE_PATH" \
    -drive format=raw,file="$DATA_PATH",if=ide,index=1,media=disk \
    -m 512M \
    -netdev user,id=n0 -device e1000,netdev=n0 \
    -usb -device usb-kbd -device usb-mouse \
    -drive if=none,id=stick,format=raw,file="$USB_PATH" \
    -device usb-storage,drive=stick \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -serial stdio \
    -no-reboot
