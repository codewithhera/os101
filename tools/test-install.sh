#!/usr/bin/env bash
# End-to-end install test:
#   1. Boot the install medium
#   2. Auto-install onto a blank ATA slave (build/os101-target.img)
#   3. Reboot from that target image and confirm the shell comes up
#
# Usage: tools/test-install.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

IMAGE="build/os101-bios.img"
TARGET="build/os101-target.img"
DATA="build/os101-data.img"
USB="build/os101-usb.img"
LOG1="build/serial-install.log"
LOG2="build/serial-boot-target.log"

if [ ! -f "$IMAGE" ]; then
    echo "🔨 No boot image yet — building..."
    ./run.sh --build-only
fi
./tools/data-disk.sh "$DATA" 64
./tools/usb-disk.sh "$USB" 64

echo "💾 Creating blank install target ($TARGET)..."
dd if=/dev/zero of="$TARGET" bs=1048576 count=64 2>/dev/null

write_autoinst() {
    local text="$1"
    if [[ "$(uname)" != "Darwin" ]]; then
        echo "❌ autoinst helper currently needs macOS hdiutil" >&2
        exit 1
    fi
    local dev
    dev="$(hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount "$USB" | awk '{print $1; exit}')"
    diskutil mount "$dev" >/dev/null
    if [ -n "$text" ]; then
        printf '%s\n' "$text" > /Volumes/OS101USB/autoinst.txt
    else
        rm -f /Volumes/OS101USB/autoinst.txt /Volumes/OS101USB/._autoinst.txt
    fi
    sync
    diskutil unmount "$dev" >/dev/null
    hdiutil detach "$dev" >/dev/null
}

echo "📝 Arming /usb/autoinst.txt → slave"
write_autoinst slave

echo "🚀 Phase 1: boot install medium and clone onto target..."
python3 tools/qemu-runner/drive.py build/drive.sh 95
sh build/drive.sh | ./tools/qemu.sh \
    -drive format=raw,file="$IMAGE" \
    -drive format=raw,file="$TARGET",if=ide,index=1,media=disk \
    -m 512M \
    -netdev user,id=n0 -device e1000,netdev=n0 \
    -usb -device usb-kbd -device usb-mouse \
    -drive if=none,id=stick,format=raw,file="$USB" \
    -device usb-storage,drive=stick \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -display none \
    -monitor stdio \
    -serial "file:$LOG1" \
    -no-reboot >/dev/null 2>&1 || true

if ! grep -q "autoinst finished" "$LOG1"; then
    echo "❌ Phase 1 failed — see $LOG1" >&2
    write_autoinst ""
    exit 1
fi

python3 - <<PY
bios = open("$IMAGE", "rb").read(512)
tgt = open("$TARGET", "rb").read(512)
assert bios == tgt, "installed MBR does not match install medium"
print("✅ Phase 1: target MBR matches install medium")
PY

write_autoinst ""
echo "🧹 Disarmed autoinst"

echo "🚀 Phase 2: boot from installed target..."
python3 tools/qemu-runner/drive.py build/drive.sh 70
sh build/drive.sh | ./tools/qemu.sh \
    -drive format=raw,file="$TARGET" \
    -drive format=raw,file="$DATA",if=ide,index=1,media=disk \
    -m 512M \
    -netdev user,id=n0 -device e1000,netdev=n0 \
    -usb -device usb-kbd -device usb-mouse \
    -drive if=none,id=stick,format=raw,file="$USB" \
    -device usb-storage,drive=stick \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -display none \
    -monitor stdio \
    -serial "file:$LOG2" \
    -no-reboot >/dev/null 2>&1 || true

if ! grep -q "OS101 v" "$LOG2" || ! grep -q "os101>" "$LOG2"; then
    echo "❌ Phase 2 failed — installed disk did not reach the shell. See $LOG2" >&2
    exit 1
fi

echo "✅ Phase 2: installed disk booted to the shell"
echo "✅ Install → permanent boot verified"
echo "   logs: $LOG1 , $LOG2"
echo "   installed image: $TARGET"
