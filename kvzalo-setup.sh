#!/usr/bin/env bash
# ==============================================================================
# KV Zalo — Universal Linux Installer (Ubuntu, Debian, Omarchy & Arch Linux)
# https://syno.vndns.net/package/kvzalo
# https://github.com/vndangkhoa/kv-zalo
# ==============================================================================
set -euo pipefail

BASE_URL="https://syno.vndns.net"
PKG_VER="0.2.0-1"
ARCH_PKG="kv-zalo-0.2.0-1-x86_64.pkg.tar.zst"
DEB_PKG="kvzalo_0.2.0-1_amd64.deb"

# Colors
C_BLUE='\033[0;34m'
C_CYAN='\033[0;36m'
C_GREEN='\033[0;32m'
C_YELLOW='\033[1;33m'
C_RED='\033[0;31m'
C_NC='\033[0m'

echo -e "${C_CYAN}================================================================${C_NC}"
echo -e "${C_BLUE}           KV Zalo — Native Linux Client Installer              ${C_NC}"
echo -e "${C_CYAN}================================================================${C_NC}"
echo -e "Phiên bản: ${C_GREEN}v${PKG_VER}${C_NC}"
echo -e "Mã nguồn: ${C_BLUE}https://github.com/vndangkhoa/kv-zalo${C_NC}"
echo -e "Trang tải: ${C_BLUE}${BASE_URL}/package/kvzalo${C_NC}\n"

# Check root / sudo
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then
        SUDO="sudo"
    else
        echo -e "${C_RED}Lỗi: Cần quyền root hoặc lệnh sudo để cài đặt gói.${C_NC}" >&2
        exit 1
    fi
fi

TMP_DIR="$(mktemp -d -t kvzalo-install-XXXXXX)"
trap 'rm -rf "$TMP_DIR"' EXIT

# Detect OS
if [ -f /etc/os-release ]; then
    . /etc/os-release
    DISTRO_ID="${ID:-}"
    DISTRO_LIKE="${ID_LIKE:-}"
else
    DISTRO_ID="unknown"
    DISTRO_LIKE=""
fi

echo -e "Hệ điều hành nhận diện: ${C_YELLOW}${PRETTY_NAME:-$DISTRO_ID}${C_NC}"

if command -v pacman >/dev/null 2>&1 || [[ "$DISTRO_ID" == "arch" || "$DISTRO_LIKE" == *"arch"* ]]; then
    echo -e "${C_CYAN}-> Đang tải gói Arch Linux / Omarchy: ${ARCH_PKG}...${C_NC}"
    curl -fSL "${BASE_URL}/${ARCH_PKG}" -o "${TMP_DIR}/${ARCH_PKG}"
    
    echo -e "${C_CYAN}-> Đang cài đặt bằng pacman...${C_NC}"
    $SUDO pacman -U --noconfirm "${TMP_DIR}/${ARCH_PKG}"

elif command -v dpkg >/dev/null 2>&1 || [[ "$DISTRO_ID" == "ubuntu" || "$DISTRO_ID" == "debian" || "$DISTRO_LIKE" == *"debian"* ]]; then
    echo -e "${C_CYAN}-> Đang tải gói Ubuntu / Debian: ${DEB_PKG}...${C_NC}"
    curl -fSL "${BASE_URL}/${DEB_PKG}" -o "${TMP_DIR}/${DEB_PKG}"
    
    echo -e "${C_CYAN}-> Đang cài đặt bằng dpkg...${C_NC}"
    $SUDO dpkg -i "${TMP_DIR}/${DEB_PKG}" || {
        echo -e "${C_YELLOW}-> Đang sửa các phụ thuộc thiếu sót bằng apt-get...${C_NC}"
        $SUDO apt-get update -qq
        $SUDO apt-get install -f -y
    }
else
    echo -e "${C_YELLOW}Không nhận diện được pacman hoặc dpkg. Thử cài đặt gói Arch (tar.zst) dạng standalone...${C_NC}"
    curl -fSL "${BASE_URL}/${ARCH_PKG}" -o "${TMP_DIR}/${ARCH_PKG}"
    $SUDO tar -I zstd -xvf "${TMP_DIR}/${ARCH_PKG}" -C / usr/
fi

echo -e "\n${C_GREEN}================================================================${C_NC}"
echo -e "${C_GREEN}  Cài đặt KV Zalo thành công!                                    ${C_NC}"
echo -e "${C_GREEN}================================================================${C_NC}"
echo -e "Bạn có thể khởi động KV Zalo bằng cách:"
echo -e "  1. Tìm '${C_CYAN}KV Zalo${C_NC}' trong Application Launcher / menu ứng dụng."
echo -e "  2. Hoặc chạy lệnh trong terminal: ${C_YELLOW}kv-zalo${C_NC}"
echo -e "  3. Hoặc khởi chạy web relay:      ${C_YELLOW}kv-zalo-web --listen 0.0.0.0:8080${C_NC}\n"
