#!/usr/bin/env bash
# Build a minimal static FFmpeg (only what lib/encode_video.c uses) and static x264 into PREFIX, so
# release binaries don't depend on the distribution's FFmpeg sonames. libva stays dynamic (host drivers).
# Usage: ci/build-ffmpeg.sh PREFIX
set -euo pipefail

FFMPEG_VERSION=7.1.2
X264_COMMIT=b35605ace3ddf7c1a5d67a2eb553f034aef41d55 # stable branch
NV_CODEC_HEADERS=n12.1.14.0                           # NVENC SDK 12.1: NVIDIA driver 530 or newer
# Keep in sync with packaging/linux/INSTALL.txt (Bundled software).

prefix="${1:?usage: build-ffmpeg.sh PREFIX}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
jobs="$(nproc)"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig"

git clone --quiet --branch stable --single-branch https://code.videolan.org/videolan/x264.git "$work/x264"
git -C "$work/x264" checkout --quiet "$X264_COMMIT"
(
    cd "$work/x264"
    ./configure --prefix="$prefix" --enable-static --enable-pic --disable-cli
    make -j"$jobs"
    make install
)

git clone --quiet --depth 1 --branch "$NV_CODEC_HEADERS" \
    https://github.com/FFmpeg/nv-codec-headers.git "$work/nv-codec-headers"
make -C "$work/nv-codec-headers" PREFIX="$prefix" install

curl -sSfL "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz" | tar -C "$work" -xJ
cd "$work/ffmpeg-$FFMPEG_VERSION"
./configure --prefix="$prefix" --pkg-config-flags=--static \
    --enable-gpl --enable-static --disable-shared --enable-pic \
    --disable-autodetect --disable-everything --disable-programs --disable-doc \
    --disable-network --disable-avdevice --disable-swresample \
    --enable-libx264 --enable-vaapi --enable-ffnvcodec --enable-cuda --enable-cuda-llvm --enable-nvenc \
    --enable-encoder=libx264,h264_vaapi,hevc_vaapi,av1_vaapi,h264_nvenc,hevc_nvenc,av1_nvenc \
    --enable-filter=scale,scale_vaapi,scale_cuda,hwupload,hwupload_cuda # buffer/buffersink are always built
for component in LIBX264_ENCODER H264_VAAPI_ENCODER HEVC_VAAPI_ENCODER AV1_VAAPI_ENCODER \
    H264_NVENC_ENCODER HEVC_NVENC_ENCODER AV1_NVENC_ENCODER \
    SCALE_FILTER SCALE_VAAPI_FILTER SCALE_CUDA_FILTER HWUPLOAD_FILTER HWUPLOAD_CUDA_FILTER; do
    grep -q "^#define CONFIG_$component 1" config_components.h ||
        { echo "FFmpeg configure left out $component" >&2; exit 1; }
done
make -j"$jobs"
make install
