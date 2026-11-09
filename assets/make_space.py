"""Writes space_waves.json: the built-in background animation (Lottie), a deep blue sky with
twinkling stars and waves spreading from a few places like radio signals, as a 9 s loop.

Run from this folder: python make_space.py
"""

import json
import math
import random

W, H, FR = 1920, 1080, 24
PERIOD = 72          # a wave from each place every 3 s
LIFE = 120           # each wave lives 5 s
LOOP = PERIOD * 3    # 9 s, so every wave repeats exactly

rnd = random.Random(20261109)


def static(v):
    return {"a": 0, "k": v}


def keys(points, ease=True):
    """Keyframes from (frame, value) pairs; eased out (fast, then slow) when asked."""
    out = []
    for i, (t, v) in enumerate(points):
        k = {"t": t, "s": v if isinstance(v, list) else [v]}
        if ease and i + 1 < len(points):
            k["o"] = {"x": [0.15], "y": [0.6]}
            k["i"] = {"x": [0.35], "y": [1.0]}
        out.append(k)
    return {"a": 1, "k": out}


def transform(p=(0, 0), s=None, o=None):
    return {
        "o": o if o is not None else static(100),
        "r": static(0),
        "p": static([p[0], p[1], 0]),
        "a": static([0, 0, 0]),
        "s": s if s is not None else static([100, 100, 100]),
    }


def group(items, o=None):
    tr = {"ty": "tr", "p": static([0, 0]), "a": static([0, 0]), "s": static([100, 100]), "r": static(0),
          "o": o if o is not None else static(100), "sk": static(0), "sa": static(0)}
    return {"ty": "gr", "it": items + [tr]}


def layer(name, shapes, ks, ip=0, op=LOOP):
    return {"ddd": 0, "ty": 4, "nm": name, "sr": 1, "ks": ks, "ao": 0, "shapes": shapes,
            "ip": ip, "op": op, "st": 0, "bm": 0}


def ellipse(d):
    return {"ty": "el", "d": 1, "s": static([d, d]), "p": static([0, 0])}


def fill(rgb, opacity=100):
    return {"ty": "fl", "c": static([*rgb, 1]), "o": static(opacity), "r": 1}


def stroke(rgb, width, opacity=100):
    return {"ty": "st", "c": static([*rgb, 1]), "o": static(opacity), "w": static(width), "lc": 2, "lj": 2}


layers = []

# Waves: from seven places, each with its own colour and its own moment in the period.
colours = [(0.31, 0.76, 0.97), (0.55, 0.47, 1.0), (0.30, 0.90, 0.80), (0.42, 0.62, 1.0)]
places = []
while len(places) < 7:
    p = (rnd.uniform(140, W - 140), rnd.uniform(110, H - 110))
    if all(math.dist(p, q) > 420 for q in places):
        places.append(p)
for n, (x, y) in enumerate(places):
    rgb = colours[n % len(colours)]
    phase = rnd.randrange(PERIOD)
    reach = rnd.uniform(240, 420)
    # Every wave alive at some moment of the loop, those begun before it included.
    start = phase - (LIFE // PERIOD + 1) * PERIOD
    while start < LOOP:
        if start + LIFE > 0:
            grow = keys([(start, [8, 8, 100]), (start + LIFE, [100, 100, 100])])
            fade = keys([(start, 90), (start + LIFE, 0)], ease=False)
            ring = [group([ellipse(reach * 2), stroke(rgb, 14, 18)]), group([ellipse(reach * 2), stroke(rgb, 3, 85)])]
            layers.append(layer(f"wave {n}", ring, transform((x, y), grow, fade), ip=max(start, 0), op=min(start + LIFE, LOOP)))
        start += PERIOD
    # The place itself: a small light that brightens as each wave leaves it.
    pulse = keys([(0, 35), (phase, 100), (phase + 24, 35), (LOOP, 35)], ease=False) if phase + 24 < LOOP else static(60)
    layers.append(layer(f"source {n}", [group([ellipse(9), fill(rgb)]), group([ellipse(26), fill(rgb, 25)])],
                        transform((x, y), o=pulse)))

# Stars: small points of light, each twinkling at its own pace.
stars = []
for i in range(90):
    x, y = rnd.uniform(0, W), rnd.uniform(0, H)
    d = rnd.choice([1.6, 2.0, 2.4, 3.0, 3.6])
    tint = rnd.choice([(1, 1, 1), (0.85, 0.92, 1), (0.75, 0.85, 1), (1, 0.95, 0.85)])
    lo, hi = rnd.uniform(15, 45), rnd.uniform(70, 100)
    a, b = sorted(rnd.sample(range(10, LOOP - 10), 2))
    twinkle = keys([(0, lo), (a, hi), (b, lo), (LOOP, lo)], ease=False)
    item = group([{"ty": "el", "d": 1, "s": static([d, d]), "p": static([x, y])}, fill(tint)], o=twinkle)
    stars.append(item)
layers.append(layer("stars", stars, transform()))

# The sky: deep blue, lighter towards the middle.
sky = group([
    {"ty": "rc", "d": 1, "s": static([W, H]), "p": static([W / 2, H / 2]), "r": static(0)},
    {"ty": "gf", "o": static(100), "r": 1, "t": 2,
     "s": static([W * 0.45, H * 0.4]), "e": static([W * 1.05, H * 0.4]),
     "h": static(0), "a": static(0),
     "g": {"p": 3, "k": static([0, 0.07, 0.15, 0.36, 0.55, 0.03, 0.07, 0.20, 1, 0.01, 0.02, 0.08])}},
])
layers.append(layer("sky", [sky], transform()))

doc = {"v": "5.7.4", "fr": FR, "ip": 0, "op": LOOP, "w": W, "h": H, "nm": "space waves", "ddd": 0,
       "assets": [], "layers": layers}
with open("space_waves.json", "w", encoding="utf-8") as f:
    json.dump(doc, f, separators=(",", ":"))
print(f"{len(layers)} layers")
