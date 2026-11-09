//! Persistent multi-account profile registry (`profiles.json`), legacy directory migration,
//! and profile metadata helpers for seamless in-window multi-account workspaces.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::platform::{sanitize_profile_slug, ZaloPlatform};

/// Filename of the persistent profile registry stored under `<instance_root_dir>/profiles.json`.
pub const PROFILES_REGISTRY_FILENAME: &str = "profiles.json";

/// Schema version for [`ProfileRegistry`].
pub const PROFILES_REGISTRY_VERSION: u32 = 1;

fn default_registry_version() -> u32 {
    PROFILES_REGISTRY_VERSION
}

fn default_active_slug() -> String {
    "default".to_string()
}

fn default_enabled_in_workspace() -> bool {
    true
}

/// Metadata describing a single isolated Zalo account profile managed by Olaz.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileMetadata {
    pub slug: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub zalo_uid: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub order: u32,
    #[serde(default = "default_enabled_in_workspace")]
    pub enabled_in_workspace: bool,
}

impl ProfileMetadata {
    /// Creates a new workspace-enabled [`ProfileMetadata`] for `slug` at `order`.
    pub fn new(slug: impl Into<String>, order: u32) -> Self {
        Self {
            slug: slug.into(),
            label: None,
            zalo_uid: None,
            display_name: None,
            avatar_url: None,
            proxy_url: None,
            order,
            enabled_in_workspace: true,
        }
    }

    /// Human-friendly title preferring user nickname (`label`), then Zalo `display_name`,
    /// then `"Mặc định"` (for `"default"`) or `slug`.
    pub fn effective_name(&self) -> String {
        if let Some(lbl) = self.label.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            return lbl.to_string();
        }
        if let Some(dn) = self
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return dn.to_string();
        }
        if self.slug == "default" {
            "Tài khoản 1".to_string()
        } else {
            self.slug.clone()
        }
    }

    /// Short 1–2 character initials for the `AccountRail` avatar button.
    pub fn initials(&self) -> String {
        let source = self
            .label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.display_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            });

        if let Some(name) = source {
            let words: Vec<&str> = name.split_whitespace().filter(|w| !w.is_empty()).collect();
            if words.len() >= 2 {
                let first = words[0].chars().next();
                let last = words[words.len() - 1].chars().next();
                if let (Some(a), Some(b)) = (first, last) {
                    return format!("{}{}", a.to_uppercase(), b.to_uppercase());
                }
            }
            let chars: String = name.chars().take(2).collect();
            if !chars.is_empty() {
                return chars.to_uppercase();
            }
        }

        if self.slug == "default" {
            return "Z1".to_string();
        }
        if let Some(num) = self
            .slug
            .strip_prefix("account-")
            .or_else(|| self.slug.strip_prefix("profile-"))
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        {
            return format!("Z{num}");
        }
        let fallback: String = self
            .slug
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(2)
            .collect();
        if fallback.is_empty() {
            "Z".to_string()
        } else {
            fallback.to_ascii_uppercase()
        }
    }
}

/// Persistent multi-account registry stored atomically (`0o600`) at `<instance_root_dir>/profiles.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRegistry {
    #[serde(default = "default_registry_version")]
    pub version: u32,
    #[serde(default = "default_active_slug")]
    pub active_slug: String,
    #[serde(default)]
    pub profiles: Vec<ProfileMetadata>,
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self {
            version: PROFILES_REGISTRY_VERSION,
            active_slug: "default".to_string(),
            profiles: vec![ProfileMetadata::new("default", 0)],
        }
    }
}

impl ProfileRegistry {
    /// Returns `<instance_root_dir>/profiles.json`.
    pub fn registry_path(instance_root_dir: &Path) -> PathBuf {
        instance_root_dir.join(PROFILES_REGISTRY_FILENAME)
    }

    /// Loads `<instance_root_dir>/profiles.json` (if present), auto-migrates any legacy
    /// `profiles/<slug>/share/olaz/ZaloData` split-brain directories into `profiles/<slug>/ZaloData`,
    /// and merges any profile directories discovered via [`ZaloPlatform::list_profiles`].
    pub fn load_or_discover(instance_root_dir: &Path) -> Self {
        let migrated = migrate_legacy_split_brain_profiles(instance_root_dir);

        let path = Self::registry_path(instance_root_dir);
        let mut reg = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .unwrap_or_default();

        if reg.version == 0 {
            reg.version = PROFILES_REGISTRY_VERSION;
        }

        // Normalize slugs and deduplicate while preserving existing metadata.
        let mut deduped: Vec<ProfileMetadata> = Vec::with_capacity(reg.profiles.len().max(1));
        for mut item in reg.profiles {
            let Ok(clean_slug) = sanitize_profile_slug(&item.slug) else {
                continue;
            };
            item.slug = clean_slug;
            if !deduped.iter().any(|p| p.slug == item.slug) {
                deduped.push(item);
            }
        }
        reg.profiles = deduped;

        let mut dirty = migrated;
        if !reg.profiles.iter().any(|p| p.slug == "default") {
            reg.profiles.insert(0, ProfileMetadata::new("default", 0));
            dirty = true;
        }

        // Merge any on-disk profile directories discovered by `ZaloPlatform::list_profiles`.
        for slug in ZaloPlatform::list_profiles(instance_root_dir) {
            if !reg.profiles.iter().any(|p| p.slug == slug) {
                let next_order = reg.next_order();
                reg.profiles.push(ProfileMetadata::new(slug, next_order));
                dirty = true;
            }
        }

        reg.profiles.sort_by_key(|p| (p.order, p.slug != "default"));
        for (idx, p) in reg.profiles.iter_mut().enumerate() {
            p.order = idx as u32;
        }

        let clean_active = sanitize_profile_slug(&reg.active_slug)
            .unwrap_or_else(|_| "default".to_string());
        if reg.profiles.iter().any(|p| p.slug == clean_active) {
            reg.active_slug = clean_active;
        } else {
            reg.active_slug = "default".to_string();
            dirty = true;
        }

        if dirty && instance_root_dir.exists() {
            let _ = reg.save_to(instance_root_dir);
        }

        reg
    }

    /// Atomically writes `<instance_root_dir>/profiles.json` with `0o600` permissions.
    pub fn save_to(&self, instance_root_dir: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            if !instance_root_dir.exists() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(instance_root_dir)
                    .with_context(|| {
                        format!("failed to create instance root {}", instance_root_dir.display())
                    })?;
            }
            let payload =
                serde_json::to_vec_pretty(self).context("failed to serialize profiles.json")?;
            let mut tmp = NamedTempFile::new_in(instance_root_dir).with_context(|| {
                format!(
                    "failed to create temp profiles.json in {}",
                    instance_root_dir.display()
                )
            })?;
            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))
                .context("failed to chmod 0600 temp profiles.json")?;
            tmp.write_all(&payload)
                .and_then(|_| tmp.flush())
                .and_then(|_| tmp.as_file().sync_all())
                .context("failed to write temp profiles.json")?;
            let dest = Self::registry_path(instance_root_dir);
            tmp.persist(&dest).map_err(|e| {
                anyhow::anyhow!("failed to persist {}: {}", dest.display(), e.error)
            })?;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(instance_root_dir)?;
            let payload = serde_json::to_vec_pretty(self)?;
            std::fs::write(Self::registry_path(instance_root_dir), payload)?;
            Ok(())
        }
    }

    fn next_order(&self) -> u32 {
        self.profiles
            .iter()
            .map(|p| p.order)
            .max()
            .map_or(0, |m| m.saturating_add(1))
    }

    /// Allocates the next unused `"account-2"`, `"account-3"`, ... profile slug,
    /// registers it in `self.profiles` with `enabled_in_workspace: true`, and returns the slug.
    pub fn allocate_new_slug(&mut self) -> String {
        let mut n = 2_u32;
        loop {
            let candidate = format!("account-{n}");
            if !self
                .profiles
                .iter()
                .any(|p| p.slug.eq_ignore_ascii_case(&candidate))
            {
                let order = self.next_order();
                self.profiles
                    .push(ProfileMetadata::new(candidate.clone(), order));
                return candidate;
            }
            n = n.saturating_add(1);
        }
    }

    /// Looks up profile metadata by slug.
    pub fn get(&self, slug: &str) -> Option<&ProfileMetadata> {
        self.profiles.iter().find(|p| p.slug == slug)
    }

    /// Looks up mutable profile metadata by slug.
    pub fn get_mut(&mut self, slug: &str) -> Option<&mut ProfileMetadata> {
        self.profiles.iter_mut().find(|p| p.slug == slug)
    }

    /// Ensures `slug` exists in `self.profiles` (enabling it in workspace) and returns a mutable reference.
    pub fn ensure_profile(&mut self, slug: &str) -> Result<&mut ProfileMetadata> {
        let clean = sanitize_profile_slug(slug)?;
        if let Some(idx) = self.profiles.iter().position(|p| p.slug == clean) {
            self.profiles[idx].enabled_in_workspace = true;
            return Ok(&mut self.profiles[idx]);
        }
        let order = self.next_order();
        self.profiles.push(ProfileMetadata::new(clean, order));
        Ok(self.profiles.last_mut().expect("just pushed profile"))
    }

    /// Returns all workspace-enabled profiles sorted by `order`.
    pub fn workspace_profiles(&self) -> Vec<ProfileMetadata> {
        let mut list: Vec<ProfileMetadata> = self
            .profiles
            .iter()
            .filter(|p| p.enabled_in_workspace)
            .cloned()
            .collect();
        list.sort_by_key(|p| p.order);
        if list.is_empty() {
            list.push(ProfileMetadata::new("default", 0));
        }
        list
    }

    /// Updates cached Zalo user identity (`zalo_uid`, `display_name`, `avatar_url`) for `slug`.
    /// Returns `true` if any persisted field changed.
    pub fn update_identity(
        &mut self,
        slug: &str,
        zalo_uid: Option<&str>,
        display_name: Option<&str>,
        avatar_url: Option<&str>,
    ) -> bool {
        let Some(meta) = self.get_mut(slug) else {
            return false;
        };
        let mut changed = false;
        if let Some(uid) = zalo_uid.map(str::trim).filter(|s| !s.is_empty()) {
            if meta.zalo_uid.as_deref() != Some(uid) {
                meta.zalo_uid = Some(uid.to_string());
                changed = true;
            }
        }
        if let Some(name) = display_name.map(str::trim).filter(|s| !s.is_empty()) {
            if meta.display_name.as_deref() != Some(name) {
                meta.display_name = Some(name.to_string());
                changed = true;
            }
        }
        if let Some(ava) = avatar_url.map(str::trim).filter(|s| !s.is_empty()) {
            if meta.avatar_url.as_deref() != Some(ava) {
                meta.avatar_url = Some(ava.to_string());
                changed = true;
            }
        }
        changed
    }

    /// Finds another profile (with `slug != current_slug`) that already owns `zalo_uid`.
    pub fn find_other_by_uid(&self, current_slug: &str, zalo_uid: &str) -> Option<&ProfileMetadata> {
        let uid = zalo_uid.trim();
        if uid.is_empty() {
            return None;
        }
        self.profiles
            .iter()
            .find(|p| p.slug != current_slug && p.zalo_uid.as_deref() == Some(uid))
    }

    /// Updates the user-chosen nickname (`label`) for `slug`. Returns `true` if changed.
    pub fn set_label(&mut self, slug: &str, label: Option<String>) -> bool {
        let Some(meta) = self.get_mut(slug) else {
            return false;
        };
        let normalized = label
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if meta.label != normalized {
            meta.label = normalized;
            true
        } else {
            false
        }
    }

    /// Removes a secondary profile (`slug != "default"`) from the registry and re-normalizes `active_slug`.
    pub fn remove_profile(&mut self, slug: &str) -> bool {
        if slug == "default" {
            // For `"default"`, clear identity metadata rather than deleting the primary slot.
            if let Some(meta) = self.get_mut("default") {
                meta.zalo_uid = None;
                meta.display_name = None;
                meta.avatar_url = None;
                meta.label = None;
            }
            return false;
        }
        let before = self.profiles.len();
        self.profiles.retain(|p| p.slug != slug);
        let removed = self.profiles.len() != before;
        if removed {
            for (idx, p) in self.profiles.iter_mut().enumerate() {
                p.order = idx as u32;
            }
            if self.active_slug == slug {
                self.active_slug = self
                    .profiles
                    .iter()
                    .find(|p| p.enabled_in_workspace)
                    .map(|p| p.slug.clone())
                    .unwrap_or_else(|| "default".to_string());
            }
        }
        removed
    }
}

/// Migrates legacy `profiles/<slug>/share/olaz/ZaloData` (and sibling `webkit`) created by the
/// old `XDG_DATA_HOME` split-brain launcher into `profiles/<slug>/ZaloData` when the canonical
/// `profiles/<slug>/ZaloData` does not exist yet.
fn migrate_legacy_split_brain_profiles(instance_root_dir: &Path) -> bool {
    let profiles_dir = instance_root_dir.join("profiles");
    let Ok(entries) = std::fs::read_dir(&profiles_dir) else {
        return false;
    };

    let mut any_migrated = false;
    for entry in entries.flatten() {
        let profile_dir = entry.path();
        if !profile_dir.is_dir() {
            continue;
        }
        let legacy_olaz = profile_dir.join("share").join("olaz");
        if !legacy_olaz.is_dir() {
            continue;
        }

        for subdir in ["ZaloData", "webkit", "keys"] {
            let legacy_src = legacy_olaz.join(subdir);
            let canonical_dst = profile_dir.join(subdir);
            if legacy_src.exists() && !canonical_dst.exists() {
                if move_or_copy_dir(&legacy_src, &canonical_dst).is_ok() {
                    tracing::info!(
                        from = %legacy_src.display(),
                        to = %canonical_dst.display(),
                        "migrated legacy secondary profile directory"
                    );
                    any_migrated = true;
                }
            }
        }
    }
    any_migrated
}

fn move_or_copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    copy_dir_all(src, dst)?;
    let _ = std::fs::remove_dir_all(src);
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_symlink() {
            #[cfg(unix)]
            {
                let target = std::fs::read_link(entry.path())?;
                let _ = std::os::unix::fs::symlink(target, &dst_path);
            }
        } else if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}
