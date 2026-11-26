#!/usr/bin/env bash
# Build the Linux release assets on Ubuntu 24.04 (glibc 2.39, the oldest supported distro):
# $DIST/weylus-v<version>-linux-x86_64.tar.gz and $DIST/Weylus-<version>-x86_64.AppImage.
# FFmpeg and x264 are linked statically (ci/build-ffmpeg.sh); libva, GStreamer, D-Bus, WebKitGTK
# and glibc come from the host, so neither asset bundles shared libraries.
# Usage, as root in an ubuntu:24.04 container, from the repository root: ci/package-linux.sh [deps|build|all]
set -euo pipefail

FFMPEG_PREFIX="${FFMPEG_PREFIX:-/opt/weylus-ffmpeg}"
TOOLS_DIR="${TOOLS_DIR:-/opt/weylus-tools}"
DIST="${DIST:-dist}"
CARGO_ABOUT_VERSION=0.9.3

deps() {
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    # The floor script's list without the FFmpeg -dev packages, plus what build-ffmpeg.sh (clang:
    # scale_cuda's CUDA kernels) and appimagetool need.
    apt-get install -y --no-install-recommends \
        build-essential pkg-config cmake git curl ca-certificates nasm xz-utils file zstd clang \
        libva-dev \
        libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libdbus-1-dev \
        libwebkit2gtk-4.1-dev libwayland-dev wayland-protocols libxkbcommon-dev nodejs npm
    if ! command -v cargo >/dev/null && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
            sh -s -- -y --profile minimal --default-toolchain stable
    fi
}

build() {
    if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
    if [ ! -f "$FFMPEG_PREFIX/lib/pkgconfig/libavcodec.pc" ]; then
        bash ci/build-ffmpeg.sh "$FFMPEG_PREFIX"
    fi
    export PKG_CONFIG_PATH="$FFMPEG_PREFIX/lib/pkgconfig"
    # The pkg-config crate links these statically; it only does so for libraries outside /usr.
    export LIBAVCODEC_STATIC=1 LIBAVFORMAT_STATIC=1 LIBAVFILTER_STATIC=1 LIBAVUTIL_STATIC=1
    pkg-config --modversion libavcodec libavfilter libavutil libva gstreamer-1.0
    rustc --version

    if ! cargo about --version 2>/dev/null | grep -q " $CARGO_ABOUT_VERSION\$"; then
        cargo install --locked --features cli "cargo-about@$CARGO_ABOUT_VERSION"
    fi

    npm ci
    # build.rs skips tsc when its cached run is fresh, but the .js files aren't in the checkout.
    npm run build
    cargo build --release --locked

    local bin=target/release/weylus
    if readelf -d "$bin" | grep NEEDED | grep -E 'libav(codec|format|filter|util|device)|libsw(scale|resample)|libx264'; then
        echo "FFmpeg is linked dynamically; expected the static build in $FFMPEG_PREFIX" >&2
        exit 1
    fi

    local version name pkg appdir tool licenses
    version="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
    stage="$(mktemp -d)"
    trap 'rm -rf "$stage"' EXIT
    mkdir -p "$DIST"

    # The licences of the crates linked into the binary, for the assets.
    licenses="$stage/THIRD-PARTY-LICENSES"
    cargo about generate --locked --output-file "$licenses" about.hbs
    test -s "$licenses" || { echo "cargo about produced no licence list" >&2; exit 1; }

    name="weylus-v$version-linux-x86_64"
    pkg="$stage/$name"
    mkdir -p "$pkg"
    install -m755 "$bin" "$pkg/weylus"
    strip "$pkg/weylus"
    sed 's/^Icon=.*/Icon=weylus/' weylus.desktop >"$pkg/weylus.desktop"
    install -m644 icons/icon.png "$pkg/weylus.png"
    install -m644 packaging/linux/weylus.svg packaging/linux/INSTALL.txt LICENSE \
        www/static/LUCIDE-LICENSE "$licenses" "$pkg/"
    install -m755 packaging/linux/install.sh "$pkg/install.sh"
    tar -C "$stage" -czf "$DIST/$name.tar.gz" "$name"

    # No libraries in the AppDir: GStreamer plugins, VA drivers and glibc must be the host's.
    appdir="$stage/AppDir"
    install -Dm755 "$pkg/weylus" "$appdir/usr/bin/weylus"
    install -Dm644 "$pkg/weylus.desktop" "$appdir/usr/share/applications/weylus.desktop"
    install -Dm644 packaging/linux/weylus.svg "$appdir/usr/share/icons/hicolor/scalable/apps/weylus.svg"
    install -Dm644 icons/icon.png "$appdir/usr/share/icons/hicolor/512x512/apps/weylus.png"
    install -Dm644 -t "$appdir/usr/share/licenses/weylus" LICENSE www/static/LUCIDE-LICENSE \
        "$licenses"
    # PNG at the root: appimagetool rejects the SVG as .DirIcon.
    cp "$pkg/weylus.desktop" "$pkg/weylus.png" "$appdir/"
    ln -s weylus.png "$appdir/.DirIcon"
    ln -s usr/bin/weylus "$appdir/AppRun"
    tool="$TOOLS_DIR/appimagetool-x86_64.AppImage"
    if [ ! -x "$tool" ]; then
        mkdir -p "$TOOLS_DIR"
        curl -sSfL -o "$tool" \
            https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
        chmod +x "$tool"
    fi
    # No FUSE in containers.
    ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream "$appdir" \
        "$DIST/Weylus-$version-x86_64.AppImage"
    ls -l "$DIST"
}

case "${1:-all}" in
deps) deps ;;
build) build ;;
all) deps && build ;;
*) echo "usage: package-linux.sh [deps|build|all]" >&2; exit 2 ;;
esac
