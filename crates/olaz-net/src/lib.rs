//! Native network proxy, RFC 6265 domain cookie jar, realtime WebSockets,
//! Zalo-scoped DoH resolver, concurrent cached media loader, HTTP 206 byte-range
//! media server, and streaming E2EE / `pcloud` decryptor.

pub mod cookies;
pub mod doh;
pub mod media_loader;
pub mod proxy;
pub mod secure_stream;

pub use cookies::{
    is_zalo_cookie_domain, CookieEntry, ZaloCookieJar, AUTH_DOMAIN, SESSION_COOKIE_DOMAINS,
};
pub use doh::{
    is_zalo_cdn, DnsBackend, DnsResolveError, ZaloDnsResolver, DEAD_HOST_TTL, DOH_INITIAL_TTL,
    DOH_STICKY_TTL, SYSTEM_DNS_TIMEOUT, SYSTEM_OK_TTL, ZALO_CDN_SUFFIXES,
};
pub use media_loader::{
    fnv1a_64_hex, open_local_media_range, sanitize_headers, serve_local_media_range,
    sniff_media_mime, strip_sensitive_headers_for_redirect, trim_cache_dir, LocalMediaRangeReader,
    LocalMediaStream, MediaLoadError, MediaLoader, MediaRangeResponse, DEFAULT_CONCURRENCY_LIMIT,
    DEFAULT_FILES_CACHE_LIMIT, DEFAULT_IMAGE_DEADLINE, FILE_IDLE_TIMEOUT, FORBIDDEN_HEADER_NAMES,
    MAX_MEDIA_FILE_BYTES, MAX_PHOTO_BYTES, MAX_REDIRECT_HOPS, SENSITIVE_HEADER_NAMES,
    STALE_PART_AGE,
};
pub use proxy::{
    describe_zalo_frame, is_observed_zalo_endpoint, NetworkProxy, ResponseObserver, WsEvent,
    WsEventSink, DEFAULT_HTTP_TIMEOUT, DEFAULT_USER_AGENT, MAX_WS_MESSAGE_BYTES, WEBSOCKET_ORIGIN,
};
pub use secure_stream::{
    Aes256CtrBlock, Aes256CtrLinear, Aes256GcmLinear, DecryptError, DownloadFailure,
    DownloadManager, DownloadOutcome, SecureMode, SecureSpec, SecureStream, DOWNLOAD_IPC_CHANNEL,
    DOWNLOAD_REQ_ABORT, DOWNLOAD_REQ_START, DOWNLOAD_RESP_CHANNEL, GCM_ONE_SHOT_CAP,
    NOT_FOUND_CACHE_CAPACITY, NOT_FOUND_CACHE_TTL, STREAM_CHUNK_BYTES,
};
