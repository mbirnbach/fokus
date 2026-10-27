# Fokus brand assets

Two laser beams converging to one focal point: the app's idea in one picture.

| File | Use |
|---|---|
| `icon/fokus.icns` | macOS app icon (`scripts/package.sh` puts it in `Fokus.app`) |
| `icon/fokus.ico` | Windows exe icon, 16–256 px (`fokus/build.rs`) |
| `icon/fokus-256.png` | Window icon (`fokus/src/main.rs`), Linux `fokus.png` |
| `icon/fokus.svg`, `icon/fokus-1024.png` | The icon at full size, for the web and documents |
| `wordmark/fokus-wordmark-{light,dark}.{svg,png}` | The word alone; *light* is for light backgrounds |
| `wordmark/fokus-lockup-{light,dark}.{svg,png}` | Icon and word side by side |
| `dmg/background.tiff` | DMG window background, 660 × 400 pt at 1x and 2x (`packaging/macos/dmg-settings.py`) |
| `src/*.vectorcraft` | The vector sources, to open and edit in VectorCraft |

The SVGs use outlines for their text, so they show correctly without the font installed.

**Colours:** Graphite `#0B0F14`, Cool Gray `#1F2937`, Off White `#F4F6F8`, Cyan `#00D7FF`.
**Type:** Inter SemiBold, tracked −25, with the "o" drawn as a ring around a cyan point.
Give the wordmark at least the height of its "o" as clear space on every side.

## Rebuilding

The art is generated: `icon.py`, `wordmark.py` and `dmg.py` write VectorCraft command lists,
[VectorCraft](https://getartcraft.com/apps/vectorcraft) draws and exports them, and
[PhotoCraft](https://getartcraft.com) adds the bloom and grain to the DMG background. With
`vectorcraft-cli` and `photocraft-cli` on `PATH`:

```bash
assets/brand/build.sh
```

It rewrites every file above in a few seconds. Change the geometry or colours in the `.py`
files rather than editing the exports. Edits made by hand in `src/*.vectorcraft` are lost on
the next rebuild.

The DMG background is dark, but Finder draws icon labels black in light mode and white in dark
mode. The cards under the two icons (white behind the icon, grey behind the label) keep the
icon and both label colours readable, so keep them if you change the background.
