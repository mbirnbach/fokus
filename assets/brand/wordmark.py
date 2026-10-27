"""The Fokus wordmark: Inter SemiBold (the app's UI face) with the "o" redrawn as a ring around
a cyan focal point.

    python3 wordmark.py out/wordmark.json light|dark [lockup]
(build.sh runs it.) The lockup puts the app icon (src/icon.vectorcraft) in front of the word.
"""

import sys

from vc import CYAN, GRAPHITE, OFF_WHITE, Doc, circle

out, theme = sys.argv[1], sys.argv[2]
lockup = len(sys.argv) > 3 and sys.argv[3] == "lockup"
ink = GRAPHITE if theme == "light" else OFF_WHITE

SIZE = 240
# The glyphs span X0+17.6 .. X0+661.1 and BASE-174.6 .. BASE+2.7.
W, H = (980, 320) if lockup else (692, 260)
X0 = 282 if lockup else 6.4
BASE = 247 if lockup else 216

doc = Doc()
doc.cmd("file.new", name=f"Fokus wordmark {theme}", width=W, height=H, units="Pixels")
text = doc.cmd("text.create", x=X0, y=BASE, text="Fokus", size=SIZE, font="Inter", style="SemiBold", color=ink)
doc.cmd("text.setStyle", id=text, tracking=-25)
doc.cmd("type.createOutlines")
# In a fresh document the outlined glyphs get ids 3 (F), 4 (o), 7, 8, 9 (k, u, s).
doc.cmd("edit.clear", ids=[4])

# Inter's "o" at this size and tracking spans x 137.8..263.1 past the origin and sits from
# 132.7 above the baseline to 2.7 below it; the ring keeps that box and the stem weight.
cx, cy = X0 + 200.4, BASE - 65.0
ring = doc.path(circle(cx, cy, 51.5), stroke=ink, width=29)
doc.cmd("object.path.outlineStroke")  # acts on the selection: the ring just drawn
doc.path(circle(cx, cy, 15.5), fill=CYAN, name="focus")

if lockup:
    doc.cmd("file.place", path="src/icon.vectorcraft", link=False, rect=[10, 30, 260, 260])

doc.save(out)
