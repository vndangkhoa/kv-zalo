#!/usr/bin/env bash
set -euo pipefail

if [ ! -f /etc/os-release ]; then
  echo "Error: /etc/os-release not found; cannot detect Ubuntu/Debian version." >&2
  exit 1
fi

# shellcheck disable=SC1091
. /etc/os-release

DISTRO_ID="${ID:-unknown}"
VERSION_NUM="${VERSION_ID:-0}"

case "$DISTRO_ID" in
  ubuntu|pop|linuxmint|elementary|zorin|neon|debian)
    ;;
  *)
    echo "Warning: Detected ID='$DISTRO_ID' ($PRETTY_NAME); proceeding with apt-based dependency setup." >&2
    ;;
esac

if [ "$DISTRO_ID" = "ubuntu" ]; then
  MAJOR_VER="${VERSION_NUM%%.*}"
  if [ "$MAJOR_VER" -lt 22 ] 2>/dev/null; then
    echo "Error: Olaz requires Ubuntu 22.04 LTS or newer (found Ubuntu $VERSION_NUM). libwebkit2gtk-4.1-dev is required." >&2
    exit 1
  elif [ "$MAJOR_VER" -eq 22 ]; then
    echo "==> Detected Ubuntu $VERSION_NUM (22.04 Jammy series)"
  else
    echo "==> Detected Ubuntu $VERSION_NUM (24.04 Noble+ series)"
  fi
else
  echo "==> Detected ${PRETTY_NAME:-$DISTRO_ID}"
fi

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

$SUDO apt-get update

SEVENZIP_PKG="7zip"
if ! apt-cache show 7zip >/dev/null 2>&1; then
  SEVENZIP_PKG="p7zip-full"
fi

$SUDO apt-get install -y \
  build-essential \
  pkg-config \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev \
  libavformat-dev \
  libavcodec-dev \
  libswresample-dev \
  libswscale-dev \
  libavutil-dev \
  gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad \
  gstreamer1.0-libav \
  gstreamer1.0-pulseaudio \
  gstreamer1.0-alsa \
  xdg-desktop-portal \
  "$SEVENZIP_PKG"

cat <<'EOF'

==> Ubuntu build & runtime dependencies installed!

Next steps:
  1. Build and run directly:
       ./bin/build
       ./target/release/olaz

  2. Or build and install a native .deb package:
       cargo install cargo-deb
       cargo deb -p olaz-app
       sudo apt install ./target/debian/olaz_*.deb
EOF
