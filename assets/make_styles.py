"""Writes the built-in styles beside the scenes (see make_scenes.py), each as NAME_still.json (drawn
once) and NAME_motion.json (drawn every frame, over clear), and the brand animation:

  brand      "SSH REMOTE" large in the middle in 3D, turning slowly, light passing over it, the
             logo beside it turning like a coin, "(thinkbigx)" below (an animation only)
  radar      a radar screen as in films: rings, bearings, a sweeping beam lighting up blips
  matrix     green code falling in columns at three depths
  synthwave  a neon sun over wireframe mountains, a glowing grid floor rushing towards you
  circuit    a circuit board with light running along its traces
  warp       deep space with nebulae, the stars streaking past
  network    a dotted globe, connections drawn between places and pulses along them

The lettering is drawn from the outlines of Hack (MIT and Bitstream Vera licences) and Ubuntu
Light (Ubuntu Font Licence), as egui ships them; fontTools reads them.

Run from this folder: python make_styles.py
"""

import glob
import json
import math
import os
import random

from make_scenes import (FR, H, W, ellipse, fill, glow, gradient, group, hexc, keys, layer, path, rect,
                         static, stroke, transform, twinkles, wave, stars, write)


def eo(paint):
    """The same paint with the even-odd rule, so letters keep their holes."""
    return dict(paint, r=2)

LOOP = 288


# ----- Radar ------------------------------------------------------------------------------------

def radar():
    rnd = random.Random(61)
    moving, staying = [], []
    cx, cy, R = W * 0.5, H * 0.5, H * 0.43
    green = hexc("#38ff8c")
    # The beam: slices of a fan, brightest at its leading edge, turning once every 4 s.
    slices = []
    for k in range(18):
        a0, a1 = -math.radians(k * 3.2), -math.radians((k + 1) * 3.2)
        slices.append(group([path([(0, 0), (R * math.cos(a0), R * math.sin(a0)), (R * math.cos(a1), R * math.sin(a1))]),
                             fill(green, 40 * (1 - k / 18) ** 1.6)]))
    slices.insert(0, group([path([(0, 0), (R, 0)], closed=False), stroke(green, 2.5, 95)]))
    turn = keys([(0, 0), (LOOP, 1080)])
    moving.append(layer("beam", slices, LOOP, transform((cx, cy), r=turn)))
    # Blips: each lights up as the beam passes, then fades.
    for i in range(14):
        ang = rnd.uniform(0, 360)
        dist = rnd.uniform(0.15, 0.95) * R
        x, y = cx + dist * math.cos(math.radians(ang)), cy + dist * math.sin(math.radians(ang))
        pts = []
        for turn_no in range(3):
            t = (ang / 360 + turn_no) * 96
            pts += [(t, 0), (t + 1, 100), (t + 60, 0)]
        pts = [(t, v) for t, v in pts if 0 <= t <= LOOP]
        pts = [(0, 0)] + pts + [(LOOP, 0)]
        pts = sorted({round(t, 2): v for t, v in pts}.items())
        moving.append(layer(f"blip {i}", [glow((0, 0), 18, green, 0.8), group([ellipse(7), fill((0.85, 1, 0.9))])],
                            LOOP, transform((x, y), o=keys(pts))))
    # The screen: rings, cross, bearings and a faint grid, in a dark green glow.
    rings = [group([ellipse(2 * R * f, at=(cx, cy)), stroke(green, 1.6 if f < 1 else 2.5, 45 if f < 1 else 75)]) for f in (0.25, 0.5, 0.75, 1.0)]
    cross = [group([path([(cx - R, cy), (cx + R, cy)], closed=False), path([(cx, cy - R), (cx, cy + R)], closed=False), stroke(green, 1.2, 35)])]
    ticks = []
    for d in range(0, 360, 5):
        a = math.radians(d)
        long = 22 if d % 30 == 0 else 10
        ticks.append(path([(cx + R * math.cos(a), cy + R * math.sin(a)), (cx + (R + long) * math.cos(a), cy + (R + long) * math.sin(a))], closed=False))
    grid = [path([(x, 0), (x, H)], closed=False) for x in range(0, W + 1, 80)] + [path([(0, y), (W, y)], closed=False) for y in range(0, H + 1, 80)]
    # Side panels as in films: framed boxes with bars of readings.
    panels = []
    for side in (-1, 1):
        x0 = cx + side * (R + 170)
        for j in range(4):
            y0 = H * 0.2 + j * 160
            panels.append(group([rect(230, 120, (x0, y0)), stroke(green, 1.4, 45)]))
            for b in range(5):
                w = rnd.uniform(40, 190)
                panels.append(group([rect(w, 8, (x0 - 115 + 20 + w / 2, y0 - 40 + b * 20)), fill(green, 35)]))
    staying.append(layer("panels", panels, LOOP))
    staying.append(layer("bearings", [group(ticks + [stroke(green, 1.6, 60)])], LOOP))
    staying.append(layer("rings", rings + cross, LOOP))
    staying.append(layer("screen glow", [glow((cx, cy), R * 1.1, hexc("#0f5c34"), 0.55)], LOOP))
    staying.append(layer("grid", [group(grid + [stroke(green, 1, 7)])], LOOP))
    staying.append(layer("back", [group([rect(W, H, (W / 2, H / 2)), gradient(2, (cx, cy), (cx + W * 0.7, cy), [(0, hexc("#03200f")), (1, hexc("#000502"))])])], LOOP))
    return moving, staying


# ----- Code rain --------------------------------------------------------------------------------

def matrix():
    rnd = random.Random(73)
    moving, staying = [], []
    green, bright = hexc("#33ff77"), hexc("#d9ffe6")
    col_w = 24
    # A glyph: two or three small strokes in a cell, so columns read as writing.
    def glyph(x, y, s):
        parts = []
        for _ in range(2):
            if rnd.random() < 0.5:
                w, h = rnd.uniform(0.3, 0.8) * s, s * 0.14
            else:
                w, h = s * 0.14, rnd.uniform(0.3, 0.8) * s
            parts.append(rect(w, h, (x + rnd.uniform(-0.25, 0.25) * s, y + rnd.uniform(-0.35, 0.35) * s)))
        return parts
    for depth, (size, period, alpha) in enumerate([(14, 288, 35), (19, 144, 60), (26, 96, 90)]):
        # Glyphs by brightness (five steps along each column, its head the brightest), each step
        # painted once: far fewer paints than glyphs.
        steps = [[] for _ in range(5)]
        heads = []
        for c in range(0, W // col_w, 3 - (depth == 2)):
            if rnd.random() < 0.72:
                continue
            x = c * col_w + rnd.uniform(0, col_w)
            top = rnd.uniform(0, H)
            length = rnd.randint(7, 16)
            for g in range(length):
                y = (top + g * size * 1.2) % H
                if g == length - 1:
                    heads += glyph(x, y, size)
                else:
                    steps[min(4, int(5 * g / length))] += glyph(x, y, size)
        cols = [group(parts + [fill(green, alpha * (0.2 + 0.18 * k))]) for k, parts in enumerate(steps) if parts]
        cols.append(group(heads + [fill(bright, alpha)]))
        for k in (0, 1):
            y0 = -H + k * H
            pos = [{"t": 0, "s": [0, y0, 0]}]
            t = 0
            while t < LOOP:
                pos.append({"t": t + period - 0.01, "s": [0, y0 + H, 0], "h": 1})
                pos.append({"t": t + period, "s": [0, y0, 0]})
                t += period
            pos = [p for p in pos if p["t"] <= LOOP]
            moving.append(layer(f"code {depth}.{k}", [group(cols)], LOOP, transform({"a": 1, "k": pos})))
    staying.append(layer("glow", [glow((W / 2, H * 0.55), 700, hexc("#0a3d1c"), 0.6, stretch=(160, 100))], LOOP))
    staying.append(layer("back", [group([rect(W, H, (W / 2, H / 2)), fill(hexc("#000a03"))])], LOOP))
    return moving, staying


# ----- Synthwave ----------------------------------------------------------------------------------

def synthwave():
    rnd = random.Random(83)
    moving, staying = [], []
    horizon = H * 0.58
    pink, cyan = hexc("#ff3fd8"), hexc("#3ff8ff")
    # The floor's cross lines rushing towards you: each falls from the horizon to the bottom in
    # perspective, one after another.
    period = 48
    for i in range(8):
        pts = []
        for t in range(0, LOOP + 1, 3):
            z = 1 - ((t / period + i / 8) % 1)          # 1 far .. 0 near
            y = horizon + (H - horizon) * (1 - z) ** 2.2
            pts.append((t, [0, y - horizon, 0]))
        o = [(t, [100 * min(1, (v[1]) / 40)]) for t, v in pts]
        moving.append(layer(f"floor line {i}", [group([path([(-200, horizon), (W + 200, horizon)], closed=False), stroke(pink, 3, 85)])],
                            LOOP, transform(keys(pts), o=keys(o))))
    moving.append(layer("twinkling stars", twinkles(rnd, 30, LOOP, horizon * 0.7), LOOP))
    # The floor's lines to the vanishing point, the wireframe mountains, the striped sun.
    rays = [path([(W / 2, horizon), (W / 2 + k * 160 * 6, H + 40)], closed=False) for k in range(-12, 13)]
    staying.append(layer("floor rays", [group(rays + [stroke(pink, 2.5, 70)])], LOOP))
    staying.append(layer("floor", [group([rect(W, H - horizon, (W / 2, (horizon + H) / 2)),
                                          gradient(1, (0, horizon), (0, H), [(0, hexc("#2a0a45")), (1, hexc("#0b0118"))])])], LOOP))
    hills = []
    for side in (-1, 1):
        pts = [(W / 2 + side * 260, horizon)]
        x = 260
        while x < W / 2 + 200:
            x += rnd.uniform(70, 140)
            pts.append((W / 2 + side * x, horizon - rnd.uniform(40, 230)))
        pts.append((W / 2 + side * (W / 2 + 200), horizon))
        hills.append(pts)
    mtn = []
    for pts in hills:
        mtn.append(group([path(pts), fill(hexc("#12062a"))]))
        mtn.append(group([path(pts, closed=False), stroke(cyan, 2.5, 90)]))
        for (x, y) in pts[1:-1]:
            mtn.append(group([path([(x, y), (x + rnd.uniform(-40, 40), horizon)], closed=False), stroke(cyan, 1.2, 45)]))
    staying.append(layer("mountains", mtn, LOOP))
    sun_r, sun_y = 230, horizon - 200
    sun = [group([ellipse(sun_r * 2, at=(W / 2, sun_y)),
                  gradient(1, (0, sun_y - sun_r), (0, sun_y + sun_r), [(0, hexc("#ffe34d")), (0.5, hexc("#ff8a3d")), (1, hexc("#ff2e9a"))])])]
    sky_at = lambda y: tuple(a + (b - a) * min(1, max(0, (y - horizon * 0.6) / (horizon * 0.4))) for a, b in zip(hexc("#3a0d5c"), hexc("#b0307a")))
    for j in range(7):
        y = sun_y + 40 + j * 27
        half = math.sqrt(max(0, sun_r ** 2 - (y - sun_y) ** 2))
        if half > 0:
            sun.insert(0, group([rect(half * 2 + 2, 3 + j * 2.2, (W / 2, y)), fill(sky_at(y))]))
    staying.append(layer("sun", sun, LOOP))
    staying.append(layer("sun glow", [glow((W / 2, sun_y), 420, hexc("#ff4fb0"), 0.55)], LOOP))
    staying.append(layer("stars", stars(rnd, 160, 0, horizon - 80), LOOP))
    staying.append(layer("sky", [group([rect(W, H, (W / 2, H / 2)), gradient(1, (0, 0), (0, horizon),
                                                                          [(0, hexc("#0a0220")), (0.6, hexc("#3a0d5c")), (1, hexc("#b0307a"))])])], LOOP))
    return moving, staying


# ----- Circuit board ------------------------------------------------------------------------------

def circuit():
    rnd = random.Random(97)
    moving, staying = [], []
    teal, bright = hexc("#25e0c8"), hexc("#b8fff4")
    traces, routes = [], []
    for i in range(46):
        x, y = rnd.choice([rnd.uniform(0, W), rnd.choice([-20, W + 20])]), rnd.uniform(40, H - 40)
        pts = [(x, y)]
        for _ in range(rnd.randint(3, 6)):
            if rnd.random() < 0.6:
                x += rnd.choice([-1, 1]) * rnd.uniform(80, 360)
            else:
                d = rnd.uniform(40, 160)
                x += rnd.choice([-1, 1]) * d
                y += rnd.choice([-1, 1]) * d
            y = min(max(y, 30), H - 30)
            pts.append((x, y))
        routes.append(pts)
        traces.append(path(pts, closed=False))
    pads = [group([ellipse(14, at=p), stroke(teal, 2, 70)]) for pts in routes for p in (pts[0], pts[-1])]
    chips = []
    for _ in range(9):
        x, y, w, h = rnd.uniform(150, W - 150), rnd.uniform(120, H - 120), rnd.uniform(90, 200), rnd.uniform(70, 140)
        chips.append(group([rect(w, h, (x, y), r=6), fill(hexc("#06231f")), stroke(teal, 2, 60)]))
        for k in range(int(w // 18)):
            for side in (-1, 1):
                chips.append(group([rect(4, 14, (x - w / 2 + 12 + k * 18, y + side * (h / 2 + 7))), fill(teal, 55)]))
    # Pulses: a bright dot with a short glow running along a trace, one after another.
    for i, pts in enumerate(routes[:24]):
        lengths = [math.dist(a, b) for a, b in zip(pts, pts[1:])]
        total = sum(lengths)
        dur = max(24, int(total / 14))
        start = rnd.randrange(0, LOOP - dur) if dur < LOOP else 0
        pos, t = [(start, [*pts[0], 0])], start
        for p, l in zip(pts[1:], lengths):
            t += dur * l / total
            pos.append((round(t, 2), [*p, 0]))
        fade = keys([(start, 0), (start + 3, 100), (start + dur - 3, 100), (start + dur, 0)])
        moving.append(layer(f"pulse {i}", [glow((0, 0), 26, teal, 0.7), group([ellipse(7), fill(bright)])], LOOP,
                            transform(keys(pos), o=fade), ip=start, op=start + dur))
    staying.append(layer("chips", chips, LOOP))
    staying.append(layer("pads", pads, LOOP))
    staying.append(layer("traces", [group(traces + [stroke(teal, 2.2, 40)])], LOOP))
    staying.append(layer("glow", [glow((W * 0.5, H * 0.5), 800, hexc("#0b3b40"), 0.5, stretch=(150, 90))], LOOP))
    staying.append(layer("back", [group([rect(W, H, (W / 2, H / 2)), fill(hexc("#020d10"))])], LOOP))
    return moving, staying


# ----- Warp -------------------------------------------------------------------------------------

def warp():
    rnd = random.Random(101)
    moving, staying = [], []
    cx, cy = W / 2, H / 2
    for i in range(46):
        a = rnd.uniform(0, 2 * math.pi)
        period = rnd.choice([48, 72, 96])
        phase = rnd.randrange(period)
        pos, sc = [], []
        t = phase - period
        while t < LOOP:
            inner = [cx + 30 * math.cos(a), cy + 30 * math.sin(a), 0]
            outer = [cx + W * 0.8 * math.cos(a), cy + W * 0.8 * math.sin(a), 0]
            pos += [{"t": t, "s": inner, "o": {"x": [0.75], "y": [0]}, "i": {"x": [1], "y": [1]}},
                    {"t": t + period - 0.01, "s": outer, "h": 1}]
            sc += [{"t": t, "s": [10, 100], "o": {"x": [0.75], "y": [0]}, "i": {"x": [1], "y": [1]}},
                   {"t": t + period - 0.01, "s": [260, 140], "h": 1}]
            t += period
        colour = rnd.choice([(1, 1, 1), (0.75, 0.85, 1), (0.85, 0.8, 1)])
        streak = group([rect(40, 2.6, (-20, 0)), gradient(1, (0, 0), (-40, 0), [(0, colour), (1, colour)], [(0, 1), (1, 0)])],
                       s={"a": 1, "k": sc})
        moving.append(layer(f"star {i}", [streak], LOOP, transform({"a": 1, "k": pos}, r=math.degrees(a))))
    staying.append(layer("stars", stars(rnd, 300, 0, H), LOOP))
    staying.append(layer("nebulae", [glow((W * 0.3, H * 0.35), 600, hexc("#5a2da8"), 0.35, stretch=(140, 80)),
                                     glow((W * 0.72, H * 0.62), 600, hexc("#1f5fbf"), 0.35, stretch=(120, 90)),
                                     glow((cx, cy), 300, hexc("#9fb6ff"), 0.25)], LOOP))
    staying.append(layer("back", [group([rect(W, H, (W / 2, H / 2)), gradient(2, (cx, cy), (cx + W * 0.7, cy), [(0, hexc("#0b0b2a")), (1, hexc("#010108"))])])], LOOP))
    return moving, staying


# ----- Network ------------------------------------------------------------------------------------

def network():
    rnd = random.Random(113)
    moving, staying = [], []
    cx, cy, R = W * 0.5, H * 0.52, H * 0.40
    blue, bright = hexc("#4fb6ff"), hexc("#dff3ff")
    # Places on the globe's front side.
    places = []
    while len(places) < 16:
        lat, lon = rnd.uniform(-60, 65), rnd.uniform(-75, 75)
        x = cx + R * math.cos(math.radians(lat)) * math.sin(math.radians(lon))
        y = cy - R * math.sin(math.radians(lat))
        places.append((x, y))
    # Connections: arcs drawn out from one place to another, then fading, one after another.
    for i in range(14):
        a, b = rnd.sample(places, 2)
        mid = ((a[0] + b[0]) / 2, (a[1] + b[1]) / 2 - math.dist(a, b) * 0.35)
        arc = {"ty": "sh", "ks": static({"c": False, "v": [list(a), list(b)],
                                         "i": [[0, 0], [(mid[0] - b[0]) * 0.66, (mid[1] - b[1]) * 0.66]],
                                         "o": [[(mid[0] - a[0]) * 0.66, (mid[1] - a[1]) * 0.66], [0, 0]]})}
        start = (i * 20 + rnd.randrange(10)) % (LOOP - 80)
        trim = {"ty": "tm", "s": static(0), "e": keys([(start, 0), (start + 30, 100)]), "o": static(0), "m": 1}
        fade = keys([(start, 100), (start + 50, 100), (start + 80, 0)])
        moving.append(layer(f"link {i}", [group([arc, trim, stroke(blue, 2.4, 90)]), glow(b, 22, bright, 0.8)],
                            LOOP, transform(o=fade), ip=start, op=start + 80))
    for i, p in enumerate(places):
        pulse = keys([(t, [v]) for t, v in wave(LOOP, rnd.choice([48, 72, 96]), 35, rnd.random(), centre=55)])
        moving.append(layer(f"node {i}", [glow((0, 0), 16, blue, 0.9), group([ellipse(6), fill(bright)])], LOOP, transform(p, o=pulse)))
    # The globe: lines of latitude and longitude, a dotted surface, a rim of light.
    lines = []
    for lat in range(-60, 61, 30):
        rr = R * math.cos(math.radians(lat))
        lines.append(group([ellipse(2 * rr, 2 * rr * 0.12, at=(cx, cy - R * math.sin(math.radians(lat)))), stroke(blue, 1.2, 30)]))
    for lon in range(-75, 76, 25):
        lines.append(group([ellipse(2 * R * abs(math.sin(math.radians(lon))) + 1, 2 * R, at=(cx, cy)), stroke(blue, 1.2, 25)]))
    dots = []
    for _ in range(900):
        lat, lon = math.degrees(math.asin(rnd.uniform(-1, 1))), rnd.uniform(-90, 90)
        x = cx + R * math.cos(math.radians(lat)) * math.sin(math.radians(lon))
        y = cy - R * math.sin(math.radians(lat))
        dots.append(ellipse(2.4, at=(x, y)))
    staying.append(layer("globe dots", [group(dots + [fill(blue, 35)])], LOOP))
    staying.append(layer("globe lines", lines + [group([ellipse(2 * R, at=(cx, cy)), stroke(blue, 2, 55)])], LOOP))
    staying.append(layer("globe glow", [glow((cx, cy), R * 1.2, hexc("#123a7a"), 0.6)], LOOP))
    staying.append(layer("stars", stars(rnd, 200, 0, H), LOOP))
    staying.append(layer("back", [group([rect(W, H, (W / 2, H / 2)), gradient(2, (cx, cy), (cx + W * 0.7, cy), [(0, hexc("#071633")), (1, hexc("#01040c"))])])], LOOP))
    return moving, staying


# ----- The brand: SSH REMOTE in 3D ----------------------------------------------------------------

def font_path(name):
    found = glob.glob(os.path.expanduser(f"~/.cargo/registry/src/*/epaint_default_fonts-*/fonts/{name}"))
    if not found:
        raise SystemExit(f"{name} not found (it comes with egui's default fonts in the cargo registry)")
    return sorted(found)[-1]


def text_shapes(text, font_file, size, at, spacing=0.0):
    """The text's outlines as Lottie paths, its left end of the baseline at `at`; and its width."""
    from fontTools.pens.basePen import BasePen
    from fontTools.ttLib import TTFont

    font = TTFont(font_file)
    gs = font.getGlyphSet()
    cmap = font.getBestCmap()
    scale = size / font["head"].unitsPerEm

    class Pen(BasePen):
        def __init__(self, ox):
            super().__init__(gs)
            self.ox, self.contours, self.cur = ox, [], None

        def pt(self, p):
            return (at[0] + self.ox + p[0] * scale, at[1] - p[1] * scale)

        def _moveTo(self, p):
            self.cur = {"v": [self.pt(p)], "i": [[0, 0]], "o": [[0, 0]]}

        def _lineTo(self, p):
            self.cur["v"].append(self.pt(p))
            self.cur["i"].append([0, 0])
            self.cur["o"].append([0, 0])

        def _curveToOne(self, p1, p2, p3):
            a, b, c = self.pt(p1), self.pt(p2), self.pt(p3)
            last = self.cur["v"][-1]
            self.cur["o"][-1] = [a[0] - last[0], a[1] - last[1]]
            self.cur["v"].append(c)
            self.cur["i"].append([b[0] - c[0], b[1] - c[1]])
            self.cur["o"].append([0, 0])

        def _qCurveToOne(self, p1, p2):
            p0 = self._getCurrentPoint()
            c1 = (p0[0] + 2 / 3 * (p1[0] - p0[0]), p0[1] + 2 / 3 * (p1[1] - p0[1]))
            c2 = (p2[0] + 2 / 3 * (p1[0] - p2[0]), p2[1] + 2 / 3 * (p1[1] - p2[1]))
            self._curveToOne(c1, c2, p2)

        def _closePath(self):
            c = self.cur
            if len(c["v"]) > 1 and math.dist(c["v"][0], c["v"][-1]) < 0.01:
                c["i"][0] = c["i"].pop()
                c["v"].pop()
                c["o"].pop()
            self.contours.append({k: [list(x) for x in v] for k, v in c.items()})
            self.cur = None

    letters, x = [], 0.0
    for ch in text:
        name = cmap.get(ord(ch))
        if name is None:
            continue
        pen = Pen(x)
        gs[name].draw(pen)
        if pen.contours:
            letters.append([{"ty": "sh", "ks": static({"c": True, **keyhole(pen.contours)})}])
        x += gs[name].width * scale + spacing
    return letters, x


def keyhole(contours):
    """Contours made one: the largest, with each other one joined in by a bridge there and back
    to its nearest point. A renderer that fills each path alone then still leaves the holes
    (their outlines run the other way)."""
    def area(c):
        v = c["v"]
        return abs(sum(v[i][0] * v[(i + 1) % len(v)][1] - v[(i + 1) % len(v)][0] * v[i][1] for i in range(len(v)))) / 2
    contours = sorted(contours, key=area, reverse=True)
    out = {k: list(v) for k, v in contours[0].items()}
    for c in contours[1:]:
        a, b = min(((i, j) for i in range(len(out["v"])) for j in range(len(c["v"]))),
                   key=lambda ij: math.dist(out["v"][ij[0]], c["v"][ij[1]]))
        n = len(c["v"])
        order = [(b + k) % n for k in range(n)]
        va, ia, oa = out["v"][a], out["i"][a], out["o"][a]
        vs = [va] + [c["v"][k] for k in order] + [c["v"][b], va]
        ins = [ia] + [[0, 0]] + [c["i"][k] for k in order[1:]] + [c["i"][b], [0, 0]]
        outs = [[0, 0]] + [c["o"][k] for k in order] + [[0, 0], oa]
        out["v"][a:a + 1], out["i"][a:a + 1], out["o"][a:a + 1] = vs, ins, outs
    return out


def logo_shapes(cx, cy, size):
    """The logo: a chevron entering an open portal, on a rounded plate (as make_icon.py draws it)."""
    k = size / 256 * 0.78
    pt = lambda x, y: (cx + (x - 128) * k, cy + (y - 128) * k)
    c = 24 * 0.5523
    portal = {"ty": "sh", "ks": static({"c": False,
        "v": [list(pt(104, 48)), list(pt(184, 48)), list(pt(208, 72)), list(pt(208, 184)), list(pt(184, 208)), list(pt(104, 208))],
        "i": [[0, 0], [0, 0], [0, -c * k], [0, 0], [c * k, 0], [0, 0]],
        "o": [[0, 0], [c * k, 0], [0, 0], [0, c * k], [0, 0], [0, 0]]})}
    chevron = path([pt(48, 88), pt(88, 128), pt(48, 168)], closed=False)
    plate = rect(size * 0.875, size * 0.875, (cx, cy), r=size * 0.22)
    return plate, portal, chevron, 28 * k


def brand():
    moving = []
    loop = LOOP
    hack, ubuntu = font_path("Hack-Regular.ttf"), font_path("Ubuntu-Light.ttf")
    size = 150
    _, width = text_shapes("SSH REMOTE", hack, size, (0, 0), spacing=6)
    logo = 190
    total = logo + 50 + width
    left = W / 2 - total / 2
    tx, ty = left + logo + 50, H / 2 + size * 0.36
    letters, _ = text_shapes("SSH REMOTE", hack, size, (tx, ty), spacing=6)
    _, sub_w = text_shapes("(thinkbigx)", ubuntu, 64, (0, 0))
    sub_letters, _ = text_shapes("(thinkbigx)", ubuntu, 64, (tx + width - sub_w, ty + 92))
    sub = [sh for l in sub_letters for sh in l]

    # Each letter rises and falls in turn, a wave running through the title.
    def bob(k):
        return keys([(t, [0, v]) for t, v in wave(loop, 96, 12, -k / 10, steps=12)])

    def word(paint):
        return [group(l + [paint], p=bob(k)) for k, l in enumerate(letters)]

    # The turn: the depth's direction swings slowly, as if the title turned in the light.
    depth_n = 9
    def offset(i):
        return keys([(t, [math.cos(math.radians(a)) * i * 3.2, math.sin(math.radians(55)) * i * 2.8, 0])
                     for t, a in wave(loop, loop / 2, 70, 0, steps=12, centre=90)])
    deep, mid_c = hexc("#050c24"), hexc("#3a6ff0")
    layers_ = []
    # A shooting star crossing above, now and then.
    a = math.radians(160)
    move = keys([(40, [W * 0.78, H * 0.22, 0]), (64, [W * 0.78 + 900 * math.cos(a), H * 0.22 + 900 * math.sin(a), 0])], ease=(0.3, 0, 0.7, 1))
    star = [group([ellipse(5), fill((1, 1, 1))]),
            group([path([(0, -2.4), (-300, -0.4), (-300, 0.4), (0, 2.4)]),
                   gradient(1, (0, 0), (-300, 0), [(0, (1, 1, 1)), (1, (0.6, 0.75, 1))], [(0, 1), (1, 0)])])]
    layers_.append(layer("shooting star", star, loop, transform(move, o=keys([(40, 0), (44, 100), (58, 100), (64, 0)]), r=160), ip=40, op=64))
    # The light passing over the face: a bright band seen only through the letters.
    band = group([rect(140, 700, (0, 0)), gradient(1, (-70, 0), (70, 0), [(0, (1, 1, 1)), (1, (1, 1, 1))], [(0, 0), (0.5, 0.85), (1, 0)])], r=20)
    sweep = keys([(0, [tx - 400, H / 2, 0]), (60, [tx - 400, H / 2, 0]), (110, [tx + width + 300, H / 2, 0]), (loop, [tx + width + 300, H / 2, 0])], ease=(0.4, 0, 0.6, 1))
    matte = layer("face for the light", word(fill((1, 1, 1))) + [group(sub + [fill((1, 1, 1))])], loop)
    matte["td"] = 1
    lit = layer("light", [band], loop, transform(sweep))
    lit["tt"] = 1
    layers_ += [matte, lit]
    # The face: white to light blue, with a fine bright edge.
    face_paint = gradient(1, (0, ty - size * 0.75), (0, ty), [(0, (1, 1, 1)), (1, hexc("#9fc2ff"))])
    face = word(face_paint) + [group(sub + [fill(hexc("#c9d8f5"), 90)])]
    layers_.append(layer("face", face, loop))
    # The depth: copies behind the face, darker as they go back.
    for i in range(depth_n, 0, -1):
        f = i / depth_n
        colour = tuple(m + (d - m) * f for m, d in zip(mid_c, deep))
        layers_.append(layer(f"depth {i}", word(fill(colour)), loop, transform(offset(i))))
    # The logo, turning like a coin now and then, with its own depth.
    lx, ly = left + logo / 2, H / 2 - 10
    plate, portal, chevron, w = logo_shapes(0, 0, logo)
    spin = keys([(0, [100, 100]), (130, [100, 100]), (148, [6, 100]), (166, [100, 100]), (loop, [100, 100])], ease=(0.45, 0, 0.55, 1))
    coin = [group([portal, stroke(hexc("#3c6cf0"), w)]), group([chevron, stroke(hexc("#7fb2ff"), w)]),
            group([plate, gradient(1, (0, -logo / 2), (0, logo / 2), [(0, hexc("#1b2b55")), (1, hexc("#0c1633"))])])]
    edge = [group([plate, fill(colour)], p=(i * 1.6, i * 1.6)) for i, colour in [(4, deep), (3, hexc("#0e1f48")), (2, hexc("#14295c")), (1, hexc("#1b3470"))]]
    layers_.append(layer("logo", [group(coin, s=spin), group(edge, s=spin)], loop, transform((lx, ly))))
    layers_.append(layer("halo", [glow((W / 2, H / 2), 700, hexc("#3a64d8"), 0.30, stretch=(150, 60))], loop))
    moving += layers_
    return moving, []


# ----- Writing them -------------------------------------------------------------------------------

for style in (brand, radar, matrix, synthwave, circuit, warp, network):
    name = style.__name__
    moving, staying = style()
    if staying:
        write(f"{name}_still", LOOP, staying, True)
    write(f"{name}_motion", LOOP, moving, False)
    print(f"{name}: {len(moving)} moving, {len(staying)} still layers")
