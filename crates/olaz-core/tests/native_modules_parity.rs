//! Comprehensive parity tests for `file_utilities`, `zwalker`, `db_cross` (`ZDB4.0` +
//! `parseBinNet`), `file_utils_get_disk_usage`, `zfile_get_disk_info`, `zfile_get_info`,
//! and `generate_video_thumbnail_sync` (640px default, even dimensions, `http://` URLs).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use aes::Aes256;
use cbc::Encryptor;
use cipher::block_padding::NoPadding;
use cipher::{BlockEncryptMut, KeyIvInit};
use olaz_core::{
    cancel_job, decompress_and_decrypt_db_v1, decompress_and_decrypt_db_v2, derive_zdb4_key,
    detect_filesystem, detect_hardlinks_in_scope, detect_hardlinks_info, dispatch_db_utils_async,
    dispatch_db_utils_sync, dispatch_file_utilities_async, dispatch_file_utilities_sync,
    file_utils_get_disk_usage, get_directory_size, get_directory_size_by_glob,
    get_directory_size_tree, handle_zwalker_ipc_with_state, parse_bin_net, register_job,
    unregister_job, wire, zfile_get_disk_info, zfile_get_info, FsBridge, PlatformPaths,
    ReferenceUpdate, ZaloPlatform, ZWalkerState,
};
use serde_json::{json, Value};
use tempfile::TempDir;

type Aes256CbcEnc = Encryptor<Aes256>;

fn assert_future_send<F: std::future::Future + Send>(_: F) {}

#[test]
fn async_dispatchers_return_send_futures() {
    let tmp = TempDir::new().unwrap();
    let platform = Arc::new(ZaloPlatform::with_root(tmp.path(), "26.10.20"));
    let fsb = FsBridge::new(Arc::clone(&platform));
    let dyn_plat: &dyn PlatformPaths = &*platform;
    let empty_args: Vec<Value> = Vec::new();

    assert_future_send(dispatch_file_utilities_async(
        dyn_plat,
        "cancelJob",
        &empty_args,
    ));
    assert_future_send(dispatch_db_utils_async(
        dyn_plat,
        "decompressAndDecryptDb",
        &empty_args,
    ));
    assert_future_send(fsb.dispatch_async(
        "$znode.nativelibs.fileUtilitiesAddon.cancelJob",
        &empty_args,
    ));
}

// ============================================================================
// 1. `file_utilities` — All 11 Exports + `options.deep` + Scoped Hardlinks
// ============================================================================

#[tokio::test]
async fn file_utilities_all_11_exports_deep_tree_hardlink_dedup_glob_and_cancellation() {
    let tmp = TempDir::new().unwrap();
    let platform = Arc::new(ZaloPlatform::with_root(tmp.path().join("app"), "26.10.20"));
    let fsb = FsBridge::new(Arc::clone(&platform));

    let root = tmp.path().join("scan-root");
    let res_dir = root.join("resource");
    let res_nested = res_dir.join("stickers");
    let cache_dir = root.join("cache");
    std::fs::create_dir_all(&res_nested).unwrap();
    std::fs::create_dir_all(&cache_dir).unwrap();

    let file_a = root.join("a.bin");
    let file_b = res_dir.join("b.txt");
    let file_c = res_nested.join("c.txt");
    let file_d = cache_dir.join("d.bin");
    std::fs::write(&file_a, vec![0xAA; 1000]).unwrap();
    std::fs::write(&file_b, vec![0xBB; 500]).unwrap();
    std::fs::write(&file_c, vec![0xCC; 300]).unwrap();
    std::fs::write(&file_d, vec![0xDD; 2000]).unwrap();

    // Hardlink of `a.bin` inside `resource/` — must be deduplicated by `(dev, ino)`!
    let hardlink_a = res_dir.join("a_hardlink.bin");
    std::fs::hard_link(&file_a, &hardlink_a).unwrap();

    // Symlinks to a file and an external directory — must be skipped completely!
    let external = tmp.path().join("external");
    std::fs::create_dir_all(&external).unwrap();
    std::fs::write(external.join("outside.bin"), vec![0xEE; 9999]).unwrap();
    symlink(&file_b, root.join("b_symlink.txt")).unwrap();
    symlink(&external, root.join("ext_dir_symlink")).unwrap();

    // Unique regular files = a.bin(1000) + b.txt(500) + c.txt(300) + d.bin(2000) = 3800 bytes, 4 files.
    let s = get_directory_size(&root, None, None).unwrap();
    assert_eq!(s.total_size, 3800.0);
    assert_eq!(s.file_count, 4);
    assert!(s.duration_ms >= 0.0);

    // Error checks on `get_directory_size`
    assert!(get_directory_size(&root, Some(0), None)
        .unwrap_err()
        .contains("Worker count must be greater than 0"));
    assert!(get_directory_size(&root.join("missing"), None, None)
        .unwrap_err()
        .contains("does not exist"));
    assert!(get_directory_size(&file_a, None, None)
        .unwrap_err()
        .contains("is not a directory"));

    // 1 & 2: `getDirectorySizeSync` + `getDirectorySizeAsync` via `FsBridge`
    let sync_val = fsb
        .dispatch_sync(
            "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeSync",
            &[json!(root.to_string_lossy()), json!({ "workers": 4 })],
        )
        .unwrap();
    assert_eq!(sync_val["totalSize"].as_f64(), Some(3800.0));
    assert_eq!(sync_val["fileCount"].as_u64(), Some(4));

    let async_val = fsb
        .dispatch_async(
            "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeAsync",
            &[json!(root.to_string_lossy()), Value::Null, json!(101)],
        )
        .await
        .unwrap();
    assert_eq!(async_val["totalSize"].as_f64(), Some(3800.0));
    assert_eq!(async_val["fileCount"].as_u64(), Some(4));

    // `options.deep` (`pc-dist`: `A.tree?.find(e => e?.relativePath === "resource")?.totalSize`)
    let deep_val = fsb
        .dispatch_async(
            "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeAsync",
            &[
                json!(root.to_string_lossy()),
                json!({ "deep": { "maxDepth": 1 } }),
                json!(102),
            ],
        )
        .await
        .unwrap();
    assert_eq!(deep_val["totalSize"].as_f64(), Some(3800.0));
    assert_eq!(deep_val["fileCount"].as_u64(), Some(4));
    let tree_arr = deep_val["tree"].as_array().expect("deep result must include tree array");
    let res_entry = tree_arr
        .iter()
        .find(|e| e["relativePath"] == "resource")
        .expect("tree must contain relativePath == 'resource'");
    // `a.bin` (sorted before `resource`) claims the hardlinked inode first, OR `resource` claims it:
    // Because `a.bin` < `resource` lexicographically, `a.bin` is visited first at depth 0, so
    // `resource` has `b.txt(500) + stickers/c.txt(300) = 800`, and `totalSize == size == 800`!
    assert_eq!(res_entry["size"].as_f64(), Some(800.0));
    assert_eq!(res_entry["totalSize"].as_f64(), Some(800.0));
    assert_eq!(res_entry["fileCount"].as_u64(), Some(2));
    assert_eq!(res_entry["children"].as_array().unwrap().len(), 0);

    // `options.deep` with `includeRoot: true` and `maxDepth: 2`
    let deep_with_root = fsb
        .dispatch_sync(
            "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeSync",
            &[
                json!(root.to_string_lossy()),
                json!({ "deep": { "maxDepth": 2, "includeRoot": true } }),
            ],
        )
        .unwrap();
    let tree_root_arr = deep_with_root["tree"].as_array().unwrap();
    assert_eq!(tree_root_arr[0]["depth"].as_u64(), Some(0));
    assert_eq!(tree_root_arr[0]["relativePath"].as_str(), Some(""));
    assert_eq!(tree_root_arr[0]["totalSize"].as_f64(), Some(3800.0));
    assert!(tree_root_arr
        .iter()
        .any(|e| e["relativePath"] == "resource/stickers" && e["totalSize"] == 300.0));

    // 3 & 4: `getDirectorySizeTreeSync` + `getDirectorySizeTreeAsync`
    assert!(get_directory_size_tree(&root, -1, true, None)
        .unwrap_err()
        .contains("max_depth must be >= 0"));
    let tree0 = fsb
        .dispatch_sync(
            "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeTreeSync",
            &[json!(root.to_string_lossy()), json!({ "maxDepth": 0 })],
        )
        .unwrap();
    assert_eq!(tree0["depth"].as_u64(), Some(0));
    assert_eq!(tree0["size"].as_f64(), Some(3800.0));
    assert_eq!(tree0["totalSize"].as_f64(), Some(3800.0));
    assert_eq!(tree0["children"].as_array().unwrap().len(), 0);

    let tree2 = fsb
        .dispatch_async(
            "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeTreeAsync",
            &[json!(root.to_string_lossy()), json!({ "maxDepth": 2 }), json!(103)],
        )
        .await
        .unwrap();
    assert_eq!(tree2["totalSize"].as_f64(), Some(3800.0));
    assert_eq!(tree2["children"].as_array().unwrap().len(), 2);

    // 5 & 6: `getDirectorySizeByGlobSync` + `getDirectorySizeByGlobAsync`
    let pat_bin = format!("{}/*.bin", root.display());
    let glob_bin = fsb
        .dispatch_sync(
            "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeByGlobSync",
            &[json!(pat_bin)],
        )
        .unwrap();
    // `a.bin`(1000) + `cache/d.bin`(2000) + `resource/a_hardlink.bin`(deduped!) = 3000, 2 files
    assert_eq!(glob_bin["totalSize"].as_f64(), Some(3000.0));
    assert_eq!(glob_bin["fileCount"].as_u64(), Some(2));

    // Brace pattern `{resource,cache}/**/*.txt` extracts literal prefix `root` before `{`
    let pat_brace = format!("{}/{{resource,cache}}/**/*.txt", root.display());
    let glob_txt = fsb
        .dispatch_async(
            "$znode.nativelibs.fileUtilitiesAddon.getDirectorySizeByGlobAsync",
            &[json!(pat_brace), Value::Null, json!(104)],
        )
        .await
        .unwrap();
    assert_eq!(glob_txt["totalSize"].as_f64(), Some(800.0));
    assert_eq!(glob_txt["fileCount"].as_u64(), Some(2));

    // Non-existent glob base dir returns 0 rather than erroring
    let missing_glob = get_directory_size_by_glob(
        &format!("{}/no-such-base/*.bin", root.display()),
        None,
    )
    .unwrap();
    assert_eq!(missing_glob.total_size, 0.0);
    assert_eq!(missing_glob.file_count, 0);

    // Invalid glob pattern errors
    assert!(get_directory_size_by_glob("[unclosed", None)
        .unwrap_err()
        .contains("Invalid glob pattern"));

    // 7 & 8: `detectHardlinksSync` + `detectHardlinksAsync` (1-arg info AND 2-arg scope scan!)
    let hl_info_linked = detect_hardlinks_info(&file_a).unwrap();
    assert!(hl_info_linked.is_hardlink);
    assert_eq!(hl_info_linked.link_count, 2);

    let hl_info_single = fsb
        .dispatch_async(
            "$znode.nativelibs.fileUtilitiesAddon.detectHardlinksAsync",
            &[json!(file_b.to_string_lossy())],
        )
        .await
        .unwrap();
    assert_eq!(hl_info_single["isHardlink"].as_bool(), Some(false));
    assert_eq!(hl_info_single["linkCount"].as_u64(), Some(1));

    // 2-arg scoped hardlink scan throws `"File has only one link"` when nlink <= 1
    let single_err = detect_hardlinks_in_scope(&file_b, &root).unwrap_err();
    assert_eq!(single_err, "File has only one link");
    let async_single_err = fsb
        .dispatch_async(
            "$znode.nativelibs.fileUtilitiesAddon.detectHardlinksAsync",
            &[json!(file_b.to_string_lossy()), json!(root.to_string_lossy())],
        )
        .await
        .unwrap_err();
    assert_eq!(async_single_err, "File has only one link");

    // 2-arg scoped scan on `file_a` returns both `a.bin` and `resource/a_hardlink.bin` (skipping symlinks)
    let scoped_links = fsb
        .dispatch_async(
            "$znode.nativelibs.fileUtilitiesAddon.detectHardlinksAsync",
            &[json!(file_a.to_string_lossy()), json!(root.to_string_lossy())],
        )
        .await
        .unwrap();
    let expected_links = json!([
        file_a.to_string_lossy().to_string(),
        hardlink_a.to_string_lossy().to_string(),
    ]);
    assert_eq!(scoped_links, expected_links);

    // 9 & 10: `detectFilesystemSync` + `detectFilesystemAsync`
    let fs_info = detect_filesystem(&root).unwrap();
    assert!(!fs_info.filesystem_type.is_empty());
    assert_eq!(
        fs_info.filesystem_type,
        fs_info.filesystem_type.to_ascii_uppercase()
    );
    assert!(fs_info.max_filename_length > 0);
    assert!(fs_info.supports_unicode_filenames);

    let fs_async = fsb
        .dispatch_async(
            "$znode.nativelibs.fileUtilitiesAddon.detectFilesystemAsync",
            &[json!(root.to_string_lossy())],
        )
        .await
        .unwrap();
    assert_eq!(
        fs_async["filesystemType"].as_str(),
        Some(fs_info.filesystem_type.as_str())
    );

    // 11: `cancelJob` + `JOBS` registry lifecycle
    let flag = register_job(77_777);
    assert!(!flag.load(Ordering::SeqCst));
    let cancel_ret = fsb
        .dispatch_sync(
            "$znode.nativelibs.fileUtilitiesAddon.cancelJob",
            &[json!(77_777)],
        )
        .unwrap();
    assert!(wire::is_undefined(&cancel_ret));
    assert!(flag.load(Ordering::SeqCst));

    let aborted = get_directory_size(&root, None, Some(&flag)).unwrap();
    assert_eq!(aborted.file_count, 0);
    assert_eq!(aborted.total_size, 0.0);
    unregister_job(77_777);

    // `cancelJob` on unknown ID is a safe no-op
    cancel_job(999_999_999);
    assert!(dispatch_file_utilities_sync(&*platform, "cancelJob", &[json!(999_999_999)]).is_ok());
}

// ============================================================================
// 2. `zwalker` — Full 5-Method Lifecycle + `main.js` IPC Shapes + Sandbox
// ============================================================================

#[test]
fn zwalker_lifecycle_always_deletes_homeless_and_matches_main_js_ipc_shapes() {
    let tmp = TempDir::new().unwrap();
    let platform = Arc::new(ZaloPlatform::with_root(tmp.path().join("app"), "26.10.20"));
    let state = ZWalkerState::new();

    // Fixture inside `platform.user_data()` (writable sandbox)
    let dl_root = platform.user_data().join("ZaloDownloads");
    let cache_dir = dl_root.join("Cache");
    let zcloud_dir = dl_root.join("zcloud");
    let protected_dir = dl_root.join("protected");
    let empty_nested = dl_root.join("empty/a/b");
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::create_dir_all(&zcloud_dir).unwrap();
    std::fs::create_dir_all(&protected_dir).unwrap();
    std::fs::create_dir_all(&empty_nested).unwrap();

    let kept_1 = cache_dir.join("a.jpg");
    let kept_2 = cache_dir.join(".hidden_thumb.jpg"); // dotfile must NOT be skipped!
    let homeless_1 = zcloud_dir.join("orphan.mp4");
    let homeless_2 = dl_root.join("loose.dat");
    let ignored_homeless = protected_dir.join("keep_me.bin");

    std::fs::write(&kept_1, vec![1u8; 100]).unwrap();
    std::fs::write(&kept_2, vec![2u8; 200]).unwrap();
    std::fs::write(&homeless_1, vec![3u8; 400]).unwrap();
    std::fs::write(&homeless_2, vec![4u8; 50]).unwrap();
    std::fs::write(&ignored_homeless, vec![5u8; 150]).unwrap();

    let root_str = dl_root.to_string_lossy().into_owned();
    let cache_glob = format!("{}/Cache/**", root_str);
    let zcloud_glob = format!("{}/zcloud/**", root_str);
    let protected_glob = format!("{}/protected/**", root_str);
    let all_glob = "**".to_string();
    let tracking = vec![cache_glob.clone(), zcloud_glob.clone(), all_glob.clone()];
    let ignore = vec![protected_glob];

    // 1. `"zwalker-collect-dir-stats"` -> `{ fileNumber, size, trackingFolderData: <object> }`
    let collect = handle_zwalker_ipc_with_state(
        &state,
        &*platform,
        "zwalker-collect-dir-stats",
        &[json!(root_str), json!(tracking)],
    )
    .unwrap();
    assert_eq!(collect["fileNumber"].as_u64(), Some(5));
    assert_eq!(collect["size"].as_u64(), Some(900));
    assert_eq!(
        collect["trackingFolderData"][&cache_glob]["file_number"].as_u64(),
        Some(2)
    );
    assert_eq!(
        collect["trackingFolderData"][&cache_glob]["size"].as_u64(),
        Some(300)
    );
    assert_eq!(
        collect["trackingFolderData"][&zcloud_glob]["size"].as_u64(),
        Some(400)
    );

    // 2. `"zwalker-update-file-stats"` -> `{ updateCount }` (renamed from `fileNumber`!)
    let upd = handle_zwalker_ipc_with_state(
        &state,
        &*platform,
        "zwalker-update-file-stats",
        &[
            json!(root_str),
            json!([
                { "filePath": kept_1.to_string_lossy(), "id": "msg_1" },
                { "filePath": kept_2.to_string_lossy(), "id": "msg_2" },
                { "filePath": dl_root.join("nonexistent.bin").to_string_lossy(), "id": "msg_x" },
            ]),
        ],
    )
    .unwrap();
    assert_eq!(upd, json!({ "updateCount": 2 }));

    // 3. `"zwalker-stat-unmarked-files"` -> `{ fileNumber, size, trackingFolderData, trackingATime }`
    let stat = handle_zwalker_ipc_with_state(
        &state,
        &*platform,
        "zwalker-stat-unmarked-files",
        &[
            json!(root_str),
            json!(ignore),
            json!(tracking),
            json!([604800, 259200, 1209600]),
        ],
    )
    .unwrap();
    // Homeless & not ignored = `orphan.mp4` (400) + `loose.dat` (50) = 450 bytes, 2 files.
    assert_eq!(stat["fileNumber"].as_u64(), Some(2));
    assert_eq!(stat["size"].as_u64(), Some(450));
    let buckets = stat["trackingATime"][&all_glob].as_array().unwrap();
    assert_eq!(buckets.len(), 4);
    assert_eq!(buckets[0]["file_number"].as_u64(), Some(2));
    assert_eq!(buckets[0]["size"].as_u64(), Some(450));

    // 4. `"zwalker-del-unmarked-files"` with `deleteStatCache = false`:
    //    CRITICAL: must STILL unlink `orphan.mp4` and `loose.dat`!
    let del = handle_zwalker_ipc_with_state(
        &state,
        &*platform,
        "zwalker-del-unmarked-files",
        &[
            json!(root_str),
            json!(ignore),
            json!(tracking),
            json!(false),
        ],
    )
    .unwrap();
    assert_eq!(del["fileNumber"].as_u64(), Some(2));
    assert_eq!(del["size"].as_u64(), Some(450));
    assert_eq!(del["failedFileNumber"].as_u64(), Some(0));
    assert_eq!(del["failedSize"].as_u64(), Some(0));
    assert_eq!(
        del["trackingFolderData"][&zcloud_glob]["file_number"].as_u64(),
        Some(1)
    );
    assert!(!homeless_1.exists(), "homeless orphan.mp4 must be deleted even when deleteStatCache=false");
    assert!(!homeless_2.exists(), "homeless loose.dat must be deleted even when deleteStatCache=false");
    assert!(kept_1.exists(), "marked file must survive");
    assert!(kept_2.exists(), "marked hidden file must survive");
    assert!(ignored_homeless.exists(), "ignored file must survive");

    // TreeState was updated in place -> re-statting shows 0 unmarked files left.
    let stat_after = state.stat_unmarked_files(&root_str, &ignore, &tracking, &[259200]);
    assert_eq!(stat_after.file_number, 0);
    assert_eq!(stat_after.size, 0);

    // Also verify `delete_homeless_files` with `None` unlinks homeless files when `ignore` is empty.
    let del_none = state.delete_homeless_files(&root_str, &[], &tracking, None);
    assert_eq!(del_none.file_number, 1);
    assert_eq!(del_none.size, 150);
    assert!(!ignored_homeless.exists());

    // 5. `"zwalker-delete-empty-folders"` -> `{ deletedCount, deletedDirs }` deepest-first
    let emp = handle_zwalker_ipc_with_state(
        &state,
        &*platform,
        "zwalker-delete-empty-folders",
        &[json!(root_str)],
    )
    .unwrap();
    // `empty/a/b`, `empty/a`, `empty`, `zcloud`, and `protected` are all empty now (5 dirs)!
    assert_eq!(emp["deletedCount"].as_u64(), Some(5));
    assert!(!dl_root.join("empty").exists());
    assert!(!zcloud_dir.exists());
    assert!(cache_dir.exists(), "non-empty Cache dir must remain");
    assert!(dl_root.exists(), "root directory itself must never be deleted");

    // 6. Sandbox confinement: IPC refuses deletion outside `platform` writable roots!
    let outside_dir = tmp.path().join("outside-secret");
    std::fs::create_dir_all(outside_dir.join("empty_sub")).unwrap();
    let outside_file = outside_dir.join("secret.txt");
    std::fs::write(&outside_file, b"do not delete").unwrap();

    // Populate state manually and attempt IPC delete on `outside_dir` -> EACCES!
    let _ = state.scan_directory(&outside_dir.to_string_lossy(), &[]);
    let err_del = handle_zwalker_ipc_with_state(
        &state,
        &*platform,
        "zwalker-del-unmarked-files",
        &[json!(outside_dir.to_string_lossy()), json!([]), json!([])],
    )
    .unwrap_err();
    assert!(err_del.contains("EACCES"));
    assert!(outside_file.exists(), "outside file must never be deleted via IPC");

    let err_emp = handle_zwalker_ipc_with_state(
        &state,
        &*platform,
        "zwalker-delete-empty-folders",
        &[json!(outside_dir.to_string_lossy())],
    )
    .unwrap_err();
    assert!(err_emp.contains("EACCES"));
    assert!(outside_dir.join("empty_sub").exists());

    let _ = ReferenceUpdate {
        file_path: String::new(),
        id: String::new(),
    };
}

// ============================================================================
// 3. `db_cross` — `ZDB4.0` 64KiB-Chunked AES-256-CBC + XZ + `parseBinNet`
// ============================================================================

fn build_synthetic_zdb4_archive(
    entries: &[(&str, &[u8])],
    private_key_upper: &str,
    pad_xz_with_trailing_zeros: usize,
) -> Vec<u8> {
    let mut raw_concat = Vec::new();
    for &(_, content) in entries {
        raw_concat.extend_from_slice(content);
    }

    let mut xz_stream = Vec::new();
    lzma_rs::xz_compress(&mut std::io::Cursor::new(&raw_concat), &mut xz_stream).unwrap();
    xz_stream.extend(std::iter::repeat_n(0u8, pad_xz_with_trailing_zeros));

    let mut pt = Vec::new();
    pt.extend_from_slice(b"ZDB4.0");
    pt.extend_from_slice(&[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]); // 8-byte meta
    pt.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for &(name, content) in entries {
        let nb = name.as_bytes();
        pt.extend_from_slice(&(nb.len() as u32).to_be_bytes());
        pt.extend_from_slice(nb);
        pt.extend_from_slice(&(content.len() as u32).to_be_bytes());
    }
    pt.extend_from_slice(&xz_stream);

    // Pad total plaintext to 16-byte AES block multiple (matching shipped ZDB4.0 files).
    if pt.len() % 16 != 0 {
        let pad = 16 - (pt.len() % 16);
        pt.extend(std::iter::repeat_n(0u8, pad));
    }

    // Encrypt in 64 KiB (65536-byte) chunks, resetting IV to [0u8; 16] on EVERY chunk!
    let key = derive_zdb4_key(private_key_upper);
    let mut ct = pt;
    for chunk in ct.chunks_mut(65_536) {
        let aligned = (chunk.len() / 16) * 16;
        if aligned > 0 {
            let iv = [0u8; 16];
            Aes256CbcEnc::new((&key).into(), (&iv).into())
                .encrypt_padded_mut::<NoPadding>(&mut chunk[..aligned], aligned)
                .unwrap();
        }
    }
    ct
}

fn make_pseudo_sqlite_payload(seed: u64, size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"SQLite format 3\0");
    // Incompressible xorshift64* stream so compressed XZ archive exceeds 2 * 64 KiB (> 131 KiB),
    // exercising multiple 64 KiB IV-reset chunks!
    let mut state = seed | 1;
    while out.len() < size {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let word = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        for b in word.to_le_bytes() {
            if out.len() < size {
                out.push(b);
            }
        }
    }
    out
}

#[tokio::test]
async fn zdb4_multi_chunk_decrypt_xz_extract_sqlite_verify_and_traversal_guard() {
    let tmp = TempDir::new().unwrap();
    let platform = Arc::new(ZaloPlatform::with_root(tmp.path().join("app"), "26.10.20"));
    let fsb = FsBridge::new(Arc::clone(&platform));

    let db1 = make_pseudo_sqlite_payload(0x1234_5678, 150_000);
    let db2 = make_pseudo_sqlite_payload(0x9ABC_DEF0, 24_000);
    let meta_json = br#"{"backupVersion":4,"uid":"10001"}"#.to_vec();

    // Pass lowercase private key to decryptor while encrypting with mixed/upper key (`derive_zdb4_key` uppercases).
    let raw_archive = build_synthetic_zdb4_archive(
        &[
            ("zalo_msg.db", &db1),
            ("sub/group_msg.db", &db2),
            ("meta.json", &meta_json),
        ],
        "zaloPrivateKeyAbCdEf123456",
        7, // non-zero trailing bytes before AES alignment to test XZ trailing-padding tolerance!
    );
    assert!(
        raw_archive.len() > 2 * 65_536,
        "archive ({}) must span >= 3 64KiB chunks to verify per-chunk IV reset",
        raw_archive.len()
    );

    let archive_path = tmp.path().join("backup.zdb4");
    std::fs::write(&archive_path, &raw_archive).unwrap();

    let out_dir = platform.user_data().join("restored_db");
    let res = fsb
        .dispatch_async(
            "$znode.nativelibs.dbUtilsAddon.decompressAndDecryptDb_V2",
            &[
                json!(archive_path.to_string_lossy()),
                json!(out_dir.to_string_lossy()),
                json!("zaloprivatekeyabcdef123456"),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        res,
        json!({
            "result": 0,
            "inner_error": "",
            "error_message": "",
        })
    );
    assert_eq!(std::fs::read(out_dir.join("zalo_msg.db")).unwrap(), db1);
    assert_eq!(
        std::fs::read(out_dir.join("sub/group_msg.db")).unwrap(),
        db2
    );
    assert_eq!(std::fs::read(out_dir.join("meta.json")).unwrap(), meta_json);

    // Wrong key -> negative `result`, never panics
    let bad_key_res = decompress_and_decrypt_db_v2(
        &archive_path,
        &platform.user_data().join("bad_key_out"),
        "wrong_key_value",
    );
    assert!(bad_key_res["result"].as_i64().unwrap() < 0);

    // Path traversal (`../evil.db` and `/etc/evil.db`) -> rejected before writing anything!
    for bad_name in ["../evil.db", "sub/../../evil.db", "/tmp/evil.db", "..\\evil.db"] {
        let evil_bytes = build_synthetic_zdb4_archive(&[(bad_name, &db2)], "KEY123", 0);
        let evil_in = tmp.path().join("evil.zdb4");
        std::fs::write(&evil_in, &evil_bytes).unwrap();
        let evil_out = platform.user_data().join("evil_out");
        let r = decompress_and_decrypt_db_v2(&evil_in, &evil_out, "key123");
        assert_eq!(r["result"].as_i64(), Some(-3), "must reject {bad_name}: {r}");
        assert!(!platform.user_data().join("evil.db").exists());
    }

    // `.db` entry without `"SQLite format 3\0"` header -> rejected with `-14`!
    let fake_db_archive = build_synthetic_zdb4_archive(
        &[("corrupt.db", b"NOT_A_SQLITE_DATABASE_HEADER_0000")],
        "KEY123",
        0,
    );
    let fake_in = tmp.path().join("fake.zdb4");
    std::fs::write(&fake_in, &fake_db_archive).unwrap();
    let fake_out = platform.user_data().join("fake_out");
    let fake_res = decompress_and_decrypt_db_v2(&fake_in, &fake_out, "key123");
    assert_eq!(fake_res["result"].as_i64(), Some(-14));
    assert!(!fake_out.join("corrupt.db").exists());

    // Sandbox check on `dispatch_db_utils_sync`: outside `userData` rejected with `-10`!
    let outside_out = tmp.path().join("outside_restore");
    let sb_res = dispatch_db_utils_sync(
        &*platform,
        "decompressAndDecryptDb_V2",
        &[
            json!(archive_path.to_string_lossy()),
            json!(outside_out.to_string_lossy()),
            json!("zaloprivatekeyabcdef123456"),
        ],
    )
    .unwrap();
    assert_eq!(sb_res["result"].as_i64(), Some(-10));
    assert!(!outside_out.exists());

    // V1 stub check
    let v1 = decompress_and_decrypt_db_v1(&archive_path, &out_dir, "k");
    assert_eq!(
        v1,
        json!({
            "result": -1,
            "err_message": "V1 format not fully implemented in this build",
        })
    );
}

fn encode_tlvbox_u16(entries: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let body_len: usize = entries.iter().map(|(_, v)| 6 + v.len()).sum();
    let total_bytes = (4 + body_len) as u32;
    let mut out = Vec::with_capacity(4 + body_len);
    out.extend_from_slice(&total_bytes.to_le_bytes());
    for (tag, val) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&(val.len() as u32).to_le_bytes());
        out.extend_from_slice(val);
    }
    out
}

fn encode_tlv32_le(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (tag, val) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&(val.len() as u32).to_le_bytes());
        out.extend_from_slice(val);
    }
    out
}

#[test]
fn parse_bin_net_decodes_both_canonical_tlvbox_and_db_cross_v4_framings() {
    let tmp = TempDir::new().unwrap();
    let platform = ZaloPlatform::with_root(tmp.path(), "26.10.20");
    let fsb = FsBridge::new(Arc::new(platform));

    // A. Canonical `[u32 LE total_bytes] + [u16 LE tag][u32 LE len][val]` (`Property`=1,
    //    `Attachs`=2, `Quote`=3, `Mentions`=4, `quoteStatus`=5, `dataSource`=6, `data`=7).
    let prop_box = encode_tlvbox_u16(&[
        (1, 4096_i32.to_le_bytes().to_vec()), // size
        (2, 2_i32.to_le_bytes().to_vec()),    // subType
        (3, 7_i32.to_le_bytes().to_vec()),    // type
        (4, b"pdf".to_vec()),                 // ext
        (5, 0xFF00AA_i32.to_le_bytes().to_vec()), // color
    ]);
    let attach_box = encode_tlvbox_u16(&[
        (1, b"photo".to_vec()),
        (2, 12_i32.to_le_bytes().to_vec()),
        (3, 99_i32.to_le_bytes().to_vec()),
        (8, b"Architecture Spec".to_vec()),
        (9, b"https://f.zalo.me/pic/123.jpg".to_vec()),
        (10, b"https://f.zalo.me/thumb/123_t.jpg".to_vec()),
        (12, b"204800".to_vec()),
        (13, b"jpg".to_vec()),
    ]);
    let quote_box = encode_tlvbox_u16(&[
        (1, 888_i32.to_le_bytes().to_vec()),
        (2, 1_700_000_000_111_i64.to_le_bytes().to_vec()),
        (3, 9_876_543_210_999_i64.to_le_bytes().to_vec()),
        (4, 1_i32.to_le_bytes().to_vec()),
        (5, 1_700_000_000_000_i64.to_le_bytes().to_vec()),
        (6, 0_i64.to_le_bytes().to_vec()),
        (7, b"Quoted hello".to_vec()),
        (10, b"Alice".to_vec()),
    ]);
    let mention_box = encode_tlvbox_u16(&[
        (1, 4242_i32.to_le_bytes().to_vec()),
        (2, 0_i32.to_le_bytes().to_vec()),
        (3, 6_i32.to_le_bytes().to_vec()),
        (4, 0_i32.to_le_bytes().to_vec()),
    ]);
    let top_box = encode_tlvbox_u16(&[
        (1, prop_box),
        (2, attach_box),
        (3, quote_box),
        (4, mention_box),
        (5, 1_i32.to_le_bytes().to_vec()),
        (6, b"zcloud".to_vec()),
        (7, br#"{"extra":true}"#.to_vec()),
    ]);

    let parsed = fsb
        .dispatch_sync(
            "$znode.nativelibs.dbUtilsAddon.parseBinNet",
            &[wire::encode_bytes(&top_box)],
        )
        .unwrap();
    assert_eq!(parsed["result"].as_i64(), Some(0));
    assert_eq!(parsed["error_message"].as_str(), Some(""));
    let d = &parsed["data"];
    assert_eq!(d["property"]["size"].as_i64(), Some(4096));
    assert_eq!(d["property"]["subType"].as_i64(), Some(2));
    assert_eq!(d["property"]["type"].as_i64(), Some(7));
    assert_eq!(d["property"]["ext"].as_str(), Some("pdf"));
    assert_eq!(d["property"]["color"].as_i64(), Some(0xFF00AA));
    assert_eq!(d["attachs"][0]["type"].as_str(), Some("photo"));
    assert_eq!(d["attachs"][0]["catId"].as_i64(), Some(12));
    assert_eq!(d["attachs"][0]["id"].as_i64(), Some(99));
    assert_eq!(d["attachs"][0]["title"].as_str(), Some("Architecture Spec"));
    assert_eq!(
        d["attachs"][0]["href"].as_str(),
        Some("https://f.zalo.me/pic/123.jpg")
    );
    assert_eq!(
        d["attachs"][0]["thumb"].as_str(),
        Some("https://f.zalo.me/thumb/123_t.jpg")
    );
    assert_eq!(d["attachs"][0]["size"].as_str(), Some("204800"));
    assert_eq!(d["attachs"][0]["ext"].as_str(), Some("jpg"));
    assert_eq!(d["quote"]["ownerId"].as_i64(), Some(888));
    assert_eq!(d["quote"]["cliMsgId"].as_i64(), Some(1_700_000_000_111));
    assert_eq!(d["quote"]["globalMsgId"].as_i64(), Some(9_876_543_210_999));
    assert_eq!(d["quote"]["msg"].as_str(), Some("Quoted hello"));
    assert_eq!(d["quote"]["fromD"].as_str(), Some("Alice"));
    assert_eq!(d["mentions"][0]["uid"].as_i64(), Some(4242));
    assert_eq!(d["mentions"][0]["len"].as_i64(), Some(6));
    assert_eq!(d["quoteStatus"].as_i64(), Some(1));
    assert_eq!(d["dataSource"].as_str(), Some("zcloud"));
    assert_eq!(d["data"].as_str(), Some(r#"{"extra":true}"#));

    // B. `db-cross-v4/src/main.cc` `[u32 LE key][u32 LE len][val]` framing (`0x05..=0x0b`)
    let main_cc_blob = encode_tlv32_le(&[
        (
            0x05,
            encode_tlv32_le(&[
                (0x1e, 512_i32.to_le_bytes().to_vec()),
                (0x21, b"mp4".to_vec()),
            ]),
        ),
        (
            0x06,
            encode_tlv32_le(&[
                (0x28, b"video".to_vec()),
                (0x30, b"https://f.zalo.me/v.mp4".to_vec()),
            ]),
        ),
        (0x09, 2_i32.to_le_bytes().to_vec()),
        (0x0a, b"server".to_vec()),
    ]);
    let parsed_v4 = parse_bin_net(&main_cc_blob);
    assert_eq!(parsed_v4["result"].as_i64(), Some(0));
    assert_eq!(parsed_v4["data"]["property"]["size"].as_i64(), Some(512));
    assert_eq!(parsed_v4["data"]["property"]["ext"].as_str(), Some("mp4"));
    assert_eq!(parsed_v4["data"]["attachs"][0]["type"].as_str(), Some("video"));
    assert_eq!(parsed_v4["data"]["quoteStatus"].as_i64(), Some(2));
    assert_eq!(parsed_v4["data"]["dataSource"].as_str(), Some("server"));

    // Error / edge cases (never panics!)
    let wrong_arg = fsb
        .dispatch_sync("$znode.nativelibs.dbUtilsAddon.parseBinNet", &[json!("not-bytes")])
        .unwrap();
    assert_eq!(wrong_arg["result"].as_i64(), Some(1));
    assert_eq!(wrong_arg["error_message"].as_str(), Some("Wrong arguments"));

    let empty_bin = parse_bin_net(&[]);
    assert_eq!(empty_bin["result"].as_i64(), Some(2));

    let corrupt_bin = parse_bin_net(&[0xFF, 0xFF, 0xFF, 0x7F, 0x01]);
    assert_eq!(corrupt_bin["result"].as_i64(), Some(2));
}

// ============================================================================
// 4. `fs_bridge` Fixes: `file-utils`, `zfile`, and `generateVideoThumbnail`
// ============================================================================

#[test]
fn file_utils_get_disk_usage_and_zfile_info_match_native_contracts() {
    let tmp = TempDir::new().unwrap();
    let platform = Arc::new(ZaloPlatform::with_root(tmp.path().join("app"), "26.10.20"));
    let fsb = FsBridge::new(Arc::clone(&platform));

    // 1. `file_utils_get_disk_usage` error strings & exact 3-key `statvfs` output
    assert_eq!(
        file_utils_get_disk_usage(&[]).unwrap_err(),
        "DISKUSAGE_WRONG_NUMBER_OF_ARGS"
    );
    assert_eq!(
        file_utils_get_disk_usage(&[json!("/"), json!("/tmp")]).unwrap_err(),
        "DISKUSAGE_WRONG_NUMBER_OF_ARGS"
    );
    assert_eq!(
        file_utils_get_disk_usage(&[json!(123)]).unwrap_err(),
        "DISKUSAGE_INVALID_ARG_TYPE:  The \"path\" argument must be one of type string"
    );
    assert_eq!(
        file_utils_get_disk_usage(&[json!("/definitely/missing/zalo/diskusage/path")]).unwrap_err(),
        "DISKUSAGE_RUNTIME_ERROR: Get diskusage failed"
    );

    let du = fsb
        .dispatch_sync(
            "$znode.nativelibs.fileUtilsAddon.getDiskUsage",
            &[json!(tmp.path().to_string_lossy())],
        )
        .unwrap();
    let du_obj = du.as_object().unwrap();
    assert_eq!(du_obj.len(), 3, "getDiskUsage must return exact 3 keys: {du}");
    assert!(du_obj.contains_key("available"));
    assert!(du_obj.contains_key("free"));
    assert!(du_obj.contains_key("total"));
    assert!(!du_obj.contains_key("used"));

    // Cross-check directly against `libc::statvfs`
    let cpath = std::ffi::CString::new(tmp.path().to_str().unwrap()).unwrap();
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::statvfs(cpath.as_ptr(), &mut vfs) }, 0);
    let expected_total = (vfs.f_blocks as u64) * (vfs.f_frsize as u64);
    assert_eq!(du["total"].as_u64(), Some(expected_total));
    assert!(du["available"].as_u64().unwrap() <= expected_total);
    assert!(du["free"].as_u64().unwrap() <= expected_total);

    // 2. `zfile_get_disk_info`
    let disks = zfile_get_disk_info().unwrap();
    let disks_map = disks.as_object().unwrap();
    assert!(disks_map.contains_key("/"), "getDiskInfo must always include '/': {disks}");
    assert!(!disks_map.contains_key("/proc"), "pseudo-fs /proc must be excluded");
    assert!(!disks_map.contains_key("/sys"), "pseudo-fs /sys must be excluded");
    let root_disk = &disks_map["/"];
    assert_eq!(root_disk["path"].as_str(), Some("/"));
    assert_eq!(root_disk["name"].as_str(), Some("/"));
    assert_eq!(root_disk["label"].as_str(), Some("/"));
    assert_eq!(root_disk["isExternal"].as_bool(), Some(false));
    assert!(root_disk["totalSpace"].as_u64().unwrap() > 0);
    assert!(root_disk["usedSpace"].as_u64().unwrap() <= root_disk["totalSpace"].as_u64().unwrap());

    // 3. `zfile_get_info` (`is_folder` false vs true + symlink skipping + `ENOENT`)
    let folder = tmp.path().join("zfile-folder");
    std::fs::create_dir_all(folder.join("sub")).unwrap();
    let f1 = folder.join("hello.txt");
    let f2 = folder.join("sub/world.bin");
    std::fs::write(&f1, b"hello").unwrap(); // 5 bytes
    std::fs::write(&f2, vec![7u8; 20]).unwrap(); // 20 bytes
    symlink(&f2, folder.join("symlink_ignored.bin")).unwrap();

    let file_info = fsb
        .dispatch_sync(
            "$znode.nativelibs.zfileAddon.getInfo",
            &[json!(f1.to_string_lossy()), json!(false)],
        )
        .unwrap();
    assert_eq!(file_info["size"].as_u64(), Some(5));
    assert_eq!(file_info["isDirectory"].as_bool(), Some(false));
    assert_eq!(file_info["name"].as_str(), Some("hello.txt"));
    assert!(file_info["lastWriteTime"].as_f64().unwrap() > 0.0);
    assert!(file_info.get("fileCount").is_none());

    let folder_info = zfile_get_info(&[json!(folder.to_string_lossy()), json!(true)]).unwrap();
    assert_eq!(folder_info["size"].as_u64(), Some(25));
    assert_eq!(folder_info["fileCount"].as_u64(), Some(2));
    assert_eq!(folder_info["isDirectory"].as_bool(), Some(true));
    assert_eq!(folder_info["name"].as_str(), Some("zfile-folder"));
    assert!(folder_info["lastWriteTime"].as_f64().unwrap() > 0.0);

    let missing_path = tmp.path().join("no_such_zfile.dat");
    let enoent = zfile_get_info(&[json!(missing_path.to_string_lossy())]).unwrap_err();
    assert_eq!(
        enoent,
        format!(
            "ENOENT: no such file or directory, stat '{}'",
            missing_path.display()
        )
    );
}

/// Extracts `(width, height)` from a baseline/progressive JPEG byte slice by scanning SOF markers.
fn parse_jpeg_dimensions(jpg: &[u8]) -> Option<(u16, u16)> {
    if jpg.len() < 4 || jpg[0] != 0xFF || jpg[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    while i + 9 < jpg.len() {
        if jpg[i] != 0xFF {
            i += 1;
            continue;
        }
        while i < jpg.len() && jpg[i] == 0xFF {
            i += 1;
        }
        if i >= jpg.len() {
            break;
        }
        let marker = jpg[i];
        i += 1;
        if matches!(marker, 0xD8 | 0xD9 | 0x01) || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if i + 2 > jpg.len() {
            break;
        }
        let seg_len = u16::from_be_bytes([jpg[i], jpg[i + 1]]) as usize;
        if seg_len < 2 || i + seg_len > jpg.len() {
            break;
        }
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) && seg_len >= 7
        {
            let h = u16::from_be_bytes([jpg[i + 3], jpg[i + 4]]);
            let w = u16::from_be_bytes([jpg[i + 5], jpg[i + 6]]);
            return Some((w, h));
        }
        i += seg_len;
    }
    None
}

#[tokio::test]
async fn video_thumbnail_uses_640_default_even_dimensions_and_supports_http_urls() {
    let ffmpeg_ok = std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ffmpeg_ok {
        return;
    }

    let tmp = TempDir::new().unwrap();
    let platform = Arc::new(ZaloPlatform::with_root(tmp.path().join("app"), "26.10.20"));
    let fsb = FsBridge::new(Arc::clone(&platform));

    // Create a 702x501 (odd height, > 640 width) MP4 using `yuv444p` (which allows odd source height).
    let src_mp4 = tmp.path().join("odd_702x501.mp4");
    let gen = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=702x501:d=0.4",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv444p",
            "-y",
        ])
        .arg(&src_mp4)
        .status()
        .unwrap();
    assert!(gen.success(), "failed to synthesize odd-dimension MP4 fixture");

    // 1. Default max dimensions (`640x640`, NOT old `480x480`) + guaranteed even dimensions!
    let thumb_local = platform.temp_dir().join("thumb_default_640.jpg");
    let out_local = fsb
        .dispatch_async(
            "$zFileManager.generateVideoThumbnail",
            &[
                json!(src_mp4.to_string_lossy()),
                json!(thumb_local.to_string_lossy()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(out_local, json!(thumb_local.to_string_lossy()));
    let jpg_bytes = std::fs::read(&thumb_local).unwrap();
    let (w, h) = parse_jpeg_dimensions(&jpg_bytes).expect("valid JPEG SOF header");
    assert_eq!(w, 640, "default max_w must be 640 (not 480), got {w}x{h}");
    assert!(h <= 640 && h > 0);
    assert_eq!(w % 2, 0, "thumbnail width must be even: {w}");
    assert_eq!(h % 2, 0, "thumbnail height must be even: {h}");

    // Also verify non-upscaling odd source (e.g. 301x201 with 640 max -> 300x200 even!)
    let small_odd_mp4 = tmp.path().join("small_301x201.mp4");
    let gen_small = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=green:s=301x201:d=0.4",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv444p",
            "-y",
        ])
        .arg(&small_odd_mp4)
        .status()
        .unwrap();
    assert!(gen_small.success());
    let thumb_small = platform.temp_dir().join("thumb_small_even.jpg");
    let _ = fsb
        .dispatch_async(
            "$zFileManager.generateVideoThumbnail",
            &[
                json!(small_odd_mp4.to_string_lossy()),
                json!(thumb_small.to_string_lossy()),
            ],
        )
        .await
        .unwrap();
    let (sw, sh) = parse_jpeg_dimensions(&std::fs::read(&thumb_small).unwrap()).unwrap();
    assert_eq!((sw, sh), (300, 200));

    // 2. `http://` video input served by a local loopback HTTP server (supports multiple connections/range-less GET)
    let mp4_data = Arc::new(std::fs::read(&src_mp4).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(false).unwrap();
    let server_data = Arc::clone(&mp4_data);
    let server_handle = std::thread::spawn(move || {
        // FFmpeg may open 1-3 HTTP requests when probing MP4 atoms; serve up to 6 requests cleanly.
        let _ = listener.set_nonblocking(true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut served = 0usize;
        while std::time::Instant::now() < deadline && served < 6 {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    served += 1;
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                    let mut req_buf = [0u8; 2048];
                    let n = stream.read(&mut req_buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&req_buf[..n]);
                    // Honor `Range: bytes=START-` if FFmpeg sends a byte-range seek for `moov`.
                    let mut start = 0usize;
                    for line in req.lines() {
                        if let Some(rest) = line
                            .strip_prefix("Range: bytes=")
                            .or_else(|| line.strip_prefix("range: bytes="))
                        {
                            if let Some(num_str) = rest.split('-').next() {
                                if let Ok(off) = num_str.trim().parse::<usize>() {
                                    if off < server_data.len() {
                                        start = off;
                                    }
                                }
                            }
                        }
                    }
                    let slice = &server_data[start..];
                    let hdr = if start > 0 {
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            start,
                            server_data.len() - 1,
                            server_data.len(),
                            slice.len()
                        )
                    } else {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            slice.len()
                        )
                    };
                    let _ = stream.write_all(hdr.as_bytes());
                    let _ = stream.write_all(slice);
                    let _ = stream.flush();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let url = format!("http://127.0.0.1:{port}/odd_702x501.mp4");
    let thumb_http = platform.temp_dir().join("thumb_from_http.jpg");
    let out_http = fsb
        .dispatch_async(
            "$zFileManager.generateVideoThumbnail",
            &[
                json!(url),
                json!(thumb_http.to_string_lossy()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(out_http, json!(thumb_http.to_string_lossy()));
    let (hw, hh) = parse_jpeg_dimensions(&std::fs::read(&thumb_http).unwrap()).unwrap();
    assert_eq!(hw, 640);
    assert_eq!(hh % 2, 0);

    // Stop server quickly by making a dummy connect if needed or letting loop exit; wait: don't block 5s!
    drop(server_handle);
    let _ = Path::new(&url);
}

#[tokio::test]
async fn global_zwalker_ipc_and_zfile_access_and_v1_db_bridge_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let platform = Arc::new(ZaloPlatform::with_root(tmp.path().join("app"), "26.10.20"));
    let fsb = FsBridge::new(Arc::clone(&platform));

    // Exercise `FsBridge::dispatch_zwalker_ipc` / `olaz_core::handle_zwalker_ipc` (global state)
    let gc_dir = platform.user_data().join("GlobalGcTest");
    std::fs::create_dir_all(gc_dir.join("empty_leaf")).unwrap();
    let orphan = gc_dir.join("orphan.bin");
    std::fs::write(&orphan, vec![9u8; 64]).unwrap();

    let gc_str = gc_dir.to_string_lossy().into_owned();
    let c1 = fsb
        .dispatch_zwalker_ipc("zwalker-collect-dir-stats", &[json!(gc_str), json!(["**"])])
        .unwrap();
    assert_eq!(c1["fileNumber"].as_u64(), Some(1));
    assert_eq!(c1["size"].as_u64(), Some(64));

    let d1 = olaz_core::handle_zwalker_ipc(
        &*platform,
        "zwalker-del-unmarked-files",
        &[json!(gc_str), json!([]), json!(["**"])],
    )
    .unwrap();
    assert_eq!(d1["fileNumber"].as_u64(), Some(1));
    assert!(!orphan.exists());

    let e1 = fsb
        .dispatch_zwalker_ipc("zwalker-delete-empty-folders", &[json!(gc_str)])
        .unwrap();
    assert_eq!(e1["deletedCount"].as_u64(), Some(1));

    // Exercise `zfileAddon.{canRead,canWrite,canReadAndWrite}` & `dbUtilsAddon.decompressAndDecryptDb`
    assert_eq!(
        fsb.dispatch_sync(
            "$znode.nativelibs.zfileAddon.canRead",
            &[json!(gc_dir.to_string_lossy())]
        )
        .unwrap(),
        json!(true)
    );
    assert_eq!(
        fsb.dispatch_sync(
            "$znode.nativelibs.zfileAddon.canWrite",
            &[json!(gc_dir.join("new_child.dat").to_string_lossy())]
        )
        .unwrap(),
        json!(true)
    );
    assert_eq!(
        fsb.dispatch_sync(
            "$znode.nativelibs.zfileAddon.canWrite",
            &[json!("/etc/passwd")]
        )
        .unwrap(),
        json!(false)
    );
    let v1_via_bridge = fsb
        .dispatch_async(
            "$znode.nativelibs.dbUtilsAddon.decompressAndDecryptDb",
            &[json!("in.db"), json!("out"), json!("key")],
        )
        .await
        .unwrap();
    assert_eq!(v1_via_bridge["result"].as_i64(), Some(-1));
    assert_eq!(
        v1_via_bridge["err_message"].as_str(),
        Some("V1 format not fully implemented in this build")
    );
}
