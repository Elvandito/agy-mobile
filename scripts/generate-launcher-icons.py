#!/usr/bin/env python3
"""Generates deterministic PNG mipmap icons (API 21+) matching the confirmed AGY typographic monogram."""

import os
import struct
import zlib

# 5x7 crisp bitmap glyphs for 'A', 'G', 'Y'
GLYPHS = {
    "A": [
        "01110",
        "10001",
        "10001",
        "11111",
        "10001",
        "10001",
        "10001",
    ],
    "G": [
        "01111",
        "10000",
        "10000",
        "10111",
        "10001",
        "10001",
        "01110",
    ],
    "Y": [
        "10001",
        "10001",
        "01010",
        "00100",
        "00100",
        "00100",
        "00100",
    ],
}

BG = (0x12, 0x14, 0x17, 0xFF)
BORDER = (0x64, 0x6E, 0x7C, 0xFF)
FG = (0xED, 0xEA, 0xE2, 0xFF)
AMBER = (0xF2, 0xA9, 0x3B, 0xFF)


def write_png(path: str, size: int) -> None:
    pixels = [[BG for _ in range(size)] for _ in range(size)]
    pad = max(2, size // 16)
    border_w = max(1, size // 48)

    for y in range(pad, size - pad):
        for x in range(pad, size - pad):
            if (
                x < pad + border_w
                or x >= size - pad - border_w
                or y < pad + border_w
                or y >= size - pad - border_w
            ):
                pixels[y][x] = BORDER

    # Draw 'AGY' centered using integer pixel scaling (17 columns x 7 rows total)
    scale = max(1, (size * 62 // 100) // 17)
    text_w = 17 * scale
    text_h = 7 * scale
    start_x = (size - text_w) // 2
    start_y = (size - text_h) // 2 - max(2, size // 18)

    for idx, ch in enumerate("AGY"):
        gx0 = start_x + idx * 6 * scale
        bitmap = GLYPHS[ch]
        for row_i, row in enumerate(bitmap):
            for col_i, bit in enumerate(row):
                if bit == "1":
                    for dy in range(scale):
                        for dx in range(scale):
                            py = start_y + row_i * scale + dy
                            px = gx0 + col_i * scale + dx
                            if 0 <= py < size and 0 <= px < size:
                                pixels[py][px] = FG

    # Warm amber baseline rule
    bar_y0 = start_y + text_h + max(2, scale)
    bar_h = max(2, scale)
    for y in range(bar_y0, min(size - pad - border_w, bar_y0 + bar_h)):
        for x in range(start_x, min(size - pad - border_w, start_x + text_w)):
            pixels[y][x] = AMBER

    raw_rows = bytearray()
    for row in pixels:
        raw_rows.append(0)  # filter type 0
        for r, g, b, a in row:
            raw_rows.extend((r, g, b, a))

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(raw_rows), 9))
        + chunk(b"IEND", b"")
    )
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(png)


def main() -> None:
    res_root = os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "android",
        "app",
        "src",
        "main",
        "res",
    )
    densities = {
        "mipmap-mdpi": 48,
        "mipmap-hdpi": 72,
        "mipmap-xhdpi": 96,
        "mipmap-xxhdpi": 144,
        "mipmap-xxxhdpi": 192,
    }
    for folder, px in densities.items():
        write_png(os.path.join(res_root, folder, "ic_launcher.png"), px)


if __name__ == "__main__":
    main()
