#!/usr/bin/env bash
# Builds all 1-click Linux release artifacts for Omarchy (Arch Linux) & Ubuntu / Debian:
#   - olaz_<ver>_<deb_arch>.deb                (Native Ubuntu 22.04 / 24.04+ / Debian package)
#   - olaz-<ver>-1-<arch>.pkg.tar.zst          (Native Omarchy / Arch Linux pacman package)
#   - olaz-setup-linux-<arch>.run              (Universal 1-click self-extracting setup file)
#   - olaz-<ver>-linux-<arch>.tar.gz           (Portable rootless tarball + install-user.sh)
#   - install.sh                               (1-line curl | bash bootstrap installer)
#   - SHA256SUMS.txt                           (SHA-256 checksums for all release files)
set -euo pipefail

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

SKIP_BUILD=0
VERSION_OVERRIDE=""
PREBUILT_ARCH_PKG="${OLAZ_PREBUILT_ARCH_PKG:-}"
PREBUILT_DEB_PKG="${OLAZ_PREBUILT_DEB_PKG:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    --version)
      VERSION_OVERRIDE="${2:?missing value for --version}"
      shift 2
      ;;
    --arch-pkg)
      PREBUILT_ARCH_PKG="${2:?missing value for --arch-pkg}"
      shift 2
      ;;
    --deb-pkg)
      PREBUILT_DEB_PKG="${2:?missing value for --deb-pkg}"
      shift 2
      ;;
    *)
      echo "Unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

WORKSPACE_VER="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$ROOT/Cargo.toml" | head -n1)"
VER="${VERSION_OVERRIDE:-${OLAZ_VERSION:-$WORKSPACE_VER}}"
VER="${VER#v}"
TAG="v${VER}"

RAW_ARCH="$(uname -m)"
case "$RAW_ARCH" in
  x86_64|amd64)
    ARCH_UNAME="x86_64"
    DEB_ARCH="amd64"
    ;;
  aarch64|arm64)
    ARCH_UNAME="aarch64"
    DEB_ARCH="arm64"
    ;;
  *)
    echo "Error: Unsupported architecture: $RAW_ARCH" >&2
    exit 1
    ;;
esac

if [ "$SKIP_BUILD" -eq 0 ]; then
  echo "==> Building release binary (olaz ${VER})..."
  "$ROOT/bin/build"
fi

BIN_PATH="$ROOT/target/release/olaz"
if [ ! -x "$BIN_PATH" ]; then
  echo "Error: Release binary not found at $BIN_PATH" >&2
  exit 1
fi

OUT_DIR="$ROOT/dist/${TAG}"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

# Helper: populate usr/ tree (shared by .deb, portable .tar.gz, and fallback Arch package)
populate_usr_tree() {
  local usr_root="$1"
  install -Dm755 "$BIN_PATH" "$usr_root/bin/olaz"
  strip --strip-unneeded "$usr_root/bin/olaz" 2>/dev/null || true
  install -Dm644 "$ROOT/pkgbuild/olaz.desktop" "$usr_root/share/applications/olaz.desktop"
  install -Dm644 "$ROOT/pkgbuild/olaz.svg" "$usr_root/share/icons/hicolor/scalable/apps/olaz.svg"
  if command -v rsvg-convert >/dev/null 2>&1; then
    for size in 32 48 64 128 256 512; do
      install -dm755 "$usr_root/share/icons/hicolor/${size}x${size}/apps"
      rsvg-convert -w "$size" -h "$size" "$ROOT/pkgbuild/olaz.svg" \
        -o "$usr_root/share/icons/hicolor/${size}x${size}/apps/olaz.png"
    done
  fi
  install -Dm644 "$ROOT/LICENSE" "$usr_root/share/licenses/olaz/LICENSE"
  install -Dm644 "$ROOT/LICENSE" "$usr_root/share/doc/olaz/copyright"
}

# ── 1. Ubuntu / Debian native .deb package ───────────────────────────────────
DEB_FILENAME="olaz_${VER}_${DEB_ARCH}.deb"
DEB_OUT="$OUT_DIR/$DEB_FILENAME"

if [ -n "$PREBUILT_DEB_PKG" ] && [ -f "$PREBUILT_DEB_PKG" ]; then
  cp -f "$PREBUILT_DEB_PKG" "$DEB_OUT"
  echo "==>Copied prebuilt Debian/Ubuntu package: $DEB_OUT"
else
  echo "==> Building Ubuntu / Debian package (${DEB_FILENAME})..."
  DEB_STAGE="$WORK_DIR/deb-root"
  mkdir -p "$DEB_STAGE/DEBIAN"
  populate_usr_tree "$DEB_STAGE/usr"

  INSTALLED_KB="$(du -sk "$DEB_STAGE/usr" | awk '{print $1}')"
  cat > "$DEB_STAGE/DEBIAN/control" <<EOF
Package: olaz
Version: ${VER}-1
Section: net
Priority: optional
Architecture: ${DEB_ARCH}
Installed-Size: ${INSTALLED_KB}
Maintainer: Olaz Contributors <noreply@olaz.app>
Homepage: https://github.com/vndangkhoa/zalo-swift-re
Depends: libc6 (>= 2.35), libwebkit2gtk-4.1-0, libjavascriptcoregtk-4.1-0, libgtk-3-0t64 | libgtk-3-0, libsoup-3.0-0, libasound2t64 | libasound2, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-libav, gstreamer1.0-pipewire | gstreamer1.0-pulseaudio | gstreamer1.0-alsa, xdg-desktop-portal, 7zip | p7zip-full
Description: Native Linux desktop client for Zalo (Omarchy & Ubuntu)
 Olaz is a native Linux desktop client for Zalo built in Rust on WebKit2GTK 4.1.
 It mounts the official Zalo PC interface with frameless desktop integration,
 inline GStreamer media playback, E2EE, phone sync, system tray, autostart,
 and XDG Desktop Portal support.
EOF

  cat > "$DEB_STAGE/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if [ "$1" = "configure" ]; then
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -f -t /usr/share/icons/hicolor || true
  fi
fi
exit 0
EOF

  cat > "$DEB_STAGE/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q -f -t /usr/share/icons/hicolor || true
fi
exit 0
EOF

  chmod 0755 "$DEB_STAGE/DEBIAN/postinst" "$DEB_STAGE/DEBIAN/postrm"
  dpkg-deb --build --root-owner-group -Zxz "$DEB_STAGE" "$DEB_OUT" >/dev/null
fi

# ── 2. Omarchy / Arch Linux native .pkg.tar.zst package ──────────────────────
ARCH_FILENAME="olaz-${VER}-1-${ARCH_UNAME}.pkg.tar.zst"
ARCH_OUT="$OUT_DIR/$ARCH_FILENAME"

if [ -n "$PREBUILT_ARCH_PKG" ] && [ -f "$PREBUILT_ARCH_PKG" ]; then
  cp -f "$PREBUILT_ARCH_PKG" "$ARCH_OUT"
  echo "==> Copied prebuilt Omarchy/Arch package: $ARCH_OUT"
elif command -v makepkg >/dev/null 2>&1 && [ "$(id -u)" -ne 0 ]; then
  echo "==> Building Omarchy / Arch package (${ARCH_FILENAME}) via makepkg..."
  MP_DIR="$WORK_DIR/pkgbuild"
  mkdir -p "$MP_DIR"
  cp "$ROOT/pkgbuild/olaz.desktop" "$ROOT/pkgbuild/olaz.svg" "$MP_DIR/"
  cp "$ROOT/LICENSE" "$MP_DIR/LICENSE"
  cp "$BIN_PATH" "$MP_DIR/olaz.bin"
  strip --strip-unneeded "$MP_DIR/olaz.bin" 2>/dev/null || true

  cat > "$MP_DIR/PKGBUILD" <<EOF
# Maintainer: Olaz Contributors <noreply@olaz.app>
pkgname=olaz
pkgver=${VER}
pkgrel=1
pkgdesc='Native Zalo desktop client for Linux (Omarchy & Ubuntu) built in Rust on WebKit2GTK'
arch=('${ARCH_UNAME}')
url='https://github.com/vndangkhoa/zalo-swift-re'
license=('GPL-3.0-only')
depends=(
  'webkit2gtk-4.1'
  'gtk3'
  'gst-plugins-base'
  'gst-plugins-good'
  'xdg-desktop-portal'
  '7zip'
  'hicolor-icon-theme'
)
optdepends=(
  'gst-plugins-bad: WebRTC, WebP, and AVIF media codecs'
  'gst-libav: FFmpeg H.264/AAC video playback in chats and calls'
)
options=('!lto' '!debug' '!strip')
source=()
sha256sums=()

package() {
  install -Dm755 "\$startdir/olaz.bin" "\$pkgdir/usr/bin/olaz"
  install -Dm644 "\$startdir/olaz.desktop" "\$pkgdir/usr/share/applications/olaz.desktop"
  install -Dm644 "\$startdir/olaz.svg" "\$pkgdir/usr/share/icons/hicolor/scalable/apps/olaz.svg"
  if command -v rsvg-convert >/dev/null 2>&1; then
    for size in 32 48 64 128 256 512; do
      install -dm755 "\$pkgdir/usr/share/icons/hicolor/\${size}x\${size}/apps"
      rsvg-convert -w "\$size" -h "\$size" "\$startdir/olaz.svg" \
        -o "\$pkgdir/usr/share/icons/hicolor/\${size}x\${size}/apps/olaz.png"
    done
  fi
  install -Dm644 "\$startdir/LICENSE" "\$pkgdir/usr/share/licenses/olaz/LICENSE"
}
EOF

  (
    cd "$MP_DIR"
    PKGEXT='.pkg.tar.zst' makepkg -fd --noconfirm >/dev/null
  )
  cp -f "$MP_DIR/${ARCH_FILENAME}" "$ARCH_OUT"
else
  echo "==> Building Omarchy / Arch package (${ARCH_FILENAME}) via bsdtar/tar + zstd..."
  ARCH_STAGE="$WORK_DIR/arch-root"
  mkdir -p "$ARCH_STAGE"
  populate_usr_tree "$ARCH_STAGE/usr"
  rm -rf "$ARCH_STAGE/usr/share/doc"
  ARCH_BYTES="$(du -sb "$ARCH_STAGE/usr" | awk '{print $1}')"
  BUILD_DATE="$(date +%s)"
  cat > "$ARCH_STAGE/.PKGINFO" <<EOF
pkgname = olaz
pkgbase = olaz
pkgver = ${VER}-1
pkgdesc = Native Zalo desktop client for Linux (Omarchy & Ubuntu) built in Rust on WebKit2GTK
url = https://github.com/vndangkhoa/zalo-swift-re
builddate = ${BUILD_DATE}
packager = Olaz Contributors <noreply@olaz.app>
size = ${ARCH_BYTES}
arch = ${ARCH_UNAME}
license = GPL-3.0-only
depend = webkit2gtk-4.1
depend = gtk3
depend = gst-plugins-base
depend = gst-plugins-good
depend = xdg-desktop-portal
depend = 7zip
depend = hicolor-icon-theme
optdepend = gst-plugins-bad: WebRTC, WebP, and AVIF media codecs
optdepend = gst-libav: FFmpeg H.264/AAC video playback in chats and calls
EOF
  (
    cd "$ARCH_STAGE"
    if command -v bsdtar >/dev/null 2>&1; then
      LANG=C bsdtar --uid 0 --gid 0 -cf - .PKGINFO usr | zstd -q -19 -o "$ARCH_OUT"
    else
      LC_ALL=C tar --owner=0 --group=0 -cf - .PKGINFO usr | zstd -q -19 -o "$ARCH_OUT"
    fi
  )
fi

# ── 3. Portable tarball (rootless user setup) ────────────────────────────────
PORTABLE_NAME="olaz-${VER}-linux-${ARCH_UNAME}"
PORTABLE_FILENAME="${PORTABLE_NAME}.tar.gz"
PORTABLE_OUT="$OUT_DIR/$PORTABLE_FILENAME"
PORTABLE_STAGE="$WORK_DIR/$PORTABLE_NAME"
mkdir -p "$PORTABLE_STAGE"
populate_usr_tree "$PORTABLE_STAGE"
rm -rf "$PORTABLE_STAGE/share/doc"

cat > "$PORTABLE_STAGE/install-user.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
SELF_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"

install -Dm755 "$SELF_DIR/bin/olaz" "$PREFIX/bin/olaz"
mkdir -p "$PREFIX/share/applications"
sed "s|^Exec=olaz %u$|Exec=${PREFIX}/bin/olaz %u\nTryExec=${PREFIX}/bin/olaz|" \
  "$SELF_DIR/share/applications/olaz.desktop" > "$PREFIX/share/applications/olaz.desktop"
chmod 0644 "$PREFIX/share/applications/olaz.desktop"

cp -r "$SELF_DIR/share/icons" "$PREFIX/share/"
install -Dm644 "$SELF_DIR/share/licenses/olaz/LICENSE" "$PREFIX/share/licenses/olaz/LICENSE"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q "$PREFIX/share/applications" || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q -f -t "$PREFIX/share/icons/hicolor" || true
fi
echo "Installed Olaz into $PREFIX"
EOF
chmod 0755 "$PORTABLE_STAGE/install-user.sh"

tar -C "$WORK_DIR" --owner=0 --group=0 -czf "$PORTABLE_OUT" "$PORTABLE_NAME"

# ── 4. Universal 1-Click Self-Extracting Setup File (.run) ───────────────────
RUN_FILENAME="olaz-setup-linux-${ARCH_UNAME}.run"
RUN_OUT="$OUT_DIR/$RUN_FILENAME"
BUNDLE_STAGE="$WORK_DIR/run-payload"
mkdir -p "$BUNDLE_STAGE"
cp -f "$DEB_OUT" "$BUNDLE_STAGE/$DEB_FILENAME"
cp -f "$ARCH_OUT" "$BUNDLE_STAGE/$ARCH_FILENAME"
cp -r "$PORTABLE_STAGE" "$BUNDLE_STAGE/portable"

PAYLOAD_TAR="$WORK_DIR/payload.tar.gz"
tar -C "$BUNDLE_STAGE" --owner=0 --group=0 -czf "$PAYLOAD_TAR" .
PAYLOAD_SHA256="$(sha256sum "$PAYLOAD_TAR" | awk '{print $1}')"

cat > "$WORK_DIR/run-header.sh" <<EOF
#!/usr/bin/env bash
# Olaz ${TAG} — Universal 1-Click Offline Setup for Omarchy (Arch Linux) & Ubuntu / Debian
set -euo pipefail

OLAZ_VER="${VER}"
OLAZ_ARCH="${ARCH_UNAME}"
DEB_FILE="${DEB_FILENAME}"
ARCH_FILE="${ARCH_FILENAME}"
EXPECTED_SHA256="${PAYLOAD_SHA256}"

USER_MODE=0
ASSUME_YES=0
CHECK_ONLY=0
LAUNCH_AFTER=0
EXTRACT_TO=""
PREFIX_DIR="\${PREFIX:-\$HOME/.local}"

while [ \$# -gt 0 ]; do
  case "\$1" in
    -y|--yes|--noconfirm) ASSUME_YES=1; shift ;;
    --user) USER_MODE=1; shift ;;
    --prefix) PREFIX_DIR="\${2:?missing dir}"; USER_MODE=1; shift 2 ;;
    --launch) LAUNCH_AFTER=1; shift ;;
    --check) CHECK_ONLY=1; shift ;;
    --extract) EXTRACT_TO="\${2:?missing dir}"; shift 2 ;;
    -h|--help)
      cat <<'HELP'
Olaz Universal 1-Click Setup (Omarchy / Arch Linux & Ubuntu / Debian)

Usage:
  ./${RUN_FILENAME} [options]

Options:
  -y, --yes         Non-interactive system install (auto-detects Omarchy/pacman or Ubuntu/apt)
  --user            Install into ~/.local without root privileges
  --prefix <dir>    Custom prefix directory (implies --user)
  --launch          Launch Olaz after installation completes
  --check           Verify embedded payload SHA-256 checksum and exit
  --extract <dir>   Extract embedded .deb, .pkg.tar.zst, and portable files to <dir>
  -h, --help        Show this help message
HELP
      exit 0
      ;;
    *)
      echo "Unknown option: \$1" >&2
      exit 1
      ;;
  esac
done

# If double-clicked from a graphical file manager with no terminal attached and no pkexec/zenity,
# re-open inside the user's desktop terminal so sudo password prompt is visible.
if [ ! -t 0 ] && [ ! -t 1 ] && [ "\$USER_MODE" -eq 0 ] && [ "\$CHECK_ONLY" -eq 0 ] && [ -z "\$EXTRACT_TO" ] && [ "\$(id -u)" -ne 0 ]; then
  if ! command -v pkexec >/dev/null 2>&1; then
    SELF_ABS="\$(CDPATH= cd -- "\$(dirname -- "\$0")" && pwd)/\$(basename -- "\$0")"
    for term in xdg-terminal-exec ghostty alacritty kitty gnome-terminal konsole xfce4-terminal xterm; do
      if command -v "\$term" >/dev/null 2>&1; then
        case "\$term" in
          gnome-terminal) exec "\$term" -- bash -c "\"\$SELF_ABS\"; echo; read -rp 'Press Enter to close...' _" ;;
          *) exec "\$term" -e bash -c "\"\$SELF_ABS\"; echo; read -rp 'Press Enter to close...' _" ;;
        esac
      fi
    done
  elif command -v zenity >/dev/null 2>&1 && [ "\$ASSUME_YES" -eq 0 ]; then
    zenity --question \
      --title="Install Olaz \${OLAZ_VER}" \
      --text="Install Olaz \${OLAZ_VER} (Native Zalo client for Omarchy & Ubuntu)?" \
      --ok-label="Install" --cancel-label="Cancel" || exit 0
    ASSUME_YES=1
  fi
fi

ARCHIVE_LINE="\$(awk '/^__OLAZ_PAYLOAD_BELOW__\$/ { print NR + 1; exit 0; }' "\$0")"
TMP_EXTRACT="\$(mktemp -d)"
trap 'rm -rf "\$TMP_EXTRACT"' EXIT

tail -n +"\$ARCHIVE_LINE" "\$0" > "\$TMP_EXTRACT/payload.tar.gz"
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL_SHA256="\$(sha256sum "\$TMP_EXTRACT/payload.tar.gz" | awk '{print \$1}')"
  if [ "\$ACTUAL_SHA256" != "\$EXPECTED_SHA256" ]; then
    echo "Error: Embedded payload SHA-256 mismatch (expected \$EXPECTED_SHA256, got \$ACTUAL_SHA256)" >&2
    exit 1
  fi
fi

if [ "\$CHECK_ONLY" -eq 1 ]; then
  echo "OK: ${RUN_FILENAME} payload SHA-256 verified (\$EXPECTED_SHA256)"
  exit 0
fi

if [ -n "\$EXTRACT_TO" ]; then
  mkdir -p "\$EXTRACT_TO"
  tar -xzf "\$TMP_EXTRACT/payload.tar.gz" -C "\$EXTRACT_TO"
  echo "Extracted payload into \$EXTRACT_TO"
  exit 0
fi

tar -xzf "\$TMP_EXTRACT/payload.tar.gz" -C "\$TMP_EXTRACT"
chmod -R a+rX "\$TMP_EXTRACT"

run_root() {
  if [ "\$(id -u)" -eq 0 ]; then
    "\$@"
  elif [ -t 0 ] && command -v sudo >/dev/null 2>&1; then
    sudo "\$@"
  elif command -v pkexec >/dev/null 2>&1; then
    pkexec "\$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "\$@"
  else
    echo "Error: Root privileges required (or re-run with --user)." >&2
    exit 1
  fi
}

DISTRO_FAMILY="generic"
if [ -f /etc/os-release ]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  case "\${ID:-} \${ID_LIKE:-}" in
    *arch*|*omarchy*|*cachyos*|*endeavouros*|*manjaro*) DISTRO_FAMILY="arch" ;;
    *ubuntu*|*debian*|*linuxmint*|*pop*|*elementary*|*zorin*|*neon*) DISTRO_FAMILY="debian" ;;
  esac
fi
if [ "\$DISTRO_FAMILY" = "generic" ]; then
  if command -v pacman >/dev/null 2>&1; then DISTRO_FAMILY="arch"
  elif command -v apt-get >/dev/null 2>&1; then DISTRO_FAMILY="debian"
  fi
fi

if [ "\$USER_MODE" -eq 1 ]; then
  PREFIX="\$PREFIX_DIR" "\$TMP_EXTRACT/portable/install-user.sh"
elif [ "\$DISTRO_FAMILY" = "arch" ] && command -v pacman >/dev/null 2>&1; then
  echo "==> Installing Olaz \${OLAZ_VER} on Omarchy / Arch Linux via pacman..."
  run_root sh -c "pacman -Sy --needed --noconfirm webkit2gtk-4.1 gtk3 gst-plugins-base gst-plugins-good xdg-desktop-portal 7zip hicolor-icon-theme && pacman -U --noconfirm '\$TMP_EXTRACT/\$ARCH_FILE'"
elif [ "\$DISTRO_FAMILY" = "debian" ] && command -v apt-get >/dev/null 2>&1; then
  echo "==> Installing Olaz \${OLAZ_VER} on Ubuntu / Debian via apt..."
  run_root env DEBIAN_FRONTEND=noninteractive sh -c "apt-get update && apt-get install -y '\$TMP_EXTRACT/\$DEB_FILE'"
else
  echo "==> Installing portable Olaz \${OLAZ_VER} into \${PREFIX_DIR}..."
  PREFIX="\$PREFIX_DIR" "\$TMP_EXTRACT/portable/install-user.sh"
fi

OLAZ_BIN="\$(command -v olaz 2>/dev/null || printf '%s/bin/olaz' "\$PREFIX_DIR")"
echo "==> Olaz \${OLAZ_VER} installed successfully! (\${OLAZ_BIN})"

if [ ! -t 0 ] && [ ! -t 1 ] && command -v zenity >/dev/null 2>&1 && [ "\$LAUNCH_AFTER" -eq 0 ]; then
  if zenity --question --title="Olaz Installed" --text="Olaz \${OLAZ_VER} is installed! Launch Olaz now?" --ok-label="Launch" --cancel-label="Close"; then
    LAUNCH_AFTER=1
  fi
fi

if [ "\$LAUNCH_AFTER" -eq 1 ] && [ -x "\$OLAZ_BIN" ]; then
  nohup "\$OLAZ_BIN" >/dev/null 2>&1 &
fi
exit 0
__OLAZ_PAYLOAD_BELOW__
EOF

cat "$WORK_DIR/run-header.sh" "$PAYLOAD_TAR" > "$RUN_OUT"
chmod 0755 "$RUN_OUT"

# ── 5. Copy 1-line bootstrap install.sh with pinned DEFAULT_TAG ──────────────
sed "s|^DEFAULT_TAG=.*|DEFAULT_TAG=\"${TAG}\"|" "$ROOT/packaging/install.sh" > "$OUT_DIR/install.sh"
chmod 0755 "$OUT_DIR/install.sh"

# ── 6. Generate SHA256SUMS.txt ───────────────────────────────────────────────
(
  cd "$OUT_DIR"
  sha256sum \
    "$DEB_FILENAME" \
    "$ARCH_FILENAME" \
    "$RUN_FILENAME" \
    "$PORTABLE_FILENAME" \
    "install.sh" > SHA256SUMS.txt
)

echo ""
echo "==> Built all Linux 1-click release artifacts in ${OUT_DIR}:"
ls -lh "$OUT_DIR"
echo ""
cat "$OUT_DIR/SHA256SUMS.txt"
