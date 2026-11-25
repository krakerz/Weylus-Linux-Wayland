# Weylus
![Build](https://github.com/H-M-H/Weylus/workflows/Build/badge.svg)

Weylus turns your tablet or smart phone into a graphic tablet/touch screen for your computer!

Weylus in action with [Xournal++](https://github.com/xournalpp/xournalpp):

![Weylus in action](In_action.gif)

## Table of Contents
* [Features](#features)
* [Installation](#installation)
    * [Packages](#packages)
* [Running](#running)
    * [Fullscreen](#fullscreen)
    * [Keyboard Input](#keyboard-input)
    * [Automation](#automation)
    * [Linux](#linux)
        * [Wayland](#wayland)
        * [Hardware Acceleration](#hardware-acceleration)
        * [Weylus as Second Screen](#weylus-as-second-screen)
            * [Dummy Plugs](#dummy-plugs)
            * [Other Options](#other-options)
        * [Encryption](#encryption)
* [Building](#building)
* [How does this work?](#how-does-this-work)
    * [Stylus/Touch](#stylustouch)
    * [Screen mirroring & window capturing](#screen-mirroring--window-capturing)
* [FAQ](#faq)

## Features
- Control your mouse with your tablet
- Mirror your screen to your tablet
- Send keyboard input using physical keyboards
- Hardware accelerated video encoding

The above features are available on all Operating Systems but Weylus works best on Linux. Additional
features on Linux are:
- Support for a stylus/pen (supports pressure and tilt)
- Multi-touch: Try it with software that supports multi-touch, like Krita, and see for yourself!
- Faster screen mirroring
- Tablet as second screen

## Installation
Just grab the latest release for your OS from the
[releases page](https://github.com/H-M-H/Weylus/releases) and install it on your computer. No apps
except a modern browser (Firefox 80+, iOS/iPadOS 13+) are required on your tablet. **If you run
Linux make sure to follow the instructions described [here](#linux) to enable uinput for features
like pressure sensitivity and multitouch!**

### Packages
AUR packages for Weylus are available here:
- From source: [weylus](https://aur.archlinux.org/packages/weylus/)
- Prebuilt binary: [weylus-bin](https://aur.archlinux.org/packages/weylus-bin/)

## Running
Start Weylus, preferably set an access code in the access code box and press the Start button. This
will start a webserver running on your computer. To control your computer with your tablet you need
to open the url `http://<address of your computer>:<port set in the menu, default is 1701>`, if
possible Weylus will display to you the url you need to open and show a QR code with the encoded
address. If you have a firewall running make sure to open a TCP port for the webserver (1701 by
default) and the websocket connection (9001 by default).

On many Linux distributions this is done with ufw:
```
sudo ufw allow 1701/tcp
sudo ufw allow 9001/tcp
```

Please only run Weylus in networks you trust as there is no encryption to enable minimal latencies.

### Fullscreen
You may want to add a bookmark to your home screen on your tablet as this enables running Weylus in
full screen mode (on iOS/iPadOS this needs to be done with Safari). If you are not on iOS/iPadOS
there is a button to toggle full screen mode.

### Keyboard Input
Weylus supports keyboard input for physical keyboards, so if you have a Bluetooth keyboard, just
connect it to your tablet and start typing. Due to technical limitations onscreen keyboards are not
supported.

### Automation
Weylus provides some features to make automation as convenient as possible. There is a command-line
interface; `--no-gui` for example starts Weylus in headless mode without a gui. For more options see
`weylus --help`. If you want to run a specific script e.g., once a client connects to your computer
you can do so by parsing the log Weylus generates. You may want to enable more verbose logging by
setting the environment variable `WEYLUS_LOG_LEVEL` to `DEBUG` or `TRACE` as well as
`WEYLUS_LOG_JSON` to `true` to enable easily parseable JSON logging.

### Linux
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

#### Wayland
Weylus captures the screen on Wayland through `xdg-desktop-portal` and PipeWire. This is enabled by
default, pass `--no-wayland` to turn it off. Installing `pipewire` and `xdg-desktop-portal` as
well as one of:
- `xdg-desktop-portal-gtk` for GNOME
- `xdg-desktop-portal-kde` for KDE
- `xdg-desktop-portal-wlr` for wlroots-based compositors like Sway
is required.

There are still some things that do not work:
- input mapping for windows
- displaying proper window names
- capturing the cursor

#### Hardware Acceleration
On Linux Weylus supports hardware accelerated video encoding through the Video Acceleration API
(VAAPI) or Nvidia's NVENC. VAAPI is enabled by default and Weylus falls back to software encoding
if it can not be used; pass `--no-vaapi` to turn it off. NVENC has to be enabled with `--try-nvenc`.
If VAAPI is used it is possible to select a specific driver by setting the
environment variable `LIBVA_DRIVER_NAME`. You can find possible values with the command
`ls /usr/lib/dri/ | sed -n 's/^\(\S*\)_drv_video.so$/\1/p'`. On some distributions the drivers may
not reside in `/usr/lib/dri` but for example in `/usr/lib/x86_64-linux-gnu/dri` and may not be found
by Weylus. To force Weylus to search another directory for drivers, the environment variable
`LIBVA_DRIVERS_PATH` can be set.
Additionally you can specify the VAAPI device to use by setting `WEYLUS_VAAPI_DEVICE`; by default
devices can be found in `/dev/dri`. On some systems this is not optional and this variable must be
set. If VAAPI doesn't work out of the box for you, have a look into `/dev/dri`, often setting
`WEYLUS_VAAPI_DEVICE=/dev/dri/renderD129` is already the solution. Note that you may need to install
the driver(s) first.

Nvidias NVENC is very fast but delivers a video stream of noticeably lower quality (at least on my
GeForce GTX 1050 Mobile GPU) but more recent GPUs should provide higher quality. For this to work
nvidia drivers need to be installed.

#### Weylus as Second Screen
There are a few possibilities to use Weylus to turn your tablet into a second screen.

##### Dummy Plugs
Weylus detects if you use multiple monitors and you can select the one you want to mirror. So if you
want to use Weylus as a second screen you could just buy another monitor. Obviously this is
pointless as if you already bought that monitor, there is no need to use Weylus! This is where so
called **HDMI/Displayport/VGA Dummy Plugs** come in handy. These are small devices that pretend to
be a monitor but only cost a fraction of the price of an actual monitor.

Once you have bought one and plugged it into your computer you can configure an additional screen
just like you would do with an actual one and then use Weylus to mirror this virtual screen.

##### Other Options
The following is untested/incomplete, feel free to do more research and open a pull request to
expand documentation on this!
- On Wayland with sway there is `create_output` which can be used to [create headless
  outputs](https://github.com/swaywm/sway/releases/tag/1.5), unfortunately it is not documented how
  to actually do that: https://github.com/swaywm/sway/issues/5553
- On Wayland with GNOME recently there has been added an option to [create virtual monitors with
  mutter](https://gitlab.gnome.org/GNOME/mutter/-/merge_requests/1698)

#### Encryption
By default Weylus comes without encryption and should only be run on networks you trust. If this is
not the case it's strongly advised to set up a TLS proxy. One option is to use
[hitch](https://hitch-tls.org/), an example script that sets up encryption is located at
`weylus_tls.sh`.
But any TLS proxy should work just fine.

Note that the mentioned script works by creating a self-signed certificate. This means your browser
will most likely display a scary looking but completely unfounded message telling you how incredibly
dangerous it is to trust the certificate you yourself just created; this can be safely ignored!

In case you are using Firefox: There is a [bug](https://bugzilla.mozilla.org/show_bug.cgi?id=1187666)
that prevents users from accepting self-signed certificates for websocket connections. A workaround
is to directly open the websocket connection via the URL bar and accept the certificate there. After
accepting the connection will of course fail as the browser expects https and not wss as protocol.

Sadly this solution is anything but frictionless and I am unhappy with the current state of affairs.
This is also another reason why encryption is not enabled by default, self-signed certificates are
just too painful to handle nowadays. I'd gladly welcome any proposals to improve the situation!

## Building
Weylus only supports Linux. To build it you need Rust (install it with [rustup](https://rustup.rs); distribution packages are often older than
the required version, see `rust-version` in Cargo.toml), Node.js with npm, pkg-config, a C compiler,
FFmpeg 6.1 or newer (libavcodec, libavformat, libavfilter and libavutil) and libva with their
development files, GStreamer with its base plugins' development files, dbus, and the
dependencies required to build fltk (cmake, pango, cairo, wayland, libxkbcommon, libx11, libxext,
libxinerama, libxcursor, libxft, libxfixes, libxrender). On Arch Linux, for example:
```sh
sudo pacman -S --needed base-devel rust ffmpeg libva libx11 libxext libxfixes dbus gstreamer \
gst-plugins-base-libs cmake wayland wayland-protocols libxkbcommon pango cairo libxinerama \
libxcursor libxft nodejs npm
```
On Ubuntu 24.04 or Debian 13:
```sh
sudo apt install build-essential pkg-config cmake libavcodec-dev libavformat-dev libavfilter-dev \
libavutil-dev libva-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libdbus-1-dev \
libx11-dev libxext-dev libxft-dev libxinerama-dev libxcursor-dev libxrender-dev libxfixes-dev \
libpango1.0-dev libgl1-mesa-dev libglu1-mesa-dev libwayland-dev wayland-protocols \
libxkbcommon-dev nodejs npm
```
On Fedora and openSUSE use the full FFmpeg from RPM Fusion / Packman; the distribution's
ffmpeg-free lacks the H.264 encoders Weylus needs.
Then build the project with:
```sh
npm install && cargo build --release
```

## How does this work?
### Stylus/Touch
Modern browsers expose so called
[PointerEvents](https://developer.mozilla.org/en-US/docs/Web/API/PointerEvent) that can convey not
only mouse but additionally stylus/pen and touch information. Weylus sets up a webserver with the
corresponding javascript code to capture these events. The events are sent back to the server using
websockets.
Weylus then processes these events using the uinput backend. It makes use of the
uinput Linux kernel module which supports creating a wide range of input devices including mouse,
stylus and touch input devices.

### Screen mirroring & window capturing
Weylus captures the screen on Wayland only: `xdg-desktop-portal` and PipeWire provide the stream
and GStreamer receives it. The images captured are
then encoded to a video stream using ffmpeg. Fragmented MP4 is used as container format to enable
browsers to play the stream via the Media Source Extensions API. The video codec used is H.264 as
this is widely supported and allows very fast encoding as opposed to formats like AV1. To minimize
dependencies ffmpeg is statically linked into Weylus.

## FAQ
Q: Why does the page not load on my tablet and instead I get a timeout?<br>
A: There probably is some kind of firewall running, make sure the ports Weylus uses are opened.

Q: Why do I get the error `ERROR Failed to create uinput device: CError: code...`?<br>
A: uinput is probably misconfigured, have you made sure to follow all instructions and logged out
and in again? You may also be running a very old kernel that does not support the required features.
In that case try to upgrade your system or use a newer one.

Q: Why is the "Capture" drop down empty and the screen not mirrored?<br>
A: It is possible that only the port for the webserver but not the websocket has been opened, check
that both ports have been opened.

Q: Why can I not select any windows in the "Capture" drop down and only see the whole screen.<br>
A: Weylus captures via xdg-desktop-portal and PipeWire, so what can be captured is decided by your
desktop's portal; X11 is not supported.

Q: Do I have to follow the instructions to setup Weylus as second screen too?<br>
A: No, this is strictly optional.

Q: Why am I unable to connect my tablet to the URL displayed by Weylus?<br>
A: It is possible that your computer and WiFi connected tablet are on different networks, make sure
they are on the same network.

Q: Why does this not run on Firefox for Android?<br>
A: Actually it does, just make sure Firefox version 80+ is installed.

Q: Why does this not run under Chrome on my iPad?<br>
A: Chrome lacks some features for video streaming on iPadOS/iOS, try Firefox or Safari.

Q: Why won't my cursor move in osu! ?<br>
A: Try disabling raw input.

Q: Can I use Weylus even if there is no WiFi?<br>
A: Probably yes! Most tablets permit setting up a WiFi hotspot that can be used to connect your
computer and tablet. Alternatively there is USB tethering too, which can be used to setup a peer to
peer connection between your tablet and computer over USB. Another method for Android devices is to
setup a socket connection with
[adb](https://developer.android.com/studio/command-line/adb#Enabling):
```console
adb reverse tcp:1701 tcp:1701
adb reverse tcp:9001 tcp:9001
```
Like that you can connect from your Android device to Weylus with the URL: `http://127.0.0.1:1701`.

Weylus only requires that your devices
are connected via the Internet Protocol and that doesn't necessarily imply WiFi.

---

[![Packaging status](
https://repology.org/badge/vertical-allrepos/weylus.svg
)](https://repology.org/project/weylus/versions)
