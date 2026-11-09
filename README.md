# KV Zalo (`kv-zalo`)

**A reverse-engineered, native Linux desktop & zero-persistence self-hosted web client for [Zalo](https://zalo.me) — written in Rust for Linux (Omarchy & Ubuntu) and Docker/Web, with the macOS Swift reference client under `macos/`.**

KV Zalo runs the **official Zalo PC renderer** (`app.asar`) inside a native WebKit host (or browser tab in `kv-zalo-web` mode) and supplies in Rust / Swift everything that Electron's preload and main process provide — without re-implementing Zalo's wire protocol:
- **Linux Desktop (`kv-zalo` — `crates/olaz-app`, `olaz-linux`, `olaz-core`, `olaz-net`)**: frameless GTK3 + `webkit2gtk-4.1` desktop client with pure-Rust 1:1 & group VoIP calls + call recorder, default-enabled anti-recall (**"Giữ thu hồi"**), single-window multi-account switcher (`#main-tab`), GStreamer/FFmpeg media playback, Secret Service vault, StatusNotifierItem tray, local REST API, and stdio MCP server.
- **Zero-Persistence Self-Hosted Web (`kv-zalo-web` — `crates/olaz-web`)**: stateless HTTP/Range/WebSocket relay (`Dockerfile` + `docker-compose.yml` running `--read-only`) where 100% of crypto, vault keys, cookies (`zpw_sek`), OPFS files, and chat state live exclusively in the user's browser.
- **macOS Reference (`macos/`)**: Swift 6 + SwiftUI Liquid Glass client hosting `app.asar` inside `WKWebView`.

Zalo PC does not need to be installed. On first launch KV Zalo downloads the official Zalo PC bundle from VNG, verifies its SHA-256 / code signature, and caches `app.asar` locally (`~/.local/share/olaz/ZaloRuntime/` on Linux).

> [!IMPORTANT]
> **Unofficial project, not affiliated with or endorsed by VNG Corporation.** "Zalo" is a trademark of VNG.
> This is an interoperability and reverse-engineering research project. Using an unofficial client may
> conflict with Zalo's terms of service, so use it at your own risk with your own account.

---

## Repository Layout

| Path | Purpose |
|---|---|
| `crates/olaz-core/` | `mmap` ASAR reader, pinned VNG runtime installer, Node-parity `$znode.crypto`, `ZDB4.0` (`db-cross-v4`), `zwalker`, `file-utilities`, keyring vault, profile registry, and embedded `assets/` (`bridge-table.json` + `host_scripts/*.js`) |
| `crates/olaz-net/` | `reqwest` (SOCKS5/HTTP `--proxy`) + `tokio-tungstenite` network proxy, `0600` cookie jar, DoH (`*.zadn.vn`), streaming `AES-GCM` / `AES-CTR` media |
| `crates/olaz-linux/` | Pure-Rust VoIP engine (`call/`: Opus 16 kHz, stereo WAV / Ogg-Opus call recorder, H.264 camera & screen share, Zavi/SRTP), `libgstolazauto.so`, `ksni` tray, privacy & anti-recall settings, scheduler/auto-reply, REST & MCP |
| `crates/olaz-app/` | Native GTK3 + `webkit2gtk-4.1` multi-account desktop app (`kv-zalo`, symlinked as `olaz`) |
| `crates/olaz-web/` | Zero-server-persistence self-hosted web relay (`kv-zalo-web`) & browser-only preload runtime |
| `bin/` | Developer helper scripts (`./bin/build`, `./bin/install`, `./bin/test`) |
| `packaging/` | 1-click Linux installer (`install.sh`), `.deb`/`.pkg.tar.zst`/`.run` builder, Flatpak manifest, `kv-zalo-web.service` |
| `pkgbuild/` | Arch Linux / Omarchy `PKGBUILD`, `kv-zalo.desktop` launcher, and hicolor SVG icon |
| `Dockerfile`, `docker-compose.yml` | Hardened `--read-only` zero-persistence container deployment (`kv-zalo:latest` / `kv-zalo-web`) |
| `docs/` | [`REVERSE-ENGINEERING-NOTES.md`](docs/REVERSE-ENGINEERING-NOTES.md) and [`reanalysis/`](docs/reanalysis/) IPC bridge specification |
| `macos/` | Self-contained macOS Swift 6 / SwiftUI client (`Package.swift`, `Sources/`, `Tests/`, `Assets/`, `scripts/`) |

---

## 1. Linux Desktop (`kv-zalo` — Omarchy & Ubuntu)

### Key Features
- **Multi-Account Workspace**: Add accounts straight from Zalo's left `#main-tab` blue sidebar (`+` button or `Ctrl+Shift+N`), switch in `0 ms` (`Ctrl+1..9` / `Ctrl+Tab`), with isolated cookies, IndexedDB, keyring secrets, and unread badges.
- **Anti-Recall ("Giữ thu hồi") — Enabled by Default**: Preserves recalled text, quotes, and cached media with a `Đã thu hồi` badge; toggle anytime from any chat bubble's **`...` → `Giữ thu hồi`** menu or the System Tray.
- **Pure-Rust VoIP Calls & Call Recorder**: 1:1 & group calls with in-call **Record** button (`● REC MM:SS`, stereo WAV or Ogg/Opus) and configurable save folder under **Cài đặt → Cài đặt cuộc gọi → Ghi âm cuộc gọi**.
- **Power Tools**: Outbound typing/seen/delivered receipt blockers, message scheduler, Vietnamese accent-folded auto-reply, SOCKS5/HTTP `--proxy`, local loopback REST API (`--api`), and stdio MCP server (`--mcp`).

### 1-Click Install (Prebuilt Releases)

```bash
# GitHub:
curl -fsSL https://raw.githubusercontent.com/vndangkhoa/kv-zalo/main/packaging/install.sh | bash

# Forgejo mirror:
curl -fsSL https://git.khoavo.myds.me/vndangkhoa/kv-zalo/raw/branch/main/packaging/install.sh | bash
```
*(Pass `... | bash -s -- --user` to install into `~/.local` without `sudo`, or download `.deb` / `.pkg.tar.zst` / `.run` packages from [GitHub Releases](https://github.com/vndangkhoa/kv-zalo/releases) · [Forgejo Releases](https://git.khoavo.myds.me/vndangkhoa/kv-zalo/releases) · [GitLab Releases](https://gitlab.khoavo.myds.me/vndangkhoa/kv-zalo/-/releases).)*

### Build from Source (Linux)

```bash
# Omarchy / Arch Linux dependencies:
sudo pacman -S --needed rust pkgconf webkit2gtk-4.1 gtk3 gstreamer gst-plugins-base opus libpulse ffmpeg xdg-desktop-portal 7zip

# Ubuntu 22.04 / 24.04 LTS+ dependencies:
./packaging/ubuntu-deps.sh

# Build & install into ~/.local (no sudo required):
./bin/build
./bin/install --user
~/.local/bin/kv-zalo
```

### Linux CLI & Tests

```bash
kv-zalo --profile work                     # launch isolated standalone profile window
kv-zalo --proxy socks5h://127.0.0.1:1080   # route traffic & DoH over SOCKS5 / HTTP proxy
kv-zalo --api                              # enable loopback REST API on 127.0.0.1:17890
kv-zalo --mcp                              # run JSON-RPC 2.0 MCP stdio server
kv-zalo --check-update                     # check GitHub & Forgejo releases (--update to install)

./bin/test                                 # run all 133 workspace unit, Node parity & E2E tests
```

---

## 2. Self-Hosted Zero-Persistence Web (`kv-zalo-web`)

`kv-zalo-web` lets you host Zalo in any modern HTTPS browser while storing **zero user data on the server**:
- `$znode.crypto`, `SafeStorage`, `KeychainCipher`, `BrowserCookieJar` (`zpw_sek`), IndexedDB, and virtual OPFS files run **100% inside the browser**.
- The Rust server runs with a **read-only filesystem** (`--read-only` / `ReadOnlyPaths=/`), acting purely as an allowlisted stateless HTTP/Range/WebSocket pass-through to `*.zalo.me` / `*.zadn.vn`.
- Supports multi-account tabs via `?profile=work` / `?profile=personal` and optional local `SubtleCrypto` `PBKDF2-SHA256` → `AES-256-GCM` PIN lock.

```bash
# Turnkey read-only Docker Compose stack (auto-fetches & SHA-256 verifies app.asar once):
KVZALO_WEB_TOKEN="replace-with-strong-secret" docker compose up -d --build
```
See [`Dockerfile`](Dockerfile), [`docker-compose.yml`](docker-compose.yml), and [`packaging/kv-zalo-web.service`](packaging/kv-zalo-web.service).

---

## 3. macOS Reference Client (`macos/`)

Requires macOS 26+ and Xcode 27 (Swift 6 toolchain):

```bash
cd macos
swift test                     # crypto parity (vs Node), ASAR reader, JSC/libsignal & host-script tests
./scripts/make-app.sh debug    # → macos/build/Olaz.app
open build/Olaz.app
```

---

## Documentation & Reverse-Engineering Notes

- [`docs/REVERSE-ENGINEERING-NOTES.md`](docs/REVERSE-ENGINEERING-NOTES.md): how official Zalo PC (Electron 22) is structured and hosted outside Electron.
- [`docs/reanalysis/bridge-spec.md`](docs/reanalysis/bridge-spec.md) & [`docs/reanalysis/bridge-table.json`](docs/reanalysis/bridge-table.json): preload bridge (`window.$z*`) and 450-channel IPC specification.
- Based on reverse-engineering research from [`qnguyen3/zalo-swift-re`](https://github.com/qnguyen3/zalo-swift-re).
- Vendored GPL-3.0 third-party file (used only by standalone macOS `JSBridge` tests): [`macos/Sources/Olaz/JSModules/vendor/libsignal-protocol.js`](macos/Sources/Olaz/JSModules/vendor/libsignal-protocol.js).

## License

KV Zalo is free software, licensed under the [GNU General Public License v3.0](LICENSE).
