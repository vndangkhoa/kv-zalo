#!/usr/bin/env bash
# Olaz — 1-Click Linux Installer for Omarchy (Arch Linux) & Ubuntu / Debian
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/vndangkhoa/zalo-swift-re/main/packaging/install.sh | bash
#   curl -fsSL https://git.khoavo.myds.me/vndangkhoa/zalo-swift-re/raw/branch/main/packaging/install.sh | bash
#
# Options:
#   --user              Install portable build into ~/.local (no root required if runtime libs exist)
#   --version <vX.Y.Z>  Install a specific release tag (default: v0.1.0 / latest)
#   --launch            Launch Olaz immediately after installation
set -euo pipefail

DEFAULT_TAG="v0.1.0"
RELEASE_TAG="${OLAZ_RELEASE_TAG:-}"
USER_MODE=0
LAUNCH_AFTER=0
PREFIX_DIR="${PREFIX:-$HOME/.local}"

while [ $# -gt 0 ]; do
  case "$1" in
    --user)
      USER_MODE=1
      shift
      ;;
    --launch)
      LAUNCH_AFTER=1
      shift
      ;;
    --version|--tag)
      RELEASE_TAG="${2:?missing value for $1}"
      shift 2
      ;;
    --prefix)
      PREFIX_DIR="${2:?missing value for $1}"
      USER_MODE=1
      shift 2
      ;;
    -h|--help)
      cat <<'EOF'
Olaz 1-Click Installer (Omarchy / Arch Linux & Ubuntu / Debian)

Usage:
  install.sh [options]

Options:
  --user              Install into ~/.local without root package manager
  --prefix <dir>      Custom user prefix (implies --user, default: ~/.local)
  --version <tag>     Release tag to install (e.g. v0.1.0)
  --launch            Start Olaz after installation completes
  -h, --help          Show this help message
EOF
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

case "$(uname -s)" in
  Linux) ;;
  *)
    echo "Error: packaging/install.sh is for Linux (Omarchy & Ubuntu). On macOS, see README.md." >&2
    exit 1
    ;;
esac

RAW_ARCH="$(uname -m)"
case "$RAW_ARCH" in
  x86_64|amd64)
    DEB_ARCH="amd64"
    ARCH_UNAME="x86_64"
    ;;
  aarch64|arm64)
    DEB_ARCH="arm64"
    ARCH_UNAME="aarch64"
    ;;
  *)
    echo "Error: Unsupported CPU architecture: $RAW_ARCH" >&2
    exit 1
    ;;
esac

fetch_to() {
  local url="$1"
  local dest="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 2 --connect-timeout 10 -o "$dest" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$dest" "$url"
  else
    echo "Error: Neither curl nor wget is installed." >&2
    return 1
  fi
}

resolve_latest_tag() {
  if [ -n "$RELEASE_TAG" ]; then
    case "$RELEASE_TAG" in
      v*) printf '%s\n' "$RELEASE_TAG" ;;
      *)  printf 'v%s\n' "$RELEASE_TAG" ;;
    esac
    return 0
  fi

  local tmp_json
  tmp_json="$(mktemp)"
  for api_url in \
    "https://api.github.com/repos/vndangkhoa/zalo-swift-re/releases/latest" \
    "https://git.khoavo.myds.me/api/v1/repos/vndangkhoa/zalo-swift-re/releases/latest"
  do
    if fetch_to "$api_url" "$tmp_json" 2>/dev/null; then
      local found
      found="$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$tmp_json" | head -n1)"
      if [ -n "$found" ]; then
        rm -f "$tmp_json"
        printf '%s\n' "$found"
        return 0
      fi
    fi
  done
  rm -f "$tmp_json"
  printf '%s\n' "$DEFAULT_TAG"
}

TAG="$(resolve_latest_tag)"
VER="${TAG#v}"

MIRROR_BASES=(
  "https://github.com/vndangkhoa/zalo-swift-re/releases/download/${TAG}"
  "https://git.khoavo.myds.me/vndangkhoa/zalo-swift-re/releases/download/${TAG}"
  "https://gitlab.khoavo.myds.me/vndangkhoa/zalo-swift-re/-/releases/${TAG}/downloads"
)

WORK_DIR="$(mktemp -d)"
chmod 0755 "$WORK_DIR"
trap 'rm -rf "$WORK_DIR"' EXIT

download_asset() {
  local asset="$1"
  local out="$WORK_DIR/$asset"
  for base in "${MIRROR_BASES[@]}"; do
    rm -f "$out"
    if fetch_to "${base}/${asset}" "$out" 2>/dev/null && [ -s "$out" ]; then
      if head -c 96 "$out" | LC_ALL=C grep -qiE '<!doctype html|<html'; then
        rm -f "$out"
        continue
      fi
      printf '%s\n' "$out"
      return 0
    fi
  done
  return 1
}

verify_checksum_if_available() {
  local file_path="$1"
  local asset_name
  asset_name="$(basename -- "$file_path")"
  local sums_path
  if sums_path="$(download_asset "SHA256SUMS.txt" 2>/dev/null)"; then
    local expected
    expected="$(awk -v target="$asset_name" '$2 == target || $2 == ("*" target) { print $1; exit }' "$sums_path")"
    if [ -n "$expected" ] && command -v sha256sum >/dev/null 2>&1; then
      local actual
      actual="$(sha256sum "$file_path" | awk '{print $1}')"
      if [ "$actual" != "$expected" ]; then
        echo "Error: SHA-256 mismatch for $asset_name (expected $expected, got $actual)" >&2
        exit 1
      fi
      echo "==> Verified SHA-256 checksum for $asset_name"
    fi
  fi
}

run_privileged() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    if [ -t 0 ]; then
      sudo "$@"
    elif [ -r /dev/tty ]; then
      sudo "$@" </dev/tty
    else
      sudo -n "$@" || pkexec "$@"
    fi
  elif command -v pkexec >/dev/null 2>&1; then
    pkexec "$@"
  else
    echo "Error: Root privileges required (install sudo or pass --user)." >&2
    exit 1
  fi
}

DISTRO_FAMILY="generic"
if [ -f /etc/os-release ]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  case "${ID:-} ${ID_LIKE:-}" in
    *arch*|*omarchy*|*cachyos*|*endeavouros*|*manjaro*)
      DISTRO_FAMILY="arch"
      ;;
    *ubuntu*|*debian*|*linuxmint*|*pop*|*elementary*|*zorin*|*neon*)
      DISTRO_FAMILY="debian"
      ;;
  esac
fi
if [ "$DISTRO_FAMILY" = "generic" ]; then
  if command -v pacman >/dev/null 2>&1; then
    DISTRO_FAMILY="arch"
  elif command -v apt-get >/dev/null 2>&1; then
    DISTRO_FAMILY="debian"
  fi
fi

echo "==> Installing Olaz ${TAG} (${ARCH_UNAME}, detected platform: ${DISTRO_FAMILY})..."

install_portable_tarball() {
  local tar_name="olaz-${VER}-linux-${ARCH_UNAME}.tar.gz"
  local tar_path
  if ! tar_path="$(download_asset "$tar_name")"; then
    echo "Error: Could not download ${tar_name} for ${TAG}." >&2
    exit 1
  fi
  verify_checksum_if_available "$tar_path"
  tar -xzf "$tar_path" -C "$WORK_DIR"
  local stage_dir="$WORK_DIR/olaz-${VER}-linux-${ARCH_UNAME}"
  PREFIX="$PREFIX_DIR" "$stage_dir/install-user.sh"
}

if [ "$USER_MODE" -eq 1 ]; then
  install_portable_tarball
elif [ "$DISTRO_FAMILY" = "arch" ] && command -v pacman >/dev/null 2>&1; then
  PKG_NAME="olaz-${VER}-1-${ARCH_UNAME}.pkg.tar.zst"
  if PKG_PATH="$(download_asset "$PKG_NAME")"; then
    verify_checksum_if_available "$PKG_PATH"
    echo "==> Installing native Omarchy / Arch package (${PKG_NAME}) via pacman..."
    run_privileged pacman -Sy --needed --noconfirm \
      webkit2gtk-4.1 gtk3 gst-plugins-base gst-plugins-good xdg-desktop-portal 7zip hicolor-icon-theme
    run_privileged pacman -U --noconfirm "$PKG_PATH"
  else
    echo "==> Prebuilt ${PKG_NAME} not found for ${ARCH_UNAME}; falling back to portable install..."
    install_portable_tarball
  fi
elif [ "$DISTRO_FAMILY" = "debian" ] && command -v apt-get >/dev/null 2>&1; then
  DEB_NAME="olaz_${VER}_${DEB_ARCH}.deb"
  if DEB_PATH="$(download_asset "$DEB_NAME")"; then
    verify_checksum_if_available "$DEB_PATH"
    chmod 0644 "$DEB_PATH"
    echo "==> Installing native Ubuntu / Debian package (${DEB_NAME}) via apt..."
    run_privileged apt-get update
    run_privileged env DEBIAN_FRONTEND=noninteractive apt-get install -y "$DEB_PATH"
  else
    echo "==> Prebuilt ${DEB_NAME} not found for ${DEB_ARCH}; falling back to portable install..."
    install_portable_tarball
  fi
else
  echo "==> Non-pacman/apt Linux detected; installing portable user package into ${PREFIX_DIR}..."
  install_portable_tarball
fi

OLAZ_BIN="$(command -v olaz 2>/dev/null || printf '%s/bin/olaz' "$PREFIX_DIR")"
echo ""
echo "==> Olaz ${TAG} is installed and ready!"
echo "    Binary:   ${OLAZ_BIN}"
echo "    Launcher: Search 'Olaz' in your app launcher (Omarchy Super+Space / Ubuntu Activities)"

if [ "$LAUNCH_AFTER" -eq 1 ] && [ -x "$OLAZ_BIN" ]; then
  nohup "$OLAZ_BIN" >/dev/null 2>&1 &
  echo "==> Launched Olaz!"
fi
