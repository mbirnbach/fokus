"""The Fokus app icon: two laser beams converging to one focal point, on a graphite squircle.

    python3 icon.py out/icon.json   then   vectorcraft-cli run --commands out/icon.json ...
(build.sh does both.)
"""

import math
import sys

from vc import CYAN, Doc, circle, linear, poly, radial, squircle, stop, beam_edge

S = 1024
C = S / 2
A = 412  # half the 824 px body of the macOS icon grid

doc = Doc()
doc.cmd("file.new", name="Fokus icon", width=S, height=S, units="Pixels")

# Soft shadow under the body, as on the macOS icon grid.
shadow = doc.path(squircle(C, C + 12, A - 4), fill="#000000", opacity=45)
doc.cmd("effect.apply", id=shadow, effect="blur.gaussian", params={"radius": 14})

art = []

# Faint measuring grid: the vertical and horizontal axis and two rings around the focus.
for d in (f"M{C} {C - A} L{C} {C + A}", f"M{C - A} {C} L{C + A} {C}"):
    art.append(doc.path(d, stroke="#FFFFFF", width=1.5, opacity=6))
art.append(doc.path(circle(C, C, 168), stroke=CYAN, width=1.5, opacity=16))
art.append(doc.path(circle(C, C, 330), stroke="#FFFFFF", width=1.2, opacity=5))

# Two beams on the diagonals, each a glowing sheet bounded by a few fine edge lines.
sheet = radial(C, C, 640, [stop(0, CYAN, 0.30), stop(0.2, CYAN, 0.08), stop(0.7, CYAN, 0.025), stop(1, CYAN, 0.0)])
edge = radial(C, C, 620, [stop(0, "#F0FDFF", 1.0), stop(0.25, "#5FE4FF", 0.9), stop(1, "#5FE4FF", 0.45)])
# (waist half-width, flare at the corners, line width, curve power)
strands = [(2.5, 190, 2.0, 1.8), (3.5, 240, 1.0, 1.65), (2.0, 150, 0.8, 1.95)]
for angle in (math.pi / 4, -math.pi / 4):
    w0, flare, _, p = strands[1]
    upper = beam_edge(C, C, angle, w0, flare, +1, p)
    lower = beam_edge(C, C, angle, w0, flare, -1, p)
    art.append(doc.path(poly(upper + lower[::-1], closed=True), fill=sheet, blend="Screen"))
    for w0, flare, width, p in strands:
        for side in (+1, -1):
            art.append(doc.path(poly(beam_edge(C, C, angle, w0, flare, side, p)), stroke=edge, width=width, blend="Screen"))

# The focal point: a wide halo, a bright bloom and a white core.
art.append(doc.path(circle(C, C, 320), fill=radial(C, C, 320, [stop(0, CYAN, 0.85), stop(0.12, CYAN, 0.30), stop(0.4, CYAN, 0.07), stop(1, CYAN, 0)]), blend="Screen"))
art.append(doc.path(circle(C, C, 64), fill=radial(C, C, 64, [stop(0, "#FFFFFF", 1), stop(0.25, "#B4F4FF", 0.95), stop(0.55, CYAN, 0.45), stop(1, CYAN, 0)]), blend="Screen"))
art.append(doc.path(circle(C, C, 9), fill="#FFFFFF"))

# The body clips everything; its own fill sits behind the art and its stroke is the rim light.
body = doc.cmd("path.create", d=squircle(C, C, A))
doc.cmd("object.clippingMask.make", ids=art + [body])
doc.paint(
    body,
    radial(C, C - 60, 560, [stop(0, "#1C2633"), stop(0.55, "#0E131A"), stop(1, "#06080B")]),
    linear(C, C - A, C, C + A, [stop(0, "#FFFFFF", 0.22), stop(0.5, "#FFFFFF", 0.05), stop(1, "#FFFFFF", 0.10)]),
    width=3,
)

doc.save(sys.argv[1])
