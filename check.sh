#!/usr/bin/env bash
set -euo pipefail

# OS101 check script - verify development environment setup
# Run this before building or running OS101

echo "Checking OS101 development environment..."

# Check Rust toolchain
if ! command -v rustc &> /dev/null; then
    echo "❌ rustc not found. Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

if ! command -v cargo &> /dev/null; then
    echo "❌ cargo not found. Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

echo "✅ Rust toolchain found"

# Check Rust version
RUST_VERSION=$(rustc --version | awk '{print $2}')
echo "📋 Rust version: $RUST_VERSION"

# Check the build target. It is a JSON spec in the tree rather than one of
# rustup's, because the kernel needs hardware SSE2 and no shipped bare-metal
# target allows it — so what has to be present is the file, plus the rust-src
# component that `core` is rebuilt from.
if [ ! -f kernel/x86_64-os101.json ]; then
    echo "❌ Target spec 'kernel/x86_64-os101.json' is missing from this checkout."
    exit 1
fi

echo "✅ Target spec kernel/x86_64-os101.json present"

# Check for nightly toolchain (for some kernel features)
if ! rustup toolchain list | grep -q "nightly"; then
    echo "⚠️  Nightly toolchain not found. Some features may not work. Install with: rustup toolchain install nightly"
else
    echo "✅ Nightly toolchain available"
fi

# Check QEMU. tools/qemu.sh knows several ways to provide one, and a missing
# QEMU is not fatal because ./run.sh can install a native one unattended.
case "$(./tools/qemu.sh --backend)" in
    native)
        echo "✅ QEMU found: $(qemu-system-x86_64 --version | head -n 1)"
        ;;
    pkgx)
        echo "✅ QEMU will run natively via pkgx"
        ;;
    docker)
        echo "⚠️  Only a containerised QEMU is available, which has no display"
        echo "   (the screen has to be viewed over VNC). For a real window:"
        echo "   ./tools/install-qemu.sh"
        ;;
    *)
        echo "⚠️  No QEMU yet. ./run.sh will install one, or do it now with:"
        echo "   ./tools/install-qemu.sh    (no administrator password needed)"
        echo "   macOS alternative:         brew install qemu"
        echo "   Ubuntu/Debian:             sudo apt install qemu-system-x86"
        ;;
esac

# Check for required Rust components
# rust-src is not optional any more: a JSON target has no prebuilt `core`.
if ! rustup component list --toolchain nightly | grep -q "rust-src.*installed"; then
    echo "❌ rust-src not installed for nightly, and the kernel target needs it."
    echo "   Run: rustup component add rust-src --toolchain nightly"
    exit 1
else
    echo "✅ rust-src installed for nightly"
fi

if ! rustup component list --toolchain nightly | grep -q "llvm-tools-preview.*installed"; then
    echo "⚠️  llvm-tools-preview not installed for nightly. Run: rustup component add llvm-tools-preview --toolchain nightly"
else
    echo "✅ llvm-tools-preview installed for nightly"
fi

echo ""
echo "🎉 Environment check complete! You're ready to build OS101."
echo "   Run './run.sh' to build and boot the OS."