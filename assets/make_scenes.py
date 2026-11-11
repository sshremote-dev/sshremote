"""Writes the background scenes (Lottie) in the two parts the program draws: NAME_still.json
(what never moves, drawn once) and NAME_motion.json (what moves, over clear, drawn every frame).
`night` is built into the app (assets/); the others ship in the extensions repository (packs/nature-scenes).

  meadow  a clear day: green rolling hills, round trees, drifting clouds, swaying flowers and
          grass, two butterflies
  night   a deep blue night over a mountain lake: the Milky Way, pines on both shores, and
          shooting stars falling one after another from the upper right
  aurora  green and violet northern lights waving over snowy peaks and a frozen lake
  snow    a pink and violet winter evening: snowy hills and pines, a cabin's warm window, snow

Run from this folder: python make_scenes.py
"""

import json
import math
import random

W, H, FR = 1920, 1080, 24


# ----- Lottie helpers -------------------------------------------------------------------------

def static(v):
    return {"a": 0, "k": v}


def keys(points, ease=None, hold=False):
    out = []
    for i, (t, v) in enumerate(points):
        k = {"t": t, "s": v if isinstance(v, list) else [v]}
        if hold:
            k["h"] = 1
        elif ease and i + 1 < len(points):
            k["o"], k["i"] = {"x": [ease[0]], "y": [ease[1]]}, {"x": [ease[2]], "y": [ease[3]]}
        out.append(k)
    return {"a": 1, "k": out}


def wave(loop, period, amp, phase=0.0, steps=8, centre=0.0):
    """A value swinging smoothly around `centre` (sampled sine), the same at 0 and `loop`."""
    pts = []
    step = period / steps
    t = 0.0
    while t <= loop + 0.01:
        pts.append((round(t, 2), centre + amp * math.sin(2 * math.pi * (t / period + phase))))
        t += step
    return pts


def transform(p=(0, 0), s=None, o=None, r=0):
    return {"o": o or static(100), "r": r if isinstance(r, dict) else static(r),
            "p": p if isinstance(p, dict) else static([p[0], p[1], 0]),
            "a": static([0, 0, 0]), "s": s or static([100, 100, 100])}


def group(items, o=None, p=(0, 0), r=0, s=(100, 100)):
    tr = {"ty": "tr", "p": p if isinstance(p, dict) else static(list(p)), "a": static([0, 0]),
          "s": s if isinstance(s, dict) else static(list(s)), "r": r if isinstance(r, dict) else static(r),
          "o": o or static(100), "sk": static(0), "sa": static(0)}
    return {"ty": "gr", "it": items + [tr]}


def layer(name, shapes, loop, ks=None, ip=0, op=None):
    return {"ddd": 0, "ty": 4, "nm": name, "sr": 1, "ks": ks or transform(), "ao": 0, "shapes": shapes,
            "ip": ip, "op": loop if op is None else op, "st": 0, "bm": 0}


def ellipse(w, h=None, at=(0, 0)):
    return {"ty": "el", "d": 1, "s": static([w, h if h is not None else w]), "p": static(list(at))}


def rect(w, h, at, r=0):
    return {"ty": "rc", "d": 1, "s": static([w, h]), "p": static(list(at)), "r": static(r)}


def fill(rgb, opacity=100):
    return {"ty": "fl", "c": static([*rgb, 1]), "o": static(opacity), "r": 1}


def stroke(rgb, width, opacity=100):
    return {"ty": "st", "c": static([*rgb, 1]), "o": static(opacity), "w": static(width), "lc": 2, "lj": 2}


def gradient(kind, start, end, stops, alphas=None):
    k = [x for off, rgb in stops for x in (off, *rgb)]
    if alphas:
        k += [x for off, a in alphas for x in (off, a)]
    return {"ty": "gf", "o": static(100), "r": 1, "t": kind, "s": static(list(start)), "e": static(list(end)),
            "h": static(0), "a": static(0), "g": {"p": len(stops), "k": static(k)}}


def shape_of(points, closed=True, smooth=0.0):
    """A path's shape; `smooth` gives each point level tangents a share of the gap to the next,
    for rolling curves."""
    n = len(points)
    ins, outs = [], []
    for i in range(n):
        if smooth and 0 < i < n - 1:
            dx = (points[i + 1][0] - points[i - 1][0]) * smooth / 2
            dy = (points[i + 1][1] - points[i - 1][1]) * smooth / 2
            ins.append([-dx, -dy])
            outs.append([dx, dy])
        else:
            ins.append([0, 0])
            outs.append([0, 0])
    return {"c": closed, "v": [list(p) for p in points], "i": ins, "o": outs}


def path(points, closed=True, smooth=0.0):
    return {"ty": "sh", "ks": static(shape_of(points, closed, smooth))}


def hexc(h):
    return tuple(int(h[i:i + 2], 16) / 255 for i in (1, 3, 5))


def glow(at, radius, rgb, alpha, stretch=(100, 100)):
    """A soft round light: a circle with a radial gradient, stretched by its group."""
    return group([ellipse(radius * 2), gradient(2, (0, 0), (radius, 0), [(0, rgb), (1, rgb)],
                                                 [(0, alpha), (0.5, alpha * 0.35), (1, 0)])],
                 p=at, s=stretch)


def sky(top_to_bottom, until):
    """The sky as a vertical gradient: [(offset, '#rrggbb')]."""
    return group([rect(W, H, (W / 2, H / 2)),
                  gradient(1, (W / 2, 0), (W / 2, until), [(o, hexc(c)) for o, c in top_to_bottom])])


def ridge(base, rough, seed, step, peaks=0.0, width=230):
    r = random.Random(seed)
    tops = [r.uniform(0, W) for _ in range(4)]
    pts, h = [], 0.0
    for x in range(-60, W + 120, step):
        h = h * 0.7 + r.uniform(-rough, rough)
        peak = sum(peaks * math.exp(-((x - t) / width) ** 2) for t in tops)
        pts.append((x, base + h - peak))
    return pts


def hills(base, amp, seed, count=5):
    """Rolling hills: a smooth line of rounded tops."""
    r = random.Random(seed)
    xs = [-200 + i * (W + 400) / (count * 2) for i in range(count * 2 + 1)]
    pts = [(x, base - (amp * r.uniform(0.6, 1.0) if i % 2 else amp * r.uniform(0.0, 0.25))) for i, x in enumerate(xs)]
    return pts


def stars(rnd, count, top, bottom, band=None, tints=None):
    tints = tints or [(1, 1, 1), (0.86, 0.92, 1), (0.74, 0.84, 1), (1, 0.94, 0.84)]
    out = []
    for i in range(count):
        if band and i < count // 2:
            t = rnd.uniform(-0.1, 1.1)
            cx, cy = band(t)
            off = rnd.gauss(0, 85)
            x, y = cx + off * 0.55, cy + off
        else:
            x, y = rnd.uniform(0, W), rnd.uniform(top, bottom)
        if not (0 <= x <= W and top <= y <= bottom):
            continue
        q = rnd.random()
        if q < 0.72:
            d, o = 1.4, rnd.uniform(40, 72)
        elif q < 0.96:
            d, o = rnd.uniform(1.9, 2.5), rnd.uniform(70, 95)
        else:
            d, o = rnd.uniform(2.8, 3.4), 100
            out.append(group([ellipse(d * 4.5, at=(x, y)), fill((0.75, 0.84, 1), 10)]))
        out.append(group([ellipse(d, at=(x, y)), fill(rnd.choice(tints), o)]))
    return out


def twinkles(rnd, count, loop, bottom):
    tw = []
    for _ in range(count):
        x, y = rnd.uniform(0, W), rnd.uniform(0, bottom)
        lo, hi = rnd.uniform(25, 45), rnd.uniform(80, 100)
        a, b = sorted(rnd.sample(range(20, loop - 20), 2))
        tw.append(group([ellipse(rnd.choice([2.0, 2.4, 2.8]), at=(x, y)), fill((0.93, 0.96, 1))],
                        o=keys([(0, lo), (a, hi), (b, lo), (loop, lo)])))
    return tw


def pines(x0, x1, ground, seed, tall, colour, snow=None):
    r = random.Random(seed)
    out, caps = [], []
    x = x0
    while x < x1:
        hgt = r.uniform(*tall)
        wid = hgt * r.uniform(0.26, 0.34)
        g = ground(x)
        for k in range(4):
            top, base = g - hgt * (1 - k * 0.22), g - hgt * (0.5 - k * 0.15)
            half = wid * (0.5 - k * 0.1)
            out.append(path([(x, top), (x + half, base + 3), (x - half, base + 3)]))
            if snow:
                caps.append(path([(x, top), (x + half * 0.45, top + (base - top) * 0.42), (x - half * 0.45, top + (base - top) * 0.42)]))
        out.append(path([(x - 1.5, g - hgt * 0.15), (x + 1.5, g - hgt * 0.15), (x + 1.5, g + 4), (x - 1.5, g + 4)]))
        x += r.uniform(10, 26)
    items = [group(out + [fill(colour)])]
    if snow:
        items.append(group(caps + [fill(snow)]))
    return items


def mirror(points, about):
    return [(x, 2 * about - y) for x, y in points]


# ----- Meadow: a clear day ----------------------------------------------------------------------

def meadow():
    loop = 288                       # 12 s
    rnd = random.Random(5)
    moving, staying = [], []
    ground = H * 0.62

    # Butterflies: wandering over the flowers, wings beating.
    for i, (cx, cy, rgb) in enumerate([(W * 0.32, H * 0.70, hexc("#ffb347")), (W * 0.66, H * 0.66, hexc("#ff7eb6"))]):
        route = [(t, [cx + 140 * math.sin(2 * math.pi * t / loop + i) + 60 * math.sin(6 * math.pi * t / loop),
                      cy + 40 * math.sin(4 * math.pi * t / loop + i), 0]) for t in range(0, loop + 1, 12)]
        flap = keys([(t, [100 * (0.25 + 0.75 * abs(math.cos(math.pi * t / 6))), 100]) for t in range(0, loop + 1, 2)])
        wing = [group([ellipse(16, 22, at=(-9, -5)), ellipse(12, 14, at=(-7, 9)), fill(rgb)], s=flap),
                group([ellipse(16, 22, at=(9, -5)), ellipse(12, 14, at=(7, 9)), fill(rgb)], s=flap),
                group([ellipse(4, 18), fill(hexc("#3b2a1a"))])]
        moving.append(layer(f"butterfly {i}", wing, loop, transform(keys(route))))

    # Flowers and grass in front, swaying in the wind, each a little differently.
    colours = [hexc(c) for c in ("#ff8fb1", "#ffd84d", "#ffffff", "#b48cff", "#ff6b6b", "#7fd1ff")]
    for i in range(46):
        x = rnd.uniform(-20, W + 20)
        base = H - rnd.uniform(10, 150)
        h = rnd.uniform(40, 95) * (0.6 + (base - H * 0.8) / (H * 0.4))
        c = rnd.choice(colours)
        petal = rnd.uniform(9, 14)
        petals = [ellipse(petal, petal * 1.4, at=(math.cos(a) * petal * 0.7, -h + math.sin(a) * petal * 0.7))
                  for a in [k * 2 * math.pi / 5 for k in range(5)]]
        sway = keys([(t, [v]) for t, v in wave(loop, rnd.choice([72, 96, 144]), rnd.uniform(4, 9), rnd.random())])
        items = [group([path([(0, 0), (2, -h * 0.5), (0, -h)], closed=False, smooth=0.5), stroke(hexc("#2e8b3e"), 3)]),
                 group([ellipse(14, 6, at=(6, -h * 0.45)), fill(hexc("#3fa34d"))], r=-30),
                 group(petals + [fill(c)]),
                 group([ellipse(petal * 0.7, at=(0, -h)), fill(hexc("#ffcf3f") if c != hexc("#ffd84d") else hexc("#e8892b"))])]
        moving.append(layer(f"flower {i}", items, loop, transform((x, base), r=sway)))
    blades = []
    for i in range(70):
        x, base = rnd.uniform(-20, W + 20), H - rnd.uniform(0, 60)
        h = rnd.uniform(26, 60)
        blades.append(group([path([(-3, 0), (rnd.uniform(-8, 8), -h), (3, 0)]), fill(hexc(rnd.choice(["#3c9a45", "#4cae50", "#2f8a3c"])))],
                            p=(x, base), r=keys([(t, [v]) for t, v in wave(loop, rnd.choice([48, 72, 96]), 6, rnd.random())])))
    moving.append(layer("grass", blades, loop))

    # Clouds drifting slowly to the right; each in two copies a width apart, so the loop is seamless.
    for i in range(4):
        y = rnd.uniform(H * 0.08, H * 0.32)
        scale = rnd.uniform(0.8, 1.4)
        puffs = [ellipse(w * scale, h * scale, at=(dx * scale, dy * scale)) for w, h, dx, dy in
                 [(260, 110, 0, 0), (180, 120, -110, 10), (200, 140, 90, -20), (160, 90, 190, 20), (150, 80, -200, 30)]]
        shade = [ellipse(520 * scale, 60 * scale, at=(0, 45 * scale))]
        cloud = [group(puffs + [fill((1, 1, 1), 96)]), group(shade + [fill(hexc("#dbe9f7"), 70)])]
        x0 = rnd.uniform(0, W)
        span = W + 700
        for k in (0, 1):
            start = x0 - k * span
            drift = keys([(0, [start, y, 0]), (loop * 3, [start + span, y, 0])])
            moving.append(layer(f"cloud {i}.{k}", cloud, loop, transform(drift)))

    # The land: hills from the hazy far ones to the deep green near ones, round trees on them.
    far = hills(ground - 10, 120, 11)
    mid = hills(ground + 80, 110, 12)
    near = hills(H * 0.86, 90, 13)
    close = lambda pts: pts + [(W + 200, H + 10), (-200, H + 10)]
    staying.append(layer("near hill", [group([path(close(near), smooth=0.38),
                                              gradient(1, (0, H * 0.75), (0, H), [(0, hexc("#5cc157")), (1, hexc("#2f8f3a"))])])], loop))
    trees = []
    for i in range(14):
        x = rnd.uniform(60, W - 60)
        y = ground + 70 + rnd.uniform(-20, 40)
        s = rnd.uniform(0.7, 1.2)
        trees.append(group([rect(10 * s, 40 * s, (x, y)), fill(hexc("#6b4a2b"))]))
        trees.append(group([ellipse(70 * s, 80 * s, at=(x, y - 50 * s)), ellipse(54 * s, 60 * s, at=(x - 22 * s, y - 34 * s)),
                            ellipse(54 * s, 60 * s, at=(x + 22 * s, y - 34 * s)), fill(hexc(rnd.choice(["#2f8f3a", "#3a9e45", "#2a7f36"])))]))
        trees.append(group([ellipse(30 * s, 26 * s, at=(x - 12 * s, y - 64 * s)), fill(hexc("#7fd17a"), 55)]))
    staying.append(layer("trees", trees, loop))
    staying.append(layer("middle hill", [group([path(close(mid), smooth=0.38),
                                                gradient(1, (0, ground), (0, H), [(0, hexc("#8fd46f")), (1, hexc("#56b04c"))])])], loop))
    staying.append(layer("far hill", [group([path(close(far), smooth=0.38),
                                             gradient(1, (0, ground - 140), (0, ground + 120), [(0, hexc("#b8e3a0")), (1, hexc("#8ccf7a"))])])], loop))
    staying.append(layer("far mountains", [group([path(ridge(ground - 70, 18, 4, 60, peaks=90, width=200) + [(W + 120, H), (-60, H)]),
                                                  fill(hexc("#a9c9e8"), 80)])], loop))
    # The sun's warmth in the upper right, and the clear sky.
    staying.append(layer("sun", [glow((W * 0.82, H * 0.14), 500, hexc("#fff1c1"), 0.85, stretch=(100, 100)),
                                 group([ellipse(110, at=(W * 0.82, H * 0.14)), fill(hexc("#fffbe8"))])], loop))
    staying.append(layer("sky", [sky([(0, "#3f8fdb"), (0.55, "#86c3ef"), (1, "#dff1fb")], ground)], loop))
    return loop, moving, staying


# ----- Night over a mountain lake -------------------------------------------------------------

def night():
    loop = 864                       # 36 s
    shore = H * 0.80
    rnd = random.Random(20261111)
    moving, staying = [], []

    # Shooting stars: all from the upper right towards the lower left, one after another.
    angle = 148
    start, n = 30, 0
    while start < loop - 40:
        big = n in (4, 10)
        life = rnd.randint(18, 26) if not big else 30
        x, y = rnd.uniform(W * 0.30, W * 1.02), rnd.uniform(-20, H * 0.38)
        travel = rnd.uniform(380, 640) * (1.3 if big else 1)
        tail = travel * rnd.uniform(0.42, 0.55)
        size = rnd.uniform(0.75, 1.05) if not big else 1.7
        a = math.radians(angle)
        dx, dy = math.cos(a) * travel, math.sin(a) * travel
        move = keys([(start, [x, y, 0]), (start + life, [x + dx, y + dy, 0])], ease=(0.3, 0.0, 0.7, 1.0))
        fade = keys([(start, 0), (start + life * 0.2, 100), (start + life * 0.62, 100), (start + life, 0)])
        w = 2.3 * size
        streak = group([path([(0, -w), (-tail, -0.4), (-tail, 0.4), (0, w)]),
                        gradient(1, (0, 0), (-tail, 0), [(0, (1, 1, 1)), (0.3, (0.82, 0.9, 1)), (1, (0.55, 0.7, 1))],
                                 [(0, 1), (0.35, 0.5), (1, 0)])])
        items = [group([ellipse(w * 2.3), fill((1, 1, 1))]), streak]
        if big:
            items.insert(0, glow((0, 0), w * 8, (0.75, 0.86, 1), 0.5))
        moving.append(layer(f"meteor {n}", items, loop, transform(move, o=fade, r=angle), ip=start, op=start + life))
        start += rnd.randint(43, 82)
        n += 1

    for i in range(10):
        y = rnd.uniform(shore + 30, H - 20)
        x = rnd.uniform(W * 0.32, W * 0.68)
        length = rnd.uniform(40, 140) * (0.6 + (y - shore) / (H - shore))
        drift = keys([(0, [x, y, 0]), (loop / 2, [x + rnd.choice([-1, 1]) * 24, y, 0]), (loop, [x, y, 0])])
        shimmer = keys([(0, 5), (loop * 0.25 + i * 20, 45), (loop * 0.5 + i * 20, 5), (loop * 0.75, 40), (loop, 5)])
        moving.append(layer(f"glint {i}", [group([ellipse(length, 1.6), fill((0.75, 0.84, 1))])], loop, transform(drift, o=shimmer)))
    moving.append(layer("twinkling stars", twinkles(rnd, 40, loop, shore * 0.8), loop))

    far = ridge(shore - 60, 30, 3, 50, peaks=70)
    mid = ridge(shore - 22, 20, 5, 40, peaks=30)
    far_shape = far + [(W + 120, shore), (-60, shore)]
    mid_shape = mid + [(W + 120, shore), (-60, shore)]
    left = [(-60, shore - 30), (120, shore - 34), (300, shore - 18), (430, shore + 2), (520, shore + 30), (560, H + 10), (-60, H + 10)]
    right = [(W + 60, shore - 40), (W - 160, shore - 36), (W - 330, shore - 14), (W - 470, shore + 8),
             (W - 560, shore + 40), (W - 600, H + 10), (W + 60, H + 10)]

    def on(points, x):
        for (ax, ay), (bx, by) in zip(points, points[1:]):
            if min(ax, bx) <= x <= max(ax, bx) and ax != bx:
                return ay + (by - ay) * (x - ax) / (bx - ax)
        return points[0][1]

    dark = hexc("#02040b")
    staying.append(layer("shores and pines",
                         pines(-20, 470, lambda x: on(left, x), 21, (70, 170), dark) +
                         pines(W - 500, W + 20, lambda x: on(right, x), 22, (70, 180), dark) +
                         [group([path(left), fill(dark)]), group([path(right), fill(dark)])], loop))
    refl = [group([ellipse(rnd.uniform(1.4, 2.2), at=(rnd.uniform(500, W - 520), rnd.uniform(shore + 6, H))),
                   fill((0.8, 0.86, 1), rnd.uniform(15, 35))]) for _ in range(70)]
    staying.append(layer("water's stars", refl, loop))
    staying.append(layer("shore light", [glow((W / 2, shore + 2), 500, hexc("#5c7fd0"), 0.35, stretch=(220, 6))], loop))
    staying.append(layer("mirrored ridges", [group([path(mirror(mid_shape, shore)), fill(hexc("#0b1534"), 70)]),
                                             group([path(mirror(far_shape, shore)), fill(hexc("#18285a"), 45)])], loop))
    staying.append(layer("water", [group([rect(W, H - shore, (W / 2, (shore + H) / 2)),
                                          gradient(1, (W / 2, shore), (W / 2, H),
                                                   [(0, hexc("#173064")), (0.35, hexc("#0b1a40")), (1, hexc("#030816"))])])], loop))
    staying.append(layer("near ridge", [group([path(mid_shape), gradient(1, (0, shore - 90), (0, shore), [(0, hexc("#0e1a3f")), (1, hexc("#08112b"))])])], loop))
    staying.append(layer("haze", [glow((W / 2, shore - 30), 500, hexc("#3a5aa8"), 0.40, stretch=(240, 30))], loop))
    staying.append(layer("far ridge", [group([path(far_shape), gradient(1, (0, shore - 200), (0, shore), [(0, hexc("#1d3166")), (1, hexc("#132350"))])])], loop))
    staying.append(layer("stars", stars(rnd, 620, 0, shore - 30, band=lambda t: (t * W, shore * 1.02 - t * shore * 0.95)), loop))
    tilt = -math.degrees(math.atan2(shore * 0.95, W))
    staying.append(layer("milky way", [glow((0, 0), 500, (0.80, 0.78, 0.95), 0.10, stretch=(170, 14)),
                                       glow((0, 0), 500, (0.55, 0.62, 0.95), 0.12, stretch=(300, 32)),
                                       glow((0, 0), 500, (0.40, 0.45, 0.85), 0.08, stretch=(320, 60))],
                         loop, transform((W * 0.5, shore * 0.55), r=tilt)))
    staying.append(layer("sky", [sky([(0, "#010210"), (0.45, "#04102e"), (0.8, "#0b1f52"), (1, "#1a3470")], shore)], loop))
    return loop, moving, staying


# ----- Aurora over snowy peaks ------------------------------------------------------------------

def aurora():
    loop = 288
    rnd = random.Random(31)
    moving, staying = [], []
    shore = H * 0.80

    # Ribbons of light: a wavy curtain, bright at its foot and fading upwards, its shape waving.
    def curtain(base, height, amp, k, phase):
        foot = [(x, base + amp * math.sin(k * x + phase) + 20 * math.sin(2.7 * k * x - phase)) for x in range(-100, W + 160, 80)]
        top = [(x, y - height * (0.7 + 0.3 * math.sin(1.7 * k * x + phase * 1.3))) for x, y in foot]
        return foot + list(reversed(top))

    for i, (base, height, amp, colour, top_colour) in enumerate([
            (H * 0.42, 300, 60, hexc("#4dffb0"), hexc("#7a5cff")),
            (H * 0.30, 240, 80, hexc("#5cf2d6"), hexc("#b06cff")),
            (H * 0.52, 200, 40, hexc("#3cff8a"), hexc("#4aa0ff"))]):
        k = rnd.uniform(0.0025, 0.004)
        shapes = [{"t": t, "s": [shape_of(curtain(base, height, amp, k, 2 * math.pi * t / loop + i), True, 0.8)]}
                  for t in range(0, loop + 1, 24)]
        breathe = keys([(t, [v]) for t, v in wave(loop, loop, 22, i / 3, centre=62)])
        moving.append(layer(f"aurora {i}", [group([{"ty": "sh", "ks": {"a": 1, "k": shapes}},
                                                   gradient(1, (0, base + amp), (0, base - height * 1.1), [(0, colour), (0.55, colour), (1, top_colour)],
                                                            [(0, 0.0), (0.12, 0.85), (0.5, 0.45), (1, 0)])])],
                            loop, transform(o=breathe)))
    moving.append(layer("twinkling stars", twinkles(rnd, 30, loop, shore * 0.7), loop))

    # Snowy ranges: the far one white and blue in the aurora's light, the near one darker with
    # snow on its tops; both mirrored in the frozen lake.
    far = ridge(shore - 120, 34, 41, 45, peaks=170, width=150) + [(W + 120, shore), (-60, shore)]
    near = ridge(shore - 40, 26, 43, 40, peaks=80, width=120) + [(W + 120, shore), (-60, shore)]
    staying.append(layer("near range", [group([path(near), gradient(1, (0, shore - 160), (0, shore),
                                                                     [(0, hexc("#9fb4d8")), (0.35, hexc("#3d5584")), (1, hexc("#14213f"))])])], loop))
    staying.append(layer("far range", [group([path(far), gradient(1, (0, shore - 330), (0, shore),
                                                                   [(0, hexc("#ffffff")), (0.4, hexc("#b9cbe8")), (1, hexc("#4a6596"))])])], loop))
    staying.append(layer("mirrored ranges", [group([path(mirror(near, shore)), fill(hexc("#16284a"), 75)]),
                                             group([path(mirror(far, shore)), fill(hexc("#5f7bb0"), 35)])], loop))
    staying.append(layer("lake glow", [glow((W / 2, shore + 60), 500, hexc("#3cff9a"), 0.20, stretch=(220, 30))], loop))
    staying.append(layer("lake", [group([rect(W, H - shore, (W / 2, (shore + H) / 2)),
                                         gradient(1, (0, shore), (0, H), [(0, hexc("#1d3a5c")), (1, hexc("#07111f"))])])], loop))
    staying.append(layer("stars", stars(rnd, 420, 0, shore - 200), loop))
    staying.append(layer("sky", [sky([(0, "#01040d"), (0.5, "#041633"), (1, "#0b2c47")], shore)], loop))
    return loop, moving, staying


# ----- A snowy evening --------------------------------------------------------------------------

def snow():
    loop = 288
    rnd = random.Random(47)
    moving, staying = [], []

    # Snow in three depths: each a sheet of flakes falling one screen in its time, in two copies
    # one above the other, so the fall never ends.
    for depth, (count, size, period, drift) in enumerate([(60, (2, 3.5), 288, 30), (45, (3.5, 5.5), 144, 40), (24, (6, 9), 96, 60)]):
        flakes = [group([ellipse(rnd.uniform(*size), at=(rnd.uniform(0, W), rnd.uniform(0, H))), fill((1, 1, 1), rnd.uniform(70, 95))])
                  for _ in range(count)]
        for k in (0, 1):
            y0 = -H + k * H
            steps = [(t, [drift * math.sin(2 * math.pi * t / loop + depth), y0 + H * ((t % period) / period), 0]) for t in range(0, loop + 1, 12)]
            # Back to the top at the end of each fall, at once.
            pts = []
            for t, v in steps:
                pts.append((t, v))
            pos = []
            for j, (t, v) in enumerate(pts):
                if j and t % period == 0 and t != loop:
                    pos.append({"t": t - 0.01, "s": [v[0], y0 + H, 0], "h": 1})
                pos.append({"t": t, "s": v})
            moving.append(layer(f"snow {depth}.{k}", [group(flakes)], loop, transform({"a": 1, "k": pos})))

    # The cabin's window glowing, gently.
    flicker = keys([(t, [v]) for t, v in wave(loop, 48, 12, 0.3, centre=80)])
    moving.append(layer("window light", [glow((W * 0.70, H * 0.80), 160, hexc("#ffb347"), 0.55)], loop, transform(o=flicker)))

    ground = H * 0.72
    near = hills(H * 0.90, 60, 21)
    mid = hills(ground + 40, 80, 22)
    close = lambda pts: pts + [(W + 200, H + 10), (-200, H + 10)]
    staying.append(layer("near snow", [group([path(close(near), smooth=0.38),
                                              gradient(1, (0, H * 0.82), (0, H), [(0, hexc("#f3f2ff")), (1, hexc("#c9c4ec"))])])], loop))
    # The cabin: walls, a snowy roof, a lit window and a chimney.
    cx, cy = W * 0.70, H * 0.80
    cabin = [group([rect(220, 130, (cx, cy)), fill(hexc("#5a3b2e"))]),
             group([path([(cx - 140, cy - 60), (cx, cy - 160), (cx + 140, cy - 60)]), fill(hexc("#f5f3ff"))]),
             group([rect(60, 50, (cx, cy + 5), r=4), fill(hexc("#ffcf6b"))]),
             group([rect(26, 60, (cx + 70, cy - 130)), fill(hexc("#4a3127"))]),
             group([rect(34, 12, (cx + 70, cy - 162), r=6), fill(hexc("#f5f3ff"))])]
    staying.append(layer("cabin", cabin, loop))
    trees = pines(80, 560, lambda x: ground + 70, 7, (90, 190), hexc("#23324f"), snow=hexc("#eef0ff")) + \
        pines(W - 420, W - 60, lambda x: ground + 60, 8, (90, 200), hexc("#23324f"), snow=hexc("#eef0ff"))
    staying.append(layer("pines", trees, loop))
    staying.append(layer("middle snow", [group([path(close(mid), smooth=0.38),
                                                gradient(1, (0, ground - 40), (0, H), [(0, hexc("#dcd8f7")), (1, hexc("#b5afe0"))])])], loop))
    staying.append(layer("far hills", [group([path(ridge(ground - 30, 14, 9, 60, peaks=60, width=260) + [(W + 120, H), (-60, H)]),
                                              fill(hexc("#9b92cf"), 85)])], loop))
    staying.append(layer("first stars", stars(random.Random(3), 90, 0, H * 0.35), loop))
    staying.append(layer("sky", [sky([(0, "#2a2a63"), (0.45, "#5c4f98"), (0.8, "#c08bb5"), (1, "#f2b8a8")], ground)], loop))
    return loop, moving, staying


# ----- Writing them -----------------------------------------------------------------------------

def write(name, loop, chosen, still):
    doc = {"v": "5.7.4", "fr": FR, "ip": 0, "op": 1 if still else loop, "w": W, "h": H, "nm": name, "ddd": 0,
           "assets": [], "layers": [dict(l, op=1) if still else l for l in chosen]}
    with open(f"{name}.json", "w", encoding="utf-8") as f:
        json.dump(doc, f, separators=(",", ":"))
    return len(chosen)


if __name__ == "__main__":
    for scene in (meadow, night, aurora, snow):
        name = scene.__name__
        loop, moving, staying = scene()
        out = "" if name == "night" else "../../sshremote-extensions/packs/nature-scenes/"
        write(f"{out}{name}_still", loop, staying, True)
        write(f"{out}{name}_motion", loop, moving, False)
        print(f"{name}: {len(moving) + len(staying)} layers, {loop / FR:.0f} s")
