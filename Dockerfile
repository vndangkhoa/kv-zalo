# ==============================================================================
# KV Zalo Web (`kv-zalo-web`) — Zero-Server-Persistence Hardened Container Image
# ==============================================================================
#
# One-command run (pre-bundles SHA-256-verified official VNG `app.asar`,
# requires ZERO volumes and runs strictly `--read-only`):
#
#   docker run -d --name kv-zalo \
#     --read-only \
#     --cap-drop=ALL \
#     --security-opt=no-new-privileges:true \
#     --ulimit core=0 \
#     -p 8080:8080 \
#     --restart unless-stopped \
#     vndangkhoa/kv-zalo:latest
# ==============================================================================

FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    p7zip-full \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Build pure-Rust `kv-zalo-web` and fetch + SHA-256 verify official VNG `app.asar` once at build time
RUN cargo build --release --locked -p olaz-web \
    && strip /src/target/release/kv-zalo-web \
    && /src/target/release/kv-zalo-web --fetch-asar /opt/kv-zalo-bundle

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
COPY --from=builder /opt/kv-zalo-bundle/current/Contents/Resources/app.asar /opt/kv-zalo/app.asar
RUN ln -sf kv-zalo-web /usr/local/bin/olaz-web && chmod 0444 /opt/kv-zalo/app.asar

ENV KVZALO_WEB_LISTEN=0.0.0.0:8080
ENV KVZALO_ASAR_PATH=/opt/kv-zalo/app.asar

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/__olaz/health >/dev/null || exit 1

USER 65532:65532

ENTRYPOINT ["/usr/local/bin/kv-zalo-web"]
