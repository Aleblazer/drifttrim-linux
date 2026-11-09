# DriftTrim for Linux

[![CI](https://github.com/Aleblazer/drifttrim-linux/actions/workflows/ci.yml/badge.svg)](https://github.com/Aleblazer/drifttrim-linux/actions/workflows/ci.yml)

Ride the volume of your apps and audio devices in real time, with physical
faders, on-screen faders or global shortcuts. This is a KDE Plasma app written
in Rust with Kirigami, reworked from the Windows
[DriftTrim](https://github.com/Aleblazer/drifttrim).

Point each fader at an **output device**, a **single app**, a **category of
apps**, or **Everything Else**, and it keeps working from the system tray.

## Features

- **Physical faders**: QMK/VIK and other MIDI CC controllers (14-bit and 7-bit),
  and ZMK vendor-HID devices. Sweep a fader to detect it.
- **Virtual faders** you drag on screen. No hardware needed.
- **Global shortcuts** for virtual faders, set in Plasma's System Settings.
- **Soft takeover**: a physical fader takes control only once it reaches the
  current volume, so levels never jump.
- **Calibration**: taper presets, inversion, a recorded range and a mute detent.
- **Ranked outputs** with automatic failover on hotplug, and per-output Max %.
- **Categories** of apps that move together.
- Follows your Plasma theme, sits in the tray and can start at login.

## Install

You need Rust and the KDE libraries. On Arch or CachyOS:

```bash
sudo pacman -S --needed rust qt6-base qt6-declarative kirigami kirigami-addons qqc2-desktop-style libpulse alsa-lib hidapi
```

Then build and install for your user:

```bash
packaging/install.sh
```

MIDI controllers work straight away. **ZMK faders** read a raw HID device, so
they need a one-time permission rule, which the installer prints for you.

## Usage

Choose **Add fader** to add a physical or virtual fader. For a physical fader,
pick the device, sweep one fader end to end and select **Capture**. Then choose
what it controls from the two dropdowns on its card.

The **⋮** menu on each card has calibration, output ranking, renaming and
reordering. To bind keys to a virtual fader, choose **Set global shortcuts…** and
look for DriftTrim in the list.

Run `drifttrim --list-devices` to see the outputs, apps and fader sources the app
can detect. Settings are stored in `~/.config/drifttrim/settings.json`.

## Differences from the Windows app

- **Shortcuts use KDE's global shortcut service**, so a bound key is consumed
  and does not also reach the focused app. Wayland offers no normal way to
  observe keys system-wide.
- **The theme follows Plasma** rather than having its own light and dark setting.
- **Windows settings are not imported**, because device and app identifiers differ.

## Status

Tested on CachyOS with Plasma 6.8 on Wayland and PipeWire 1.6, with a QMK
ZodiPact over MIDI and with real audio devices. **ZMK HID input has unit tests
for its report parsing but has not been run against real hardware yet.** Reports
from ZMK users are very welcome.

## Development

```bash
cargo test --workspace
```

`crates/drifttrim-core` holds the fader logic and has no system dependencies.
`crates/drifttrim-app` holds the PulseAudio, ALSA, HID, D-Bus and QML layers.

## License

[MIT](LICENSE)
