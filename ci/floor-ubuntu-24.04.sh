#!/usr/bin/env bash
# Build and test Weylus on Ubuntu 24.04 (the oldest supported distro) with the MSRV toolchain.
# Run inside an ubuntu:24.04 container with the repository as the working directory.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends \
    build-essential pkg-config cmake git curl ca-certificates \
    libavcodec-dev libavformat-dev libavfilter-dev libavutil-dev libva-dev \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libdbus-1-dev \
    libx11-dev libxext-dev libxft-dev libxinerama-dev libxcursor-dev libxrender-dev \
    libxfixes-dev \
    libpango1.0-dev libgl1-mesa-dev libglu1-mesa-dev libwayland-dev wayland-protocols \
    libxkbcommon-dev nodejs npm

MSRV="$(grep -m1 '^rust-version' Cargo.toml | cut -d'"' -f2)"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
    sh -s -- -y --profile minimal --default-toolchain "$MSRV"
. "$HOME/.cargo/env"

pkg-config --modversion libavcodec libavfilter libavutil libva gstreamer-1.0
rustc --version

npm ci
cargo build --locked
cargo test --locked -- --nocapture  # show hardware-test SKIPPED lines
