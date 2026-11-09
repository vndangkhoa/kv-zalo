//! Pure-Rust port of Zalo's `zwalker` storage garbage-collector native addon (all 5
//! methods + `main.js` `zwalker-*` IPC return shapes + `PlatformPaths` write sandboxing).

use std::collections::{HashMap, HashSet};
use std::fs::Metadata;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use globset::{Glob, GlobSet, GlobSetBuilder};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::fs_bridge::{BridgeResult, PlatformPaths};

/// Per-file metadata tracked in memory during a cleanup cycle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    pub file_name: String,
    pub file_path: String,
    pub size: u64,
    pub atime: i64,
    pub reference_message_id: String,
}

impl FileInfo {
    #[inline]
    pub fn is_homeless(&self) -> bool {
        self.reference_message_id.is_empty()
    }
}

/// Per-tracking-glob aggregate (`snake_case` keys `{ file_number, size }` per `main.js` /
/// `ResourceCleanupManager`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderBasicInfo {
    pub file_number: u32,
    pub size: u64,
}

/// One `{ filePath, id }` pair supplied to `update_reference_message_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceUpdate {
    #[serde(alias = "file_path")]
    pub file_path: String,
    pub id: String,
}

/// In-memory scan tree snapshot populated by `scan_directory` and mutated by
/// `update_reference_message_id` / `delete_homeless_files`.
#[derive(Clone, Debug, Default)]
pub struct TreeState {
    pub root: String,
    pub files: Vec<FileInfo>,
    pub index: HashMap<String, usize>,
}

/// Result of `scan_directory`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub file_number: u32,
    pub size: u64,
    pub tracking_path: Option<String>,
    #[serde(skip)]
    pub tracking: HashMap<String, FolderBasicInfo>,
}

/// Result of `update_reference_message_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateResult {
    pub file_number: u32,
}

/// Result of `stat_unmarked_files`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatResult {
    pub file_number: u32,
    pub size: u64,
    pub tracking_path: Option<String>,
    pub tracking_a_time: Option<String>,
    #[serde(skip)]
    pub tracking: HashMap<String, FolderBasicInfo>,
    #[serde(skip)]
    pub tracking_atime: HashMap<String, Vec<FolderBasicInfo>>,
}

/// Result of `delete_homeless_files`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub file_number: u32,
    pub size: u64,
    pub failed_file_number: u32,
    pub failed_size: u64,
    pub tracking_path: Option<String>,
    #[serde(skip)]
    pub tracking: HashMap<String, FolderBasicInfo>,
}

/// Result of `delete_empty_folders`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyFolderResult {
    pub deleted_count: u32,
    pub deleted_dirs: Vec<String>,
}

/// Instance-able zwalker state container for isolated parallel tests (plus a process-wide
/// default instance [`GLOBAL_ZWALKER`] for IPC).
#[derive(Debug, Default)]
pub struct ZWalkerState {
    tree: Mutex<Option<TreeState>>,
}

/// Process-global zwalker state backing module-level functions and [`handle_zwalker_ipc`].
pub static GLOBAL_ZWALKER: Lazy<ZWalkerState> = Lazy::new(ZWalkerState::new);

fn build_globset(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        if let Ok(g) = Glob::new(pat) {
            builder.add(g);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn atime_secs(md: &Metadata) -> i64 {
    let t = md
        .accessed()
        .or_else(|_| md.modified())
        .unwrap_or(UNIX_EPOCH);
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn aggregate_tracking(
    files: &[FileInfo],
    tracking_globs: &[String],
) -> HashMap<String, FolderBasicInfo> {
    let mut map: HashMap<String, FolderBasicInfo> = HashMap::new();
    let sets: Vec<(String, GlobSet)> = tracking_globs
        .iter()
        .map(|g| (g.clone(), build_globset(std::slice::from_ref(g))))
        .collect();
    for (pat, set) in &sets {
        let entry = map.entry(pat.clone()).or_default();
        for f in files {
            if set.is_match(&f.file_path) {
                entry.file_number += 1;
                entry.size += f.size;
            }
        }
    }
    map
}

fn bucket_for(age: i64, thresholds: &[i64]) -> usize {
    for (idx, &t) in thresholds.iter().enumerate() {
        if age < t {
            return idx;
        }
    }
    thresholds.len()
}

impl ZWalkerState {
    pub fn new() -> Self {
        Self {
            tree: Mutex::new(None),
        }
    }

    /// Walks `root` (without following symlinks or applying gitignore/hidden filters),
    /// populates the in-memory [`TreeState`], and returns total and per-tracking-glob stats.
    pub fn scan_directory(&self, root: &str, tracking_folder_paths: &[String]) -> ScanResult {
        let mut files = Vec::new();
        if !root.is_empty() && Path::new(root).exists() {
            for entry in WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let p = entry.path();
                let Ok(md) = entry.metadata() else {
                    continue;
                };
                if !md.is_file() {
                    continue;
                }
                let file_name = p
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                files.push(FileInfo {
                    file_name,
                    file_path: p.to_string_lossy().into_owned(),
                    size: md.len(),
                    atime: atime_secs(&md),
                    reference_message_id: String::new(),
                });
            }
        }

        files.sort_by(|a, b| a.file_path.cmp(&b.file_path));

        let file_number = files.len() as u32;
        let size: u64 = files.iter().map(|f| f.size).sum();
        let tracking = aggregate_tracking(&files, tracking_folder_paths);
        let tracking_path = serde_json::to_string(&tracking).ok();

        let mut index = HashMap::with_capacity(files.len());
        for (i, f) in files.iter().enumerate() {
            index.insert(f.file_path.clone(), i);
        }

        *self.tree.lock() = Some(TreeState {
            root: root.to_string(),
            files,
            index,
        });

        ScanResult {
            file_number,
            size,
            tracking_path,
            tracking,
        }
    }

    /// Marks referenced files in the active [`TreeState`] with their owning `id`.
    pub fn update_reference_message_id(
        &self,
        _root: &str,
        updates: &[ReferenceUpdate],
    ) -> UpdateResult {
        let mut guard = self.tree.lock();
        let Some(state) = guard.as_mut() else {
            return UpdateResult { file_number: 0 };
        };
        let mut updated = 0u32;
        for u in updates {
            if let Some(&idx) = state.index.get(&u.file_path) {
                state.files[idx].reference_message_id = u.id.clone();
                updated += 1;
            }
        }
        UpdateResult {
            file_number: updated,
        }
    }

    /// Computes read-only statistics and access-time age buckets for unmarked, non-ignored files.
    pub fn stat_unmarked_files(
        &self,
        _root: &str,
        ignore_folder_paths: &[String],
        tracking_folder_paths: &[String],
        age_thresholds: &[i64],
    ) -> StatResult {
        let guard = self.tree.lock();
        let ignore = build_globset(ignore_folder_paths);

        let unmarked: Vec<FileInfo> = guard
            .as_ref()
            .map(|state| {
                state
                    .files
                    .iter()
                    .filter(|f| f.is_homeless() && !ignore.is_match(&f.file_path))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let file_number = unmarked.len() as u32;
        let size: u64 = unmarked.iter().map(|f| f.size).sum();
        let tracking = aggregate_tracking(&unmarked, tracking_folder_paths);
        let tracking_path = serde_json::to_string(&tracking).ok();

        let mut thresholds = age_thresholds.to_vec();
        thresholds.sort_unstable();
        let now = now_secs();
        let n_buckets = thresholds.len() + 1;

        let sets: Vec<(String, GlobSet)> = tracking_folder_paths
            .iter()
            .map(|g| (g.clone(), build_globset(std::slice::from_ref(g))))
            .collect();
        let mut tracking_atime: HashMap<String, Vec<FolderBasicInfo>> = HashMap::new();
        for (pat, set) in &sets {
            let buckets = tracking_atime
                .entry(pat.clone())
                .or_insert_with(|| vec![FolderBasicInfo::default(); n_buckets]);
            for f in &unmarked {
                if set.is_match(&f.file_path) {
                    let age = (now - f.atime).max(0);
                    let b = &mut buckets[bucket_for(age, &thresholds)];
                    b.file_number += 1;
                    b.size += f.size;
                }
            }
        }
        let tracking_a_time = serde_json::to_string(&tracking_atime).ok();

        StatResult {
            file_number,
            size,
            tracking_path,
            tracking_a_time,
            tracking,
            tracking_atime,
        }
    }

    /// Unlinks all homeless (`reference_message_id.is_empty()`), non-ignored files from disk.
    ///
    /// Per `RE-PARAMS.md` & commit `846282b`, `delete_stat_cache` only concerns clearing an
    /// on-disk stat cache (not present in our RAM tree); homeless files are **always**
    /// unlinked regardless of whether `delete_stat_cache` is `Some(true)`, `Some(false)`, or `None`.
    pub fn delete_homeless_files(
        &self,
        root: &str,
        ignore_folder_paths: &[String],
        tracking_folder_paths: &[String],
        delete_stat_cache: Option<bool>,
    ) -> DeleteResult {
        self.delete_homeless_files_filtered(
            root,
            ignore_folder_paths,
            tracking_folder_paths,
            delete_stat_cache,
            None,
        )
    }

    fn delete_homeless_files_filtered(
        &self,
        root: &str,
        ignore_folder_paths: &[String],
        tracking_folder_paths: &[String],
        _delete_stat_cache: Option<bool>,
        sandbox: Option<&dyn PlatformPaths>,
    ) -> DeleteResult {
        let mut guard = self.tree.lock();
        let ignore = build_globset(ignore_folder_paths);
        let root_path = Path::new(root);

        let candidates: Vec<FileInfo> = guard
            .as_ref()
            .map(|state| {
                state
                    .files
                    .iter()
                    .filter(|f| f.is_homeless() && !ignore.is_match(&f.file_path))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let mut deleted = Vec::new();
        let mut removed_paths = HashSet::new();
        let mut failed_file_number = 0u32;
        let mut failed_size = 0u64;

        for f in candidates {
            let fp = Path::new(&f.file_path);
            let allowed = match sandbox {
                Some(platform) => {
                    (!root.is_empty() && fp.starts_with(root_path)) && platform.is_writable(fp)
                }
                None => true,
            };
            if !allowed {
                failed_file_number += 1;
                failed_size += f.size;
                continue;
            }
            match std::fs::remove_file(fp) {
                Ok(()) => {
                    removed_paths.insert(f.file_path.clone());
                    deleted.push(f);
                }
                Err(_) => {
                    failed_file_number += 1;
                    failed_size += f.size;
                }
            }
        }

        let file_number = deleted.len() as u32;
        let size: u64 = deleted.iter().map(|f| f.size).sum();
        let tracking = aggregate_tracking(&deleted, tracking_folder_paths);
        let tracking_path = serde_json::to_string(&tracking).ok();

        if !removed_paths.is_empty() {
            if let Some(state) = guard.as_mut() {
                state.files.retain(|f| !removed_paths.contains(&f.file_path));
                state.index.clear();
                for (i, f) in state.files.iter().enumerate() {
                    state.index.insert(f.file_path.clone(), i);
                }
            }
        }

        DeleteResult {
            file_number,
            size,
            failed_file_number,
            failed_size,
            tracking_path,
            tracking,
        }
    }

    /// Removes empty subdirectories under `root` deepest-first (never removing `root` itself).
    pub fn delete_empty_folders(&self, root: &str) -> EmptyFolderResult {
        delete_empty_folders_filtered(root, None)
    }
}

fn delete_empty_folders_filtered(
    root: &str,
    sandbox: Option<&dyn PlatformPaths>,
) -> EmptyFolderResult {
    let root_path = Path::new(root);
    if root.is_empty() || !root_path.exists() {
        return EmptyFolderResult {
            deleted_count: 0,
            deleted_dirs: Vec::new(),
        };
    }

    let mut dirs: Vec<String> = Vec::new();
    for entry in WalkDir::new(root_path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_dir() && entry.path() != root_path {
            dirs.push(entry.path().to_string_lossy().into_owned());
        }
    }

    dirs.sort_by(|a, b| {
        let depth_a = Path::new(a).components().count();
        let depth_b = Path::new(b).components().count();
        depth_b.cmp(&depth_a).then_with(|| a.cmp(b))
    });

    let mut deleted_dirs = Vec::new();
    for d in dirs {
        let dp = Path::new(&d);
        if let Some(platform) = sandbox {
            if !dp.starts_with(root_path) || !platform.is_writable(dp) {
                continue;
            }
        }
        if std::fs::remove_dir(dp).is_ok() {
            deleted_dirs.push(d);
        }
    }

    EmptyFolderResult {
        deleted_count: deleted_dirs.len() as u32,
        deleted_dirs,
    }
}

/// Scans `root` into the process-global [`GLOBAL_ZWALKER`] tree.
pub fn scan_directory(root: &str, tracking_folder_paths: &[String]) -> ScanResult {
    GLOBAL_ZWALKER.scan_directory(root, tracking_folder_paths)
}

/// Marks referenced files in the process-global [`GLOBAL_ZWALKER`] tree.
pub fn update_reference_message_id(root: &str, updates: &[ReferenceUpdate]) -> UpdateResult {
    GLOBAL_ZWALKER.update_reference_message_id(root, updates)
}

/// Reports unmarked, non-ignored files and age-bucket breakdowns from [`GLOBAL_ZWALKER`].
pub fn stat_unmarked_files(
    root: &str,
    ignore_folder_paths: &[String],
    tracking_folder_paths: &[String],
    age_thresholds: &[i64],
) -> StatResult {
    GLOBAL_ZWALKER.stat_unmarked_files(
        root,
        ignore_folder_paths,
        tracking_folder_paths,
        age_thresholds,
    )
}

/// Unlinks homeless, non-ignored files tracked in [`GLOBAL_ZWALKER`].
pub fn delete_homeless_files(
    root: &str,
    ignore_folder_paths: &[String],
    tracking_folder_paths: &[String],
    delete_stat_cache: Option<bool>,
) -> DeleteResult {
    GLOBAL_ZWALKER.delete_homeless_files(
        root,
        ignore_folder_paths,
        tracking_folder_paths,
        delete_stat_cache,
    )
}

/// Deletes empty folders under `root` deepest-first.
pub fn delete_empty_folders(root: &str) -> EmptyFolderResult {
    GLOBAL_ZWALKER.delete_empty_folders(root)
}

fn parse_string_vec(val: Option<&Value>) -> Vec<String> {
    val.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_i64_vec(val: Option<&Value>) -> Vec<i64> {
    val.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
                .collect()
        })
        .unwrap_or_default()
}

fn extract_root_arg(args: &[Value]) -> Result<String, String> {
    if let Some(s) = args.first().and_then(Value::as_str) {
        return Ok(s.to_string());
    }
    if let Some(obj) = args.first().and_then(Value::as_object) {
        for key in ["rootPath", "root", "dirPath", "path"] {
            if let Some(s) = obj.get(key).and_then(Value::as_str) {
                return Ok(s.to_string());
            }
        }
    }
    Err("zwalker root path argument must be a non-empty string".to_string())
}

fn require_writable_root(platform: &dyn PlatformPaths, root: &str) -> Result<(), String> {
    let p = Path::new(root);
    if root.is_empty() || !platform.is_writable(p) {
        return Err(format!(
            "EACCES: zwalker path '{root}' is outside writable userData sandbox"
        ));
    }
    Ok(())
}

fn require_readable_or_writable_root(
    platform: &dyn PlatformPaths,
    root: &str,
) -> Result<(), String> {
    let p = Path::new(root);
    if root.is_empty() || (!platform.is_writable(p) && !platform.is_readable(p)) {
        return Err(format!(
            "EACCES: zwalker path '{root}' is outside sandbox"
        ));
    }
    Ok(())
}

/// Dispatches all 5 `zwalker-*` IPC channels against an explicit [`ZWalkerState`], enforcing
/// `platform` sandbox boundaries and returning the exact shapes expected by `main.js`.
pub fn handle_zwalker_ipc_with_state(
    state: &ZWalkerState,
    platform: &dyn PlatformPaths,
    channel: &str,
    args: &[Value],
) -> BridgeResult {
    match channel {
        "zwalker-collect-dir-stats" => {
            let root = extract_root_arg(args)?;
            require_readable_or_writable_root(platform, &root)?;
            let tracking_paths = args
                .first()
                .and_then(Value::as_object)
                .and_then(|o| o.get("trackingFolderPaths"))
                .map(|v| parse_string_vec(Some(v)))
                .unwrap_or_else(|| parse_string_vec(args.get(1)));
            let res = state.scan_directory(&root, &tracking_paths);
            let tracking_data =
                serde_json::to_value(&res.tracking).unwrap_or_else(|_| json!({}));
            Ok(json!({
                "fileNumber": res.file_number,
                "size": res.size,
                "trackingFolderData": tracking_data,
            }))
        }
        "zwalker-update-file-stats" => {
            let root = extract_root_arg(args).unwrap_or_default();
            if !root.is_empty() {
                require_readable_or_writable_root(platform, &root)?;
            }
            let updates_val = args
                .first()
                .and_then(Value::as_object)
                .and_then(|o| o.get("updates"))
                .or_else(|| args.get(1));
            let updates: Vec<ReferenceUpdate> = updates_val
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| serde_json::from_value(item.clone()).ok())
                        .collect()
                })
                .unwrap_or_default();
            let res = state.update_reference_message_id(&root, &updates);
            Ok(json!({
                "updateCount": res.file_number,
            }))
        }
        "zwalker-del-unmarked-files" => {
            let root = extract_root_arg(args)?;
            require_writable_root(platform, &root)?;
            let obj = args.first().and_then(Value::as_object);
            let ignore_paths = obj
                .and_then(|o| o.get("ignoreFolderPaths"))
                .map(|v| parse_string_vec(Some(v)))
                .unwrap_or_else(|| parse_string_vec(args.get(1)));
            let tracking_paths = obj
                .and_then(|o| o.get("trackingFolderPaths"))
                .map(|v| parse_string_vec(Some(v)))
                .unwrap_or_else(|| parse_string_vec(args.get(2)));
            let delete_stat_cache = obj
                .and_then(|o| o.get("deleteStatCache"))
                .and_then(Value::as_bool)
                .or_else(|| args.get(3).and_then(Value::as_bool));
            let res = state.delete_homeless_files_filtered(
                &root,
                &ignore_paths,
                &tracking_paths,
                delete_stat_cache,
                Some(platform),
            );
            let tracking_data =
                serde_json::to_value(&res.tracking).unwrap_or_else(|_| json!({}));
            Ok(json!({
                "fileNumber": res.file_number,
                "size": res.size,
                "failedFileNumber": res.failed_file_number,
                "failedSize": res.failed_size,
                "trackingFolderData": tracking_data,
            }))
        }
        "zwalker-stat-unmarked-files" => {
            let root = extract_root_arg(args)?;
            require_readable_or_writable_root(platform, &root)?;
            let obj = args.first().and_then(Value::as_object);
            let ignore_paths = obj
                .and_then(|o| o.get("ignoreFolderPaths"))
                .map(|v| parse_string_vec(Some(v)))
                .unwrap_or_else(|| parse_string_vec(args.get(1)));
            let tracking_paths = obj
                .and_then(|o| o.get("trackingFolderPaths"))
                .map(|v| parse_string_vec(Some(v)))
                .unwrap_or_else(|| parse_string_vec(args.get(2)));
            let age_thresholds = obj
                .and_then(|o| o.get("ageThresholds"))
                .map(|v| parse_i64_vec(Some(v)))
                .unwrap_or_else(|| parse_i64_vec(args.get(3)));
            let res = state.stat_unmarked_files(
                &root,
                &ignore_paths,
                &tracking_paths,
                &age_thresholds,
            );
            let tracking_data =
                serde_json::to_value(&res.tracking).unwrap_or_else(|_| json!({}));
            let tracking_atime =
                serde_json::to_value(&res.tracking_atime).unwrap_or_else(|_| json!({}));
            Ok(json!({
                "fileNumber": res.file_number,
                "size": res.size,
                "trackingFolderData": tracking_data,
                "trackingATime": tracking_atime,
            }))
        }
        "zwalker-delete-empty-folders" => {
            let root = extract_root_arg(args)?;
            require_writable_root(platform, &root)?;
            let res = delete_empty_folders_filtered(&root, Some(platform));
            Ok(json!({
                "deletedCount": res.deleted_count,
                "deletedDirs": res.deleted_dirs,
            }))
        }
        other => Err(format!("Unsupported zwalker IPC channel: {other}")),
    }
}

/// Dispatches all 5 `zwalker-*` IPC channels against [`GLOBAL_ZWALKER`].
pub fn handle_zwalker_ipc(
    platform: &dyn PlatformPaths,
    channel: &str,
    args: &[Value],
) -> BridgeResult {
    handle_zwalker_ipc_with_state(&GLOBAL_ZWALKER, platform, channel, args)
}
