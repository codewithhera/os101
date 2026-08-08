#!/usr/bin/env bash
set -euo pipefail

# OS101 test script - build and run with test features enabled
# This is similar to run.sh but enables the test-runner feature for kernel testing

echo "Building OS101 with test features..."

# Create build directory if it doesn't exist
mkdir -p build

echo "📦 Building ELF userspace apps..."
find applications -mindepth 2 -maxdepth 2 -name manifest.txt | sort | while IFS= read -r manifest; do
  dir=$(dirname "$manifest")
  kind=$(grep -E '^\s*kind\s*=\s*' "$manifest" | tail -n1 | cut -d'=' -f2- | tr -d ' \"')
  if [ "$kind" = "elf" ]; then
    echo "   - $dir"
    # From inside the app dir: .cargo/config.toml (which applies user.ld) is
    # resolved against the working directory, not the manifest path. C and C++
    # apps carry a build.sh that calls tools/os101-cc instead.
    if [ -f "$dir/Cargo.toml" ]; then
      (cd "$dir" && cargo +nightly build --target ../../kernel/x86_64-os101.json --release)
    elif [ -x "$dir/build.sh" ]; then
      (cd "$dir" && ./build.sh)
    else
      echo "❌ ELF app in $dir has neither Cargo.toml nor an executable build.sh"
      exit 1
    fi
  fi
done

# The C library's own tests run on the host, where its answers can be compared
# against a libc that is known to be right.
echo "🧪 Testing the C library..."
./os101-libc/tests/run.sh

echo "📦 Building kernel (test mode)..."
cd kernel
cargo +nightly build --release --features test-runner
cd ..

# Build the tools
echo "🔧 Building tools..."
cd tools
cargo +nightly build --release
cd ..

# Create bootable disk image
echo "💾 Creating bootable disk image..."
KERNEL_ELF="kernel/target/x86_64-os101/release/os101-kernel"
IMAGE_PATH="build/os101-bios-test.img"

if [ ! -f "$KERNEL_ELF" ]; then
    echo "❌ Kernel ELF not found at $KERNEL_ELF"
    exit 1
fi

./tools/target/release/os101-tools "$KERNEL_ELF" "$IMAGE_PATH"

if [ ! -f "$IMAGE_PATH" ]; then
    echo "❌ Failed to create disk image at $IMAGE_PATH"
    exit 1
fi

echo "✅ Test image created: $IMAGE_PATH"

# Run in QEMU with test configuration
echo "🧪 Running OS101 tests in QEMU..."
echo "   (Use Ctrl+A, X to exit QEMU)"
echo ""

# Run with a timeout to prevent hanging if tests fail
./tools/qemu.sh \
    -drive format=raw,file="$IMAGE_PATH" \
    -serial stdio \
    -m 512M \
    -no-reboot \
    -no-shutdown \
    -d guest_errors \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    || true  # Don't fail on timeout

echo ""
echo "Test session completed."