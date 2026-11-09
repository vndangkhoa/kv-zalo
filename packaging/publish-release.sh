#!/usr/bin/env bash
# Publishes KV Zalo Linux 1-Click Release (Omarchy & Ubuntu) across all configured Git forges:
#   1. Forgejo (origin / forgejo: https://git.khoavo.myds.me/vndangkhoa/kv-zalo)
#   2. Forgejo (forgejo-vndns:    https://git.khoavo.vndns.net/vndangkhoa/kv-zalo)
#   3. GitHub  (github:           https://github.com/vndangkhoa/kv-zalo)
#   4. GitLab  (gitlab:           https://gitlab.khoavo.myds.me/vndangkhoa/kv-zalo)
set -euo pipefail

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
cd "$ROOT"

VERSION_OVERRIDE=""
SKIP_BUILD=0
FORCE_TAG=0

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      VERSION_OVERRIDE="${2:?missing value for --version}"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    --force-tag)
      FORCE_TAG=1
      shift
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

WORKSPACE_VER="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"
VER="${VERSION_OVERRIDE:-${KVZALO_VERSION:-${OLAZ_VERSION:-$WORKSPACE_VER}}}"
VER="${VER#v}"
TAG="v${VER}"
DIST_DIR="$ROOT/dist/${TAG}"

if [ "$SKIP_BUILD" -eq 0 ] || [ ! -d "$DIST_DIR" ]; then
  "$ROOT/packaging/build-linux-release.sh" --version "$VER"
fi

# Ensure annotated git tag exists
if [ "$FORCE_TAG" -eq 1 ] || ! git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
  git tag -fa "$TAG" -m "KV Zalo ${TAG} — 1-Click Setup for Omarchy & Ubuntu"
fi

echo "==> Pushing branch & tag ${TAG} to remotes..."
git push origin main
git push $( [ "$FORCE_TAG" -eq 1 ] && printf '%s' '--force' ) origin "$TAG"

if git remote get-url forgejo-vndns >/dev/null 2>&1; then
  git push forgejo-vndns main || true
  git push $( [ "$FORCE_TAG" -eq 1 ] && printf '%s' '--force' ) forgejo-vndns "$TAG" || true
fi

git push github main
if [ "$FORCE_TAG" -eq 1 ]; then
  git push github ":refs/tags/${TAG}" 2>/dev/null || true
fi
git push github "$TAG"

if git remote get-url gitlab >/dev/null 2>&1; then
  git push gitlab main || echo "Warning: GitLab git push failed (server may be unavailable)."
  git push $( [ "$FORCE_TAG" -eq 1 ] && printf '%s' '--force' ) gitlab "$TAG" || true
fi

# Generate release notes markdown
NOTES_FILE="$(mktemp)"
trap 'rm -f "$NOTES_FILE"' EXIT
cat > "$NOTES_FILE" <<EOF
## KV Zalo ${TAG} — Pure-Rust VoIP Calls, Call Recorder, Anti-Recall & Multi-Account for Omarchy & Ubuntu

100% pure-Rust + GTK3 + WebKit2GTK 4.1 desktop client & zero-persistence self-hosted web relay for Zalo:

- **📞 Pure-Rust 1:1 & Group VoIP Calls + Call Recorder (\`zcall\` + \`zrtc\`)**: Full-duplex Opus 16 kHz audio, stereo WAV / Ogg-Opus call recording with configurable save folder, H.264 camera & screen sharing, Zavi/SRTP encryption, and native incoming/in-call/device windows.
- **🛡️ Default Anti-Recall ("Giữ thu hồi") & Multi-Account Sidebar**: Preserves recalled messages with per-bubble \`...\` toggle, and native \`#main-tab\` blue sidebar account switcher + \`+\` Add Account button.
- **🌐 Zero-Server-Persistence Self-Hosted Web (\`kv-zalo-web\`)**: Hardened \`--read-only\` Docker / systemd relay storing 100% of cookies, vault keys, OPFS files, and chats exclusively in the browser.
- **🗄️ Native Modules, Proxy & Automation**: Pure-Rust \`db-cross-v4\` (\`ZDB4.0\`), \`zwalker\`, \`file-utilities\`, \`zfile\`, SOCKS5/HTTP \`--proxy\`, loopback REST API (\`--api\`), stdio MCP server (\`--mcp\`), and SHA-256 verified release updater (\`--check-update\` / \`--update\`).

### 🚀 1-Command Terminal Setup (Auto-detects Omarchy & Ubuntu)

\`\`\`bash
# GitHub mirror:
curl -fsSL https://github.com/vndangkhoa/kv-zalo/releases/download/${TAG}/install.sh | bash

# Forgejo mirror:
curl -fsSL https://git.khoavo.myds.me/vndangkhoa/kv-zalo/releases/download/${TAG}/install.sh | bash
\`\`\`

### 📦 1-Click Offline / Graphical Setup Packages

| Platform | File | 1-Click Action |
|---|---|---|
| **Omarchy + Ubuntu (Universal)** | \`kv-zalo-setup-linux-x86_64.run\` | \`chmod +x kv-zalo-setup-linux-x86_64.run && ./kv-zalo-setup-linux-x86_64.run\` (or double-click) |
| **Ubuntu 22.04 / 24.04+ / Debian / Mint** | \`kv-zalo_${VER}_amd64.deb\` | Double-click in Software Install or \`sudo apt install ./kv-zalo_${VER}_amd64.deb\` |
| **Omarchy / Arch Linux** | \`kv-zalo-${VER}-1-x86_64.pkg.tar.zst\` | \`sudo pacman -U ./kv-zalo-${VER}-1-x86_64.pkg.tar.zst\` |
| **Portable (\`~/.local\`, no root)** | \`kv-zalo-${VER}-linux-x86_64.tar.gz\` | \`tar -xzf kv-zalo-${VER}-linux-x86_64.tar.gz && ./kv-zalo-${VER}-linux-x86_64/install-user.sh\` |
EOF

# 1. GitHub Release (direct via `gh` if authenticated, otherwise via GitHub Actions tag workflow)
if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  echo "==> Publishing release ${TAG} directly to GitHub via gh..."
  if gh release view "$TAG" --repo vndangkhoa/kv-zalo >/dev/null 2>&1; then
    gh release edit "$TAG" --repo vndangkhoa/kv-zalo \
      --title "KV Zalo ${TAG} — 1-Click Setup for Omarchy & Ubuntu" \
      --notes-file "$NOTES_FILE"
    gh release upload "$TAG" "$DIST_DIR"/* --repo vndangkhoa/kv-zalo --clobber
  else
    gh release create "$TAG" "$DIST_DIR"/* --repo vndangkhoa/kv-zalo \
      --title "KV Zalo ${TAG} — 1-Click Setup for Omarchy & Ubuntu" \
      --notes-file "$NOTES_FILE"
  fi
else
  echo "==> Pushed tag ${TAG} to GitHub; .github/workflows/release-linux.yml will publish https://github.com/vndangkhoa/kv-zalo/releases/tag/${TAG}"
fi

# 2 & 3. Forgejo + GitLab REST API release & asset upload (using git credential helper safely in-memory)
GIT_TERMINAL_PROMPT=0 python3 - "$TAG" "$VER" "$DIST_DIR" "$NOTES_FILE" <<'PY'
import base64
import json
import mimetypes
import pathlib
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid

tag, ver, dist_dir_str, notes_path = sys.argv[1:5]
dist_dir = pathlib.Path(dist_dir_str)
notes = pathlib.Path(notes_path).read_text(encoding="utf-8")
title = f"KV Zalo {tag} — 1-Click Setup for Omarchy & Ubuntu"
assets = sorted(p for p in dist_dir.iterdir() if p.is_file())
ua = "Mozilla/5.0 (X11; Linux x86_64) kv-zalo-release-publisher/1.0"


def get_git_cred(host: str):
    proc = subprocess.run(
        ["git", "credential", "fill"],
        input=f"protocol=https\nhost={host}\n\n",
        text=True,
        capture_output=True,
    )
    kv = dict(line.split("=", 1) for line in proc.stdout.splitlines() if "=" in line)
    return kv.get("username", ""), kv.get("password", "")


def api_json(url: str, method: str, headers: dict, payload=None):
    data = json.dumps(payload).encode("utf-8") if payload is not None else None
    hdrs = dict(headers)
    hdrs["User-Agent"] = ua
    if data is not None:
        hdrs["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=hdrs, method=method)
    with urllib.request.urlopen(req, timeout=60) as resp:
        raw = resp.read()
        return resp.status, (json.loads(raw.decode("utf-8")) if raw else None)


# ── Forgejo (`git.khoavo.myds.me`) ───────────────────────────────────────────
fj_user, fj_pw = get_git_cred("git.khoavo.myds.me")
if fj_pw:
    fj_base = "https://git.khoavo.myds.me/api/v1/repos/vndangkhoa/kv-zalo"
    b64 = base64.b64encode(f"{fj_user}:{fj_pw}".encode("utf-8")).decode("ascii")
    fj_hdrs = {"Authorization": f"Basic {b64}"}
    try:
        rel = None
        try:
            _, rel = api_json(f"{fj_base}/releases/tags/{urllib.parse.quote(tag)}", "GET", fj_hdrs)
        except urllib.error.HTTPError as e:
            if e.code != 404:
                raise
        body_obj = {
            "tag_name": tag,
            "target_commitish": "main",
            "name": title,
            "body": notes,
            "draft": False,
            "prerelease": False,
        }
        if rel and rel.get("id"):
            rel_id = rel["id"]
            _, rel = api_json(f"{fj_base}/releases/{rel_id}", "PATCH", fj_hdrs, body_obj)
        else:
            _, rel = api_json(f"{fj_base}/releases", "POST", fj_hdrs, body_obj)
            rel_id = rel["id"]

        existing_assets = {a["name"]: a["id"] for a in (rel.get("assets") or [])}
        for path in assets:
            if path.name in existing_assets:
                del_req = urllib.request.Request(
                    f"{fj_base}/releases/{rel_id}/assets/{existing_assets[path.name]}",
                    headers={**fj_hdrs, "User-Agent": ua},
                    method="DELETE",
                )
                with urllib.request.urlopen(del_req, timeout=30):
                    pass

            boundary = f"----KVZaloBoundary{uuid.uuid4().hex}"
            ctype = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
            file_bytes = path.read_bytes()
            head = (
                f"--{boundary}\r\n"
                f'Content-Disposition: form-data; name="attachment"; filename="{path.name}"\r\n'
                f"Content-Type: {ctype}\r\n\r\n"
            ).encode("utf-8")
            tail = f"\r\n--{boundary}--\r\n".encode("utf-8")
            multipart_body = head + file_bytes + tail

            up_url = f"{fj_base}/releases/{rel_id}/assets?name={urllib.parse.quote(path.name)}"
            up_req = urllib.request.Request(
                up_url,
                data=multipart_body,
                headers={
                    **fj_hdrs,
                    "User-Agent": ua,
                    "Content-Type": f"multipart/form-data; boundary={boundary}",
                },
                method="POST",
            )
            with urllib.request.urlopen(up_req, timeout=180) as r:
                print(f"  [Forgejo] Uploaded {path.name} ({len(file_bytes)} bytes) -> HTTP {r.status}")
        print(f"==> Published Forgejo release: https://git.khoavo.myds.me/vndangkhoa/kv-zalo/releases/tag/{tag}")
    except Exception as exc:
        print(f"Warning: Forgejo release publish error: {exc}", file=sys.stderr)
else:
    print("Skipping Forgejo API upload (no stored credential for git.khoavo.myds.me)")

# ── GitLab (`gitlab.khoavo.myds.me`) ─────────────────────────────────────────
gl_user, gl_pw = get_git_cred("gitlab.khoavo.myds.me")
if gl_pw:
    gl_proj = "https://gitlab.khoavo.myds.me/api/v4/projects/vndangkhoa%2Fkv-zalo"
    gl_hdrs = {"PRIVATE-TOKEN": gl_pw}
    try:
        links = []
        for path in assets:
            pkg_url = f"{gl_proj}/packages/generic/kv-zalo/{urllib.parse.quote(ver)}/{urllib.parse.quote(path.name)}"
            file_bytes = path.read_bytes()
            up_req = urllib.request.Request(
                pkg_url,
                data=file_bytes,
                headers={**gl_hdrs, "User-Agent": ua, "Content-Type": "application/octet-stream"},
                method="PUT",
            )
            with urllib.request.urlopen(up_req, timeout=180) as r:
                print(f"  [GitLab] Uploaded {path.name} -> HTTP {r.status}")
            links.append({"name": path.name, "url": pkg_url, "filepath": f"/{path.name}", "link_type": "package"})

        rel_exists = False
        try:
            api_json(f"{gl_proj}/releases/{urllib.parse.quote(tag)}", "GET", gl_hdrs)
            rel_exists = True
        except urllib.error.HTTPError as e:
            if e.code != 404:
                raise

        if rel_exists:
            api_json(
                f"{gl_proj}/releases/{urllib.parse.quote(tag)}",
                "PUT",
                gl_hdrs,
                {"name": title, "description": notes},
            )
        else:
            api_json(
                f"{gl_proj}/releases",
                "POST",
                gl_hdrs,
                {"name": title, "tag_name": tag, "ref": "main", "description": notes, "assets": {"links": links}},
            )
        print(f"==> Published GitLab release: https://gitlab.khoavo.myds.me/vndangkhoa/kv-zalo/-/releases/{tag}")
    except Exception as exc:
        print(f"Warning: GitLab release publish unavailable ({exc})", file=sys.stderr)
PY
