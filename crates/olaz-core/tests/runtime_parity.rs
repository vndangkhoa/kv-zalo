//! Comprehensive parity and security test suite for `olaz-core`.
//!
//! Covers:
//! 1. `wire`: byte/ArrayBuffer/undefined/Stats wire envelopes.
//! 2. `crypto`: live `node` CLI byte-for-byte parity across AES-128/192/256 CBC, CTR, ECB,
//!    GCM (12-byte IV AND 16-byte IV + AAD), strict PKCS#7 `"bad decrypt"` rejection,
//!    MD5/SHA1/SHA256/SHA384/SHA512, hex truncation, forgiving base64, base64url, latin1,
//!    UTF-16LE, and stateful cipher/hash handles.
//! 3. `asar`: synthetic Chromium Pickle ASAR v0 archive construction, zero-copy reads,
//!    percent-decoding, unpacked fallback, and MIME mapping.
//! 4. `vault`: `KeychainCipher` roundtrip, envelope tamper rejection, locked-keyring
//!    non-overwrite guarantee, headless `0600` keyfile permissions, and `SafeStorage` `zsskv=2`.
//! 5. `platform` & `fs_bridge`: XDG/custom roots, persistent `imei`, store sanitization,
//!    `normalize_database_adapter_state`, symlink escape confinement, and sync/async FS bridge.
//! 6. `bundle_patcher`: `__zaloBridgeSpec`, 21-script ordering, and `dal-worker` /
//!    `sync-v2-sub-worker` / `main-startup` rewriters verified in live `node`.
//! 7. `installer`: `ZaloRelease::PINNED`, >4 MiB streaming SHA-256, `prepare_for_launch`,
//!    and end-to-end `7z` userspace runtime extraction + SHA-256 mismatch rejection.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use olaz_core::asar::{mime_for_path, AsarArchive, AsarEntry};
use olaz_core::bundle_patcher::{
    bridge_ipc_table, bridge_spec_script, expose_webpack_require, ordered_user_scripts,
    patch_asar_bundle, patch_sync_global_id_resolver, patch_zalo_linux_renderer_bundle,
    worker_prelude,
};
use olaz_core::crypto::CryptoBridge;
use olaz_core::fs_bridge::FsBridge;
use olaz_core::installer::{InstallProgress, ZaloRelease, ZaloRuntimeManager};
use olaz_core::platform::{ClientIdentity, ZaloPlatform};
use olaz_core::vault::{
    KeychainCipher, LinuxSecretBackend, MemorySecretBackend, SafeStorage, SecretBackend, VaultError,
};
use olaz_core::wire;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tempfile::TempDir;
use url::Url;

/// Executes a JavaScript snippet with `node` via a temporary `.cjs` file and parses stdout as JSON.
fn run_node_json(script: &str) -> Value {
    let dir = TempDir::new().expect("tempdir for node script");
    let script_path = dir.path().join("snippet.cjs");
    std::fs::write(&script_path, script).expect("write snippet.cjs");
    let out = Command::new("node")
        .arg(&script_path)
        .output()
        .expect("node binary must be available to run parity tests");
    assert!(
        out.status.success(),
        "node exited with {}: stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 node stdout");
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON from node ({e}): {stdout}"))
}

/// Builds a synthetic Electron ASAR v0 archive matching Chromium's Pickle header layout.
fn build_synthetic_asar(path: &Path, files: &[(&str, Option<&[u8]>)]) {
    fn insert_tree(node: &mut Map<String, Value>, parts: &[&str], meta: Value) {
        let files_obj = node
            .entry("files".to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap();
        if parts.len() == 1 {
            files_obj.insert(parts[0].to_string(), meta);
        } else {
            let child = files_obj
                .entry(parts[0].to_string())
                .or_insert_with(|| json!({ "files": {} }))
                .as_object_mut()
                .unwrap();
            insert_tree(child, &parts[1..], meta);
        }
    }

    let mut root = Map::new();
    let mut payload = Vec::new();
    for (rel_path, content) in files {
        let parts: Vec<&str> = rel_path.split('/').collect();
        let meta = match content {
            Some(bytes) => {
                let offset = payload.len();
                payload.extend_from_slice(bytes);
                // ASAR serializes `"offset"` as a decimal String!
                json!({
                    "size": bytes.len(),
                    "offset": offset.to_string(),
                })
            }
            None => json!({
                "size": 0,
                "unpacked": true,
            }),
        };
        insert_tree(&mut root, &parts, meta);
    }

    let json_bytes = serde_json::to_vec(&Value::Object(root)).unwrap();
    let json_len = json_bytes.len() as u32;
    let padded_json_len = (json_len + 3) & !3;
    let header_size = 4 + padded_json_len;
    let header_pickle_size = 4 + header_size;

    let mut archive = Vec::new();
    archive.extend_from_slice(&4u32.to_le_bytes());
    archive.extend_from_slice(&header_pickle_size.to_le_bytes());
    archive.extend_from_slice(&header_size.to_le_bytes());
    archive.extend_from_slice(&json_len.to_le_bytes());
    archive.extend_from_slice(&json_bytes);
    archive.resize(archive.len() + (padded_json_len - json_len) as usize, 0u8);
    assert_eq!(archive.len(), 8 + header_pickle_size as usize);
    archive.extend_from_slice(&payload);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, archive).unwrap();
}

// ============================================================================
// 1. Wire helpers
// ============================================================================

#[test]
fn wire_value_envelopes_and_stats() {
    let raw = [0u8, 1, 2, 254, 255];

    let u8_wire = wire::encode_bytes(&raw);
    assert_eq!(u8_wire.get("__arraybuffer"), None);
    assert_eq!(wire::decode_bytes(&u8_wire).as_deref(), Some(&raw[..]));

    let ab_wire = wire::encode_array_buffer(&raw);
    assert_eq!(ab_wire.get("__arraybuffer"), Some(&json!(true)));
    assert_eq!(wire::decode_bytes(&ab_wire).as_deref(), Some(&raw[..]));

    let node_buf = json!({ "type": "Buffer", "data": [10, 20, 30] });
    assert_eq!(wire::decode_bytes(&node_buf), Some(vec![10, 20, 30]));
    assert_eq!(wire::decode_bytes(&json!([99, 100, 255])), Some(vec![99, 100, 255]));
    assert_eq!(wire::decode_bytes(&json!([256])), None);
    assert_eq!(wire::decode_bytes(&json!("plain string")), None);
    assert_eq!(
        wire::decode_bytes_or_utf8(&json!("hello")),
        Some(b"hello".to_vec())
    );

    let undef = wire::undefined();
    assert!(wire::is_undefined(&undef));
    assert!(wire::is_null_or_undefined(&undef));
    assert!(wire::is_null_or_undefined(&Value::Null));
    assert!(!wire::is_undefined(&Value::Null));

    let tmp = TempDir::new().unwrap();
    let file_path = tmp.path().join("sample.bin");
    std::fs::write(&file_path, b"abcdef").unwrap();
    let link_path = tmp.path().join("sample.lnk");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&file_path, &link_path).unwrap();

    let file_stats = wire::encode_stats(std::fs::metadata(&file_path).unwrap());
    assert_eq!(file_stats["__stats"], json!(true));
    assert_eq!(file_stats["size"], json!(6));
    assert_eq!(file_stats["isFile"], json!(true));
    assert_eq!(file_stats["isDirectory"], json!(false));
    assert_eq!(file_stats["isSymbolicLink"], json!(false));
    assert!(file_stats["mtimeMs"].as_f64().unwrap() > 0.0);

    #[cfg(unix)]
    {
        let sym_stats = wire::encode_stats(std::fs::symlink_metadata(&link_path).unwrap());
        assert_eq!(sym_stats["isSymbolicLink"], json!(true));
        assert_eq!(sym_stats["isFile"], json!(false));
    }
}

// ============================================================================
// 2. CryptoBridge + live `node` CLI parity tests
// ============================================================================

#[test]
fn crypto_swift_reference_vectors() {
    let crypto = CryptoBridge::new();

    // 1. Exact `zcidEncryptMatchesNode` vector from `Tests/OlazTests/RuntimeTests.swift:23-29`
    let zcid = crypto
        .dispatch_sync(
            "$znode.crypto.encrypt",
            &[
                json!("23,abc-imei,1792963077777"),
                json!("aes-256-cbc"),
                json!("3FC4F0D2AB50057BCE0D90D9187A22B1"),
                Value::Null,
                json!({ "key": "utf-8", "input": "utf-8", "output": "hex" }),
            ],
        )
        .unwrap();
    assert_eq!(
        zcid,
        json!("a1a69db6a344da4d883865c637f979495dd86112a4534347b462259bfdeb641b")
    );

    // 2. Exact `paramsRoundTripBase64` vector from `RuntimeTests.swift:31-42`
    let key = "DE71B7D4C57C0749158CB90D5E431AE3";
    let enc = crypto
        .dispatch_sync(
            "$znode.crypto.encrypt",
            &[
                json!("{\"language\":\"vi\"}"),
                json!("aes-256-cbc"),
                json!(key),
                Value::Null,
                json!({ "key": "utf-8", "input": "utf-8", "output": "base64" }),
            ],
        )
        .unwrap();
    assert_eq!(enc, json!("V5p1S6tTwaE91W8UDCagn9vVP9wUQ3QzA04wG7xh+5E="));
    let dec = crypto
        .dispatch_sync(
            "$znode.crypto.decrypt",
            &[
                enc,
                json!("aes-256-cbc"),
                json!(key),
                 wire::undefined(),
                json!({ "key": "utf-8", "input": "base64", "output": "utf-8" }),
            ],
        )
        .unwrap();
    assert_eq!(dec, json!("{\"language\":\"vi\"}"));

    // 3. Exact `streamingCTRAndGCM` vectors from `RuntimeTests.swift:44-56`
    let ctr = crypto
        .dispatch_sync(
            "$znode.crypto.__cipherCreate",
            &[
                json!("cipher"),
                json!("aes-128-ctr"),
                wire::encode_bytes(&[7u8; 16]),
                wire::encode_bytes(&[1u8; 16]),
            ],
        )
        .unwrap()
        .as_u64()
        .unwrap();
    let ctr_out = crypto
        .dispatch_sync(
            "$znode.crypto.__cipherUpdate",
            &[json!(ctr), json!("hello zalo ctr mode!"), json!("utf8"), json!("hex")],
        )
        .unwrap();
    assert_eq!(ctr_out, json!("324a84ef5fc3a21b11456b948b319aab1c6e924d"));
    crypto
        .dispatch_sync("$znode.crypto.__cipherFinal", &[json!(ctr)])
        .unwrap();

    let gcm = crypto
        .dispatch_sync(
            "$znode.crypto.__cipherCreate",
            &[
                json!("cipher"),
                json!("aes-256-gcm"),
                wire::encode_bytes(&[3u8; 32]),
                wire::encode_bytes(&[9u8; 12]),
            ],
        )
        .unwrap()
        .as_u64()
        .unwrap();
    crypto
        .dispatch_sync(
            "$znode.crypto.__cipherUpdate",
            &[json!(gcm), json!("gcm payload"), json!("utf8"), Value::Null],
        )
        .unwrap();
    let sealed = crypto
        .dispatch_sync("$znode.crypto.__cipherFinal", &[json!(gcm), json!("hex")])
        .unwrap();
    assert_eq!(sealed, json!("636b00fbfe1c3505e56f0c"));
    let tag_wire = crypto
        .dispatch_sync(
            "$znode.crypto.__cipherOption",
            &[json!(gcm), json!("getAuthTag")],
        )
        .unwrap();
    assert_eq!(
        hex::encode(wire::decode_bytes(&tag_wire).unwrap()),
        "377ee9d6cfa423566629c0cfbb8eb885"
    );

    // 4. Exact `hashes` vectors from `RuntimeTests.swift:58-65`
    let h = crypto
        .dispatch_sync("$znode.crypto.__hashCreate", &[json!("md5")])
        .unwrap()
        .as_u64()
        .unwrap();
    crypto
        .dispatch_sync(
            "$znode.crypto.__hashUpdate",
            &[json!(h), json!("841630ae41f"), Value::Null],
        )
        .unwrap();
    assert_eq!(
        crypto
            .dispatch_sync("$znode.crypto.__hashDigest", &[json!(h), json!("hex")])
            .unwrap(),
        json!("d9e97c18b27cd54e969268d9b39e8680")
    );
    assert_eq!(
        crypto
            .dispatch_sync(
                "$znode.crypto.hash",
                &[json!("zalo"), json!("sha256"), Value::Null, json!("base64")],
            )
            .unwrap(),
        json!("PpYUHCFwTpavYlfqQcD8aQzgUEyFKdxCTciyRbY0PA8=")
    );

    // 5. Exact `badDecryptThrows` vector from `RuntimeTests.swift:67-74`
    assert!(crypto
        .dispatch_sync(
            "$znode.crypto.decrypt",
            &[
                json!("AAAAAAAAAAAAAAAAAAAAAA=="),
                json!("aes-256-cbc"),
                json!(key),
                Value::Null,
                json!({ "key": "utf-8", "input": "base64", "output": "utf-8" }),
            ],
        )
        .is_err());

    // Device ID MD5 vector verified against `node`
    let expected_ua_md5 = run_node_json(
        r#"console.log(JSON.stringify(require('crypto').createHash('md5').update('ZaloPC-darwin-23v694').digest('hex')));"#,
    );
    let md5 = crypto
        .dispatch_sync(
            "$znode.crypto.hash",
            &[json!("ZaloPC-darwin-23v694"), json!("md5"), json!("utf8"), json!("hex")],
        )
        .unwrap();
    assert_eq!(md5, expected_ua_md5);
}

#[test]
fn crypto_live_node_parity_cbc_ctr_ecb() {
    let node = run_node_json(
        r#"
        const crypto = require('crypto');
        const key16 = Buffer.from('000102030405060708090a0b0c0d0e0f', 'hex');
        const key24 = Buffer.from('000102030405060708090a0b0c0d0e0f1011121314151617', 'hex');
        const key32 = Buffer.from('000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f', 'hex');
        const iv16  = Buffer.from('f0e1d2c3b4a5968778695a4b3c2d1e0f', 'hex');
        const plain = Buffer.from('Xin chào Zalo Linux! Streaming & multi-block payload 🚀 0123456789abcdef', 'utf8');
        const exact32 = Buffer.from('0123456789abcdeffedcba9876543210', 'utf8');

        function run(alg, k, iv, data, autoPad = true) {
          const c = crypto.createCipheriv(alg, k, iv);
          c.setAutoPadding(autoPad);
          return Buffer.concat([c.update(data), c.final()]).toString('hex');
        }

        console.log(JSON.stringify({
          plainHex: plain.toString('hex'),
          exact32Hex: exact32.toString('hex'),
          cbc128: run('aes-128-cbc', key16, iv16, plain),
          cbc192: run('aes-192-cbc', key24, iv16, plain),
          cbc256: run('aes-256-cbc', key32, iv16, plain),
          cbc256NoPad: run('aes-256-cbc', key32, iv16, exact32, false),
          ctr128: run('aes-128-ctr', key16, iv16, plain),
          ctr192: run('aes-192-ctr', key24, iv16, plain),
          ctr256: run('aes-256-ctr', key32, iv16, plain),
          ecb128: run('aes-128-ecb', key16, null, plain),
          ecb192: run('aes-192-ecb', key24, null, plain),
          ecb256: run('aes-256-ecb', key32, null, plain),
          ecb128NoPad: run('aes-128-ecb', key16, null, exact32, false),
        }));
        "#,
    );

    let key16 = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
    let key24 = hex::decode("000102030405060708090a0b0c0d0e0f1011121314151617").unwrap();
    let key32 =
        hex::decode("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f").unwrap();
    let iv16 = hex::decode("f0e1d2c3b4a5968778695a4b3c2d1e0f").unwrap();
    let plain = hex::decode(node["plainHex"].as_str().unwrap()).unwrap();
    let exact32 = hex::decode(node["exact32Hex"].as_str().unwrap()).unwrap();

    let cb = CryptoBridge::new();

    for (alg, key, iv, field) in [
        ("aes-128-cbc", key16.as_slice(), Some(iv16.as_slice()), "cbc128"),
        ("aes-192-cbc", key24.as_slice(), Some(iv16.as_slice()), "cbc192"),
        ("aes-256-cbc", key32.as_slice(), Some(iv16.as_slice()), "cbc256"),
        ("aes-128-ctr", key16.as_slice(), Some(iv16.as_slice()), "ctr128"),
        ("aes-192-ctr", key24.as_slice(), Some(iv16.as_slice()), "ctr192"),
        ("aes-256-ctr", key32.as_slice(), Some(iv16.as_slice()), "ctr256"),
        ("aes-128-ecb", key16.as_slice(), None, "ecb128"),
        ("aes-192-ecb", key24.as_slice(), None, "ecb192"),
        ("aes-256-ecb", key32.as_slice(), None, "ecb256"),
    ] {
        let expected_ct = hex::decode(node[field].as_str().unwrap()).unwrap();

        // One-shot encrypt + decrypt
        let ct = cb.encrypt(alg, key, iv, &plain).unwrap();
        assert_eq!(ct, expected_ct, "one-shot encrypt mismatch for {alg}");
        let pt = cb.decrypt(alg, key, iv, &ct).unwrap();
        assert_eq!(pt, plain, "one-shot decrypt mismatch for {alg}");

        // Stateful handle streaming with awkward non-block-aligned chunk boundaries
        let iv_slice = iv.unwrap_or(&[]);
        let enc_h = cb.cipher_create(true, alg, key, iv_slice).unwrap();
        let mut streamed_ct = Vec::new();
        for chunk in [
            &plain[0..1],
            &plain[1..17],
            &plain[17..19],
            &plain[19..35],
            &plain[35..],
        ] {
            streamed_ct.extend_from_slice(&cb.cipher_update(enc_h, chunk).unwrap());
        }
        streamed_ct.extend_from_slice(&cb.cipher_final(enc_h).unwrap());
        assert_eq!(streamed_ct, expected_ct, "streamed encrypt mismatch for {alg}");

        let dec_h = cb.cipher_create(false, alg, key, iv_slice).unwrap();
        let mut streamed_pt = Vec::new();
        let mut cursor = 0;
        for step in [3usize, 14, 16, 5, 1, 19, 1024] {
            if cursor >= streamed_ct.len() {
                break;
            }
            let end = (cursor + step).min(streamed_ct.len());
            streamed_pt
                .extend_from_slice(&cb.cipher_update(dec_h, &streamed_ct[cursor..end]).unwrap());
            cursor = end;
        }
        streamed_pt.extend_from_slice(&cb.cipher_final(dec_h).unwrap());
        assert_eq!(streamed_pt, plain, "streamed decrypt mismatch for {alg}");
    }

    // autoPadding = false parity for CBC and ECB
    let h_cbc = cb.cipher_create(true, "aes-256-cbc", &key32, &iv16).unwrap();
    cb.cipher_set_auto_padding(h_cbc, false).unwrap();
    let mut cbc_nopad = cb.cipher_update(h_cbc, &exact32[..10]).unwrap();
    cbc_nopad.extend_from_slice(&cb.cipher_update(h_cbc, &exact32[10..]).unwrap());
    cbc_nopad.extend_from_slice(&cb.cipher_final(h_cbc).unwrap());
    assert_eq!(hex::encode(&cbc_nopad), node["cbc256NoPad"].as_str().unwrap());

    let h_cbc_dec = cb.cipher_create(false, "aes-256-cbc", &key32, &iv16).unwrap();
    cb.cipher_set_auto_padding(h_cbc_dec, false).unwrap();
    let mut cbc_nopad_pt = cb.cipher_update(h_cbc_dec, &cbc_nopad).unwrap();
    cbc_nopad_pt.extend_from_slice(&cb.cipher_final(h_cbc_dec).unwrap());
    assert_eq!(cbc_nopad_pt, exact32);

    let h_ecb = cb.cipher_create(true, "aes-128-ecb", &key16, &[]).unwrap();
    cb.cipher_set_auto_padding(h_ecb, false).unwrap();
    let mut ecb_nopad = cb.cipher_update(h_ecb, &exact32).unwrap();
    ecb_nopad.extend_from_slice(&cb.cipher_final(h_ecb).unwrap());
    assert_eq!(hex::encode(&ecb_nopad), node["ecb128NoPad"].as_str().unwrap());
}

#[test]
fn crypto_live_node_parity_gcm_12_and_16_byte_ivs_and_aad() {
    let node = run_node_json(
        r#"
        const crypto = require('crypto');
        const key16 = Buffer.from('00112233445566778899aabbccddeeff', 'hex');
        const key32 = Buffer.from('000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f', 'hex');
        const iv12  = Buffer.from('cafebabefacedbaddecaf888', 'hex');
        const iv16  = Buffer.from('0102030405060708090a0b0c0d0e0f10', 'hex');
        const aad   = Buffer.from('zalo-e2ee-associated-header-v2', 'utf8');
        const plain = Buffer.from('Secret Zalo E2EE attachment key material & message body!', 'utf8');

        function seal(alg, k, iv, withAad) {
          const c = crypto.createCipheriv(alg, k, iv);
          if (withAad) c.setAAD(aad);
          const ct = Buffer.concat([c.update(plain), c.final()]);
          return { ct: ct.toString('hex'), tag: c.getAuthTag().toString('hex') };
        }

        console.log(JSON.stringify({
          plainHex: plain.toString('hex'),
          aadHex: aad.toString('hex'),
          gcm128_iv12: seal('aes-128-gcm', key16, iv12, false),
          gcm128_iv16_aad: seal('aes-128-gcm', key16, iv16, true),
          gcm256_iv12_aad: seal('aes-256-gcm', key32, iv12, true),
          gcm256_iv16_aad: seal('aes-256-gcm', key32, iv16, true),
        }));
        "#,
    );

    let key16 = hex::decode("00112233445566778899aabbccddeeff").unwrap();
    let key32 =
        hex::decode("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f").unwrap();
    let iv12 = hex::decode("cafebabefacedbaddecaf888").unwrap();
    let iv16 = hex::decode("0102030405060708090a0b0c0d0e0f10").unwrap();
    let plain = hex::decode(node["plainHex"].as_str().unwrap()).unwrap();
    let aad = hex::decode(node["aadHex"].as_str().unwrap()).unwrap();

    let cb = CryptoBridge::new();

    for (alg, key, iv, use_aad, field) in [
        ("aes-128-gcm", key16.as_slice(), iv12.as_slice(), false, "gcm128_iv12"),
        (
            "aes-128-gcm",
            key16.as_slice(),
            iv16.as_slice(),
            true,
            "gcm128_iv16_aad",
        ),
        (
            "aes-256-gcm",
            key32.as_slice(),
            iv12.as_slice(),
            true,
            "gcm256_iv12_aad",
        ),
        (
            "aes-256-gcm",
            key32.as_slice(),
            iv16.as_slice(),
            true,
            "gcm256_iv16_aad",
        ),
    ] {
        let expected_ct = hex::decode(node[field]["ct"].as_str().unwrap()).unwrap();
        let expected_tag = hex::decode(node[field]["tag"].as_str().unwrap()).unwrap();

        // Encrypt via dispatch_sync (`__cipherCreate` -> `__cipherOption("aad")` -> `__cipherUpdate` -> `__cipherFinal` -> `__cipherOption("getAuthTag")`)
        let h_enc = cb
            .dispatch_sync(
                "$znode.crypto.__cipherCreate",
                &[
                    json!("cipher"),
                    json!(alg),
                    wire::encode_bytes(key),
                    wire::encode_bytes(iv),
                ],
            )
            .unwrap()
            .as_u64()
            .unwrap();
        if use_aad {
            cb.dispatch_sync(
                "$znode.crypto.__cipherOption",
                &[json!(h_enc), json!("aad"), wire::encode_bytes(&aad)],
            )
            .unwrap();
        }
        let u1 = wire::decode_bytes(
            &cb.dispatch_sync(
                "$znode.crypto.__cipherUpdate",
                &[json!(h_enc), wire::encode_bytes(&plain[..15])],
            )
            .unwrap(),
        )
        .unwrap();
        let u2 = wire::decode_bytes(
            &cb.dispatch_sync(
                "$znode.crypto.__cipherUpdate",
                &[json!(h_enc), wire::encode_bytes(&plain[15..])],
            )
            .unwrap(),
        )
        .unwrap();
        let fin = wire::decode_bytes(
            &cb.dispatch_sync("$znode.crypto.__cipherFinal", &[json!(h_enc)])
                .unwrap(),
        )
        .unwrap();

        // Handle MUST still be alive after `__cipherFinal` so `getAuthTag` succeeds!
        let tag_wire = cb
            .dispatch_sync(
                "$znode.crypto.__cipherOption",
                &[json!(h_enc), json!("getAuthTag")],
            )
            .unwrap();
        let actual_tag = wire::decode_bytes(&tag_wire).expect("16-byte GCM authTag");

        let mut actual_ct = Vec::new();
        actual_ct.extend_from_slice(&u1);
        actual_ct.extend_from_slice(&u2);
        actual_ct.extend_from_slice(&fin);

        assert_eq!(actual_ct, expected_ct, "GCM ciphertext mismatch for {field}");
        assert_eq!(actual_tag, expected_tag, "GCM authTag mismatch for {field}");

        // Decrypt back
        let h_dec = cb.cipher_create(false, alg, key, iv).unwrap();
        if use_aad {
            cb.cipher_set_aad(h_dec, &aad).unwrap();
        }
        cb.cipher_set_auth_tag(h_dec, &actual_tag).unwrap();
        let mut dec = cb.cipher_update(h_dec, &actual_ct).unwrap();
        dec.extend_from_slice(&cb.cipher_final(h_dec).unwrap());
        assert_eq!(dec, plain, "GCM decrypt roundtrip mismatch for {field}");

        // Tampered authTag must fail
        let mut bad_tag = actual_tag.clone();
        bad_tag[0] ^= 0x01;
        let h_bad = cb.cipher_create(false, alg, key, iv).unwrap();
        if use_aad {
            cb.cipher_set_aad(h_bad, &aad).unwrap();
        }
        cb.cipher_set_auth_tag(h_bad, &bad_tag).unwrap();
        cb.cipher_update(h_bad, &actual_ct).unwrap();
        assert!(cb.cipher_final(h_bad).is_err(), "tampered GCM tag must fail");

        // Tampered AAD must fail when AAD was bound
        if use_aad {
            let h_bad_aad = cb.cipher_create(false, alg, key, iv).unwrap();
            cb.cipher_set_aad(h_bad_aad, b"wrong-aad").unwrap();
            cb.cipher_set_auth_tag(h_bad_aad, &actual_tag).unwrap();
            cb.cipher_update(h_bad_aad, &actual_ct).unwrap();
            assert!(
                cb.cipher_final(h_bad_aad).is_err(),
                "tampered GCM AAD must fail"
            );
        }
    }
}

#[test]
fn crypto_strict_pkcs7_bad_decrypt_rejection() {
    let cb = CryptoBridge::new();
    let key = [0x42u8; 32];
    let iv = [0x24u8; 16];

    // Encrypt raw 16-byte blocks with auto_padding = false to craft exact invalid PKCS#7 blocks
    let encrypt_raw_block = |raw_plain: [u8; 16]| -> Vec<u8> {
        let h = cb.cipher_create(true, "aes-256-cbc", &key, &iv).unwrap();
        cb.cipher_set_auto_padding(h, false).unwrap();
        let mut out = cb.cipher_update(h, &raw_plain).unwrap();
        out.extend_from_slice(&cb.cipher_final(h).unwrap());
        out
    };

    // Case 1: pad byte == 0
    let zero_pad_ct = encrypt_raw_block([0u8; 16]);
    assert_eq!(
        cb.decrypt("aes-256-cbc", &key, Some(&iv), &zero_pad_ct),
        Err("bad decrypt".to_string())
    );

    // Case 2: pad byte > 16 (e.g. 17)
    let mut gt16 = [0xAAu8; 16];
    gt16[15] = 17;
    let gt16_ct = encrypt_raw_block(gt16);
    assert_eq!(
        cb.decrypt("aes-256-cbc", &key, Some(&iv), &gt16_ct),
        Err("bad decrypt".to_string())
    );

    // Case 3: inconsistent pad bytes `[..., 3, 99, 3]`
    let mut mismatch = [0xBBu8; 16];
    mismatch[13] = 3;
    mismatch[14] = 99;
    mismatch[15] = 3;
    let mismatch_ct = encrypt_raw_block(mismatch);
    assert_eq!(
        cb.decrypt("aes-256-cbc", &key, Some(&iv), &mismatch_ct),
        Err("bad decrypt".to_string())
    );

    // Case 4: non-block-aligned ciphertext
    assert_eq!(
        cb.decrypt("aes-256-cbc", &key, Some(&iv), &[1, 2, 3, 4, 5]),
        Err("bad decrypt".to_string())
    );
}

#[test]
fn crypto_live_node_parity_hashes_and_encodings() {
    let node = run_node_json(
        r#"
        const crypto = require('crypto');
        const msg = Buffer.from('Zalo-Olaz-Linux-Parity-Test- Tiếng Việt 🎉', 'utf8');
        const hashes = {};
        for (const alg of ['md5', 'sha1', 'sha256', 'sha384', 'sha512']) {
          hashes[alg] = crypto.createHash(alg).update(msg).digest('hex');
        }

        const utf16Sample = 'Xin chào 🚀🎉';
        const utf16Buf = Buffer.from(utf16Sample, 'utf16le');
        const oddUtf16 = Buffer.from([0x41, 0x00, 0x42, 0x00, 0xff]);
        const b64UrlSample = Buffer.from([0xfb, 0xff, 0xfe, 0x01, 0x02]).toString('base64url');

        console.log(JSON.stringify({
          msgHex: msg.toString('hex'),
          hashes,
          hexTrunc: Buffer.from('deadbeefZZ99', 'hex').toString('hex'),
          hexOdd: Buffer.from('abcdef0', 'hex').toString('hex'),
          forgivingB64: Buffer.from('  YWJj \n -_ == ', 'base64').toString('hex'),
          b64UrlSample,
          b64UrlDecoded: Buffer.from(b64UrlSample, 'base64url').toString('hex'),
          latin1Encoded: Buffer.from('héllo\u1234', 'latin1').toString('hex'),
          utf16Hex: utf16Buf.toString('hex'),
          utf16Roundtrip: utf16Buf.toString('utf16le'),
          oddUtf16Str: oddUtf16.toString('utf16le'),
        }));
        "#,
    );

    let cb = CryptoBridge::new();
    let msg = hex::decode(node["msgHex"].as_str().unwrap()).unwrap();

    for (alg, hyphen_alias) in [
        ("md5", "MD5"),
        ("sha1", "SHA-1"),
        ("sha256", "sha-256"),
        ("sha384", "SHA-384"),
        ("sha512", "sha-512"),
    ] {
        let expected = node["hashes"][alg].as_str().unwrap();
        assert_eq!(hex::encode(cb.hash(alg, &msg).unwrap()), expected);
        assert_eq!(hex::encode(cb.hash(hyphen_alias, &msg).unwrap()), expected);

        // Stateful hash handle + double-digest error check
        let h = cb
            .dispatch_sync("$znode.crypto.__hashCreate", &[json!(hyphen_alias)])
            .unwrap()
            .as_u64()
            .unwrap();
        cb.dispatch_sync(
            "$znode.crypto.__hashUpdate",
            &[json!(h), wire::encode_bytes(&msg[..9])],
        )
        .unwrap();
        cb.dispatch_sync(
            "$znode.crypto.__hashUpdate",
            &[json!(h), wire::encode_bytes(&msg[9..])],
        )
        .unwrap();
        let digest = cb
            .dispatch_sync("$znode.crypto.__hashDigest", &[json!(h), json!("hex")])
            .unwrap();
        assert_eq!(digest, json!(expected));

        let second = cb.dispatch_sync("$znode.crypto.__hashDigest", &[json!(h), json!("hex")]);
        assert_eq!(second, Err("Digest already called".to_string()));
    }

    // Encodings parity
    assert_eq!(
        hex::encode(CryptoBridge::decode_string("deadbeefZZ99", "hex").unwrap()),
        node["hexTrunc"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(CryptoBridge::decode_string("abcdef0", "hex").unwrap()),
        node["hexOdd"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(CryptoBridge::decode_string("  YWJj \n -_ == ", "base64").unwrap()),
        node["forgivingB64"].as_str().unwrap()
    );

    let b64_url_sample = node["b64UrlSample"].as_str().unwrap();
    assert_eq!(
        CryptoBridge::encode_output(&[0xfb, 0xff, 0xfe, 0x01, 0x02], Some("base64url")),
        json!(b64_url_sample)
    );
    assert_eq!(
        hex::encode(CryptoBridge::decode_string(b64_url_sample, "base64url").unwrap()),
        node["b64UrlDecoded"].as_str().unwrap()
    );

    assert_eq!(
        hex::encode(CryptoBridge::decode_string("héllo\u{1234}", "latin1").unwrap()),
        node["latin1Encoded"].as_str().unwrap()
    );

    let utf16_bytes = CryptoBridge::decode_string("Xin chào 🚀🎉", "utf16le").unwrap();
    assert_eq!(hex::encode(&utf16_bytes), node["utf16Hex"].as_str().unwrap());
    assert_eq!(
        CryptoBridge::encode_output(&utf16_bytes, Some("ucs2")),
        node["utf16Roundtrip"]
    );
    assert_eq!(
        CryptoBridge::encode_output(&[0x41, 0x00, 0x42, 0x00, 0xff], Some("utf-16le")),
        node["oddUtf16Str"]
    );

    // None output -> ArrayBuffer wire envelope
    let ab = CryptoBridge::encode_output(&[1, 2, 3], None);
    assert_eq!(ab["__arraybuffer"], json!(true));
    assert_eq!(wire::decode_bytes(&ab), Some(vec![1, 2, 3]));

    // randomBytes
    let rand_wire = cb
        .dispatch_sync("$znode.crypto.randomBytes", &[json!(32)])
        .unwrap();
    assert_eq!(rand_wire["__arraybuffer"], json!(true));
    let rand_bytes = wire::decode_bytes(&rand_wire).unwrap();
    assert_eq!(rand_bytes.len(), 32);
    assert!(rand_bytes.iter().any(|&b| b != 0));
}

// ============================================================================
// 3. Synthetic ASAR Pickle archive reader + MIME mapping
// ============================================================================

#[test]
fn synthetic_asar_pickle_reader_and_mime_table() {
    let tmp = TempDir::new().unwrap();
    let asar_path = tmp.path().join("app.asar");

    build_synthetic_asar(
        &asar_path,
        &[
            ("package.json", Some(br#"{"name":"Zalo"}"#)),
            ("pc-dist/index.html", Some(b"<!doctype html><title>Zalo</title>")),
            ("pc-dist/assets/hello world.txt", Some(b"percent decoded ok")),
            ("native/addon.node", None),
        ],
    );

    // Populate companion `app.asar.unpacked` file
    let unpacked_target = tmp.path().join("app.asar.unpacked/native/addon.node");
    std::fs::create_dir_all(unpacked_target.parent().unwrap()).unwrap();
    std::fs::write(&unpacked_target, b"ELF-UNPACKED").unwrap();

    let archive = AsarArchive::open(&asar_path).unwrap();
    assert_eq!(archive.entries().len(), 4);
    assert_eq!(
        archive.entry("package.json"),
        Some(&AsarEntry {
            offset: 0,
            size: 15,
            unpacked: false
        })
    );
    assert_eq!(archive.read("package.json"), Some(&br#"{"name":"Zalo"}"#[..]));
    assert_eq!(
        archive.read("/pc-dist/index.html"),
        Some(&b"<!doctype html><title>Zalo</title>"[..])
    );
    assert_eq!(
        archive.read("/pc-dist/assets/hello%20world.txt"),
        Some(&b"percent decoded ok"[..])
    );
    assert_eq!(archive.read("missing.js"), None);

    // Unpacked entry returns `None` from zero-copy `read()`, `Some` from `read_or_unpacked()`
    assert_eq!(archive.read("native/addon.node"), None);
    assert_eq!(
        archive.read_or_unpacked("/native/addon.node"),
        Some(Cow::Owned(b"ELF-UNPACKED".to_vec()))
    );

    assert_eq!(
        archive.paths_with_prefix("/pc-dist/"),
        vec!["pc-dist/assets/hello world.txt", "pc-dist/index.html"]
    );

    // Corrupt small file is rejected cleanly
    let bad_path = tmp.path().join("bad.asar");
    std::fs::write(&bad_path, b"short").unwrap();
    assert!(AsarArchive::open(&bad_path).is_err());

    // Required MIME mappings
    for (p, expected) in [
        ("pc-dist/index.html?type=23", "text/html; charset=utf-8"),
        ("pc-dist/dal-worker.123.js", "text/javascript; charset=utf-8"),
        ("pc-dist/esm.mjs", "text/javascript; charset=utf-8"),
        ("pc-dist/styles.css", "text/css; charset=utf-8"),
        ("package.json", "application/json"),
        ("zstd.wasm", "application/wasm"),
        ("icon.svg", "image/svg+xml"),
        ("avatar.png", "image/png"),
        ("photo.jpg", "image/jpeg"),
        ("sticker.webp", "image/webp"),
        ("anim.gif", "image/gif"),
        ("ringtone.mp3", "audio/mpeg"),
        ("clip.mp4", "video/mp4"),
        ("font.woff2", "font/woff2"),
        ("font.ttf", "font/ttf"),
    ] {
        assert_eq!(mime_for_path(p), expected, "MIME mismatch for {p}");
    }
}

// ============================================================================
// 4. Vault: KeychainCipher + SafeStorage (`zsskv=2`)
// ============================================================================

#[tokio::test]
async fn keychain_cipher_roundtrip_tamper_and_locked_safety() {
    let backend = MemorySecretBackend::new();
    let cipher = KeychainCipher::new(backend);

    let items: Vec<&[u8]> = vec![
        b"",
        b"trusted-device-session-secret",
        &[0x00, 0xff, 0x49, 0x13, 0x37],
    ];

    let sealed1 = cipher.encrypt("Account_ABC", &items).await.unwrap();
    let sealed2 = cipher.encrypt("account_abc", &items).await.unwrap();
    assert_eq!(sealed1.len(), 3);
    for (s1, s2) in sealed1.iter().zip(sealed2.iter()) {
        assert!(s1.len() >= 65);
        assert_eq!(s1[0], 0x49);
        // Fresh random IV + salt per call -> distinct ciphertexts
        assert_ne!(s1, s2);
    }

    let opened = cipher.decrypt("account_abc", &sealed1).await.unwrap();
    assert_eq!(
        opened,
        items.iter().map(|b| b.to_vec()).collect::<Vec<Vec<u8>>>()
    );

    // Wire adapter roundtrip
    let wire_enc = cipher
        .encrypt_wire(&[json!("wire-id"), json!([wire::encode_bytes(b"hello-wire")])])
        .await
        .unwrap();
    let wire_dec = cipher
        .decrypt_wire(&[json!("wire-id"), wire_enc])
        .await
        .unwrap();
    let arr = wire_dec.as_array().unwrap();
    assert_eq!(wire::decode_bytes(&arr[0]).as_deref(), Some(&b"hello-wire"[..]));

    // Tamper checks
    let valid = &sealed1[1];

    // 1. Truncated < 65 bytes
    assert!(matches!(
        cipher.decrypt("account_abc", &[&valid[..64]]).await,
        Err(VaultError::InvalidLength)
    ));

    // 2. Wrong header byte
    let mut bad_hdr = valid.clone();
    bad_hdr[0] = 0x50;
    assert!(matches!(
        cipher.decrypt("account_abc", &[bad_hdr]).await,
        Err(VaultError::InvalidHeader)
    ));

    // 3. Flipped IV, salt, ciphertext, and HMAC bytes all fail ConstantTimeEq MAC check
    for flip_idx in [1usize, 17, 33, valid.len() - 1] {
        let mut tampered = valid.clone();
        tampered[flip_idx] ^= 0x80;
        assert!(
            matches!(
                cipher.decrypt("account_abc", &[tampered]).await,
                Err(VaultError::InvalidMac)
            ),
            "tamper at byte {flip_idx} was not rejected with InvalidMac"
        );
    }

    // 4. Unknown identifier returns NotFound (never creates a random key on decrypt)
    assert!(matches!(
        cipher.decrypt("nonexistent-user", &[valid.clone()]).await,
        Err(VaultError::NotFound(_))
    ));

    // 5. CRITICAL: Locked keyring MUST return Err(VaultError::Locked) and MUST NEVER
    //    overwrite the existing root key with a new random key!
    cipher.backend().set_locked(true);
    assert!(matches!(
        cipher.encrypt("account_abc", &[b"while-locked"]).await,
        Err(VaultError::Locked(_))
    ));
    assert!(matches!(
        cipher.decrypt("account_abc", &sealed1).await,
        Err(VaultError::Locked(_))
    ));

    // Unlock and verify previously encrypted credentials STILL decrypt cleanly!
    cipher.backend().set_locked(false);
    let after_unlock = cipher.decrypt("account_abc", &sealed1).await.unwrap();
    assert_eq!(
        after_unlock,
        items.iter().map(|b| b.to_vec()).collect::<Vec<Vec<u8>>>()
    );
}

#[test]
fn safe_storage_zsskv2_node_compatibility_and_dot_key_guard() {
    let tmp = TempDir::new().unwrap();
    let platform = ZaloPlatform::with_root(tmp.path(), "26.10.20");
    let ss = SafeStorage::new();

    // Cross-check SafeStorage::encrypt_string against Node's AES-256-CBC with the exact fixed key & IV
    let secret_text = "zpw_sek=super-secret-session-cookie-value";
    let sealed = ss
        .execute(&platform, "encryptString", &[json!(secret_text)])
        .unwrap();
    let sealed_str = sealed.as_str().unwrap();

    let node_sealed = run_node_json(&format!(
        r#"
        const crypto = require('crypto');
        const key = Buffer.from('d9c07b9de9915af401607d3f360211ac', 'utf8');
        const iv = Buffer.from([155, 72, 37, 195, 9, 217, 80, 42, 145, 102, 171, 75, 214, 176, 134, 18]);
        const c = crypto.createCipheriv('aes-256-cbc', key, iv);
        const out = Buffer.concat([c.update({plain:?}, 'utf8'), c.final()]).toString('base64');
        console.log(JSON.stringify({{ sealed: out }}));
        "#,
        plain = secret_text
    ));
    assert_eq!(sealed_str, node_sealed["sealed"].as_str().unwrap());
    assert_eq!(
        ss.execute(&platform, "decryptString", &[json!(sealed_str)])
            .unwrap(),
        json!(secret_text)
    );

    // Store set / get / on-disk zsskv="2" format
    let set_res = ss
        .execute(&platform, "set", &[json!("auth_token"), json!(secret_text)])
        .unwrap();
    assert!(wire::is_undefined(&set_res));

    let raw_store = platform.read_store("zsafe-storage");
    assert_eq!(raw_store.get("zsskv"), Some(&json!("2")));
    assert_eq!(
        raw_store["zsskd"]["auth_token"],
        json!(sealed_str)
    );

    assert_eq!(
        ss.execute(&platform, "get", &[json!("auth_token")]).unwrap(),
        json!(secret_text)
    );

    // Keys containing `.` must return null without touching the store
    assert_eq!(
        ss.execute(&platform, "set", &[json!("nested.key"), json!("bad")])
            .unwrap(),
        Value::Null
    );
    assert_eq!(
        ss.execute(&platform, "get", &[json!("nested.key")]).unwrap(),
        Value::Null
    );

    // Non-string value deletes key
    ss.execute(&platform, "set", &[json!("auth_token"), Value::Null])
        .unwrap();
    assert_eq!(
        ss.execute(&platform, "get", &[json!("auth_token")]).unwrap(),
        Value::Null
    );
}

#[tokio::test]
async fn linux_secret_backend_headless_keyfile_permissions() {
    // Verify that when D-Bus Secret Service is unreachable (e.g. pointed at a nonexistent socket
    // in a child helper or when absent), the fallback keyfile is written with 0600 permissions.
    // Run in a subprocess with an isolated `DBUS_SESSION_BUS_ADDRESS` so we don't mutate env vars
    // in the multithreaded test runner or touch the developer's live desktop keyring!
    if std::env::var("OLAZ_SUBTEST_HEADLESS_KEYFILE").as_deref() == Ok("1") {
        let dir = PathBuf::from(std::env::var("OLAZ_SUBTEST_DIR").unwrap());
        let backend = LinuxSecretBackend::new(&dir);
        let secret = [0x5Au8; 32];
        backend
            .set_secret("app.olaz.secure", "zprotocol.headless", &secret)
            .await
            .unwrap();
        let loaded = backend
            .get_secret("app.olaz.secure", "zprotocol.headless")
            .await
            .unwrap();
        assert_eq!(loaded.as_deref(), Some(&secret[..]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let key_path = dir.join("keys/zprotocol.headless.key");
            let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "fallback keyfile must be chmod 0600");
        }
        return;
    }

    let tmp = TempDir::new().unwrap();
    let exe = std::env::current_exe().unwrap();
    let status = Command::new(exe)
        .arg("--exact")
        .arg("linux_secret_backend_headless_keyfile_permissions")
        .env("OLAZ_SUBTEST_HEADLESS_KEYFILE", "1")
        .env("OLAZ_SUBTEST_DIR", tmp.path())
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/olaz-no-dbus.sock")
        .status()
        .unwrap();
    assert!(status.success(), "headless keyfile subprocess test failed");
}

// ============================================================================
// 5. Platform + Symlink Escape Prevention + FsBridge
// ============================================================================

#[tokio::test]
async fn platform_identity_imei_stores_adapter_and_symlink_confinement() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("olaz-root");
    let outside = tmp.path().join("outside-secret");
    std::fs::create_dir_all(&outside).unwrap();
    let outside_file = outside.join("passwd.txt");
    std::fs::write(&outside_file, b"do-not-overwrite").unwrap();

    let platform = Arc::new(ZaloPlatform::with_root(&root, "26.10.20"));
    assert_eq!(ClientIdentity::default().user_agent(), "ZaloPC-darwin-23v694");

    // Persistent IMEI across platform reloads
    let imei1 = platform.imei();
    assert_eq!(imei1.len(), 36);
    assert_eq!(imei1, imei1.to_lowercase());
    let platform_reloaded = ZaloPlatform::with_root(&root, "26.10.20");
    assert_eq!(platform_reloaded.imei(), imei1);

    // Store sanitization & sorted deterministic JSON
    let traversal_path = platform.store_path("../../etc/shadow");
    assert_eq!(traversal_path.parent(), Some(platform.user_data()));
    let mut store = Map::new();
    store.insert("z_last".into(), json!(1));
    store.insert("a_first".into(), json!(2));
    platform.write_store("my-store", &store).unwrap();
    let on_disk = std::fs::read_to_string(platform.store_path("my-store")).unwrap();
    assert!(on_disk.find("\"a_first\"").unwrap() < on_disk.find("\"z_last\"").unwrap());

    // Database adapter normalization (`*adp-st` -> 0, strip `*upgradeRequired*`)
    let mut db_cfg = Map::new();
    db_cfg.insert(
        "987654_adp-st".into(),
        json!("{\"msg\":2,\"group\":1,\"friend\":0}"),
    );
    db_cfg.insert("111111_adp-st".into(), json!(3));
    db_cfg.insert("sqliteUpgradeRequired".into(), json!(true));
    db_cfg.insert("keepKey".into(), json!("preserved"));
    platform.write_store("database-config", &db_cfg).unwrap();
    platform.normalize_database_adapter_state();

    let norm_cfg = platform.read_store("database-config");
    assert!(!norm_cfg.contains_key("sqliteUpgradeRequired"));
    assert_eq!(norm_cfg.get("keepKey"), Some(&json!("preserved")));
    assert_eq!(norm_cfg.get("111111_adp-st"), Some(&json!(0)));
    assert_eq!(
        norm_cfg.get("987654_adp-st"),
        Some(&json!("{\"friend\":0,\"group\":0,\"msg\":0}"))
    );

    // `is_writable` confinement + symlink escape prevention
    assert!(platform.is_writable(&platform.user_data().join("nested/new/file.txt")));
    assert!(platform.is_writable(&platform.temp_dir().join("scratch.tmp")));
    assert!(platform.is_writable(&platform.electron_path("downloads").join("report.pdf")));

    assert!(!platform.is_writable(Path::new("")));
    assert!(!platform.is_writable(&outside_file));
    assert!(!platform.is_writable(&platform.runtime_current().join("Contents/Resources/app.asar")));
    assert!(!platform.is_writable(&platform.user_data().join("../../outside-secret/pwn.txt")));

    #[cfg(unix)]
    {
        // Escape attempt 1: symlink directory inside `userData` -> `outside`
        let sym_dir = platform.user_data().join("escape_dir");
        std::os::unix::fs::symlink(&outside, &sym_dir).unwrap();
        assert!(!platform.is_writable(&sym_dir.join("new_evil.txt")));
        assert!(!platform.is_writable(&sym_dir.join("passwd.txt")));

        // Escape attempt 2: symlink file inside `userData` -> `outside_file`
        let sym_file = platform.user_data().join("escape_file.txt");
        std::os::unix::fs::symlink(&outside_file, &sym_file).unwrap();
        assert!(!platform.is_writable(&sym_file));

        // Escape attempt 3: dangling symlink inside `userData` -> nonexistent outside target
        let dangling = platform.user_data().join("dangling_escape.txt");
        std::os::unix::fs::symlink(outside.join("future_target.txt"), &dangling).unwrap();
        assert!(!platform.is_writable(&dangling));

        // Verify FsBridge rejects writes through symlink escapes with `EACCES:`
        let fs = FsBridge::new(platform.clone());
        let err = fs
            .write_file_sync(&sym_dir.join("blocked.txt"), b"nope")
            .unwrap_err();
        assert_eq!(err.code, "EACCES");
        assert!(err.to_string().starts_with("EACCES:"));
        assert_eq!(std::fs::read(&outside_file).unwrap(), b"do-not-overwrite");
    }

    // FsBridge sync + async operations inside allowed sandbox
    let fs = FsBridge::new(platform.clone());
    let missing = platform.user_data().join("does-not-exist.bin");
    let enoent = fs.read_file_sync(&missing).unwrap_err();
    assert!(enoent.to_string().starts_with("ENOENT:"));

    let sub = platform.user_data().join("chat/media");
    fs.mkdir(&sub, true).await.unwrap();
    let f1 = sub.join("hello.txt");
    fs.write_file(&f1, b"Hello async FS!").await.unwrap();
    assert!(fs.exists(&f1).await);
    assert_eq!(
        fs.read_file_async(&f1, Some("utf8")).await.unwrap(),
        json!("Hello async FS!")
    );
    let ab = fs.get_file_array_buffer(&f1).await.unwrap();
    assert_eq!(ab["__arraybuffer"], json!(true));
    assert_eq!(
        wire::decode_bytes(&ab).as_deref(),
        Some(&b"Hello async FS!"[..])
    );

    let f2 = sub.join("copied.txt");
    fs.copy_file(&f1, &f2).await.unwrap();
    let f3 = sub.join("renamed.txt");
    fs.rename(&f2, &f3).await.unwrap();
    assert_eq!(
        fs.readdir(&sub).await.unwrap(),
        vec!["hello.txt".to_string(), "renamed.txt".to_string()]
    );
    fs.unlink(&f3).await.unwrap();
    assert!(!fs.exists(&f3).await);
    fs.rm(&platform.user_data().join("chat"), true, true)
        .await
        .unwrap();
    assert!(!fs.exists(&sub).await);
}

// ============================================================================
// 6. Bundle Patcher + Host Scripts + `SyncGlobalIdPatchTests` live JS execution
// ============================================================================

#[test]
fn bundle_patcher_scripts_and_worker_rewriters() {
    let tmp = TempDir::new().unwrap();
    let platform = ZaloPlatform::with_root(tmp.path(), "26.10.20");

    // 1. `__zaloBridgeSpec` structure
    let spec_js = bridge_spec_script(&platform);
    let json_str = spec_js
        .strip_prefix("window.__zaloBridgeSpec = ")
        .and_then(|s| s.strip_suffix(';'))
        .expect("bridge_spec_script wrapper");
    let spec: Value = serde_json::from_str(json_str).unwrap();
    assert_eq!(spec["values"]["$zenv.platform"], json!("darwin"));
    assert_eq!(spec["members"]["$znode.crypto.encrypt"], json!("sync"));
    assert_eq!(spec["members"]["$znode.fs.readFile"], json!("async"));
    assert!(!bridge_ipc_table().is_empty());
    assert!(spec["ipc"]["$capp.getRelaunchState"].is_object());

    // 2. Exact 21 ordered user scripts
    let scripts = ordered_user_scripts(&platform, true);
    assert_eq!(scripts.len(), 21);
    let names: Vec<&str> = scripts.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "console",
            "network-shim",
            "bridge-spec",
            "local-modules",
            "bridge",
            "state-adapter",
            "chat-adapter",
            "media-store-adapter",
            "card-actions-adapter",
            "sticker-adapter",
            "composer-adapter",
            "message-actions-adapter",
            "transfer-adapter",
            "conversations-adapter",
            "conversation-actions-adapter",
            "contacts-adapter",
            "group-create-adapter",
            "notification-adapter",
            "zcloud-adapter",
            "poll-adapter",
            "call-adapter",
        ]
    );
    assert!(scripts[0].1.starts_with("window.__zaloTraceWorkers = true;\n"));

    // 3. `dal-worker.*.js` prepends `worker_prelude()`
    let dal_orig = b"self.postMessage('dal-ready');";
    let dal_patched = patch_asar_bundle("/pc-dist/dal-worker.7a2bc14df93c.js", dal_orig).unwrap();
    assert!(dal_patched.starts_with(worker_prelude().as_bytes()));
    assert!(dal_patched.ends_with(dal_orig));

    // 4. `sync-v2-sub-worker.*.js` exposes `self.__swReq=<ident>;` at both `;` and `}` boundaries
    let sw_orig = b"!function(modules){var a={};function r(n){return modules[n];}r.c={};r.e=function(e){var t=[],n=a[e];return t}}([]);";
    let sw_patched =
        patch_asar_bundle("/sync-v2-sub-worker.4f03aa362c.js?ver=1", sw_orig).unwrap();
    let sw_str = std::str::from_utf8(&sw_patched).unwrap();
    assert!(sw_str.contains(";self.__swReq=r;r.e=function(e){var t=[],n=a[e]"));

    let sw_brace = b"!function(modules){var a={};function r(n){return modules[n]}r.e=function(e){var t=[],n=a[e];return t}}([]);";
    let sw_brace_patched = expose_webpack_require(sw_brace).unwrap();
    assert!(std::str::from_utf8(&sw_brace_patched)
        .unwrap()
        .contains("}self.__swReq=r;r.e=function(e){var t=[],n=a[e]"));

    // Verify in `node` that `self.__swReq` is actually exposed and callable
    let sw_eval = run_node_json(&format!(
        "globalThis.self = globalThis; {sw_str}\nconsole.log(JSON.stringify({{ exposed: typeof self.__swReq === 'function' && typeof self.__swReq.c === 'object' }}));"
    ));
    assert_eq!(sw_eval["exposed"], json!(true));

    // Shape guards on `expose_webpack_require`: missing, ambiguous, non-statement boundary
    assert!(expose_webpack_require(b"console.log('no runtime');").is_none());
    let dup = [sw_orig.as_slice(), sw_orig.as_slice()].concat();
    assert!(expose_webpack_require(&dup).is_none());
    let bad_boundary = b"var x=r.e=function(e){var t=[],n=a[e];return t};";
    assert!(expose_webpack_require(bad_boundary).is_none());

    // Unrelated asset returns `None`
    assert!(patch_asar_bundle("/pc-dist/vendor.123.js", b"var a=1;").is_none());
}

#[test]
fn main_startup_sync_global_id_patch_matches_swift_tests() {
    // Port of `Tests/OlazTests/SyncGlobalIdPatchTests.swift` executed in live `node`!
    let bundle = br#"const x=1;return o.resolveWithRetry(r,(e=>a.default.getGlobalIdsFromUserIds(e)));"#;
    let patched = patch_asar_bundle("/lazy/main-startup.99945eb37e.js", bundle)
        .expect("main-startup should be rewritten");
    let patched_fn = String::from_utf8(patched).unwrap();

    // Case 1: Fallback resolves missing IDs while preserving IDB hits
    let res1 = run_node_json(&format!(
        r#"
        (async () => {{
          globalThis.window = {{}};
          const o = {{ resolveWithRetry: (ids, fn) => fn(ids) }};
          const a = {{ default: {{ getGlobalIdsFromUserIds: async () => ({{ '100': 'G100' }}) }} }};
          let fallbackArg = null;
          window.__swzGlobalIdFallback = async (missing) => {{
            fallbackArg = missing;
            return {{ '300': 'G300' }};
          }};
          const r = ['100', '300'];
          const out = await (async () => {{ {patched_fn} }})();
          console.log(JSON.stringify({{ out, fallbackArg }}));
        }})();
        "#
    ));
    assert_eq!(res1["out"], json!({ "100": "G100", "300": "G300" }));
    assert_eq!(res1["fallbackArg"], json!(["300"]));

    // Case 2: Safe when fallback hook is not installed
    let res2 = run_node_json(&format!(
        r#"
        (async () => {{
          globalThis.window = {{}};
          const o = {{ resolveWithRetry: (ids, fn) => fn(ids) }};
          const a = {{ default: {{ getGlobalIdsFromUserIds: async () => ({{ '100': 'G100' }}) }} }};
          const r = ['100', '300'];
          const out = await (async () => {{ {patched_fn} }})();
          console.log(JSON.stringify({{ out }}));
        }})();
        "#
    ));
    assert_eq!(res2["out"], json!({ "100": "G100" }));

    // Case 3: Throwing fallback hook is swallowed and IDB hits survive
    let res3 = run_node_json(&format!(
        r#"
        (async () => {{
          globalThis.window = {{
            __swzGlobalIdFallback: async () => {{ throw new Error('boom'); }}
          }};
          const o = {{ resolveWithRetry: (ids, fn) => fn(ids) }};
          const a = {{ default: {{ getGlobalIdsFromUserIds: async () => ({{ '100': 'G100' }}) }} }};
          const r = ['100', '300'];
          const out = await (async () => {{ {patched_fn} }})();
          console.log(JSON.stringify({{ out }}));
        }})();
        "#
    ));
    assert_eq!(res3["out"], json!({ "100": "G100" }));

    // Case 4: Mismatched arrow parameter (`e` vs `t`) is left unpatched
    assert!(
        patch_sync_global_id_resolver(
            b"o.resolveWithRetry(r,(e=>a.default.getGlobalIdsFromUserIds(t)))"
        )
        .is_none()
    );

    // Case 5: Missing or duplicated pattern returns `None`
    assert!(patch_sync_global_id_resolver(b"const untouched = true;").is_none());
    let ambiguous = format!(
        "{};{}",
        std::str::from_utf8(bundle).unwrap(),
        std::str::from_utf8(bundle).unwrap()
    );
    assert!(patch_sync_global_id_resolver(ambiguous.as_bytes()).is_none());
}

// ============================================================================
// 7. Runtime Installer (`ZaloRuntimeManager` + `7z` extraction + SHA-256)
// ============================================================================

#[tokio::test]
async fn runtime_installer_pinned_sha256_promotion_and_7z_extract() {
    assert_eq!(ZaloRelease::PINNED.version, "26.10.20");
    assert_eq!(
        ZaloRelease::PINNED.url,
        "https://res-download-pc.zadn.vn/mac/ZaloSetup-universal-26.10.20.dmg"
    );
    assert_eq!(
        ZaloRelease::PINNED.sha256.as_deref(),
        Some("a45d30b7f7b060f76b87722ef419b8f41d23a2913cdef084dcb0955dde8d85a0")
    );
    assert_eq!(
        ZaloRuntimeManager::version_from_installer_name("ZaloSetup-universal-26.10.20.dmg")
            .as_deref(),
        Some("26.10.20")
    );
    assert!(ZaloRuntimeManager::is_newer("26.10.20", "26.9.99"));
    assert!(!ZaloRuntimeManager::is_newer("26.10.20", "26.10.20"));

    let tmp = TempDir::new().unwrap();

    // Verify streaming SHA-256 over a > 4 MiB file (5 MiB + 123 bytes)
    let big_file = tmp.path().join("multi-chunk.bin");
    let big_data: Vec<u8> = (0..(5 * 1024 * 1024 + 123))
        .map(|i| (i % 251) as u8)
        .collect();
    std::fs::write(&big_file, &big_data).unwrap();
    let expected_sha = hex::encode(CryptoBridge::new().hash("sha256", &big_data).unwrap());
    assert_eq!(ZaloRuntimeManager::sha256_file(&big_file).unwrap(), expected_sha);

    // Build a realistic nested archive (`Volume/Zalo.app/Contents/{Info.plist,Resources/app.asar}`)
    // with `7z` and install it via `ZaloRuntimeManager::install` over `file://`!
    let payload_src = tmp.path().join("dmg-src/Zalo 26.10.20/Zalo.app/Contents");
    std::fs::create_dir_all(payload_src.join("Resources")).unwrap();

    let mut info_dict = plist::Dictionary::new();
    info_dict.insert(
        "CFBundleIdentifier".into(),
        plist::Value::String("com.vng.zalo".into()),
    );
    info_dict.insert(
        "CFBundleShortVersionString".into(),
        plist::Value::String("26.10.20".into()),
    );
    plist::Value::Dictionary(info_dict)
        .to_file_xml(payload_src.join("Info.plist"))
        .unwrap();

    build_synthetic_asar(
        &payload_src.join("Resources/app.asar"),
        &[("pc-dist/index.html", Some(b"<html>Zalo 26.10.20</html>"))],
    );

    let fake_dmg = tmp.path().join("ZaloSetup-universal-26.10.20.7z");
    let seven_zip = ["7zz", "7z"]
        .into_iter()
        .find(|bin| {
            Command::new(bin)
                .arg("i")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
        .expect("7zz or 7z must be installed");

    let pack_status = Command::new(seven_zip)
        .arg("a")
        .arg("-y")
        .arg(&fake_dmg)
        .arg(tmp.path().join("dmg-src/Zalo 26.10.20"))
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(pack_status.success());

    let dmg_sha = ZaloRuntimeManager::sha256_file(&fake_dmg).unwrap();
    let file_url = Url::from_file_path(&fake_dmg).unwrap().to_string();

    let rt_root = tmp.path().join("runtime");

    // 1. Wrong SHA-256 must fail before extraction
    let bad_mgr = ZaloRuntimeManager::new(&rt_root)
        .with_allows_latest(false)
        .with_release(ZaloRelease::new(
            "26.10.20",
            file_url.clone(),
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
        ));
    assert!(bad_mgr.install(|_| {}).await.is_err());
    assert!(!ZaloRuntimeManager::is_complete(&bad_mgr.current_dir()));

    // 2. Valid SHA-256 installs cleanly and emits all progress milestones
    let mgr = ZaloRuntimeManager::new(&rt_root)
        .with_allows_latest(false)
        .with_release(ZaloRelease::new("26.10.20", file_url.clone(), Some(dmg_sha.clone())));

    let events: Arc<Mutex<Vec<InstallProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let ev_clone = events.clone();
    let installed_ver = mgr
        .install(move |p| {
            ev_clone.lock().push(p);
        })
        .await
        .unwrap();

    assert_eq!(installed_ver, "26.10.20");
    assert!(ZaloRuntimeManager::is_complete(&mgr.current_dir()));
    assert_eq!(
        ZaloRuntimeManager::installed_version(&mgr.current_dir()).as_deref(),
        Some("26.10.20")
    );

    let recorded = events.lock().clone();
    assert!(recorded.contains(&InstallProgress::Looking));
    assert!(recorded.contains(&InstallProgress::Verifying));
    assert!(recorded.contains(&InstallProgress::Extracting));
    assert!(recorded.contains(&InstallProgress::Installing));
    assert_eq!(recorded.last(), Some(&InstallProgress::Complete));

    let opened_asar = mgr.open_asar().unwrap();
    assert_eq!(
        opened_asar.read("/pc-dist/index.html"),
        Some(&b"<html>Zalo 26.10.20</html>"[..])
    );

    // 3. Stage an upgrade (`26.11.0`) into `pending/` and verify `prepare_for_launch()` promotes it
    let mut upgrade_dict = BTreeMap::new();
    upgrade_dict.insert("CFBundleIdentifier", "com.vng.zalo");
    upgrade_dict.insert("CFBundleShortVersionString", "26.11.0");
    let mut up_plist = plist::Dictionary::new();
    for (k, v) in upgrade_dict {
        up_plist.insert(k.into(), plist::Value::String(v.into()));
    }
    plist::Value::Dictionary(up_plist)
        .to_file_xml(payload_src.join("Info.plist"))
        .unwrap();

    let fake_dmg_v2 = tmp.path().join("ZaloSetup-universal-26.11.0.7z");
    Command::new(seven_zip)
        .arg("a")
        .arg("-y")
        .arg(&fake_dmg_v2)
        .arg(tmp.path().join("dmg-src/Zalo 26.10.20"))
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    let dmg_v2_sha = ZaloRuntimeManager::sha256_file(&fake_dmg_v2).unwrap();
    let v2_url = Url::from_file_path(&fake_dmg_v2).unwrap().to_string();

    let up_mgr = ZaloRuntimeManager::new(&rt_root)
        .with_allows_latest(false)
        .with_release(ZaloRelease::new("26.11.0", v2_url, Some(dmg_v2_sha)));
    let staged_ver = up_mgr.stage_upgrade(|_| {}).await.unwrap();
    assert_eq!(staged_ver, "26.11.0");
    // `current/` is still 26.10.20 until `prepare_for_launch()` runs!
    assert_eq!(
        ZaloRuntimeManager::installed_version(&up_mgr.current_dir()).as_deref(),
        Some("26.10.20")
    );

    // Put leftover junk in `work/` to confirm `prepare_for_launch` cleans it up
    std::fs::create_dir_all(up_mgr.work_dir().join("leftover")).unwrap();
    up_mgr.prepare_for_launch();
    assert_eq!(
        ZaloRuntimeManager::installed_version(&up_mgr.current_dir()).as_deref(),
        Some("26.11.0")
    );
    assert!(!up_mgr.pending_dir().exists());
}

#[test]
fn zalo_linux_renderer_patches_rewrite_ui_controls_clipboard_and_calls_idempotently() {
    let tmp = TempDir::new().unwrap();
    let platform = ZaloPlatform::with_root(tmp.path(), "26.10.20");
    let spec_js = bridge_spec_script(&platform);
    let json_str = spec_js
        .strip_prefix("window.__zaloBridgeSpec = ")
        .and_then(|s| s.strip_suffix(';'))
        .unwrap();
    let spec: Value = serde_json::from_str(json_str).unwrap();
    assert_eq!(spec["values"]["$znode.os.__info"]["release"], json!("22.6.0"));
    assert_eq!(
        spec["members"]["$zelectronNative.clipboardImagePng"],
        json!("sync")
    );
    assert_eq!(spec["members"]["$znode.shell.openExternal"], json!("async"));

    let direct_scripts = olaz_core::direct_ui_user_scripts(&platform, false);
    assert_eq!(direct_scripts.len(), 7);
    assert_eq!(direct_scripts[4].0, "linux-direct-ui");
    assert_eq!(direct_scripts[5].0, "bridge");
    assert_eq!(olaz_core::popup_user_scripts(&platform).len(), 6);

    // Verify `grant_writable_path` unlocks user-approved Save-As targets outside `~/Downloads`
    let outside_save = tmp.path().join("home/Documents/invoice.pdf");
    assert!(!platform.is_writable(&outside_save));
    platform.grant_writable_path(&outside_save);
    assert!(platform.is_writable(&outside_save));

    // 1. Login titlebar `platform:"WIN32"` while keeping `getClientType(){return 23}`
    let login_src = br#"var x={handlerMinimiseAppWindowOS:()=>{$zwindow.minimize(re.c)},platform:"DARWIN",getClientType(){return 23}};"#;
    let login_patched = patch_asar_bundle("/pc-dist/lazy/login-startup.abc.js", login_src).unwrap();
    let login_str = std::str::from_utf8(&login_patched).unwrap();
    assert!(login_str.contains(r#"platform:"WIN32""#));
    assert!(login_str.contains("getClientType(){return 23}"));
    assert!(patch_zalo_linux_renderer_bundle(&login_patched).is_none());

    // 2. Main `#titleBar`, `MediaViewer`, `Ctrl+V` clipboard paste, call gate, enrich, and call-log
    let ui_src = br#"
        i.a.createElement("div",{id:"titleBar",className:(this.state.locked?" locked ":"")+"DARWIN"},
          i.a.createElement("div",{className:`title-name  +  macos ${o}`}),
          i.a.createElement("div",{className:"titlebar__btns clickable",style:t}),
          i.a.createElement(x.c,{className:o}),
          a?i.a.createElement("span",{id:"STR_NEW_VER"})))):null));
        var mv = i.a.createElement("div",{className:this.props.className||" titlebar rel flx"},null,e,i.a.createElement("div",{className:"draggable w100 title-drag",style:{position:"absolute",left:0},onDoubleClick:this.maximize.bind(this)}),null));
        var mvTitle = "titlebar__title  mac";
        var W=(t,n="")=>{G||(G=!0,$zscreencap.getClipboard(e.currentUserId));};
        var ae = e => !(Number(e.split(".")[0])<17);
        var sup = { isSupport(){return!!u.default.enable_mac_call&&(u.default.enableCall&&se)} };
        function ctrl(e){ const t={type:"control",data:e}; const p=T.default.getMiniInfo("1"); return t; }
        function logBubble(n,t){ if(cfg.call.sync_call_message.ui)return; const e={caller:n.role,duration:1e3*n.duration,userId:n.partnerId}; return { action: r?"recommened.calling":"recommened.receivecall", params:'{"isEnableCallback":1}', e }; }
        class UrlUtil { static transformLocalUrl(e){return UrlUtil.checkNeedTransformLocalUrl(e)?"zfile://"+e:e} }
        function an(e){ return Boolean(!1)?"zfile://"+e:e; }
        function resizeJxl(){ Object(F.jxlNativeSupported)()?(d=this.resizerFactory.createResizer(c.e.LIBJXL),u=this.resizerFactory.createResizer(c.e.LIBJXL_WASM)):d=this.resizerFactory.createResizer(c.e.LIBJXL_WASM); }
        function decJxl(){ Object(fe.jxlNativeSupported)()?t._decoder||(fe.ImageLoaderConfig.nestedKey("offload_config.enable_offload_decode_jxl")?t._decoder=new ve:t._decoder=new ue,t._lazyDecoderFallbackInit=!1):t._decoder=new Me; }
        function encJxl(){ Object(fe.jxlNativeSupported)()?(t._encoder||(fe.ImageLoaderConfig.nestedKey("offload_config.enable_offload_encode_jxl")?t._encoder=new St:t._encoder=new Tt),t._lazyDecoderFallbackInit=!1):t._encoder||(t._encoder=new yt); }
        async function handleDecompdeJpegMultiFallback(e,t){ const l=await Object(Be.b)(t,{sizes:r,priority:0,outputFormat:"image/jpeg",quality:e.options.quality,timeout:1e4*r.length}); return l; }
        function pickResizer(){ d=!J.default.image_resizer.enable_libvips_macos||"image/jpeg"!==o&&"image/png"!==o?this.compatibilityValidation.isOffscreenCanvasSupported()?this.resizerFactory.createResizer(c.e.WEB_WORKER):this.compatibilityValidation.isAutoRotationSupported()?this.resizerFactory.createResizer(c.e.MAIN):this.resizerFactory.createResizer(c.e.PRIMITIVE):this.resizerFactory.createResizer(c.e.LIBVIPS); }
    "#;
    let ui_patched = patch_asar_bundle(
        "/pc-dist/lazy/default-login-main-startup-shared-worker-znotification.abc.js",
        ui_src,
    )
    .expect("UI bundle should be rewritten");
    let ui_str = std::str::from_utf8(&ui_patched).unwrap();
    assert!(ui_str.contains("fa-Lock_24_Line btn titlebar__menu__btn ${o}"));
    assert!(ui_str.contains(r#"?" locked ":"")+"WIN32""#));
    assert!(ui_str.contains("`title-name + ${o}`"));
    assert!(ui_str.contains("titlebar__menu__btnPreviewPhoto"));
    assert!(ui_str.contains(r#"className:"titlebar__resize""#));
    assert!(ui_str.contains(r#""titlebar__title ""#));
    assert!(ui_str.contains("/*__znative_clip_paste__*/"));
    assert!(ui_str.contains("!1/*call-linux*/"));
    assert!(ui_str.contains("/*mac-forced*/return!!(u.default.enableCall&&se)"));
    assert!(ui_str.contains("__zcallEnrich=1") && ui_str.contains("T.default.getMiniInfo(_u)"));
    assert!(ui_str.contains("sync_call_message.ui&&0)return;"));
    assert!(ui_str.contains(r#""recommened.misscall":t.caller?"#));
    assert!(ui_str.contains("isCaller:t.caller?1:0"));
    assert!(ui_str.contains(r#""zfile://local/"+s.replace(/^file:\/+/,"")"#));
    assert_eq!(ui_str.matches("(!1/*olaz-wasm-jxl*/)").count(), 3);
    assert!(ui_str.contains("(!0/*olaz-main-resizer*/?this.resizerFactory.createResizer(c.e.MAIN):null)"));
    // Second pass is strictly idempotent (`None`)
    assert!(patch_zalo_linux_renderer_bundle(&ui_patched).is_none());
}

#[test]
fn fs_bridge_upload_slice_checksum_svc_read_and_file_noise_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let platform = Arc::new(ZaloPlatform::with_root(tmp.path(), "26.10.20"));
    let fs_bridge = FsBridge::new(Arc::clone(&platform));

    // Create a 256-byte binary payload outside ZaloData (simulating `/mnt/data/.../subscriptions-proof.png`).
    let external_dir = tmp.path().join("mnt-data/kv-tube");
    std::fs::create_dir_all(&external_dir).unwrap();
    let external_img = external_dir.join("subscriptions-proof.png");
    let mut payload = Vec::with_capacity(256);
    for i in 0u16..256 {
        payload.push(((i * 37 + 13) & 0xff) as u8);
    }
    std::fs::write(&external_img, &payload).unwrap();
    let img_path_str = external_img.to_string_lossy().to_string();

    // 1. `svcStatAsync` & `svcReadPath` (full + inclusive `[start..=end]`) on read-only external path
    let stat = fs_bridge
        .dispatch_sync("$zFileManager.svcStatAsync", &[json!(img_path_str)])
        .unwrap();
    assert_eq!(stat["size"].as_u64(), Some(256));

    let full_ab = fs_bridge
        .dispatch_sync("$zFileManager.svcReadPath", &[json!(img_path_str), json!({})])
        .unwrap();
    assert_eq!(wire::decode_bytes(&full_ab).unwrap(), payload);

    let ranged_ab = fs_bridge
        .dispatch_sync(
            "$zFileManager.svcReadPath",
            &[json!(img_path_str), json!({ "start": 10, "end": 49 })],
        )
        .unwrap();
    assert_eq!(wire::decode_bytes(&ranged_ab).unwrap(), &payload[10..=49]);

    // 2. `readFileSlice` (offset + length + clamped EOF) & `readLogRange`
    let slice_ab = fs_bridge
        .dispatch_sync(
            "$zFileManager.readFileSlice",
            &[json!(img_path_str), json!(200), json!(100), json!(false)],
        )
        .unwrap();
    assert_eq!(wire::decode_bytes(&slice_ab).unwrap(), &payload[200..256]);
    let log_range = fs_bridge
        .dispatch_sync(
            "$zFileManager.readLogRange",
            &[json!("calf.bin"), json!(0), json!(32)],
        )
        .unwrap();
    assert_eq!(wire::decode_bytes(&log_range).unwrap(), vec![0u8; 32]);

    // 3. `getMd5LargeFileObject` & `testChecksum` parity against CryptoBridge MD5
    let cb = CryptoBridge::new();
    let h_md5 = cb.hash_create("md5").unwrap();
    cb.hash_update(h_md5, &payload).unwrap();
    let expected_md5 = hex::encode(cb.hash_digest(h_md5).unwrap());
    let actual_md5 = fs_bridge
        .dispatch_sync("$zFileManager.getMd5LargeFileObject", &[json!(img_path_str)])
        .unwrap();
    assert_eq!(actual_md5.as_str(), Some(expected_md5.as_str()));

    let ok_chk = fs_bridge
        .dispatch_sync(
            "$zFileManager.testChecksum",
            &[json!(img_path_str), json!(expected_md5.to_uppercase())],
        )
        .unwrap();
    assert_eq!(ok_chk, json!(true));

    // 4. `zcloudNoiseFile` -> `svcStatAsync` / `svcReadPath` / `zfileGetChecksum` -> `zcloudDenoiseFile` -> `zcloudCloneAndMatchChecksum`
    let noised_path = platform
        .user_data()
        .join("ZaloDownloads/12345/fileNoise")
        .join(&expected_md5);
    let noised_str = noised_path.to_string_lossy().to_string();
    fs_bridge
        .dispatch_sync(
            "$zFileManager.zcloudNoiseFile",
            &[json!(img_path_str), json!(noised_str), json!(expected_md5)],
        )
        .unwrap();
    // Raw on-disk file has 6-byte `077a416c4f07` trailer (`256 + 6 = 262`)
    assert_eq!(std::fs::metadata(&noised_path).unwrap().len(), 262);

    // Logical stat & read automatically strip the 6-byte trailer and un-XOR the first 100 bytes
    let noise_stat = fs_bridge
        .dispatch_sync("$zFileManager.svcStatAsync", &[json!(noised_str)])
        .unwrap();
    assert_eq!(noise_stat["size"].as_u64(), Some(256));

    let noise_slice = fs_bridge
        .dispatch_sync(
            "$zFileManager.svcReadPath",
            &[json!(noised_str), json!({ "start": 80, "end": 120 })],
        )
        .unwrap();
    assert_eq!(wire::decode_bytes(&noise_slice).unwrap(), &payload[80..=120]);

    let noise_md5 = fs_bridge
        .dispatch_sync("$zFileManager.zfileGetChecksum", &[json!(noised_str)])
        .unwrap();
    assert_eq!(noise_md5.as_str(), Some(expected_md5.as_str()));

    let denoised_path = platform.downloads_dir().join("restored-proof.png");
    let denoised_str = denoised_path.to_string_lossy().to_string();
    fs_bridge
        .dispatch_sync(
            "$zFileManager.zcloudDenoiseFile",
            &[json!(noised_str), json!(denoised_str), json!(expected_md5)],
        )
        .unwrap();
    assert_eq!(std::fs::read(&denoised_path).unwrap(), payload);

    let cloned = fs_bridge
        .dispatch_sync(
            "$zFileManager.zcloudCloneAndMatchChecksum",
            &[json!(expected_md5), json!(img_path_str)],
        )
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    assert!(!cloned.is_empty());
    assert_eq!(std::fs::read(&cloned).unwrap(), payload);
}

#[test]
fn live_installed_asar_patches_all_zalo_linux_targets_and_passes_node_syntax() {
    let platform = ZaloPlatform::new("0.1.0");
    let mgr = ZaloRuntimeManager::from_platform(&platform);
    let Ok(asar) = mgr.open_asar() else {
        return;
    };

    let check_syntax = |label: &str, js_bytes: &[u8]| {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), js_bytes).unwrap();
        let status = Command::new("node")
            .arg("--check")
            .arg(tmp.path())
            .status()
            .expect("node --check must execute");
        assert!(status.success(), "JS syntax check failed for {label}");
    };

    for (name, src) in olaz_core::direct_ui_user_scripts(&platform, false) {
        check_syntax(&format!("direct_ui:{name}"), src.as_bytes());
    }
    for (name, src) in olaz_core::popup_user_scripts(&platform) {
        check_syntax(&format!("popup:{name}"), src.as_bytes());
    }

    let mut hit_login_win32 = false;
    let mut hit_main_titlebar = false;
    let mut hit_media_viewer = false;
    let mut hit_clipboard_paste = false;
    let mut hit_call_gate = false;
    let mut hit_call_enrich = false;
    let mut hit_call_log = false;
    let mut hit_zfile_url = false;
    let mut hit_wasm_jxl = 0usize;
    let mut hit_jxl_lazy_fallback = false;
    let mut hit_jxl_noresize_fallback = false;
    let mut hit_main_resizer = false;
    let mut hit_main_resizer_path = false;
    let mut hit_wasm_jxl_resizer_path = false;
    let mut hit_lazy_main_resizer_path = false;
    let mut hit_lazy_wasm_jxl_resizer_path = false;
    let mut hit_zimg_complete = false;
    let mut hit_io_obs = false;
    let mut hit_io_win = false;
    let mut hit_zimg_eager = false;
    let mut hit_autostart_ui = false;
    let mut hit_autostart_lang_en = false;
    let mut hit_autostart_lang_vi = false;
    let mut hit_video_thumb_flag = false;
    let mut hit_video_thumb_gate = false;
    let mut raw_had_ntfs_fs_check = false;
    let mut hit_drive_migration_fs = false;
    let mut hit_html_csp = 0usize;
    let mut preserved_client_type_23 = false;
    let mut patched_files = 0usize;

    for html_name in ["pc-dist/index.html", "pc-dist/popup-viewer.html"] {
        let raw_html = asar
            .read_or_unpacked(html_name)
            .unwrap_or_else(|| panic!("missing {html_name} in live app.asar"));
        let patched_html = patch_asar_bundle(html_name, &raw_html)
            .unwrap_or_else(|| panic!("expected CSP patch on {html_name}"));
        let html_str = std::str::from_utf8(&patched_html).unwrap();
        assert!(
            !html_str.contains("upgrade-insecure-requests")
                && html_str.contains("http://127.0.0.1:*")
                && html_str.contains("zfile:")
                && html_str.contains("data:"),
            "expected CSP loopback/data:/zfile: allowance in {html_name}: {html_str}"
        );
        assert!(
            patch_asar_bundle(html_name, &patched_html).is_none(),
            "HTML CSP patch must be idempotent on {html_name}"
        );
        hit_html_csp += 1;
    }
    assert_eq!(hit_html_csp, 2);

    for path in asar.paths_with_prefix("pc-dist/") {
        if !path.ends_with(".js") {
            continue;
        }
        let Some(raw) = asar.read_or_unpacked(path) else {
            continue;
        };
        if let Ok(raw_s) = std::str::from_utf8(&raw) {
            if raw_s.contains("getClientType(){return 23}") {
                preserved_client_type_23 = true;
            }
            if raw_s.contains("detectFilesystemSync(") && raw_s.contains(r#""ntfs"==="#) {
                raw_had_ntfs_fs_check = true;
            }
        }
        let Some(patched) = patch_asar_bundle(path, &raw) else {
            continue;
        };
        patched_files += 1;
        check_syntax(path, &patched);

        let s = std::str::from_utf8(&patched).expect("patched JS must be valid UTF-8");
        if s.contains("handlerMinimiseAppWindowOS:") && s.contains(r#"platform:"WIN32""#) {
            hit_login_win32 = true;
        }
        if s.contains("fa-Lock_24_Line btn titlebar__menu__btn") {
            hit_main_titlebar = true;
        }
        if s.contains("titlebar__menu__btnPreviewPhoto") {
            hit_media_viewer = true;
        }
        if s.contains("/*__znative_clip_paste__*/") {
            hit_clipboard_paste = true;
        }
        if s.contains("!1/*call-linux*/") {
            hit_call_gate = true;
        }
        if s.contains("__zcallEnrich=1") {
            hit_call_enrich = true;
        }
        if s.contains("recommened.misscall") {
            hit_call_log = true;
        }
        if s.contains("zfile://local/") {
            hit_zfile_url = true;
        }
        hit_wasm_jxl += s.matches("(!1/*olaz-wasm-jxl*/)").count();
        if s.contains("._lazyDecoderFallbackInit=!0") {
            hit_jxl_lazy_fallback = true;
        }
        if s.contains("/*olaz-jxl-noresize*/") {
            hit_jxl_noresize_fallback = true;
        }
        if s.contains("(!0/*olaz-main-resizer*/") {
            hit_main_resizer = true;
        }
        if s.contains("/*olaz-main-path*/") {
            hit_main_resizer_path = true;
            if path.contains("lazy/default-login-main-startup-shared-worker-znotification.") {
                hit_lazy_main_resizer_path = true;
            }
        }
        if s.contains("/*olaz-wasm-jxl-path*/") {
            hit_wasm_jxl_resizer_path = true;
            if path.contains("lazy/default-login-main-startup-shared-worker-znotification.") {
                hit_lazy_wasm_jxl_resizer_path = true;
            }
        }
        if s.contains("/*olaz-zimg-complete*/") {
            hit_zimg_complete = true;
        }
        if s.contains("/*olaz-io-obs*/") {
            hit_io_obs = true;
        }
        if s.contains("/*olaz-io-win*/") {
            hit_io_win = true;
        }
        if s.contains(r#""data-z-element-type":"image","data-zdrag-region":"zdraggable-region",decoding:"sync",loading:"eager""#) {
            hit_zimg_eager = true;
        }
        if s.contains("/*olaz-autostart-ui*/") && s.contains("/*olaz-autostart-fx*/") {
            hit_autostart_ui = true;
        }
        if s.contains(r#""STR_OLAZ_START_IN_TRAY":"Launch minimized to system tray""#) {
            hit_autostart_lang_en = true;
        }
        if s.contains(r#""STR_OLAZ_START_IN_TRAY":"Khởi động thu nhỏ dưới khay hệ thống""#) {
            hit_autostart_lang_vi = true;
        }
        if s.contains("gen_video_thumb:{enable:!0") {
            hit_video_thumb_flag = true;
        }
        if s.contains(r#"!0)throw{error_code:"NOT_SUPPORT""#)
            || (s.contains("genVideoThumbFromFile(") && s.contains("if(!!0){const ") && s.contains(".NOT_SUPPORT"))
        {
            hit_video_thumb_gate = true;
        }
        if s.contains(r#"["ntfs","ext4","btrfs","xfs","f2fs","zfs"].includes("#) {
            hit_drive_migration_fs = true;
        }
        if !path.contains("dal-worker.") && !path.contains("sync-v2-sub-worker.") {
            assert!(
                patch_zalo_linux_renderer_bundle(&patched).is_none(),
                "patch_zalo_linux_renderer_bundle must be idempotent on {path}"
            );
        }
        if raw
            .windows("getClientType(){return 23}".len())
            .any(|w| w == b"getClientType(){return 23}")
        {
            assert!(
                s.contains("getClientType(){return 23}"),
                "clientType=23 must remain intact in {path}"
            );
        }
    }

    println!("Verified {patched_files} patched JS bundles against live installed app.asar");
    assert!(preserved_client_type_23, "expected getClientType(){{return 23}} in live app.asar");
    assert!(hit_login_win32, "Login WIN32 titlebar patch did not match live app.asar");
    assert!(hit_main_titlebar, "Main #titleBar WIN32 patch did not match live app.asar");
    assert!(hit_media_viewer, "MediaViewer WIN32 patch did not match live app.asar");
    assert!(hit_clipboard_paste, "Ctrl+V clipboard PNG paste patch did not match live app.asar");
    assert!(hit_call_gate, "Linux call gate patch did not match live app.asar");
    assert!(hit_call_enrich, "__zcallEnrich caller info patch did not match live app.asar");
    assert!(hit_call_log, "Call outcome log bubble patch did not match live app.asar");
    assert!(hit_zfile_url, "zfile://local/ transformLocalUrl patch did not match live app.asar");
    assert!(hit_wasm_jxl >= 3, "Expected JXL codec sites (resize/decode/encode) to route to WASM in live app.asar (got {hit_wasm_jxl})");
    assert!(hit_jxl_lazy_fallback, "Expected JXL _lazyDecoderFallbackInit=!0 patch to match live app.asar");
    assert!(hit_jxl_noresize_fallback, "Expected JXL Original no-resize fallback patch to match live app.asar");
    assert!(hit_main_resizer, "Expected ee.resizeImage JPEG/PNG MAIN canvas patch to match live app.asar");
    assert!(hit_main_resizer_path, "Expected 3Rxw MAIN canvas local-path/destinationPath patch to match live app.asar");
    assert!(hit_wasm_jxl_resizer_path, "Expected 3Rxw LIBJXL_WASM local-path/destinationPath patch to match live app.asar");
    assert!(hit_lazy_main_resizer_path, "Expected 3Rxw MAIN canvas local-path patch to match index.html lazy_shared bundle");
    assert!(hit_lazy_wasm_jxl_resizer_path, "Expected 3Rxw LIBJXL_WASM local-path patch to match index.html lazy_shared bundle");
    assert!(hit_zimg_complete, "Expected Ze componentDidMount complete-image check patch to match live app.asar");
    assert!(hit_io_obs, "Expected 0tHf IntersectionObserver popup observe patch to match live app.asar");
    assert!(hit_io_win, "Expected 0tHf IntersectionObserver getObserver closed-window refresh patch to match live app.asar");
    assert!(hit_zimg_eager, "Expected Z.d.Image loading:\"eager\" patch to match live app.asar");
    assert!(hit_autostart_ui, "Expected General Settings autostart + start-in-tray UI patch to match live app.asar");
    assert!(hit_autostart_lang_en, "Expected STR_OLAZ_START_IN_TRAY English translation in live app.asar");
    assert!(hit_autostart_lang_vi, "Expected STR_OLAZ_START_IN_TRAY Vietnamese translation in live app.asar");
    assert!(hit_video_thumb_flag, "Expected gen_video_thumb:{{enable:!0 patch to match live app.asar");
    assert!(hit_video_thumb_gate, "Expected genVideoThumbFromFile NOT_SUPPORT gate patch to match live app.asar");
    if raw_had_ntfs_fs_check {
        assert!(hit_drive_migration_fs, "Expected DriveMigrationService Linux filesystem patch to match live app.asar");
    }
}

#[test]
fn transform_local_url_unwraps_whatwg_url_objects_and_platform_permits_cache_and_zalo_received() {
    let tmp = TempDir::new().unwrap();
    let platform = ZaloPlatform::with_root(tmp.path(), "26.10.20");
    let cache_sticker = platform.cache_dir().join("stickers/12345/ami.png");
    let received_doc = platform
        .electron_path("documents")
        .join("Zalo Received Files/photo_original.jpg");
    assert!(platform.is_writable(&cache_sticker));
    assert!(platform.is_writable(&received_doc));

    let synthetic = br#"class R{static checkNeedTransformLocalUrl(e){return!0}static transformLocalUrl(e){return R.checkNeedTransformLocalUrl(e)?"zfile://"+e:e}}"#;
    let patched = patch_asar_bundle("pc-dist/compact-app-pc.1234.js", synthetic)
        .expect("transformLocalUrl patch must match");
    let patched_js = std::str::from_utf8(&patched).unwrap();

    let script = format!(
        "{patched_js}\n\
         const uObj = new URL('file:///home/khoavo/.local/share/olaz/ZaloData/sticker/ami%201.png');\n\
         const out = [\n\
           R.transformLocalUrl(uObj),\n\
           R.transformLocalUrl('file:///tmp/photo #1?.jpg'),\n\
           R.transformLocalUrl('//stc-zalo.zadn.vn/sticker/ami.png'),\n\
         ];\n\
         console.log(JSON.stringify(out));"
    );
    let output = Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .expect("node must execute");
    assert!(output.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        parsed,
        json!([
            "zfile://local/home/khoavo/.local/share/olaz/ZaloData/sticker/ami%201.png",
            "zfile://local/tmp/photo%20%231%3F.jpg",
            "https://stc-zalo.zadn.vn/sticker/ami.png"
        ])
    );
}

#[test]
fn general_settings_autostart_and_start_in_tray_toggles_execute_cleanly_in_node() {
    let tmp = TempDir::new().unwrap();
    let platform = ZaloPlatform::with_root(tmp.path(), "26.10.20");
    assert!(platform.is_isolated_root());
    assert_eq!(platform.autostart_dir(), tmp.path().join("config/autostart"));

    let spec = bridge_spec_script(&platform);
    assert!(spec.contains(r#""$zconfig.getAutostartState":"async""#));
    assert!(spec.contains(r#""$zconfig.setStartInTray":"async""#));
    assert!(spec.contains(r#""$zconfig.migrateLegacyAutoLaunch":"async""#));

    let synthetic = br#"function GeneralSettings(){const[d,c]=Object(i.useState)(o.g.getLaunch()),[u,f]=Object(i.useState)(p.default.currentLanguage());Object(i.useEffect)((()=>{$zconfig.checkAutoLaunchEnable().then((e=>{o.g.setLaunch(e),c(e)})).catch((()=>{}))}),[]);return [l.a.createElement(Za.Toggle,{key:"isAutoLauch",stateValue:d,onToggle:e=>{$zconfig.toggleAutoLaunch(e),o.g.setLaunch(e),c(e),v.default.logAction(e?248010301:248010302)},titleTextKey:"STR_START_ZALO_WITH_OS",className:"first general no-border-bottom"})]}"#;
    let patched = patch_zalo_linux_renderer_bundle(synthetic)
        .expect("GeneralSettings autostart patch must match");
    assert!(patch_zalo_linux_renderer_bundle(&patched).is_none());
    let patched_js = std::str::from_utf8(&patched).unwrap();

    let script = format!(
        r#"
        let states = [];
        let cursor = 0;
        let effectFns = [];
        const i = {{
          useState(init) {{
            const idx = cursor++;
            if (!(idx in states)) states[idx] = init;
            return [states[idx], (v) => {{ states[idx] = v; }}];
          }},
          useEffect(fn) {{ effectFns.push(fn); }},
        }};
        let launchStored = false;
        const o = {{
          g: {{
            getLaunch: () => launchStored,
            setLaunch: (v) => {{ launchStored = v; }},
            getRemember: () => true,
            isShowSettingRemember: () => true,
          }},
        }};
        const p = {{ default: {{ currentLanguage: () => 'vi' }} }};
        const l = {{ a: {{ createElement: (comp, props) => ({{ comp, props }}) }} }};
        const Za = {{ Toggle: 'Toggle' }};
        const v = {{ default: {{ logAction: () => {{}} }} }};

        let hostState = {{ openAtLogin: false, startInTray: false }};
        const calls = [];
        globalThis.$zconfig = {{
          getAutostartState: async () => ({{ ...hostState }}),
          checkAutoLaunchEnable: async () => hostState.openAtLogin,
          toggleAutoLaunch: async (enabled) => {{
            calls.push(['toggleAutoLaunch', enabled]);
            if (enabled && !hostState.openAtLogin) hostState.startInTray = true;
            hostState.openAtLogin = enabled;
            return {{ ...hostState }};
          }},
          setStartInTray: async (inTray) => {{
            calls.push(['setStartInTray', inTray]);
            hostState.startInTray = inTray;
            return {{ ...hostState }};
          }},
        }};

        function render() {{
          cursor = 0;
          effectFns = [];
          return GeneralSettings();
        }}

        {patched_js}

        (async () => {{
          let rows = render();
          for (const fx of effectFns) fx();
          await new Promise(r => setTimeout(r, 10));
          rows = render();
          const initial = rows.map(x => ({{ key: x.props.key, val: x.props.stateValue, disabled: !!x.props.disabled, cls: x.props.className }}));

          // Turn ON "Start with system" -> "Launch minimized to system tray" must default ON!
          rows[0].props.onToggle(true);
          await new Promise(r => setTimeout(r, 10));
          rows = render();
          const afterEnable = rows.map(x => ({{ key: x.props.key, val: x.props.stateValue, disabled: !!x.props.disabled }}));

          // User turns OFF "Launch minimized to system tray" while keeping autostart ON
          rows[1].props.onToggle(false);
          await new Promise(r => setTimeout(r, 10));
          rows = render();
          const afterTrayOff = rows.map(x => ({{ key: x.props.key, val: x.props.stateValue, disabled: !!x.props.disabled }}));

          // Turn OFF autostart and turn ON again -> tray toggle defaults back to ON!
          rows[0].props.onToggle(false);
          await new Promise(r => setTimeout(r, 10));
          rows = render();
          rows[0].props.onToggle(true);
          await new Promise(r => setTimeout(r, 10));
          rows = render();
          const afterReEnable = rows.map(x => ({{ key: x.props.key, val: x.props.stateValue, disabled: !!x.props.disabled }}));

          console.log(JSON.stringify({{ initial, afterEnable, afterTrayOff, afterReEnable, calls }}));
        }})();
        "#
    );

    let result = run_node_json(&script);
    assert_eq!(
        result["initial"],
        json!([
            { "key": "isAutoLauch", "val": false, "disabled": false, "cls": "first general no-border-bottom" },
            { "key": "isStartInTray", "val": false, "disabled": true, "cls": "general no-border-bottom" },
        ])
    );
    assert_eq!(
        result["afterEnable"],
        json!([
            { "key": "isAutoLauch", "val": true, "disabled": false },
            { "key": "isStartInTray", "val": true, "disabled": false },
        ])
    );
    assert_eq!(
        result["afterTrayOff"],
        json!([
            { "key": "isAutoLauch", "val": true, "disabled": false },
            { "key": "isStartInTray", "val": false, "disabled": false },
        ])
    );
    assert_eq!(
        result["afterReEnable"],
        json!([
            { "key": "isAutoLauch", "val": true, "disabled": false },
            { "key": "isStartInTray", "val": true, "disabled": false },
        ])
    );
    assert_eq!(
        result["calls"],
        json!([
            ["toggleAutoLaunch", true],
            ["setStartInTray", false],
            ["toggleAutoLaunch", false],
            ["toggleAutoLaunch", true],
        ])
    );
}

#[tokio::test]
async fn html_csp_patch_direct_ui_unused_filename_mp4thumb_media_stream_rewrite_and_video_thumb() {
    // 1. HTML CSP patch strips `upgrade-insecure-requests;` and allows `http://127.0.0.1:*` + `zfile:`.
    let raw_html = br#"<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="upgrade-insecure-requests; default-src 'none'; img-src * data: blob: zfile: file:; media-src * blob: zfile: file:; connect-src 'self' https://*.zalo.me blob:"></head><body></body></html>"#;
    let patched_html = patch_asar_bundle("pc-dist/index.html", raw_html)
        .expect("HTML CSP meta tag must be rewritten");
    let html_str = std::str::from_utf8(&patched_html).unwrap();
    assert!(!html_str.contains("upgrade-insecure-requests"));
    assert!(html_str.contains("media-src * blob: zfile: file: 'self' zalo-app: http://127.0.0.1:*"));
    assert!(html_str.contains("img-src * data: blob: zfile: file: 'self' zalo-app: http://127.0.0.1:*"));
    assert!(html_str.contains("connect-src 'self' https://*.zalo.me blob: zalo-app: http://127.0.0.1:* zfile: data:"));
    assert!(
        patch_asar_bundle("pc-dist/popup-viewer.html", &patched_html).is_none(),
        "already-patched CSP must be idempotent"
    );

    // 2. Plain un-noised files MUST NOT be corrupted by `zcloudDenoiseFile`, and `zcloudNoiseFile` is idempotent.
    let tmp = TempDir::new().unwrap();
    let platform = Arc::new(ZaloPlatform::with_root(tmp.path(), "26.10.20"));
    let fsb = FsBridge::new(Arc::clone(&platform));
    let plain_src = platform.user_data().join("plain_sample.bin");
    let plain_out = platform.user_data().join("plain_denoised.bin");
    let noised_once = platform.user_data().join("noised_1.bin");
    let noised_twice = platform.user_data().join("noised_2.bin");
    let denoised_back = platform.user_data().join("denoised_back.bin");
    let original_bytes: Vec<u8> = (0..256u16).map(|i| (i & 0xff) as u8).collect();
    std::fs::write(&plain_src, &original_bytes).unwrap();

    fsb.dispatch_sync(
        "$zFileManager.zcloudDenoiseFile",
        &[
            json!(plain_src.to_string_lossy()),
            json!(plain_out.to_string_lossy()),
            json!(""),
        ],
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&plain_out).unwrap(),
        original_bytes,
        "zcloudDenoiseFile must copy plain un-noised files verbatim without XOR corruption"
    );

    fsb.dispatch_sync(
        "$zFileManager.zcloudNoiseFile",
        &[
            json!(plain_src.to_string_lossy()),
            json!(noised_once.to_string_lossy()),
            json!(""),
        ],
    )
    .unwrap();
    fsb.dispatch_sync(
        "$zFileManager.zcloudNoiseFile",
        &[
            json!(noised_once.to_string_lossy()),
            json!(noised_twice.to_string_lossy()),
            json!(""),
        ],
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&noised_once).unwrap(),
        std::fs::read(&noised_twice).unwrap(),
        "zcloudNoiseFile must be idempotent on already-noised files"
    );
    fsb.dispatch_sync(
        "$zFileManager.zcloudDenoiseFile",
        &[
            json!(noised_twice.to_string_lossy()),
            json!(denoised_back.to_string_lossy()),
            json!(""),
        ],
    )
    .unwrap();
    assert_eq!(std::fs::read(&denoised_back).unwrap(), original_bytes);

    // 3. Execute `LINUX_DIRECT_UI_SCRIPT` + `bridge.js` in Node to verify `$znode.unusedFilename`,
    //    `$zFeatures.genThumb.writeSavePathFilePhoto`, `$znode.nativelibs.mp4thumb`, `<video>` proxy URL
    //    rewriting, `<video poster>` rewriting, and `window.fetch('file:///...')` preservation!
    let scripts = olaz_core::popup_user_scripts(&platform);
    let spec_js = &scripts.iter().find(|(n, _)| n == "bridge-spec").unwrap().1;
    let local_js = &scripts.iter().find(|(n, _)| n == "local-modules").unwrap().1;
    let direct_js = &scripts.iter().find(|(n, _)| n == "linux-direct-ui").unwrap().1;
    let bridge_js = &scripts.iter().find(|(n, _)| n == "bridge").unwrap().1;

    let node_harness = format!(
        r#"
        const existing = new Set(['/dl/photo.jpg', '/dl/photo (1).jpg']);
        const ipcCalls = [];
        const asyncCalls = [];
        class Element {{
          constructor(tag) {{ this.tagName = tag; this._attrs = {{}}; }}
          hasAttribute(k) {{ return Object.prototype.hasOwnProperty.call(this._attrs, k.toLowerCase()); }}
          getAttribute(k) {{ return this._attrs[k.toLowerCase()] ?? null; }}
          setAttribute(k, v) {{ this._attrs[k.toLowerCase()] = String(v); }}
        }}
        class HTMLImageElement extends Element {{
          constructor() {{ super('IMG'); this._src = ''; }}
          get src() {{ return this._src; }}
          set src(v) {{ this._src = String(v); }}
        }}
        class HTMLMediaElement extends Element {{
          constructor(tag) {{ super(tag || 'VIDEO'); this._src = ''; }}
          get src() {{ return this._src; }}
          set src(v) {{ this._src = String(v); }}
        }}
        class HTMLVideoElement extends HTMLMediaElement {{
          constructor() {{ super('VIDEO'); this._poster = ''; }}
          get poster() {{ return this._poster; }}
          set poster(v) {{ this._poster = String(v); }}
        }}
        class HTMLSourceElement extends Element {{
          constructor() {{ super('SOURCE'); this._src = ''; }}
          get src() {{ return this._src; }}
          set src(v) {{ this._src = String(v); }}
        }}
        globalThis.Element = Element;
        globalThis.HTMLImageElement = HTMLImageElement;
        globalThis.HTMLMediaElement = HTMLMediaElement;
        globalThis.HTMLVideoElement = HTMLVideoElement;
        globalThis.HTMLSourceElement = HTMLSourceElement;
        globalThis.window = globalThis;
        globalThis.location = {{ protocol: 'zalo-app:', href: 'zalo-app://local/login.html', pathname: '/login.html' }};
        globalThis.document = {{ readyState: 'complete', addEventListener() {{}} }};
        globalThis.addEventListener = () => {{}};
        globalThis.__olazMediaProxyBase = 'http://127.0.0.1:43210/secret-tok';
        globalThis.fetch = async (u) => ({{ url: String(u) }});
        globalThis.__zaloNativeXHR = class {{
          open(_m, _u) {{}}
          send(body) {{
            const req = JSON.parse(body);
            if (req.path === '$znode.fs.existsSync') {{
              this.responseText = JSON.stringify({{ value: existing.has(req.args[0]) }});
            }} else {{
              this.responseText = JSON.stringify({{ value: null }});
            }}
          }}
        }};
        globalThis.webkit = {{
          messageHandlers: {{
            zlog: {{ postMessage() {{}} }},
            zstate: {{ postMessage() {{}} }},
            __olaz_win_drag: {{ postMessage() {{}} }},
            zbridge: {{
              postMessage: async (msg) => {{
                if (msg.kind === 'invoke') {{
                  ipcCalls.push(msg);
                  return '/saved/thumb_123.jpg';
                }}
                if (msg.kind === 'call') {{
                  asyncCalls.push(msg);
                  if (msg.path === '$znode.fs.exists') return existing.has(msg.args[0]);
                  if (msg.path === '$zFileManager.generateVideoThumbnail') return msg.args[1];
                }}
                return null;
              }},
            }},
          }},
        }};
        {spec_js}
        {local_js}
        {direct_js}
        {bridge_js}
        globalThis.__zaloBridge = {{
          callSync: (path, args) => path === '$znode.fs.existsSync' ? existing.has(args[0]) : null,
          callAsync: async (path, args) => {{
            asyncCalls.push({{ path, args }});
            if (path === '$znode.fs.exists') return existing.has(args[0]);
            if (path === '$zFileManager.generateVideoThumbnail') return args[1];
            return null;
          }},
        }};
        (async () => {{
          const syncFresh = window.$znode.unusedFilename.sync('/dl/fresh.jpg');
          const syncCollided = window.$znode.unusedFilename.sync('/dl/photo.jpg');
          const asyncCollided = await window.$znode.unusedFilename.async('/dl/photo.jpg');
          const thumbSaved = await window.$zFeatures.genThumb.writeSavePathFilePhoto(
            'u1', 'fb', new Uint8Array([0xff, 0xd8, 0xff]), {{ msgId: '99' }}
          );
          const mp4Out = await window.$znode.nativelibs.mp4thumb().generateThumbnail(
            '/cache/in.mp4', '/cache/out.jpg', 320, 240
          );
          const vid = new HTMLVideoElement();
          vid.src = 'file:///home/user/Zalo Received Files/clip 1.mp4';
          vid.poster = 'file:///home/user/thumb 1.jpg';
          const img = new HTMLImageElement();
          img.src = 'file:///home/user/pic 1.jpg';
          const portalImg = new HTMLImageElement();
          portalImg.setAttribute('src', 'zfile://local/home/user/Cache/1794199743201_h');
          const fetched = await window.fetch('file:///home/user/local.jxl');
          console.log(JSON.stringify({{
            syncFresh,
            syncCollided,
            asyncCollided,
            thumbSaved,
            ipcChannel: ipcCalls[0] && ipcCalls[0].channel,
            ipcHasBytes: Boolean(ipcCalls[0] && ipcCalls[0].args[2] && ipcCalls[0].args[2].__bytes),
            mp4Out,
            vidSrc: vid.src,
            vidCrossOrigin: vid.getAttribute('crossorigin'),
            vidPoster: vid.poster,
            imgSrc: img.src,
            portalImgAttr: portalImg.getAttribute('src'),
            fetchedUrl: fetched.url,
          }}));
        }})();
        "#
    );
    let harness_path = tmp.path().join("direct_ui_harness.js");
    std::fs::write(&harness_path, &node_harness).unwrap();
    let out = Command::new("node")
        .arg(&harness_path)
        .output()
        .expect("node must run direct-ui harness");
    assert!(
        out.status.success(),
        "node harness failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let res: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(res["syncFresh"], "/dl/fresh.jpg");
    assert_eq!(res["syncCollided"], "/dl/photo (2).jpg");
    assert_eq!(res["asyncCollided"], "/dl/photo (2).jpg");
    assert_eq!(res["thumbSaved"], "/saved/thumb_123.jpg");
    assert_eq!(res["ipcChannel"], "gen-thumb_write-savepath-file-photo");
    assert_eq!(res["ipcHasBytes"], true);
    assert_eq!(res["mp4Out"], "/cache/out.jpg");
    assert_eq!(
        res["vidSrc"],
        "http://127.0.0.1:43210/secret-tok/zfile/local/home/user/Zalo%20Received%20Files/clip%201.mp4"
    );
    assert_eq!(res["vidCrossOrigin"], "anonymous");
    assert_eq!(
        res["vidPoster"],
        "zalo-app://local/__olaz/zfile/home/user/thumb%201.jpg"
    );
    assert_eq!(
        res["imgSrc"],
        "zalo-app://local/__olaz/zfile/home/user/pic%201.jpg"
    );
    assert_eq!(
        res["portalImgAttr"],
        "zalo-app://local/__olaz/zfile/home/user/Cache/1794199743201_h"
    );
    assert_eq!(
        res["fetchedUrl"],
        "zalo-app://local/__olaz/zfile/home/user/local.jxl"
    );

    // 4. Native `ffmpeg` video thumbnail extraction works on BOTH plain `.mp4` and `fileNoise` `.mp4`!
    if Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        let sample_mp4 = platform.user_data().join("sample_vid.mp4");
        let noised_mp4 = platform.user_data().join("sample_vid_noised");
        let thumb_plain = platform.user_data().join("thumbs/plain.jpg");
        let thumb_noised = platform.user_data().join("thumbs/noised.jpg");

        let ff = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=green:s=160x120:d=0.5",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&sample_mp4)
            .output()
            .unwrap();
        assert!(ff.status.success());

        olaz_core::copy_with_noise_mode(&sample_mp4, &noised_mp4, true, "").unwrap();

        let r1 = fsb
            .dispatch_async(
                "$zFileManager.generateVideoThumbnail",
                &[
                    json!(sample_mp4.to_string_lossy()),
                    json!(thumb_plain.to_string_lossy()),
                    json!(120),
                    json!(120),
                ],
            )
            .await
            .unwrap();
        assert_eq!(r1, json!(thumb_plain.to_string_lossy()));
        let jpg1 = std::fs::read(&thumb_plain).unwrap();
        assert!(jpg1.starts_with(&[0xFF, 0xD8, 0xFF]));

        let r2 = fsb
            .dispatch_async(
                "$zFileManager.generateVideoThumbnail",
                &[
                    json!(noised_mp4.to_string_lossy()),
                    json!(thumb_noised.to_string_lossy()),
                    json!(120),
                    json!(120),
                ],
            )
            .await
            .unwrap();
        assert_eq!(r2, json!(thumb_noised.to_string_lossy()));
        let jpg2 = std::fs::read(&thumb_noised).unwrap();
        assert!(jpg2.starts_with(&[0xFF, 0xD8, 0xFF]));
    }
}

#[test]
fn fs_bridge_portal_granted_file_allows_ensure_dir_on_existing_parent_without_exposing_siblings() {
    let tmp = TempDir::new().unwrap();
    let app_root = tmp.path().join("app-root");
    let external_dir = tmp.path().join("external-pictures");
    std::fs::create_dir_all(&external_dir).unwrap();

    let platform = Arc::new(ZaloPlatform::with_root(&app_root, "26.10.20"));
    let fs_bridge = FsBridge::new(Arc::clone(&platform));

    let chosen_photo = external_dir.join("saved_photo.jpg");
    let sibling_photo = external_dir.join("other_photo.jpg");
    let new_subdir = external_dir.join("unapproved_subdir");

    // Before portal grant, writing to `chosen_photo` is rejected.
    assert_eq!(
        fs_bridge
            .write_file_sync(&chosen_photo, b"jpeg-bytes")
            .unwrap_err()
            .code,
        "EACCES"
    );

    // Granting only the chosen file allows `fsExtra.ensureDir(parent)` (`mkdir(parent, true)`)
    // and writing/overwriting `chosen_photo`, while keeping siblings and new subdirs blocked.
    platform.grant_writable_path(&chosen_photo);
    fs_bridge
        .mkdir_sync(&external_dir, true)
        .expect("ensureDir on existing parent of portal-granted file must succeed");
    fs_bridge
        .write_file_sync(&chosen_photo, b"jpeg-bytes-v1")
        .expect("writing portal-granted file must succeed");
    assert_eq!(std::fs::read(&chosen_photo).unwrap(), b"jpeg-bytes-v1");

    assert_eq!(
        fs_bridge
            .write_file_sync(&sibling_photo, b"unauthorized")
            .unwrap_err()
            .code,
        "EACCES"
    );
    assert_eq!(
        fs_bridge.mkdir_sync(&new_subdir, true).unwrap_err().code,
        "EACCES"
    );
}

#[test]
fn linux_direct_ui_utility_process_media_unused_filename_and_anchor_download_work_in_node() {
    let tmp = TempDir::new().unwrap();
    let platform = ZaloPlatform::with_root(tmp.path(), "26.10.20");
    let scripts = olaz_core::popup_user_scripts(&platform);

    let find_script = |label: &str| -> &str {
        scripts
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, s)| s.as_str())
            .unwrap_or_else(|| panic!("missing script {label}"))
    };

    let harness = format!(
        r#"
globalThis.window = globalThis;
globalThis.self = globalThis;
globalThis.location = {{ href: 'zalo-app://local/pc-dist/index.html', pathname: '/pc-dist/index.html', origin: 'zalo-app://local' }};
globalThis.navigator = {{ userAgent: 'Mozilla/5.0', clipboard: {{}} }};
class HTMLAnchorElement {{
  constructor(href, download) {{
    this.href = href;
    this._dl = download;
  }}
  hasAttribute(k) {{ return k === 'download' && this._dl !== undefined; }}
  getAttribute(k) {{ return k === 'download' ? (this._dl || '') : (k === 'href' ? this.href : null); }}
  click() {{}}
}}
globalThis.HTMLAnchorElement = HTMLAnchorElement;
globalThis.document = {{
  readyState: 'loading',
  documentElement: {{ style: {{ setProperty() {{}} }} }},
  head: {{ appendChild() {{}} }},
  createElement(tag) {{ return {{ tagName: tag, style: {{}}, setAttribute() {{}} }}; }},
  addEventListener() {{}},
}};
globalThis.MutationObserver = class {{ observe() {{}} }};
globalThis.addEventListener = () => {{}};

const existingSet = new Set(['/tmp/photo.jpg', '/tmp/photo (1).jpg']);
const writtenFiles = new Map();

globalThis.XMLHttpRequest = class {{
  open(_m, _u) {{}}
  setRequestHeader() {{}}
  send(body) {{
    const req = JSON.parse(body);
    this.status = 200;
    if (req.path === '$znode.fs.existsSync') {{
      this.responseText = JSON.stringify({{ value: existingSet.has(req.args[0]) }});
    }} else {{
      this.responseText = JSON.stringify({{ value: null }});
    }}
  }}
}};

globalThis.webkit = {{
  messageHandlers: {{
    zlog: {{ postMessage() {{}} }},
    zstate: {{ postMessage() {{}} }},
    znet: {{ postMessage: async () => ({{ status: 200, headers: {{}}, body: '' }}) }},
    zbridge: {{
      postMessage: async (msg) => {{
        if (msg.kind === 'invoke' && msg.channel === 'show-save-dialog') {{
          return {{ canceled: false, filePath: '/tmp/saved_from_blob.jpg' }};
        }}
        if (msg.kind === 'call' && msg.path === '$zFileManager.writeBufferToPath') {{
          const raw = msg.args[1];
          const bytes = raw && raw.__bytes ? Buffer.from(raw.__bytes, 'base64') : Buffer.from(raw);
          writtenFiles.set(msg.args[0], Array.from(bytes));
          return true;
        }}
        return null;
      }}
    }}
  }}
}};

{bridge_spec}
{local_modules}
{direct_ui}
{bridge_js}

(async () => {{
  'use strict';
  // 1. `$znode.unusedFilename.sync` must return a string synchronously (not a Promise!).
  const unused = window.$znode.unusedFilename.sync('/tmp/photo.jpg');
  if (typeof unused !== 'string' || unused !== '/tmp/photo (2).jpg') {{
    throw new Error('unexpected unusedFilename.sync: ' + String(unused));
  }}

  // 2. Simulate Zalo's strict-mode `Be()` overwriting `window.$zsub = {{}}` and assigning
  //    `$zsub.$zMsgChannel.onReceiveTransferredPortV2` on `zalo-app://local` (non-`file://`).
  window.$zsub = {{}};
  let deliveredPort = null;
  window.$zsub.$zMsgChannel.onReceiveTransferredPortV2((ev, payload) => {{
    deliveredPort = {{ payload, port: ev && ev.ports && ev.ports[0] }};
  }});

  await window.$zMsgChannel.requestOpenChannelTo('main-renderer', 'utility-process-media', 'chan-test-1');
  await new Promise((r) => setTimeout(r, 15));
  if (!deliveredPort || !deliveredPort.port || deliveredPort.payload.channelID !== 'chan-test-1') {{
    throw new Error('utility-process-media port was not delivered');
  }}
  let channelEstablished = false;
  window.$zsub.$zMsgChannel.onChannelEstablished((_ev, info) => {{
    if (Array.isArray(info) && info[2] === 'chan-test-1') channelEstablished = true;
  }});
  await window.$zMsgChannel.notifyPortDelivered('main-renderer', 'utility-process-media', 'chan-test-1', deliveredPort.payload.channelSessionID);
  await new Promise((r) => setTimeout(r, 15));
  if (!channelEstablished) {{
    throw new Error('onChannelEstablished did not fire after notifyPortDelivered');
  }}
  const port = deliveredPort.port;
  port.start();

  const sendTask = (id, type, payload) => new Promise((resolve, reject) => {{
    const onMsg = (ev) => {{
      const d = ev.data || ev;
      if (d && d.id === id && d.messageType === 'response') {{
        port.removeEventListener('message', onMsg);
        if (!d.error) resolve(d.result);
        else reject(new Error(JSON.stringify(d.error)));
      }}
    }};
    port.addEventListener('message', onMsg);
    port.postMessage({{ id, type, messageType: 'request', params: {{ payload }} }});
  }});

  const pong = await sendTask(1, 'PING', null);
  if (pong !== 'pong') throw new Error('expected pong, got ' + String(pong));

  // Naked JXL codestream: FF 0A + SizeHeader (div8=0, h=480 -> 13-bit=479; w=640 -> ratio=0, 13-bit=639)
  // + ImageMetadata (all_default=0, extra_fields=1, orientation_minus_1=5 => orientation=6).
  const bits = [];
  const pushBitsLE = (val, n) => {{ for (let i = 0; i < n; i++) bits.push((val >>> i) & 1); }};
  pushBitsLE(0, 1);       // div8 = false
  pushBitsLE(1, 2);       // h selector = 1 (13-bit)
  pushBitsLE(480 - 1, 13);
  pushBitsLE(0, 3);       // ratio = 0 (explicit width)
  pushBitsLE(1, 2);       // w selector = 1 (13-bit)
  pushBitsLE(640 - 1, 13);
  pushBitsLE(0, 1);       // all_default = false
  pushBitsLE(1, 1);       // extra_fields = true
  pushBitsLE(6 - 1, 3);   // orientation = 6
  const payloadBytes = new Uint8Array(Math.ceil(bits.length / 8) + 2);
  payloadBytes[0] = 0xff;
  payloadBytes[1] = 0x0a;
  for (let i = 0; i < bits.length; i++) {{
    if (bits[i]) payloadBytes[2 + (i >> 3)] |= (1 << (i & 7));
  }}

  // Wrap in ISO BMFF JXL container + apply Zalo `fileNoise` (`0x93` XOR + `07 7a 41 6c 4f 07`)!
  const jxlSig = [0x00,0x00,0x00,0x0c,0x4a,0x58,0x4c,0x20,0x0d,0x0a,0x87,0x0a];
  const jxlcLen = 8 + payloadBytes.length;
  const container = new Uint8Array(jxlSig.length + jxlcLen);
  container.set(jxlSig, 0);
  container[12] = (jxlcLen >>> 24) & 0xff;
  container[13] = (jxlcLen >>> 16) & 0xff;
  container[14] = (jxlcLen >>> 8) & 0xff;
  container[15] = jxlcLen & 0xff;
  container.set([0x6a, 0x78, 0x6c, 0x63], 16); // 'jxlc'
  container.set(payloadBytes, 20);

  const noised = new Uint8Array(container.length + 6);
  noised.set(container, 0);
  for (let i = 0; i < Math.min(100, container.length); i++) noised[i] ^= 0x93;
  noised.set([0x07, 0x7a, 0x41, 0x6c, 0x4f, 0x07], container.length);

  const info = await sendTask(2, 'GET_JXL_INFO', {{ input: noised }});
  if (info.width !== 640 || info.height !== 480 || info.orientation !== 6) {{
    throw new Error('unexpected JXL info: ' + JSON.stringify(info));
  }}

  // 3. Detached `<a href="blob:..." download="chat.jpg">.click()` followed immediately by `revokeObjectURL`.
  const blobBytes = new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 0x11, 0x22, 0xff, 0xd9]);
  const blobUrl = URL.createObjectURL(new Blob([blobBytes], {{ type: 'image/jpeg' }}));
  const a = new HTMLAnchorElement(blobUrl, 'chat.jpg');
  a.click();
  URL.revokeObjectURL(blobUrl);

  await new Promise((r) => setTimeout(r, 50));
  const saved = writtenFiles.get('/tmp/saved_from_blob.jpg');
  if (!saved || JSON.stringify(saved) !== JSON.stringify(Array.from(blobBytes))) {{
    throw new Error('blob anchor download did not persist expected bytes: ' + JSON.stringify(saved));
  }}

  port.close();
  console.log('ALL_OK');
  process.exit(0);
}})().catch((e) => {{
  console.error(e);
  process.exit(1);
}});
"#,
        bridge_spec = find_script("bridge-spec"),
        local_modules = find_script("local-modules"),
        direct_ui = find_script("linux-direct-ui"),
        bridge_js = find_script("bridge"),
    );

    let harness_path = tmp.path().join("harness.js");
    std::fs::write(&harness_path, &harness).expect("write harness.js");

    let output = Command::new("node")
        .arg(&harness_path)
        .output()
        .expect("node must execute harness");
    assert!(
        output.status.success(),
        "node harness failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ALL_OK"));
}

#[test]
fn popup_viewer_zimg_portal_sync_and_photofs_denoising_work_in_node() {
    // 1. Verify Patch #13 (`Ze` `Z.d.Image`): `decoding:"sync"`, `loading:"eager"`, native DOM `load`
    //    listener + cached `complete && naturalWidth > 0` across both mount and `src` updates.
    let synthetic_ze = br#"class Ze extends o.a.Component{constructor(e){super(e),this.imageRef=o.a.createRef(),this._mounted=!1,this.state={loaded:!1},this.interceptOnLoad=e=>{this._mounted&&this.setState({loaded:!0})}}componentDidMount(){this._mounted=!0,this.props.src&&Xe.register(this.props.src)}componentWillUnmount(){this._mounted=!1,this.props.src&&Xe.unregister(this.props.src)}render(){const e={draggable:!1,style:Object.assign({width:"inherit",height:"inherit",cursor:"pointer"},this.props.style,{visibility:this.state.loaded?"visible":"hidden"}),loading:"lazy"};return o.a.createElement("img",Object.assign({ref:this.imageRef},e,{src:this.props.src,onLoad:this.interceptOnLoad,"data-z-element-type":"image","data-zdrag-region":"zdraggable-region",decoding:"async",loading:"lazy"}))}}"#;
    let patched_ze = patch_zalo_linux_renderer_bundle(synthetic_ze)
        .expect("Ze patch #13 must match");
    assert!(patch_zalo_linux_renderer_bundle(&patched_ze).is_none());
    let patched_ze_str = std::str::from_utf8(&patched_ze).unwrap();
    assert!(patched_ze_str.contains(r#"decoding:"sync",loading:"eager""#));

    // 2. Verify `PHOTO_FS` (`READ_ARRAY_BUFFER` + `READ_PARTIAL`) strips Zalo `fileNoise` and
    //    `wrappedWinOpen` installs `zalo-app://local/__olaz/zfile/` DOM URL rewriters onto child `popupWin`.
    let tmp = TempDir::new().unwrap();
    let platform = ZaloPlatform::with_root(tmp.path(), "26.10.20");
    let scripts = olaz_core::popup_user_scripts(&platform);
    let find_script = |label: &str| -> &str {
        scripts
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, s)| s.as_str())
            .unwrap()
    };

    let script = format!(
        r#"
const registered = [];
const unregistered = [];
const Xe = {{
  register: (u) => registered.push(u),
  unregister: (u) => unregistered.push(u),
}};
const o = {{
  a: {{
    Component: class {{
      constructor(p) {{ this.props = p; }}
      setState(next) {{ this.state = typeof next === 'function' ? next(this.state) : Object.assign({{}}, this.state, next); }}
    }},
    createRef: () => ({{ current: null }}),
    createElement: (tag, props) => ({{ tag, props }}),
  }},
}};
{patched_ze_str}

// Simulate cross-window portal `<img>` where React's synthetic `onLoad` does NOT fire,
// testing both native DOM `load` event AND `componentDidUpdate` to an already-cached image!
const makePortalImg = (complete, nw) => {{
  const listeners = {{}};
  return {{
    complete,
    naturalWidth: nw,
    addEventListener(type, fn) {{ (listeners[type] = listeners[type] || []).push(fn); }},
    emit(type) {{ for (const fn of (listeners[type] || [])) fn({{ target: this }}); }},
  }};
}};

const plainJpeg = Buffer.from([0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0xff, 0xd9]);
const noisedJpeg = Buffer.alloc(plainJpeg.length + 6);
plainJpeg.copy(noisedJpeg, 0);
for (let i = 0; i < Math.min(100, plainJpeg.length); i++) noisedJpeg[i] ^= 0x93;
Buffer.from([0x07, 0x7a, 0x41, 0x6c, 0x4f, 0x07]).copy(noisedJpeg, plainJpeg.length);

// Child popup realm with its own Element.prototype!
class ChildElement {{
  constructor(tag) {{ this.tagName = tag; this._attrs = {{}}; }}
  hasAttribute(k) {{ return Object.prototype.hasOwnProperty.call(this._attrs, k.toLowerCase()); }}
  getAttribute(k) {{ return this._attrs[k.toLowerCase()] ?? null; }}
  setAttribute(k, v) {{ this._attrs[k.toLowerCase()] = String(v); }}
}}
const childWin = {{
  closed: false,
  location: {{ href: 'about:blank' }},
  document: {{ readyState: 'loading', body: null }},
  Element: ChildElement,
  addEventListener() {{}},
  removeEventListener() {{}},
}};

globalThis.window = globalThis;
globalThis.self = globalThis;
globalThis.location = {{ protocol: 'zalo-app:', href: 'zalo-app://local/index.html', pathname: '/index.html', origin: 'zalo-app://local' }};
globalThis.navigator = {{ userAgent: 'Mozilla/5.0', clipboard: {{}} }};
globalThis.__olazMediaProxyBase = 'http://127.0.0.1:45555/tok';
globalThis.open = () => childWin;
globalThis.document = {{
  readyState: 'complete',
  documentElement: {{ style: {{ setProperty() {{}} }} }},
  head: {{ appendChild() {{}} }},
  createElement(tag) {{ return {{ tagName: tag, style: {{}}, setAttribute() {{}} }}; }},
  addEventListener() {{}},
}};
globalThis.MutationObserver = class {{ observe() {{}} }};
globalThis.addEventListener = () => {{}};

let sliceCheckNoise = null;
globalThis.webkit = {{
  messageHandlers: {{
    zlog: {{ postMessage() {{}} }},
    zstate: {{ postMessage() {{}} }},
    znet: {{ postMessage: async () => ({{ status: 200, headers: {{}}, body: '' }}) }},
    zbridge: {{
      postMessage: async (msg) => {{
        if (msg.kind === 'call' && msg.path === '$zFileManager.getFileArrayBuffer') {{
          return {{ __arraybuffer: true, __bytes: noisedJpeg.toString('base64') }};
        }}
        if (msg.kind === 'call' && msg.path === '$zFileManager.readFileSlice') {{
          sliceCheckNoise = msg.args[3];
          return {{ __arraybuffer: true, __bytes: plainJpeg.subarray(0, msg.args[2]).toString('base64') }};
        }}
        return null;
      }},
    }},
  }},
}};

{bridge_spec}
{local_modules}
{direct_ui}
{bridge_js}

(async () => {{
  // A. Ze native DOM `load` listener in portal
  const z1 = new Ze({{ src: 'zalo-app://local/__olaz/zfile/tmp/a.jpg' }});
  const el1 = makePortalImg(false, 0);
  z1.imageRef.current = el1;
  z1.componentDidMount();
  const r1Before = z1.render();
  el1.emit('load');
  const r1After = z1.render();

  // B. Ze `componentDidUpdate` to an already-cached image
  const z2 = new Ze({{ src: '' }});
  const el2 = makePortalImg(false, 0);
  z2.imageRef.current = el2;
  z2.componentDidMount();
  el2.complete = true;
  el2.naturalWidth = 640;
  const prevProps = z2.props;
  z2.props = {{ src: 'zalo-app://local/__olaz/zfile/tmp/cached_h.jpg' }};
  z2.componentDidUpdate(prevProps);
  await new Promise((r) => setTimeout(r, 15));
  const r2After = z2.render();

  // C. Child popup window `Element.prototype.setAttribute('src', 'zfile://local/...')`
  const opened = window.open('/popup-viewer.html', 'windowId=photo-viewer', 'width=800,height=600');
  let dclFired = false;
  opened.addEventListener('DOMContentLoaded', () => {{ dclFired = true; }});
  childWin.location.href = 'zalo-app://local/popup-viewer.html';
  childWin.document.readyState = 'complete';
  childWin.document.body = {{}};
  await new Promise((r) => setTimeout(r, 40));
  const popupImg = new ChildElement('IMG');
  popupImg.setAttribute('src', 'zfile://local/home/khoavo/Cache/1794199743201_h');
  const popupVid = new ChildElement('VIDEO');
  popupVid.setAttribute('src', 'zfile://local/home/khoavo/Cache/clip.mp4');

  // D. PHOTO_FS READ_ARRAY_BUFFER + READ_PARTIAL denoising
  window.$zsub = {{}};
  let port = null;
  window.$zsub.$zMsgChannel.onReceiveTransferredPortV2((ev) => {{ port = ev.ports[0]; }});
  await window.$zMsgChannel.requestOpenChannelTo('main-renderer', 'utility-process-media', 'ch-denoise');
  await new Promise((r) => setTimeout(r, 15));
  port.start();
  const ask = (id, op, extra) => new Promise((res, rej) => {{
    const h = (ev) => {{
      const d = ev.data || ev;
      if (d && d.id === id && d.messageType === 'response') {{
        port.removeEventListener('message', h);
        d.error ? rej(d.error) : res(d.result);
      }}
    }};
    port.addEventListener('message', h);
    port.postMessage({{ id, type: 'PHOTO_FS', messageType: 'request', params: {{ payload: Object.assign({{ operation: op, path: '/cache/noised_photo' }}, extra) }} }});
  }});
  const fullAb = await ask(10, 'READ_ARRAY_BUFFER', {{}});
  const partAb = await ask(11, 'READ_PARTIAL', {{ start: 0, end: 4 }});
  port.close();

  console.log(JSON.stringify({{
    r1VisBefore: r1Before.props.style.visibility,
    r1VisAfter: r1After.props.style.visibility,
    r2VisAfter: r2After.props.style.visibility,
    dclFired,
    popupImgSrc: popupImg.getAttribute('src'),
    popupVidSrc: popupVid.getAttribute('src'),
    fullHex: Buffer.from(fullAb).toString('hex'),
    expectedFullHex: plainJpeg.toString('hex'),
    partHex: Buffer.from(partAb).toString('hex'),
    sliceCheckNoise,
  }}));
  process.exit(0);
}})();
"#,
        bridge_spec = find_script("bridge-spec"),
        local_modules = find_script("local-modules"),
        direct_ui = find_script("linux-direct-ui"),
        bridge_js = find_script("bridge"),
    );

    let res = run_node_json(&script);
    assert_eq!(res["r1VisBefore"], "hidden");
    assert_eq!(res["r1VisAfter"], "visible");
    assert_eq!(res["r2VisAfter"], "visible");
    assert_eq!(res["dclFired"], true);
    assert_eq!(
        res["popupImgSrc"],
        "zalo-app://local/__olaz/zfile/home/khoavo/Cache/1794199743201_h"
    );
    assert_eq!(
        res["popupVidSrc"],
        "http://127.0.0.1:45555/tok/zfile/local/home/khoavo/Cache/clip.mp4"
    );
    assert_eq!(res["fullHex"], res["expectedFullHex"]);
    assert_eq!(res["partHex"], "ffd8ffe0");
    assert_eq!(res["sliceCheckNoise"], true);
}

#[test]
fn video_thumb_flag_and_drive_migration_filesystem_patches_work_and_are_idempotent() {
    let synthetic = br#"const cfg={zalo_cloud:{z_cloud:{feats:{gen_video_thumb:{enable:!1,timeout:5e3}}}}};async function genVideoThumbFromFile(e){if(!Object(J.l)())throw{error_code:"NOT_SUPPORT",message:"Version not support"};return e}class DriveMigrationService{filterAvailableDrive(e,t){return"ntfs"===t.detectFilesystemSync(e).filesystemType.toLocaleLowerCase()}}"#;
    let patched = patch_zalo_linux_renderer_bundle(synthetic)
        .expect("video-thumb + DriveMigrationService patch must match");
    let patched_str = std::str::from_utf8(&patched).unwrap();
    assert!(patched_str.contains("gen_video_thumb:{enable:!0"));
    assert!(patched_str.contains(r#"if(!!0)throw{error_code:"NOT_SUPPORT""#));
    assert!(patched_str.contains(r#"(["ntfs","ext4","btrfs","xfs","f2fs","zfs"].includes(t.detectFilesystemSync(e).filesystemType.toLocaleLowerCase()))"#));
    assert!(
        patch_zalo_linux_renderer_bundle(&patched).is_none(),
        "video-thumb + DriveMigrationService patches must be idempotent"
    );

    let js = format!(
        r#"
(async () => {{
  const J = {{ l: () => false }};
  {patched_str}
  const thumbOut = await genVideoThumbFromFile('/tmp/clip.mp4');
  const svc = new DriveMigrationService();
  const fsHelper = (fsType) => ({{
    detectFilesystemSync: (_p) => ({{ filesystemType: fsType }}),
  }});
  const allowed = ['NTFS', 'ext4', 'BTRFS', 'xfs', 'f2fs', 'ZFS'].map((f) => svc.filterAvailableDrive('/mnt', fsHelper(f)));
  const rejected = ['vfat', 'tmpfs', 'overlay'].map((f) => svc.filterAvailableDrive('/mnt', fsHelper(f)));
  console.log(JSON.stringify({{
    cfgEnable: cfg.zalo_cloud.z_cloud.feats.gen_video_thumb.enable,
    thumbOut,
    allowed,
    rejected,
  }}));
  process.exit(0);
}})();
"#
    );
    let res = run_node_json(&js);
    assert_eq!(res["cfgEnable"], true);
    assert_eq!(res["thumbOut"], "/tmp/clip.mp4");
    assert_eq!(res["allowed"], json!([true, true, true, true, true, true]));
    assert_eq!(res["rejected"], json!([false, false, false]));
}

#[test]
fn preload_file_utilities_dbutils_zfile_longest_prefix_and_clipboard_jpeg_work_in_node() {
    let tmp = TempDir::new().unwrap();
    let platform = ZaloPlatform::with_root(tmp.path(), "26.10.20");

    // 1. Verify `bridge_spec_script` registers all sync and async addon members.
    let spec_js = bridge_spec_script(&platform);
    let json_str = spec_js
        .strip_prefix("window.__zaloBridgeSpec = ")
        .and_then(|s| s.strip_suffix(';'))
        .unwrap();
    let spec: Value = serde_json::from_str(json_str).unwrap();
    for sync_path in [
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeSync",
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeTreeSync",
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeByGlobSync",
        "$znode.nativelibs.fileUtilitiesAddon.detectHardlinksSync",
        "$znode.nativelibs.fileUtilitiesAddon.detectFilesystemSync",
        "$znode.nativelibs.fileUtilitiesAddon.cancelJob",
        "$znode.nativelibs.dbUtilsAddon.decompressAndDecryptDb_V2",
        "$znode.nativelibs.dbUtilsAddon.decompressAndDecryptDb",
        "$znode.nativelibs.dbUtilsAddon.parseBinNet",
    ] {
        assert_eq!(
            spec["members"][sync_path],
            json!("sync"),
            "missing sync member {sync_path}"
        );
    }
    for async_path in [
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeAsync",
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeTreeAsync",
        "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeByGlobAsync",
        "$znode.nativelibs.fileUtilitiesAddon.detectHardlinksAsync",
        "$znode.nativelibs.fileUtilitiesAddon.detectFilesystemAsync",
    ] {
        assert_eq!(
            spec["members"][async_path],
            json!("async"),
            "missing async member {async_path}"
        );
    }

    let scripts = olaz_core::direct_ui_user_scripts(&platform, false);
    let find_script = |name: &str| -> String {
        scripts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| panic!("missing script {name}"))
    };

    // 2. Run Node harness verifying:
    //    - `fileUtilitiesAddon` sync + async + `AbortSignal` -> `jobId` (3rd arg) + `cancelJob(jobId)`
    //    - `dbUtilsAddon.decompressAndDecryptDb_V2` stripping 4th `progressCallback` function arg and invoking `cb(null, 100)` when `result === 0`
    //    - `dbUtilsAddon.parseBinNet` converting `Uint8Array` / `ArrayBuffer` into numeric `Array` for `olaz-fs-sync`
    //    - `zfile.diskInfo()` & `zfile.getDiskInfo()` longest-prefix mount `Proxy` + `Object.keys(disks)`
    //    - `$zelectronNative.getClipboardImage().toJPEG(quality)` via `olaz-clipboard-image-jpeg` + fallback to PNG bytes when `null`
    let script = format!(
        r#"
(async () => {{
  const syncCalls = [];
  const asyncCalls = [];
  let clipJpegMode = 'jpeg';
  const pngClipBytes = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x01, 0x02]);
  const jpgClipBytes = Buffer.from([0xff, 0xd8, 0xff, 0xe0, 0xaa, 0xbb, 0xff, 0xd9]);

  globalThis.window = globalThis;
  globalThis.location = {{ protocol: 'zalo-app:', href: 'zalo-app://local/login.html', pathname: '/login.html' }};
  globalThis.document = {{ readyState: 'complete', addEventListener() {{}} }};
  globalThis.addEventListener = () => {{}};
  globalThis.__zaloNativeXHR = class {{
    open(_m, _u) {{}}
    send(body) {{
      const req = JSON.parse(body);
      syncCalls.push(req);
      if (req.path === '$zelectronNative.clipboardImagePng') {{
        this.responseText = JSON.stringify({{ value: {{ __bytes: pngClipBytes.toString('base64') }} }});
        return;
      }}
      if (req.channel === 'olaz-clipboard-image-jpeg') {{
        if (clipJpegMode === 'jpeg') {{
          this.responseText = JSON.stringify({{ value: {{ __bytes: jpgClipBytes.toString('base64') }} }});
        }} else {{
          this.responseText = JSON.stringify({{ value: null }});
        }}
        return;
      }}
      if (req.channel === 'olaz-fs-sync') {{
        const [method, ...rest] = req.args;
        if (method === 'detectFilesystemSync') {{
          this.responseText = JSON.stringify({{ value: {{ filesystemType: 'ext4', path: rest[0] }} }});
        }} else if (method === 'cancelJob') {{
          this.responseText = JSON.stringify({{ value: true }});
        }} else if (method === 'decompressAndDecryptDb_V2') {{
          this.responseText = JSON.stringify({{ value: {{ result: 0, argsCount: rest.length }} }});
        }} else if (method === 'parseBinNet') {{
          this.responseText = JSON.stringify({{ value: {{ ok: true, isArray: Array.isArray(rest[0]), bytes: rest[0] }} }});
        }} else {{
          this.responseText = JSON.stringify({{ value: {{ method, rest }} }});
        }}
        return;
      }}
      // Unhandled `path` returns `__undefined: true` so addon sync helper falls back to `olaz-fs-sync`.
      this.responseText = JSON.stringify({{ value: {{ __undefined: true }} }});
    }}
  }};
  globalThis.webkit = {{
    messageHandlers: {{
      zlog: {{ postMessage() {{}} }},
      zstate: {{ postMessage() {{}} }},
      __olaz_win_drag: {{ postMessage() {{}} }},
      zbridge: {{
        postMessage(msg) {{
          asyncCalls.push(msg);
          if (msg.kind === 'call' && msg.path === '$znode.nativelibs.zfileAddon.getDiskInfo') {{
            return Promise.resolve({{
              '/': {{ label: 'Root', mountpoint: '/', totalSpace: 500e9, freeSpace: 200e9 }},
              '/home': {{ label: 'Home', mountpoint: '/home', totalSpace: 1000e9, freeSpace: 600e9 }},
              '/home/khoavo/mnt': {{ label: 'Data', mountpoint: '/home/khoavo/mnt', totalSpace: 2000e9, freeSpace: 1500e9 }},
            }});
          }}
          if (msg.kind === 'call' && msg.path.startsWith('$znode.nativelibs.fileUtilitiesAddon.')) {{
            return new Promise((resolve) => setTimeout(() => resolve({{ totalSize: 4096, fileCount: 3, args: msg.args }}), 20));
          }}
          return Promise.resolve(null);
        }},
      }},
    }},
  }};

  {bridge_spec}
  {local_modules}
  {direct_ui}
  {bridge_js}

  // A. `fileUtilities` sync + async AbortSignal -> cancelJob(jobId)
  const fu = window.$znode.nativelibs.fileUtilities();
  const fsInfo = fu.detectFilesystemSync('/home/khoavo');
  const ac = new AbortController();
  const dirSizePromise = fu.getDirectorySizeAsync('/home/khoavo/Zalo', {{ workers: 2, signal: ac.signal }});
  ac.abort();
  const dirSizeRes = await dirSizePromise;
  const cancelCalls = syncCalls.filter((c) => c.channel === 'olaz-fs-sync' && c.args[0] === 'cancelJob');

  // B. `dbUtils` decompressAndDecryptDb_V2 + parseBinNet
  const db = window.$znode.nativelibs.dbUtils();
  const progressEvents = [];
  const decRes = db.decompressAndDecryptDb_V2('/tmp/in.db', '/tmp/out.db', 'secret', (err, pct) => {{
    progressEvents.push([err, pct]);
  }});
  const parsedU8 = db.parseBinNet(new Uint8Array([10, 20, 30]));
  const parsedAb = db.parseBinNet(new Uint8Array([40, 50]).buffer);

  // C. `zfile` longest-prefix mount `Proxy`
  const zf = window.$znode.nativelibs.zfile();
  const disks = await zf.diskInfo();
  const diskKeys = Object.keys(disks);
  const resolvedRoot = disks['/var/log/syslog'].mountpoint;
  const resolvedHome = disks['/home/khoavo/.local/share/olaz'].mountpoint;
  const resolvedNested = disks['/home/khoavo/mnt/projects/zalo'].mountpoint;
  const resolvedSibling = disks['/home2/other'].mountpoint;
  const disks2 = await zf.getDiskInfo();
  const resolvedHome2 = disks2['/home/khoavo/Downloads'].mountpoint;

  // D. Clipboard `toJPEG(quality)` + fallback
  const clip1 = window.$zelectronNative.getClipboardImage();
  const jpg72 = Array.from(clip1.toJPEG(72));
  const jpgDefault = Array.from(clip1.toJPEG());
  const jpegArgs = syncCalls.filter((c) => c.channel === 'olaz-clipboard-image-jpeg').map((c) => c.args[0]);
  clipJpegMode = 'null';
  const clipFallback = Array.from(window.$zelectronNative.getClipboardImage().toJPEG(90));

  console.log(JSON.stringify({{
    fsType: fsInfo.filesystemType,
    asyncArgs: dirSizeRes.args,
    cancelJobArgs: cancelCalls.map((c) => c.args),
    decRes,
    progressEvents,
    parsedU8,
    parsedAb,
    diskKeys,
    resolvedRoot,
    resolvedHome,
    resolvedNested,
    resolvedSibling,
    resolvedHome2,
    jpg72,
    jpgDefault,
    jpegArgs,
    clipFallback,
  }}));
  process.exit(0);
}})();
"#,
        bridge_spec = find_script("bridge-spec"),
        local_modules = find_script("local-modules"),
        direct_ui = find_script("linux-direct-ui"),
        bridge_js = find_script("bridge"),
    );

    let res = run_node_json(&script);
    assert_eq!(res["fsType"], "ext4");
    assert_eq!(res["asyncArgs"], json!(["/home/khoavo/Zalo", { "workers": 2 }, 1]));
    assert_eq!(res["cancelJobArgs"], json!([["cancelJob", 1]]));
    assert_eq!(res["decRes"], json!({ "result": 0, "argsCount": 3 }));
    assert_eq!(res["progressEvents"], json!([[null, 100]]));
    assert_eq!(res["parsedU8"], json!({ "ok": true, "isArray": true, "bytes": [10, 20, 30] }));
    assert_eq!(res["parsedAb"], json!({ "ok": true, "isArray": true, "bytes": [40, 50] }));
    assert_eq!(res["diskKeys"], json!(["/", "/home", "/home/khoavo/mnt"]));
    assert_eq!(res["resolvedRoot"], "/");
    assert_eq!(res["resolvedHome"], "/home");
    assert_eq!(res["resolvedNested"], "/home/khoavo/mnt");
    assert_eq!(res["resolvedSibling"], "/");
    assert_eq!(res["resolvedHome2"], "/home");
    assert_eq!(res["jpg72"], json!([0xff, 0xd8, 0xff, 0xe0, 0xaa, 0xbb, 0xff, 0xd9]));
    assert_eq!(res["jpgDefault"], json!([0xff, 0xd8, 0xff, 0xe0, 0xaa, 0xbb, 0xff, 0xd9]));
    assert_eq!(res["jpegArgs"], json!([72, 85]));
    assert_eq!(res["clipFallback"], json!([0x89, 0x50, 0x4e, 0x47, 0x01, 0x02]));
}

#[test]
fn utility_media_port_jxl_tasks_and_live_privacy_receipt_blocker_work_in_node() {
    let tmp = TempDir::new().unwrap();
    let platform = ZaloPlatform::with_root(tmp.path(), "26.10.20");
    let scripts = olaz_core::direct_ui_user_scripts(&platform, false);
    let find_script = |name: &str| -> String {
        scripts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| panic!("missing script {name}"))
    };

    let script = format!(
        r#"
(async () => {{
  const networkHits = [];
  globalThis.window = globalThis;
  globalThis.location = {{ protocol: 'zalo-app:', href: 'zalo-app://local/login.html', pathname: '/login.html' }};
  globalThis.document = {{ readyState: 'complete', addEventListener() {{}} }};
  globalThis.addEventListener = () => {{}};
  globalThis.fetch = async (input) => {{
    const u = typeof input === 'string' ? input : input.url;
    networkHits.push(['fetch', u]);
    return new Response(JSON.stringify({{ realNetwork: true, url: u }}), {{ status: 200 }});
  }};
  class MockXHR extends EventTarget {{
    constructor() {{
      super();
      this.headers = {{}};
      this.responseType = '';
      this.onreadystatechange = null;
      this.onload = null;
      this.onloadend = null;
    }}
    open(method, url, asyncFlag = true) {{
      this._m = method;
      this._u = url;
      this._a = asyncFlag;
    }}
    setRequestHeader(k, v) {{
      this.headers[k] = v;
    }}
    send(_body) {{
      networkHits.push(['xhr', this._u]);
      this.status = 200;
      this.readyState = 4;
      this.responseText = JSON.stringify({{ realNetwork: true, url: this._u }});
      this.response = this.responseType === 'json' ? JSON.parse(this.responseText) : this.responseText;
      if (typeof this.onload === 'function') this.onload({{ type: 'load' }});
    }}
  }}
  globalThis.XMLHttpRequest = MockXHR;
  globalThis.__zaloNativeXHR = class {{
    open() {{}}
    send() {{ this.responseText = JSON.stringify({{ value: null }}); }}
  }};
  globalThis.webkit = {{
    messageHandlers: {{
      zlog: {{ postMessage() {{}} }},
      zstate: {{ postMessage() {{}} }},
      __olaz_win_drag: {{ postMessage() {{}} }},
      zbridge: {{ postMessage() {{ return Promise.resolve(null); }} }},
    }},
  }};

  {bridge_spec}
  {local_modules}
  {direct_ui}
  {bridge_js}

  // 1. Live privacy receipt blocker (`window.__olazPrivacy` & `window.__olazSetPrivacy`)
  // Initially all false -> requests hit network.
  await (await window.fetch('https://chat.zalo.me/api/message/typing?zpw_ver=645')).json();
  window.__olazSetPrivacy({{ blockTyping: true, blockSeen: true, blockDelivered: true }});

  const blockedTyping = await (await window.fetch('https://chat.zalo.me/api/message/typing?zpw_ver=645')).json();
  const blockedGroupSeen = await (await window.fetch('https://chat.zalo.me/api/group/seen?zpw_ver=645')).json();
  const allowedSend = await (await window.fetch('https://chat.zalo.me/api/message/sms?zpw_ver=645')).json();

  const xhrDelivered = await new Promise((resolve) => {{
    const x = new window.XMLHttpRequest();
    x.responseType = 'json';
    x.open('POST', 'https://chat.zalo.me/api/group/delivered?zpw_ver=645', true);
    x.setRequestHeader('Content-Type', 'application/x-www-form-urlencoded');
    x.onload = () => resolve({{ status: x.status, readyState: x.readyState, body: x.response }});
    x.send('params=abc');
  }});

  window.__olazSetPrivacy({{ blockSeen: false }});
  await (await window.fetch('https://chat.zalo.me/api/message/seen?zpw_ver=645')).json();

  // 2. `createUtilityMediaPort` `GET_JXL_INFO` (`status_code: 1`) + `DECODE` / `DECODE_JXL_SINGLE` / `RESIZE_JXL` / `ENCODE_JXL`
  let deliveredPort = null;
  window.$zsub.$zMsgChannel.onReceiveTransferredPortV2((ev, payload) => {{
    deliveredPort = ev.ports[0];
    window.$zMsgChannel.notifyPortDelivered('main-renderer', 'utility-process-media', 'chan-jxl', payload.channelSessionID);
  }});
  await window.$zMsgChannel.requestOpenChannelTo('main-renderer', 'utility-process-media', 'chan-jxl');
  await new Promise((r) => setTimeout(r, 15));
  deliveredPort.start();

  const askPort = (id, type, payload) => new Promise((resolve, reject) => {{
    const h = (ev) => {{
      const d = ev.data || ev;
      if (d && d.id === id && d.messageType === 'response') {{
        deliveredPort.removeEventListener('message', h);
        d.error ? reject(new Error(JSON.stringify(d.error))) : resolve(d.result);
      }}
    }};
    deliveredPort.addEventListener('message', h);
    deliveredPort.postMessage({{ id, type, messageType: 'request', params: {{ payload }} }});
  }});

  // Minimal 8x8 naked JXL codestream: FF 0A + div8=1 (1), h8=0 -> 8px (5 bits=0), ratio=1 (3 bits=1 -> w=h=8), all_default=1 (1 bit=1)
  const jxl8x8 = new Uint8Array([0xff, 0x0a, 0b01000001, 0b00000010]);
  const jxlInfo = await askPort(1, 'GET_JXL_INFO', {{ input: jxl8x8 }});
  const decMulti = await askPort(2, 'DECODE', {{ input: jxl8x8, tasks: [{{ maxWidth: 4, maxHeight: 4 }}] }});
  const decSingle = await askPort(3, 'DECODE_JXL_SINGLE', {{ input: jxl8x8 }});
  const resJxl = await askPort(4, 'RESIZE_JXL', {{ input: jxl8x8, width: 4, height: 4 }});
  const encJxl = await askPort(5, 'ENCODE_JXL', {{ buffer: new Uint8Array(12), width: 2, height: 2 }});
  deliveredPort.close();

  console.log(JSON.stringify({{
    networkHits,
    blockedTyping,
    blockedGroupSeen,
    allowedSend,
    xhrDelivered,
    jxlInfo,
    decMultiFallback: Boolean(decMulti && decMulti.status_code !== undefined),
    decSingleFallback: Boolean(decSingle && decSingle.status_code !== undefined),
    resJxlWidth: resJxl.width,
    encJxlFallback: Boolean(encJxl && encJxl.status_code !== undefined),
  }}));
  process.exit(0);
}})();
"#,
        bridge_spec = find_script("bridge-spec"),
        local_modules = find_script("local-modules"),
        direct_ui = find_script("linux-direct-ui"),
        bridge_js = find_script("bridge"),
    );

    let res = run_node_json(&script);
    assert_eq!(
        res["networkHits"],
        json!([
            ["fetch", "https://chat.zalo.me/api/message/typing?zpw_ver=645"],
            ["fetch", "https://chat.zalo.me/api/message/sms?zpw_ver=645"],
            ["fetch", "https://chat.zalo.me/api/message/seen?zpw_ver=645"]
        ])
    );
    let expected_ok = json!({ "error_code": 0, "error_message": "Successful.", "data": "" });
    assert_eq!(res["blockedTyping"], expected_ok);
    assert_eq!(res["blockedGroupSeen"], expected_ok);
    assert_eq!(res["allowedSend"]["realNetwork"], true);
    assert_eq!(
        res["xhrDelivered"],
        json!({ "status": 200, "readyState": 4, "body": expected_ok })
    );
    assert_eq!(
        res["jxlInfo"],
        json!({ "width": 8, "height": 8, "orientation": 1, "status_code": 1 })
    );
    assert_eq!(res["decMultiFallback"], true);
    assert_eq!(res["decSingleFallback"], true);
    assert_eq!(res["resJxlWidth"], 4);
    assert_eq!(res["encJxlFallback"], true);
}

// ============================================================================
// 8. Track B (Anti-Recall): `dal-worker` IndexedDB Guard & `LINUX_DIRECT_UI_SCRIPT`
// ============================================================================

#[test]
fn anti_recall_dal_worker_idb_put_tombstone_and_quote_preservation_in_node() {
    let prelude = worker_prelude();
    let script = format!(
        r#"
(async () => {{
  const bcPeers = new Map();
  class MockBroadcastChannel {{
    constructor(name) {{
      this.name = name;
      this.onmessage = null;
      if (!bcPeers.has(name)) bcPeers.set(name, new Set());
      bcPeers.get(name).add(this);
    }}
    postMessage(data) {{
      const set = bcPeers.get(this.name) || new Set();
      for (const peer of set) {{
        if (peer !== this && typeof peer.onmessage === 'function') {{
          setTimeout(() => peer.onmessage({{ data }}), 0);
        }}
      }}
    }}
    close() {{
      const set = bcPeers.get(this.name);
      if (set) set.delete(this);
    }}
  }}

  const makeAsyncReq = (resultVal) => {{
    const listeners = {{ success: [], error: [] }};
    const req = {{
      result: undefined,
      error: null,
      readyState: 'pending',
      onsuccess: null,
      onerror: null,
      addEventListener(type, fn) {{
        if (listeners[type]) listeners[type].push(fn);
      }},
    }};
    setTimeout(() => {{
      req.result = resultVal;
      req.readyState = 'done';
      const ev = {{ type: 'success', target: req }};
      if (typeof req.onsuccess === 'function') req.onsuccess(ev);
      for (const fn of listeners.success) fn(ev);
    }}, 0);
    return req;
  }};

  class MockIDBDatabase {{
    transaction() {{
      return {{ addEventListener() {{}} }};
    }}
  }}
  class MockIDBObjectStore {{
    constructor(name, keyPath = 'msgId') {{
      this.name = name;
      this.keyPath = keyPath;
      this._rows = new Map();
    }}
    _k(val, explicitKey) {{
      if (explicitKey !== undefined) return JSON.stringify(explicitKey);
      if (Array.isArray(this.keyPath)) return JSON.stringify(this.keyPath.map((k) => val[k]));
      return JSON.stringify(val[this.keyPath || 'msgId']);
    }}
    get(lookupKey) {{
      const raw = this._rows.get(JSON.stringify(lookupKey));
      return makeAsyncReq(raw ? JSON.parse(JSON.stringify(raw)) : undefined);
    }}
    put(val, explicitKey) {{
      const k = this._k(val, explicitKey);
      this._rows.set(k, JSON.parse(JSON.stringify(val)));
      return makeAsyncReq(explicitKey !== undefined ? explicitKey : val[this.keyPath || 'msgId']);
    }}
    add(val, explicitKey) {{
      return this.put(val, explicitKey);
    }}
  }}

  const workerPings = [];
  const mainBc = new MockBroadcastChannel('olaz_privacy');
  mainBc.onmessage = (ev) => {{
    workerPings.push(ev.data);
    if (ev.data && ev.data.type === 'request_privacy') {{
      mainBc.postMessage({{ type: 'privacy_update', anti_recall: true }});
    }}
  }};

  globalThis.self = globalThis;
  globalThis.BroadcastChannel = MockBroadcastChannel;
  globalThis.IDBFactory = class {{
    open() {{ return makeAsyncReq(null); }}
  }};
  globalThis.IDBDatabase = MockIDBDatabase;
  globalThis.IDBObjectStore = MockIDBObjectStore;
  globalThis.postMessage = () => {{}};
  globalThis.addEventListener = () => {{}};

  {prelude}

  await new Promise((r) => setTimeout(r, 15));
  const syncedEnabled = self.__olazGetAntiRecall();

  const waitReq = (req) => new Promise((resolve, reject) => {{
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  }});

  const msgStore = new MockIDBObjectStore('message', 'msgId');

  // 1. Store original text/media message, then incoming msgType=20 chat.undo tombstone
  await waitReq(msgStore.put({{
    msgId: 'm_100',
    msgType: 1,
    message: 'Bí mật không được thu hồi',
    thumb: 'https://cdn.zalo.me/thumb100.jpg',
    localPath: '/tmp/zalo/photo100.jpg',
    e2eeStatus: 1,
  }}));
  await waitReq(msgStore.put({{
    msgId: 'm_100',
    msgType: 20,
    originMsgType: 'chat.undo',
    message: '[Tin nhắn đã được thu hồi]',
  }}));
  const preserved20 = await waitReq(msgStore.get('m_100'));

  // 2. Store another message and recall with msgType=-27
  await waitReq(msgStore.put({{
    msgId: 'm_101',
    msgType: 3,
    message: 'Ghi âm quan trọng',
    folderPath: '/tmp/zalo/voice101.aac',
  }}));
  await waitReq(msgStore.put({{
    msgId: 'm_101',
    msgType: -27,
    message: '',
  }}));
  const preservedNeg27 = await waitReq(msgStore.get('m_101'));

  // 3. Reply quote-wiping protection (`quote.cliMsgType === 20`)
  await waitReq(msgStore.put({{
    msgId: 'm_reply_1',
    msgType: 1,
    message: ' Mình trả lời câu này nhé',
    quote: {{ cliMsgType: 1, msg: 'Nội dung gốc trong trích dẫn', ownerId: 'u_9' }},
  }}));
  await waitReq(msgStore.put({{
    msgId: 'm_reply_1',
    msgType: 1,
    message: ' Mình trả lời câu này nhé',
    quote: {{ cliMsgType: 20, msg: '', ownerId: 'u_9' }},
  }}));
  const preservedQuoteMsg = await waitReq(msgStore.get('m_reply_1'));

  // 4. Composite keyPath support (`['convId', 'msgId']`)
  const compositeStore = new MockIDBObjectStore('message', ['convId', 'msgId']);
  await waitReq(compositeStore.put({{
    convId: 'c_1',
    msgId: 'm_comp',
    msgType: 1,
    message: 'Composite key message',
  }}));
  await waitReq(compositeStore.put({{
    convId: 'c_1',
    msgId: 'm_comp',
    msgType: 20,
    message: '[Tin nhắn đã được thu hồi]',
  }}));
  const preservedComp = await waitReq(compositeStore.get(['c_1', 'm_comp']));

  // 5. Turn anti_recall OFF via BroadcastChannel -> recall tombstones overwrite normally
  mainBc.postMessage({{ type: 'privacy_update', anti_recall: false }});
  await new Promise((r) => setTimeout(r, 10));
  const syncedDisabled = self.__olazGetAntiRecall();
  await waitReq(msgStore.put({{
    msgId: 'm_off',
    msgType: 1,
    message: 'Cho phép thu hồi khi tắt',
  }}));
  await waitReq(msgStore.put({{
    msgId: 'm_off',
    msgType: 20,
    message: '[Tin nhắn đã được thu hồi]',
  }}));
  const overwrittenWhenOff = await waitReq(msgStore.get('m_off'));

  console.log(JSON.stringify({{
    workerPings,
    syncedEnabled,
    preserved20,
    preservedNeg27,
    preservedQuoteMsg,
    preservedComp,
    syncedDisabled,
    overwrittenWhenOff,
  }}));
  process.exit(0);
}})();
"#,
        prelude = prelude,
    );

    let res = run_node_json(&script);
    assert_eq!(res["workerPings"][0]["type"], "request_privacy");
    assert_eq!(res["syncedEnabled"], true);

    assert_eq!(res["preserved20"]["msgType"], 1);
    assert_eq!(res["preserved20"]["message"], "Bí mật không được thu hồi");
    assert_eq!(
        res["preserved20"]["thumb"],
        "https://cdn.zalo.me/thumb100.jpg"
    );
    assert_eq!(res["preserved20"]["localPath"], "/tmp/zalo/photo100.jpg");
    assert_eq!(res["preserved20"]["e2eeStatus"], 1);
    assert_eq!(res["preserved20"]["__olazPresalledType"], 20);
    assert_eq!(res["preserved20"]["__olazRecalled"], true);
    assert_eq!(res["preserved20"]["__olazPreserved"], true);

    assert_eq!(res["preservedNeg27"]["msgType"], 3);
    assert_eq!(res["preservedNeg27"]["message"], "Ghi âm quan trọng");
    assert_eq!(res["preservedNeg27"]["folderPath"], "/tmp/zalo/voice101.aac");
    assert_eq!(res["preservedNeg27"]["__olazRecalled"], true);

    assert_eq!(res["preservedQuoteMsg"]["quote"]["cliMsgType"], 1);
    assert_eq!(
        res["preservedQuoteMsg"]["quote"]["msg"],
        "Nội dung gốc trong trích dẫn"
    );
    assert_eq!(
        res["preservedQuoteMsg"]["quote"]["__olazQuoteRecalled"],
        true
    );

    assert_eq!(res["preservedComp"]["msgType"], 1);
    assert_eq!(res["preservedComp"]["message"], "Composite key message");
    assert_eq!(res["preservedComp"]["__olazRecalled"], true);

    assert_eq!(res["syncedDisabled"], false);
    assert_eq!(res["overwrittenWhenOff"]["msgType"], 20);
    assert_eq!(
        res["overwrittenWhenOff"]["message"],
        "[Tin nhắn đã được thu hồi]"
    );
}

#[test]
fn anti_recall_direct_ui_shadow_redux_guard_dom_badge_and_zstate_in_node() {
    let tmp = TempDir::new().unwrap();
    let platform = ZaloPlatform::with_root(tmp.path(), "26.10.20");
    let scripts = olaz_core::direct_ui_user_scripts(&platform, false);
    let find_script = |name: &str| -> String {
        scripts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| panic!("missing script {name}"))
    };

    let script = format!(
        r#"
(async () => {{
  const bcPeers = new Map();
  class MockBroadcastChannel {{
    constructor(name) {{
      this.name = name;
      this.onmessage = null;
      if (!bcPeers.has(name)) bcPeers.set(name, new Set());
      bcPeers.get(name).add(this);
    }}
    postMessage(data) {{
      const set = bcPeers.get(this.name) || new Set();
      for (const peer of set) {{
        if (peer !== this && typeof peer.onmessage === 'function') {{
          peer.onmessage({{ data }});
        }}
      }}
    }}
  }}

  // Minimal DOM tree supporting `.chat-message[id^="bb_msg_id_"]`, `.zmenu-body`, `#main-tab`, and events
  class MockElement {{
    constructor(tag) {{
      this.tagName = String(tag || 'DIV').toUpperCase();
      this.id = '';
      this.textContent = '';
      this.children = [];
      this.parentNode = null;
      this._attrs = {{}};
      this._listeners = {{}};
      this._classes = new Set();
      this.classList = {{
        add: (...cs) => cs.forEach((c) => this._classes.add(c)),
        remove: (...cs) => cs.forEach((c) => this._classes.delete(c)),
        contains: (c) => this._classes.has(c),
        toggle: (c, f) => {{
          const on = f === undefined ? !this._classes.has(c) : Boolean(f);
          if (on) this._classes.add(c); else this._classes.delete(c);
          return on;
        }},
      }};
    }}
    set className(v) {{
      this._classes = new Set(String(v || '').split(/\s+/).filter(Boolean));
    }}
    get className() {{
      return Array.from(this._classes).join(' ');
    }}
    get firstChild() {{ return this.children[0] || null; }}
    get firstElementChild() {{ return this.children[0] || null; }}
    get previousElementSibling() {{
      if (!this.parentNode) return null;
      const idx = this.parentNode.children.indexOf(this);
      return idx > 0 ? this.parentNode.children[idx - 1] : null;
    }}
    get nextSibling() {{
      if (!this.parentNode) return null;
      const idx = this.parentNode.children.indexOf(this);
      return idx >= 0 && idx + 1 < this.parentNode.children.length ? this.parentNode.children[idx + 1] : null;
    }}
    appendChild(child) {{
      if (child.parentNode) child.parentNode.removeChild(child);
      child.parentNode = this;
      this.children.push(child);
      return child;
    }}
    insertBefore(child, refNode) {{
      if (!refNode) return this.appendChild(child);
      if (child.parentNode) child.parentNode.removeChild(child);
      const idx = this.children.indexOf(refNode);
      child.parentNode = this;
      if (idx >= 0) this.children.splice(idx, 0, child);
      else this.children.push(child);
      return child;
    }}
    removeChild(child) {{
      const idx = this.children.indexOf(child);
      if (idx >= 0) this.children.splice(idx, 1);
      child.parentNode = null;
      return child;
    }}
    remove() {{
      if (this.parentNode) this.parentNode.removeChild(this);
    }}
    contains(target) {{
      let cur = target;
      while (cur) {{
        if (cur === this) return true;
        cur = cur.parentNode;
      }}
      return false;
    }}
    closest(sel) {{
      let cur = this;
      while (cur) {{
        if (typeof cur._matches === 'function' && cur._matches(sel)) return cur;
        cur = cur.parentNode;
      }}
      return null;
    }}
    hasAttribute(k) {{ return Object.prototype.hasOwnProperty.call(this._attrs, k); }}
    getAttribute(k) {{ return Object.prototype.hasOwnProperty.call(this._attrs, k) ? this._attrs[k] : null; }}
    setAttribute(k, v) {{
      this._attrs[k] = String(v);
      if (k === 'id') this.id = String(v);
    }}
    addEventListener(type, fn) {{
      if (!this._listeners[type]) this._listeners[type] = [];
      this._listeners[type].push(fn);
    }}
    dispatchEvent(ev) {{
      const fns = this._listeners[ev && ev.type] || [];
      for (const fn of fns) fn(ev);
      return true;
    }}
    click() {{
      this.dispatchEvent({{ type: 'click', preventDefault() {{}}, stopPropagation() {{}} }});
    }}
    _matches(sel) {{
      if (sel === '.chat-message[id^="bb_msg_id_"]') {{
        return this._classes.has('chat-message') && String(this.id).startsWith('bb_msg_id_');
      }}
      if (sel === '[data-id^="div_CX_"]') {{
        return String(this._attrs['data-id'] || '').startsWith('div_CX_');
      }}
      const exactAttr = sel.match(/^\[([a-zA-Z0-9_-]+)="([^"]+)"\]$/);
      if (exactAttr) {{
        return this._attrs[exactAttr[1]] === exactAttr[2];
      }}
      if (sel.startsWith('#')) return this.id === sel.slice(1);
      if (sel.startsWith('.')) {{
        return sel.slice(1).split('.').filter(Boolean).every((c) => this._classes.has(c));
      }}
      return false;
    }}
    querySelectorAll(sel) {{
      const parts = sel.split(',').map((s) => s.trim()).filter(Boolean);
      const out = [];
      const walk = (node) => {{
        for (const ch of node.children) {{
          if (parts.some((p) => ch._matches(p))) out.push(ch);
          walk(ch);
        }}
      }};
      walk(this);
      return out;
    }}
    querySelector(sel) {{
      return this.querySelectorAll(sel)[0] || null;
    }}
  }}

  const docRoot = new MockElement('html');
  const headEl = docRoot.appendChild(new MockElement('head'));
  const bodyEl = docRoot.appendChild(new MockElement('body'));

  // Left navigation sidebar (`#main-tab > div > .nav__tabs__top` + `.nav__tabs__bottom`)
  const mainTab = bodyEl.appendChild(new MockElement('nav'));
  mainTab.id = 'main-tab';
  const mainTabTopGroup = mainTab.appendChild(new MockElement('div'));
  const navZalo = mainTabTopGroup.appendChild(new MockElement('div'));
  navZalo.className = 'nav__tabs__zalo';
  const navTabsTop = mainTabTopGroup.appendChild(new MockElement('div'));
  navTabsTop.className = 'nav__tabs__top';
  const navTabsBottom = mainTab.appendChild(new MockElement('div'));
  navTabsBottom.className = 'nav__tabs__bottom';

  const msgView = bodyEl.appendChild(new MockElement('div'));
  msgView.id = 'messageView';

  const bubbleNormal = msgView.appendChild(new MockElement('div'));
  bubbleNormal.className = 'chat-message';
  bubbleNormal.id = 'bb_msg_id_m_500';
  const cardNormal = bubbleNormal.appendChild(new MockElement('div'));
  cardNormal.className = 'card';

  const bubbleUndo = msgView.appendChild(new MockElement('div'));
  bubbleUndo.className = 'chat-message';
  bubbleUndo.id = 'bb_msg_id_m_500';
  const cardUndo = bubbleUndo.appendChild(new MockElement('div'));
  cardUndo.className = 'card card--undo';

  // Chat bubble `"..."` context menu portal mounted under `document.body`
  const cxMenu = bodyEl.appendChild(new MockElement('div'));
  cxMenu.className = 'zmenu-body expand';
  const cxOuter = cxMenu.appendChild(new MockElement('div'));
  const cxList = cxOuter.appendChild(new MockElement('div'));
  const cxSelectWrap = cxList.appendChild(new MockElement('div'));
  cxSelectWrap.className = 'rel';
  const cxSelect = cxSelectWrap.appendChild(new MockElement('div'));
  cxSelect.className = 'zmenu-item';
  cxSelect.setAttribute('data-id', 'div_CX_Select');
  const cxSep = cxList.appendChild(new MockElement('div'));
  cxSep.className = 'zmenu-separator';
  const cxDelete = cxList.appendChild(new MockElement('div'));
  cxDelete.className = 'zmenu-item --DANGER';
  cxDelete.setAttribute('data-id', 'div_CX_Delete');

  // Call Settings modal (`Cài đặt` -> `Cài đặt cuộc gọi`) DOM container
  const callSettingsBottom = bodyEl.appendChild(new MockElement('div'));
  callSettingsBottom.className = 'setting--bottom';
  const notifySection = callSettingsBottom.appendChild(new MockElement('div'));
  notifySection.className = 'setting-section';
  const notifySwitch = notifySection.appendChild(new MockElement('div'));
  notifySwitch.id = 'settingNotifyCall';
  notifySwitch.setAttribute('data-id', 'idsettingNotifyCall');
  const camSection = callSettingsBottom.appendChild(new MockElement('div'));
  camSection.className = 'setting-section';

  const zstateEvents = [];
  globalThis.window = globalThis;
  globalThis.BroadcastChannel = MockBroadcastChannel;
  globalThis.MutationObserver = class {{
    constructor(cb) {{ this.cb = cb; }}
    observe() {{}}
    disconnect() {{}}
  }};
  globalThis.requestAnimationFrame = (cb) => setTimeout(cb, 0);
  globalThis.location = {{ protocol: 'zalo-app:', href: 'zalo-app://local/index.html', pathname: '/index.html' }};
  globalThis.document = {{
    readyState: 'complete',
    head: headEl,
    body: bodyEl,
    documentElement: docRoot,
    createElement: (tag) => new MockElement(tag),
    getElementById: (id) => docRoot.querySelector('#' + id),
    querySelector: (sel) => docRoot.querySelector(sel),
    querySelectorAll: (sel) => docRoot.querySelectorAll(sel),
    addEventListener() {{}},
  }};
  globalThis.addEventListener = () => {{}};
  globalThis.fetch = async () => new Response('{{}}', {{ status: 200 }});
  globalThis.__zaloNativeXHR = class {{
    open() {{}}
    send() {{ this.responseText = JSON.stringify({{ value: null }}); }}
  }};
  globalThis.webkit = {{
    messageHandlers: {{
      zlog: {{ postMessage() {{}} }},
      zstate: {{ postMessage(raw) {{ zstateEvents.push(typeof raw === 'string' ? JSON.parse(raw) : raw); }} }},
      __olaz_win_drag: {{ postMessage() {{}} }},
      zbridge: {{ postMessage() {{ return Promise.resolve(null); }} }},
    }},
  }};

  // Set up mock Redux store + message recall service in `__zaloRequire`
  let deletedFiles = 0;
  const recallService = {{
    deleteMessageResourceIfExists(m) {{
      deletedFiles += 1;
      return true;
    }},
    convertToRecalled(m) {{
      m.msgType = 20;
      m.message = '[Tin nhắn đã được thu hồi]';
      return m;
    }},
    mergeUndo(m) {{
      m.msgType = 20;
      m.message = '[Tin nhắn đã được thu hồi]';
      return m;
    }},
  }};

  const subscribers = new Set();
  const state = {{
    user: {{ userId: 'u_me_1', displayName: 'Khoa' }},
    conversations: {{
      alls: {{
        conv_1: {{
          messages: [
            {{
              msgId: 'm_500',
              cliMsgId: 'c_500',
              fromUid: 'u_friend_9',
              dName: 'Bạn Bè',
              msgType: 1,
              status: 0,
              sendDttm: 1762000123000,
              message: 'Tin nhắn gốc sẽ bị thu hồi',
              localPath: '/tmp/zalo/voice_500.aac',
              quote: {{ cliMsgType: 1, msg: 'Trích dẫn ban đầu' }},
            }},
            {{
              msgId: 'm_501',
              cliMsgId: 'c_501',
              fromUid: 'u_friend_9',
              dName: 'Bạn Bè',
              msgType: 1,
              status: 0,
              sendDttm: 1762000124000,
              message: 'Tin trả lời không bị thu hồi',
              quote: {{ cliMsgType: 1, msg: 'Câu được trích dẫn' }},
            }},
          ],
        }},
      }},
    }},
  }};
  const reduxStore = {{
    getState: () => state,
    subscribe: (fn) => {{ subscribers.add(fn); return () => subscribers.delete(fn); }},
    dispatch(action) {{
      if (action && action.type === 'SIMULATE_UNDO') {{
        const msg = state.conversations.alls.conv_1.messages[0];
        msg.msgType = 20;
        msg.status = -27;
        msg.message = '[Tin nhắn đã được thu hồi]';
        msg.quote = {{ cliMsgType: 20, msg: '' }};
        const replyMsg = state.conversations.alls.conv_1.messages[1];
        replyMsg.quote = {{ cliMsgType: 20, msg: '' }};
      }}
      for (const fn of subscribers) fn();
      return action;
    }},
  }};

  globalThis.__zaloRequire = {{
    c: {{
      1: {{ exports: reduxStore }},
      2: {{ exports: recallService }},
    }},
  }};

  const workerBcMessages = [];
  const workerBc = new MockBroadcastChannel('olaz_privacy');
  workerBc.onmessage = (ev) => workerBcMessages.push(ev.data);

  {bridge_spec}
  {local_modules}
  {direct_ui}
  {bridge_js}

  // Verify default `antiRecall === true` out-of-the-box + BroadcastChannel worker ping reply
  const defaultAntiRecallOnBoot = Boolean(window.__olazPrivacy && window.__olazPrivacy.antiRecall);
  workerBc.postMessage({{ type: 'request_privacy' }});
  const workerAntiRecallWhileOn = workerBcMessages[workerBcMessages.length - 1]?.anti_recall;
  const workerBcCountWhileOn = workerBcMessages.length;

  // 1. Verify `deleteMessageResourceIfExists` is suppressed while antiRecall=true
  await recallService.deleteMessageResourceIfExists({{ msgId: 'm_500' }});
  const deletedWhileOn = deletedFiles;

  // 2. Verify `convertToRecalled` marks `__olazRecalled` without clobbering text
  const sampleConvert = {{ msgId: 'm_conv', cliMsgId: 'c_conv', msgType: 1, message: 'Giữ nguyên chữ' }};
  const convertedOut = recallService.convertToRecalled(sampleConvert);

  // 3. Dispatch SIMULATE_UNDO on Redux store -> shadow cache restores original text, quote, localPath
  reduxStore.dispatch({{ type: 'SIMULATE_UNDO' }});
  const afterUndoMsg = state.conversations.alls.conv_1.messages[0];
  const afterUndoReply = state.conversations.alls.conv_1.messages[1];

  // Wait for `flushStoreSync` (250ms debounce)
  await new Promise((r) => setTimeout(r, 320));

  // 4. Run DOM pass and verify:
  //    - `.olaz-is-recalled`, `.olaz-recalled-badge`, `.olaz-recovered-text`
  //    - `"Giữ thu hồi"` `menuitemcheckbox` injected inside `.zmenu-body` before `div_CX_Delete`
  //    - `#olaz-sidebar-accounts` + `#olaz-sidebar-add-account` injected inside `#main-tab`
  window.__olazSetAccounts({{
    selfSlug: 'default',
    activeSlug: 'default',
    accounts: [
      {{ slug: 'default', name: 'Khv', initials: 'KH', unread: 0, shortcutIndex: 1 }},
      {{ slug: 'account-2', name: 'Shop Phụ', initials: 'SP', unread: 5, shortcutIndex: 2 }},
    ],
  }});
  window.__olazSetCallRecording({{
    dir: '/mnt/data/CallRecordings',
    defaultDir: '/home/user/.local/share/olaz/recordings',
    isCustom: true,
  }});
  window.__olazRefreshAntiRecallDom();

  const hasStyleTag = Boolean(document.getElementById('olaz-anti-recall-style'));
  const hasLegacyTitlebarTools = Boolean(document.getElementById('olaz-titlebar-tools'));
  const badgeOnNormal = cardNormal.querySelector('.olaz-recalled-badge');
  const recoveredOnUndo = cardUndo.querySelector('.olaz-recovered-text');
  const classWhileOn = bubbleNormal.classList.contains('olaz-is-recalled');

  const cxBtn = cxMenu.querySelector('.olaz-cx-antirecall');
  const cxRole = cxBtn ? cxBtn.getAttribute('role') : null;
  const cxCheckedInitial = cxBtn ? cxBtn.getAttribute('aria-checked') : null;
  const cxLabel = cxBtn && cxBtn.querySelector('.olaz-cx-label') ? cxBtn.querySelector('.olaz-cx-label').textContent : null;
  const cxBtnIdx = cxBtn ? cxList.children.indexOf(cxBtn) : -1;
  const cxDelIdx = cxList.children.indexOf(cxDelete);

  const sidebarWrap = document.getElementById('olaz-sidebar-accounts');
  const sidebarParentIsMainTop = Boolean(sidebarWrap && sidebarWrap.parentNode === mainTabTopGroup);
  const sidebarAddBtn = document.getElementById('olaz-sidebar-add-account');
  const sidebarOtherBtn = sidebarWrap ? sidebarWrap.querySelector('[data-account-slug="account-2"]') : null;
  const sidebarBadgeText = sidebarOtherBtn && sidebarOtherBtn.querySelector('.olaz-sidebar-badge')
    ? sidebarOtherBtn.querySelector('.olaz-sidebar-badge').textContent
    : null;
  if (sidebarOtherBtn) sidebarOtherBtn.click();
  if (sidebarAddBtn) sidebarAddBtn.click();

  const callRecSec = document.getElementById('olaz-call-recording-section');
  const callRecMountedAfterCamera = Boolean(
    callRecSec
    && callRecSec.parentNode === callSettingsBottom
    && callSettingsBottom.children.indexOf(callRecSec) > callSettingsBottom.children.indexOf(camSection)
  );
  const callRecPathText = document.getElementById('olaz-call-recording-path')?.textContent || null;
  const callRecOpenBtn = callRecSec ? callRecSec.querySelector('[data-action="open"]') : null;
  const callRecChangeBtn = callRecSec ? callRecSec.querySelector('[data-action="change"]') : null;
  const callRecResetBtn = callRecSec ? callRecSec.querySelector('[data-action="reset"]') : null;
  if (callRecOpenBtn) callRecOpenBtn.click();
  if (callRecChangeBtn) callRecChangeBtn.click();
  if (callRecResetBtn) callRecResetBtn.click();

  // 5. Click `"Giữ thu hồi"` in the `"..."` bubble menu -> toggles antiRecall OFF, updates aria-checked,
  //    removes badges & classes, emits `uiAction: 'toggleAntiRecall'`, and restores deleteMessageResourceIfExists!
  if (cxBtn) cxBtn.click();
  const cxCheckedAfterClick = cxBtn ? cxBtn.getAttribute('aria-checked') : null;
  const classAfterOff = bubbleNormal.classList.contains('olaz-is-recalled');
  const badgesAfterOff = document.querySelectorAll('.olaz-recalled-badge, .olaz-recovered-text').length;
  await recallService.deleteMessageResourceIfExists({{ msgId: 'm_500' }});
  const deletedAfterOff = deletedFiles;

  const uiActions = zstateEvents.filter((e) => e && e.uiAction).map((e) => e);
  const chatEvents = zstateEvents.filter((e) => e && e.chat && e.chat.convId === 'conv_1');
  const lastChatMsg = chatEvents.length ? chatEvents[chatEvents.length - 1].chat.messages[0] : null;

  console.log(JSON.stringify({{
    defaultAntiRecallOnBoot,
    workerBcCountWhileOn,
    workerAntiRecallWhileOn,
    workerAntiRecallAfterOff: workerBcMessages[workerBcMessages.length - 1]?.anti_recall,
    shadowHasMsgId: window.__olazMsgShadow.has('m_500'),
    shadowHasCliId: window.__olazMsgShadow.has('c_500'),
    deletedWhileOn,
    deletedAfterOff,
    convertedOut,
    afterUndoMsg,
    afterUndoReply,
    recalledIdRecorded: window.__olazRecalledIds.has('m_500') && window.__olazRecalledIds.has('c_500'),
    hasStyleTag,
    hasLegacyTitlebarTools,
    classWhileOn,
    badgeText: badgeOnNormal ? badgeOnNormal.textContent : null,
    recoveredText: recoveredOnUndo ? recoveredOnUndo.textContent : null,
    cxRole,
    cxCheckedInitial,
    cxCheckedAfterClick,
    cxLabel,
    cxBeforeDelete: cxBtnIdx > 0 && cxBtnIdx < cxDelIdx,
    sidebarParentIsMainTop,
    hasSidebarAddBtn: Boolean(sidebarAddBtn),
    sidebarBadgeText,
    callRecMountedAfterCamera,
    callRecPathText,
    uiActions,
    classAfterOff,
    badgesAfterOff,
    lastChatMsg,
  }}));
  process.exit(0);
}})();
"#,
        bridge_spec = find_script("bridge-spec"),
        local_modules = find_script("local-modules"),
        direct_ui = find_script("linux-direct-ui"),
        bridge_js = find_script("bridge"),
    );

    let res = run_node_json(&script);
    assert_eq!(res["defaultAntiRecallOnBoot"], true);
    assert!(res["workerBcCountWhileOn"].as_u64().unwrap() >= 2);
    assert_eq!(res["workerAntiRecallWhileOn"], true);
    assert_eq!(res["workerAntiRecallAfterOff"], false);
    assert_eq!(res["shadowHasMsgId"], true);
    assert_eq!(res["shadowHasCliId"], true);
    assert_eq!(res["deletedWhileOn"], 0);
    assert_eq!(res["deletedAfterOff"], 1);

    assert_eq!(res["convertedOut"]["msgType"], 1);
    assert_eq!(res["convertedOut"]["message"], "Giữ nguyên chữ");
    assert_eq!(res["convertedOut"]["__olazRecalled"], true);

    assert_eq!(res["afterUndoMsg"]["msgType"], 1);
    assert_eq!(
        res["afterUndoMsg"]["message"],
        "Tin nhắn gốc sẽ bị thu hồi"
    );
    assert_eq!(
        res["afterUndoMsg"]["localPath"],
        "/tmp/zalo/voice_500.aac"
    );
    assert_eq!(
        res["afterUndoMsg"]["quote"]["msg"],
        "Trích dẫn ban đầu"
    );
    assert_eq!(res["afterUndoMsg"]["__olazRecalled"], true);
    assert_eq!(
        res["afterUndoReply"]["quote"]["msg"],
        "Câu được trích dẫn"
    );
    assert_eq!(res["afterUndoReply"]["quote"]["__olazQuoteRecalled"], true);
    assert_eq!(res["recalledIdRecorded"], true);

    assert_eq!(res["hasStyleTag"], true);
    assert_eq!(res["hasLegacyTitlebarTools"], false);
    assert_eq!(res["classWhileOn"], true);
    assert_eq!(res["badgeText"], "Đã thu hồi");
    assert_eq!(res["recoveredText"], "Tin nhắn gốc sẽ bị thu hồi");
    assert_eq!(res["cxRole"], "menuitemcheckbox");
    assert_eq!(res["cxCheckedInitial"], "true");
    assert_eq!(res["cxCheckedAfterClick"], "false");
    assert_eq!(res["cxLabel"], "Giữ thu hồi");
    assert_eq!(res["cxBeforeDelete"], true);
    assert_eq!(res["sidebarParentIsMainTop"], true);
    assert_eq!(res["hasSidebarAddBtn"], true);
    assert_eq!(res["sidebarBadgeText"], "5");
    assert_eq!(res["callRecMountedAfterCamera"], true);
    assert_eq!(res["callRecPathText"], "/mnt/data/CallRecordings");
    assert_eq!(
        res["uiActions"],
        json!([
            { "uiAction": "queryCallRecordingsDir" },
            { "uiAction": "switchAccount", "slug": "account-2" },
            { "uiAction": "addAccount" },
            { "uiAction": "openCallRecordingsDir" },
            { "uiAction": "chooseCallRecordingsDir" },
            { "uiAction": "resetCallRecordingsDir" },
            { "uiAction": "toggleAntiRecall", "enabled": false },
        ])
    );
    assert_eq!(res["classAfterOff"], false);
    assert_eq!(res["badgesAfterOff"], 0);

    // `flushStoreSync` preserves text while marking `recalled: true` so autoreply ignores it!
    assert_eq!(
        res["lastChatMsg"]["text"],
        "Tin nhắn gốc sẽ bị thu hồi"
    );
    assert_eq!(res["lastChatMsg"]["recalled"], true);
}

// ============================================================================
// 9. Track C (Multi-Account): `ProfileRegistry` & Profile-Scoped `KeychainCipher`
// ============================================================================

#[test]
fn profile_registry_persistence_slug_allocation_discovery_and_legacy_migration() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("olaz");

    // 1. Seed a legacy split-brain `profiles/profile-2/share/olaz/ZaloData` directory AND a CLI `work` profile
    let legacy_zdata = root.join("profiles/profile-2/share/olaz/ZaloData");
    std::fs::create_dir_all(&legacy_zdata).unwrap();
    std::fs::write(legacy_zdata.join("Partitions.json"), b"{\"migrated\":true}").unwrap();

    let cli_work_zdata = root.join("profiles/work/ZaloData");
    std::fs::create_dir_all(&cli_work_zdata).unwrap();

    // 2. `load_or_discover` migrates legacy `share/olaz/ZaloData` -> `profiles/profile-2/ZaloData` and merges CLI profiles
    let mut reg = olaz_core::ProfileRegistry::load_or_discover(&root);
    assert!(
        root.join("profiles/profile-2/ZaloData/Partitions.json")
            .is_file(),
        "legacy split-brain ZaloData must be migrated into profiles/profile-2/ZaloData"
    );

    let slugs: Vec<String> = reg.profiles.iter().map(|p| p.slug.clone()).collect();
    assert!(slugs.contains(&"default".to_string()));
    assert!(slugs.contains(&"profile-2".to_string()));
    assert!(slugs.contains(&"work".to_string()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let reg_path = olaz_core::ProfileRegistry::registry_path(&root);
        assert!(reg_path.is_file());
        let mode = std::fs::metadata(&reg_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "profiles.json must be saved with 0600 permissions");
    }

    // 3. Allocate fresh sequential slugs (`account-2`, `account-3`)
    let s2 = reg.allocate_new_slug();
    assert_eq!(s2, "account-2");
    reg.ensure_profile(&s2).unwrap();
    let s3 = reg.allocate_new_slug();
    assert_eq!(s3, "account-3");
    reg.ensure_profile(&s3).unwrap();

    // 4. Identity update, duplicate-UID detection (`find_other_by_uid`), labels & initials
    assert!(reg.update_identity(
        "account-2",
        Some("zuid_888"),
        Some("Nguyễn Văn B"),
        Some("https://ava.zalo.me/b.jpg"),
    ));
    assert_eq!(
        reg.find_other_by_uid("account-3", "zuid_888")
            .map(|p| p.slug.as_str()),
        Some("account-2")
    );
    assert!(reg.find_other_by_uid("account-2", "zuid_888").is_none());

    let meta_acc2 = reg.profiles.iter().find(|p| p.slug == "account-2").unwrap();
    assert_eq!(meta_acc2.effective_name(), "Nguyễn Văn B");
    assert_eq!(meta_acc2.initials(), "NB");

    reg.set_label("account-2", Some("Shop Phụ".into()));
    reg.active_slug = "account-2".to_string();
    reg.save_to(&root).unwrap();

    let reloaded = olaz_core::ProfileRegistry::load_or_discover(&root);
    assert_eq!(reloaded.active_slug, "account-2");
    let reloaded_acc2 = reloaded
        .profiles
        .iter()
        .find(|p| p.slug == "account-2")
        .unwrap();
    assert_eq!(reloaded_acc2.effective_name(), "Shop Phụ");
    assert_eq!(reloaded_acc2.zalo_uid.as_deref(), Some("zuid_888"));
}

#[tokio::test]
async fn keychain_cipher_with_profile_isolates_accounts_and_keeps_default_backward_compatible() {
    // Subprocess branch for headless `LinuxSecretBackend::with_profile` keyfile isolation
    if std::env::var("OLAZ_SUBTEST_PROFILE_KEYFILE").as_deref() == Ok("1") {
        let dir = PathBuf::from(std::env::var("OLAZ_SUBTEST_DIR").unwrap());
        let c_def = KeychainCipher::with_profile(&dir, "default");
        let c_acc2 = KeychainCipher::with_profile(&dir, "account-2");

        let ct_def = c_def.encrypt("headless", &[b"default-secret"]).await.unwrap();
        let ct_acc2 = c_acc2.encrypt("headless", &[b"acc2-secret"]).await.unwrap();

        assert!(dir.join("keys/zprotocol.headless.key").is_file());
        assert!(
            dir.join("keys/profile.account-2.zprotocol.headless.key")
                .is_file()
        );
        assert!(c_acc2.decrypt("headless", &ct_def).await.is_err());
        assert_eq!(
            c_acc2.decrypt("headless", &ct_acc2).await.unwrap()[0],
            b"acc2-secret"
        );
        return;
    }

    // 1. In-memory shared `SecretBackend` verifying backward-compatible `"default"` attribute
    //    (`"zprotocol.<id>"`) vs isolated secondary profile (`"profile.<slug>.zprotocol.<id>"`).
    let shared_mem = Arc::new(MemorySecretBackend::new());
    let cipher_legacy = KeychainCipher::new(Arc::clone(&shared_mem));
    let cipher_default = KeychainCipher::new_with_profile(Arc::clone(&shared_mem), "default");
    let cipher_acc2 = KeychainCipher::new_with_profile(Arc::clone(&shared_mem), "account-2");

    assert_eq!(
        cipher_default.account_for_identifier("ZaloDB"),
        "zprotocol.zalodb"
    );
    assert_eq!(
        cipher_acc2.account_for_identifier("ZaloDB"),
        "profile.account-2.zprotocol.zalodb"
    );

    let sealed_legacy = cipher_legacy
        .encrypt("ZaloDB", &[b"legacy-default-db-key"])
        .await
        .unwrap();
    // `with_profile(..., "default")` is 100% compatible with existing `KeychainCipher::new` vaults!
    let opened_by_default = cipher_default
        .decrypt("ZaloDB", &sealed_legacy)
        .await
        .unwrap();
    assert_eq!(opened_by_default[0], b"legacy-default-db-key");

    // Secondary profile mints its own independent key and cannot read or overwrite `"default"`'s key!
    let sealed_acc2 = cipher_acc2
        .encrypt("ZaloDB", &[b"account-2-db-key"])
        .await
        .unwrap();
    assert!(matches!(
        cipher_acc2.decrypt("ZaloDB", &sealed_legacy).await,
        Err(VaultError::InvalidMac)
    ));
    assert_eq!(
        cipher_acc2.decrypt("ZaloDB", &sealed_acc2).await.unwrap()[0],
        b"account-2-db-key"
    );
    // And `"default"`'s key is still intact!
    assert_eq!(
        cipher_default
            .decrypt("ZaloDB", &sealed_legacy)
            .await
            .unwrap()[0],
        b"legacy-default-db-key"
    );

    // 2. Verify headless `LinuxSecretBackend::with_profile` file scoping in an isolated non-DBus subprocess
    let tmp = TempDir::new().unwrap();
    let exe = std::env::current_exe().unwrap();
    let status = Command::new(exe)
        .arg("--exact")
        .arg("keychain_cipher_with_profile_isolates_accounts_and_keeps_default_backward_compatible")
        .env("OLAZ_SUBTEST_PROFILE_KEYFILE", "1")
        .env("OLAZ_SUBTEST_DIR", tmp.path())
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/olaz-no-dbus.sock",
        )
        .status()
        .unwrap();
    assert!(
        status.success(),
        "profile-scoped headless keyfile subprocess test failed"
    );
}


