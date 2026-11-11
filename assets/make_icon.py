#!/usr/bin/env python3
"""SSH Remote logo. Python 3 standard library only; run in an output folder:
python make_icon.py        icon.png (256), icon.ico (16..256), preview.png
python make_icon.py svg    logo.svg (the mark in currentColor) and favicon.svg (mark on the plate)

The mark is drawn like a toolbar icon: a 24-unit grid, round 2-unit lines. A prompt chevron
enters an open frame: one SSH connection into the whole machine. The app icon puts it in white
on a flat blue squircle, the same on light and dark task bars.
"""
import math
import struct
import sys
import zlib
from pathlib import Path

SIZES = (16, 24, 32, 48, 64, 256)
PLATE = (0x25, 0x63, 0xEB)
MARK = (0xFF, 0xFF, 0xFF)
CHEVRON = 'M5 8.5 8.5 12 5 15.5'
FRAME = 'M12 5h4a3 3 0 0 1 3 3v8a3 3 0 0 1-3 3h-4'


def capsule(x, y, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    h = max(0, min(1, ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(x - ax - h * dx, y - ay - h * dy)


def mark(x, y):
    """Distance to the centre lines of the mark, in grid units."""
    d = min(capsule(x, y, 5, 8.5, 8.5, 12), capsule(x, y, 8.5, 12, 5, 15.5),
            capsule(x, y, 12, 5, 16, 5), capsule(x, y, 19, 8, 19, 16), capsule(x, y, 12, 19, 16, 19))
    for cx, cy, sy in ((16, 8, -1), (16, 16, 1)):
        if x >= cx and (y - cy) * sy >= 0:
            d = min(d, abs(math.hypot(x - cx, y - cy) - 3))
    return d


def sample(x, y, size):
    u, v = (x - 128) / 112, (y - 128) / 112
    if abs(u) ** 5 + abs(v) ** 5 > 1:
        return (0, 0, 0, 0)
    # The mark spans 14 of 24 units; small icons give it more of the plate and a heavier line.
    unit = {16: 0.80, 24: 0.74}.get(size, 0.58) * 224 / 14
    gx, gy = 12 + (x - 128) / unit, 12 + (y - 128) / unit
    device = {16: 1.6, 24: 2.2, 32: 2.6, 48: 3.6}.get(size)
    width = 2.0 if device is None else device * 256 / size / unit
    return (*MARK, 255) if mark(gx, gy) <= width / 2 else (*PLATE, 255)


def render(size):
    ss = 8 if size <= 48 else 4
    scale = 256 / (size * ss)
    out = bytearray()
    for py in range(size):
        for px in range(size):
            acc = [0, 0, 0, 0]
            for sy in range(ss):
                for sx in range(ss):
                    r, g, b, a = sample((px * ss + sx + .5) * scale, (py * ss + sy + .5) * scale, size)
                    acc[0] += r * a
                    acc[1] += g * a
                    acc[2] += b * a
                    acc[3] += a
            out.extend((*(round(acc[c] / acc[3]) if acc[3] else 0 for c in range(3)), round(acc[3] / (ss * ss))))
    return bytes(out)


def png(w, h, pixels):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff)
    raw = b''.join(b'\0' + pixels[y * w * 4:(y + 1) * w * 4] for y in range(h))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b''))


def squircle_path():
    pts = []
    for i in range(96):
        a = 2 * math.pi * i / 96
        c, s = math.cos(a), math.sin(a)
        pts.append((128 + 112 * math.copysign(abs(c) ** 0.4, c), 128 + 112 * math.copysign(abs(s) ** 0.4, s)))
    return 'M' + 'L'.join(f'{x:.1f} {y:.1f}' for x, y in pts) + 'Z'


def svgs():
    lines = f'<path d="{CHEVRON}"/><path d="{FRAME}"/>'
    logo = ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" '
            f'stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{lines}</svg>\n')
    unit = 0.58 * 224 / 14
    favicon = ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">'
               f'<path d="{squircle_path()}" fill="#2563eb"/>'
               f'<g transform="translate(128 128) scale({unit:.4f}) translate(-12 -12)" fill="none" stroke="#fff" '
               f'stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{lines}</g></svg>\n')
    Path('logo.svg').write_text(logo, encoding='utf-8')
    Path('favicon.svg').write_text(favicon, encoding='utf-8')
    print('Wrote logo.svg and favicon.svg')


def main():
    if 'svg' in sys.argv[1:]:
        return svgs()
    masters = {s: render(s) for s in SIZES}
    images = [(s, png(s, s, masters[s])) for s in SIZES]
    Path('icon.png').write_bytes(images[-1][1])
    offset = 6 + 16 * len(images)
    entries = bytearray()
    for s, data in images:
        entries.extend(struct.pack('<BBBBHHII', s % 256, s % 256, 0, 0, 1, 32, len(data), offset))
        offset += len(data)
    Path('icon.ico').write_bytes(struct.pack('<HHH', 0, 1, len(images)) + entries + b''.join(d for _, d in images))
    # Review sheet: each small size at its pixels and enlarged 4x, on a light and a dark band.
    w, h = 760, 440
    sheet = bytearray()
    for y in range(h):
        sheet.extend((0xF4, 0xF4, 0xF5, 255) * w if y < h // 2 else (0x18, 0x18, 0x1B, 255) * w)
    for band in (0, h // 2):
        x0 = 10
        for s in (16, 24, 32, 48):
            for f, oy in ((1, 8), (3, 60)):
                for y in range(s * f):
                    for x in range(s * f):
                        src = ((y // f) * s + x // f) * 4
                        dst = ((band + oy + y) * w + x0 + x) * 4
                        a = masters[s][src + 3] / 255
                        for c in range(3):
                            sheet[dst + c] = round(masters[s][src + c] * a + sheet[dst + c] * (1 - a))
            x0 += s * 3 + 14
    Path('preview.png').write_bytes(png(w, h, bytes(sheet)))
    print('Wrote icon.png, icon.ico (16..256) and preview.png')


if __name__ == '__main__':
    main()
