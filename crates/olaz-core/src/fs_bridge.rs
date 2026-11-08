//! Sandboxed `$znode.fs`, `$znode.fsPromise`, `$znode.fsExtra`, and `$zFileManager`
//! file operations (both synchronous and asynchronous).
//!
//! Port of `Sources/Olaz/ZaloRuntime/FileSystemBridge.swift` and
//! `FileManagerBridge` (`Sources/Olaz/ZaloRuntime/BridgeImplementation.swift:84-175`).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Map, Value};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::crypto::CryptoBridge;
use crate::platform::ZaloPlatform;
use crate::wire;

/// Node-shaped filesystem error formatted as `"<CODE>: <detail>"` so `bridge.js`
/// populates `err.code` (`ENOENT`, `EACCES`, `EEXIST`, `EISDIR`, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{code}: {detail}")]
pub struct FsError {
    pub code: &'static str,
    pub detail: String,
}

impl FsError {
    pub fn enoent(path: impl AsRef<Path>) -> Self {
        Self {
            code: "ENOENT",
            detail: path.as_ref().display().to_string(),
        }
    }

    pub fn eacces(path: impl AsRef<Path>) -> Self {
        Self {
            code: "EACCES",
            detail: path.as_ref().display().to_string(),
        }
    }

    pub fn from_io(err: std::io::Error, path: impl AsRef<Path>) -> Self {
        use std::io::ErrorKind;
        let code = match err.kind() {
            ErrorKind::NotFound => "ENOENT",
            ErrorKind::PermissionDenied => "EACCES",
            ErrorKind::AlreadyExists => "EEXIST",
            ErrorKind::IsADirectory => "EISDIR",
            ErrorKind::NotADirectory => "ENOTDIR",
            ErrorKind::InvalidInput => "EINVAL",
            _ => "EIO",
        };
        Self {
            code,
            detail: format!("{} ({})", path.as_ref().display(), err),
        }
    }
}

/// Bridge implementation for `$znode.fs*`, `$zFileManager.*`, and `$zelectron.store*`.
#[derive(Debug, Clone)]
pub struct FsBridge {
    platform: Arc<ZaloPlatform>,
}

impl FsBridge {
    pub fn new(platform: Arc<ZaloPlatform>) -> Self {
        Self { platform }
    }

    pub fn platform(&self) -> &ZaloPlatform {
        &self.platform
    }

    fn require_writable<'a>(&self, path: &'a Path) -> Result<&'a Path, FsError> {
        if path.as_os_str().is_empty() || !self.platform.is_writable(path) {
            return Err(FsError::eacces(path));
        }
        Ok(path)
    }

    fn require_exists<'a>(&self, path: &'a Path) -> Result<&'a Path, FsError> {
        if path.as_os_str().is_empty() || !path.exists() {
            return Err(FsError::enoent(path));
        }
        Ok(path)
    }

    // ========================================================================
    // Synchronous operations
    // ========================================================================

    pub fn exists_sync(&self, path: &Path) -> bool {
        !path.as_os_str().is_empty() && path.exists()
    }

    pub fn access_sync(&self, path: &Path) -> Result<(), FsError> {
        self.require_exists(path)?;
        Ok(())
    }

    pub fn read_file_sync(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        if path.as_os_str().is_empty() {
            return Err(FsError::enoent(path));
        }
        std::fs::read(path).map_err(|e| FsError::from_io(e, path))
    }

    pub fn write_file_sync(&self, path: &Path, data: &[u8]) -> Result<(), FsError> {
        self.require_writable(path)?;
        let parent = path.parent().ok_or_else(|| FsError::eacces(path))?;
        self.require_writable(parent)?;
        std::fs::create_dir_all(parent).map_err(|e| FsError::from_io(e, parent))?;

        let mut tmp =
            NamedTempFile::new_in(parent).map_err(|e| FsError::from_io(e, parent))?;
        tmp.write_all(data)
            .and_then(|_| tmp.flush())
            .map_err(|e| FsError::from_io(e, path))?;
        tmp.persist(path)
            .map_err(|e| FsError::from_io(e.error, path))?;
        Ok(())
    }

    pub fn mkdir_sync(&self, path: &Path, recursive: bool) -> Result<(), FsError> {
        self.require_writable(path)?;
        if recursive {
            if path.is_dir() {
                return Ok(());
            }
            std::fs::create_dir_all(path).map_err(|e| FsError::from_io(e, path))
        } else {
            std::fs::create_dir(path).map_err(|e| FsError::from_io(e, path))
        }
    }

    pub fn unlink_sync(&self, path: &Path) -> Result<(), FsError> {
        self.require_writable(path)?;
        std::fs::symlink_metadata(path).map_err(|e| FsError::from_io(e, path))?;
        std::fs::remove_file(path).map_err(|e| FsError::from_io(e, path))
    }

    pub fn stat_sync(&self, path: &Path) -> Result<Value, FsError> {
        if path.as_os_str().is_empty() {
            return Err(FsError::enoent(path));
        }
        let meta = std::fs::metadata(path).map_err(|e| FsError::from_io(e, path))?;
        Ok(wire::encode_stats(&meta))
    }

    pub fn lstat_sync(&self, path: &Path) -> Result<Value, FsError> {
        if path.as_os_str().is_empty() {
            return Err(FsError::enoent(path));
        }
        let meta = std::fs::symlink_metadata(path).map_err(|e| FsError::from_io(e, path))?;
        Ok(wire::encode_stats(&meta))
    }

    pub fn readdir_sync(&self, path: &Path) -> Result<Vec<String>, FsError> {
        if path.as_os_str().is_empty() {
            return Err(FsError::enoent(path));
        }
        let rd = std::fs::read_dir(path).map_err(|e| FsError::from_io(e, path))?;
        let mut names = Vec::new();
        for entry in rd {
            let entry = entry.map_err(|e| FsError::from_io(e, path))?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    pub fn rename_sync(&self, from: &Path, to: &Path) -> Result<(), FsError> {
        self.require_writable(from)?;
        self.require_writable(to)?;
        std::fs::symlink_metadata(from).map_err(|e| FsError::from_io(e, from))?;
        if let Some(parent) = to.parent() {
            self.require_writable(parent)?;
            std::fs::create_dir_all(parent).map_err(|e| FsError::from_io(e, parent))?;
        }
        if to.exists() {
            if to.is_dir() {
                std::fs::remove_dir_all(to).map_err(|e| FsError::from_io(e, to))?;
            } else {
                std::fs::remove_file(to).map_err(|e| FsError::from_io(e, to))?;
            }
        }
        match std::fs::rename(from, to) {
            Ok(()) => Ok(()),
            Err(_) => {
                // Cross-device (`EXDEV`) fallback.
                self.copy_file_sync(from, to)?;
                self.rm_sync(from, true, true)
            }
        }
    }

    pub fn copy_file_sync(&self, from: &Path, to: &Path) -> Result<(), FsError> {
        self.require_exists(from)?;
        self.require_writable(to)?;
        if let Some(parent) = to.parent() {
            self.require_writable(parent)?;
            std::fs::create_dir_all(parent).map_err(|e| FsError::from_io(e, parent))?;
        }
        if from.is_dir() {
            copy_dir_recursive(from, to)?;
        } else {
            std::fs::copy(from, to).map_err(|e| FsError::from_io(e, to))?;
        }
        Ok(())
    }

    pub fn rm_sync(&self, path: &Path, recursive: bool, force: bool) -> Result<(), FsError> {
        self.require_writable(path)?;
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && force => return Ok(()),
            Err(e) => return Err(FsError::from_io(e, path)),
        };
        if meta.file_type().is_dir() {
            if recursive {
                std::fs::remove_dir_all(path).map_err(|e| FsError::from_io(e, path))
            } else {
                std::fs::remove_dir(path).map_err(|e| FsError::from_io(e, path))
            }
        } else {
            std::fs::remove_file(path).map_err(|e| FsError::from_io(e, path))
        }
    }

    pub fn link_sync(&self, existing: &Path, dst: &Path) -> Result<(), FsError> {
        self.require_exists(existing)?;
        self.require_writable(dst)?;
        if let Some(parent) = dst.parent() {
            self.require_writable(parent)?;
            std::fs::create_dir_all(parent).map_err(|e| FsError::from_io(e, parent))?;
        }
        std::fs::hard_link(existing, dst).map_err(|e| FsError::from_io(e, dst))
    }

    /// Returns hardlinks of `file` under `scope` (matching inode + device when `st_nlink > 1`).
    pub fn detect_hardlinks_sync(&self, file: &Path, scope: &Path, limit: usize) -> Vec<String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if file.as_os_str().is_empty() || scope.as_os_str().is_empty() {
                return Vec::new();
            }
            let Ok(target) = std::fs::symlink_metadata(file) else {
                return Vec::new();
            };
            if target.nlink() <= 1 {
                return Vec::new();
            }
            let target_ino = target.ino();
            let target_dev = target.dev();
            let mut links = Vec::new();
            let mut seen = 0usize;
            let mut stack = vec![scope.to_path_buf()];
            while let Some(dir) = stack.pop() {
                if seen >= limit {
                    break;
                }
                let Ok(rd) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in rd.flatten() {
                    if seen >= limit {
                        break;
                    }
                    seen += 1;
                    let p = entry.path();
                    let Ok(meta) = std::fs::symlink_metadata(&p) else {
                        continue;
                    };
                    if meta.file_type().is_dir() {
                        stack.push(p);
                    } else if meta.file_type().is_file()
                        && meta.ino() == target_ino
                        && meta.dev() == target_dev
                    {
                        links.push(p.to_string_lossy().into_owned());
                    }
                }
            }
            links.sort();
            links
        }
        #[cfg(not(unix))]
        {
            let _ = (file, scope, limit);
            Vec::new()
        }
    }

    // ========================================================================
    // Asynchronous operations (`tokio::task::spawn_blocking` / `tokio::fs`)
    // ========================================================================

    pub async fn exists(&self, path: impl AsRef<Path>) -> bool {
        let this = self.clone();
        let p = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || this.exists_sync(&p))
            .await
            .unwrap_or(false)
    }

    pub async fn read_file(&self, path: impl AsRef<Path>) -> Result<Vec<u8>, FsError> {
        let this = self.clone();
        let p = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || this.read_file_sync(&p))
            .await
            .map_err(|e| FsError {
                code: "EIO",
                detail: e.to_string(),
            })?
    }

    /// `$zFileManager.readFileAsync(path, encoding?)`.
    pub async fn read_file_async(
        &self,
        path: impl AsRef<Path>,
        encoding: Option<&str>,
    ) -> Result<Value, FsError> {
        let bytes = self.read_file(path).await?;
        Ok(encode_read_result(&bytes, encoding))
    }

    /// `$zFileManager.getFileArrayBuffer(path)` -> `{"__bytes": "<b64>", "__arraybuffer": true}`.
    pub async fn get_file_array_buffer(&self, path: impl AsRef<Path>) -> Result<Value, FsError> {
        let bytes = self.read_file(path).await?;
        Ok(wire::encode_array_buffer(&bytes))
    }

    pub async fn write_file(
        &self,
        path: impl AsRef<Path>,
        data: impl AsRef<[u8]>,
    ) -> Result<(), FsError> {
        let this = self.clone();
        let p = path.as_ref().to_path_buf();
        let buf = data.as_ref().to_vec();
        tokio::task::spawn_blocking(move || this.write_file_sync(&p, &buf))
            .await
            .map_err(|e| FsError {
                code: "EIO",
                detail: e.to_string(),
            })?
    }

    pub async fn mkdir(&self, path: impl AsRef<Path>, recursive: bool) -> Result<(), FsError> {
        let this = self.clone();
        let p = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || this.mkdir_sync(&p, recursive))
            .await
            .map_err(|e| FsError {
                code: "EIO",
                detail: e.to_string(),
            })?
    }

    pub async fn unlink(&self, path: impl AsRef<Path>) -> Result<(), FsError> {
        let this = self.clone();
        let p = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || this.unlink_sync(&p))
            .await
            .map_err(|e| FsError {
                code: "EIO",
                detail: e.to_string(),
            })?
    }

    pub async fn stat(&self, path: impl AsRef<Path>) -> Result<Value, FsError> {
        let this = self.clone();
        let p = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || this.stat_sync(&p))
            .await
            .map_err(|e| FsError {
                code: "EIO",
                detail: e.to_string(),
            })?
    }

    pub async fn lstat(&self, path: impl AsRef<Path>) -> Result<Value, FsError> {
        let this = self.clone();
        let p = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || this.lstat_sync(&p))
            .await
            .map_err(|e| FsError {
                code: "EIO",
                detail: e.to_string(),
            })?
    }

    pub async fn readdir(&self, path: impl AsRef<Path>) -> Result<Vec<String>, FsError> {
        let this = self.clone();
        let p = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || this.readdir_sync(&p))
            .await
            .map_err(|e| FsError {
                code: "EIO",
                detail: e.to_string(),
            })?
    }

    pub async fn rename(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
    ) -> Result<(), FsError> {
        let this = self.clone();
        let f = from.as_ref().to_path_buf();
        let t = to.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || this.rename_sync(&f, &t))
            .await
            .map_err(|e| FsError {
                code: "EIO",
                detail: e.to_string(),
            })?
    }

    pub async fn copy_file(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
    ) -> Result<(), FsError> {
        let this = self.clone();
        let f = from.as_ref().to_path_buf();
        let t = to.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || this.copy_file_sync(&f, &t))
            .await
            .map_err(|e| FsError {
                code: "EIO",
                detail: e.to_string(),
            })?
    }

    pub async fn rm(
        &self,
        path: impl AsRef<Path>,
        recursive: bool,
        force: bool,
    ) -> Result<(), FsError> {
        let this = self.clone();
        let p = path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || this.rm_sync(&p, recursive, force))
            .await
            .map_err(|e| FsError {
                code: "EIO",
                detail: e.to_string(),
            })?
    }

    // ========================================================================
    // Wire dispatchers (`$znode.fs*`, `$zFileManager.*`, `$zelectron.store*`)
    // ========================================================================

    /// Dispatches synchronous bridge calls (`*Sync`, `$zelectron.store*`, `getSentryConfig`, etc.).
    pub fn dispatch_sync(&self, path: &str, args: &[Value]) -> Result<Value, String> {
        match path {
            "$zelectron.storePath" => {
                let name = arg_str(args, 0).unwrap_or("config");
                Ok(Value::String(
                    self.platform.store_path(name).to_string_lossy().into_owned(),
                ))
            }
            "$zelectron.storeRead" => {
                let name = arg_str(args, 0).unwrap_or("config");
                Ok(Value::Object(self.platform.read_store(name)))
            }
            "$zelectron.storeWrite" => {
                let name = arg_str(args, 0).unwrap_or("config");
                let empty = Map::new();
                let obj = args.get(1).and_then(Value::as_object).unwrap_or(&empty);
                self.platform
                    .write_store(name, obj)
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$zFileManager.getSentryConfig" => Ok(wire::undefined()),
            "$znode.fs.accessSync" | "$znode.fs.access" | "$znode.fsPromise.access" => {
                self.access_sync(&arg_path(args, 0))
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$znode.fs.existsSync"
            | "$znode.fsExtra.existsSync"
            | "$znode.fs.exists"
            | "$znode.fsExtra.pathExists" => Ok(Value::Bool(self.exists_sync(&arg_path(args, 0)))),
            "$znode.fs.statSync"
            | "$znode.fsExtra.statSync"
            | "$znode.fs.stat"
            | "$znode.fsPromise.stat" => self
                .stat_sync(&arg_path(args, 0))
                .map_err(|e| e.to_string()),
            "$znode.fs.lstatSync" | "$znode.fs.lstat" | "$znode.fsPromise.lstat" => self
                .lstat_sync(&arg_path(args, 0))
                .map_err(|e| e.to_string()),
            "$znode.fs.mkdirSync" | "$znode.fs.mkdir" | "$znode.fsPromise.mkdir" => {
                let recursive = parse_bool_opt(args.get(1), "recursive", false);
                self.mkdir_sync(&arg_path(args, 0), recursive)
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$znode.fsExtra.ensureDirSync" | "$znode.fsExtra.ensureDir" => {
                self.mkdir_sync(&arg_path(args, 0), true)
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$znode.fs.readdirSync" | "$znode.fs.readdir" | "$znode.fsPromise.readdir" => {
                let names = self
                    .readdir_sync(&arg_path(args, 0))
                    .map_err(|e| e.to_string())?;
                Ok(json!(names))
            }
            "$znode.fs.readFileSync" | "$znode.fs.readFile" | "$znode.fsPromise.readFile" => {
                let bytes = self
                    .read_file_sync(&arg_path(args, 0))
                    .map_err(|e| e.to_string())?;
                let enc = parse_encoding_opt(args.get(1));
                Ok(encode_read_result(&bytes, enc))
            }
            "$znode.fs.writeFileSync" | "$znode.fs.writeFile" | "$znode.fsPromise.writeFile" => {
                let enc = parse_encoding_opt(args.get(2));
                let data = CryptoBridge::decode_input(args.get(1), enc)?;
                self.write_file_sync(&arg_path(args, 0), &data)
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$znode.fs.unlinkSync" | "$znode.fs.unlink" | "$znode.fsPromise.unlink" => {
                self.unlink_sync(&arg_path(args, 0))
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$znode.fs.rmdirSync" | "$znode.fs.rmdir" | "$znode.fsPromise.rmdir" => {
                let recursive = parse_bool_opt(args.get(1), "recursive", false);
                self.rm_sync(&arg_path(args, 0), recursive, false)
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$znode.fs.rmSync" | "$znode.fs.rm" | "$znode.fsPromise.rm" => {
                let recursive = parse_bool_opt(args.get(1), "recursive", false);
                let force = parse_bool_opt(args.get(1), "force", false);
                self.rm_sync(&arg_path(args, 0), recursive, force)
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$znode.fsExtra.removeSync" | "$znode.fsExtra.remove" | "$znode.rimraf" => {
                self.rm_sync(&arg_path(args, 0), true, true)
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$znode.fs.renameSync"
            | "$znode.fsExtra.renameSync"
            | "$znode.fs.rename"
            | "$znode.fsPromise.rename"
            | "$znode.fsExtra.rename"
            | "$znode.fsExtra.move" => {
                self.rename_sync(&arg_path(args, 0), &arg_path(args, 1))
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$znode.fs.copyFileSync"
            | "$znode.fsExtra.copySync"
            | "$znode.fs.copyFile"
            | "$znode.fsPromise.copyFile"
            | "$znode.fsExtra.copy" => {
                self.copy_file_sync(&arg_path(args, 0), &arg_path(args, 1))
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$znode.fs.link" | "$znode.fsPromise.link" => {
                self.link_sync(&arg_path(args, 0), &arg_path(args, 1))
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$znode.nativelibs.fileUtilsAddon.getDiskUsage" => {
                let total: i64 = 64 * 1024 * 1024 * 1024;
                let avail: i64 = 32 * 1024 * 1024 * 1024;
                Ok(json!({
                    "total": total,
                    "free": avail,
                    "available": avail,
                    "used": total - avail,
                }))
            }
            "$znode.nativelibs.fileUtilitiesAddon.detectHardlinksAsync" => {
                let links =
                    self.detect_hardlinks_sync(&arg_path(args, 0), &arg_path(args, 1), 200_000);
                Ok(json!(links))
            }
            _ => self.dispatch_file_manager_sync(path, args),
        }
    }

    /// Dispatches asynchronous bridge calls on a background blocking thread.
    pub async fn dispatch_async(&self, path: &str, args: &[Value]) -> Result<Value, String> {
        let this = self.clone();
        let path_owned = path.to_string();
        let args_owned = args.to_vec();
        tokio::task::spawn_blocking(move || this.dispatch_sync(&path_owned, &args_owned))
            .await
            .map_err(|e| format!("EIO: {e}"))?
    }

    fn dispatch_file_manager_sync(&self, path: &str, args: &[Value]) -> Result<Value, String> {
        let log_dir = self.platform.user_data().join("cal");
        let log_file = |name: &str| -> PathBuf {
            let base = Path::new(name)
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("z.log");
            log_dir.join(base)
        };

        match path {
            "$zFileManager.saveSentryConfig" => Ok(wire::undefined()),
            "$zFileManager.getViewerkey" => {
                let dir = arg_str(args, 0)
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.platform.user_data().to_path_buf());
                let file = dir.join("z_u.txt");
                let bytes = std::fs::read(&file).map_err(|_| "z_u.txt not found".to_string())?;
                Ok(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
            }
            "$zFileManager.writeRecentId" => {
                let content = arg_str(args, 0).unwrap_or("");
                let file = self.platform.user_data().join("z_u.txt");
                self.write_file_sync(&file, content.as_bytes())
                    .map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$zFileManager.writeZLog" => {
                std::fs::create_dir_all(&log_dir).map_err(|e| e.to_string())?;
                let target = log_file(arg_str(args, 0).unwrap_or("z.log"));
                let data = args
                    .get(1)
                    .and_then(wire::decode_bytes_or_utf8)
                    .unwrap_or_default();
                let overwrite = parse_bool_opt(args.get(2), "overwrite", false);
                if overwrite || !target.exists() {
                    self.write_file_sync(&target, &data)
                        .map_err(|e| e.to_string())?;
                } else {
                    let mut f = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&target)
                        .map_err(|e| FsError::from_io(e, &target).to_string())?;
                    f.write_all(&data)
                        .map_err(|e| FsError::from_io(e, &target).to_string())?;
                }
                Ok(wire::undefined())
            }
            "$zFileManager.writeZLogBetween" => {
                std::fs::create_dir_all(&log_dir).map_err(|e| e.to_string())?;
                let target = log_file(arg_str(args, 0).unwrap_or("z.log"));
                let data = args
                    .get(1)
                    .and_then(wire::decode_bytes_or_utf8)
                    .unwrap_or_default();
                let mut f = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&target)
                    .map_err(|e| FsError::from_io(e, &target).to_string())?;
                f.write_all(&data)
                    .map_err(|e| FsError::from_io(e, &target).to_string())?;
                Ok(wire::undefined())
            }
            "$zFileManager.readZLog" => {
                let target = log_file(arg_str(args, 0).unwrap_or(""));
                match std::fs::read(&target) {
                    Ok(bytes) => Ok(Value::String(String::from_utf8_lossy(&bytes).into_owned())),
                    Err(_) => Ok(wire::undefined()),
                }
            }
            "$zFileManager.scandirLog" => {
                let entries = self.readdir_sync(&log_dir).unwrap_or_default();
                Ok(json!(entries))
            }
            "$zFileManager.getExtraLogFiles" => Ok(json!([])),
            "$zFileManager.getQosLog"
            | "$zFileManager.getActionLog"
            | "$zFileManager.readUptimeLog" => Ok(json!("")),
            "$zFileManager.removeQosLog"
            | "$zFileManager.removeActionLog"
            | "$zFileManager.writeActionLog"
            | "$zFileManager.writeUptimeLog"
            | "$zFileManager.truncateFileUpto"
            | "$zFileManager.cleanupLogFileDescriptors"
            | "$zFileManager.initBinaryLogFile"
            | "$zFileManager.sealCalfLog"
            | "$zFileManager.mergeCalfLogGroup"
            | "$zFileManager.writeLogRange"
            | "$zFileManager.trustBlob" => Ok(wire::undefined()),
            "$zFileManager.getInstallSource" => Err("ENOENT: install source".to_string()),
            "$zFileManager.getFileArrayBuffer" => {
                let bytes = self
                    .read_file_sync(&arg_path(args, 0))
                    .map_err(|e| e.to_string())?;
                Ok(wire::encode_array_buffer(&bytes))
            }
            "$zFileManager.readFileAsync" => {
                let bytes = self
                    .read_file_sync(&arg_path(args, 0))
                    .map_err(|e| e.to_string())?;
                let enc = parse_encoding_opt(args.get(1));
                Ok(encode_read_result(&bytes, enc))
            }
            "$zFileManager.svcStatAsync" => {
                let p = arg_path(args, 0);
                let meta = std::fs::metadata(&p).map_err(|e| FsError::from_io(e, &p).to_string())?;
                Ok(json!({ "size": meta.len() }))
            }
            "$zFileManager.writeBufferToPath" => {
                let p = arg_path(args, 0);
                let data = args
                    .get(1)
                    .and_then(wire::decode_bytes_or_utf8)
                    .unwrap_or_default();
                self.write_file_sync(&p, &data).map_err(|e| e.to_string())?;
                Ok(Value::Bool(true))
            }
            other => Err(format!("Unsupported filesystem bridge call: {other}")),
        }
    }
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<(), FsError> {
    std::fs::create_dir_all(to).map_err(|e| FsError::from_io(e, to))?;
    let rd = std::fs::read_dir(from).map_err(|e| FsError::from_io(e, from))?;
    for entry in rd {
        let entry = entry.map_err(|e| FsError::from_io(e, from))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let ft = entry.file_type().map_err(|e| FsError::from_io(e, &src))?;
        if ft.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| FsError::from_io(e, &dst))?;
        }
    }
    Ok(())
}

fn encode_read_result(bytes: &[u8], encoding: Option<&str>) -> Value {
    match encoding.filter(|e| !e.is_empty()) {
        None => wire::encode_bytes(bytes),
        Some(enc) => CryptoBridge::encode_output(bytes, Some(enc)),
    }
}

fn arg_str(args: &[Value], idx: usize) -> Option<&str> {
    args.get(idx).and_then(Value::as_str)
}

fn arg_path(args: &[Value], idx: usize) -> PathBuf {
    PathBuf::from(arg_str(args, idx).unwrap_or(""))
}

fn parse_bool_opt(val: Option<&Value>, key: &str, default: bool) -> bool {
    match val {
        Some(Value::Bool(b)) => *b,
        Some(Value::Object(map)) => map.get(key).and_then(Value::as_bool).unwrap_or(default),
        _ => default,
    }
}

fn parse_encoding_opt(val: Option<&Value>) -> Option<&str> {
    match val {
        Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
        Some(Value::Object(map)) => map
            .get("encoding")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty()),
        _ => None,
    }
}
