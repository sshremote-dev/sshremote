#!/usr/bin/env python3
"""SSH Remote icon. Python 3 standard library only; run: python make_icon.py.
Independent optical masters, SDF geometry, 4x/8x area supersampling.
Outputs go to the current working directory, not the script directory.
"""
import math
import struct
import zlib
from pathlib import Path

SIZES = (16, 24, 32, 48, 64, 256)
NAVY = (18, 29, 43)
TEAL = (48, 218, 188)
WHITE = (244, 250, 252)


def rounded_box(x, y, left, top, right, bottom, radius):
    qx = abs(x - (left + right) / 2) - (right - left) / 2 + radius
    qy = abs(y - (top + bottom) / 2) - (bottom - top) / 2 + radius
    return math.hypot(max(qx, 0), max(qy, 0)) + min(max(qx, qy), 0) - radius


def capsule(x, y, ax, ay, bx, by, radius):
    dx, dy = bx - ax, by - ay
    h = max(0, min(1, ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(x - ax - h * dx, y - ay - h * dy) - radius


def polygon(x, y, points):
    """Signed Euclidean distance to a closed polygon, including concave ones."""
    inside = False
    distance = float('inf')
    for i, (ax, ay) in enumerate(points):
        bx, by = points[(i + 1) % len(points)]
        distance = min(distance, capsule(x, y, ax, ay, bx, by, 0))
        if (ay > y) != (by > y) and x < (bx - ax) * (y - ay) / (by - ay) + ax:
            inside = not inside
    return -distance if inside else distance


def sample(x, y, small):
    # Transparent outer corners; all foreground elements stay inside the tile.
    if rounded_box(x, y, 8, 8, 248, 248, 53) > 0:
        return (0, 0, 0, 0)
    t = (y - 8) / 240
    color = tuple(round(c + (1 - t) * 5) for c in NAVY)
    # Optical sizing: ~1 px outline at 16 px, with a more open aperture.
    thickness = 16 if small else 12
    outer = rounded_box(x, y, 42, 53, 214, 179, 20)
    inner = rounded_box(x, y, 42 + thickness, 53 + thickness,
                        214 - thickness, 179 - thickness, 20 - thickness / 2)
    stand = min(capsule(x, y, 113, 180, 113, 202, 7),
                capsule(x, y, 83, 204, 143, 204, 7))
    if max(outer, -inner) <= 0 or stand <= 0:
        color = TEAL
    # Familiar SSH prompt, deliberately no text/font dependency.
    radius = 8 if small else 7
    chevron = min(capsule(x, y, 78, 91, 102, 112, radius),
                  capsule(x, y, 102, 112, 78, 133, radius))
    underscore = capsule(x, y, 119, 138, 139, 138, 6)
    if chevron <= 0 or (not small and underscore <= 0):
        color = WHITE
    # A real pointer silhouette crossing the screen edge: remote interaction.
    points = ((156, 124), (207, 161), (184, 166), (173, 190))
    pointer = polygon(x, y, points)
    if pointer <= (7 if small else 6):
        color = NAVY  # negative-space moat keeps cursor and bezel separate
    if pointer <= 0:
        color = WHITE
    return (*color, 255)


def render(size):
    ss = 8 if size <= 32 else 4
    scale = 256 / (size * ss)
    pixels = bytearray()
    for py in range(size):
        for px in range(size):
            sums = [0, 0, 0, 0]
            for sy in range(ss):
                for sx in range(ss):
                    rgba = sample((px * ss + sx + .5) * scale,
                                  (py * ss + sy + .5) * scale, size <= 24)
                    # Premultiplied accumulation avoids dark transparent fringes.
                    for c in range(3):
                        sums[c] += rgba[c] * rgba[3] / 255
                    sums[3] += rgba[3]
            alpha = sums[3] / (ss * ss)
            rgb = [round(sums[c] * 255 / sums[3]) if sums[3] else 0 for c in range(3)]
            pixels.extend((*rgb, round(alpha)))
    return bytes(pixels)


def png(size, pixels):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff)
    raw = b''.join(b'\x00' + pixels[y * size * 4:(y + 1) * size * 4] for y in range(size))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', size, size, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b''))


def main():
    masters = {size: render(size) for size in SIZES}
    images = [(size, png(size, masters[size])) for size in SIZES]
    Path('icon.png').write_bytes(images[-1][1])
    offset = 6 + 16 * len(images)
    entries = bytearray()
    for size, data in images:
        entries.extend(struct.pack('<BBBBHHII', size % 256, size % 256, 0, 0, 1, 32, len(data), offset))
        offset += len(data)
    Path('icon.ico').write_bytes(struct.pack('<HHH', 0, 1, len(images)) + entries + b''.join(data for _, data in images))
    # Review sheet: actual pixels and nearest-neighbor enlargements for optical QA.
    width, height = 768, 420
    sheet = bytearray((230, 234, 238, 255) * (width * height))
    for i, size in enumerate(SIZES):
        pixels = masters[size]
        factor = 4 if size <= 32 else 1
        ox, oy = (i * 140 + 12, 24) if size != 256 else (500, 150)
        for y in range(size * factor):
            for x in range(size * factor):
                if ox + x >= width or oy + y >= height:
                    continue
                src = ((y // factor) * size + x // factor) * 4
                dst = ((oy + y) * width + ox + x) * 4
                a = pixels[src + 3] / 255
                for c in range(3):
                    sheet[dst + c] = round(pixels[src + c] * a + sheet[dst + c] * (1 - a))
    # Review sheet is optional and rectangular; encode it using the same PNG format.
    def chunk(k, d):
        return struct.pack('>I', len(d)) + k + d + struct.pack('>I', zlib.crc32(k + d) & 0xffffffff)
    raw = b''.join(b'\0' + sheet[y * width * 4:(y + 1) * width * 4] for y in range(height))
    Path('preview.png').write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b''))
    print('Wrote icon.png (256x256 RGBA), icon.ico (16,24,32,48,64,256), preview.png')


if __name__ == '__main__':
    main()
