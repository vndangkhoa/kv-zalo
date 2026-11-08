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

/// Executes a JavaScript snippet with `node -e` and parses stdout as JSON.
fn run_node_json(script: &str) -> Value {
    let out = Command::new("node")
        .arg("-e")
        .arg(script)
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
    assert!(ui_str.contains(r#""zfile://local/"+e.replace(/^file:\/+/,""):e"#));
    // Second pass is strictly idempotent (`None`)
    assert!(patch_zalo_linux_renderer_bundle(&ui_patched).is_none());
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
    let mut preserved_client_type_23 = false;
    let mut patched_files = 0usize;

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
}
