#!/usr/bin/env bash
# package.sh: the Linux packages of the GPUI client from one ship binary (gpui-client/PACKAGING.md):
#
#   decibell-<v>-linux-x86_64.tar.xz   a relocatable directory (what aur/decibell-native-bin consumes)
#   Decibell-<v>-x86_64.AppImage       updates itself (needs --appimagetool and --runtime)
#   decibell-native_<v>_amd64.deb      Debian / Ubuntu
#   decibell-<v>-linux-x86_64.debug    the binary's symbols (crash report symbolication; not shipped)
#
#   packaging/linux/package.sh --binary target/release/decibell --out target/packages \
#       [--resources <dir with sentry.json / gifs.json>] [--appimagetool <path> --runtime <path>] \
#       [--kinds tarball,appimage,deb]
#
# From gpui-client/. The binary is checked first (packaging/check-ship.sh); the version is
# electron-client/package.json's. Each package carries resources/install.json with its kind (the
# updater's mode and the crash reports' tag come from it), the desktop entry, the hicolor icons and
# THIRD_PARTY_LICENSES.md. Nothing here registers anything on the build machine.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
client="$(cd "$here/../.." && pwd)"
repo="$(cd "$client/.." && pwd)"

binary="" out="" resources="" appimagetool="" runtime="" kinds="tarball,appimage,deb"
while [ $# -gt 0 ]; do
  case "$1" in
    --binary) binary="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --resources) resources="$2"; shift 2 ;;
    --appimagetool) appimagetool="$2"; shift 2 ;;
    --runtime) runtime="$2"; shift 2 ;;
    --kinds) kinds="$2"; shift 2 ;;
    *) echo "package.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -n "$binary" ] && [ -n "$out" ] || { echo "usage: package.sh --binary <decibell> --out <dir> [...]" >&2; exit 2; }
version="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$repo/electron-client/package.json")"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

"$client/packaging/check-ship.sh" "$binary" "$version"

# The symbols go to their own file; the shipped binary has none.
debug="$out/decibell-$version-linux-x86_64.debug"
objcopy --only-keep-debug "$binary" "$debug"
strip --strip-all -o "$work/decibell" "$binary"
chmod 0755 "$work/decibell"
build_id="$(readelf -n "$work/decibell" | sed -n 's/.*Build ID: //p')"
echo "package: decibell $version, build id $build_id, $(stat -c %s "$work/decibell") bytes stripped"

python3 -I "$client/packaging/licenses.py" --target x86_64-unknown-linux-gnu --c-licenses "$client/vendor/ffmpeg" \
  --version "$version" --out "$work/THIRD_PARTY_LICENSES.md"

# stage <dir> <kind>: the program directory (the executable, resources/, licenses/).
stage() {
  local dir="$1" kind="$2"
  mkdir -p "$dir/resources" "$dir/licenses"
  install -m 0755 "$work/decibell" "$dir/decibell"
  printf '{"kind":"%s","version":"%s"}\n' "$kind" "$version" > "$dir/resources/install.json"
  for f in sentry.json gifs.json; do
    if [ -n "$resources" ] && [ -f "$resources/$f" ]; then install -m 0644 "$resources/$f" "$dir/resources/$f"; fi
  done
  install -m 0644 "$work/THIRD_PARTY_LICENSES.md" "$dir/licenses/THIRD_PARTY_LICENSES.md"
}

# share <dir>: the desktop entry and the icons under a share/ directory.
share() {
  local dir="$1"
  install -D -m 0644 "$client/assets/linux/decibell.desktop" "$dir/applications/decibell.desktop"
  for png in "$client"/assets/linux/icons/hicolor/*/apps/decibell.png; do
    local size; size="$(basename "$(dirname "$(dirname "$png")")")"
    install -D -m 0644 "$png" "$dir/icons/hicolor/$size/apps/decibell.png"
  done
}

IFS=, read -r -a want <<< "$kinds"
for kind in "${want[@]}"; do
  case "$kind" in
    tarball)
      name="decibell-$version-linux-x86_64"
      root="$work/tar/$name"
      stage "$root" tarball
      share "$root/share"
      install -m 0644 "$here/README-tarball.txt" "$root/README.txt"
      # Reproducible: fixed owner, order and times.
      tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@${SOURCE_DATE_EPOCH:-0}" \
        -C "$work/tar" -cf - "$name" | xz -T0 -9 > "$out/$name.tar.xz"
      echo "package: $out/$name.tar.xz"
      ;;
    appimage)
      appdir="$work/Decibell.AppDir"
      stage "$appdir" appimage
      share "$appdir/usr/share"
      install -m 0644 "$client/assets/linux/decibell.desktop" "$appdir/decibell.desktop"
      install -m 0644 "$client/assets/linux/icons/hicolor/256x256/apps/decibell.png" "$appdir/decibell.png"
      ln -s decibell.png "$appdir/.DirIcon"
      # The runtime mounts the image and runs AppRun: the executable itself, so it finds
      # resources/ beside it.
      ln -s decibell "$appdir/AppRun"
      if [ -z "$appimagetool" ] || [ -z "$runtime" ]; then
        echo "package: AppImage skipped (no --appimagetool / --runtime); the AppDir is in $out/Decibell.AppDir"
        rm -rf "$out/Decibell.AppDir"
        cp -a "$appdir" "$out/Decibell.AppDir"
        continue
      fi
      ARCH=x86_64 "$appimagetool" --no-appstream --runtime-file "$runtime" "$appdir" "$out/Decibell-$version-x86_64.AppImage"
      echo "package: $out/Decibell-$version-x86_64.AppImage"
      ;;
    deb)
      root="$work/deb"
      stage "$root/usr/lib/decibell" deb
      mkdir -p "$root/usr/bin"
      ln -s ../lib/decibell/decibell "$root/usr/bin/decibell"
      share "$root/usr/share"
      install -D -m 0644 "$work/THIRD_PARTY_LICENSES.md" "$root/usr/share/doc/decibell-native/THIRD_PARTY_LICENSES.md"
      python3 -I "$here/mkdeb.py" --root "$root" --version "$version" --out "$out/decibell-native_${version}_amd64.deb"
      echo "package: $out/decibell-native_${version}_amd64.deb"
      ;;
    *) echo "package.sh: unknown kind $kind" >&2; exit 2 ;;
  esac
done
