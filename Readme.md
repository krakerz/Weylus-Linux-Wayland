# Weylus

Weylus turns your tablet into a graphic tablet, touch screen and mouse for your Linux desktop.

This is a fork of [H-M-H/Weylus](https://github.com/H-M-H/Weylus), maintained at
[krakerz/Weylus-Linux-Wayland](https://github.com/krakerz/Weylus-Linux-Wayland). It supports Wayland only.

## Table of Contents
* [Description](#description)
* [Features](#features)
* [Dependencies](#dependencies)
    * [Runtime](#runtime)
    * [Build](#build)
* [Installation](#installation)
    * [uinput](#uinput)
* [Building from source](#building-from-source)
* [Usage](#usage)
    * [Command line options](#command-line-options)
    * [Environment variables](#environment-variables)
    * [Settings panel](#settings-panel)
    * [Encryption](#encryption)
* [FAQ](#faq)
* [License](#license)

## Description
Weylus runs a small web server on your computer. Open the address it shows in the browser of your
tablet, and the tablet becomes a pen tablet, touch surface and mouse for your computer. The screen
is mirrored to the tablet as a low-latency video stream (WebCodecs), and pen, touch, mouse and
keyboard events are sent back to the computer and injected through `uinput`.

![Weylus in action](In_action.gif)

## Features
- Pen (stylus) input as a tablet device with pressure and tilt, through `uinput`
- Multi-touch input
- Mouse and physical keyboard input
- Screen mirroring of a Wayland screen via `xdg-desktop-portal` and PipeWire
- Low-latency video in the browser with WebCodecs; late frames are dropped instead of building up delay
- Video codecs H.264, H.265 and AV1; the Codec setting defaults to Auto, which picks the best codec the browser can decode and falls back to H.264
- Hardware video encoding through VA-API, and NVENC with `--try-nvenc`; software encoding when neither is available
- Bitrate, frame rate and maximum resolution settings
- Zoom and pan of the video view: zoom buttons and reset, and pinch or drag with fingers when touch input is off
- Fixed pen pressure mode for pencils without a pressure sensor
- Screen share is remembered until Weylus restarts, so reloading the page does not ask for the screen again
- Access code
- Tablet as a second screen, using a virtual output such as a dummy plug

## Dependencies

### Runtime
Needed to run Weylus.
- A Wayland session with `xdg-desktop-portal` (ScreenCast interface) and PipeWire
- An `xdg-desktop-portal` backend for your compositor: GNOME, KDE, wlroots-based (for example Sway)
  or Hyprland
- The GStreamer PipeWire plugin (provides the `pipewiresrc` element)
- `uinput` access for pen, touch and mouse input (see [uinput](#uinput))
- Optional, for hardware encoding: a VA-API driver (Mesa or Intel media driver) for H.264/H.265/AV1,
  or an NVIDIA GPU with its driver for NVENC (`--try-nvenc`)
- libva, GStreamer and D-Bus, which the binary links against; when built from source also the shared
  libraries of FFmpeg 6.1 or newer (libavcodec, libavformat, libavfilter, libavutil). The release
  downloads include FFmpeg
- WebKitGTK 4.1 (2.40 or newer) and GTK 3, for the desktop window (linked even with `--no-gui`)
- Optional: libayatana-appindicator3 (loaded at run time), for the system tray icon; without it Weylus runs without a tray icon and closing the window quits

Package names:

| Distribution | Packages |
|---|---|
| Arch Linux | `pipewire xdg-desktop-portal gst-plugin-pipewire mesa libva-utils webkit2gtk-4.1` (verified with `pacman -Si`), plus one portal backend: `xdg-desktop-portal-gnome`, `xdg-desktop-portal-kde`, `xdg-desktop-portal-wlr` or `xdg-desktop-portal-hyprland`; for Intel GPUs `intel-media-driver`; optional, for the tray icon: `libayatana-appindicator` |
| Ubuntu 24.04 / Debian 13 | the libraries from the Build list plus `pipewire xdg-desktop-portal gstreamer1.0-pipewire mesa-va-drivers` and one portal backend: `xdg-desktop-portal-gnome`, `xdg-desktop-portal-kde` or `xdg-desktop-portal-wlr` (no Hyprland portal package in these releases); for Intel GPUs `intel-media-va-driver`; optional, for the tray icon: `libayatana-appindicator3-1` |
| Fedora, openSUSE | the equivalents of the packages above; use the pkg-config names in [Build](#build) to find the libraries |

### Build
Needed to compile Weylus, in addition to the runtime dependencies.

Tools:
- Rust 1.92 or newer, installed with [rustup](https://rustup.rs). Distribution Rust packages are often
  older; the required version is `rust-version` in `Cargo.toml`.
- Node.js with npm (builds the TypeScript web client)
- pkg-config
- A C compiler (gcc or clang)

Libraries, found through pkg-config (their development files must be installed):
- FFmpeg 6.1 or newer: `libavcodec` (≥ 60.31.102), `libavformat` (≥ 60.16.100), `libavfilter`
  (≥ 9.12.100), `libavutil` (≥ 58.29.100)
- `libva` ≥ 1.20.0
- GStreamer 1.0 with `gstreamer-app-1.0`, `gstreamer-video-1.0` and the base plugins
  (`gstreamer-plugins-base-1.0`)
- `dbus-1`
- Wayland (`wayland-client`, `wayland-protocols`) and `xkbcommon`
- `webkit2gtk-4.1` ≥ 2.40 (pulls in GTK 3 and libsoup 3)

Package names:

**Ubuntu 24.04 or Debian 13** (exact list from the project's CI build script):
```sh
sudo apt install build-essential pkg-config nodejs npm curl ca-certificates \
libavcodec-dev libavformat-dev libavfilter-dev libavutil-dev libva-dev \
libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libdbus-1-dev \
libwebkit2gtk-4.1-dev libwayland-dev wayland-protocols libxkbcommon-dev
```
`curl` and `ca-certificates` are only needed to install Rust with rustup.

**Arch Linux** (verified with `pacman -Si`):
```sh
sudo pacman -S --needed base-devel pkgconf nodejs npm rustup ffmpeg libva gstreamer \
gst-plugins-base-libs dbus wayland wayland-protocols libxkbcommon webkit2gtk-4.1
```
Then `rustup default stable` (any Rust 1.92 or newer works).

**Fedora and openSUSE:** the package names differ, so install the development packages that provide
the libraries above. Check with `pkg-config --modversion libavcodec libva gstreamer-1.0 dbus-1
wayland-client xkbcommon webkit2gtk-4.1`. Use the full FFmpeg from RPM Fusion (Fedora) or Packman (openSUSE): the
distribution's default FFmpeg usually lacks the H.264 encoders Weylus needs. This is usually
required, but was not checked on these distributions.

## Installation
Download a release from the [releases page](https://github.com/krakerz/Weylus-Linux-Wayland/releases) (x86_64; Ubuntu
24.04, Debian 13 or newer, current Fedora, Arch and openSUSE):
- `Weylus-<version>-x86_64.AppImage`: make it executable (`chmod +x`) and run it.
- `weylus-v<version>-linux-x86_64.tar.gz`: unpack it and run `./install.sh` (installs to `~/.local`;
  `./install.sh --uninstall` removes it). `INSTALL.txt` in the archive lists the runtime packages.

Both include FFmpeg; install the other [Runtime](#runtime) dependencies. To build Weylus yourself,
follow [Building from source](#building-from-source).

Tablet: no app is needed. Use a browser with WebCodecs: iPadOS 16.4 or newer with Safari, or a current
Chrome or Firefox. H.265 and AV1 need a client that can decode them (AV1 needs a recent iPad, for
example one with M3, M4 or A17 Pro); Auto picks a codec the client supports.

### uinput
Weylus uses the `uinput` interface to simulate input events on Linux. **To enable stylus and
multi-touch support `/dev/uinput` needs to be writable by Weylus.** To make `/dev/uinput`
permanently writable by your user, run:
```sh
sudo groupadd -r uinput
sudo usermod -aG uinput $USER
echo 'KERNEL=="uinput", MODE="0660", GROUP="uinput", OPTIONS+="static_node=uinput"' \
| sudo tee /etc/udev/rules.d/60-weylus.rules
```

Then, either reboot, or run

```sh
sudo udevadm control --reload
sudo udevadm trigger
```

then log out and log in again. To undo this, run:

```sh
sudo rm /etc/udev/rules.d/60-weylus.rules
```

This allows your user to synthesize input events system-wide, even when another user is logged in.
Therefore, untrusted users should not be added to the uinput group.

## Building from source
Weylus only supports Linux. With the [Build](#build) dependencies installed, run:
```sh
npm install && cargo build --release
```
`npm install` is needed once; it installs the TypeScript compiler used by the build. The binary is
then at `target/release/weylus`.

## Usage
Start Weylus, then open the address it displays on your tablet. Both devices must be on the same
network. Weylus shows the address and a QR code in its window. The default address is
`http://<address of your computer>:1701`.

1. Start Weylus, from the GUI or headless: `./target/release/weylus --no-gui --auto-start`.
2. Optionally set an access code (`--access-code`) in the GUI, or on the command line.
3. Open the shown URL on the tablet.
4. Pick the screen to share in the screen-share dialog of your desktop. The dialog appears once per
   server run, because the screen share is remembered until Weylus restarts (`--no-remember-screen`
   asks on every connection).
5. Use the pen, touch or keyboard on the tablet. Keyboard input works with physical keyboards only;
   onscreen keyboards are not supported.

If a firewall is running, open the web port (1701/tcp by default); the video and input websocket
runs on the same port (`/ws`). With `ufw`:
```sh
sudo ufw allow 1701/tcp
```

### Updates
The AppImage and the `install.sh` build check for a new release at start-up and show it at the
bottom of the window: **Update** downloads and verifies it, **Restart** starts the new version. Turn
the check off with *Check at startup* or `--no-check-updates`; *Check now* checks on demand. Without
the GUI, Weylus only logs that a new release exists. Builds from source and distribution packages
update through their own channel.

### Command line options
Run `weylus --help` for the full list. The important options:

| Option | Effect |
|---|---|
| `--no-gui` | Run without the GUI and start immediately (headless) |
| `--auto-start` | Start the server immediately on program start |
| `--access-code <CODE>` | Require an access code on the tablet |
| `--allowed-clients <LIST>` | Only accept these IP addresses or CIDR ranges, comma-separated (loopback is always allowed) |
| `--ask-new-devices` | Ask in the desktop window before a device outside the allowed clients connects |
| `--web-port <PORT>` | Port of the web server (default `1701`) |
| `--bind-address <ADDR>` | Address to bind to (default `0.0.0.0`) |
| `--no-wayland` | Do not capture through `xdg-desktop-portal`/PipeWire (Wayland capture is on by default) |
| `--no-vaapi` | Do not use VA-API hardware encoding (on by default; falls back to software) |
| `--try-nvenc` | Try NVIDIA's NVENC for GPU encoding |
| `--no-remember-screen` | Ask for the screen on every connection instead of remembering it until restart |
| `--no-check-updates` | Don't check for a newer release at start-up |
| `--completions <SHELL>` | Print shell completions for bash, elvish, fish, powershell or zsh |

### Environment variables
- `WEYLUS_LOG_LEVEL`: log level, for example `DEBUG` or `TRACE`
- `WEYLUS_LOG_JSON`: set to any value to print logs as JSON, which is easier to parse in scripts
- `WEYLUS_PEN_LINGER=0`: turns off the pen-linger fix for Qt applications (pen strokes are joined by
  straight lines again)
- `WEBKIT_DISABLE_DMABUF_RENDERER=1`: try this if the desktop window stays blank (the AppImage sets it already)
- `LIBVA_DRIVER_NAME`: select a VA-API driver. The available drivers are in `/usr/lib/dri`
  (`ls /usr/lib/dri/ | sed -n 's/^\(\S*\)_drv_video.so$/\1/p'`)
- `LIBVA_DRIVERS_PATH`: directory to search for VA-API drivers, if they are not in `/usr/lib/dri`
- `WEYLUS_VAAPI_DEVICE`: the VA-API device, for example `/dev/dri/renderD129`. Devices are in `/dev/dri`

### Settings panel
The settings panel in the browser (the handle at the side of the page):
- **Capture**: the screen to mirror; `Refresh List` asks for the screen again.
- **Video**: Enable Video, Energy Saving, Stretch Video, Toggle Fullscreen, Capture Cursor, Max Video
  Resolution, Frame Rate, Bitrate (Auto keeps constant quality) and Codec (Auto, H.264, H.265 or AV1).
  A codec the device cannot decode switches to Auto.
- **Input**: Enable Mouse, Stylus, Touch and uinput; Pen pressure (from the device, or a fixed
  value for pencils without pressure); Client Name.
- At the bottom: the received frame rate and further display options.

The zoom controls float at the bottom of the video: zoom out, zoom level, zoom in and reset. With
Enable Touch off, pinch with two fingers to zoom and drag to pan; the pen keeps drawing.

Capture Cursor: turning it on or off asks for the screen again, because it restarts the screen share.
This is a limitation of the Wayland screen-share portal.

### Encryption
By default Weylus has no encryption and should only be run on networks you trust. If this is not the
case, set up a TLS proxy. One option is [hitch](https://hitch-tls.org/); the script `weylus_tls.sh`
in this repository sets one up with a self-signed certificate. Any TLS proxy works.

A self-signed certificate makes the browser show a warning; this can be accepted. In Firefox, a
[bug](https://bugzilla.mozilla.org/show_bug.cgi?id=1187666) prevents accepting a self-signed
certificate for the websocket connection. A workaround is to open the websocket URL directly in the
address bar and accept the certificate there.

## FAQ
Q: Why does the page not load on my tablet and I get a timeout?<br>
A: A firewall is probably blocking the web port. Open it (1701 by default).

Q: Why do I get the error `ERROR Failed to create uinput device: CError: code...`?<br>
A: `uinput` is not writable for your user. Follow the [uinput](#uinput) instructions and log out and
in again. A very old kernel may also lack the required features.

Q: Why is the "Capture" list empty, or the screen is not mirrored?<br>
A: Weylus captures through `xdg-desktop-portal` and PipeWire, so a portal backend for your
compositor must be installed and running. What can be captured is decided by the portal; if the
screen-share dialog was cancelled, press `Refresh List` to ask again.

Q: Why can I not select single windows, only the whole screen?<br>
A: The portal decides what can be captured. Weylus does not capture individual X11 windows.

Q: Why is there no video, a black screen, or a long video delay?<br>
A: Check the log for the video line `Video: <width>x<height>@<encoder> ...`. If the encoder is
`h264_vaapi` (hardware), test with `--no-vaapi`; if it is still wrong, check the VA-API driver with
`LIBVA_DRIVER_NAME`, `LIBVA_DRIVERS_PATH` and `WEYLUS_VAAPI_DEVICE`. Set `WEYLUS_LOG_LEVEL=DEBUG` to
see more.

Q: Why does the video not play on an older iPad with AV1?<br>
A: Only recent iPads decode AV1. Set Codec to Auto, which falls back to a codec the device can decode.

Q: Why is there no X11, Windows or macOS support?<br>
A: This fork targets Linux on Wayland only. X11 capture and input, and Windows and macOS support,
were removed.

Q: Why are pen strokes joined by straight lines in Qt applications such as Krita?<br>
A: This is fixed in this fork. `WEYLUS_PEN_LINGER=0` restores the previous behaviour.

Q: Do I need a second screen to use Weylus?<br>
A: No. Setting up a virtual or second screen is optional.

Q: Why am I unable to connect my tablet to the URL displayed by Weylus?<br>
A: The computer and the tablet must be on the same network. USB tethering and a tablet's WiFi hotspot
also work, because Weylus only needs an IP connection between the two devices.

Q: Can I use Weylus with an Android device over USB?<br>
A: Forward the port with `adb`: `adb reverse tcp:1701 tcp:1701`.
Then open `http://127.0.0.1:1701` on the device.

## License
Weylus is licensed under the GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later), see
[LICENSE](LICENSE).

This fork is modified by Alvi since November 2026; changes are listed in [CHANGELOG.md](CHANGELOG.md).
Contributed work is under the BSD-3-Clause licence, see [CONTRIBUTORS](CONTRIBUTORS).

Release builds bundle FFmpeg and x264 (GPL; the sources are linked in `INSTALL.txt`), the Lucide
icons (ISC) and the third-party Rust crates listed in `THIRD-PARTY-LICENSES`.

---

### Notes

Built and maintained with the help of AI.
