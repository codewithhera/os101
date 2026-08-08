#!/usr/bin/env python3
"""Convert a binary PPM (P6) to PNG.

QEMU's `screendump` emits P6 PPM. Rather than depend on Pillow, this writes
the PNG by hand: a PNG is just a signature plus IHDR/IDAT/IEND chunks, and
the pixel data is zlib-deflated scanlines each prefixed with a filter byte.

Usage: ppm2png.py <in.ppm> <out.png>
"""
import struct
import sys
import zlib


def read_ppm(path):
    with open(path, "rb") as f:
        data = f.read()

    if not data.startswith(b"P6"):
        raise SystemExit(f"{path}: not a binary PPM (P6)")

    # Header fields are whitespace-separated tokens, with '#' comments
    # allowed between them.
    fields = []
    pos = 2
    while len(fields) < 3:
        while pos < len(data) and data[pos : pos + 1].isspace():
            pos += 1
        if data[pos : pos + 1] == b"#":
            while pos < len(data) and data[pos] != 0x0A:
                pos += 1
            continue
        start = pos
        while pos < len(data) and not data[pos : pos + 1].isspace():
            pos += 1
        fields.append(int(data[start:pos]))
    pos += 1  # single whitespace byte after maxval

    width, height, maxval = fields
    if maxval != 255:
        raise SystemExit(f"{path}: unsupported maxval {maxval}")

    expected = width * height * 3
    pixels = data[pos : pos + expected]
    if len(pixels) < expected:
        raise SystemExit(f"{path}: truncated pixel data")
    return width, height, pixels


def write_png(path, width, height, pixels):
    raw = bytearray()
    stride = width * 3
    for y in range(height):
        raw.append(0)  # filter type 0 (None)
        raw += pixels[y * stride : (y + 1) * stride]

    def chunk(tag, payload):
        out = struct.pack(">I", len(payload)) + tag + payload
        return out + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", ihdr)
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")

    with open(path, "wb") as f:
        f.write(png)


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: ppm2png.py <in.ppm> <out.png>")
    width, height, pixels = read_ppm(sys.argv[1])
    write_png(sys.argv[2], width, height, pixels)
    print(f"{sys.argv[2]}: {width}x{height}")


if __name__ == "__main__":
    main()
