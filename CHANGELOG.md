# Changelog

## [Unreleased]

## [0.12.0] — 2026-11-25

### Changed
- Weylus builds against the system's FFmpeg and libva instead of bundled copies
- Wayland screen capture and VA-API hardware encoding are on by default (`--no-wayland` and `--no-vaapi` turn them off)
- Builds on Ubuntu 24.04, Debian 13, Fedora, Arch and openSUSE (FFmpeg 6.1 or newer; Rust 1.92 or newer via rustup)

### Removed
- Windows and macOS support
- X11 screen capture and X11 input; Weylus needs a Wayland session

### Fixed
- Pen strokes in Qt apps (Krita, Drawy) on Wayland are no longer joined by straight lines
- Random video delay with VA-API encoding
- VA-API hardware encoding works instead of always falling back to software encoding
- Pen tilt is reported in the correct range (was up to several times too large)
- The pen stays one tool across strokes instead of being re-added for every stroke on Wayland compositors
