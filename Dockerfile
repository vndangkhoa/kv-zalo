# ==============================================================================
# KV Zalo Web (`kv-zalo-web`) — Zero-Server-Persistence Hardened Container Image
# ==============================================================================
#
# Option A — Docker Compose (recommended one-command setup):
#   KVZALO_WEB_TOKEN="replace-with-strong-secret" docker compose up -d --build
#
# Option B — Standalone Docker CLI:
#   1. Build image:
#      docker build -t kv-zalo:latest .
#
#   2. Fetch & SHA-256 verify official VNG `app.asar` once onto host:
#      mkdir -p /srv/kv-zalo-bundle && chmod 0777 /srv/kv-zalo-bundle
#      docker run --rm --tmpfs /tmp:rw,size=1536m -v /srv/kv-zalo-bundle:/bundle kv-zalo:latest --fetch-asar /bundle
#
#   3. Run stateless read-only relay (zero writable filesystems):
#      docker run -d --name kv-zalo-web \
#        --read-only \
#        --cap-drop=ALL \
#        --security-opt=no-new-privileges:true \
#        --ulimit core=0 \
#        --memory-swappiness=0 \
#        -p 127.0.0.1:8080:8080 \
#        -v /srv/kv-zalo-bundle/current/Contents/Resources/app.asar:/opt/kv-zalo/app.asar:ro \
#        -e KVZALO_WEB_TOKEN="replace-with-strong-secret" \
#        kv-zalo:latest
# ==============================================================================

FROM rust:1-bookworm AS builder

WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# `olaz-web` + `olaz-core` + `olaz-net` are 100% pure Rust (`rustls` + `zbus`, zero GTK/WebKit linkage)
RUN cargo build --release --locked -p olaz-web && strip /src/target/release/kv-zalo-web

# ------------------------------------------------------------------------------
# Runtime Stage: Minimal read-only rootfs, non-root UID 65532, zero writable dirs
# ------------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    p7zip-full \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 65532 kvzalo \
    && useradd -u 65532 -g 65532 -d /nonexistent -s /usr/sbin/nologin -M kvzalo

COPY --from=builder /src/target/release/kv-zalo-web /usr/local/bin/kv-zalo-web
RUN ln -sf kv-zalo-web /usr/local/bin/olaz-web

ENV KVZALO_WEB_LISTEN=0.0.0.0:8080
ENV KVZALO_ASAR_PATH=/opt/kv-zalo/app.asar

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/__olaz/health >/dev/null || exit 1

USER 65532:65532

ENTRYPOINT ["/usr/local/bin/kv-zalo-web"]
