# ==============================================================================
# Olaz Web (`olaz-web`) — Zero-Server-Persistence Hardened Container Image
# ==============================================================================
#
# Option A — Docker Compose (recommended one-command setup):
#   OLAZ_WEB_TOKEN="replace-with-strong-secret" docker compose up -d --build
#
# Option B — Standalone Docker CLI:
#   1. Build image:
#      docker build -t olaz-web:latest .
#
#   2. Fetch & SHA-256 verify official VNG `app.asar` once onto host:
#      mkdir -p /srv/olaz-bundle && chmod 0777 /srv/olaz-bundle
#      docker run --rm --tmpfs /tmp:rw,size=1536m -v /srv/olaz-bundle:/bundle olaz-web:latest --fetch-asar /bundle
#
#   3. Run stateless read-only relay (zero writable filesystems):
#      docker run -d --name olaz-web \
#        --read-only \
#        --cap-drop=ALL \
#        --security-opt=no-new-privileges:true \
#        --ulimit core=0 \
#        --memory-swappiness=0 \
#        -p 127.0.0.1:8080:8080 \
#        -v /srv/olaz-bundle/current/Contents/Resources/app.asar:/opt/olaz/app.asar:ro \
#        -e OLAZ_WEB_TOKEN="replace-with-strong-secret" \
#        olaz-web:latest
# ==============================================================================

FROM rust:1-bookworm AS builder

WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# `olaz-web` + `olaz-core` + `olaz-net` are 100% pure Rust (`rustls` + `zbus`, zero GTK/WebKit linkage)
RUN cargo build --release --locked -p olaz-web && strip /src/target/release/olaz-web

# ------------------------------------------------------------------------------
# Runtime Stage: Minimal read-only rootfs, non-root UID 65532, zero writable dirs
# ------------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    p7zip-full \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 65532 olaz \
    && useradd -u 65532 -g 65532 -d /nonexistent -s /usr/sbin/nologin -M olaz

COPY --from=builder /src/target/release/olaz-web /usr/local/bin/olaz-web

ENV OLAZ_WEB_LISTEN=0.0.0.0:8080
ENV OLAZ_ASAR_PATH=/opt/olaz/app.asar

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/__olaz/health >/dev/null || exit 1

USER 65532:65532

ENTRYPOINT ["/usr/local/bin/olaz-web"]
