#!/usr/bin/env python3
"""SSH Remote icon. Python 3 standard library only; run: python make_icon.py [dark | svg].
`dark` draws the variant for dark themes (a dark plate, brighter blues) as icon-dark.*.
Independent optical masters, SDF geometry, 4x/8x area supersampling.
Outputs go to the current working directory, not the script directory.
"""
import math
import sys
import struct
import zlib
from pathlib import Path

SIZES = (16, 24, 32, 48, 64, 256)
PLATE_TOP = (0xF8, 0xFA, 0xFD)
PLATE_BOTTOM = (0xE5, 0xEB, 0xF5)
LIGHT = (0x4F, 0x8F, 0xF7)
DEEP = (0x36, 0x59, 0xD9)
RIM = 0.9


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


def portal(x, y):
    """Distance to the centre line of the portal: a rounded frame open on the left."""
    d = min(capsule(x, y, 104, 48, 184, 48, 0), capsule(x, y, 208, 72, 208, 184, 0),
            capsule(x, y, 104, 208, 184, 208, 0))
    for cx, cy, sx, sy in ((184, 72, 1, -1), (184, 184, 1, 1)):
        if (x - cx) * sx >= 0 and (y - cy) * sy >= 0:
            d = min(d, abs(math.hypot(x - cx, y - cy) - 24))
    return d


def chevron(x, y):
    return min(capsule(x, y, 48, 88, 88, 128, 0), capsule(x, y, 88, 128, 48, 168, 0))


def sample(x, y, size):
    """A command chevron entering an open portal (one SSH connection into the whole machine),
    on a light rounded plate: the chevron light blue in front, the portal deeper behind."""
    u, v = (x - 128) / 112, (y - 128) / 112
    if abs(u) ** 5 + abs(v) ** 5 > 1:
        return (0, 0, 0, 0)
    t = min(max((y - 16) / 224, 0), 1)
    plate = tuple(round(a + (b - a) * t) for a, b in zip(PLATE_TOP, PLATE_BOTTOM))
    # A hairline rim, about one device pixel, so the plate holds its shape on any background.
    if abs(u) ** 5 + abs(v) ** 5 > (1 - 1.1 * 256 / size / 112) ** 5:
        plate = tuple(round(c * RIM) for c in plate)
    # The mark at 0.78 of the plate, with a stroke set per size so small icons stay crisp.
    k = {16: 0.86, 24: 0.84}.get(size, 0.78)
    mx, my = 128 + (x - 128) / k, 128 + (y - 128) / k
    device = {16: 2.25, 24: 3.0, 32: 3.5, 48: 5.0}.get(size)
    w = 28 if device is None else device * 256 / size / k
    if chevron(mx, my) <= w / 2:
        return (*LIGHT, 255)
    if portal(mx, my) <= w / 2:
        return (*DEEP, 255)
    return (*plate, 255)


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
                                  (py * ss + sy + .5) * scale, size)
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


def squircle_path():
    """The plate outline as an SVG path: the same |u|^5 + |v|^5 = 1 curve the bitmaps use."""
    points = []
    for i in range(96):
        a = 2 * math.pi * i / 96
        c, s = math.cos(a), math.sin(a)
        points.append((128 + 112 * math.copysign(abs(c) ** 0.4, c), 128 + 112 * math.copysign(abs(s) ** 0.4, s)))
    return 'M' + 'L'.join(f'{x:.1f} {y:.1f}' for x, y in points) + 'Z'


def svg():
    """One SVG for every theme: light colours, and dark ones when the page or system is dark."""
    css = ('.t{stop-color:#f8fafd}.b{stop-color:#e5ebf5}.c{stroke:#4f8ff7}.p{stroke:#3659d9}.r{stroke:#000;stroke-opacity:.1}'
           '@media (prefers-color-scheme:dark){.t{stop-color:#2a313d}.b{stop-color:#171b23}.c{stroke:#7cb4ff}.p{stroke:#4f7cf5}.r{stroke:#fff;stroke-opacity:.12}}')
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256"><style>' + css + '</style>'
            '<defs><linearGradient id="lg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" class="t"/><stop offset="1" class="b"/></linearGradient></defs>'
            f'<path d="{squircle_path()}" fill="url(#lg)" class="r" stroke-width="3"/>' + MARK + '</svg>\n')


MARK = ('<g transform="translate(128 128) scale(.78) translate(-128 -128)" fill="none" stroke-width="28" stroke-linecap="round" stroke-linejoin="round">'
        '<path class="p" d="M104 48H184A24 24 0 0 1 208 72V184A24 24 0 0 1 184 208H104"/><path class="c" d="M48 88L88 128L48 168"/></g>')


def main():
    if 'svg' in sys.argv[1:]:
        Path('logo.svg').write_text(svg(), encoding='utf-8')
        print('Wrote logo.svg; site/favicon.svg is a copy, site/site.js takes the plate path')
        return
    global PLATE_TOP, PLATE_BOTTOM, LIGHT, DEEP, RIM
    suffix = ''
    if 'dark' in sys.argv[1:]:
        # The same mark for dark themes: a dark plate, and blues bright enough on it.
        PLATE_TOP, PLATE_BOTTOM = (0x2A, 0x31, 0x3D), (0x17, 0x1B, 0x23)
        LIGHT, DEEP = (0x7C, 0xB4, 0xFF), (0x4F, 0x7C, 0xF5)
        RIM = 1.35
        suffix = '-dark'
    masters = {size: render(size) for size in SIZES}
    images = [(size, png(size, masters[size])) for size in SIZES]
    Path(f'icon{suffix}.png').write_bytes(images[-1][1])
    offset = 6 + 16 * len(images)
    entries = bytearray()
    for size, data in images:
        entries.extend(struct.pack('<BBBBHHII', size % 256, size % 256, 0, 0, 1, 32, len(data), offset))
        offset += len(data)
    Path(f'icon{suffix}.ico').write_bytes(struct.pack('<HHH', 0, 1, len(images)) + entries + b''.join(data for _, data in images))
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
