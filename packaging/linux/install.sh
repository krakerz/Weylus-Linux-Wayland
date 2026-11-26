#!/bin/sh
# Install Weylus for the current user (default prefix ~/.local), or remove it again.
# Usage: ./install.sh [--prefix DIR] [--uninstall]
set -eu

prefix="$HOME/.local"
uninstall=0
while [ $# -gt 0 ]; do
    case "$1" in
    --prefix) prefix="${2:?--prefix needs a directory}"; shift 2 ;;
    --uninstall) uninstall=1; shift ;;
    -h | --help) sed -n '2,3p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

bin="$prefix/bin/weylus"
desktop="$prefix/share/applications/weylus.desktop"
icon="$prefix/share/icons/hicolor/scalable/apps/weylus.svg"
icon_png="$prefix/share/icons/hicolor/512x512/apps/weylus.png"
# Weylus only offers in-app updates when this file sits next to its install.
marker="$prefix/share/weylus/installed-with"

if [ "$uninstall" = 1 ]; then
    rm -f "$bin" "$desktop" "$icon" "$icon_png" "$marker"
    rmdir "$(dirname "$marker")" 2>/dev/null || true
    echo "Removed Weylus from $prefix (your Weylus settings are kept)."
    exit 0
fi

here="$(cd "$(dirname "$0")" && pwd)"
install -Dm644 "$here/weylus.svg" "$icon"
install -Dm644 "$here/weylus.png" "$icon_png"
mkdir -p "$(dirname "$desktop")"
# Absolute path: ~/.local/bin is not on every desktop session's PATH.
sed -e "s|^Exec=.*|Exec=$bin|" -e "s|^TryExec=.*|TryExec=$bin|" "$here/weylus.desktop" >"$desktop"
if command -v update-desktop-database >/dev/null; then
    update-desktop-database -q "$(dirname "$desktop")" || true
fi
mkdir -p "$(dirname "$marker")"
echo install.sh >"$marker"
# Copy, then rename: a running Weylus (an in-app update) keeps its old file and writing over a
# running binary fails with "text file busy".
mkdir -p "$prefix/bin"
install -m755 "$here/weylus" "$prefix/bin/.weylus.new"
mv -f "$prefix/bin/.weylus.new" "$bin"
echo "Installed Weylus to $bin"
case ":$PATH:" in
*":$prefix/bin:"*) ;;
*) echo "Note: $prefix/bin is not in your PATH; start Weylus from the app menu or with $bin." ;;
esac
echo "Pen and touch input need write access to /dev/uinput: see INSTALL.txt."
