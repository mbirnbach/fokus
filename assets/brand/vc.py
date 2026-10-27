"""Tiny helpers for writing VectorCraft command lists (vectorcraft-cli run --commands)."""

import json
import math

CYAN = "#00D7FF"
GRAPHITE = "#0B0F14"
COOL_GRAY = "#1F2937"
OFF_WHITE = "#F4F6F8"


class Doc:
    def __init__(self):
        self.steps = []

    def cmd(self, command, **params):
        self.steps.append({"command": command, "params": params})
        return f"${len(self.steps) - 1}.id"

    def path(self, d, fill="none", stroke="none", width=None, opacity=None, blend=None, name=None):
        ref = self.cmd("path.create", d=d)
        self.paint(ref, fill, stroke, width)
        props = {}
        if opacity is not None:
            props["opacity"] = opacity
        if blend:
            props["blend"] = blend
        if name:
            props["name"] = name
        if props:
            self.cmd("object.setProps", id=ref, **props)
        return ref

    def paint(self, ref, fill, stroke, width=None):
        self.cmd("paint.setFill", id=ref, **fill_params(fill))
        self.cmd("paint.setStroke", id=ref, **fill_params(stroke))
        if width is not None:
            self.cmd("stroke.set", id=ref, weight=width, cap="round", join="round")

    def save(self, path):
        with open(path, "w") as f:
            json.dump(self.steps, f, indent=1)


def fill_params(p):
    if p == "none":
        return {"none": True}
    if isinstance(p, str):
        return {"color": p}
    return {"gradient": p}


def radial(cx, cy, r, stops):
    return {"kind": "radial", "start": [cx, cy], "end": [cx + r, cy], "stops": stops}


def linear(x0, y0, x1, y1, stops):
    return {"kind": "linear", "start": [x0, y0], "end": [x1, y1], "stops": stops}


def stop(offset, color, opacity=1.0):
    return {"offset": offset, "color": color, "opacity": opacity}


def poly(points, closed=False):
    d = "M" + " L".join(f"{x:.2f} {y:.2f}" for x, y in points)
    return d + (" Z" if closed else "")


def circle(cx, cy, r):
    return (f"M{cx - r:.2f} {cy:.2f} A{r:.2f} {r:.2f} 0 1 0 {cx + r:.2f} {cy:.2f} "
            f"A{r:.2f} {r:.2f} 0 1 0 {cx - r:.2f} {cy:.2f} Z")


def squircle(cx, cy, a, n=5.0, steps=720):
    """Superellipse |x|^n + |y|^n = a^n: close to macOS's continuous-corner icon shape."""
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        c, s = math.cos(t), math.sin(t)
        x = a * math.copysign(abs(c) ** (2 / n), c)
        y = a * math.copysign(abs(s) ** (2 / n), s)
        pts.append((cx + x, cy + y))
    return poly(pts, closed=True)


def beam_edge(cx, cy, angle, w0, flare, side, p=1.7, reach=640, steps=160):
    """One edge of a beam focused at (cx, cy) along `angle`: half-width w0 + flare * |u/reach|^p,
    so the edges bow outward like light drawn to a focal point."""
    ux, uy = math.cos(angle), math.sin(angle)
    nx, ny = -uy, ux
    pts = []
    for i in range(steps + 1):
        u = reach * (2 * i / steps - 1)
        v = side * (w0 + flare * abs(u / reach) ** p)
        pts.append((cx + u * ux + v * nx, cy + u * uy + v * ny))
    return pts
