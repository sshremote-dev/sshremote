"""Writes night_sky.json: the built-in background animation (Lottie). A deep night: a blue
starry sky with a faint Milky Way, dark ridges and pines below, and shooting stars falling now
and then from different places, as a 12 s loop.

Run from this folder: python make_night.py
"""

import json
import math
import random

W, H, FR = 1920, 1080, 24
LOOP = 288  # 12 s
GROUND = 0.84  # the land's highest point, as a share of the height

rnd = random.Random(20261110)


def static(v):
    return {"a": 0, "k": v}


def keys(points, ease=None):
    """Keyframes from (frame, value) pairs, each with its own start value."""
    out = []
    for i, (t, v) in enumerate(points):
        k = {"t": t, "s": v if isinstance(v, list) else [v]}
        if ease and i + 1 < len(points):
            k["o"], k["i"] = {"x": [ease[0]], "y": [ease[1]]}, {"x": [ease[2]], "y": [ease[3]]}
        out.append(k)
    return {"a": 1, "k": out}


def transform(p=(0, 0), s=None, o=None, r=0):
    return {"o": o or static(100), "r": static(r), "p": p if isinstance(p, dict) else static([p[0], p[1], 0]),
            "a": static([0, 0, 0]), "s": s or static([100, 100, 100])}


def group(items, o=None, p=(0, 0), r=0, s=(100, 100)):
    tr = {"ty": "tr", "p": static(list(p)), "a": static([0, 0]), "s": static(list(s)), "r": static(r),
          "o": o or static(100), "sk": static(0), "sa": static(0)}
    return {"ty": "gr", "it": items + [tr]}


def layer(name, shapes, ks=None, ip=0, op=LOOP):
    return {"ddd": 0, "ty": 4, "nm": name, "sr": 1, "ks": ks or transform(), "ao": 0, "shapes": shapes,
            "ip": ip, "op": op, "st": 0, "bm": 0}


def ellipse(w, h=None, at=(0, 0)):
    return {"ty": "el", "d": 1, "s": static([w, h if h is not None else w]), "p": static(list(at))}


def fill(rgb, opacity=100):
    return {"ty": "fl", "c": static([*rgb, 1]), "o": static(opacity), "r": 1}


def gradient(kind, start, end, stops, alphas=None):
    """A gradient fill: colour stops (offset, rgb), then opacity stops (offset, alpha) if any."""
    k = [x for off, rgb in stops for x in (off, *rgb)]
    if alphas:
        k += [x for off, a in alphas for x in (off, a)]
    return {"ty": "gf", "o": static(100), "r": 1, "t": kind, "s": static(list(start)), "e": static(list(end)),
            "h": static(0), "a": static(0), "g": {"p": len(stops), "k": static(k)}}


def path(points, closed=True):
    n = len(points)
    return {"ty": "sh", "ks": static({"c": closed, "v": [list(p) for p in points], "i": [[0, 0]] * n, "o": [[0, 0]] * n})}


def hex_rgb(h):
    return tuple(int(h[i:i + 2], 16) / 255 for i in (1, 3, 5))


layers = []

# Shooting stars, on top: a bright head and a tail that thins and fades behind it.
METEORS = [
    # start frame, length in frames, from (x, y), angle (degrees below the horizontal), travel, tail, size
    (10, 18, (1280, 90), 32, 520, 260, 1.0),
    (52, 22, (420, 140), 148, 600, 300, 0.9),
    (93, 16, (1580, 260), 38, 420, 210, 0.8),
    (128, 26, (760, 60), 28, 760, 380, 1.6),   # the rare large one, with a glow
    (171, 18, (260, 300), 150, 460, 220, 0.85),
    (204, 20, (1120, 180), 36, 560, 260, 1.0),
    (236, 16, (1700, 120), 142, 480, 230, 0.9),
    (262, 18, (560, 220), 34, 500, 240, 0.8),
]
for n, (start, life, (x, y), angle, travel, tail, size) in enumerate(METEORS):
    a = math.radians(angle)
    dx, dy = math.cos(a) * travel, math.sin(a) * travel
    move = keys([(start, [x, y, 0]), (start + life, [x + dx, y + dy, 0])], ease=(0.3, 0.0, 0.7, 1.0))
    fade = keys([(start, 0), (start + life * 0.2, 100), (start + life * 0.65, 100), (start + life, 0)])
    w = 2.4 * size
    # In the layer's own frame the head is at the origin and the tail runs back along -x.
    streak = group([
        path([(0, -w), (-tail * size, -0.4), (-tail * size, 0.4), (0, w)]),
        gradient(1, (0, 0), (-tail * size, 0), [(0, (1, 1, 1)), (0.25, (0.80, 0.88, 1)), (1, (0.55, 0.68, 1))],
                 [(0, 1), (0.35, 0.55), (1, 0)]),
    ])
    head = group([ellipse(w * 2.2), fill((1, 1, 1))])
    items = [head, streak]
    if size > 1.2:
        items.insert(0, group([ellipse(w * 14), gradient(2, (0, 0), (w * 7, 0), [(0, (0.75, 0.85, 1)), (1, (0.75, 0.85, 1))], [(0, 0.45), (1, 0)])]))
    layers.append(layer(f"meteor {n}", items, transform(move, o=fade, r=angle), ip=start, op=start + life))

# The land: a far ridge, then a near one with pines, almost black with a hint of blue.
def ridge(base, rough, seed, step):
    r = random.Random(seed)
    pts, h = [], 0.0
    for x in range(-40, W + 80, step):
        h = h * 0.75 + r.uniform(-rough, rough)
        pts.append((x, base + h))
    return pts

near = ridge(H * 0.905, 18, 11, 40)
pines = []
for x0 in range(-20, W + 40, 17):
    if rnd.random() < 0.6:
        ground = max(p[1] for p in near if abs(p[0] - x0) < 30)
        hgt, wid = rnd.uniform(34, 96), rnd.uniform(14, 24)
        # A pine: three tiers narrowing upward, on a thin trunk.
        for k in range(3):
            top, base = ground - hgt * (1 - k * 0.28), ground - hgt * (0.45 - k * 0.2)
            half = wid * (0.5 - k * 0.12)
            pines.append(path([(x0, top), (x0 + half, base + 4), (x0 - half, base + 4)]))
        pines.append(path([(x0 - 1.5, ground - hgt * 0.1), (x0 + 1.5, ground - hgt * 0.1), (x0 + 1.5, ground + 4), (x0 - 1.5, ground + 4)]))
dark = hex_rgb("#02050e")
layers.append(layer("near ridge and pines", [group(pines + [fill(dark)]), group([path(near + [(W + 40, H + 10), (-40, H + 10)]), fill(dark)])]))
far = ridge(H * 0.86, 30, 7, 60)
layers.append(layer("far ridge", [group([path(far + [(W + 80, H + 10), (-40, H + 10)]),
                                         gradient(1, (0, H * 0.82), (0, H), [(0, hex_rgb("#13224d")), (1, hex_rgb("#0a1330"))])])]))

# Stars that twinkle slowly, each at its own pace.
twinkles = []
for _ in range(36):
    x, y = rnd.uniform(0, W), rnd.uniform(0, H * GROUND - 20)
    d = rnd.choice([2.0, 2.4, 2.8])
    lo, hi = rnd.uniform(25, 45), rnd.uniform(80, 100)
    a, b = sorted(rnd.sample(range(12, LOOP - 12), 2))
    twinkles.append(group([ellipse(d, at=(x, y)), fill((0.92, 0.95, 1))],
                          o=keys([(0, lo), (a, hi), (b, lo), (LOOP, lo)])))
layers.append(layer("twinkling stars", twinkles))

# Still stars: many tiny and dim, some brighter, a few bright with a soft halo; denser along the
# Milky Way.
tints = [(1, 1, 1), (0.85, 0.91, 1), (0.74, 0.83, 1), (1, 0.93, 0.82)]
still = []
def band_point():
    # Around a line across the sky, from lower left to upper right.
    t = rnd.uniform(-0.1, 1.1)
    cx, cy = t * W, H * 0.78 - t * H * 0.62
    off = rnd.gauss(0, 70)
    return cx + off * 0.55, cy + off
for i in range(420):
    x, y = band_point() if i < 170 else (rnd.uniform(0, W), rnd.uniform(0, H * GROUND))
    if not (0 <= x <= W and 0 <= y <= H * GROUND - 10):
        continue
    r = rnd.random()
    if r < 0.70:
        d, o = 1.5, rnd.uniform(45, 75)
    elif r < 0.95:
        d, o = rnd.uniform(1.9, 2.5), rnd.uniform(70, 95)
    else:
        d, o = rnd.uniform(2.8, 3.4), 100
        still.append(group([ellipse(d * 4, at=(x, y)), fill((0.75, 0.84, 1), 10)]))
    still.append(group([ellipse(d, at=(x, y)), fill(rnd.choice(tints), o)]))
layers.append(layer("stars", still))

# The Milky Way's glow: a long faint band.
band = group([ellipse(1000), gradient(2, (0, 0), (500, 0), [(0, (0.50, 0.57, 0.90)), (1, (0.50, 0.57, 0.90))], [(0, 0.13), (0.5, 0.06), (1, 0)])], s=(280, 40))
layers.append(layer("milky way", [band], transform((W * 0.5, H * 0.47), r=-33)))

# The horizon's glow, and the sky from near black above to deep blue below.
glow = group([ellipse(1000), gradient(2, (0, 0), (500, 0), [(0, hex_rgb("#3550a8")), (1, hex_rgb("#3550a8"))], [(0, 0.50), (0.55, 0.18), (1, 0)])], s=(260, 60))
layers.append(layer("horizon glow", [glow], transform((W * 0.5, H * 0.92))))
sky = group([{"ty": "rc", "d": 1, "s": static([W, H]), "p": static([W / 2, H / 2]), "r": static(0)},
             gradient(1, (W / 2, 0), (W / 2, H),
                      [(0, hex_rgb("#01030b")), (0.45, hex_rgb("#040d2b")), (0.75, hex_rgb("#0a1a48")), (1, hex_rgb("#14275e"))])])
layers.append(layer("sky", [sky]))

def write(name, chosen, still):
    doc = {"v": "5.7.4", "fr": FR, "ip": 0, "op": 1 if still else LOOP, "w": W, "h": H, "nm": name, "ddd": 0,
           "assets": [], "layers": chosen}
    with open(f"{name}.json", "w", encoding="utf-8") as f:
        json.dump(doc, f, separators=(",", ":"))
    print(f"{name}: {len(chosen)} layers")


# The whole scene, and for the program the same in two parts: what moves (shooting stars and
# twinkling stars, over clear), drawn every frame, and what stays (sky, land, stars), drawn once.
moving = [l for l in layers if l["nm"].startswith("meteor") or l["nm"] == "twinkling stars"]
staying = [dict(l, op=1) for l in layers if l not in moving]
write("night_sky", layers, False)
write("night_motion", moving, False)
write("night_still", staying, True)
