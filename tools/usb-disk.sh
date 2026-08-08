#!/usr/bin/env bash
# Make sure a test USB stick image exists, without ever overwriting one.
#
# This is what `-device usb-storage` in run.sh/screenshot.sh attaches to the
# emulated machine, standing in for a real USB flash drive. It is created
# once, formatted FAT32 with a couple of sample files, and left alone after
# that — exactly like tools/data-disk.sh does for the ATA data disk, and for
# the same reason: anything OS101 (or a test) writes onto it should survive
# the next `./run.sh`.
#
# Usage: tools/usb-disk.sh <path> <size-in-MiB>
set -euo pipefail

PATH_OUT="${1:?usage: tools/usb-disk.sh <path> <size-in-MiB>}"
SIZE_MIB="${2:-64}"

if [ -f "$PATH_OUT" ]; then
    exit 0
fi

mkdir -p "$(dirname "$PATH_OUT")"
dd if=/dev/zero of="$PATH_OUT" bs=1048576 count="$SIZE_MIB" 2> /dev/null

# Best-effort real FAT32 formatting, using whatever this host has. A blank
# image is still useful — OS101 will just report it as "attached, but not
# FAT32" until it is formatted some other way — so a host with neither tool
# is not a failure, just a less interesting demo.
format_macos() {
    command -v hdiutil &> /dev/null && command -v newfs_msdos &> /dev/null
}
format_linux() {
    command -v mkfs.vfat &> /dev/null
}

if format_macos; then
    dev="$(hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount "$PATH_OUT" | awk '{print $1; exit}')"
    newfs_msdos -F 32 -v OS101USB "$dev" > /dev/null
    diskutil mount "$dev" > /dev/null
    vol="/Volumes/OS101USB"
    echo "This is a sample USB flash drive for OS101 to read and write." > "$vol/readme.txt"
    mkdir -p "$vol/Photos"
    echo "A file inside a subdirectory, to test folder browsing." > "$vol/Photos/notes.txt"
    diskutil unmount "$dev" > /dev/null
    hdiutil detach "$dev" > /dev/null
    echo "💽 Created ${PATH_OUT} (${SIZE_MIB} MiB, FAT32) — a virtual USB drive for OS101."
elif format_linux; then
    mkfs.vfat -F 32 -n OS101USB "$PATH_OUT" > /dev/null
    echo "💽 Created ${PATH_OUT} (${SIZE_MIB} MiB, FAT32) — a virtual USB drive for OS101."
else
    echo "💽 Created ${PATH_OUT} (${SIZE_MIB} MiB, blank) — install mtools or dosfstools to have it pre-formatted."
fi
