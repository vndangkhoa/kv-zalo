//! Memory-mapped zero-extraction Electron ASAR v0 archive reader.
//!
//! Port of `Sources/Olaz/ZaloRuntime/AsarArchive.swift`.
//!
//! Format:
//! - `[0..4]`  `u32` pickle header size (`4`)
//! - `[4..8]`  `u32` `header_pickle_size`
//! - `[8..12]` `u32` inner pickle payload size
//! - `[12..16]` `u32` `json_len`
//! - `[16..16 + json_len]` UTF-8 JSON directory tree (`{"files": ...}`)
//! - File offsets in the JSON header are serialized as decimal strings relative to
//!   `base = 8 + header_pickle_size`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use memmap2::Mmap;
use percent_encoding::percent_decode_str;
use serde_json::Value;

/// Metadata for a single file indexed inside an Electron ASAR archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsarEntry {
    /// Byte offset relative to `base = 8 + header_pickle_size`.
    pub offset: usize,
    /// Uncompressed file size in bytes.
    pub size: usize,
    /// `true` when the file lives beside the archive under `app.asar.unpacked/`.
    pub unpacked: bool,
}

/// Read-only memory-mapped Electron `app.asar` v0 archive.
#[derive(Debug)]
pub struct AsarArchive {
    path: PathBuf,
    mmap: Mmap,
    base: usize,
    entries: BTreeMap<String, AsarEntry>,
}

impl AsarArchive {
    /// Opens and indexes an Electron `.asar` archive using `mmap(2)` without extracting files.
    pub fn open(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("failed to open ASAR {}", path.display()))?;
        // SAFETY: `app.asar` is an immutable read-only runtime asset managed by `ZaloRuntimeManager`.
        let mmap = unsafe { Mmap::map(&file) }
            .with_context(|| format!("failed to mmap ASAR {}", path.display()))?;

        ensure!(
            mmap.len() > 16,
            "invalid ASAR header in {}: file too small ({} bytes)",
            path.display(),
            mmap.len()
        );

        let header_pickle_size = u32::from_le_bytes(mmap[4..8].try_into().unwrap()) as usize;
        let json_len = u32::from_le_bytes(mmap[12..16].try_into().unwrap()) as usize;

        let json_end = 16usize
            .checked_add(json_len)
            .context("ASAR header json_len overflow")?;
        let base = 8usize
            .checked_add(header_pickle_size)
            .context("ASAR header_pickle_size overflow")?;

        ensure!(
            json_end <= mmap.len() && base <= mmap.len(),
            "corrupt ASAR header bounds in {} (json_end={}, base={}, len={})",
            path.display(),
            json_end,
            base,
            mmap.len()
        );

        let root: Value = serde_json::from_slice(&mmap[16..json_end])
            .with_context(|| format!("invalid ASAR header JSON in {}", path.display()))?;
        if !root.is_object() {
            bail!("ASAR header root in {} is not a JSON object", path.display());
        }

        let mut entries = BTreeMap::new();
        Self::index_node(&root, "", &mut entries);

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            base,
            entries,
        })
    }

    fn index_node(node: &Value, prefix: &str, out: &mut BTreeMap<String, AsarEntry>) {
        let Some(files) = node.get("files").and_then(Value::as_object) else {
            return;
        };
        for (name, child) in files {
            let Some(child_obj) = child.as_object() else {
                continue;
            };
            let full_path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if child_obj.contains_key("files") {
                Self::index_node(child, &full_path, out);
            } else if let Some(size_u64) = child_obj.get("size").and_then(Value::as_u64) {
                let Ok(size) = usize::try_from(size_u64) else {
                    continue;
                };
                // ASAR serializes `"offset"` as a decimal string so >2^53 offsets survive JSON.
                let offset = match child_obj.get("offset") {
                    Some(Value::String(s)) => s.parse::<usize>().unwrap_or(0),
                    Some(Value::Number(n)) => {
                        n.as_u64().and_then(|v| usize::try_from(v).ok()).unwrap_or(0)
                    }
                    _ => 0,
                };
                let unpacked = child_obj
                    .get("unpacked")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                out.insert(
                    full_path,
                    AsarEntry {
                        offset,
                        size,
                        unpacked,
                    },
                );
            }
        }
    }

    /// Filesystem path of the mapped `.asar` archive.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the flattened index of normalized archive paths to [`AsarEntry`] metadata.
    pub fn entries(&self) -> &BTreeMap<String, AsarEntry> {
        &self.entries
    }

    /// Looks up the [`AsarEntry`] for `path` (percent-decoded and stripped of leading `/`).
    pub fn entry(&self, path: &str) -> Option<&AsarEntry> {
        let key = Self::normalize_path(path);
        self.entries.get(key.as_ref())
    }

    /// Returns `true` if `path` exists in the archive index.
    pub fn contains(&self, path: &str) -> bool {
        self.entry(path).is_some()
    }

    /// Returns the validated `(start, end)` byte range inside the memory-mapped archive
    /// for a packed file, or `None` if missing, marked `unpacked`, or out of bounds.
    pub fn byte_range(&self, path: &str) -> Option<(usize, usize)> {
        let entry = self.entry(path)?;
        if entry.unpacked {
            return None;
        }
        let start = self.base.checked_add(entry.offset)?;
        let end = start.checked_add(entry.size)?;
        if end > self.mmap.len() {
            return None;
        }
        Some((start, end))
    }

    /// Returns a zero-copy byte slice for a validated `[start..end]` range in the mapped archive.
    pub fn slice(&self, start: usize, end: usize) -> Option<&[u8]> {
        if start <= end && end <= self.mmap.len() {
            Some(&self.mmap[start..end])
        } else {
            None
        }
    }

    /// Returns a zero-copy byte slice for a packed file inside the mapped archive.
    ///
    /// Percent-decodes `path` and strips any leading `/`. Returns `None` if the entry
    /// is missing, marked `unpacked`, or out of bounds.
    pub fn read(&self, path: &str) -> Option<&[u8]> {
        let (start, end) = self.byte_range(path)?;
        self.slice(start, end)
    }

    /// Reads either a zero-copy slice from the mapped archive or, if the entry is marked
    /// `unpacked`, reads from `<archive_dir>/app.asar.unpacked/<path>` on disk.
    pub fn read_or_unpacked(&self, path: &str) -> Option<Cow<'_, [u8]>> {
        let key = Self::normalize_path(path);
        let entry = self.entries.get(key.as_ref())?;
        if entry.unpacked {
            let parent = self.path.parent()?;
            let file_name = self.path.file_name()?.to_string_lossy();
            let unpacked_dir = parent.join(format!("{file_name}.unpacked"));
            let target = unpacked_dir.join(key.as_ref());
            return std::fs::read(target).ok().map(Cow::Owned);
        }
        self.read(key.as_ref()).map(Cow::Borrowed)
    }

    /// Returns sorted archive paths starting with `prefix` (after normalizing `prefix`).
    pub fn paths_with_prefix(&self, prefix: &str) -> Vec<&str> {
        let norm = Self::normalize_path(prefix);
        self.entries
            .keys()
            .filter(|k| k.starts_with(norm.as_ref()))
            .map(String::as_str)
            .collect()
    }

    /// Percent-decodes `path` and strips leading `/` characters.
    pub fn normalize_path(path: &str) -> Cow<'_, str> {
        let decoded = percent_decode_str(path).decode_utf8_lossy();
        match decoded {
            Cow::Borrowed(s) => Cow::Borrowed(s.trim_start_matches('/')),
            Cow::Owned(s) => Cow::Owned(s.trim_start_matches('/').to_owned()),
        }
    }
}

/// Returns the HTTP `Content-Type` MIME string for an ASAR asset path.
pub fn mime_for_path(path: &str) -> &'static str {
    let clean = path.split(['?', '#']).next().unwrap_or(path);
    let ext = Path::new(clean)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" | "cjs" | "jsapi" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        _ => mime_guess::from_path(clean)
            .first_raw()
            .unwrap_or("application/octet-stream"),
    }
}
