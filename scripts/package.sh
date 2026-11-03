#!/usr/bin/env bash
# Package a release build of Fokus for one platform into dist/.
#
#   scripts/package.sh <target-triple>   e.g. x86_64-pc-windows-msvc
#
# Expects `cargo build --release --locked -p fokus --target <triple>` to have
# run. Used by .github/workflows/release.yml and for local test packages.
set -euo pipefail

target="${1:?usage: scripts/package.sh <target-triple>}"
root="$(cd "$(dirname "$0")/.." && pwd)"
version="$(grep -m1 '^version' "$root/fokus/Cargo.toml" | cut -d'"' -f2)"
out="$root/dist"
bin_dir="$root/target/$target/release"
# Download names say the system in words, not the target triple.
case "$target" in
  aarch64-apple-darwin) label=mac-apple-silicon ;;
  x86_64-apple-darwin) label=mac-intel ;;
  x86_64-pc-windows-msvc) label=windows ;;
  x86_64-unknown-linux-gnu) label=linux-x86_64 ;;
  *) label="$target" ;;
esac
name="fokus-$version-$label"
mkdir -p "$out"
rm -rf "${out:?}/$name" "$out/$name".* "$out/$name"-setup.*

case "$target" in
  *windows*)
    mkdir -p "$out/$name"
    cp "$bin_dir/fokus.exe" "$root/README.md" "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$out/$name/"
    (cd "$out" && 7z a -tzip "$name.zip" "$name" >/dev/null)
    # The installer (Inno Setup 6 or newer; ISCC on PATH or in Program Files).
    iscc="$(command -v iscc || ls "/c/Program Files (x86)/Inno Setup "*/ISCC.exe "/c/Program Files/Inno Setup "*/ISCC.exe 2>/dev/null | tail -1 || true)"
    [ -n "$iscc" ] || { echo "Inno Setup not found (choco install innosetup)" >&2; exit 1; }
    # Windows paths, and no MSYS path conversion of the /D options.
    MSYS2_ARG_CONV_EXCL='*' "$iscc" /Q "/DVersion=$version" "/DSource=$(cygpath -w "$out/$name")" \
      "/DOutput=$(cygpath -w "$out")" "/DOutputName=$name-setup" "$(cygpath -w "$root/packaging/windows/fokus.iss")"
    ;;
  *apple-darwin*)
    app="$out/$name/Fokus.app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp "$bin_dir/fokus" "$app/Contents/MacOS/fokus"
    cp "$root/assets/brand/icon/fokus.icns" "$app/Contents/Resources/"
    sed "s/__VERSION__/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
    # Signed when MACOS_SIGN_IDENTITY names a "Developer ID Application"
    # certificate in the keychain, notarised when there are notary
    # credentials too: an App Store Connect API key (NOTARY_KEY = path to the
    # .p8, NOTARY_KEY_ID, NOTARY_ISSUER) or a profile saved with
    # `xcrun notarytool store-credentials` (NOTARY_PROFILE). Without them the
    # app stays unsigned and macOS asks once (Privacy & Security → Open Anyway).
    identity="${MACOS_SIGN_IDENTITY:-}"
    notary=()
    if [ -n "${NOTARY_KEY:-}" ]; then
      notary=(--key "$NOTARY_KEY" --key-id "${NOTARY_KEY_ID:?}" --issuer "${NOTARY_ISSUER:?}")
    elif [ -n "${NOTARY_PROFILE:-}" ]; then
      notary=(--keychain-profile "$NOTARY_PROFILE")
    fi
    notarise() { # file to notarise, then staple
      xcrun notarytool submit "$1" "${notary[@]}" --wait --timeout 30m
      xcrun stapler staple "$2"
    }
    if [ -n "$identity" ]; then
      codesign --force --timestamp --options runtime --sign "$identity" "$app"
      codesign --verify --strict --verbose=2 "$app"
      if [ ${#notary[@]} -gt 0 ]; then
        # The app itself too, so it opens without a network check once copied out of the image.
        ditto -c -k --keepParent "$app" "$out/$name-app.zip"
        notarise "$out/$name-app.zip" "$app"
        rm "$out/$name-app.zip"
      fi
    fi
    # A disk image with the app, an Applications link and the background that says what to do.
    # dmgbuild writes the Finder layout itself, so this needs no GUI session.
    venv="$root/target/dmgbuild-venv"
    [ -x "$venv/bin/dmgbuild" ] || { python3 -m venv "$venv" && "$venv/bin/pip" install -q dmgbuild==1.6.7; }
    "$venv/bin/dmgbuild" -s "$root/packaging/macos/dmg-settings.py" \
      -D app="$app" -D background="$root/assets/brand/dmg/background.tiff" \
      "Fokus $version" "$out/$name.dmg" >/dev/null
    if [ -n "$identity" ]; then
      codesign --force --timestamp --sign "$identity" "$out/$name.dmg"
      [ ${#notary[@]} -eq 0 ] || notarise "$out/$name.dmg" "$out/$name.dmg"
    fi
    ;;
  *linux*)
    mkdir -p "$out/$name"
    cp "$bin_dir/fokus" "$root/README.md" "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/packaging/linux/fokus.desktop" "$out/$name/"
    cp "$root/assets/brand/icon/fokus-256.png" "$out/$name/fokus.png"
    tar -C "$out" -czf "$out/$name.tar.gz" "$name"
    # An AppImage: one file that runs on most distributions. GTK and udev come from the system.
    appdir="$out/$name.AppDir"
    mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" "$appdir/usr/share/icons/hicolor/256x256/apps"
    cp "$bin_dir/fokus" "$appdir/usr/bin/"
    cp "$root/packaging/linux/fokus.desktop" "$appdir/usr/share/applications/"
    cp "$root/assets/brand/icon/fokus-256.png" "$appdir/usr/share/icons/hicolor/256x256/apps/fokus.png"
    ln -s usr/share/applications/fokus.desktop "$appdir/fokus.desktop"
    ln -s usr/share/icons/hicolor/256x256/apps/fokus.png "$appdir/fokus.png"
    ln -s usr/bin/fokus "$appdir/AppRun"
    tool="$root/target/appimagetool-1.9.1-x86_64.AppImage"
    [ -x "$tool" ] || { curl -fsSL -o "$tool" https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage && chmod +x "$tool"; }
    # Extract-and-run, so building needs no FUSE.
    ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream "$appdir" "$out/$name.AppImage" >/dev/null
    rm -rf "$appdir"
    ;;
  *)
    echo "unknown target $target" >&2
    exit 1
    ;;
esac
rm -rf "${out:?}/$name"
# Signatures for the app's updater (.sig next to each file), when the key is there.
shopt -s nullglob
files=("$out/$name".* "$out/$name"-setup.*)
if [ -n "${UPDATE_SIGNING_KEY:-}" ]; then
  (cd "$root" && cargo run -q -p fokus-sign -- sign "${files[@]}")
fi
ls -l "$out/$name".* "$out/$name"-setup.*
