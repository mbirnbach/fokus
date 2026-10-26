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
name="fokus-$version-$target"
mkdir -p "$out"
rm -rf "${out:?}/$name" "$out/$name".*

case "$target" in
  *windows*)
    mkdir -p "$out/$name"
    cp "$bin_dir/fokus.exe" "$root/README.md" "$out/$name/"
    (cd "$out" && 7z a -tzip "$name.zip" "$name" >/dev/null)
    ;;
  *apple-darwin*)
    app="$out/$name/Fokus.app"
    mkdir -p "$app/Contents/MacOS"
    cp "$bin_dir/fokus" "$app/Contents/MacOS/fokus"
    sed "s/__VERSION__/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
    cp "$root/README.md" "$out/$name/"
    # Not signed or notarised: macOS asks once (right-click → Open).
    (cd "$out" && ditto -c -k --norsrc --noextattr --keepParent "$name" "$name.zip")
    ;;
  *linux*)
    mkdir -p "$out/$name"
    cp "$bin_dir/fokus" "$root/README.md" "$root/packaging/linux/fokus.desktop" "$out/$name/"
    tar -C "$out" -czf "$out/$name.tar.gz" "$name"
    ;;
  *)
    echo "unknown target $target" >&2
    exit 1
    ;;
esac
rm -rf "${out:?}/$name"
ls -l "$out"/"$name".*
