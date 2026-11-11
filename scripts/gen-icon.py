#!/usr/bin/env python3
"""Generates assets/icon.png (1024x1024 rounded square with CD letters). Stdlib only."""
import struct, zlib, sys
N = 1024
R = 200
BG = (37, 99, 235)
FG = (255, 255, 255)
# 5x7 bitmap glyphs
G = {
 "C": ["01110","10001","10000","10000","10000","10001","01110"],
 "D": ["11110","10001","10001","10001","10001","10001","11110"],
}
S = 36  # pixel scale
def glyph_mask():
    cells = set()
    total_w = 5 * 2 + 1
    x0 = (N - total_w * S) // 2
    y0 = (N - 7 * S) // 2
    for gi, ch in enumerate("CD"):
        for r, row in enumerate(G[ch]):
            for c, v in enumerate(row):
                if v == "1":
                    cells.add((x0 + (gi * 6 + c) * S, y0 + r * S))
    return cells
cells = glyph_mask()
cellrows = {}
for (x, y) in cells:
    cellrows.setdefault(y, []).append(x)
def inside(x, y):
    cx = min(max(x, R), N - 1 - R)
    cy = min(max(y, R), N - 1 - R)
    return (x - cx) ** 2 + (y - cy) ** 2 <= R * R
raw = bytearray()
for y in range(N):
    raw.append(0)
    gx = []
    for ry, xs in cellrows.items():
        if ry <= y < ry + S:
            gx = xs
            break
    for x in range(N):
        if not inside(x, y):
            raw += bytes((0, 0, 0, 0)); continue
        lit = any(cx <= x < cx + S for cx in gx)
        raw += bytes(FG + (255,) if lit else BG + (255,))
def chunk(t, d):
    c = struct.pack(">I", len(d)) + t + d
    return c + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")
open(sys.argv[1] if len(sys.argv) > 1 else "assets/icon.png", "wb").write(png)
