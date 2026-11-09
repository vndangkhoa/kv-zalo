# Zalo for macOS — Reverse Engineering Notes

Source: `ZaloSetup-universal-26.10.20.dmg` (337.9 MB) — analyzed 2026-10-25.

## 1. Packaging identity

| Fact | Value |
|---|---|
| Bundle ID | `com.vng.zalo` |
| Version | 26.10.20 (build 26.10.20.3253) |
| Runtime | **Electron 22.3.9** (Chromium 108), universal (arm64 + x64) |
| App size | 762 MB installed; `app.asar` 180 MB |
| Updater | electron-updater / Squirrel (`app-update.yml`) |
| URL scheme | `zalo://` |
| Entry | `bootstrap.js` → `main-dist/main.js` (single-instance lock) |
| Permissions | Camera, microphone, Bluetooth (calls); ATS allows arbitrary loads |

Other processes: `compact-app` (mini overlay chat), `znotification` (notification window),
utility processes for SQLite and media, `mainless-worker`.

## 2. Code layout (extracted ASAR → `reanalysis/app_extracted/`)

- `main-dist/` — Electron main process, **obfuscated** with `@zsecure/zpc` (`zsymb(...)` calls).
- `pc-dist/` — the renderer. **This is the Zalo Web app** (`index.html` title "Đăng nhập Zalo Web",
  `body class="web"`). React + Redux Toolkit, webpack-bundled:
  - `lazy/default-login-main-startup-…js` (17 MB) — entire UI: **0 references to `require("electron")`**.
    Desktop integration goes through a preload bridge (`preload-render.js`), not direct Electron APIs.
  - `lazy/lang-vi.js` — 6,469 Vietnamese UI strings (full feature map, parsed to `reanalysis/lang_vi.json` — VNG content, gitignored; regenerate locally).
  - `libs/libsignal-protocol.static.js` — Signal-protocol E2E encryption (pure JS).
  - Workers: `sync-v2`, `dal`, `zd`, `search`, `cpu-heavy`, `opfs`, `pdf`, `trust`.
- `native/nativelibs/` — `.node` binaries: `zcall` (VoIP/video via `vcmac`), `sqlite3`,
  `db-cross-v4`, `mp4thumb`, `os-sys-info`, `file-utils`, `v8-profiles`. These are the only
  parts that cannot run under plain JavaScriptCore.

## 3. UI structure (from lang pack + CSS class census)

- **Login** (`login.html`): QR-code login ("Quét mã QR để đăng nhập"), phone-number login,
  email login, company-account (Zalo Business) login.
- **Main window** (`index.html`): left icon **tab-bar nav rail** + conversation list + chat pane.
  - Rail tabs (strings found): **Tin nhắn** (Messages), **Danh bạ** (Contacts/You),
    **Ưu tiên** (Focus tab), phân loại **Labels/Khách hàng** (customer labels for Business mode),
    **Công việc** (Todo), ZCloud (drive), Calls, Settings, profile.
  - Conversation list: `conv-item`, pinned ("Ưu tiên ghim"), group/community badges, unread counts,
    labels ("Phân loại hội thoại"), broadcast.
  - Chat: `message-view` / `chat-message` (810 rules) with pictures, voice messages
    ("Tin nhắn thoại"), quotes/replies ("Trả lời"), reactions, comments ("Bình luận"),
    recall ("Thu hồi"), E2EE notices, date separators (`chat-date`), typing status.
  - Composer: `chat-box-input` ("Nhập tin nhắn"), attachments, stickers/emoji, AI translate
    (`ai-translation-language-model.js` — TF.js on-device model), screenshot capture.
- **Communities** (348 strings — biggest feature): groups of groups, membership approval,
  management menus.

## 4. Brand / theme tokens

- Primary blue: `#0068FF` (CTA links, buttons; pressed `#005AE0`, tonal bg `#E5F1FF`).
- Text primary (light): `#081B3A`. Dark-mode window bg: `#22262B`.
- Fonts: system + embedded fonts in `pc-dist/fonts/`.
- Tagline: "Zalo - Nhắn gửi yêu thương" ("Send love").

## 5. Network surface (from CSP + bundle strings)

- Static/resources: `res-zalo.zadn.vn`, `stc-chat.zdn.vn`, `res.zalo.zdn.vn` (avatars: `s120-ava-talk.zadn.vn`).
- Messaging API: `zimsg.chat.zalo.me`, `wss://…chat.zalo.me`, `wss://*.zavi.me` (realtime).
- Identity/auth: `zwid.api.zalo.me`. Cloud drive: `f12/f14-zpcloud.zdn.vn`. Web: `zalo.me`.
- Avatars modelled as `https://s120-ava-talk.zadn.vn/…` URLs; CDN for files `*.z-cdn.me`.

## 6. Feasibility verdict for the native rewrite

- **UI layer**: 100 % portable to SwiftUI — the renderer is plain React with no Electron coupling.
- **Engine layer**: pure-JS protocol/crypto (Signal + sync + DAL workers) is embeddable in
  **JavaScriptCore** with Swift shims (WebSocket, storage, fetch). Native `.node` modules
  (calls, sqlite) would need a Node sidecar — out of scope for v1.
- **Real login/messaging against VNG servers** requires re-implementing their proprietary
  handshake/obfuscation (`@zsecure/zpc` on the wire too) — v1 ships a demo engine behind a
  `ZaloService` protocol so a future JS engine can be dropped in.

## 7. Asset provenance

`Assets/zalo-wordmark.svg`, `zalo-logo.png`, `avatar-default.png` extracted from `pc-dist/assets/`;
`zalo.icns` from `Resources/`. Used for visual fidelity of the early recreation. These are VNG's assets: they are
gitignored and **not part of the public repo** (Olaz ships its own artwork); extract them locally if needed.
