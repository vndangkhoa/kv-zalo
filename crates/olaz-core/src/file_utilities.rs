//! Pure-Rust port of Zalo's `file-utilities` native addon (all 11 exports + `index.js`
//! `options.deep` and scoped `detectHardlinks` ergonomics).

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use globset::Glob;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::fs_bridge::{BridgeResult, PlatformPaths};
use crate::wire;

/// Global cancellation registry keyed by caller-supplied `jobId`.
pub static JOBS: Lazy<Mutex<HashMap<u64, Arc<AtomicBool>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Registers an asynchronous job in [`JOBS`] and returns its cancellation flag.
pub fn register_job(job_id: u64) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    JOBS.lock().insert(job_id, Arc::clone(&flag));
    flag
}

/// Removes a completed or failed job from [`JOBS`].
pub fn unregister_job(job_id: u64) {
    JOBS.lock().remove(&job_id);
}

/// Signals cooperative cancellation for `job_id` if currently active (no-op if unknown).
pub fn cancel_job(job_id: u64) {
    if let Some(flag) = JOBS.lock().get(&job_id) {
        flag.store(true, Ordering::SeqCst);
    }
}

/// Result of `getDirectorySize*` and `getDirectorySizeByGlob*`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectorySizeResult {
    pub total_size: f64,
    pub file_count: u64,
    pub duration_ms: f64,
}

/// Node in `getDirectorySizeTree*` and `getDirectorySize*({ deep })`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryTreeNode {
    pub name: String,
    pub path: String,
    pub relative_path: String,
    pub depth: u32,
    pub size: f64,
    pub total_size: f64,
    pub file_count: u64,
    pub children: Vec<DirectoryTreeNode>,
}

/// Result of 1-argument `detectHardlinks*`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HardlinkInfo {
    pub is_hardlink: bool,
    pub link_count: u64,
}

/// Result of `detectFilesystem*`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemInfo {
    pub filesystem_type: String,
    pub volume_name: String,
    pub max_filename_length: u32,
    pub supports_case_sensitive_names: bool,
    pub supports_unicode_filenames: bool,
    pub supports_compression: bool,
    pub supports_encryption: bool,
}

fn ensure_existing_directory(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() || !path.exists() {
        return Err(format!("Root path does not exist: {}", path.display()));
    }
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("Failed to stat '{}': {e}", path.display()))?;
    if !meta.file_type().is_dir() {
        return Err(format!("Path '{}' is not a directory", path.display()));
    }
    Ok(())
}

fn validate_workers_option(options: Option<&Value>) -> Result<Option<usize>, String> {
    let Some(opts) = options.and_then(Value::as_object) else {
        return Ok(None);
    };
    let Some(w_val) = opts.get("workers") else {
        return Ok(None);
    };
    if w_val.is_null() || wire::is_undefined(w_val) {
        return Ok(None);
    }
    let n = w_val
        .as_i64()
        .or_else(|| w_val.as_f64().map(|f| f as i64))
        .ok_or_else(|| "Worker count must be a positive integer".to_string())?;
    if n <= 0 {
        return Err("Worker count must be greater than 0".to_string());
    }
    Ok(Some(n as usize))
}

/// Computes logical byte total and unique-inode file count under `path`, skipping symlinks
/// (`symlink_metadata`) and deduplicating hardlinks by `(dev, ino)`.
pub fn get_directory_size(
    path: &Path,
    workers_opt: Option<usize>,
    cancel_flag_opt: Option<&AtomicBool>,
) -> Result<DirectorySizeResult, String> {
    if workers_opt == Some(0) {
        return Err("Worker count must be greater than 0".to_string());
    }
    ensure_existing_directory(path)?;

    let start = Instant::now();
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut total_size = 0.0_f64;
    let mut file_count = 0_u64;
    let mut stack: Vec<PathBuf> = vec![path.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if cancel_flag_opt.is_some_and(|c| c.load(Ordering::SeqCst)) {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            if cancel_flag_opt.is_some_and(|c| c.load(Ordering::SeqCst)) {
                break;
            }
            let entry_path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&entry_path) else {
                continue;
            };
            let ft = meta.file_type();
            if ft.is_dir() {
                stack.push(entry_path);
            } else if ft.is_file() && seen.insert((meta.dev(), meta.ino())) {
                total_size += meta.len() as f64;
                file_count += 1;
            }
        }
    }

    Ok(DirectorySizeResult {
        total_size,
        file_count,
        duration_ms: start.elapsed().as_secs_f64() * 1000.0,
    })
}

fn build_tree_node(
    dir: &Path,
    root: &Path,
    depth: u32,
    max_depth: u32,
    seen: &mut HashSet<(u64, u64)>,
    cancel_flag_opt: Option<&AtomicBool>,
) -> DirectoryTreeNode {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned());
    let relative_path = dir
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut size = 0.0_f64;
    let mut file_count = 0_u64;
    let mut children = Vec::new();

    if !cancel_flag_opt.is_some_and(|c| c.load(Ordering::SeqCst)) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();

            for entry_path in entries {
                if cancel_flag_opt.is_some_and(|c| c.load(Ordering::SeqCst)) {
                    break;
                }
                let Ok(meta) = std::fs::symlink_metadata(&entry_path) else {
                    continue;
                };
                let ft = meta.file_type();
                if ft.is_dir() {
                    let child = build_tree_node(
                        &entry_path,
                        root,
                        depth + 1,
                        max_depth,
                        seen,
                        cancel_flag_opt,
                    );
                    size += child.size;
                    file_count += child.file_count;
                    if depth < max_depth {
                        children.push(child);
                    }
                } else if ft.is_file() && seen.insert((meta.dev(), meta.ino())) {
                    size += meta.len() as f64;
                    file_count += 1;
                }
            }
        }
    }

    DirectoryTreeNode {
        name,
        path: dir.to_string_lossy().into_owned(),
        relative_path,
        depth,
        size,
        total_size: size,
        file_count,
        children,
    }
}

/// Builds a cumulative directory size tree rooted at `path`, truncating `children` beyond
/// `max_depth` while preserving full bottom-up subtree `size`/`totalSize` and `fileCount`.
pub fn get_directory_size_tree(
    path: &Path,
    max_depth: i32,
    _include_root: bool,
    cancel_flag_opt: Option<&AtomicBool>,
) -> Result<DirectoryTreeNode, String> {
    if max_depth < 0 {
        return Err("max_depth must be >= 0".to_string());
    }
    ensure_existing_directory(path)?;

    let mut seen = HashSet::new();
    Ok(build_tree_node(
        path,
        path,
        0,
        max_depth as u32,
        &mut seen,
        cancel_flag_opt,
    ))
}

fn flatten_tree_nodes(root: &DirectoryTreeNode, include_root: bool) -> Vec<DirectoryTreeNode> {
    fn push_rec(node: &DirectoryTreeNode, out: &mut Vec<DirectoryTreeNode>) {
        out.push(node.clone());
        for child in &node.children {
            push_rec(child, out);
        }
    }

    let mut out = Vec::new();
    if include_root {
        push_rec(root, &mut out);
    } else {
        for child in &root.children {
            push_rec(child, &mut out);
        }
    }
    out
}

/// Evaluates `getDirectorySize*` with full `index.js` `options` support (`workers` and
/// `deep: { maxDepth, includeRoot? }`).
pub fn get_directory_size_with_options(
    path: &Path,
    options: Option<&Value>,
    cancel_flag_opt: Option<&AtomicBool>,
) -> BridgeResult {
    let workers_opt = validate_workers_option(options)?;

    if let Some(deep_val) = options
        .and_then(Value::as_object)
        .and_then(|o| o.get("deep"))
        .filter(|v| !v.is_null() && !wire::is_undefined(v) && **v != Value::Bool(false))
    {
        let max_depth = match deep_val {
            Value::Object(map) => map
                .get("maxDepth")
                .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
                .unwrap_or(1) as i32,
            Value::Number(n) => n
                .as_i64()
                .or_else(|| n.as_f64().map(|f| f as i64))
                .unwrap_or(1) as i32,
            Value::Bool(true) => 1,
            _ => 1,
        };
        let include_root = deep_val
            .as_object()
            .and_then(|m| m.get("includeRoot"))
            .and_then(Value::as_bool)
            .or_else(|| {
                options
                    .and_then(Value::as_object)
                    .and_then(|o| o.get("includeRoot"))
                    .and_then(Value::as_bool)
            })
            .unwrap_or(false);

        let start = Instant::now();
        let root = get_directory_size_tree(path, max_depth, include_root, cancel_flag_opt)?;
        let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        let tree = flatten_tree_nodes(&root, include_root);
        return Ok(json!({
            "totalSize": root.size,
            "fileCount": root.file_count,
            "durationMs": duration_ms,
            "tree": tree,
        }));
    }

    let res = get_directory_size(path, workers_opt, cancel_flag_opt)?;
    serde_json::to_value(res).map_err(|e| e.to_string())
}

fn extract_glob_base_dir(pattern: &str) -> PathBuf {
    let mut base = PathBuf::new();
    for comp in Path::new(pattern).components() {
        let s = comp.as_os_str().to_string_lossy();
        if s.contains(['*', '?', '[', '{']) {
            break;
        }
        base.push(comp.as_os_str());
    }
    if base.as_os_str().is_empty() {
        base.push(".");
    }
    base
}

/// Computes logical byte total and unique-inode file count for regular files matching `pattern`.
pub fn get_directory_size_by_glob(
    pattern: &str,
    cancel_flag_opt: Option<&AtomicBool>,
) -> Result<DirectorySizeResult, String> {
    let start = Instant::now();
    if pattern.is_empty() {
        return Err("Invalid glob pattern '': empty pattern".to_string());
    }
    let matcher = Glob::new(pattern)
        .map_err(|e| format!("Invalid glob pattern '{pattern}': {e}"))?
        .compile_matcher();

    let base = extract_glob_base_dir(pattern);
    if !base.exists() {
        return Ok(DirectorySizeResult {
            total_size: 0.0,
            file_count: 0,
            duration_ms: start.elapsed().as_secs_f64() * 1000.0,
        });
    }

    let mut total_size = 0.0_f64;
    let mut file_count = 0_u64;
    let mut seen: HashSet<(u64, u64)> = HashSet::new();

    for entry in WalkDir::new(&base)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if cancel_flag_opt.is_some_and(|c| c.load(Ordering::SeqCst)) {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if !matcher.is_match(entry.path()) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if seen.insert((meta.dev(), meta.ino())) {
                total_size += meta.len() as f64;
                file_count += 1;
            }
        }
    }

    Ok(DirectorySizeResult {
        total_size,
        file_count,
        duration_ms: start.elapsed().as_secs_f64() * 1000.0,
    })
}

/// Returns `{ isHardlink, linkCount }` for regular file `path` via `symlink_metadata`.
pub fn detect_hardlinks_info(path: &Path) -> Result<HardlinkInfo, String> {
    if path.as_os_str().is_empty() || !path.exists() {
        return Err(format!("Root path does not exist: {}", path.display()));
    }
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("Failed to read file metadata for {}: {e}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("Path is not a file: {}", path.display()));
    }
    let link_count = meta.nlink();
    Ok(HardlinkInfo {
        is_hardlink: link_count > 1,
        link_count,
    })
}

/// Walks `scope_dir` via `symlink_metadata` (skipping symlinks) and returns all regular-file
/// paths sharing `(dev, ino)` with `path`. Errors with `"File has only one link"` if `nlink <= 1`.
pub fn detect_hardlinks_in_scope(path: &Path, scope_dir: &Path) -> Result<Vec<String>, String> {
    if path.as_os_str().is_empty() || !path.exists() {
        return Err(format!("Root path does not exist: {}", path.display()));
    }
    let target = std::fs::symlink_metadata(path)
        .map_err(|e| format!("Failed to read file metadata for {}: {e}", path.display()))?;
    if !target.file_type().is_file() {
        return Err(format!("Path is not a file: {}", path.display()));
    }
    if target.nlink() <= 1 {
        return Err("File has only one link".to_string());
    }
    ensure_existing_directory(scope_dir)?;

    let target_dev = target.dev();
    let target_ino = target.ino();
    let mut matches = Vec::new();
    let mut stack = vec![scope_dir.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            let ft = meta.file_type();
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && meta.dev() == target_dev && meta.ino() == target_ino {
                matches.push(p.to_string_lossy().into_owned());
            }
        }
    }

    matches.sort();
    Ok(matches)
}

fn linux_fs_type_name(f_type: u64) -> &'static str {
    match f_type & 0xFFFF_FFFF {
        0xEF53 => "EXT4",
        0x9123_683E => "BTRFS",
        0x5846_5342 => "XFS",
        0xF2F5_2010 => "F2FS",
        0x2FC1_2FC1 => "ZFS",
        0x0102_1994 => "TMPFS",
        0x5346_544E | 0x7366_746E => "NTFS",
        0x4D44 => "VFAT",
        0x2011_BAB0 => "EXFAT",
        0x6969 => "NFS",
        0xFF53_4D42 | 0xFE53_4D42 => "CIFS",
        0x6573_5546 => "FUSE",
        0x794C_7630 => "OVERLAYFS",
        0x7371_7368 => "SQUASHFS",
        0x8584_58F6 => "RAMFS",
        _ => "UNKNOWN",
    }
}

fn linux_fs_capabilities(fs_type: &str) -> (bool, bool, bool, bool) {
    // (supports_case_sensitive_names, supports_unicode_filenames, supports_compression, supports_encryption)
    match fs_type {
        "EXT4" => (true, true, false, true),
        "BTRFS" => (true, true, true, false),
        "XFS" => (true, true, false, false),
        "ZFS" | "F2FS" => (true, true, true, true),
        "SQUASHFS" => (true, true, true, false),
        "VFAT" | "EXFAT" | "CIFS" => (false, true, false, false),
        "NTFS" => (false, true, true, true),
        _ => (true, true, false, false),
    }
}

/// Detects filesystem type and capabilities for `path` via `libc::statfs`.
pub fn detect_filesystem(path: &Path) -> Result<FilesystemInfo, String> {
    if path.as_os_str().is_empty() || !path.exists() {
        return Err(format!("Path does not exist: {}", path.display()));
    }
    let cpath = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("Invalid path string: {}", path.display()))?;
    let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(cpath.as_ptr(), &mut sfs) };
    if rc != 0 {
        return Err(format!(
            "Failed to get filesystem information for '{}'",
            path.display()
        ));
    }

    let fs_type = linux_fs_type_name(sfs.f_type as u64);
    let (cs, uni, comp, enc) = linux_fs_capabilities(fs_type);
    Ok(FilesystemInfo {
        filesystem_type: fs_type.to_string(),
        volume_name: String::new(),
        max_filename_length: sfs.f_namelen.max(0) as u32,
        supports_case_sensitive_names: cs,
        supports_unicode_filenames: uni,
        supports_compression: comp,
        supports_encryption: enc,
    })
}

fn normalize_method(method: &str) -> &str {
    method
        .strip_prefix("$znode.nativelibs.fileUtilitiesAddon.")
        .unwrap_or(method)
}

fn parse_job_id(val: Option<&Value>) -> Option<u64> {
    val.and_then(|v| v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)))
}

fn parse_tree_args(args: &[Value]) -> Result<(PathBuf, i32, bool), String> {
    let raw = args
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| "Path argument must be a string".to_string())?;
    let opts = args.get(1);
    let _ = validate_workers_option(opts)?;
    let max_depth = opts
        .and_then(Value::as_object)
        .and_then(|m| m.get("maxDepth"))
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .ok_or_else(|| "options.maxDepth is required".to_string())? as i32;
    let include_root = opts
        .and_then(Value::as_object)
        .and_then(|m| m.get("includeRoot"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Ok((PathBuf::from(raw), max_depth, include_root))
}

fn dispatch_hardlinks(args: &[Value]) -> BridgeResult {
    let raw_path = args
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| "Path argument must be a string".to_string())?;
    let scope_opt = args
        .get(1)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    if let Some(scope) = scope_opt {
        let links = detect_hardlinks_in_scope(Path::new(raw_path), Path::new(scope))?;
        Ok(json!(links))
    } else {
        let info = detect_hardlinks_info(Path::new(raw_path))?;
        serde_json::to_value(info).map_err(|e| e.to_string())
    }
}

fn run_method_with_cancel(
    method: &str,
    args: &[Value],
    cancel_flag: Option<&AtomicBool>,
) -> BridgeResult {
    match normalize_method(method) {
        "getDirectorySizeSync" | "getDirectorySizeAsync" => {
            let raw = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "Path argument must be a string".to_string())?;
            get_directory_size_with_options(Path::new(raw), args.get(1), cancel_flag)
        }
        "getDirectorySizeTreeSync" | "getDirectorySizeTreeAsync" => {
            let (path, max_depth, include_root) = parse_tree_args(args)?;
            let tree = get_directory_size_tree(&path, max_depth, include_root, cancel_flag)?;
            serde_json::to_value(tree).map_err(|e| e.to_string())
        }
        "getDirectorySizeByGlobSync" | "getDirectorySizeByGlobAsync" => {
            let pattern = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "Glob pattern argument must be a string".to_string())?;
            let _ = validate_workers_option(args.get(1))?;
            let res = get_directory_size_by_glob(pattern, cancel_flag)?;
            serde_json::to_value(res).map_err(|e| e.to_string())
        }
        "detectHardlinksSync" | "detectHardlinksAsync" => dispatch_hardlinks(args),
        "detectFilesystemSync" | "detectFilesystemAsync" => {
            let raw = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "Path argument must be a string".to_string())?;
            let info = detect_filesystem(Path::new(raw))?;
            serde_json::to_value(info).map_err(|e| e.to_string())
        }
        "cancelJob" => {
            if let Some(job_id) = parse_job_id(args.first()) {
                cancel_job(job_id);
            }
            Ok(wire::undefined())
        }
        other => Err(format!("Unsupported fileUtilitiesAddon method: {other}")),
    }
}

/// Synchronous dispatcher for all 11 `file-utilities` exports.
pub fn dispatch_file_utilities_sync(
    _platform: &dyn PlatformPaths,
    method: &str,
    args: &[Value],
) -> BridgeResult {
    let short = normalize_method(method);
    if matches!(
        short,
        "getDirectorySizeAsync" | "getDirectorySizeTreeAsync" | "getDirectorySizeByGlobAsync"
    ) {
        if let Some(job_id) = parse_job_id(args.get(2)) {
            let flag = register_job(job_id);
            let res = run_method_with_cancel(short, args, Some(&flag));
            unregister_job(job_id);
            return res;
        }
    }
    run_method_with_cancel(short, args, None)
}

/// Asynchronous dispatcher for all 11 `file-utilities` exports (registers `jobId` in [`JOBS`]
/// immediately before offloading onto Tokio's blocking pool so concurrent `cancelJob(jobId)`
/// calls observe the running job).
pub async fn dispatch_file_utilities_async(
    _platform: &dyn PlatformPaths,
    method: &str,
    args: &[Value],
) -> BridgeResult {
    let short = normalize_method(method).to_string();
    if short == "cancelJob" {
        if let Some(job_id) = parse_job_id(args.first()) {
            cancel_job(job_id);
        }
        return Ok(wire::undefined());
    }

    let tracked_job: Option<(u64, Arc<AtomicBool>)> = matches!(
        short.as_str(),
        "getDirectorySizeAsync" | "getDirectorySizeTreeAsync" | "getDirectorySizeByGlobAsync"
    )
    .then(|| parse_job_id(args.get(2)))
    .flatten()
    .map(|id| (id, register_job(id)));

    let args_owned = args.to_vec();
    let flag_for_worker = tracked_job.as_ref().map(|(_, f)| Arc::clone(f));
    let join_res = tokio::task::spawn_blocking(move || {
        run_method_with_cancel(&short, &args_owned, flag_for_worker.as_deref())
    })
    .await;

    if let Some((id, _)) = tracked_job {
        unregister_job(id);
    }

    join_res.map_err(|e| format!("EIO: {e}"))?
}
