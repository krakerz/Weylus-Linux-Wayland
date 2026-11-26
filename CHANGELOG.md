# Changelog

## [Unreleased]

## [0.13.0] — 2026-11-26

### Added
- New desktop window: settings, the tablet address with QR code, the log and updates in one view, with light and dark mode
- "Remember shared screen" setting in the desktop window; with a portal that supports restore tokens it also lasts across restarts
- "Start with my session" setting in the desktop window: Weylus starts hidden in the tray with the server running when you log in (`--hidden` does the same by hand)
- Tray icon with show, start/stop and quit; closing the window keeps Weylus in the tray (can be turned off)
- Copy button for the tablet address
- Shortcut bar on the tablet for drawing apps (undo, redo, eraser, brush size, hold-to-pan and more) with Krita, GIMP, Xournal++ and generic presets plus custom buttons
- Stats overlay on the tablet: frame rate, decode time, estimated end-to-end latency, dropped frames, bitrate and codec
- Allowed clients setting (`--allowed-clients`): only the listed IP addresses or ranges can connect; this computer always can
- Optional prompt in the desktop window before a new device connects: allow this session, always allow, or deny
- Connected devices list in the desktop window with codec, resolution and frame rate, and Disconnect and Block buttons
- Source code link in the tablet page and the desktop window
- Release packages include the licences of bundled third-party code
- Optional HTTPS mode (`--https` or the desktop setting) with a self-signed certificate, so tablet browsers treat the page as secure; plain HTTP stays the default

### Changed
- The desktop window needs WebKitGTK 4.1 (`webkit2gtk-4.1`) instead of FLTK
- Custom input areas are set as percentages of the screen in the desktop window
- The zoom controls on the tablet fold into a small button after a few seconds; tap it to open them again
- The tablet's "Refresh List" button is now "Change screen" and shows when to pick the screen on the computer

### Removed
- The `--gui-theme` option and the FLTK themes

### Security
- Access codes are compared in constant time
- Five wrong access codes within five minutes block that address for five minutes

## [0.12.0] — 2026-11-25

### Added
- Bitrate setting for the video stream (Auto keeps constant quality)
- Fixed pen pressure option for pencils without a pressure sensor
- H.265 and AV1 video with hardware encoding (VA-API, NVENC) and a Codec setting; Auto picks the best codec the browser can decode and falls back to H.264
- Zoom and pan for the video view: +/− and reset buttons, and pinch or drag with fingers when touch input is off
- A chosen codec the device can't decode switches to Auto automatically
- The screen share is remembered until Weylus restarts, so reloading the page no longer asks for the screen again (`--no-remember-screen` turns this off)
- Prebuilt Linux downloads on the releases page: an AppImage and a tar.gz with an install script, both with FFmpeg included
- In-app updates for the AppImage and the install.sh build: Weylus checks for a new release at start-up (`--no-check-updates` turns this off) and installs it with one click

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
