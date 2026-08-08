#!/usr/bin/env bash
# Boot OS101 in a window you can actually click on.
#
# With no native QEMU there is no local display, so QEMU runs in the
# container and exposes its screen over VNC on port 5900. macOS has a VNC
# client built in (Screen Sharing), so nothing extra needs installing.
#
# Usage:
#   tools/vnc.sh          boot and open the viewer
#   tools/vnc.sh stop     shut the machine down
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

CONTAINER="os101-vnc"
PORT=5900
IMAGE="build/os101-bios.img"

stop_existing() {
    if docker ps -aq -f "name=^${CONTAINER}$" | grep -q .; then
        docker rm -f "$CONTAINER" > /dev/null 2>&1 || true
    fi
}

if [ "${1:-}" = "stop" ]; then
    stop_existing
    echo "🛑 OS101 stopped."
    exit 0
fi

if [ ! -f "$IMAGE" ]; then
    echo "❌ $IMAGE not found. Build it with ./run.sh first." >&2
    exit 1
fi

if ! docker info &> /dev/null; then
    echo "❌ Docker is not running. Start Docker Desktop and try again." >&2
    exit 1
fi

if ! docker image inspect os101-qemu &> /dev/null; then
    echo "🐳 Building the QEMU runner container..."
    docker build -t os101-qemu tools/qemu-runner > /dev/null
fi

stop_existing

# The VNC password is not about security — the port is bound to this machine
# only. macOS Screen Sharing hangs at "Connecting" against a server that
# offers no authentication at all, so QEMU has to offer VNC auth. The
# protocol truncates passwords to 8 characters, hence a short one.
VNC_PASSWORD="os101"

# This path builds its own QEMU command rather than going through
# tools/qemu.sh, so it needs that script's video card repeated here: the
# default card's 16 MiB is too small for the mode the kernel asks for, and a
# card that cannot fit the mode stays at the bootloader's 1280x720 without
# saying so.
docker run -d --name "$CONTAINER" \
    -p "127.0.0.1:${PORT}:5900" \
    -v "$REPO_ROOT/build:/work" \
    os101-qemu \
    "qemu-system-x86_64 \
        -drive format=raw,file=/work/os101-bios.img \
        -drive format=raw,file=/work/os101-data.img,if=ide,index=1,media=disk \
        -m 512M \
        -vga none -device VGA,vgamem_mb=64 \
        -netdev user,id=n0 -device e1000,netdev=n0 \
        -usb -device usb-kbd -device usb-mouse \
        -object secret,id=vncsec,data=${VNC_PASSWORD} \
        -vnc 0.0.0.0:0,password-secret=vncsec \
        -serial file:/work/serial.log \
        -no-reboot" > /dev/null

echo "🚀 OS101 is booting..."
# Wait for QEMU to accept VNC connections before handing over to the viewer.
for _ in $(seq 1 30); do
    if nc -z 127.0.0.1 "$PORT" 2>/dev/null; then
        break
    fi
    sleep 1
done

echo "🖥️  Screen:   vnc://localhost:${PORT}"
echo "🔑 Password: ${VNC_PASSWORD}"
echo "📝 Serial log: build/serial.log"
echo
echo "   Try:  help          list every command"
echo "         pkg install /fat/demo.opk"
echo "         pkg list"
echo "         gui           enter the desktop  (F2 = launcher, ESC = leave)"
echo
echo "   Stop it with: ./tools/vnc.sh stop"

if [ "$(uname)" = "Darwin" ]; then
    # Credentials in the URL so Screen Sharing connects without prompting.
    open "vnc://:${VNC_PASSWORD}@localhost:${PORT}"
fi
