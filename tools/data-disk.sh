#!/usr/bin/env bash
# Make sure the user's data disk exists, without ever overwriting one.
#
# OS101 keeps everything it saves — downloads, the chosen wallpaper — on a
# second disk that the build never touches. That separation is the whole point:
# `./run.sh` regenerates the boot image from source on every run, so anything
# stored there would not survive a rebuild, let alone a reboot.
#
# Usage: tools/data-disk.sh <path> <size-in-MiB>
set -euo pipefail

PATH_OUT="${1:?usage: tools/data-disk.sh <path> <size-in-MiB>}"
SIZE_MIB="${2:-64}"

if [ -f "$PATH_OUT" ]; then
    exit 0
fi

mkdir -p "$(dirname "$PATH_OUT")"
# A blank disk: the kernel finds no filesystem on it and formats it on the
# first boot. `bs=1048576` rather than `1M`/`1m`, which differ between the BSD
# and GNU versions of dd.
dd if=/dev/zero of="$PATH_OUT" bs=1048576 count="$SIZE_MIB" 2> /dev/null
echo "💽 Created ${PATH_OUT} (${SIZE_MIB} MiB) — your saved files live here."
