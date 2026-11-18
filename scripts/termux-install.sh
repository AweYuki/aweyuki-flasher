#!/data/data/com.termux/files/usr/bin/bash
set -euo pipefail

echo "=== Aweyuki Flasher — Termux Android-to-Android OTG Setup ==="
pkg update -y
pkg install -y rust git android-tools termux-api libusb

# Install termux-fastboot / termux-adb if available in repo
if pkg search termux-fastboot >/dev/null 2>&1; then
  pkg install -y termux-fastboot termux-adb || true
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SCRIPT_DIR"

echo "Building aweyuki CLI natively for Termux ($(uname -m))..."
cargo build --release -p aweyuki-cli
install -Dm755 target/release/aweyuki "$PREFIX/bin/aweyuki"

echo ""
echo "Installed: $PREFIX/bin/aweyuki"
echo "Quick usage:"
echo "  aweyuki devices"
echo "  aweyuki inspect /sdcard/Download/rom.zip"
echo "  aweyuki flash /sdcard/Download/rom.zip --termux-usb --max-size 128M"
