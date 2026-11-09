# zalo-swift-re — Olaz

**A reverse-engineered, native desktop client for [Zalo](https://zalo.me) — written in Swift for macOS and in Rust for Linux (Omarchy & Ubuntu).**

Olaz runs the **official Zalo PC renderer** (`app.asar`) inside the OS native WebKit engine (`WKWebView` on macOS,
`WebKit2GTK 4.1` on Linux) and supplies in native Swift / Rust everything that Electron's preload and main process
used to provide. It does **not** re-implement Zalo's wire protocol:
- **macOS (`Sources/`)**: puts a native SwiftUI interface (Liquid Glass, sidebar + labels, native media viewer) on top
  of a headless `WKWebView` hosting Zalo's official client state.
- **Linux (`crates/`)**: mounts the official Zalo PC interface directly in a frameless GTK3 + `webkit2gtk-4.1` window
  with on-the-fly Linux UI/window/clipboard/call bundle patches, embedded GStreamer/FFmpeg inline voice-note & video
  playback (`libgstolazauto.so`), Secret Service (`org.freedesktop.secrets`) vault, StatusNotifierItem tray, and XDG
  Desktop Portals across **Omarchy** (Arch Linux + Hyprland/Wayland) and **Ubuntu 22.04 LTS / 24.04 LTS+**.

Zalo PC does not need to be installed. On first launch Olaz downloads the official Zalo PC installer from VNG,
verifies its integrity (Apple code signature on macOS; pinned SHA-256 on Linux), and stores `app.asar` locally
(`~/Library/Application Support/Olaz/ZaloRuntime/` on macOS; `~/.local/share/olaz/ZaloRuntime/` on Linux).
See [`ZaloRuntime/ZaloRuntimeInstaller.swift`](Sources/Olaz/ZaloRuntime/ZaloRuntimeInstaller.swift) and
[`crates/olaz-core/src/installer.rs`](crates/olaz-core/src/installer.rs).

> [!IMPORTANT]
> **Unofficial project, not affiliated with or endorsed by VNG Corporation.** "Zalo" is a trademark of VNG.
> This is an interoperability and reverse-engineering research project. Using an unofficial client may
> conflict with Zalo's terms of service, so use it at your own risk with your own account.

## Features

- **Real QR login** (trusted-device QR → scan with Zalo on your phone → confirm), session restore on relaunch
- **Chat**: history and load older, send text / reply / mentions, reactions, typing, read state, realtime updates
- **Composer "+"**: photos / video, files, screenshot (Zalo's own `ZaloCap`), my contact card, reminders
- **Message actions**: forward, copy, recall, delete with undo, pin bar, star
- **Voice / video calls** through Zalo's own `ZaloCall` helper
- **E2EE conversations**, including E2EE video playback
- **Phone → PC message sync** with a native progress UI
- Contacts and friend requests, My Cloud, polls, stickers, labels, hidden chats (PIN), stranger folder,
  shared-media browser, quick switcher, native notifications

## Requirements

- **macOS**: macOS 26 or later, Xcode 27 (Swift 6 toolchain)
- **Linux**: Omarchy (Arch Linux + Hyprland/Wayland) or Ubuntu 22.04 LTS / 24.04 LTS+ (Wayland or X11), Rust 1.80+
- A Zalo account and the Zalo app on your phone (for QR login)
- About 340 MB of download on first launch (Zalo PC's code, fetched from VNG)

Zalo allows **one desktop (PC) session per account**. If the official Zalo PC is also running, quit it while
using Olaz.

## Build & run (macOS — Swift)

```bash
git clone https://github.com/qnguyen3/zalo-swift-re.git
cd zalo-swift-re

swift test                     # crypto parity (vs Node), asar reader, JSC/libsignal and host-script tests
./scripts/make-app.sh debug    # → build/Olaz.app (ad-hoc signed, JIT entitlement)
open build/Olaz.app            # real QR login — scan with Zalo on your phone
```

A new PC session has no message history except My Cloud, because Zalo keeps history on the phone. Use
**Đồng bộ tin nhắn từ điện thoại** (confirm on the phone) to import it.

## Install & build (Linux — Rust: Omarchy & Ubuntu)

### 1-Click Install (Prebuilt Releases — Omarchy & Ubuntu)

- **1-Line Terminal Installer** (auto-detects **Omarchy / Arch Linux** `pacman` or **Ubuntu / Debian** `apt`, verifies `SHA256SUMS.txt`, and installs desktop launcher + icons):
  ```bash
  # GitHub:
  curl -fsSL https://raw.githubusercontent.com/vndangkhoa/zalo-swift-re/main/packaging/install.sh | bash

  # Forgejo mirror:
  curl -fsSL https://git.khoavo.myds.me/vndangkhoa/zalo-swift-re/raw/branch/main/packaging/install.sh | bash
  ```
  *(Pass `... | bash -s -- --user` to install into `~/.local` without `sudo`, or `--launch` to start Olaz right away.)*

- **1-Click Setup Files** ([GitHub Releases](https://github.com/vndangkhoa/zalo-swift-re/releases) · [Forgejo Releases](https://git.khoavo.myds.me/vndangkhoa/zalo-swift-re/releases) · [GitLab Releases](https://gitlab.khoavo.myds.me/vndangkhoa/zalo-swift-re/-/releases)):
  - **Universal 1-Click Offline Setup (Omarchy + Ubuntu)**: `olaz-setup-linux-x86_64.run`
    ```bash
    chmod +x olaz-setup-linux-x86_64.run && ./olaz-setup-linux-x86_64.run
    ```
  - **Ubuntu 22.04 LTS / 24.04 LTS+ / Debian / Linux Mint / Pop!\_OS**: double-click `olaz_0.1.0_amd64.deb` in Software Install, or run:
    ```bash
    sudo apt install ./olaz_0.1.0_amd64.deb
    ```
  - **Omarchy / Arch Linux / CachyOS / EndeavourOS**:
    ```bash
    sudo pacman -U ./olaz-0.1.0-1-x86_64.pkg.tar.zst
    ```

### Build from source — Omarchy / Arch Linux

1. **Install build & runtime packages** (already present on most Omarchy setups; `libgstolazauto.so` also embeds
   fallback audio/video sinks and FFmpeg MP4/AAC/MP3/H.264 demuxing when `gst-plugins-good` / `gst-libav` are absent):
   ```bash
   sudo pacman -S --needed rust pkgconf webkit2gtk-4.1 gtk3 gstreamer gst-plugins-base ffmpeg xdg-desktop-portal 7zip
   ```

2. **Option A — Build & install to `~/.local` (no `sudo` needed for install)**:
   ```bash
   ./bin/build
   ./bin/install --user        # installs ~/.local/bin/olaz + desktop launcher + hicolor icons
   ~/.local/bin/olaz
   ```

3. **Option B — Build & install a native Arch `pacman` package (`pkgbuild/PKGBUILD`)**:
   ```bash
   ./bin/install               # runs `makepkg -fsi` inside pkgbuild/
   ```

### Ubuntu 22.04 LTS / 24.04 LTS+ (and Debian / Pop!\_OS / Linux Mint)

1. **Install system dependencies**:
   ```bash
   ./packaging/ubuntu-deps.sh
   ```
   *(If Rust is not installed yet, install `rustup` via <https://rustup.rs>.)*

2. **Option A — Build & install to `~/.local`**:
   ```bash
   ./bin/build
   ./bin/install --user
   ~/.local/bin/olaz
   ```

3. **Option B — Build & install native Linux release packages (`.deb`, `.pkg.tar.zst`, `.run`, `.tar.gz`)**:
   ```bash
   ./packaging/build-linux-release.sh
   sudo apt install ./dist/v0.1.0/olaz_0.1.0_amd64.deb
   ```

### Linux tests & developer harness

```bash
./bin/test                                 # workspace unit + Node crypto parity + ASAR + DoH + portal tests
./target/release/olaz --demo               # offline Setup / Liquid Glass preview mode
./target/release/olaz --engine-probe [login|main] [seconds]
RUST_LOG=olaz=debug,olaz_app=debug OLAZ_TRACE_WORKERS=1 ./target/release/olaz
```

### Release (maintainers)

```bash
xcrun notarytool store-credentials "olaz-notary" --apple-id <apple-id> --team-id <team-id>   # once
./scripts/release.sh           # → dist/Olaz-1.0.dmg + dist/Olaz-1.0.zip (Developer ID, hardened runtime, notarized, stapled)
OLAZ_VERSION=1.1 ./scripts/release.sh
```

The script signs `build/Olaz.app` with the "Developer ID Application" identity in your keychain, notarizes and
staples the app, then wraps it in a drag-to-Applications DMG that is signed, notarized and stapled as well.
Credentials stay in your keychain. Users still fetch the pinned Zalo runtime from VNG on first launch.

### Pinned Zalo version

The Zalo version is pinned in `ZaloRuntime.pinned` (version, DMG URL, SHA-256). To move Olaz to a newer Zalo,
test against it and bump the pin. Installed runtimes are upgraded in the background and switched at the next launch.
If the pinned DMG disappears from VNG's CDN, a fresh install falls back to whatever `zalo.me/download/zalo-pc` offers
(still signature-checked). `OLAZ_ZALO_DMG=pinned OLAZ_NETWORK_TESTS=1 swift test --filter ZaloRuntimeTests` runs the
install end to end.

### Developer harness

Prints every bridge/IPC call, console line and network request:

```bash
.build/debug/Olaz --engine-probe [login|main] [seconds]
OLAZ_TRACE_WORKERS=1 OLAZ_DEBUG_RESPONSES=1 OLAZ_QR_OUT=/tmp/qr.png …
```

* Engine diagnostics: `~/Library/Application Support/Olaz/logs/engine.log` (shapes only, no message content or tokens).
* Dev evaluation channel (debug builds only, off by default): `defaults write app.olaz.mac OlazDevEval -bool YES`.

## Architecture

```
SwiftUI (Liquid Glass)                     Views/, Shell/, App/
        │  @Observable ZaloEngine           ZaloRuntime/ZaloEngine.swift (+ feature extensions)
        ▼
OfficialClientHost  ── WKWebView, headless
  ├─ zalo-app:// scheme → pc-dist served straight out of Zalo's app.asar      (AsarArchive, ZaloRuntimeInstaller)
  ├─ HostScripts/network-shim.js → XHR/fetch/WebSocket proxied to URLSession   (NetworkProxy)
  ├─ HostScripts/bridge.js       → $z* preload globals, Electron IPC emulation (ZaloBridge)
  ├─ HostScripts/local-modules.js→ Node path/os/crypto objects, electron-store
  ├─ HostScripts/state-adapter.js→ official state → Swift (QR via DI container, Redux store)
  └─ HostScripts/*-adapter.js    → per-feature adapters over the official services (chat, composer, calls, …)
        │
ZaloMainProcess  ── Swift stand-in for Electron main.js: IPC channel handlers,
                    cookies (zpw_sek), zsafe-storage, keychain cipher, page hand-off
```

* **Bridge table**: `Resources/bridge-table.json` maps all 450 table-driven preload members
  to their IPC channel + type, generated from the official `preload-render.js`.
  Semantics per channel: [`reanalysis/bridge-spec.md`](reanalysis/bridge-spec.md).
* **Node parity**: `$znode.crypto` is reproduced on CommonCrypto/CryptoKit and verified
  byte-for-byte against Node (`Tests/OlazTests/RuntimeTests.swift`). The official
  client uses it to encrypt `zcid` and request params.
* **Storage**: forced to IndexedDB (no native sqlite3): `fail_sqlite3=true`,
  `canIUseSQLite→false`, `isMigrateFullDone→false`, as the spec prescribes.
* **JSBridge/**: standalone JavaScriptCore runtime + WebCrypto shim + libsignal E2EE
  module, for JS-only pieces that run outside the renderer.

### Linux Rust Workspace (`crates/`)

```
OlazAppWindow (GTK3 frameless + webkit2gtk-4.1)            crates/olaz-app/
  ├─ OfficialClientHost  ── mounts official Zalo PC UI directly (`login.html` → `index.html`)
  │    ├─ zalo-app:// & zfile:// schemes with HTTP 206 byte-range slicing
  │    ├─ Loopback HTTP 206 GStreamer media proxy + WebKit bubblewrap sandbox paths
  │    ├─ Secondary frameless `popup-viewer.html` (MediaViewer) & `child.html` windows
  │    └─ On-the-fly Linux renderer bundle patches (`WIN32` controls, `Ctrl+V` PNG paste, call logs)
  └─ ZaloMainProcess     ── Rust Electron main-process & IPC implementation (`$zwindow`, `$znode`, `$znoti`, …)
        │
  ├─ crates/olaz-core/   ── Zero-copy `mmap` AsarArchive, pinned DMG installer (`7zz`/`7z` + SHA-256),
  │                         Node-parity `$znode.crypto`, `zsafe-storage`, `org.freedesktop.secrets` vault
  ├─ crates/olaz-net/    ── `reqwest` + `tokio-tungstenite` proxy, `0600` cookie jar (`zpw_sek`),
  │                         DoH (`1.1.1.1` / `8.8.8.8`) for `*.zadn.vn`, streaming AES-GCM / AES-CTR media
  └─ crates/olaz-linux/  ── Embedded `libgstolazauto.so` (PipeWire/Pulse/ALSA auto sinks + FFmpeg MP4/AAC/H.264
                            demuxer + `olazmediasrc`), `ksni` tray, `xdg-desktop-portal` dialogs & screenshots,
                            Omarchy theme watcher & GNOME `gsettings` fallback
```

## Status

| Area | State |
|---|---|
| Official renderer in WKWebView, from the Zalo PC code Olaz fetches from VNG (no Zalo PC install needed) | ✅ |
| Electron main-process emulation (450-member IPC table) | ✅ login + main client paths |
| Network via Swift: cookies, UA `ZaloPC-darwin-23v694`, CSP-safe sync bridge | ✅ |
| Real QR login (trusted-device QR) → scan → confirm → session | ✅ verified end to end |
| Session restore on relaunch (no QR) | ✅ |
| Realtime WebSocket (`wsN-msg.chat.zalo.me`) | ✅ handshake + official sync commands |
| Conversation list from official services, native SwiftUI | ✅ names, groups, members, unread |
| Chat: open, history + load older, send text / reply, reactions, typing, read state, realtime | ✅ official chatbox controller + message loaders (`HostScripts/chat-adapter.js`) |
| Native chat UI (bubbles, media, quotes, reactions, composer with IME-safe Return) | ✅ `Views/ChatView.swift`, `MessageBubble.swift`, `ChatComposer.swift` |
| Composer "+": photos / video, files, screenshot, my contact card, reminder | ✅ official toolbar paths (`HostScripts/composer-adapter.js`, `ZaloEngine+Composer.swift`) |
| Message actions: forward, copy, recall, delete + undo, pin bar, star | ✅ `HostScripts/message-actions-adapter.js` |
| Voice / video calls | ✅ official `ZaloCall` helper (`CallBridge.swift`, `HostScripts/call-adapter.js`) |
| Phone → PC message sync, native Liquid Glass progress UI | ✅ official sync-v2 state machine (`HostScripts/transfer-adapter.js`, `Views/TransferView.swift`) |
| Hidden shared-worker window (Electron `sharedWorkerManager` port) | ✅ `ZaloRuntime/SharedWorkerRouter.swift` |
| CDN media when system DNS fails `*.zadn.vn` | ✅ DNS-over-HTTPS fallback with pinned-SNI TLS (`ZaloRuntime/MediaLoader.swift`), Zalo hosts only |
| Create group | 🧪 implemented over the official `createGroupV2` path, not yet verified against a live account |
| Native SQLite backend (Zalo 26.10 migrates new sessions to SQLite) | ⏳ host stays on IndexedDB via the official "migration disabled" branch |

## Reverse-engineering notes

* [REVERSE-ENGINEERING-NOTES.md](REVERSE-ENGINEERING-NOTES.md): how the original Zalo PC app (Electron 22) is
  packaged and structured, and why the renderer can be hosted outside Electron.
* [reanalysis/bridge-spec.md](reanalysis/bridge-spec.md): the preload bridge (`window.$z*`), IPC channels and
  their semantics, written as a spec for a Swift/WKWebView host.
* [reanalysis/bridge-table.json](reanalysis/bridge-table.json): machine-readable channel table.
* Comments at the top of each `HostScripts/*-adapter.js` document the official code paths each feature drives.

## What is (and isn't) in this repo

* **The Zalo runtime is not included.** Each user's Olaz downloads it from VNG and checks VNG's signature.
* **Zalo's brand assets, UI string table and other files from the official bundle are not included.** They're
  gitignored (see [.gitignore](.gitignore)). To regenerate them for research, extract them from your own copy of
  Zalo PC.
* **One third-party file is vendored**: [`Sources/Olaz/JSModules/vendor/libsignal-protocol.js`](Sources/Olaz/JSModules/vendor/libsignal-protocol.js),
  VNG's fork of [signalapp/libsignal-protocol-javascript](https://github.com/signalapp/libsignal-protocol-javascript),
  taken from Zalo PC 26.10.20 (`pc-dist/libs/libsignal-protocol.static.js`). Upstream is licensed **GPL-3.0**, so
  this build is redistributed under GPL-3.0. Only the standalone `JSBridge` E2EE module and its tests use it. The live
  client runs the copy inside the `app.asar` it downloads.

## License

Olaz is free software, licensed under the [GNU General Public License v3.0](LICENSE).
Copyright © 2026 Quan Nguyen.
