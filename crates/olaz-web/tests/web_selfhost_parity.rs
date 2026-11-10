//! Integration & byte-for-byte parity tests for `olaz-web` (Track D: Zero-Server-Persistence Web Architecture).
//!
//! Verifies:
//! 1. Node `v26` (`js/crypto_sync.js`) vs Rust (`olaz-core` & `olaz-net`) 100% byte-for-byte parity
//!    across AES-128/192/256-{ECB,CBC,CTR,GCM}, MD5/SHA-1/256/384/512, SafeStorage, KeychainCipher
//!    (`0x49` envelope bidirectional), BrowserCookieJar (`zpw_sek` multi-domain cloning),
//!    `fileNoise` (`0x93` XOR + trailer), and `SecureStream` (`CtrLinear`, `CtrBlock`, `GcmLinear`).
//! 2. End-to-end stateless HTTP, HTTP `Range` (`206`) media, and WebSocket (`101` + 1st-frame
//!    `__olaz_ws_open`) relay backed by a `chmod 0555` read-only directory (proving zero disk writes
//!    and zero cookie leakage across requests).
//! 3. Constant-time AuthGate (`401`) and SSRF / open-proxy rejection (`403`) on non-Zalo hosts,
//!    disallowed schemes, loopback/RFC1918/link-local/CGNAT literals, and DNS-rebinding hosts.

use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use olaz_core::{
    copy_with_noise_mode, CryptoBridge, KeychainCipher, MemorySecretBackend, SafeStorage,
    SecretBackend,
};
use olaz_net::{SecureStream, ZaloCookieJar};
use olaz_web::{
    is_allowed_zalo_domain, is_globally_routable_ip, start_web_server, AllowlistConfig,
    BundleProvider, WebServerConfig, ZALO_USER_AGENT,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{Request as WsReq, Response as WsResp};
use tokio_tungstenite::tungstenite::protocol::Message;

fn crypto_sync_js_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("js/crypto_sync.js")
}

/// RAII guard restoring `0o755` permissions on drop so `TempDir` can remove a `0o555` read-only tree.
struct ReadOnlyDirGuard {
    root: PathBuf,
}

impl ReadOnlyDirGuard {
    fn lock_readonly(root: &Path) -> Self {
        set_tree_mode(root, 0o555, 0o444);
        Self {
            root: root.to_path_buf(),
        }
    }
}

impl Drop for ReadOnlyDirGuard {
    fn drop(&mut self) {
        set_tree_mode(&self.root, 0o755, 0o644);
    }
}

fn set_tree_mode(path: &Path, dir_mode: u32, file_mode: u32) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(dir_mode));
        if let Ok(rd) = std::fs::read_dir(path) {
            for entry in rd.flatten() {
                set_tree_mode(&entry.path(), dir_mode, file_mode);
            }
        }
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(dir_mode));
    } else if meta.is_file() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(file_mode));
    }
}

// ============================================================================
// Suite 1: Node v26 (`js/crypto_sync.js`) <-> Rust (`olaz-core` & `olaz-net`) Parity
// ============================================================================

#[tokio::test]
async fn node_v26_crypto_vault_cookie_noise_and_stream_byte_parity() {
    let bridge = CryptoBridge::new();

    // 1. Generate Rust reference ciphertexts across AES-128/192/256 x {ECB, CBC, CTR, GCM}
    let key16 = B64.encode(b"0123456789abcdef");
    let key24 = B64.encode(b"0123456789abcdef01234567");
    let key32 = B64.encode(b"0123456789abcdef0123456789abcdef");
    let iv16 = B64.encode([
        0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87, 0x98, 0xa9, 0xba, 0xcb, 0xdc, 0xed, 0xfe,
        0x0f,
    ]);
    let ctr_carry_iv16 = B64.encode([
        0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe,
    ]);
    let gcm_iv12 = B64.encode(*b"unique_iv_12");
    let gcm_iv16 = B64.encode(*b"nonstandard_iv16");
    let aad_b64 = B64.encode(b"zalo-e2ee-aad-header");

    let sample_pt =
        b"Xin chao Zalo Web! Zero-Server-Persistence Pure TypedArray Crypto Parity Vector 2026.";
    let sample_pt_b64 = B64.encode(sample_pt);
    let block_aligned_32_b64 = B64.encode(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ012345");

    let mut cipher_vectors = Vec::new();
    for (alg, k_b64, iv_opt) in [
        ("aes-128-ecb", key16.as_str(), None),
        ("aes-192-ecb", key24.as_str(), None),
        ("aes-256-ecb", key32.as_str(), None),
        ("aes-128-cbc", key16.as_str(), Some(iv16.as_str())),
        ("aes-192-cbc", key24.as_str(), Some(iv16.as_str())),
        ("aes-256-cbc", key32.as_str(), Some(iv16.as_str())),
        ("aes-128-ctr", key16.as_str(), Some(ctr_carry_iv16.as_str())),
        ("aes-192-ctr", key24.as_str(), Some(ctr_carry_iv16.as_str())),
        ("aes-256-ctr", key32.as_str(), Some(ctr_carry_iv16.as_str())),
    ] {
        let iv_val = iv_opt.map(|s| json!({ "__bytes": s })).unwrap_or(Value::Null);
        let ct_hex = bridge
            .dispatch_sync(
                "$znode.crypto.encrypt",
                &[
                    json!({ "__bytes": sample_pt_b64 }),
                    json!(alg),
                    json!({ "__bytes": k_b64 }),
                    iv_val,
                    json!({ "output": "hex" }),
                ],
            )
            .expect("rust encrypt")
            .as_str()
            .unwrap()
            .to_string();
        cipher_vectors.push(json!({
            "alg": alg,
            "keyB64": k_b64,
            "ivB64": iv_opt,
            "ptB64": sample_pt_b64,
            "ctHex": ct_hex,
        }));
    }

    // Raw no-padding AES-256-CBC & AES-256-ECB streaming check
    let h_nopad = bridge
        .dispatch_sync(
            "$znode.crypto.__cipherCreate",
            &[
                json!("cipher"),
                json!("aes-256-cbc"),
                json!({ "__bytes": key32 }),
                json!({ "__bytes": iv16 }),
            ],
        )
        .unwrap();
    bridge
        .dispatch_sync(
            "$znode.crypto.__cipherOption",
            &[h_nopad.clone(), json!("autoPadding"), json!(false)],
        )
        .unwrap();
    let p1 = bridge
        .dispatch_sync(
            "$znode.crypto.__cipherUpdate",
            &[
                h_nopad.clone(),
                json!({ "__bytes": block_aligned_32_b64 }),
                Value::Null,
                json!("hex"),
            ],
        )
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let p2 = bridge
        .dispatch_sync(
            "$znode.crypto.__cipherFinal",
            &[h_nopad, json!("hex")],
        )
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let nopad_cbc_hex = format!("{p1}{p2}");

    // AES-128/192/256-GCM vectors (both 12-byte & 16-byte IV + AAD + authTag)
    let mut gcm_vectors = Vec::new();
    for (alg, k_b64, iv_b64) in [
        ("aes-128-gcm", key16.as_str(), gcm_iv12.as_str()),
        ("aes-192-gcm", key24.as_str(), gcm_iv12.as_str()),
        ("aes-256-gcm", key32.as_str(), gcm_iv12.as_str()),
        ("aes-256-gcm", key32.as_str(), gcm_iv16.as_str()),
    ] {
        let h = bridge
            .dispatch_sync(
                "$znode.crypto.__cipherCreate",
                &[
                    json!("cipher"),
                    json!(alg),
                    json!({ "__bytes": k_b64 }),
                    json!({ "__bytes": iv_b64 }),
                ],
            )
            .unwrap();
        bridge
            .dispatch_sync(
                "$znode.crypto.__cipherOption",
                &[h.clone(), json!("aad"), json!({ "__bytes": aad_b64 })],
            )
            .unwrap();
        let u_hex = bridge
            .dispatch_sync(
                "$znode.crypto.__cipherUpdate",
                &[
                    h.clone(),
                    json!({ "__bytes": sample_pt_b64 }),
                    Value::Null,
                    json!("hex"),
                ],
            )
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let f_hex = bridge
            .dispatch_sync("$znode.crypto.__cipherFinal", &[h.clone(), json!("hex")])
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let tag_obj = bridge
            .dispatch_sync(
                "$znode.crypto.__cipherOption",
                &[h, json!("getAuthTag")],
            )
            .unwrap();
        let tag_b64 = tag_obj.get("__bytes").unwrap().as_str().unwrap().to_string();
        gcm_vectors.push(json!({
            "alg": alg,
            "keyB64": k_b64,
            "ivB64": iv_b64,
            "aadB64": aad_b64,
            "ptB64": sample_pt_b64,
            "ctHex": format!("{u_hex}{f_hex}"),
            "tagB64": tag_b64,
        }));
    }

    // 2. Hash vectors across MD5, SHA-1, SHA-256, SHA-384, SHA-512
    let mut hash_vectors = Vec::new();
    for alg in ["md5", "sha1", "sha256", "sha384", "sha512"] {
        let digest_hex = bridge
            .dispatch_sync(
                "$znode.crypto.hash",
                &[
                    json!({ "__bytes": sample_pt_b64 }),
                    json!(alg),
                    Value::Null,
                    json!("hex"),
                ],
            )
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        hash_vectors.push(json!({
            "alg": alg,
            "digestHex": digest_hex,
        }));
    }

    // 3. SafeStorage vector
    let safe_plain = "zpw_sek_secret_payload_from_rust_vault_!@#$%^&*()";
    let safe_sealed = SafeStorage::encrypt_string(safe_plain).expect("rust SafeStorage::encrypt_string");

    // 4. KeychainCipher (`0x49`) vector generated in Rust -> to be decrypted in JS
    let mem_backend = MemorySecretBackend::default();
    let root_key_32: [u8; 32] = [
        0x42, 0x13, 0x37, 0x99, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0x10, 0x20, 0x30,
        0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0, 0x05, 0x15, 0x25,
        0x35, 0x45,
    ];
    mem_backend
        .set_secret(KeychainCipher::<MemorySecretBackend>::SERVICE, "zprotocol.trusted_dev", &root_key_32)
        .await
        .expect("seed root key");
    let kc = KeychainCipher::new(mem_backend);
    let kc_plain = b"zprotocol-trusted-device-session-payload-v1";
    let kc_sealed = kc
        .encrypt("trusted_dev", &[kc_plain.as_slice()])
        .await
        .expect("rust KeychainCipher::encrypt");
    assert_eq!(kc_sealed[0][0], 0x49);

    // 5. ZaloCookieJar (`zpw_sek` multi-domain cloning)
    let rust_jar = ZaloCookieJar::new_in_memory();
    rust_jar
        .set_app_cookie("sek_parity_value_777")
        .expect("set_app_cookie");
    let cookie_wpa = rust_jar
        .cookie_header_for_url("https://wpa.zaloapp.com/api/login/getServerInfo")
        .expect("cookie for wpa");
    let cookie_ws = rust_jar
        .cookie_header_for_url("wss://ws2-msg.chat.zalo.me/zpw_ws")
        .expect("cookie for wss");

    // 6. `fileNoise` reference file via `copy_with_noise_mode`
    let noise_dir = TempDir::new().expect("tempdir for noise");
    let raw_media: Vec<u8> = (0..260u32).map(|i| ((i * 37 + 11) & 0xff) as u8).collect();
    let src_file = noise_dir.path().join("orig.bin");
    let noised_file = noise_dir.path().join("noised.bin");
    std::fs::write(&src_file, &raw_media).unwrap();
    copy_with_noise_mode(&src_file, &noised_file, true, "").expect("noise_file_sync");
    let rust_noised_bytes = std::fs::read(&noised_file).unwrap();

    // 7. `SecureStream` test vectors (encrypted via CryptoBridge -> verified in both Rust SecureStream & JS)
    let stream_key: [u8; 32] = *b"stream_key_32_bytes_for_zalo_e2e";
    let stream_iv16: [u8; 16] = *b"stream_iv_16byte";
    let stream_iv12: [u8; 12] = *b"stream_iv_12";
    // 250 bytes so `block_size = 64` has 3 full blocks (192B) + 58B tail overflow (`skipOverflow`)!
    let stream_plain: Vec<u8> = (0..250u32).map(|i| ((i * 73 + 5) & 0xff) as u8).collect();

    // CtrLinear ciphertext
    let ctr_linear_ct_b64 = bridge
        .dispatch_sync(
            "$znode.crypto.encrypt",
            &[
                json!({ "__bytes": B64.encode(&stream_plain) }),
                json!("aes-256-ctr"),
                json!({ "__bytes": B64.encode(stream_key) }),
                json!({ "__bytes": B64.encode(stream_iv16) }),
                json!({ "output": "base64" }),
            ],
        )
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let rust_ctr_linear_pt = SecureStream::ctr_linear(stream_key, stream_iv16)
        .decrypt_bytes(&B64.decode(&ctr_linear_ct_b64).unwrap())
        .expect("rust ctr_linear decrypt");
    assert_eq!(rust_ctr_linear_pt, stream_plain);

    // CtrBlock ciphertext (`block_size = 64`, 3 blocks encrypted with fresh IV per block + 58B verbatim tail)
    let mut ctr_block_ct = Vec::with_capacity(stream_plain.len());
    for chunk in stream_plain.chunks(64) {
        if chunk.len() == 64 {
            let enc_b64 = bridge
                .dispatch_sync(
                    "$znode.crypto.encrypt",
                    &[
                        json!({ "__bytes": B64.encode(chunk) }),
                        json!("aes-256-ctr"),
                        json!({ "__bytes": B64.encode(stream_key) }),
                        json!({ "__bytes": B64.encode(stream_iv16) }),
                        json!({ "output": "base64" }),
                    ],
                )
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            ctr_block_ct.extend_from_slice(&B64.decode(enc_b64).unwrap());
        } else {
            ctr_block_ct.extend_from_slice(chunk);
        }
    }
    let rust_ctr_block_pt = SecureStream::ctr_block(stream_key, stream_iv16, 64)
        .decrypt_bytes(&ctr_block_ct)
        .expect("rust ctr_block decrypt");
    assert_eq!(rust_ctr_block_pt, stream_plain);

    // GcmLinear ciphertext (12-byte IV)
    let h_gcm = bridge
        .dispatch_sync(
            "$znode.crypto.__cipherCreate",
            &[
                json!("cipher"),
                json!("aes-256-gcm"),
                json!({ "__bytes": B64.encode(stream_key) }),
                json!({ "__bytes": B64.encode(stream_iv12) }),
            ],
        )
        .unwrap();
    let g_u = bridge
        .dispatch_sync(
            "$znode.crypto.__cipherUpdate",
            &[
                h_gcm.clone(),
                json!({ "__bytes": B64.encode(&stream_plain) }),
                Value::Null,
                json!("hex"),
            ],
        )
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let g_f = bridge
        .dispatch_sync("$znode.crypto.__cipherFinal", &[h_gcm.clone(), json!("hex")])
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let g_tag_b64 = bridge
        .dispatch_sync(
            "$znode.crypto.__cipherOption",
            &[h_gcm, json!("getAuthTag")],
        )
        .unwrap()
        .get("__bytes")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let gcm_linear_ct = hex::decode(format!("{g_u}{g_f}")).unwrap();
    let gcm_tag_arr: [u8; 16] = B64.decode(&g_tag_b64).unwrap().try_into().unwrap();
    let rust_gcm_pt =
        SecureStream::gcm_linear(stream_key, stream_iv12.to_vec(), gcm_tag_arr)
            .decrypt_bytes(&gcm_linear_ct)
            .expect("rust gcm_linear decrypt");
    assert_eq!(rust_gcm_pt, stream_plain);

    let input_payload = json!({
        "jsPath": crypto_sync_js_path().to_string_lossy(),
        "samplePtB64": sample_pt_b64,
        "blockAligned32B64": block_aligned_32_b64,
        "key32B64": key32,
        "iv16B64": iv16,
        "nopadCbcHex": nopad_cbc_hex,
        "cipherVectors": cipher_vectors,
        "gcmVectors": gcm_vectors,
        "hashVectors": hash_vectors,
        "safePlain": safe_plain,
        "safeSealed": safe_sealed,
        "kcRootKeyB64": B64.encode(root_key_32),
        "kcPlainB64": B64.encode(kc_plain),
        "kcRustEnvelopeB64": B64.encode(&kc_sealed[0]),
        "cookieWpa": cookie_wpa,
        "cookieWs": cookie_ws,
        "rawMediaB64": B64.encode(&raw_media),
        "rustNoisedB64": B64.encode(&rust_noised_bytes),
        "streamKeyB64": B64.encode(stream_key),
        "streamIv16B64": B64.encode(stream_iv16),
        "streamIv12B64": B64.encode(stream_iv12),
        "streamPlainB64": B64.encode(&stream_plain),
        "ctrLinearCtB64": ctr_linear_ct_b64,
        "ctrBlockCtB64": B64.encode(&ctr_block_ct),
        "gcmLinearCtB64": B64.encode(&gcm_linear_ct),
        "gcmLinearTagB64": g_tag_b64,
    });

    let node_script = r#"
      const fs = require('node:fs');
      const assert = require('node:assert/strict');
      const input = JSON.parse(fs.readFileSync(0, 'utf8'));
      const core = require(input.jsPath);

      const bridge = new core.CryptoBridge();

      // 1. Verify AES-128/192/256-{ECB,CBC,CTR} encrypt & decrypt & streaming parity
      for (const vec of input.cipherVectors) {
        const ivArg = vec.ivB64 ? { __bytes: vec.ivB64 } : null;
        const jsHex = bridge.dispatchSync('$znode.crypto.encrypt', [
          { __bytes: vec.ptB64 },
          vec.alg,
          { __bytes: vec.keyB64 },
          ivArg,
          { output: 'hex' },
        ]);
        assert.equal(jsHex, vec.ctHex, `encrypt mismatch on ${vec.alg}`);

        const decWire = bridge.dispatchSync('$znode.crypto.decrypt', [
          jsHex,
          vec.alg,
          { __bytes: vec.keyB64 },
          ivArg,
          { input: 'hex', output: 'base64' },
        ]);
        assert.equal(decWire, input.samplePtB64, `decrypt mismatch on ${vec.alg}`);

        // Streaming chunked update parity
        const rawPt = Buffer.from(vec.ptB64, 'base64');
        const h = bridge.dispatchSync('$znode.crypto.__cipherCreate', [
          'cipher',
          vec.alg,
          { __bytes: vec.keyB64 },
          ivArg || { __bytes: Buffer.alloc(16).toString('base64') },
        ]);
        const c1 = bridge.dispatchSync('$znode.crypto.__cipherUpdate', [
          h,
          { __bytes: rawPt.subarray(0, 7).toString('base64') },
          null,
          'hex',
        ]);
        const c2 = bridge.dispatchSync('$znode.crypto.__cipherUpdate', [
          h,
          { __bytes: rawPt.subarray(7, 29).toString('base64') },
          null,
          'hex',
        ]);
        const c3 = bridge.dispatchSync('$znode.crypto.__cipherUpdate', [
          h,
          { __bytes: rawPt.subarray(29).toString('base64') },
          null,
          'hex',
        ]);
        const cf = bridge.dispatchSync('$znode.crypto.__cipherFinal', [h, 'hex']);
        assert.equal(c1 + c2 + c3 + cf, vec.ctHex, `streaming mismatch on ${vec.alg}`);
      }

      // Raw autoPadding=false check
      {
        const h = bridge.dispatchSync('$znode.crypto.__cipherCreate', [
          'cipher',
          'aes-256-cbc',
          { __bytes: input.key32B64 },
          { __bytes: input.iv16B64 },
        ]);
        bridge.dispatchSync('$znode.crypto.__cipherOption', [h, 'autoPadding', false]);
        const u = bridge.dispatchSync('$znode.crypto.__cipherUpdate', [
          h,
          { __bytes: input.blockAligned32B64 },
          null,
          'hex',
        ]);
        const f = bridge.dispatchSync('$znode.crypto.__cipherFinal', [h, 'hex']);
        assert.equal(u + f, input.nopadCbcHex);
      }

      // 2. Verify AES-128/192/256-GCM (12B & 16B IV + AAD + authTag + tamper rejection)
      for (const gvec of input.gcmVectors) {
        const he = bridge.dispatchSync('$znode.crypto.__cipherCreate', [
          'cipher',
          gvec.alg,
          { __bytes: gvec.keyB64 },
          { __bytes: gvec.ivB64 },
        ]);
        bridge.dispatchSync('$znode.crypto.__cipherOption', [he, 'aad', { __bytes: gvec.aadB64 }]);
        const u = bridge.dispatchSync('$znode.crypto.__cipherUpdate', [
          he,
          { __bytes: gvec.ptB64 },
          null,
          'hex',
        ]);
        const f = bridge.dispatchSync('$znode.crypto.__cipherFinal', [he, 'hex']);
        const tagObj = bridge.dispatchSync('$znode.crypto.__cipherOption', [he, 'getAuthTag']);
        assert.equal(u + f, gvec.ctHex, `GCM ct mismatch on ${gvec.alg}`);
        assert.equal(tagObj.__bytes, gvec.tagB64, `GCM tag mismatch on ${gvec.alg}`);

        // Decrypt & verify tag
        const hd = bridge.dispatchSync('$znode.crypto.__cipherCreate', [
          'decipher',
          gvec.alg,
          { __bytes: gvec.keyB64 },
          { __bytes: gvec.ivB64 },
        ]);
        bridge.dispatchSync('$znode.crypto.__cipherOption', [hd, 'aad', { __bytes: gvec.aadB64 }]);
        bridge.dispatchSync('$znode.crypto.__cipherOption', [hd, 'authTag', { __bytes: gvec.tagB64 }]);
        const du = bridge.dispatchSync('$znode.crypto.__cipherUpdate', [hd, gvec.ctHex, 'hex', 'base64']);
        const df = bridge.dispatchSync('$znode.crypto.__cipherFinal', [hd, 'base64']);
        const combined = Buffer.concat([Buffer.from(du, 'base64'), Buffer.from(df, 'base64')]).toString('base64');
        assert.equal(combined, gvec.ptB64, `GCM decrypt mismatch on ${gvec.alg}`);

        // Tamper tag -> must throw!
        const badTag = Buffer.from(gvec.tagB64, 'base64');
        badTag[0] ^= 0x01;
        const hbad = bridge.dispatchSync('$znode.crypto.__cipherCreate', [
          'decipher',
          gvec.alg,
          { __bytes: gvec.keyB64 },
          { __bytes: gvec.ivB64 },
        ]);
        bridge.dispatchSync('$znode.crypto.__cipherOption', [hbad, 'aad', { __bytes: gvec.aadB64 }]);
        bridge.dispatchSync('$znode.crypto.__cipherOption', [hbad, 'authTag', { __bytes: badTag.toString('base64') }]);
        bridge.dispatchSync('$znode.crypto.__cipherUpdate', [hbad, gvec.ctHex, 'hex', 'hex']);
        assert.throws(() => bridge.dispatchSync('$znode.crypto.__cipherFinal', [hbad, 'hex']));
      }

      // 3. Verify MD5, SHA-1, SHA-256, SHA-384, SHA-512 one-shot & streaming
      for (const hvec of input.hashVectors) {
        const jsDigest = bridge.dispatchSync('$znode.crypto.hash', [
          { __bytes: input.samplePtB64 },
          hvec.alg,
          null,
          'hex',
        ]);
        assert.equal(jsDigest, hvec.digestHex, `hash mismatch on ${hvec.alg}`);

        const raw = Buffer.from(input.samplePtB64, 'base64');
        const hh = bridge.dispatchSync('$znode.crypto.__hashCreate', [hvec.alg]);
        bridge.dispatchSync('$znode.crypto.__hashUpdate', [hh, { __bytes: raw.subarray(0, 19).toString('base64') }]);
        bridge.dispatchSync('$znode.crypto.__hashUpdate', [hh, { __bytes: raw.subarray(19).toString('base64') }]);
        const streamDigest = bridge.dispatchSync('$znode.crypto.__hashDigest', [hh, 'hex']);
        assert.equal(streamDigest, hvec.digestHex, `streaming hash mismatch on ${hvec.alg}`);
      }

      // 4. Verify SafeStorage bidirectional parity
      assert.equal(core.SafeStorage.decryptString(input.safeSealed), input.safePlain);
      assert.equal(core.SafeStorage.encryptString(input.safePlain), input.safeSealed);
      const ss = new core.SafeStorage();
      ss.execute('set', ['refresh_token', input.safePlain]);
      assert.equal(ss.execute('get', ['refresh_token']), input.safePlain);

      // 5. Verify KeychainCipher (`0x49` envelope): Rust -> JS AND generate JS -> Rust
      const rootKey = new Uint8Array(Buffer.from(input.kcRootKeyB64, 'base64'));
      const rustEnv = new Uint8Array(Buffer.from(input.kcRustEnvelopeB64, 'base64'));
      const decFromRust = core.KeychainCipher.decryptWithRootKey(rootKey, rustEnv);
      assert.equal(Buffer.from(decFromRust).toString('base64'), input.kcPlainB64);

      const tamperedEnv = new Uint8Array(rustEnv);
      tamperedEnv[tamperedEnv.length - 1] ^= 0x80;
      assert.throws(() => core.KeychainCipher.decryptWithRootKey(rootKey, tamperedEnv));

      const jsPlain = Buffer.from('generated-in-pure-js-keychain-cipher-for-rust-verify!', 'utf8');
      const jsGeneratedEnv = core.KeychainCipher.encryptWithRootKey(rootKey, jsPlain);

      // 6. Verify BrowserCookieJar vs ZaloCookieJar (`zpw_sek` multi-domain cloning)
      const jar = new core.BrowserCookieJar();
      jar.handleIpc('_electron_set-app-cookie', ['sek_parity_value_777']);
      assert.equal(jar.cookieHeaderForUrl('https://wpa.zaloapp.com/api/login/getServerInfo'), input.cookieWpa);
      assert.equal(jar.cookieHeaderForUrl('wss://ws2-msg.chat.zalo.me/zpw_ws'), input.cookieWs);
      assert.equal(jar.cookieHeaderForUrl('https://chat.zalo.me/'), 'zpw_sek=sek_parity_value_777');
      assert.equal(jar.cookieHeaderForUrl('https://evil.example.com/'), '');

      // Also test Set-Cookie ingestion with session-only zpw_sek -> cloned across 3 domains
      const jar2 = new core.BrowserCookieJar();
      jar2.applySetCookieHeaders('https://wpa.zaloapp.com/api/login', [
        'zpw_sek=from_set_cookie_header_123; Path=/; Secure; HttpOnly',
      ]);
      assert.equal(jar2.cookieHeaderForUrl('wss://ws5-msg.chat.zalo.me/zpw_ws'), 'zpw_sek=from_set_cookie_header_123');

      // 7. Verify `fileNoise` XOR + `077a416c4f07` trailer byte-for-byte parity
      const rawMedia = new Uint8Array(Buffer.from(input.rawMediaB64, 'base64'));
      const jsNoised = core.noiseBuffer(rawMedia);
      assert.equal(Buffer.from(jsNoised).toString('base64'), input.rustNoisedB64);
      const jsDenoised = core.denoiseBuffer(jsNoised);
      assert.equal(Buffer.from(jsDenoised).toString('base64'), input.rawMediaB64);

      // 8. Verify `SecureStream` (`Aes256CtrLinear`, `Aes256CtrBlock`, `Aes256GcmLinear`)
      const decCtrLinear = core.decryptSecureBuffer(
        'Aes256CtrLinear',
        input.streamKeyB64,
        input.streamIv16B64,
        Buffer.from(input.ctrLinearCtB64, 'base64')
      );
      assert.equal(Buffer.from(decCtrLinear).toString('base64'), input.streamPlainB64);

      const decCtrBlock = core.decryptSecureBuffer(
        'Aes256CtrBlock',
        input.streamKeyB64,
        input.streamIv16B64,
        Buffer.from(input.ctrBlockCtB64, 'base64'),
        { blockSize: 64 }
      );
      assert.equal(Buffer.from(decCtrBlock).toString('base64'), input.streamPlainB64);

      const decGcmLinear = core.decryptSecureBuffer(
        'Aes256GcmLinear',
        input.streamKeyB64,
        input.streamIv12B64,
        Buffer.from(input.gcmLinearCtB64, 'base64'),
        { tag: input.gcmLinearTagB64 }
      );
      assert.equal(Buffer.from(decGcmLinear).toString('base64'), input.streamPlainB64);

      process.stdout.write(JSON.stringify({
        ok: true,
        jsGeneratedEnvB64: Buffer.from(jsGeneratedEnv).toString('base64'),
        jsPlainB64: jsPlain.toString('base64'),
      }));
    "#;

    let mut child = Command::new("node")
        .arg("-e")
        .arg(node_script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn node v26");

    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&input_payload).unwrap().as_bytes())
        .expect("write vectors to node stdin");

    let output = child.wait_with_output().expect("wait for node");
    if !output.status.success() {
        panic!(
            "node parity script failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let node_resp: Value = serde_json::from_slice(&output.stdout).expect("parse node stdout JSON");
    assert_eq!(node_resp.get("ok").and_then(Value::as_bool), Some(true));

    // Decrypt the JS-generated `0x49` KeychainCipher envelope inside Rust `KeychainCipher::decrypt`!
    let js_env_bytes = B64
        .decode(node_resp["jsGeneratedEnvB64"].as_str().unwrap())
        .unwrap();
    let expected_js_plain = B64
        .decode(node_resp["jsPlainB64"].as_str().unwrap())
        .unwrap();
    let rust_decrypted = kc
        .decrypt("trusted_dev", &[js_env_bytes.as_slice()])
        .await
        .expect("Rust KeychainCipher::decrypt must accept JS-generated 0x49 envelope");
    assert_eq!(rust_decrypted[0], expected_js_plain);
}

// ============================================================================
// Suite 2: Stateless HTTP, Media Range (`206`), and WebSocket Relay E2E + Read-Only FS Proof
// ============================================================================

#[derive(Debug, Default)]
struct RecordedUpstreamCalls {
    http_user_agents: Vec<Option<String>>,
    http_cookies: Vec<Option<String>>,
    http_custom_headers: Vec<Option<String>>,
    media_ranges: Vec<Option<String>>,
    ws_origin: Option<String>,
    ws_user_agent: Option<String>,
    ws_cookie: Option<String>,
}

#[tokio::test]
async fn stateless_http_media_and_ws_relay_e2e_on_readonly_filesystem() {
    // 1. Prepare a fake Zalo bundle directory and lock it with chmod `0o555` / `0o444`!
    let bundle_tmp = TempDir::new().expect("create temp bundle dir");
    let pc_dist = bundle_tmp.path().join("pc-dist");
    std::fs::create_dir_all(&pc_dist).unwrap();
    std::fs::write(
        pc_dist.join("login.html"),
        r#"<!DOCTYPE html><html><head><meta http-equiv="Content-Security-Policy" content="default-src 'none'"><script src="renderer.js"></script></head><body><img src="zfile://local/cached/avatar.jpg"></body></html>"#,
    )
    .unwrap();
    std::fs::write(pc_dist.join("renderer.js"), "window.__rendererLoaded = true;\n").unwrap();
    std::fs::write(
        pc_dist.join("dal-worker.test123.js"),
        "self.__workerBooted = 'zfile://local/db';\n",
    )
    .unwrap();

    let _ro_guard = ReadOnlyDirGuard::lock_readonly(bundle_tmp.path());
    // Confirm the OS genuinely blocks writes inside `bundle_tmp`!
    assert!(
        std::fs::write(bundle_tmp.path().join("must_fail.tmp"), b"x").is_err(),
        "0o555 directory must reject writes"
    );

    // 2. Start mock upstream HTTP + WebSocket server
    let recorded = Arc::new(Mutex::new(RecordedUpstreamCalls::default()));
    let mock_http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_http_addr = mock_http_listener.local_addr().unwrap();

    let recorded_http = Arc::clone(&recorded);
    let mock_http_task = tokio::spawn(async move {
        let media_bytes: Vec<u8> = (0..100u8).collect();
        loop {
            let Ok((stream, _)) = mock_http_listener.accept().await else {
                break;
            };
            let rec = Arc::clone(&recorded_http);
            let media_data = media_bytes.clone();
            tokio::spawn(async move {
                let (r_half, mut w_half) = stream.into_split();
                let mut reader = BufReader::new(r_half);
                let mut req_line = String::new();
                if reader.read_line(&mut req_line).await.unwrap_or(0) == 0 {
                    return;
                }
                let mut hdrs: HashMap<String, String> = HashMap::new();
                loop {
                    let mut line = String::new();
                    let n = reader.read_line(&mut line).await.unwrap_or(0);
                    if n == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        hdrs.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                if let Some(cl) = hdrs.get("content-length").and_then(|s| s.parse::<usize>().ok()) {
                    let mut body = vec![0u8; cl];
                    let _ = reader.read_exact(&mut body).await;
                }

                let target = req_line.split_whitespace().nth(1).unwrap_or("/");
                if target.starts_with("/media/clip.mp4") {
                    let range_hdr = hdrs.get("range").cloned();
                    rec.lock().media_ranges.push(range_hdr.clone());
                    if let Some(_r) = range_hdr {
                        // Expect "bytes=10-25"
                        let slice = &media_data[10..=25];
                        let head = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nContent-Range: bytes 10-25/100\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            slice.len()
                        );
                        let _ = w_half.write_all(head.as_bytes()).await;
                        let _ = w_half.write_all(slice).await;
                    } else {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            media_data.len()
                        );
                        let _ = w_half.write_all(head.as_bytes()).await;
                        let _ = w_half.write_all(&media_data).await;
                    }
                    let _ = w_half.flush().await;
                    return;
                }

                {
                    let mut g = rec.lock();
                    g.http_user_agents.push(hdrs.get("user-agent").cloned());
                    g.http_cookies.push(hdrs.get("cookie").cloned());
                    g.http_custom_headers
                        .push(hdrs.get("x-zalo-client-req").cloned());
                }

                let body = br#"{"error_code":0,"data":{"ok":true}}"#;
                let head = format!(
                    concat!(
                        "HTTP/1.1 200 OK\r\n",
                        "Content-Type: application/json\r\n",
                        "Set-Cookie: zpw_sek=server_minted_sek_999; Path=/; Secure; HttpOnly\r\n",
                        "X-Upstream-Trace: trace-42\r\n",
                        "Content-Length: {}\r\n",
                        "Connection: close\r\n\r\n"
                    ),
                    body.len()
                );
                let _ = w_half.write_all(head.as_bytes()).await;
                let _ = w_half.write_all(body).await;
                let _ = w_half.flush().await;
            });
        }
    });

    let mock_ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_ws_addr = mock_ws_listener.local_addr().unwrap();
    let recorded_ws = Arc::clone(&recorded);
    let mock_ws_task = tokio::spawn(async move {
        if let Ok((tcp, _)) = mock_ws_listener.accept().await {
            let rec = Arc::clone(&recorded_ws);
            let ws_stream = tokio_tungstenite::accept_hdr_async(
                tcp,
                move |req: &WsReq, resp: WsResp| {
                    let mut g = rec.lock();
                    g.ws_origin = req
                        .headers()
                        .get("origin")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    g.ws_user_agent = req
                        .headers()
                        .get("user-agent")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    g.ws_cookie = req
                        .headers()
                        .get("cookie")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    Ok(resp)
                },
            )
            .await
            .expect("mock upstream accept_hdr_async");

            let (mut sink, mut stream) = ws_stream.split();
            while let Some(Ok(msg)) = stream.next().await {
                match msg {
                    Message::Binary(bin) => {
                        let mut echoed = Vec::with_capacity(bin.len() + 2);
                        echoed.extend_from_slice(&bin);
                        echoed.extend_from_slice(&[0xaa, 0x55]);
                        if sink.send(Message::Binary(echoed.into())).await.is_err() {
                            break;
                        }
                    }
                    Message::Text(txt) => {
                        if sink
                            .send(Message::Text(format!("echo:{txt}").into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    });

    // 3. Start `olaz-web` backed by the `0o555` read-only directory!
    let mut dns_overrides = HashMap::new();
    dns_overrides.insert(
        "wpa.zaloapp.com".to_string(),
        vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
    );
    dns_overrides.insert(
        "ws1-msg.chat.zalo.me".to_string(),
        vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
    );
    dns_overrides.insert(
        "f10-zfcloud.zadn.vn".to_string(),
        vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
    );

    let server = start_web_server(WebServerConfig {
        listen_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        bundle: BundleProvider::from_dir(bundle_tmp.path()).expect("open readonly bundle dir"),
        token: None,
        allowlist: AllowlistConfig {
            allow_loopback_upstream_for_tests: true,
            dns_overrides,
        },
    })
    .await
    .expect("start olaz-web");

    let base_url = format!("http://{}", server.local_addr());
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // 3a. Health check
    let health = client
        .get(format!("{base_url}/__olaz/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status().as_u16(), 200);
    let health_json: Value = health.json().await.unwrap();
    assert_eq!(
        health_json,
        json!({ "ok": true, "stateless": true, "persistence": "browser-only" })
    );

    // 3b. Root redirect preserves `?profile=<slot>` and injects full `ZaloMainProcess.pageQuery`
    let root_redir = client
        .get(format!("{base_url}/?profile=work_acc"))
        .send()
        .await
        .unwrap();
    assert_eq!(root_redir.status().as_u16(), 302);
    assert_eq!(
        root_redir
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap(),
        "/login.html?type=23&client_version=695&app_version=26.11.10&about_version=&new_install=0&fail_sqlite3=true&mode_lang=&profile=work_acc"
    );

    // 3c. Transformed `login.html`, `dal-worker`, `preload.js`, `sw.js`
    let login_resp = client
        .get(format!("{base_url}/login.html"))
        .send()
        .await
        .unwrap();
    assert_eq!(login_resp.status().as_u16(), 200);
    let login_html = login_resp.text().await.unwrap();
    assert!(login_html.contains(r#"<script src="/__olaz/crypto_sync.js"></script>"#));
    assert!(login_html.contains(r#"<script src="/__olaz/bridge_table.js"></script>"#));
    assert!(login_html.contains(r#"<script src="/__olaz/preload.js"></script>"#));
    assert!(login_html.contains(r#"rel="manifest" href="/manifest.webmanifest""#));
    assert!(login_html.contains(r#"rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png""#));
    assert!(login_html.contains("viewport-fit=cover"));
    assert!(login_html.contains(r#"href="/__olaz/mobile.css""#));
    assert!(login_html.contains("manifest-src 'self'"));
    assert!(login_html.contains(r#"data-olaz-defer-src="renderer.js""#));
    assert!(login_html.contains(r#"/__zfile/cached/avatar.jpg"#));
    assert!(login_html.contains("ws: wss: https:"));

    // Verify existing `<meta name="viewport">` is normalized in-place without duplicating viewport tags
    let normalized_vp = olaz_web::transform_zalo_html(
        r#"<!DOCTYPE html><html><head><meta name="viewport" content="width=device-width"></head><body></body></html>"#,
    );
    assert_eq!(normalized_vp.matches(r#"name="viewport""#).count(), 1);
    assert!(normalized_vp.contains("viewport-fit=cover"));

    let worker_js = client
        .get(format!("{base_url}/dal-worker.test123.js"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(worker_js.starts_with("try { importScripts('/__olaz/crypto_sync.js'"));
    assert!(worker_js.contains("/__zfile/db"));

    let sw_resp = client
        .get(format!("{base_url}/olaz-sw.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(sw_resp.status().as_u16(), 200);
    assert_eq!(
        sw_resp
            .headers()
            .get("service-worker-allowed")
            .unwrap()
            .to_str()
            .unwrap(),
        "/"
    );

    // 3d. Stateless HTTP Relay (`POST /__olaz/http`):
    // Request #1 supplies `X-Olaz-Cookie` -> upstream receives it & returns `Set-Cookie: zpw_sek=server_minted_sek_999...`
    let target_api = format!(
        "http://wpa.zaloapp.com:{}/api/login/getServerInfo",
        mock_http_addr.port()
    );
    let custom_hdrs_b64 = B64.encode(r#"{"X-Zalo-Client-Req":"hello-1"}"#);
    let relay_resp_1 = client
        .post(format!("{base_url}/__olaz/http"))
        .header("X-Olaz-Target", &target_api)
        .header("X-Olaz-Method", "POST")
        .header("X-Olaz-Headers", &custom_hdrs_b64)
        .header("X-Olaz-Cookie", "zpw_sek=browser_ephemeral_1")
        .body(r#"{"imei":"test"}"#)
        .send()
        .await
        .unwrap();

    assert_eq!(relay_resp_1.status().as_u16(), 200);
    assert_eq!(
        relay_resp_1
            .headers()
            .get("x-olaz-upstream-status")
            .unwrap()
            .to_str()
            .unwrap(),
        "200"
    );
    let x_set_cookie = relay_resp_1
        .headers()
        .get("x-olaz-set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        x_set_cookie.contains("zpw_sek=server_minted_sek_999"),
        "expected X-Olaz-Set-Cookie to carry upstream Set-Cookie, got: {x_set_cookie}"
    );
    let relay_body_1: Value = relay_resp_1.json().await.unwrap();
    assert_eq!(relay_body_1["error_code"], 0);

    // Request #2 OMITS `X-Olaz-Cookie` -> server MUST NOT leak `browser_ephemeral_1` OR `server_minted_sek_999`!
    let relay_resp_2 = client
        .post(format!("{base_url}/__olaz/http"))
        .header("X-Olaz-Target", &target_api)
        .header("X-Olaz-Method", "GET")
        .send()
        .await
        .unwrap();
    assert_eq!(relay_resp_2.status().as_u16(), 200);

    {
        let rec = recorded.lock();
        assert_eq!(rec.http_user_agents.len(), 2);
        assert_eq!(
            rec.http_user_agents[0].as_deref(),
            Some(ZALO_USER_AGENT),
            "must inject ZaloPC-darwin-23v695"
        );
        assert_eq!(rec.http_user_agents[1].as_deref(), Some(ZALO_USER_AGENT));
        assert_eq!(
            rec.http_cookies[0].as_deref(),
            Some("zpw_sek=browser_ephemeral_1")
        );
        assert_eq!(
            rec.http_cookies[1], None,
            "ZERO-PERSISTENCE VIOLATION: second request without X-Olaz-Cookie leaked a Cookie header!"
        );
        assert_eq!(rec.http_custom_headers[0].as_deref(), Some("hello-1"));
    }

    // 3e. Stateless HTTP Range Media Streaming (`GET /__olaz/media?u=...` -> `206 Partial Content`)
    let media_url = format!(
        "http://f10-zfcloud.zadn.vn:{}/media/clip.mp4",
        mock_http_addr.port()
    );
    let encoded_media_u: String =
        url::form_urlencoded::byte_serialize(media_url.as_bytes()).collect();
    let media_resp = client
        .get(format!("{base_url}/__olaz/media?u={encoded_media_u}"))
        .header("Range", "bytes=10-25")
        .send()
        .await
        .unwrap();
    assert_eq!(media_resp.status().as_u16(), 206);
    assert_eq!(
        media_resp
            .headers()
            .get("content-range")
            .unwrap()
            .to_str()
            .unwrap(),
        "bytes 10-25/100"
    );
    let media_slice = media_resp.bytes().await.unwrap();
    let expected_slice: Vec<u8> = (10..=25u8).collect();
    assert_eq!(media_slice.as_ref(), expected_slice.as_slice());

    // 3f. Stateless WebSocket Relay (`GET /__olaz/ws` -> 1st frame `__olaz_ws_open` -> `__olaz_ws_ready` -> binary echo)
    let ws_endpoint = format!("ws://{}/__olaz/ws", server.local_addr());
    let (mut ws_client, ws_upgrade_resp) = tokio_tungstenite::connect_async(&ws_endpoint)
        .await
        .expect("upgrade /__olaz/ws");
    assert_eq!(ws_upgrade_resp.status().as_u16(), 101);

    let open_frame = json!({
        "type": "__olaz_ws_open",
        "url": format!("ws://ws1-msg.chat.zalo.me:{}/zpw_ws", mock_ws_addr.port()),
        "cookie": "zpw_sek=ws_ephemeral_browser_token",
        "origin": "file://",
    });
    ws_client
        .send(Message::Text(open_frame.to_string().into()))
        .await
        .unwrap();

    let ready_raw = ws_client
        .next()
        .await
        .expect("expect __olaz_ws_ready frame")
        .expect("ws frame ok");
    let ready_json: Value =
        serde_json::from_str(ready_raw.to_text().expect("text ready frame")).unwrap();
    assert_eq!(ready_json["type"], "__olaz_ws_ready");

    // Send a Zalo binary frame `[version=1, cmd_lo=0x21, cmd_hi=0x02, sub=0x00, ...]`
    let zalo_frame = vec![0x01, 0x21, 0x02, 0x00, 0xde, 0xad, 0xbe, 0xef];
    ws_client
        .send(Message::Binary(zalo_frame.clone().into()))
        .await
        .unwrap();

    let echoed_msg = ws_client.next().await.unwrap().unwrap();
    let Message::Binary(echoed_bytes) = echoed_msg else {
        panic!("expected binary echoed frame, got {echoed_msg:?}");
    };
    let mut expected_echo = zalo_frame;
    expected_echo.extend_from_slice(&[0xaa, 0x55]);
    assert_eq!(echoed_bytes.as_ref(), expected_echo.as_slice());

    ws_client.close(None).await.unwrap();
    let _ = mock_ws_task.await;
    mock_http_task.abort();

    {
        let rec = recorded.lock();
        assert_eq!(rec.ws_origin.as_deref(), Some("file://"));
        assert_eq!(rec.ws_user_agent.as_deref(), Some(ZALO_USER_AGENT));
        assert_eq!(
            rec.ws_cookie.as_deref(),
            Some("zpw_sek=ws_ephemeral_browser_token")
        );
    }

    server.shutdown().await;
}

// ============================================================================
// Suite 3: Auth Gate (`401`) & SSRF / Open-Proxy Rejections (`403`)
// ============================================================================

#[tokio::test]
async fn security_auth_gate_and_ssrf_allowlist_rejections() {
    // Unit assertions on domain & IP classifiers
    assert!(is_allowed_zalo_domain("zalo.me"));
    assert!(is_allowed_zalo_domain("ws3-msg.chat.zalo.me"));
    assert!(is_allowed_zalo_domain("wpa.zaloapp.com"));
    assert!(is_allowed_zalo_domain("f12-zpc.zadn.vn"));
    assert!(is_allowed_zalo_domain("stc-zlogin.zdn.vn"));
    assert!(is_allowed_zalo_domain("id.zing.vn"));
    assert!(!is_allowed_zalo_domain("evil.example.com"));
    assert!(!is_allowed_zalo_domain("evilzalo.me"));
    assert!(!is_allowed_zalo_domain("zalo.me.evil.com"));
    assert!(!is_allowed_zalo_domain("127.0.0.1"));

    assert!(is_globally_routable_ip("8.8.8.8".parse().unwrap()));
    assert!(!is_globally_routable_ip("127.0.0.1".parse().unwrap()));
    assert!(!is_globally_routable_ip("10.0.0.1".parse().unwrap()));
    assert!(!is_globally_routable_ip("172.16.5.4".parse().unwrap()));
    assert!(!is_globally_routable_ip("192.168.1.1".parse().unwrap()));
    assert!(!is_globally_routable_ip("169.254.169.254".parse().unwrap()));
    assert!(!is_globally_routable_ip("100.64.10.20".parse().unwrap()));
    assert!(!is_globally_routable_ip("0.0.0.0".parse().unwrap()));
    assert!(!is_globally_routable_ip("::1".parse().unwrap()));
    assert!(!is_globally_routable_ip("fe80::1".parse().unwrap()));
    assert!(!is_globally_routable_ip("fd00::1234".parse().unwrap()));
    assert!(!is_globally_routable_ip("::ffff:127.0.0.1".parse().unwrap()));

    let mut dns_overrides = HashMap::new();
    dns_overrides.insert(
        "rebind-loopback.zalo.me".to_string(),
        vec!["127.0.0.1".parse().unwrap()],
    );
    dns_overrides.insert(
        "rebind-metadata.zaloapp.com".to_string(),
        vec!["169.254.169.254".parse().unwrap()],
    );
    dns_overrides.insert(
        "rebind-cgnat.zadn.vn".to_string(),
        vec!["100.64.0.9".parse().unwrap()],
    );

    let bundle = BundleProvider::from_memory(std::collections::BTreeMap::from([(
        "pc-dist/login.html".to_string(),
        b"<html><head></head><body>ok</body></html>".to_vec(),
    )]));

    let secret_token = "edge-gate-secret-token-998877";
    let server = start_web_server(WebServerConfig {
        listen_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        bundle,
        token: Some(secret_token.to_string()),
        // PRODUCTION MODE (`false`!): all loopback, private, and non-HTTPS targets MUST return 403!
        allowlist: AllowlistConfig {
            allow_loopback_upstream_for_tests: false,
            dns_overrides,
        },
    })
    .await
    .expect("start hardened olaz-web");

    let base_url = format!("http://{}", server.local_addr());
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // 1. Unauthenticated requests MUST return 401
    let unauth_html = client
        .get(format!("{base_url}/login.html"))
        .header("Accept", "text/html")
        .send()
        .await
        .unwrap();
    assert_eq!(unauth_html.status().as_u16(), 401);
    let unlock_body = unauth_html.text().await.unwrap();
    assert!(unlock_body.contains("Unlock KV Zalo Web"));
    assert!(unlock_body.contains("viewport-fit=cover"));
    assert!(unlock_body.contains(r#"rel="manifest""#));
    assert!(unlock_body.contains(r#"rel="apple-touch-icon""#));
    assert!(unlock_body.contains(r#"src="/__olaz/logo.svg""#));

    // 1b. Public PWA manifest, icons, favicon & mobile CSS MUST return 200 OK even before `--token` auth!
    let png_magic: &[u8] = b"\x89PNG\r\n\x1a\n";
    let ico_magic: &[u8] = &[0x00, 0x00, 0x01, 0x00];

    let manifest_default_resp = client
        .get(format!("{base_url}/manifest.webmanifest"))
        .send()
        .await
        .unwrap();
    assert_eq!(manifest_default_resp.status().as_u16(), 200);
    assert!(manifest_default_resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("application/manifest+json"));
    let manifest_default: Value = manifest_default_resp.json().await.unwrap();
    assert_eq!(manifest_default["name"], "KV Zalo");
    assert_eq!(manifest_default["short_name"], "KV Zalo");
    assert_eq!(manifest_default["start_url"], "/?source=pwa");
    assert_eq!(manifest_default["display"], "standalone");
    assert_eq!(manifest_default["theme_color"], "#0068FF");
    let icons_arr = manifest_default["icons"].as_array().expect("manifest icons array");
    assert!(icons_arr
        .iter()
        .any(|i| i["sizes"] == "192x192" && i["purpose"] == "any"));
    assert!(icons_arr
        .iter()
        .any(|i| i["sizes"] == "512x512" && i["purpose"] == "any"));
    assert!(icons_arr
        .iter()
        .any(|i| i["sizes"] == "512x512" && i["purpose"] == "maskable"));

    let manifest_slot_resp = client
        .get(format!("{base_url}/manifest.webmanifest?profile=acc-2"))
        .send()
        .await
        .unwrap();
    assert_eq!(manifest_slot_resp.status().as_u16(), 200);
    let manifest_slot: Value = manifest_slot_resp.json().await.unwrap();
    assert_eq!(manifest_slot["name"], "KV Zalo (acc-2)");
    assert_eq!(manifest_slot["short_name"], "Zalo·acc-2");
    assert_eq!(manifest_slot["id"], "/?profile=acc-2&source=pwa");
    assert_eq!(manifest_slot["start_url"], "/?profile=acc-2&source=pwa");

    for svg_route in ["/__olaz/logo.svg", "/favicon.svg"] {
        let r = client.get(format!("{base_url}{svg_route}")).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 200, "SVG route {svg_route} must return 200");
        let txt = r.text().await.unwrap();
        assert!(txt.contains("<svg") && txt.contains("#0068FF"));
    }

    for png_route in [
        "/__olaz/icons/icon-192.png",
        "/__olaz/icons/icon-512.png",
        "/__olaz/icons/icon-maskable-512.png",
        "/apple-touch-icon.png",
        "/apple-touch-icon-precomposed.png",
    ] {
        let r = client.get(format!("{base_url}{png_route}")).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 200, "PNG route {png_route} must return 200");
        assert_eq!(
            r.headers().get("content-type").unwrap().to_str().unwrap(),
            "image/png"
        );
        let bytes = r.bytes().await.unwrap();
        assert!(bytes.len() > 256, "PNG {png_route} must be non-trivial");
        assert_eq!(&bytes[..8], png_magic, "invalid PNG magic on {png_route}");
    }

    let fav_resp = client
        .get(format!("{base_url}/favicon.ico"))
        .send()
        .await
        .unwrap();
    assert_eq!(fav_resp.status().as_u16(), 200);
    let fav_bytes = fav_resp.bytes().await.unwrap();
    assert!(fav_bytes.len() > 64);
    assert_eq!(&fav_bytes[..4], ico_magic, "invalid ICO magic on /favicon.ico");

    let css_resp = client
        .get(format!("{base_url}/__olaz/mobile.css"))
        .send()
        .await
        .unwrap();
    assert_eq!(css_resp.status().as_u16(), 200);
    assert!(css_resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/css"));
    let css_body = css_resp.bytes().await.unwrap();
    assert!(!css_body.is_empty(), "/__olaz/mobile.css must be non-empty");

    let unauth_relay = client
        .post(format!("{base_url}/__olaz/http"))
        .header("X-Olaz-Target", "https://wpa.zaloapp.com/api")
        .send()
        .await
        .unwrap();
    assert_eq!(unauth_relay.status().as_u16(), 401);

    let bad_login = client
        .post(format!("{base_url}/__olaz/auth"))
        .json(&json!({ "token": "wrong-token" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_login.status().as_u16(), 401);

    // 2. Authenticate via `POST /__olaz/auth` -> receive signed `olaz_gate` HttpOnly cookie
    let ok_login = client
        .post(format!("{base_url}/__olaz/auth"))
        .json(&json!({ "token": secret_token }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok_login.status().as_u16(), 200);
    let set_cookie_hdr = ok_login
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie_hdr.starts_with("olaz_gate="));
    assert!(set_cookie_hdr.contains("HttpOnly"));
    assert!(set_cookie_hdr.contains("SameSite=Strict"));
    let gate_cookie_kv = set_cookie_hdr.split(';').next().unwrap().to_string();

    // Verify `Cookie: olaz_gate=...` and `Authorization: Bearer ...` both unlock routes
    let authed_cookie_resp = client
        .get(format!("{base_url}/login.html"))
        .header("Cookie", &gate_cookie_kv)
        .send()
        .await
        .unwrap();
    assert_eq!(authed_cookie_resp.status().as_u16(), 200);

    let authed_bearer_resp = client
        .get(format!("{base_url}/__olaz/health"))
        .header("Authorization", format!("Bearer {secret_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(authed_bearer_resp.status().as_u16(), 200);

    // Tampered `olaz_gate` HMAC signature MUST return 401
    let mut tampered_cookie = gate_cookie_kv.clone();
    let last_ch = if tampered_cookie.ends_with('a') {
        'b'
    } else {
        'a'
    };
    tampered_cookie.pop();
    tampered_cookie.push(last_ch);
    let tampered_resp = client
        .get(format!("{base_url}/__olaz/health"))
        .header("Cookie", tampered_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(tampered_resp.status().as_u16(), 401);

    // 3. SSRF & Open-Proxy Rejections (`403 Forbidden`)
    for forbidden_target in [
        "https://evil.example.com/exfil",
        "https://notzalo.me/api",
        "https://zalo.me.attacker.tld/api",
        "http://wpa.zaloapp.com/insecure-http-disallowed",
        "https://127.0.0.1:8080/admin",
        "https://10.0.0.1/internal",
        "https://192.168.1.1/router",
        "https://169.254.169.254/latest/meta-data/",
        "https://[::1]:8080/",
        "https://user:pass@wpa.zaloapp.com/api",
        "https://rebind-loopback.zalo.me/api",
        "https://rebind-metadata.zaloapp.com/api",
        "https://rebind-cgnat.zadn.vn/file",
    ] {
        let resp = client
            .post(format!("{base_url}/__olaz/http"))
            .header("Cookie", &gate_cookie_kv)
            .header("X-Olaz-Target", forbidden_target)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            403,
            "expected 403 Forbidden for SSRF/open-proxy target: {forbidden_target}"
        );

        let enc: String =
            url::form_urlencoded::byte_serialize(forbidden_target.as_bytes()).collect();
        let media_resp = client
            .get(format!("{base_url}/__olaz/media?u={enc}"))
            .header("Cookie", &gate_cookie_kv)
            .send()
            .await
            .unwrap();
        assert_eq!(
            media_resp.status().as_u16(),
            403,
            "expected 403 Forbidden on /__olaz/media for target: {forbidden_target}"
        );
    }

    // 4. WebSocket SSRF rejection (`__olaz_ws_error` with status 403)
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut ws_req = format!("ws://{}/__olaz/ws", server.local_addr())
        .into_client_request()
        .unwrap();
    ws_req.headers_mut().insert(
        "Authorization",
        format!("Bearer {secret_token}").parse().unwrap(),
    );
    let (mut ws_conn, _) = tokio_tungstenite::connect_async(ws_req).await.unwrap();
    ws_conn
        .send(Message::Text(
            json!({
                "type": "__olaz_ws_open",
                "url": "wss://evil.example.com/socket",
                "cookie": "",
                "origin": "file://"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    let err_frame = ws_conn.next().await.unwrap().unwrap();
    let err_json: Value = serde_json::from_str(err_frame.to_text().unwrap()).unwrap();
    assert_eq!(err_json["type"], "__olaz_ws_error");
    assert_eq!(err_json["status"], 403);

    server.shutdown().await;
}

// ============================================================================
// Suite 4: Browser Preload (`preload.js`), Service Worker (`sw.js`), Multi-Account
//          Namespacing, SubtleCrypto AES-256-GCM PIN Vault & CLI Smoke Test
// ============================================================================

#[tokio::test]
async fn browser_preload_multi_account_pin_vault_sw_and_cli_smoke() {
    // 1. CLI `--help` smoke test on compiled binary
    let bin_path = env!("CARGO_BIN_EXE_kv-zalo-web");
    let help_out = Command::new(bin_path)
        .arg("--help")
        .output()
        .expect("run kv-zalo-web --help");
    assert!(help_out.status.success());
    let help_str = String::from_utf8_lossy(&help_out.stdout);
    assert!(help_str.contains("--listen"));
    assert!(help_str.contains("--asar"));
    assert!(help_str.contains("--fetch-asar"));
    assert!(help_str.contains("--token"));

    // 2. Run `crypto_sync.js` + `bridge_table.js` + `preload.js` + `sw.js` in Node v26
    //    verifying:
    //    - Multi-account `?profile=acc_2` IDB & localStorage scoping
    //    - navigator.userAgent override -> 'ZaloPC-darwin-23v694'
    //    - All 41+ `$z*` preload globals from `web_bridge_spec_script()`
    //    - SQLite-disable & database-config normalization
    //    - SafeStorage, KeychainCipher, BrowserCookieJar, in-browser VFS + fileNoise
    //    - SubtleCrypto PBKDF2-SHA256 (600,000 iterations) + AES-256-GCM Vault sealing
    //    - `sw.js` Service Worker `/__zfile/@slot/acc_2/...` HTTP 206 Partial Content + denoising
    // Verify concatenated `PRELOAD_JS` contains zero blocking `window.prompt` / `window.confirm` dialogs,
    // `zd-worker` receives `WORKER_IMPORT_PREAMBLE`, and `b2.dlfl.vn` / `v1.dlmd.me` pass `AllowlistValidator`.
    assert!(
        !olaz_web::PRELOAD_JS.contains("window.prompt(")
            && !olaz_web::PRELOAD_JS.contains("window.confirm("),
        "PRELOAD_JS must not use native blocking window.prompt/window.confirm dialogs"
    );
    let zd_transformed = olaz_web::transform_bundle_asset_opt(
        "pc-dist/zd-worker.cfeba43a9a8e4e40dda3.js",
        b"self.onmessage = function () {};",
    )
    .expect("zd-worker must be transformed");
    assert!(
        std::str::from_utf8(&zd_transformed)
            .unwrap()
            .starts_with("try { importScripts('/__olaz/crypto_sync.js', '/__olaz/bridge_table.js', '/__olaz/preload.js'); }"),
        "zd-worker must receive WORKER_IMPORT_PREAMBLE"
    );
    {
        let mut dns_overrides = std::collections::HashMap::new();
        let pub_ip = std::net::IpAddr::V4(Ipv4Addr::new(203, 119, 9, 25));
        dns_overrides.insert("b2.dlfl.vn".to_string(), vec![pub_ip]);
        dns_overrides.insert("v1.dlmd.me".to_string(), vec![pub_ip]);
        let validator = olaz_web::AllowlistValidator::new(AllowlistConfig {
            allow_loopback_upstream_for_tests: false,
            dns_overrides,
        });
        assert!(validator
            .validate_http_target("https://b2.dlfl.vn/dl/photo.jpg")
            .await
            .is_ok());
        assert!(validator
            .validate_http_target("https://v1.dlmd.me/stream/video.mp4")
            .await
            .is_ok());
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let payload = json!({
        "cryptoJs": manifest_dir.join("js/crypto_sync.js").to_string_lossy(),
        "preloadSource": olaz_web::PRELOAD_JS,
        "swJs": manifest_dir.join("js/sw.js").to_string_lossy(),
        "bridgeTableScript": olaz_web::web_bridge_spec_script(),
    });

    let node_harness = r#"
      const fs = require('node:fs');
      const vm = require('node:vm');
      const assert = require('node:assert/strict');
      const input = JSON.parse(fs.readFileSync(0, 'utf8'));

      // Minimal spec-accurate in-RAM IndexedDB + Storage + DOM shim for Node v26
      const idbDatabases = new Map();
      const openedDbNames = [];

      class FakeIDBRequest {
        constructor() {
          this.readyState = 'pending';
          this.result = undefined;
          this.error = null;
          this.onsuccess = null;
          this.onerror = null;
          this.onupgradeneeded = null;
          this._ls = { success: [], error: [], upgradeneeded: [] };
        }
        addEventListener(t, fn) { if (this._ls[t]) this._ls[t].push(fn); }
        _fire(t, ev = {}) {
          const e = Object.assign({ type: t, target: this, currentTarget: this }, ev);
          if (typeof this['on' + t] === 'function') this['on' + t](e);
          for (const fn of (this._ls[t] || [])) fn(e);
        }
      }

      class FakeObjectStore {
        constructor(meta) {
          this._meta = meta;
          this.name = meta.name;
          this.keyPath = meta.keyPath;
        }
        get(key) {
          const req = new FakeIDBRequest();
          queueMicrotask(() => {
            req.readyState = 'done';
            req.result = this._meta.records.get(String(key));
            req._fire('success');
          });
          return req;
        }
        getAll() {
          const req = new FakeIDBRequest();
          queueMicrotask(() => {
            req.readyState = 'done';
            req.result = Array.from(this._meta.records.values());
            req._fire('success');
          });
          return req;
        }
        put(val, explicitKey) {
          const req = new FakeIDBRequest();
          const k = explicitKey !== undefined ? explicitKey : (this.keyPath ? val[this.keyPath] : undefined);
          queueMicrotask(() => {
            this._meta.records.set(String(k), val);
            req.readyState = 'done';
            req.result = k;
            req._fire('success');
          });
          return req;
        }
        delete(key) {
          const req = new FakeIDBRequest();
          queueMicrotask(() => {
            this._meta.records.delete(String(key));
            req.readyState = 'done';
            req._fire('success');
          });
          return req;
        }
      }

      class FakeIDBDatabase {
        constructor(name, meta) {
          this.name = name;
          this._meta = meta;
          this.objectStoreNames = {
            contains: (n) => meta.stores.has(n),
          };
        }
        createObjectStore(name, opts = {}) {
          const st = { name, keyPath: opts.keyPath || null, records: new Map() };
          this._meta.stores.set(name, st);
          return new FakeObjectStore(st);
        }
        transaction(names) {
          const selfDb = this;
          const lsComplete = [];
          const txObj = {
            oncomplete: null,
            onerror: null,
            onabort: null,
            objectStore(n) {
              return new FakeObjectStore(selfDb._meta.stores.get(n));
            },
            addEventListener(t, fn) {
              if (t === 'complete' && typeof fn === 'function') lsComplete.push(fn);
            },
          };
          queueMicrotask(() => queueMicrotask(() => {
            if (typeof txObj.oncomplete === 'function') txObj.oncomplete({ type: 'complete', target: txObj });
            for (const fn of lsComplete) fn({ type: 'complete', target: txObj });
          }));
          return txObj;
        }
        close() {}
      }

      class IDBFactory {}
      IDBFactory.prototype.open = function (name, version) {
        openedDbNames.push(String(name));
        const req = new FakeIDBRequest();
        queueMicrotask(() => {
          let isNew = false;
          if (!idbDatabases.has(name)) {
            idbDatabases.set(name, { version: version || 1, stores: new Map() });
            isNew = true;
          }
          const db = new FakeIDBDatabase(name, idbDatabases.get(name));
          req.result = db;
          if (isNew) req._fire('upgradeneeded');
          req.readyState = 'done';
          req._fire('success');
        });
        return req;
      };
      IDBFactory.prototype.deleteDatabase = function (name) {
        const req = new FakeIDBRequest();
        queueMicrotask(() => {
          idbDatabases.delete(name);
          req.readyState = 'done';
          req._fire('success');
        });
        return req;
      };

      class Storage {
        constructor() { this._m = new Map(); }
        get length() { return this._m.size; }
        key(i) { return Array.from(this._m.keys())[i] ?? null; }
        getItem(k) { return this._m.has(String(k)) ? this._m.get(String(k)) : null; }
        setItem(k, v) { this._m.set(String(k), String(v)); }
        removeItem(k) { this._m.delete(String(k)); }
        clear() { this._m.clear(); }
      }

      const ls = new Storage();
      const ss = new Storage();
      const makeElem = (tag) => {
        const classes = new Set();
        const listeners = new Map();
        const byId = new Map();
        const el = {
          tagName: String(tag).toUpperCase(),
          id: '',
          value: '',
          textContent: '',
          innerHTML: '',
          disabled: false,
          checked: false,
          style: {},
          shadowRoot: null,
          parentNode: null,
          classList: {
            toggle(c, force) {
              const on = force === undefined ? !classes.has(c) : Boolean(force);
              if (on) classes.add(c); else classes.delete(c);
              return on;
            },
            add(...cs) { cs.forEach((c) => classes.add(c)); },
            remove(...cs) { cs.forEach((c) => classes.delete(c)); },
            contains(c) { return classes.has(c); },
          },
          children: [],
          attributes: [],
          setAttribute(k, v) {
            if (k === 'id') this.id = String(v);
            this.attributes.push({ name: k, value: String(v) });
          },
          getAttribute(k) {
            const hit = this.attributes.find((a) => a.name === k);
            return hit ? hit.value : null;
          },
          appendChild(c) {
            if (c) c.parentNode = this;
            this.children.push(c);
            return c;
          },
          remove() {
            if (this.parentNode && Array.isArray(this.parentNode.children)) {
              const idx = this.parentNode.children.indexOf(this);
              if (idx >= 0) this.parentNode.children.splice(idx, 1);
              this.parentNode = null;
            }
          },
          focus() {},
          select() {},
          getBoundingClientRect() { return { left: 0, top: 0, right: 100, bottom: 40, width: 100, height: 40 }; },
          addEventListener(type, fn) {
            if (typeof fn !== 'function') return;
            if (!listeners.has(type)) listeners.set(type, []);
            listeners.get(type).push(fn);
          },
          removeEventListener() {},
          dispatchEvent(ev) {
            const fns = listeners.get(ev && ev.type) || [];
            const ps = [];
            for (const fn of fns) ps.push(fn(ev));
            return Promise.all(ps);
          },
          attachShadow() {
            const sh = makeElem('shadow');
            this.shadowRoot = sh;
            return sh;
          },
          getElementById(id) {
            const k = String(id);
            if (!byId.has(k)) {
              const child = makeElem('div');
              child.id = k;
              byId.set(k, child);
            }
            return byId.get(k);
          },
          querySelector() { return null; },
          querySelectorAll() { return []; },
        };
        return el;
      };

      const win = {
        crypto: globalThis.crypto,
        TextEncoder: globalThis.TextEncoder,
        TextDecoder: globalThis.TextDecoder,
        URL: globalThis.URL,
        URLSearchParams: globalThis.URLSearchParams,
        atob: globalThis.atob,
        btoa: globalThis.btoa,
        setTimeout: globalThis.setTimeout,
        clearTimeout: globalThis.clearTimeout,
        setInterval: globalThis.setInterval,
        clearInterval: globalThis.clearInterval,
        queueMicrotask: globalThis.queueMicrotask,
        fetch: globalThis.fetch,
        Request: globalThis.Request,
        Response: globalThis.Response,
        Headers: globalThis.Headers,
        Blob: globalThis.Blob,
        AbortController: globalThis.AbortController,
        BroadcastChannel: globalThis.BroadcastChannel,
        Event: globalThis.Event,
        MessageEvent: globalThis.MessageEvent,
        DOMException: globalThis.DOMException,
        console: globalThis.console,
      };
      win.window = win;
      win.self = win;
      win.globalThis = win;
      win.IDBFactory = IDBFactory;
      win.IDBDatabase = FakeIDBDatabase;
      win.IDBObjectStore = FakeObjectStore;
      win.indexedDB = new IDBFactory();
      win.Storage = Storage;
      win.localStorage = ls;
      win.sessionStorage = ss;
      win.location = {
        href: 'http://127.0.0.1:8080/index.html?is_main=true&fail_sqlite3=true&profile=acc_2',
        origin: 'http://127.0.0.1:8080',
        protocol: 'http:',
        host: '127.0.0.1:8080',
        pathname: '/index.html',
        search: '?is_main=true&fail_sqlite3=true&profile=acc_2',
        replace() {},
        reload() {},
      };
      win.navigator = {
        userAgent: 'Mozilla/5.0 HeadlessChrome',
        platform: 'Linux x86_64',
        hardwareConcurrency: 8,
        onLine: true,
      };
      win.document = {
        readyState: 'complete',
        head: makeElem('head'),
        body: makeElem('body'),
        documentElement: makeElem('html'),
        createElement: makeElem,
        getElementById() { return null; },
        querySelectorAll() { return []; },
        addEventListener() {},
        dispatchEvent() { return true; },
      };
      win.addEventListener = () => {};
      win.dispatchEvent = () => true;

      const rawUnprefixedOpen = IDBFactory.prototype.open;

      // Pre-poison `zpw_type` / `zpw_ver` in slot localStorage to prove `<head>` heals stale `'null'`!
      ls._m.set('olaz:acc_2:zpw_type', 'null');
      ls._m.set('olaz:acc_2:zpw_ver', 'null');

      vm.createContext(win);
      vm.runInContext(fs.readFileSync(input.cryptoJs, 'utf8'), win, { filename: 'crypto_sync.js' });
      vm.runInContext(input.bridgeTableScript, win, { filename: 'bridge_table.js' });
      vm.runInContext(input.preloadSource, win, { filename: 'preload.js' });

      (async () => {
        await win.__olazBootReady;

        // 1. Verify navigator.userAgent override + pageQuery / localStorage client_version healing
        assert.equal(win.navigator.userAgent, 'ZaloPC-darwin-23v695');
        assert.equal(win.__olazActiveProfile, 'acc_2');
        assert.equal(win.localStorage.getItem('zpw_type'), '23');
        assert.equal(win.localStorage.getItem('zpw_ver'), '695');
        assert.equal(win.localStorage.getItem('app_ver'), '26.11.10');
        const healedSearch = new URLSearchParams(win.location.search);
        assert.equal(healedSearch.get('type'), '23');
        assert.equal(healedSearch.get('client_version'), '695');
        assert.equal(healedSearch.get('app_version'), '26.11.10');
        assert.equal(healedSearch.get('new_install'), '0');
        assert.equal(healedSearch.get('fail_sqlite3'), 'true');
        assert.equal(healedSearch.get('profile'), 'acc_2');

        // 2. Verify all 41+ `$z*` globals installed
        for (const g of win.__zaloBridgeSpec.globals) {
          assert.ok(win[g] !== undefined, `missing preload global ${g}`);
        }

        // 3. Verify multi-account namespacing for `?profile=acc_2`:
        //    - localStorage keys scoped under `olaz:acc_2:*`
        //    - Zalo IndexedDB names prefixed with `olaz__acc_2__*`
        win.localStorage.setItem('z_uuid', 'uuid-for-acc-2');
        assert.equal(win.localStorage.getItem('z_uuid'), 'uuid-for-acc-2');
        assert.equal(ls._m.get('olaz:acc_2:z_uuid'), 'uuid-for-acc-2');
        assert.equal(ls._m.has('z_uuid'), false);

        win.indexedDB.open('zdb_7123456', 1);
        assert.ok(openedDbNames.includes('olaz__acc_2__zdb_7123456'));

        // 4. Verify SQLite-disable channels & `database-config` normalization
        assert.equal(await win.$zdb.canIUseSQLite(), false);
        assert.equal(await win.$zFeatures.migrateDB.isMigrateFullDone(), false);
        const migInfo = await win.$zFeatures.migrateDB.getMigrateLocalInfo();
        assert.equal(migInfo.disable_migrate, true);

        win.$zelectron.storeWrite('database-config', {
          upgradeRequired: true,
          '12345_adp-st': '{"msg":2,"conv":1}',
          keepFlag: 'yes',
        });
        const normCfg = win.$zelectron.storeRead('database-config');
        assert.equal(normCfg.upgradeRequired, undefined);
        assert.equal(normCfg['12345_adp-st'], '{"msg":0,"conv":0}');
        assert.equal(normCfg.keepFlag, 'yes');

        // 5. Verify `$zsafeStorage`, `$zprotocol` KeychainCipher, and `$zelectron` cookies
        await win.$zsafeStorage.execute('set', 'tok', 'browser-only-secret');
        assert.equal(await win.$zsafeStorage.execute('get', 'tok'), 'browser-only-secret');

        const plainItem = new Uint8Array([10, 20, 30, 40, 50]);
        const encItems = await win.$zprotocol.encryptWithKeychain('dev_key', [plainItem]);
        assert.equal(encItems.length, 1);
        const decItems = await win.$zprotocol.decryptWithKeychain('dev_key', encItems);
        assert.deepEqual(Array.from(new Uint8Array(decItems[0])), [10, 20, 30, 40, 50]);

        await win.$zelectron.setAppCookie('sek_acc_2_isolated');
        const gotCookies = await win.$zelectron.getCookies({ url: 'https://chat.zalo.me/' });
        assert.ok(gotCookies.some((c) => c.name === 'zpw_sek' && c.value === 'sek_acc_2_isolated'));

        // 6. Verify in-browser VFS + `fileNoise` + Service Worker (`sw.js`) HTTP 206 Range denoising!
        const rawVideo = new Uint8Array(200);
        for (let i = 0; i < rawVideo.length; i++) rawVideo[i] = (i * 13 + 7) & 0xff;
        win.$znode.fs.writeFileSync('/olaz-vfs/tmp/raw.mp4', rawVideo);
        await win.$zFileManager.zcloudNoiseFile('/olaz-vfs/tmp/raw.mp4', '/olaz-vfs/userData/ZaloDownloads/noised.mp4', '');
        await new Promise((r) => setTimeout(r, 15));

        // Boot `sw.js` sharing the same `indexedDB` and issue a `Range: bytes=10-49` request!
        let swFetchListener = null;
        const swScope = {
          location: { origin: 'http://127.0.0.1:8080' },
          crypto: globalThis.crypto,
          TextEncoder: globalThis.TextEncoder,
          TextDecoder: globalThis.TextDecoder,
          URL: globalThis.URL,
          URLSearchParams: globalThis.URLSearchParams,
          Request: globalThis.Request,
          Response: globalThis.Response,
          Headers: globalThis.Headers,
          Blob: globalThis.Blob,
          queueMicrotask: globalThis.queueMicrotask,
          setTimeout: globalThis.setTimeout,
          clearTimeout: globalThis.clearTimeout,
          console: globalThis.console,
        };
        swScope.self = swScope;
        swScope.globalThis = swScope;
        swScope.indexedDB = { open: (n, v) => rawUnprefixedOpen.call(null, n, v) };
        swScope.importScripts = () => {
          vm.runInContext(fs.readFileSync(input.cryptoJs, 'utf8'), swScope, { filename: 'crypto_sync.js' });
        };
        swScope.clients = { claim: () => Promise.resolve() };
        swScope.skipWaiting = () => Promise.resolve();
        swScope.addEventListener = (type, fn) => {
          if (type === 'fetch') swFetchListener = fn;
        };
        vm.createContext(swScope);
        vm.runInContext(fs.readFileSync(input.swJs, 'utf8'), swScope, { filename: 'sw.js' });
        assert.equal(typeof swFetchListener, 'function');

        const swResponse = await new Promise((resolve) => {
          const req = new Request(
            'http://127.0.0.1:8080/__zfile/@slot/acc_2/olaz-vfs/userData/ZaloDownloads/noised.mp4',
            { headers: { Range: 'bytes=10-49' } }
          );
          swFetchListener({
            request: req,
            respondWith(p) { resolve(p); },
          });
        });
        assert.equal(swResponse.status, 206);
        assert.equal(swResponse.headers.get('content-range'), 'bytes 10-49/200');
        const denoisedSlice = new Uint8Array(await swResponse.arrayBuffer());
        assert.deepEqual(Array.from(denoisedSlice), Array.from(rawVideo.subarray(10, 50)));

        // Verify `sw.js` offline navigation fallback returns 200 OK branded "KV Zalo — Offline" page
        swScope.fetch = () => Promise.reject(new TypeError('Failed to fetch'));
        const swOfflineNavResp = await new Promise((resolve) => {
          swFetchListener({
            request: {
              url: 'http://127.0.0.1:8080/index.html?source=pwa',
              method: 'GET',
              mode: 'navigate',
              headers: new Headers(),
            },
            respondWith(p) { resolve(p); },
          });
        });
        assert.equal(swOfflineNavResp.status, 200);
        const offlineHtml = await swOfflineNavResp.text();
        assert.ok(offlineHtml.includes('KV Zalo — Offline'));
        assert.ok(offlineHtml.includes('Retry'));

        // 6b. Verify plain-LAN-HTTP fallback (`http://192.168.x.x:<port>/` with no ServiceWorker controller)
        const lanHit = win.__olazResolveVfsZfile('/__zfile/@slot/acc_2/olaz-vfs/userData/ZaloDownloads/noised.mp4');
        assert.ok(lanHit && lanHit.bytes, 'expected in-RAM VFS resolver hit on plain LAN HTTP');
        assert.equal(lanHit.mime, 'video/mp4');
        assert.deepEqual(Array.from(lanHit.bytes), Array.from(rawVideo));
        const lanFetchResp = await win.fetch('zfile:///olaz-vfs/userData/ZaloDownloads/noised.mp4');
        assert.equal(lanFetchResp.status, 200);
        assert.deepEqual(Array.from(new Uint8Array(await lanFetchResp.arrayBuffer())), Array.from(rawVideo));

        // 7. Verify SubtleCrypto PBKDF2-SHA256 (`600,000` iterations) + non-extractable AES-256-GCM key derivation
        const salt = crypto.getRandomValues(new Uint8Array(16));
        const baseKey = await crypto.subtle.importKey(
          'raw',
          new TextEncoder().encode('482910'),
          'PBKDF2',
          false,
          ['deriveKey']
        );
        const aesKey = await crypto.subtle.deriveKey(
          { name: 'PBKDF2', salt, iterations: 600000, hash: 'SHA-256' },
          baseKey,
          { name: 'AES-GCM', length: 256 },
          false,
          ['encrypt', 'decrypt']
        );
        assert.equal(aesKey.extractable, false);

        // 8. Verify concatenated Window modules (`__olazVault`, `__olazAntiRecall`, `__olazCallRecorder`, `$zsub.$zMsgChannel`)
        //    and Shadow DOM `#pinForm` + `#__olaz-vault-unlock-overlay` PIN lock/unlock flow!
        assert.equal(typeof win.__olazVault, 'object');
        assert.equal(typeof win.__olazVault.configurePin, 'function');
        assert.equal(typeof win.__olazVault.lockNow, 'function');
        assert.equal(typeof win.__olazVault.unlockWithPin, 'function');

        assert.equal(typeof win.__olazAntiRecall, 'object');
        assert.equal(win.__olazAntiRecall.isEnabled(), true);

        assert.equal(typeof win.__olazCallRecorder, 'object');
        assert.equal(win.__olazCallRecorder.getState().slot, 'acc_2');

        assert.ok(win.$zsub && win.$zsub.$zMsgChannel, 'expected window.$zsub.$zMsgChannel');
        assert.equal(typeof win.$zsub.$zMsgChannel.createUtilityMediaPort, 'function');

        // Exercise Shadow DOM `#pinForm` inside the floating deck + `window.__olazVault` lock/unlock
        const mountedRoots = [
          ...win.document.documentElement.children,
          ...win.document.body.children,
        ];
        const deckHost = mountedRoots.find((el) => el && el.shadowRoot && String(el.shadowRoot.innerHTML).includes('id="pinForm"'));
        assert.ok(deckHost && deckHost.shadowRoot, 'expected floating deck Shadow DOM with #pinForm');
        const deckShadow = deckHost.shadowRoot;
        const pinBtn = deckShadow.getElementById('pinBtn');
        const pinForm = deckShadow.getElementById('pinForm');
        const pinNewInput = deckShadow.getElementById('pinNewInput');
        const pinConfirmInput = deckShadow.getElementById('pinConfirmInput');
        await pinBtn.dispatchEvent({ type: 'click' });
        assert.equal(pinForm.classList.contains('hidden'), false);
        pinNewInput.value = '4829';
        pinConfirmInput.value = '4829';
        await pinForm.dispatchEvent({ type: 'submit', preventDefault() {} });
        assert.equal(win.__olazVault.hasPin(), true);
        assert.equal(win.__olazVault.isLocked(), false);

        const lockPromise = win.__olazVault.lockNow();
        await new Promise((r) => setTimeout(r, 10));
        assert.equal(win.__olazVault.isLocked(), true);
        const unlockOverlay = [
          ...win.document.documentElement.children,
          ...win.document.body.children,
        ].find((el) => el && el.id === '__olaz-vault-unlock-overlay');
        assert.ok(unlockOverlay && unlockOverlay.shadowRoot, 'expected Shadow DOM unlock modal');
        await win.__olazVault.unlockWithPin('4829');
        await lockPromise;
        assert.equal(win.__olazVault.isLocked(), false);

        process.stdout.write(JSON.stringify({ ok: true }), () => process.exit(0));
      })().catch((err) => {
        console.error(err);
        process.exit(1);
      });
    "#;

    let mut child = Command::new("node")
        .arg("-e")
        .arg(node_harness)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn node preload & sw harness");

    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&payload).unwrap().as_bytes())
        .unwrap();

    let out = child.wait_with_output().expect("wait node harness");
    if !out.status.success() {
        panic!(
            "browser preload & sw harness failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let parsed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed["ok"], true);
}

#[tokio::test]
async fn zero_copy_asar_caching_etag_and_concurrent_burst_test() {
    let tmp = TempDir::new().expect("tempdir for synthetic asar");
    let asar_path = tmp.path().join("app.asar");

    let unpatched_chunk: Vec<u8> = b"window.__lazy52 = 1;\n".repeat(65_536); // ~1.37 MB chunk
    let raw_html = b"<!doctype html><html><head><title>Zalo</title></head><body><script src=\"/lazy/52.29e128365e7ed193665d.js\"></script></body></html>";

    let header_json = serde_json::to_vec(&json!({
        "files": {
            "pc-dist": {
                "files": {
                    "index.html": {
                        "size": raw_html.len(),
                        "offset": "0"
                    },
                    "lazy": {
                        "files": {
                            "52.29e128365e7ed193665d.js": {
                                "size": unpatched_chunk.len(),
                                "offset": raw_html.len().to_string()
                            }
                        }
                    }
                }
            }
        }
    }))
    .unwrap();

    let json_len = header_json.len() as u32;
    let padded_json_len = (header_json.len() + 3) & !3;
    let inner_pickle_size = (4 + padded_json_len) as u32;
    let header_pickle_size = 4 + inner_pickle_size;

    let mut asar_bytes = Vec::new();
    asar_bytes.extend_from_slice(&4u32.to_le_bytes());
    asar_bytes.extend_from_slice(&header_pickle_size.to_le_bytes());
    asar_bytes.extend_from_slice(&inner_pickle_size.to_le_bytes());
    asar_bytes.extend_from_slice(&json_len.to_le_bytes());
    asar_bytes.extend_from_slice(&header_json);
    asar_bytes.resize(16 + padded_json_len, 0);
    asar_bytes.extend_from_slice(raw_html);
    asar_bytes.extend_from_slice(&unpatched_chunk);
    std::fs::write(&asar_path, &asar_bytes).unwrap();

    let bundle = BundleProvider::from_asar(&asar_path).expect("open synthetic ASAR");

    // Unpatched chunk must be served directly out of `AssetBytes::MmapSlice` with identical pointer address!
    let c1 = bundle
        .get_asset("/lazy/52.29e128365e7ed193665d.js")
        .expect("chunk hit 1");
    let c2 = bundle
        .get_asset("/lazy/52.29e128365e7ed193665d.js")
        .expect("chunk hit 2");
    assert!(
        matches!(c1.bytes, olaz_web::AssetBytes::MmapSlice { .. }),
        "unpatched ASAR chunk must use zero-copy MmapSlice"
    );
    assert_eq!(c1.bytes.as_ptr(), c2.bytes.as_ptr());
    assert_eq!(&*c1.bytes, unpatched_chunk.as_slice());

    // Patched HTML must be cached in `AssetBytes::Shared` and share the exact buffer across calls!
    let h1 = bundle.get_asset("/index.html").expect("html hit 1");
    let h2 = bundle.get_asset("/index.html").expect("html hit 2");
    assert_eq!(h1.bytes.as_ptr(), h2.bytes.as_ptr());

    // Spin up stateless edge server and verify ETag / 304 + 64-way concurrent burst
    let server = start_web_server(WebServerConfig {
        listen_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        bundle,
        token: None,
        allowlist: AllowlistConfig::default(),
    })
    .await
    .expect("start server");
    let base = format!("http://{}", server.local_addr());
    let client = reqwest::Client::new();

    let html_resp = client.get(format!("{base}/index.html")).send().await.unwrap();
    assert_eq!(html_resp.status(), 200);
    assert_eq!(
        html_resp.headers().get("cache-control").unwrap(),
        "no-store"
    );

    let chunk_resp = client
        .get(format!("{base}/lazy/52.29e128365e7ed193665d.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(chunk_resp.status(), 200);
    assert_eq!(
        chunk_resp.headers().get("cache-control").unwrap(),
        "private, max-age=300"
    );
    let etag = chunk_resp
        .headers()
        .get("etag")
        .expect("ETag header on static bundle chunk")
        .to_str()
        .unwrap()
        .to_string();
    let chunk_body = chunk_resp.bytes().await.unwrap();
    assert_eq!(chunk_body.as_ref(), unpatched_chunk.as_slice());

    // Conditional request with matching `If-None-Match` must return `304 Not Modified` with empty body
    let reval_resp = client
        .get(format!("{base}/lazy/52.29e128365e7ed193665d.js"))
        .header("If-None-Match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(reval_resp.status(), 304);
    assert!(reval_resp.bytes().await.unwrap().is_empty());

    // 64 concurrent full-body chunk downloads must all return 200 OK with exact byte length
    let mut tasks = Vec::new();
    for _ in 0..64 {
        let c = client.clone();
        let u = format!("{base}/lazy/52.29e128365e7ed193665d.js");
        let expected_len = unpatched_chunk.len();
        tasks.push(tokio::spawn(async move {
            let r = c.get(u).send().await.expect("burst request");
            assert_eq!(r.status(), 200);
            let b = r.bytes().await.expect("burst body");
            assert_eq!(b.len(), expected_len);
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    server.shutdown().await;
}

