//! Portable settings profiles and the `.valsync` file format.
//!
//! A profile contains only setting values. It never contains PUUIDs, Riot IDs,
//! tokens, machine identifiers or cosmetic ownership: every entry is re-checked
//! against [`crate::catalog::classify`] when captured *and* when imported.

use crate::catalog::{self, Category, Classification, Scope};
use crate::ini::IniDoc;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const FORMAT_ID: &str = "valsync-profile";
pub const FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingEntry {
    /// File path relative to the account config folder, `/` separated.
    pub file: String,
    pub section: String,
    pub key: String,
    /// Usually one value; repeated keys (arrays) carry several.
    pub values: Vec<String>,
    pub category: Category,
    pub scope: Scope,
}

impl SettingEntry {
    pub fn id(&self) -> String {
        format!("{}|{}|{}", self.file.to_ascii_lowercase(), self.section.to_ascii_lowercase(), self.key.to_ascii_lowercase())
    }

    pub fn display_value(&self) -> String {
        let v = self.values.join(" | ");
        if v.chars().count() > 60 { format!("{}…", v.chars().take(59).collect::<String>()) } else { v }
    }
}

/// A value the user records for settings that cannot be transferred
/// automatically (e.g. a crosshair code to paste in-game).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManualValue {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub created_at: String,
    /// VALORANT patch detected when the profile was captured.
    pub patch: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    pub settings: Vec<SettingEntry>,
    #[serde(default)]
    pub manual: Vec<ManualValue>,
    /// Account settings stored by Riot (sensitivity, crosshair, keybinds, …),
    /// machine-specific entries removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<serde_json::Value>,
}

#[derive(Debug, Default, Clone)]
pub struct CaptureReport {
    pub files: usize,
    pub local: usize,
    /// Account settings (from Riot, or old local cache copies when offline).
    pub cloud: usize,
    pub excluded: usize,
    pub excluded_reasons: BTreeMap<&'static str, usize>,
}

impl Profile {
    /// Build a profile from parsed account config files.
    pub fn capture(name: &str, files: &BTreeMap<String, IniDoc>, patch: Option<String>, own_puuid: Option<&str>) -> (Profile, CaptureReport) {
        let mut rep = CaptureReport { files: files.len(), ..Default::default() };
        let mut settings = Vec::new();
        for (file, doc) in files {
            for (section, key, values) in doc.grouped() {
                let joined = values.join("\n");
                let leaks_id = own_puuid.map(|p| !p.is_empty() && joined.to_ascii_lowercase().contains(&p.to_ascii_lowercase())).unwrap_or(false);
                let class = values
                    .iter()
                    .map(|v| catalog::classify(file, &section, &key, v))
                    .find(|c| matches!(c, Classification::Excluded(_)))
                    .unwrap_or_else(|| catalog::classify(file, &section, &key, values.first().map(String::as_str).unwrap_or("")));
                match class {
                    Classification::Excluded(r) => {
                        rep.excluded += 1;
                        *rep.excluded_reasons.entry(r.label()).or_default() += 1;
                    }
                    _ if leaks_id => {
                        rep.excluded += 1;
                        *rep.excluded_reasons.entry(catalog::ExcludeReason::Identity.label()).or_default() += 1;
                    }
                    Classification::Setting { category, scope } => {
                        match scope {
                            Scope::Local => rep.local += 1,
                            Scope::Cloud => rep.cloud += 1,
                        }
                        settings.push(SettingEntry { file: file.clone(), section, key, values, category, scope });
                    }
                }
            }
        }
        let profile = Profile {
            name: name.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            patch,
            notes: None,
            settings,
            manual: Vec::new(),
            cloud: None,
        };
        (profile, rep)
    }

    /// Attach cloud settings; the stale local cache copies of them are dropped.
    pub fn set_cloud(&mut self, cloud: &serde_json::Value) {
        self.cloud = Some(crate::cloud::sanitize(cloud));
        self.settings.retain(|s| s.scope == Scope::Local);
    }

    pub fn cloud_count(&self) -> usize {
        self.cloud.as_ref().map(|c| crate::cloud::category_counts(c).values().sum()).unwrap_or(0)
    }

    /// Per category: (local file settings, account/cloud settings).
    pub fn categories(&self) -> BTreeMap<Category, (usize, usize)> {
        let mut m: BTreeMap<Category, (usize, usize)> = BTreeMap::new();
        if let Some(c) = &self.cloud {
            for (cat, n) in crate::cloud::category_counts(c) {
                m.entry(cat).or_default().1 += n;
            }
        }
        for s in &self.settings {
            let e = m.entry(s.category).or_default();
            match s.scope {
                Scope::Local => e.0 += 1,
                Scope::Cloud => e.1 += 1,
            }
        }
        m
    }

    pub fn local_count(&self) -> usize {
        self.settings.iter().filter(|s| s.scope == Scope::Local).count()
    }

    pub fn checksum(&self) -> String {
        let canon = serde_json::to_vec(self).expect("profile serializes");
        hex::encode(Sha256::digest(&canon))
    }

    /// Remove anything that must never be stored or transferred.
    pub fn sanitize(&mut self) -> usize {
        let before = self.settings.len();
        self.settings.retain(|s| {
            crate::paths::safe_relative(&s.file).is_some()
                && !s.values.iter().any(|v| matches!(catalog::classify(&s.file, &s.section, &s.key, v), Classification::Excluded(_)))
                && !s.key.contains(['\r', '\n', '='])
                && !s.section.contains(['\r', '\n', ']'])
                && !s.values.iter().any(|v| v.contains(['\r', '\n']))
        });
        // Re-derive category/scope so a hand-edited file cannot relabel a key.
        for s in &mut self.settings {
            if let Classification::Setting { category, scope } =
                catalog::classify(&s.file, &s.section, &s.key, s.values.first().map(String::as_str).unwrap_or(""))
            {
                s.category = category;
                s.scope = scope;
            }
        }
        let mut removed = before - self.settings.len();
        if let Some(c) = &self.cloud {
            if !c.is_object() {
                self.cloud = None;
                removed += 1;
            } else {
                let clean = crate::cloud::sanitize(c);
                removed += crate::cloud::entries(c).len() - crate::cloud::entries(&clean).len();
                self.cloud = Some(clean);
            }
        }
        removed
    }

    pub fn to_file_json(&self) -> Result<String> {
        let file = ProfileFile {
            format: FORMAT_ID.to_string(),
            version: FORMAT_VERSION,
            app: format!("VALSYNC {}", env!("CARGO_PKG_VERSION")),
            checksum: self.checksum(),
            profile: self.clone(),
        };
        Ok(serde_json::to_string_pretty(&file)?)
    }

    pub fn from_file_json(text: &str) -> Result<(Profile, usize)> {
        let file: ProfileFile = serde_json::from_str(text).context("not a valid .valsync profile (JSON parse failed)")?;
        if file.format != FORMAT_ID {
            bail!("not a VALSYNC profile (format '{}')", file.format);
        }
        if file.version > FORMAT_VERSION {
            bail!("profile format v{} is newer than this VALSYNC supports (v{FORMAT_VERSION}); please update VALSYNC", file.version);
        }
        if file.profile.checksum() != file.checksum {
            bail!("profile checksum mismatch: the file is corrupted or was modified");
        }
        let mut p = file.profile;
        let removed = p.sanitize();
        Ok((p, removed))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ProfileFile {
    format: String,
    version: u32,
    app: String,
    checksum: String,
    profile: Profile,
}

/// Difference between two profiles.
#[derive(Debug, Default)]
pub struct ProfileDiff {
    pub changed: Vec<(SettingEntry, SettingEntry)>,
    pub only_a: Vec<SettingEntry>,
    pub only_b: Vec<SettingEntry>,
    pub same: usize,
    /// Differences in account (cloud) settings, `a` → `b`.
    pub cloud: Vec<crate::engine::Change>,
}

pub fn compare(a: &Profile, b: &Profile) -> ProfileDiff {
    let mut d = ProfileDiff::default();
    let bmap: BTreeMap<String, &SettingEntry> = b.settings.iter().map(|s| (s.id(), s)).collect();
    let amap: BTreeMap<String, &SettingEntry> = a.settings.iter().map(|s| (s.id(), s)).collect();
    for s in &a.settings {
        match bmap.get(&s.id()) {
            Some(o) if o.values == s.values => d.same += 1,
            Some(o) => d.changed.push((s.clone(), (*o).clone())),
            None => d.only_a.push(s.clone()),
        }
    }
    for s in &b.settings {
        if !amap.contains_key(&s.id()) {
            d.only_b.push(s.clone());
        }
    }
    let empty = serde_json::Value::Object(Default::default());
    if a.cloud.is_some() || b.cloud.is_some() {
        d.cloud = crate::cloud::diff(a.cloud.as_ref().unwrap_or(&empty), b.cloud.as_ref().unwrap_or(&empty));
        d.same += crate::cloud::entries(a.cloud.as_ref().unwrap_or(&empty)).len().saturating_sub(d.cloud.len());
    }
    d
}

/// Validate a profile name.
pub fn validate_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        bail!("profile name cannot be empty");
    }
    if n.chars().count() > 64 {
        bail!("profile name is too long (max 64 characters)");
    }
    if n.chars().any(|c| c.is_control()) {
        bail!("profile name contains control characters");
    }
    Ok(n.to_string())
}

/// File-system friendly version of a profile name for exports.
pub fn file_stem(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '_' })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() { "profile".into() } else { s }
}
