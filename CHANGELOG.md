# Changelog

## [Unreleased]

## [0.12.0] — 2026-11-25

### Added
- Bitrate setting for the video stream (Auto keeps constant quality)
- Fixed pen pressure option for pencils without a pressure sensor
- H.265 and AV1 video with hardware encoding (VA-API, NVENC) and a Codec setting; Auto picks the best codec the browser can decode and falls back to H.264
- Zoom and pan for the video view: +/− and reset buttons, and pinch or drag with fingers when touch input is off
- A chosen codec the device can't decode switches to Auto automatically
- The screen share is remembered until Weylus restarts, so reloading the page no longer asks for the screen again (`--no-remember-screen` turns this off)

### Changed
- Weylus builds against the system's FFmpeg and libva instead of bundled copies
- Wayland screen capture and VA-API hardware encoding are on by default (`--no-wayland` and `--no-vaapi` turn them off)
- Builds on Ubuntu 24.04, Debian 13, Fedora, Arch and openSUSE (FFmpeg 6.1 or newer; Rust 1.92 or newer via rustup)
- The web client decodes video with WebCodecs and drops late frames instead of falling behind; browsers without WebCodecs (iPadOS before 16.4) are no longer supported

### Removed
- Windows and macOS support
- X11 screen capture and X11 input; Weylus needs a Wayland session
- The "Lower Latency" video option (no longer needed)

### Fixed
- Capture Cursor works on Wayland; toggling it shows the screen picker again to restart the screen share
- Changing a video setting (bitrate, frame rate, resolution) or rotating the device no longer freezes the video on Wayland
- Pen strokes in Qt apps (Krita, Drawy) on Wayland are no longer joined by straight lines
- Random video delay with VA-API encoding
- VA-API hardware encoding works instead of always falling back to software encoding
- Pen tilt is reported in the correct range (was up to several times too large)
- The pen stays one tool across strokes instead of being re-added for every stroke on Wayland compositors
