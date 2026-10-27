# dmgbuild settings for the Fokus disk image (scripts/package.sh passes -D app=… -D background=…).
# The icon positions match the gaps in assets/brand/dmg.py's background.
import os.path

app = defines["app"]  # noqa: F821 (dmgbuild provides `defines`)

format = "UDZO"
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
badge_icon = None
icon = os.path.join(app, "Contents", "Resources", "fokus.icns")

background = defines["background"]  # noqa: F821
window_rect = ((200, 120), (660, 400))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
icon_size = 128
text_size = 13
icon_locations = {
    os.path.basename(app): (165, 200),
    "Applications": (495, 200),
}
