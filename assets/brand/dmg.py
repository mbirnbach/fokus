"""The DMG window background (660 x 400 pt): faint beams converging between the app icon and
the Applications folder, a laser-line arrow through the focus, the wordmark above.

    python3 dmg.py out/dmg.json out/dmg-cards.json
The cards under the icons are a document of their own, so PhotoCraft can bloom the light without
them and lay them on top after (build.sh). Finder draws the icons at (165, 200) and (495, 200);
see packaging/macos/dmg-settings.py.
"""

import math
import sys

from vc import CYAN, OFF_WHITE, Doc, beam_edge, circle, linear, poly, radial, stop

W, H = 660, 400
FX, FY = 330, 200  # the focus, between the two icons

doc = Doc()
doc.cmd("file.new", name="Fokus DMG background", width=W, height=H, units="Pixels")
doc.path(f"M0 0 L{W} 0 L{W} {H} L0 {H} Z",
         fill=radial(FX, FY - 30, 460, [stop(0, "#17202B"), stop(0.5, "#0D1218"), stop(1, "#06080B")]))

# Measuring rings and axis, barely there.
doc.path(circle(FX, FY, 92), stroke=CYAN, width=0.75, opacity=14)
doc.path(circle(FX, FY, 210), stroke="#FFFFFF", width=0.6, opacity=5)
doc.path(f"M{FX} 0 L{FX} {H}", stroke="#FFFFFF", width=0.6, opacity=4)

# Two beams through the focus, flatter than on the icon so they reach the window's corners.
edge = radial(FX, FY, 380, [stop(0, "#E6FBFF", 0.9), stop(0.25, "#5FE4FF", 0.55), stop(1, "#5FE4FF", 0.12)])
sheet = radial(FX, FY, 380, [stop(0, CYAN, 0.22), stop(0.25, CYAN, 0.05), stop(1, CYAN, 0)])
strands = [(1.2, 70, 1.0, 1.8), (1.8, 92, 0.55, 1.65), (1.0, 55, 0.45, 1.95)]
for angle in (math.atan2(H, W), -math.atan2(H, W)):
    w0, flare, _, p = strands[1]
    upper = beam_edge(FX, FY, angle, w0, flare, +1, p, reach=400)
    lower = beam_edge(FX, FY, angle, w0, flare, -1, p, reach=400)
    doc.path(poly(upper + lower[::-1], closed=True), fill=sheet, blend="Screen")
    for w0, flare, width, p in strands:
        for side in (+1, -1):
            pts = beam_edge(FX, FY, angle, w0, flare, side, p, reach=400)
            doc.path(poly(pts), stroke=edge, width=width, blend="Screen")

# The arrow: a laser line fading in from the app icon, through the focus, to a fine head.
x0, x1 = FX - 62, FX + 62
doc.path(f"M{x0} {FY} L{x1} {FY}", width=1.5,
         stroke=linear(x0, FY, x1, FY, [stop(0, "#7FE9FF", 0), stop(0.5, "#E6FBFF", 0.95), stop(1, "#E6FBFF", 0.9)]))
doc.path(f"M{x1 - 9} {FY - 7} L{x1} {FY} L{x1 - 9} {FY + 7}", stroke="#E6FBFF", width=1.5, opacity=90)

# The focus.
doc.path(circle(FX, FY, 120), fill=radial(FX, FY, 120, [stop(0, CYAN, 0.6), stop(0.15, CYAN, 0.18), stop(0.5, CYAN, 0.04), stop(1, CYAN, 0)]), blend="Screen")
doc.path(circle(FX, FY, 20), fill=radial(FX, FY, 20, [stop(0, "#FFFFFF", 1), stop(0.3, "#B4F4FF", 0.9), stop(1, CYAN, 0)]), blend="Screen")
doc.path(circle(FX, FY, 3.2), fill="#FFFFFF")

# A card under each icon: white behind the icon so the dark app icon stands out, and a grey
# footer behind the label, as Finder writes labels black in light mode and white in dark mode
# and this grey keeps both above 4.5:1.
def card(x, top, bottom, r, round_bottom=True):
    left, right = x - 76, x + 76
    end = (f"L{right} {bottom - r} A{r} {r} 0 0 1 {right - r} {bottom} L{left + r} {bottom} "
           f"A{r} {r} 0 0 1 {left} {bottom - r}") if round_bottom else f"L{right} {bottom} L{left} {bottom}"
    return (f"M{left} {top + r} A{r} {r} 0 0 1 {left + r} {top} L{right - r} {top} "
            f"A{r} {r} 0 0 1 {right} {top + r} {end} Z")


cards = Doc()
cards.cmd("file.new", name="Fokus DMG cards", width=W, height=H, units="Pixels")
for x in (165, 495):
    shadow = cards.path(card(x, 126, 306, 22), fill="#000000", opacity=55, name="card shadow")
    cards.cmd("effect.apply", id=shadow, effect="blur.gaussian", params={"radius": 10})
    cards.path(card(x, 120, 302, 22), fill="#6B7787", name="nameplate")
    cards.path(card(x, 120, 270, 22, round_bottom=False), fill=OFF_WHITE, name="card")

# Wordmark and the one instruction the window needs.
doc.cmd("file.place", path="src/wordmark-dark.vectorcraft", link=False, rect=[W / 2 - 60, 36, 120, 46])
doc.cmd("text.create", x=W / 2, y=356, text="DRAG FOKUS INTO APPLICATIONS", size=9.5, font="Inter",
        style="Medium", color="#7D8A9A", justify="center")
doc.cmd("text.setStyle", tracking=260)
doc.cmd("type.createOutlines")

doc.save(sys.argv[1])
cards.save(sys.argv[2])
