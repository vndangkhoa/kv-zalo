//! Sandboxed `$znode.fs`, `$znode.fsPromise`, `$znode.fsExtra`, and `$zFileManager`
//! file operations (both synchronous and asynchronous).
//!
//! Port of `Sources/Olaz/ZaloRuntime/FileSystemBridge.swift` and
//! `FileManagerBridge` (`Sources/Olaz/ZaloRuntime/BridgeImplementation.swift:84-175`).

use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use digest::Digest;
use md5::Md5;
use serde_json::{json, Map, Value};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::crypto::CryptoBridge;
use crate::platform::ZaloPlatform;
use crate::wire;

/// Standard JSON return type for synchronous and asynchronous host bridge handlers.
pub type BridgeResult = Result<Value, String>;

/// Sandbox path capability interface shared across native module ports (`file_utilities`,
/// `zwalker`, `db_cross`, `fs_bridge`).
pub trait PlatformPaths: Send + Sync {
    fn user_data(&self) -> &Path;
    fn temp_dir(&self) -> &Path;
    fn is_writable(&self, path: &Path) -> bool;
    fn is_readable(&self, path: &Path) -> bool;
}

impl PlatformPaths for ZaloPlatform {
    fn user_data(&self) -> &Path {
        ZaloPlatform::user_data(self)
    }

    fn temp_dir(&self) -> &Path {
        ZaloPlatform::temp_dir(self)
    }

    fn is_writable(&self, path: &Path) -> bool {
        ZaloPlatform::is_writable(self, path)
    }

    fn is_readable(&self, path: &Path) -> bool {
        ZaloPlatform::is_readable(self, path)
    }
}

impl<T: PlatformPaths + ?Sized> PlatformPaths for Arc<T> {
    fn user_data(&self) -> &Path {
        (**self).user_data()
    }

    fn temp_dir(&self) -> &Path {
        (**self).temp_dir()
    }

    fn is_writable(&self, path: &Path) -> bool {
        (**self).is_writable(path)
    }

    fn is_readable(&self, path: &Path) -> bool {
        (**self).is_readable(path)
    }
}

const ZALO_NOISE_SIGN: [u8; 6] = [0x07, 0x7a, 0x41, 0x6c, 0x4f, 0x07];
const ZALO_NOISE_XOR_KEY: u8 = 0x93;
const ZALO_NOISE_XOR_LEN: u64 = 100;

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
        if !parent.is_dir() {
            self.require_writable(parent)?;
            std::fs::create_dir_all(parent).map_err(|e| FsError::from_io(e, parent))?;
        }

        match NamedTempFile::new_in(parent) {
            Ok(mut tmp) => {
                tmp.write_all(data)
                    .and_then(|_| tmp.flush())
                    .map_err(|e| FsError::from_io(e, path))?;
                tmp.persist(path)
                    .map_err(|e| FsError::from_io(e.error, path))?;
            }
            Err(_) => {
                // Fallback for XDG document-portal FUSE mounts (`/run/user/<uid>/doc/<id>/file`)
                // that permit opening the granted target path directly while forbidding sibling `.tmp*` files.
                let mut f = File::create(path).map_err(|e| FsError::from_io(e, path))?;
                f.write_all(data)
                    .and_then(|_| f.flush())
                    .map_err(|e| FsError::from_io(e, path))?;
            }
        }
        Ok(())
    }

    pub fn mkdir_sync(&self, path: &Path, recursive: bool) -> Result<(), FsError> {
        if recursive && path.is_dir() {
            return Ok(());
        }
        self.require_writable(path)?;
        if recursive {
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
            if !parent.is_dir() {
                self.require_writable(parent)?;
                std::fs::create_dir_all(parent).map_err(|e| FsError::from_io(e, parent))?;
            }
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
            if !parent.is_dir() {
                self.require_writable(parent)?;
                std::fs::create_dir_all(parent).map_err(|e| FsError::from_io(e, parent))?;
            }
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
            if !parent.is_dir() {
                self.require_writable(parent)?;
                std::fs::create_dir_all(parent).map_err(|e| FsError::from_io(e, parent))?;
            }
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
            "$znode.nativelibs.fileUtilsAddon.getDiskUsage" => file_utils_get_disk_usage(args),
            "$znode.nativelibs.zfileAddon.getDiskInfo" => zfile_get_disk_info(),
            "$znode.nativelibs.zfileAddon.getInfo" => zfile_get_info(args),
            "$znode.nativelibs.zfileAddon.canRead" => {
                let ok = arg_str(args, 0).is_some_and(|raw| !raw.is_empty() && Path::new(raw).exists());
                Ok(Value::Bool(ok))
            }
            "$znode.nativelibs.zfileAddon.canWrite"
            | "$znode.nativelibs.zfileAddon.canReadAndWrite" => {
                let ok = arg_str(args, 0).is_some_and(|raw| {
                    if raw.is_empty() {
                        return false;
                    }
                    let target = Path::new(raw);
                    self.platform.is_writable(target)
                        || target
                            .parent()
                            .is_some_and(|par| par.exists() && self.platform.is_writable(par))
                });
                Ok(Value::Bool(ok))
            }
            p if p.starts_with("$znode.nativelibs.fileUtilitiesAddon.") => {
                let method = &p["$znode.nativelibs.fileUtilitiesAddon.".len()..];
                crate::file_utilities::dispatch_file_utilities_sync(&*self.platform, method, args)
            }
            p if p.starts_with("$znode.nativelibs.dbUtilsAddon.") => {
                let method = &p["$znode.nativelibs.dbUtilsAddon.".len()..];
                crate::db_cross::dispatch_db_utils_sync(&*self.platform, method, args)
            }
            _ => self.dispatch_file_manager_sync(path, args),
        }
    }

    /// Dispatches `zwalker-*` IPC channels against this bridge's confined platform paths.
    pub fn dispatch_zwalker_ipc(&self, channel: &str, args: &[Value]) -> BridgeResult {
        crate::zwalker::handle_zwalker_ipc(&*self.platform, channel, args)
    }

    /// Dispatches asynchronous bridge calls on a background blocking thread.
    pub async fn dispatch_async(&self, path: &str, args: &[Value]) -> Result<Value, String> {
        if let Some(method) = path.strip_prefix("$znode.nativelibs.fileUtilitiesAddon.") {
            return crate::file_utilities::dispatch_file_utilities_async(
                &*self.platform,
                method,
                args,
            )
            .await;
        }
        if let Some(method) = path.strip_prefix("$znode.nativelibs.dbUtilsAddon.") {
            return crate::db_cross::dispatch_db_utils_async(&*self.platform, method, args).await;
        }
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
            "$zFileManager.readLogRange" => {
                let len = args
                    .get(2)
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    .min(1024 * 1024) as usize;
                Ok(wire::encode_bytes(&vec![0u8; len]))
            }
            "$zFileManager.isTrustPath" | "$zFileManager.trustPathFromBlob" => {
                Ok(Value::Bool(true))
            }
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
                let (logical_len, _) = inspect_file_noise(&p, is_noise_candidate_path(&p))
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "size": logical_len }))
            }
            "$zFileManager.svcReadPath" => {
                let p = arg_path(args, 0);
                let opts = args.get(1);
                let start = opts
                    .and_then(|v| v.get("start"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let end_inclusive = opts
                    .and_then(|v| v.get("end"))
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX);
                let buf = read_file_range_sync(&p, start, end_inclusive, is_noise_candidate_path(&p))
                    .map_err(|e| e.to_string())?;
                Ok(wire::encode_array_buffer(&buf))
            }
            "$zFileManager.readFileSlice" => {
                let p = arg_path(args, 0);
                let offset = args.get(1).and_then(Value::as_u64).unwrap_or(0);
                let length = args.get(2).and_then(Value::as_u64).unwrap_or(0);
                let check_noise = args
                    .get(3)
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| is_noise_candidate_path(&p));
                if length == 0 {
                    return Ok(wire::encode_array_buffer(&[]));
                }
                let end_inclusive = offset.saturating_add(length.saturating_sub(1));
                let buf = read_file_range_sync(&p, offset, end_inclusive, check_noise)
                    .map_err(|e| e.to_string())?;
                Ok(wire::encode_array_buffer(&buf))
            }
            "$zFileManager.zfileFindNoiseSignature" => {
                let p = arg_path(args, 0);
                let (_, sig) = inspect_file_noise(&p, true).map_err(|e| e.to_string())?;
                Ok(match sig {
                    Some(off) => json!(off),
                    None => wire::undefined(),
                })
            }
            "$zFileManager.getMd5LargeFileObject" => {
                let p = arg_path(args, 0);
                let hex = compute_file_md5_sync(&p, false).map_err(|e| e.to_string())?;
                Ok(Value::String(hex))
            }
            "$zFileManager.zfileGetChecksum" => {
                let p = arg_path(args, 0);
                let hex = compute_file_md5_sync(&p, is_noise_candidate_path(&p))
                    .map_err(|e| e.to_string())?;
                Ok(Value::String(hex))
            }
            "$zFileManager.testChecksum" => {
                let p = arg_path(args, 0);
                let expected = arg_str(args, 1).unwrap_or("").trim();
                let check_noise = args.get(2).and_then(Value::as_bool).unwrap_or(false);
                if expected.is_empty() {
                    return Ok(Value::Bool(false));
                }
                match compute_file_md5_sync(&p, check_noise) {
                    Ok(actual) => Ok(Value::Bool(actual.eq_ignore_ascii_case(expected))),
                    Err(_) => Ok(Value::Bool(false)),
                }
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
            "$zFileManager.appendBufferToPath" => {
                let p = arg_path(args, 0);
                let data = args
                    .get(1)
                    .and_then(wire::decode_bytes_or_utf8)
                    .unwrap_or_default();
                self.require_writable(&p).map_err(|e| e.to_string())?;
                if let Some(parent) = p.parent() {
                    if !parent.is_dir() {
                        self.require_writable(parent).map_err(|e| e.to_string())?;
                        std::fs::create_dir_all(parent)
                            .map_err(|e| FsError::from_io(e, parent).to_string())?;
                    }
                }
                let mut f = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&p)
                    .map_err(|e| FsError::from_io(e, &p).to_string())?;
                f.write_all(&data)
                    .map_err(|e| FsError::from_io(e, &p).to_string())?;
                Ok(wire::undefined())
            }
            "$zFileManager.zcloudCloneAndMatchChecksum" => {
                let expected = arg_str(args, 0).unwrap_or("").trim();
                let src = arg_path(args, 1);
                if expected.len() != 32 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Ok(Value::String(String::new()));
                }
                if !src.is_file() {
                    return Ok(Value::String(String::new()));
                }
                let stem = src
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("zcloud");
                let ext = src
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|e| format!(".{e}"))
                    .unwrap_or_default();
                let tmp_name = format!(".{stem}_tmp{ext}");
                let dest_dir = src
                    .parent()
                    .filter(|d| self.platform.is_writable(d))
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.platform.temp_dir().to_path_buf());
                let _ = std::fs::create_dir_all(&dest_dir);
                let dest = dest_dir.join(tmp_name);
                if std::fs::copy(&src, &dest).is_err() {
                    return Ok(Value::String(String::new()));
                }
                match compute_file_md5_sync(&dest, false) {
                    Ok(actual) if actual.eq_ignore_ascii_case(expected) => {
                        self.platform.grant_writable_path(&dest);
                        Ok(Value::String(dest.to_string_lossy().into_owned()))
                    }
                    _ => {
                        let _ = std::fs::remove_file(&dest);
                        Ok(Value::String(String::new()))
                    }
                }
            }
            "$zFileManager.zcloudDenoiseFile" => {
                let src = arg_path(args, 0);
                let dest = arg_path(args, 1);
                let checksum = arg_str(args, 2).unwrap_or("").trim();
                self.require_writable(&dest).map_err(|e| e.to_string())?;
                if let Some(parent) = dest.parent() {
                    if !parent.is_dir() {
                        self.require_writable(parent).map_err(|e| e.to_string())?;
                        std::fs::create_dir_all(parent)
                            .map_err(|e| FsError::from_io(e, parent).to_string())?;
                    }
                }
                denoise_file_sync(&src, &dest, checksum).map_err(|e| e.to_string())?;
                Ok(wire::undefined())
            }
            "$zFileManager.zcloudNoiseFile" => {
                let src = arg_path(args, 0);
                let dest = arg_path(args, 1);
                let checksum = arg_str(args, 2).unwrap_or("").trim();
                self.require_writable(&dest).map_err(|e| e.to_string())?;
                if let Some(parent) = dest.parent() {
                    if !parent.is_dir() {
                        self.require_writable(parent).map_err(|e| e.to_string())?;
                        std::fs::create_dir_all(parent)
                            .map_err(|e| FsError::from_io(e, parent).to_string())?;
                    }
                }
                noise_file_sync(&src, &dest, checksum).map_err(|e| e.to_string())?;
                Ok(json!({ "savePath": dest.to_string_lossy() }))
            }
            "$zFileManager.generateVideoThumbnail"
            | "$znode.nativelibs.mp4thumbAddon.getThumbnail" => {
                let src_raw = arg_str(args, 0).unwrap_or("").trim();
                let src = Path::new(src_raw);
                let dest = arg_path(args, 1);
                let max_w = args
                    .get(2)
                    .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
                    .unwrap_or(640)
                    .clamp(16, 4096) as u32;
                let max_h = args
                    .get(3)
                    .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
                    .unwrap_or(if path == "$znode.nativelibs.mp4thumbAddon.getThumbnail" {
                        max_w as u64
                    } else {
                        640
                    })
                    .clamp(16, 4096) as u32;
                let is_remote = is_remote_http_url(src_raw);
                if src_raw.is_empty()
                    || dest.as_os_str().is_empty()
                    || (!is_remote && !src.is_file())
                {
                    return Ok(Value::String(String::new()));
                }
                self.require_writable(&dest).map_err(|e| e.to_string())?;
                if let Some(parent) = dest.parent() {
                    self.require_writable(parent).map_err(|e| e.to_string())?;
                    std::fs::create_dir_all(parent)
                        .map_err(|e| FsError::from_io(e, parent).to_string())?;
                }
                match generate_video_thumbnail_sync(
                    src,
                    &dest,
                    max_w,
                    max_h,
                    self.platform.temp_dir(),
                ) {
                    Ok(true) => Ok(Value::String(dest.to_string_lossy().into_owned())),
                    _ => Ok(Value::String(String::new())),
                }
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

fn is_noise_candidate_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("fileNoise")
}

/// Inspects `path`, returning `(logical_len, noise_signature_offset)`.
/// When `check_noise` is true and the file ends with `077a416c4f07` (`ZALO_NOISE_SIGN`),
/// the 6-byte trailer is excluded from `logical_len` and `Some(raw_len - 6)` is returned.
fn inspect_file_noise(path: &Path, check_noise: bool) -> Result<(u64, Option<u64>), FsError> {
    if path.as_os_str().is_empty() {
        return Err(FsError::enoent(path));
    }
    let mut f = File::open(path).map_err(|e| FsError::from_io(e, path))?;
    let meta = f.metadata().map_err(|e| FsError::from_io(e, path))?;
    if !meta.is_file() {
        return Err(FsError {
            code: "EISDIR",
            detail: path.display().to_string(),
        });
    }
    let raw_len = meta.len();
    if check_noise && raw_len > ZALO_NOISE_SIGN.len() as u64 {
        let sig_off = raw_len - ZALO_NOISE_SIGN.len() as u64;
        if f.seek(SeekFrom::Start(sig_off)).is_ok() {
            let mut tail = [0u8; 6];
            if f.read_exact(&mut tail).is_ok() && tail == ZALO_NOISE_SIGN {
                return Ok((sig_off, Some(sig_off)));
            }
        }
    }
    Ok((raw_len, None))
}

fn apply_noise_xor_at_offset(file_offset: u64, buf: &mut [u8]) {
    if file_offset >= ZALO_NOISE_XOR_LEN || buf.is_empty() {
        return;
    }
    let xor_end = ((ZALO_NOISE_XOR_LEN - file_offset) as usize).min(buf.len());
    for b in &mut buf[..xor_end] {
        *b ^= ZALO_NOISE_XOR_KEY;
    }
}

fn read_file_range_sync(
    path: &Path,
    start: u64,
    end_inclusive: u64,
    check_noise: bool,
) -> Result<Vec<u8>, FsError> {
    let (logical_len, noise_sig) = inspect_file_noise(path, check_noise)?;
    if logical_len == 0 || start >= logical_len || end_inclusive < start {
        return Ok(Vec::new());
    }
    let clamped_end = end_inclusive.min(logical_len - 1);
    let to_read = (clamped_end - start + 1) as usize;
    let mut f = File::open(path).map_err(|e| FsError::from_io(e, path))?;
    f.seek(SeekFrom::Start(start))
        .map_err(|e| FsError::from_io(e, path))?;
    let mut buf = Vec::with_capacity(to_read.min(8 * 1024 * 1024));
    f.take(to_read as u64)
        .read_to_end(&mut buf)
        .map_err(|e| FsError::from_io(e, path))?;
    if noise_sig.is_some() {
        apply_noise_xor_at_offset(start, &mut buf);
    }
    Ok(buf)
}

fn compute_file_md5_sync(path: &Path, check_noise: bool) -> Result<String, FsError> {
    let (logical_len, noise_sig) = inspect_file_noise(path, check_noise)?;
    let mut f = File::open(path).map_err(|e| FsError::from_io(e, path))?;
    let mut hasher = Md5::new();
    let mut remaining = logical_len;
    let mut offset = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let want = (remaining as usize).min(chunk.len());
        let n = f
            .read(&mut chunk[..want])
            .map_err(|e| FsError::from_io(e, path))?;
        if n == 0 {
            break;
        }
        if noise_sig.is_some() {
            apply_noise_xor_at_offset(offset, &mut chunk[..n]);
        }
        hasher.update(&chunk[..n]);
        offset += n as u64;
        remaining -= n as u64;
    }
    Ok(hex::encode(hasher.finalize()))
}

fn denoise_file_sync(src: &Path, dest: &Path, expected_checksum: &str) -> Result<(), FsError> {
    let (logical_len, noise_sig) = inspect_file_noise(src, true)?;
    let has_noise = noise_sig.is_some();
    let mut fin = File::open(src).map_err(|e| FsError::from_io(e, src))?;
    let mut fout = File::create(dest).map_err(|e| FsError::from_io(e, dest))?;
    let mut hasher = (!expected_checksum.is_empty()).then(Md5::new);
    let mut remaining = logical_len;
    let mut offset = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let want = (remaining as usize).min(chunk.len());
        let n = fin
            .read(&mut chunk[..want])
            .map_err(|e| FsError::from_io(e, src))?;
        if n == 0 {
            break;
        }
        if has_noise {
            apply_noise_xor_at_offset(offset, &mut chunk[..n]);
        }
        if let Some(h) = hasher.as_mut() {
            h.update(&chunk[..n]);
        }
        fout.write_all(&chunk[..n])
            .map_err(|e| FsError::from_io(e, dest))?;
        offset += n as u64;
        remaining -= n as u64;
    }
    fout.flush().map_err(|e| FsError::from_io(e, dest))?;
    if let Some(h) = hasher {
        let actual = hex::encode(h.finalize());
        if !actual.eq_ignore_ascii_case(expected_checksum) {
            let _ = std::fs::remove_file(dest);
            return Err(FsError {
                code: "EIO",
                detail: format!("COPY_SERVICE_CHECKSUM_MISMATCH: expected {expected_checksum}, got {actual}"),
            });
        }
    }
    Ok(())
}

fn noise_file_sync(src: &Path, dest: &Path, expected_checksum: &str) -> Result<(), FsError> {
    let (logical_len, noise_sig) = inspect_file_noise(src, true)?;
    let src_already_noised = noise_sig.is_some();
    let mut fin = File::open(src).map_err(|e| FsError::from_io(e, src))?;
    let mut fout = File::create(dest).map_err(|e| FsError::from_io(e, dest))?;
    let mut hasher = (!expected_checksum.is_empty()).then(Md5::new);
    let mut remaining = logical_len;
    let mut offset = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let want = (remaining as usize).min(chunk.len());
        let n = fin
            .read(&mut chunk[..want])
            .map_err(|e| FsError::from_io(e, src))?;
        if n == 0 {
            break;
        }
        if src_already_noised {
            apply_noise_xor_at_offset(offset, &mut chunk[..n]);
        }
        if let Some(h) = hasher.as_mut() {
            h.update(&chunk[..n]);
        }
        apply_noise_xor_at_offset(offset, &mut chunk[..n]);
        fout.write_all(&chunk[..n])
            .map_err(|e| FsError::from_io(e, dest))?;
        offset += n as u64;
        remaining -= n as u64;
    }
    if let Some(h) = hasher {
        let actual = hex::encode(h.finalize());
        if !actual.eq_ignore_ascii_case(expected_checksum) {
            let _ = std::fs::remove_file(dest);
            return Err(FsError {
                code: "EIO",
                detail: format!("COPY_SERVICE_CHECKSUM_MISMATCH: expected {expected_checksum}, got {actual}"),
            });
        }
    }
    fout.write_all(&ZALO_NOISE_SIGN)
        .map_err(|e| FsError::from_io(e, dest))?;
    fout.flush().map_err(|e| FsError::from_io(e, dest))?;
    Ok(())
}

/// Copies `src` to `dest`, ensuring the destination is either `fileNoise`-encoded
/// (`want_noise_output == true`, idempotent if `src` is already noised) or clean
/// plaintext (`want_noise_output == false`, stripping `fileNoise` if `src` has it).
pub fn copy_with_noise_mode(
    src: &Path,
    dest: &Path,
    want_noise_output: bool,
    expected_checksum: &str,
) -> Result<(), FsError> {
    if want_noise_output {
        noise_file_sync(src, dest, expected_checksum)
    } else {
        denoise_file_sync(src, dest, expected_checksum)
    }
}

fn is_remote_http_url(raw: &str) -> bool {
    raw.starts_with("http://") || raw.starts_with("https://")
}

/// Real `libc::statvfs` implementation of `$znode.nativelibs.fileUtilsAddon.getDiskUsage`.
pub fn file_utils_get_disk_usage(args: &[Value]) -> BridgeResult {
    if args.len() != 1 {
        return Err("DISKUSAGE_WRONG_NUMBER_OF_ARGS".to_string());
    }
    let Some(path_str) = args[0].as_str() else {
        return Err(
            "DISKUSAGE_INVALID_ARG_TYPE:  The \"path\" argument must be one of type string"
                .to_string(),
        );
    };
    let cpath = CString::new(path_str)
        .map_err(|_| "DISKUSAGE_RUNTIME_ERROR: Get diskusage failed".to_string())?;
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(cpath.as_ptr(), &mut vfs) } != 0 {
        return Err("DISKUSAGE_RUNTIME_ERROR: Get diskusage failed".to_string());
    }
    let frsize = if vfs.f_frsize > 0 {
        vfs.f_frsize as u64
    } else {
        vfs.f_bsize as u64
    };
    let available = (vfs.f_bavail as u64).saturating_mul(frsize);
    let free = (vfs.f_bfree as u64).saturating_mul(frsize);
    let total = (vfs.f_blocks as u64).saturating_mul(frsize);
    Ok(json!({
        "available": available,
        "free": free,
        "total": total,
    }))
}

fn unescape_mount_field(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && (b'0'..=b'7').contains(&bytes[i + 1])
            && (b'0'..=b'7').contains(&bytes[i + 2])
            && (b'0'..=b'7').contains(&bytes[i + 3])
        {
            let val = ((bytes[i + 1] - b'0') << 6)
                | ((bytes[i + 2] - b'0') << 3)
                | (bytes[i + 3] - b'0');
            out.push(val);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_real_zfile_fs(fstype: &str, dev: &str) -> bool {
    const PSEUDO: &[&str] = &[
        "proc",
        "sysfs",
        "tmpfs",
        "devtmpfs",
        "devpts",
        "cgroup",
        "cgroup2",
        "overlay",
        "squashfs",
        "mqueue",
        "debugfs",
        "tracefs",
        "securityfs",
        "pstore",
        "bpf",
        "configfs",
        "hugetlbfs",
        "autofs",
        "fusectl",
        "binfmt_misc",
        "ramfs",
        "nsfs",
        "rpc_pipefs",
        "fuse.gvfsd-fuse",
        "fuse.portal",
        "efivarfs",
    ];
    if PSEUDO.contains(&fstype) {
        return false;
    }
    if dev.starts_with("/dev/") {
        return true;
    }
    const ALLOW: &[&str] = &[
        "ext2", "ext3", "ext4", "btrfs", "xfs", "vfat", "exfat", "ntfs", "ntfs3", "fuseblk",
        "f2fs", "zfs", "jfs", "reiserfs",
    ];
    ALLOW.contains(&fstype)
}

fn statvfs_disk_entry(dir: &str, fstype: &str) -> Option<Value> {
    let cdir = CString::new(dir).ok()?;
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(cdir.as_ptr(), &mut vfs) } != 0 {
        return None;
    }
    let frsize = if vfs.f_frsize != 0 {
        vfs.f_frsize as u64
    } else {
        vfs.f_bsize as u64
    };
    let blocks = vfs.f_blocks as u64;
    let bfree = vfs.f_bfree as u64;
    let bavail = vfs.f_bavail as u64;
    let total = blocks.saturating_mul(frsize);
    if total == 0 {
        return None;
    }
    let free = bfree.saturating_mul(frsize);
    let available = bavail.saturating_mul(frsize);
    let used = blocks.saturating_sub(bfree).saturating_mul(frsize);
    let is_external =
        dir.starts_with("/media/") || dir.starts_with("/run/media/") || dir.starts_with("/mnt/");
    Some(json!({
        "name": dir,
        "label": dir,
        "path": dir,
        "filesystemType": fstype,
        "isExternal": is_external,
        "totalSpace": total,
        "usedSpace": used,
        "total": total,
        "used": used,
        "free": free,
        "available": available,
    }))
}

/// Real `/proc/mounts` + `libc::statvfs` implementation of `$znode.nativelibs.zfileAddon.getDiskInfo`.
pub fn zfile_get_disk_info() -> BridgeResult {
    let mut out = Map::new();
    let mut seen = HashSet::new();

    if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
        for line in mounts.lines() {
            let mut parts = line.split_whitespace();
            let (Some(raw_dev), Some(raw_dir), Some(fstype)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let dev = unescape_mount_field(raw_dev);
            let dir = unescape_mount_field(raw_dir);
            if seen.contains(&dir) || !is_real_zfile_fs(fstype, &dev) {
                continue;
            }
            if let Some(entry) = statvfs_disk_entry(&dir, fstype) {
                seen.insert(dir.clone());
                out.insert(dir, entry);
            }
        }
    }

    if !out.contains_key("/") {
        if let Some(root_entry) = statvfs_disk_entry("/", "ext4") {
            out.insert("/".to_string(), root_entry);
        }
    }

    Ok(Value::Object(out))
}

fn walk_regular_files_recursive(dir: &Path) -> (u64, u64) {
    let mut total_size = 0u64;
    let mut file_count = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&cur) else {
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
            } else if ft.is_file() {
                total_size = total_size.saturating_add(meta.len());
                file_count += 1;
            }
        }
    }
    (total_size, file_count)
}

/// Implementation of `$znode.nativelibs.zfileAddon.getInfo(path, isFolder?)`.
pub fn zfile_get_info(args: &[Value]) -> BridgeResult {
    let raw = arg_str(args, 0).unwrap_or("");
    let is_folder = args.get(1).and_then(Value::as_bool).unwrap_or(false);
    if raw.is_empty() {
        return Err("ENOENT: no such file or directory, stat ''".to_string());
    }
    let path = Path::new(raw);
    let meta = std::fs::metadata(path)
        .map_err(|_| format!("ENOENT: no such file or directory, stat '{raw}'"))?;
    let is_dir = meta.is_dir();
    let is_file = meta.is_file();
    let last_write_time = wire::WireStats::from(&meta).mtime_ms;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| raw.to_string());

    if is_folder {
        let (size, file_count) = if is_dir {
            walk_regular_files_recursive(path)
        } else {
            (0, 0)
        };
        Ok(json!({
            "size": size,
            "isDirectory": is_dir,
            "isFile": is_file,
            "name": name,
            "lastWriteTime": last_write_time,
            "fileCount": file_count,
        }))
    } else {
        Ok(json!({
            "size": meta.len(),
            "isDirectory": is_dir,
            "isFile": is_file,
            "name": name,
            "lastWriteTime": last_write_time,
        }))
    }
}

/// Generates a scaled JPEG thumbnail at `dest` from a local video file or `http(s)://` URL `src`
/// (handling Zalo `fileNoise` inputs transparently on local files) using `ffmpeg`. Returns
/// `Ok(true)` if `dest` was written and is non-empty, or `Ok(false)` when `ffmpeg` is
/// unavailable or fails.
pub fn generate_video_thumbnail_sync(
    src: &Path,
    dest: &Path,
    max_width: u32,
    max_height: u32,
    temp_dir: &Path,
) -> Result<bool, FsError> {
    let src_lossy = src.to_string_lossy();
    let is_remote = is_remote_http_url(&src_lossy);

    let mut denoised_guard: Option<NamedTempFile> = None;
    let effective_input: std::ffi::OsString = if is_remote {
        src.as_os_str().to_os_string()
    } else {
        let (_, noise_sig) = inspect_file_noise(src, true)?;
        let _ = std::fs::create_dir_all(temp_dir);
        if noise_sig.is_some() {
            let tmp_in =
                NamedTempFile::new_in(temp_dir).map_err(|e| FsError::from_io(e, temp_dir))?;
            denoise_file_sync(src, tmp_in.path(), "")?;
            let os = tmp_in.path().as_os_str().to_os_string();
            denoised_guard = Some(tmp_in);
            os
        } else {
            src.as_os_str().to_os_string()
        }
    };

    let parent = dest.parent().ok_or_else(|| FsError::eacces(dest))?;
    std::fs::create_dir_all(parent).map_err(|e| FsError::from_io(e, parent))?;
    let tmp_out = NamedTempFile::new_in(parent).map_err(|e| FsError::from_io(e, parent))?;
    let tmp_out_path = tmp_out.path().to_path_buf();

    let mw = max_width.max(2);
    let mh = max_height.max(2);
    let vf = format!(
        "scale='min({mw},iw)':'min({mh},ih)':force_original_aspect_ratio=decrease,scale=trunc(iw/2)*2:trunc(ih/2)*2"
    );

    let run_ffmpeg = |seek_arg: &str| -> bool {
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-ss",
                seek_arg,
            ])
            .arg("-i")
            .arg(&effective_input)
            .args([
                "-frames:v",
                "1",
                "-vf",
                &vf,
                "-q:v",
                "3",
                "-f",
                "image2",
                "-y",
            ])
            .arg(&tmp_out_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        matches!(status, Ok(s) if s.success())
            && std::fs::metadata(&tmp_out_path)
                .map(|m| m.is_file() && m.len() > 0)
                .unwrap_or(false)
    };

    let ok = run_ffmpeg("0.1") || run_ffmpeg("0");
    drop(denoised_guard);

    if !ok {
        return Ok(false);
    }

    tmp_out
        .persist(dest)
        .map_err(|e| FsError::from_io(e.error, dest))?;
    Ok(true)
}
