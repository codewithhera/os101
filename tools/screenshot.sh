#!/usr/bin/env bash
# Boot OS101 headlessly, optionally type some commands, and save a PNG of the
# screen plus the serial log.
#
# Useful for documenting the UI and for checking a change without watching a
# QEMU window. Works with every backend tools/qemu.sh supports, including the
# container runner, which has no display of its own.
#
# Usage:
#   tools/screenshot.sh <out.png> [step ...]
#
# Steps are passed straight to tools/qemu-runner/drive.py:
#   type:<text>     send text as keystrokes
#   key:<name>      send one key (ret, esc, f2, spc, ...)
#   wait:<seconds>  pause
#
# Example — install a package and photograph the launcher:
#   tools/screenshot.sh build/launcher.png \
#       "type:pkg install /fat/demo.opk" key:ret wait:2 \
#       "type:gui" key:ret wait:5 key:f2 wait:4
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

OUT_PNG="${1:?usage: tools/screenshot.sh <out.png> [step ...]}"
shift || true

IMAGE="build/os101-bios.img"
BOOT_WAIT=20

# Photographing a stale image is worse than not photographing at all: the
# picture looks plausible and quietly documents code that is no longer there.
stale=""
if [ -f "$IMAGE" ]; then
    stale=$(find kernel/src applications os101-package/src tools/src \
                 -name '*.rs' -newer "$IMAGE" -print -quit 2>/dev/null || true)
fi
if [ ! -f "$IMAGE" ] || [ -n "$stale" ]; then
    echo "🔨 Sources changed since the last image — rebuilding..."
    ./run.sh --build-only
fi
./tools/data-disk.sh build/os101-data.img 64
./tools/usb-disk.sh build/os101-usb.img 64

mkdir -p build

# The driver script paces monitor commands with `sleep`, so it has to be
# *run* with its output piped into QEMU's stdin monitor — which means the
# native and container paths differ in more than just a binary name.
echo "🚀 Booting OS101 (about $((BOOT_WAIT + 15))s)..."

if [ "$(tools/qemu.sh --backend)" != "docker" ]; then
    python3 tools/qemu-runner/drive.py build/drive.sh "$BOOT_WAIT" \
        "$@" "shot:$REPO_ROOT/build/screen.ppm" > /dev/null
    sh build/drive.sh | tools/qemu.sh \
        -drive format=raw,file=build/os101-bios.img \
        -drive format=raw,file=build/os101-data.img,if=ide,index=1,media=disk \
        -m 512M \
        -netdev user,id=n0 -device e1000,netdev=n0 \
        -usb -device usb-kbd -device usb-mouse \
        -drive if=none,id=stick,format=raw,file=build/os101-usb.img \
        -device usb-storage,drive=stick \
        -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
        -display none \
        -monitor stdio \
        -serial file:build/serial.log \
        -no-reboot > /dev/null 2>&1 || true
else
    python3 tools/qemu-runner/drive.py build/drive.sh "$BOOT_WAIT" \
        "$@" "shot:/work/screen.ppm" > /dev/null
    if ! docker image inspect os101-qemu &> /dev/null; then
        echo "🐳 Building the QEMU runner container..."
        docker build -t os101-qemu tools/qemu-runner > /dev/null
    fi
    # QEMU is invoked directly here rather than through tools/qemu.sh, so the
    # video card it sets up has to be repeated: the default one has too little
    # memory for the mode the kernel asks for, and answers a mode that does not
    # fit by quietly staying at the bootloader's 1280x720.
    docker run --rm -v "$REPO_ROOT/build:/work" os101-qemu \
        'sh /work/drive.sh | qemu-system-x86_64 \
            -drive format=raw,file=/work/os101-bios.img \
            -drive format=raw,file=/work/os101-data.img,if=ide,index=1,media=disk \
            -m 512M -netdev user,id=n0 -device e1000,netdev=n0 \
            -usb -device usb-kbd -device usb-mouse \
            -drive if=none,id=stick,format=raw,file=/work/os101-usb.img \
            -device usb-storage,drive=stick \
            -vga none -device VGA,vgamem_mb=64 \
            -display none -monitor stdio \
            -serial file:/work/serial.log -no-reboot > /dev/null 2>&1' \
        > /dev/null 2>&1 || true
fi

if [ ! -s build/screen.ppm ]; then
    echo "❌ No screenshot captured. See build/serial.log for what the kernel printed." >&2
    exit 1
fi

python3 tools/ppm2png.py build/screen.ppm "$OUT_PNG"
rm -f build/screen.ppm build/drive.sh
echo "📸 $OUT_PNG"
echo "📝 build/serial.log"
