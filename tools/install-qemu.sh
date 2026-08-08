#!/usr/bin/env bash
# Install a native qemu-system-x86_64 without needing an administrator
# password.
#
# The usual route on macOS is Homebrew, which writes to /opt/homebrew and so
# prompts for sudo. pkgx instead unpacks prebuilt binaries under $HOME, which
# means this can run unattended — and a native QEMU gives OS101 a real window
# with working keyboard and mouse, instead of a container with no display
# that has to be viewed over VNC.
set -euo pipefail

PKGX_BIN="$HOME/.local/bin/pkgx"

if command -v qemu-system-x86_64 &> /dev/null; then
    echo "✅ qemu-system-x86_64 is already installed: $(command -v qemu-system-x86_64)"
    exit 0
fi

if ! command -v pkgx &> /dev/null && [ ! -x "$PKGX_BIN" ]; then
    echo "⬇️  Installing pkgx into $PKGX_BIN ..."
    mkdir -p "$(dirname "$PKGX_BIN")"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    curl -fsSL "https://pkgx.sh/$(uname)/$(uname -m).tgz" -o "$tmp/pkgx.tgz"
    tar -xzf "$tmp/pkgx.tgz" -C "$tmp"
    mv "$tmp/pkgx" "$PKGX_BIN"
    chmod +x "$PKGX_BIN"
fi

PKGX="$(command -v pkgx || echo "$PKGX_BIN")"

echo "⬇️  Fetching QEMU (about 300 MB with its dependencies, one time only)..."
"$PKGX" +qemu.org -- qemu-system-x86_64 --version

echo
echo "✅ Native QEMU is ready. ./run.sh will now open a real window."
