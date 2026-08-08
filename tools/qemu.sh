#!/usr/bin/env bash
# Run qemu-system-x86_64 by whatever means this host allows.
#
# In preference order:
#   1. qemu-system-x86_64 on PATH — Homebrew, MacPorts, a distro package
#   2. pkgx — prebuilt binaries under $HOME, so no administrator password
#   3. a container — last resort, because it has no display at all and the
#      screen has to be exported over VNC
#
# Usage:
#   tools/qemu.sh <qemu args...>   run QEMU
#   tools/qemu.sh --backend        print which of the three would be used
#
# Paths in the QEMU arguments must live under build/, which is what gets
# mounted into the container (as /work).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE_NAME="os101-qemu"

pkgx_bin() {
    if command -v pkgx &> /dev/null; then
        command -v pkgx
    elif [ -x "$HOME/.local/bin/pkgx" ]; then
        echo "$HOME/.local/bin/pkgx"
    else
        return 1
    fi
}

detect_backend() {
    if command -v qemu-system-x86_64 &> /dev/null; then
        echo native
    elif pkgx_bin > /dev/null 2>&1; then
        echo pkgx
    elif command -v docker &> /dev/null && docker info &> /dev/null; then
        echo docker
    else
        echo none
    fi
}

BACKEND="$(detect_backend)"

if [ "${1:-}" = "--backend" ]; then
    echo "$BACKEND"
    exit 0
fi

# Wire the emulated PC speaker to the host so the kids' games can beep.
# Modern QEMU keeps the speaker silent until an audiodev is named; without
# this the kernel's tone generator runs but nothing is heard.
host_audio_driver() {
    case "$BACKEND" in
        native|pkgx) ;;
        *) echo none; return ;;
    esac
    case "$(uname -s)" in
        Darwin) echo coreaudio ;;
        Linux)
            # Prefer Pulse/PipeWire, then ALSA, then SDL. pkgx builds usually
            # ship at least one of these; if none, stay silent rather than fail.
            echo pulseaudio
            ;;
        *) echo none ;;
    esac
}

AUDIO_ARGS=()
if [ "$BACKEND" = "native" ] || [ "$BACKEND" = "pkgx" ]; then
    driver="$(host_audio_driver)"
    if [ "$driver" != "none" ]; then
        AUDIO_ARGS=(-audiodev "$driver,id=os101snd" -machine pcspk-audiodev=os101snd)
    fi
fi

# Give the guest one video card with enough memory for the mode the kernel asks
# for at boot: 3112x2008x4 is about 24 MiB, and the default card has 16 MiB. A
# card that cannot fit the mode reports no error, it just stays at the
# bootloader's 1280x720, so getting this wrong is quiet rather than loud.
#
# The card is named outright, and the automatic one turned off, because the
# automatic one cannot be relied on: `-display none` omits it entirely — which
# also stops the boot dead, since the bootloader needs a card to set up a
# framebuffer — and a `-global` override of its memory is silently dropped on
# some builds. Naming it makes every entry point get the same card.
VIDEO_ARGS=(-vga none -device VGA,vgamem_mb=64)

case "$BACKEND" in
    native)
        exec qemu-system-x86_64 "${VIDEO_ARGS[@]}" "${AUDIO_ARGS[@]}" "$@"
        ;;
    pkgx)
        exec "$(pkgx_bin)" +qemu.org -- \
            qemu-system-x86_64 "${VIDEO_ARGS[@]}" "${AUDIO_ARGS[@]}" "$@"
        ;;
    docker)
        if ! docker image inspect "$IMAGE_NAME" &> /dev/null; then
            echo "🐳 qemu-system-x86_64 not found; building the $IMAGE_NAME container..." >&2
            docker build -t "$IMAGE_NAME" "$REPO_ROOT/tools/qemu-runner" >&2
        fi
        echo "🐳 Running QEMU in a container (no native QEMU installed)." >&2

        # Rewrite build/ paths to the container's mount point. Done with sed
        # rather than ${var//pat/repl} because the replacement text contains
        # slashes, which makes the brace form both unreadable and subtly
        # wrong on bash 3.2 (the version macOS ships).
        args=("${VIDEO_ARGS[@]}")
        for arg in "$@"; do
            args+=("$(printf '%s' "$arg" \
                | sed -e "s|${REPO_ROOT}/build/|/work/|g" -e "s|build/|/work/|g")")
        done

        exec docker run --rm -i \
            -v "$REPO_ROOT/build:/work" \
            "$IMAGE_NAME" \
            "qemu-system-x86_64 $(printf '%q ' "${args[@]}")"
        ;;
    *)
        echo "❌ No way to run QEMU on this host." >&2
        echo "   Install one without needing a password:  ./tools/install-qemu.sh" >&2
        echo "   Or with Homebrew:                        brew install qemu" >&2
        exit 1
        ;;
esac
