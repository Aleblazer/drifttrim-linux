#!/usr/bin/env bash
# Builds DriftTrim and installs it for the current user under ~/.local.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
prefix="${PREFIX:-$HOME/.local}"

cd "$root"
cargo build --release -p drifttrim

install -Dm755 target/release/drifttrim "$prefix/bin/drifttrim"
install -Dm644 packaging/org.drifttrim.DriftTrim.desktop \
    "$prefix/share/applications/org.drifttrim.DriftTrim.desktop"
install -Dm644 packaging/org.drifttrim.DriftTrim.png \
    "$prefix/share/icons/hicolor/128x128/apps/org.drifttrim.DriftTrim.png"

# Plasma shows the new launcher and icon without logging out.
kbuildsycoca6 >/dev/null 2>&1 || true

echo "Installed to $prefix. Start DriftTrim from the application launcher."
echo
echo "ZMK faders also need this one-time permission rule (MIDI does not):"
echo "  sudo install -Dm644 $root/packaging/70-drifttrim-hid.rules /etc/udev/rules.d/70-drifttrim-hid.rules"
echo "  sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=hidraw"
