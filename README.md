<p align="center">
  <img src="pkgbuild/kv-zalo.svg" alt="KV-Zalo Logo" width="100" height="100" style="border-radius: 22px; box-shadow: 0 10px 30px rgba(0,104,255,0.35);">
</p>

<h1 align="center">💬 KV-Zalo</h1>

<p align="center">
  <strong>Native Linux Desktop Client & Zero-Storage Self-Hosted Web Relay for Zalo.</strong><br>
  Pure-Rust 1:1 & group VoIP calls with stereo call recording, default anti-recall (<i>"Giữ thu hồi"</i>), single-window multi-account switcher, and a stateless <code>--read-only</code> browser relay.<br>
  <i>Powered by Rust, GTK3 + WebKit2GTK 4.1, GStreamer/Opus, and a 100% client-side IndexedDB/OPFS web runtime.</i>
</p>

<p align="center">
  <a href="https://github.com/vndangkhoa/kv-zalo/stargazers"><img src="https://img.shields.io/github/stars/vndangkhoa/kv-zalo?style=for-the-badge&logo=apachespark&color=f59e0b" alt="GitHub Stars"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Core-Rust_2021-DEA584?style=for-the-badge&logo=rust&logoColor=black" alt="Rust"></a>
  <a href="#-quick-start-30-seconds"><img src="https://img.shields.io/badge/Linux-Omarchy_%7C_Ubuntu-2563eb?style=for-the-badge&logo=linux&logoColor=white" alt="Linux Desktop"></a>
  <a href="#-docker--nas-deployment"><img src="https://img.shields.io/badge/Self--Host-Zero--Storage_Docker-06b6d4?style=for-the-badge&logo=docker&logoColor=white" alt="Zero-Storage Docker"></a>
  <a href="https://git.khoavo.myds.me/vndangkhoa/bla"><img src="https://img.shields.io/badge/Counter--REA-BLA_v3.2_Protected-7c3aed?style=for-the-badge&logo=shield&logoColor=white" alt="BLA Counter-REA Protected"></a>
  <a href="https://git.khoavo.myds.me/vndangkhoa/kv-zalo"><img src="https://img.shields.io/badge/Forgejo-Mirror-FF5722?style=for-the-badge&logo=git" alt="Forgejo Mirror"></a>
  <a href="#-license"><img src="https://img.shields.io/badge/License-GPL--3.0-gray?style=for-the-badge" alt="License GPL-3.0"></a>
</p>

<p align="center">
  <a href="#-quick-start-30-seconds"><b>Quick Start</b></a> •
  <a href="#-why-kv-zalo"><b>Why KV-Zalo?</b></a> •
  <a href="#-competitive-comparison"><b>Comparison</b></a> •
  <a href="#-killer-features"><b>Features</b></a> •
  <a href="#-docker--nas-deployment"><b>Docker & NAS</b></a> •
  <a href="#-architecture"><b>Architecture</b></a> •
  <a href="#-configuration--cli"><b>Config & CLI</b></a> •
  <a href="#-support--star-history"><b>Star History</b></a>
</p>

---

## ⚡ Why KV-Zalo?

Zalo has no official Linux desktop client. Running Zalo PC under Wine/Bottles breaks Wayland fractional scaling, notifications, and audio devices; basic `chat.zalo.me` web wrappers lack full Zalo PC storage (`ZDB4.0`), lose recalled messages, allow only one account per window, and leak session cookies onto server disks when self-hosted.

**KV-Zalo** hosts the **official VNG Zalo PC bundle (`app.asar`)** directly inside a native Rust runtime—delivering first-class Linux desktop performance and a hardened zero-persistence web relay without re-implementing Zalo's wire protocol:

- 📞 **Pure-Rust VoIP & Call Recorder**: Native 1:1 & group audio/video/screen-share calls (`Opus 16 kHz`, `H.264`, `Zavi/SRTP`) with built-in stereo `WAV` / `Ogg-Opus` call recording and customizable save path.
- 🛡️ **Default Anti-Recall ("Giữ thu hồi")**: Automatically preserves recalled text, quotes, and cached attachments in-place with a `Đã thu hồi` badge—toggleable anytime from any chat bubble's `...` menu.
- 👥 **Native Multi-Account Sidebar**: Switch between unlimited personal & work Zalo accounts in `0 ms` directly inside Zalo's blue `#main-tab` sidebar (`+` button or <kbd>Ctrl</kbd>+<kbd>1..9</kbd>) with isolated cookies, IndexedDB, and keyring secrets.
- 🌐 **Zero-Server-Persistence Web & Mobile PWA (`kv-zalo-web`)**: Installable Progressive Web App with responsive mobile master-detail chat layout, OS/lock-screen Media Session + in-app Now-Playing Media Card, and `read_only: true` Docker execution—`100%` of cookies (`zpw_sek`), E2EE keys, OPFS files, and chat history stay strictly inside your browser.
- 🤖 **Privacy, Proxy & AI MCP Ready**: Outbound typing/seen/delivered receipt blockers, `SOCKS5`/`HTTP` proxy + DoH routing, scheduled messages, auto-reply, loopback REST API, and `stdio` MCP server.

---

## 📊 Competitive Comparison

| Capability | 💬 **KV-Zalo** | 🌐 Zalo Web (`chat.zalo.me`) | 🍷 Zalo PC on Wine / Bottles | 📦 Electron Web Wrappers |
| :--- | :---: | :---: | :---: | :---: |
| **Native Wayland & GTK3 Client** | **✅ Pure Rust + WebKit2GTK** | ❌ Browser tab only | ❌ XWayland glitches / crashes | ⚠️ Heavy Chromium (~400 MB+) |
| **Full Zalo PC (`app.asar` + `ZDB4.0`)** | **✅ Automatic VNG SHA-256 sync** | ❌ Limited web build | ✅ Yes | ❌ Wraps `chat.zalo.me` |
| **1:1 & Group VoIP + Screen Share** | **✅ Native Rust (`zcall` / `zrtc`)** | ⚠️ Limited browser support | ❌ Broken PipeWire / camera | ⚠️ Partial |
| **Built-in Call Recorder** | **✅ Stereo WAV / Ogg-Opus** | ❌ No | ❌ External loopback setup | ❌ No |
| **Anti-Recall ("Giữ thu hồi")** | **✅ Default ON (`...` bubble menu)** | ❌ Messages wiped | ❌ Messages wiped | ❌ Messages wiped |
| **Multi-Account in One Window** | **✅ Native `#main-tab` sidebar (`0 ms`)** | ❌ 1 account per browser | ❌ 1 account only | ❌ Separate windows |
| **Zero-Storage Self-Hosted Docker** | **✅ `--read-only` stateless relay** | N/A | ❌ Impossible | ❌ Stores profile on server |
| **Counter-REA Binary Shield ([BLA](https://git.khoavo.myds.me/vndangkhoa/bla))** | **✅ Built-in (`bla-agents` v3.2)** | ❌ No | ❌ No | ❌ No |
| **REST API & AI Agent MCP** | **✅ Built-in (`--api` & `--mcp`)** | ❌ No | ❌ No | ❌ No |

---

## 🎯 Killer Features

### 1. 📞 Pure-Rust VoIP Calls & Stereo Call Recorder
Full-duplex `Opus 16 kHz` audio (PipeWire / PulseAudio + `cpal`), `H.264` camera & screen sharing, and `Zavi` / `SRTP_AES128_CM_HMAC_SHA1_80` encryption. Click **Record** in any active call (`● REC MM:SS`) to capture crystal-clear stereo `WAV` (local on left channel, remote on right) or `Ogg/Opus`—with a customizable save folder right inside **Cài đặt → Cài đặt cuộc gọi → Ghi âm cuộc gọi**.

### 2. 🛡️ Anti-Recall ("Giữ thu hồi") — Enabled by Default
Intercepts recall events before Zalo wipes message payloads, keeping original text, quoted replies, and cached media visible with a subtle **`Đã thu hồi`** badge. Toggle it effortlessly from any chat bubble's **`...` → `Giữ thu hồi`** context menu or the system tray.

### 3. 👥 Native `#main-tab` Multi-Account Workspace
Integrates account avatars and an aligned **`+` (Thêm tài khoản Zalo)** button directly into Zalo's native blue left navigation bar. Each profile runs in an isolated `WebDataManager` + cookie jar + Secret Service vault with instant `0 ms` switching (<kbd>Ctrl</kbd>+<kbd>1..9</kbd> / <kbd>Ctrl</kbd>+<kbd>Tab</kbd>) and live unread badges.

### 4. 🌐 Zero-Server-Persistence Self-Hosted Web & Mobile PWA (`kv-zalo-web`)
Host Zalo on any home server, VPS, or Synology NAS with **zero server-side user data**:
- `$znode.crypto`, `SafeStorage`, `KeychainCipher`, `BrowserCookieJar` (`zpw_sek`), IndexedDB, and virtual OPFS files execute **100% inside the client browser**.
- **Installable PWA & Mobile Responsive UI**: Serves `/manifest.webmanifest` (with per-profile `?profile=<slot>` PWA manifests), crisp SVG + maskable icons, offline Service Worker shell fallback, fluid `clamp()` typography, and a single-pane master-detail mobile chat layout (`320px–767px`) with bottom tab navigation and `← Chats` back button.
- **OS Media Session & In-App Media Controls**: Integrates `navigator.mediaSession` lock-screen/headset controls (`play`, `pause`, `stop`, `seekbackward`, `seekforward`, `seekto`, `previoustrack`, `nexttrack`, live position state) alongside a floating Now-Playing Media Card with scrubber, `-10s`/`+10s` skip, `1×–2×` playback speed, mute, and `<video>` Picture-in-Picture.
- The Rust container runs with `read_only: true`, `cap_drop: [ALL]`, and `core: 0`—acting strictly as an allowlisted HTTP/Range/WebSocket pass-through to `*.zalo.me` and `*.zadn.vn`.
- Supports multi-account browser tabs (`?profile=work`, `?profile=personal`) and optional `SubtleCrypto` `PBKDF2-SHA256` (600,000 iterations) → `AES-256-GCM` local PIN lock.

### 5. 🤖 Privacy Shields, Proxy Routing & AI Automation
Block outbound typing, delivered, and seen receipts per profile; tunnel HTTP, WebSocket, DoH, and WebView traffic over `socks5h://` or `http://` proxies; schedule messages or Vietnamese accent-folded auto-replies; and automate chats via the loopback REST API (`127.0.0.1:17890`) or `stdio` MCP server.

---

## 🚀 Quick Start (30 Seconds)

### Option A: 1-Line Linux Desktop Install (Omarchy / Arch & Ubuntu / Debian)

```bash
# GitHub mirror:
curl -fsSL https://raw.githubusercontent.com/vndangkhoa/kv-zalo/main/packaging/install.sh | bash

# Forgejo mirror:
curl -fsSL https://git.khoavo.myds.me/vndangkhoa/kv-zalo/raw/branch/main/packaging/install.sh | bash
```

> Pass `... | bash -s -- --user` to install into `~/.local` without `sudo`. On first launch, `kv-zalo` automatically downloads and SHA-256 verifies the official VNG `app.asar` runtime.

### Option B: Offline / Native Linux Packages

| Platform | Package | Install Command |
| :--- | :--- | :--- |
| **Universal (Omarchy + Ubuntu)** | `kv-zalo-setup-linux-x86_64.run` | `chmod +x kv-zalo-setup-linux-x86_64.run && ./kv-zalo-setup-linux-x86_64.run` |
| **Ubuntu 22.04 / 24.04+ / Debian** | `kv-zalo_<ver>_amd64.deb` | `sudo apt install ./kv-zalo_<ver>_amd64.deb` |
| **Omarchy / Arch / CachyOS** | `kv-zalo-<ver>-1-x86_64.pkg.tar.zst` | `sudo pacman -U ./kv-zalo-<ver>-1-x86_64.pkg.tar.zst` |
| **Rootless Portable (`~/.local`)** | `kv-zalo-<ver>-linux-x86_64.tar.gz` | `tar -xzf kv-zalo-*.tar.gz && ./kv-zalo-*/install-user.sh` |

### Option C: Build from Source

```bash
# Omarchy / Arch Linux prerequisites:
sudo pacman -S --needed rust pkgconf webkit2gtk-4.1 gtk3 gstreamer gst-plugins-base opus libpulse ffmpeg xdg-desktop-portal 7zip

# Ubuntu / Debian prerequisites:
./packaging/ubuntu-deps.sh

# Build & install into ~/.local:
./bin/build
./bin/install --user
~/.local/bin/kv-zalo
```

---

## 🐳 Docker & NAS Deployment

Deploy the stateless zero-persistence web relay (`kv-zalo-web`) on any Docker host or Synology NAS.

### Option A: Turnkey Read-Only Docker Compose (Recommended)

[`docker-compose.yml`](docker-compose.yml) runs a one-shot init container (`kv-zalo-asar-init`) to fetch & SHA-256 verify VNG's `app.asar` once into a shared volume, then launches `kv-zalo-web` with `read_only: true` and `:ro` mounts:

```bash
KVZALO_WEB_TOKEN="replace-with-strong-secret" docker compose up -d --build
```

Open **`http://localhost:8080`** (or place behind your HTTPS reverse proxy for `SubtleCrypto` / Service Worker support).

### Option B: Standalone Hardened Docker CLI

```bash
# 1. Build image:
docker build -t kv-zalo:latest .

# 2. Fetch & SHA-256 verify official VNG app.asar once onto host:
mkdir -p /srv/kv-zalo-bundle && chmod 0777 /srv/kv-zalo-bundle
docker run --rm --tmpfs /tmp:rw,size=1536m -v /srv/kv-zalo-bundle:/bundle kv-zalo:latest --fetch-asar /bundle

# 3. Run stateless read-only container (zero writable filesystems):
docker run -d --name kv-zalo-web \
  --read-only \
  --cap-drop=ALL \
  --security-opt=no-new-privileges:true \
  --ulimit core=0 \
  -p 127.0.0.1:8080:8080 \
  -v /srv/kv-zalo-bundle/current/Contents/Resources/app.asar:/opt/kv-zalo/app.asar:ro \
  -e KVZALO_WEB_TOKEN="replace-with-strong-secret" \
  --restart unless-stopped \
  kv-zalo:latest
```

### Synology NAS (Container Manager)

1. Open **Container Manager** on DSM → **Project** → **Create**.
2. Set project name to `kv-zalo` and upload [`docker-compose.yml`](docker-compose.yml) (or point to `/volume1/docker/kv-zalo`).
3. Set `KVZALO_WEB_TOKEN` and `KVZALO_WEB_BIND=0.0.0.0:8080`, then expose via **DSM Reverse Proxy (HTTPS)** so WebCrypto & Service Worker features run in a secure browser context.

---

## 🧩 Architecture

```
┌─────────────────────────────────────────┐   ┌─────────────────────────────────────────┐
│    Linux Desktop (`kv-zalo` Binary)     │   │   Self-Hosted Browser (`kv-zalo-web`)   │
│  GTK3 + WebKit2GTK 4.1 (`zalo-app://`)  │   │  Browser Preload + IndexedDB + OPFS SW  │
└────────────────────┬────────────────────┘   └────────────────────┬────────────────────┘
                     │ Native IPC (`window.$z*`)                   │ Stateless HTTP/Range/WSS
┌────────────────────▼────────────────────┐   ┌────────────────────▼────────────────────┐
│     `olaz-app` + `olaz-linux` (Rust)    │   │   `kv-zalo-web` (`--read-only` Relay)   │
│ • VoIP (`zcall`/`zrtc`) + Call Recorder │   │ • `mmap` ASAR static asset server       │
│ • Anti-Recall, Multi-Account, Tray      │   │ • Zero cookies / keys / DBs on disk     │
│ • Secret Service, ZDB4.0, REST & MCP    │   │ • Strict `*.zalo.me` / `*.zadn.vn` ACL  │
└────────────────────┬────────────────────┘   └────────────────────┬────────────────────┘
                     └──────────────────────┬──────────────────────┘
                                            │ TLS 1.3 / DoH / WSS / SRTP
                             ┌──────────────▼──────────────┐
                             │  Official Zalo Servers/CDN  │
                             └─────────────────────────────┘
```

### Repository Layout

| Path | Description |
| :--- | :--- |
| [`crates/olaz-core/`](crates/olaz-core) | `mmap` ASAR reader, VNG runtime verifier, Node-parity `$znode.crypto`, `ZDB4.0` (`db-cross-v4`), `zwalker`, keyring vault, profile registry & embedded `assets/` |
| [`crates/olaz-net/`](crates/olaz-net) | `reqwest` (`SOCKS5`/`HTTP` proxy) + `tokio-tungstenite`, `0600` cookie jar, DoH (`*.zadn.vn`), streaming `AES-GCM`/`AES-CTR` media |
| [`crates/olaz-linux/`](crates/olaz-linux) | Pure-Rust VoIP engine & stereo call recorder, `libgstolazauto.so`, `ksni` tray, anti-recall/privacy config, scheduler, REST & MCP |
| [`crates/olaz-app/`](crates/olaz-app) | Native GTK3 + `webkit2gtk-4.1` multi-account Linux desktop client (`kv-zalo`) |
| [`crates/olaz-web/`](crates/olaz-web) | Zero-server-persistence self-hosted web relay (`kv-zalo-web`) & browser-only preload runtime |
| [`packaging/`](packaging) · [`pkgbuild/`](pkgbuild) | 1-click `install.sh`, `.deb`/`.pkg.tar.zst`/`.run` builder, `PKGBUILD`, `kv-zalo.desktop`, `kv-zalo-web.service` |
| [`.blarc`](.blarc) · [`bin/harden`](bin/harden) · [`vendor/`](vendor) | [BLA (`bla-agents` v3.2)](https://git.khoavo.myds.me/vndangkhoa/bla) Counter-REA native Rust & JS source weave + post-build ELF64 shield |
| [`macos/`](macos) | Self-contained macOS Swift 6 / SwiftUI reference client (`WKWebView` + Liquid Glass) |
| [`docs/`](docs) | [`REVERSE-ENGINEERING-NOTES.md`](docs/REVERSE-ENGINEERING-NOTES.md) and [`reanalysis/`](docs/reanalysis) 450-channel IPC specification |

---

## 🔧 Configuration & CLI

### Environment Variables

| Variable | Target | Default | Description |
| :--- | :---: | :--- | :--- |
| `KVZALO_WEB_LISTEN` | Web | `127.0.0.1:8080` | Bind address & port for `kv-zalo-web` (`0.0.0.0:8080` in Docker) |
| `KVZALO_ASAR_PATH` | Web | Auto-discovered | Read-only path to official `app.asar` bundle |
| `KVZALO_WEB_TOKEN` | Web | _(disabled)_ | Constant-time shared passphrase / Bearer gate for `kv-zalo-web` |
| `OLAZ_PROFILE` | Desktop | `default` | Isolated Zalo profile slug (`[A-Za-z0-9_-]+`) |
| `OLAZ_DATA_DIR` | Desktop | `~/.local/share/olaz` | Root profile, `ZDB4.0`, and runtime cache directory |
| `OLAZ_PROXY` | Desktop | _(empty)_ | Upstream `socks5h://`, `socks5://`, `http://`, or `https://` proxy URL |
| `OLAZ_API_PORT` | Desktop | `17890` | Loopback HTTP REST & MCP server port (`--api`) |
| `OLAZ_API_TOKEN` | Desktop | _(optional)_ | Bearer token protecting the loopback REST API |

### Desktop CLI & Keyboard Shortcuts

```bash
kv-zalo --profile work                     # Launch isolated standalone profile window
kv-zalo --proxy socks5h://127.0.0.1:1080   # Route HTTP, WebSocket, DoH & WebView via proxy
kv-zalo --api                              # Start local REST & HTTP-MCP server on 127.0.0.1:17890
kv-zalo --mcp                              # Run Model Context Protocol JSON-RPC 2.0 server over stdio
kv-zalo --check-update                     # Check GitHub & Forgejo releases (--update to install)
./bin/test                                 # Run all 133 unit, Node parity & E2E workspace tests
```

| Shortcut | Action |
| :--- | :--- |
| <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>N</kbd> | Add a new Zalo account in the `#main-tab` sidebar |
| <kbd>Ctrl</kbd> + <kbd>1</kbd> .. <kbd>9</kbd> | Instant `0 ms` switch to account slot `1..9` |
| <kbd>Ctrl</kbd> + <kbd>Tab</kbd> | Cycle through logged-in Zalo accounts |

---

## 🌟 Support & Star History

If **KV-Zalo** makes Zalo feel right at home on your Linux desktop or homelab:

- Give the repository a **Star ⭐** on [GitHub](https://github.com/vndangkhoa/kv-zalo) or [Forgejo](https://git.khoavo.myds.me/vndangkhoa/kv-zalo)!
- Report bugs or suggest improvements via [GitHub Issues](https://github.com/vndangkhoa/kv-zalo/issues).

<p align="center">
  <a href="https://star-history.com/#vndangkhoa/kv-zalo&Date">
    <img src="https://api.star-history.com/svg?repos=vndangkhoa/kv-zalo&type=Date" alt="KV-Zalo Star History" width="75%">
  </a>
</p>

> [!IMPORTANT]
> **Disclaimer:** Unofficial interoperability & reverse-engineering project (building upon research from [`qnguyen3/zalo-swift-re`](https://github.com/qnguyen3/zalo-swift-re)), not affiliated with or endorsed by VNG Corporation. "Zalo" is a trademark of VNG. Use at your own risk with your own account.

---

## 📄 License

Distributed under the **GNU General Public License v3.0**. See [`LICENSE`](LICENSE) for details.

Developed with ❤️ by **Khoa Vo ([@vndangkhoa](https://github.com/vndangkhoa))**.
