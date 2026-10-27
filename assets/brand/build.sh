#!/usr/bin/env bash
# Rebuild the Fokus brand assets from the generator scripts.
#
#   assets/brand/build.sh
#
# Needs vectorcraft-cli and photocraft-cli on PATH (macOS: also iconutil and tiffutil).
# The .py files write VectorCraft command lists; VectorCraft draws and exports the art,
# PhotoCraft adds the bloom and grain to the DMG background. Everything it writes is committed,
# so a normal build never runs this.
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p out src icon wordmark dmg

vc() { vectorcraft-cli run --commands "$@" >/dev/null; }

# App icon: the vector source, an SVG, and each size rendered from the vectors.
python3 icon.py out/icon.json
vc out/icon.json --export src/icon.vectorcraft --export icon/fokus.svg --export icon/fokus-1024.png
iconset=out/Fokus.iconset
rm -rf "$iconset" && mkdir -p "$iconset"
for size in 16 32 48 64 128 256 512 1024; do
  vectorcraft-cli convert src/icon.vectorcraft "out/icon-$size.png" --scale "$(python3 -c "print($size / 1024)")" >/dev/null
done
for size in 16 32 128 256 512; do
  cp "out/icon-$size.png" "$iconset/icon_${size}x${size}.png"
  cp "out/icon-$((size * 2)).png" "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o icon/fokus.icns
python3 ico.py icon/fokus.ico out/icon-16.png out/icon-32.png out/icon-48.png out/icon-256.png
cp out/icon-256.png icon/fokus-256.png

# Wordmark, for light and dark backgrounds, alone and with the icon.
for theme in light dark; do
  python3 wordmark.py "out/wordmark-$theme.json" "$theme"
  vc "out/wordmark-$theme.json" --export "src/wordmark-$theme.vectorcraft" \
    --export "wordmark/fokus-wordmark-$theme.svg" --export "wordmark/fokus-wordmark-$theme.png" --scale 2
  python3 wordmark.py "out/lockup-$theme.json" "$theme" lockup
  vc "out/lockup-$theme.json" --export "src/lockup-$theme.vectorcraft" \
    --export "wordmark/fokus-lockup-$theme.svg" --export "wordmark/fokus-lockup-$theme.png" --scale 2
done

# DMG background: vectors at 2x, then PhotoCraft lifts the light into a soft bloom (a blurred
# copy of only the bright parts, screened on), lays the icon cards on top, unbloomed, and adds
# fine grain against gradient banding.
python3 dmg.py out/dmg.json out/dmg-cards.json
vc out/dmg.json --export src/dmg.vectorcraft --export out/dmg-vector@2x.png --scale 2
vc out/dmg-cards.json --export src/dmg-cards.vectorcraft --export out/dmg-cards@2x.png --scale 2
photocraft-cli run out/dmg-vector@2x.png \
  --cmd layer.duplicate --params '{"name":"bloom"}' \
  --cmd image.adjustments.levels --params '{"inBlack":70,"gamma":0.8}' \
  --cmd filter.blur.gaussianBlur --params '{"radius":16}' \
  --cmd layer.setProps --params '{"blend":"Screen","opacity":0.7}' \
  --cmd layer.flattenImage \
  --cmd file.placeEmbedded --params "{\"path\":\"$PWD/out/dmg-cards@2x.png\",\"scale\":100,\"fit\":false,\"smartObject\":false,\"center\":[660,400]}" \
  --cmd layer.flattenImage \
  --cmd filter.noise.addNoise --params '{"amount":1.4,"distribution":"gaussian","monochromatic":true,"seed":7}' \
  --out dmg/background@2x.png >/dev/null
photocraft-cli run dmg/background@2x.png \
  --cmd image.imageSize --params '{"percent":50,"resample":"bicubicSharper"}' \
  --out dmg/background.png >/dev/null
# One TIFF holding both, so Finder picks the sharp one on Retina screens.
tiffutil -cathidpicheck dmg/background.png dmg/background@2x.png -out dmg/background.tiff 2>/dev/null

ls -l icon wordmark dmg
