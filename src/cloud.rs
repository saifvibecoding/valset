//! Account settings stored by Riot (`Ares.PlayerSettings`): sensitivity,
//! crosshair, keybinds, HUD, minimap, audio, accessibility and more.
//!
//! The document looks like:
//! ```json
//! { "floatSettings": [{"settingEnum": "EAresFloatSettingName::MouseSensitivity", "value": 0.4}],
//!   "boolSettings": [...], "intSettings": [...], "stringSettings": [...],
//!   "actionMappings": [...], "axisMappings": [...], "roamingSetttingsVersion": 1, ... }
//! ```
//! VALSYNC transfers it the way the established tools do: the profile replaces
//! the target's settings, except for machine/account-specific entries
//! (audio devices, monitors, "has seen" flags), which keep the target's values.

use crate::catalog::{self, Category, Classification, Scope};
use crate::engine::Change;
use anyhow::Result;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const CLOUD_REL: &str = "@cloud/Ares.PlayerSettings.json";
pub const SETTING_ARRAYS: [&str; 4] = ["floatSettings", "boolSettings", "intSettings", "stringSettings"];
pub const MAPPING_ARRAYS: [&str; 2] = ["actionMappings", "axisMappings"];

/// Where cloud settings are read from / written to.
pub trait CloudSettings {
    /// PUUID of the account currently signed in, if any.
    fn account(&self) -> Option<String>;
    fn fetch(&self) -> Result<Value>;
    fn store(&self, v: &Value) -> Result<()>;
}

/// Settings that belong to the PC or the account, never to a profile.
pub fn is_machine_specific(setting_enum: &str) -> bool {
    const EXACT: [&str; 6] = [
        "VoiceDeviceCaptureHandle",
        "VoiceDeviceRenderHandle",
        "DefaultMonitorDeviceID",
        "PreferredMonitorDeviceID",
        "PreferredGamePods",
        "PlayerUuid",
    ];
    const PREFIX: [&str; 5] = ["HasSeen", "HasAccepted", "HasEver", "LastSeen", "Seen"];
    let name = short_name(setting_enum);
    EXACT.contains(&name) || PREFIX.iter().any(|p| name.starts_with(p))
}

/// `EAresFloatSettingName::MouseSensitivity` → `MouseSensitivity`.
pub fn short_name(setting_enum: &str) -> &str {
    setting_enum.rsplit("::").next().unwrap_or(setting_enum)
}

pub fn category_of(setting_enum: &str, value: &Value) -> Category {
    let v = value_text(value);
    match catalog::classify("RiotUserSettings.ini", "Settings", setting_enum, &v) {
        Classification::Setting { category, .. } => category,
        Classification::Excluded(_) => Category::Gameplay,
    }
}

pub fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "(unset)".into(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub array: String,
    pub setting: String,
    pub value: Value,
    pub category: Category,
}

/// Every individual setting in the document (mappings count as one Keybinds entry each array).
pub fn entries(v: &Value) -> Vec<Entry> {
    let mut out = Vec::new();
    let Some(obj) = v.as_object() else { return out };
    for arr in SETTING_ARRAYS {
        for item in obj.get(arr).and_then(Value::as_array).into_iter().flatten() {
            let Some(e) = item.get("settingEnum").and_then(Value::as_str) else { continue };
            let value = item.get("value").cloned().unwrap_or(Value::Null);
            out.push(Entry { array: arr.into(), setting: e.into(), category: category_of(e, &value), value });
        }
    }
    for arr in MAPPING_ARRAYS {
        if let Some(a) = obj.get(arr).and_then(Value::as_array)
            && !a.is_empty()
        {
            out.push(Entry { array: arr.into(), setting: arr.into(), value: Value::Array(a.clone()), category: Category::Keybinds });
        }
    }
    out
}

pub fn category_counts(v: &Value) -> BTreeMap<Category, usize> {
    let mut m = BTreeMap::new();
    for e in entries(v) {
        if !is_machine_specific(&e.setting) {
            *m.entry(e.category).or_default() += match e.value.as_array() {
                Some(a) if MAPPING_ARRAYS.contains(&e.array.as_str()) => a.len(),
                _ => 1,
            };
        }
    }
    m
}

/// Remove machine-specific entries (used when saving a profile).
pub fn sanitize(v: &Value) -> Value {
    let mut out = match v {
        Value::Object(o) => Value::Object(o.clone()),
        _ => Value::Object(Map::new()),
    };
    if let Some(o) = out.as_object_mut() {
        for arr in SETTING_ARRAYS {
            if let Some(a) = o.get_mut(arr).and_then(Value::as_array_mut) {
                a.retain(|i| i.get("settingEnum").and_then(Value::as_str).map(|e| !is_machine_specific(e)).unwrap_or(false));
            }
        }
    }
    out
}

fn enum_of(item: &Value) -> Option<&str> {
    item.get("settingEnum").and_then(Value::as_str)
}

/// Build the document to upload: `preset` applied onto `target`.
/// `categories = None` transfers everything (full replace); otherwise only the
/// chosen categories change and everything else stays as the target had it.
pub fn merge(preset: &Value, target: &Value, categories: Option<&BTreeSet<Category>>) -> Value {
    let empty = Map::new();
    let p = preset.as_object().unwrap_or(&empty);
    let t = target.as_object().unwrap_or(&empty);
    let mut out: Map<String, Value> = match categories {
        None => p.clone(),
        Some(_) => t.clone(),
    };
    let wanted = |e: &str, v: &Value| -> bool {
        !is_machine_specific(e) && categories.map(|c| c.contains(&category_of(e, v))).unwrap_or(true)
    };
    for arr in SETTING_ARRAYS {
        let mut items: Vec<Value> = Vec::new();
        // Target entries that are kept.
        for i in t.get(arr).and_then(Value::as_array).into_iter().flatten() {
            let Some(e) = enum_of(i) else { continue };
            let v = i.get("value").cloned().unwrap_or(Value::Null);
            if !wanted(e, &v) {
                items.push(i.clone());
            }
        }
        // Preset entries that are transferred.
        for i in p.get(arr).and_then(Value::as_array).into_iter().flatten() {
            let Some(e) = enum_of(i) else { continue };
            let v = i.get("value").cloned().unwrap_or(Value::Null);
            if wanted(e, &v) {
                items.retain(|x| enum_of(x) != Some(e));
                items.push(i.clone());
            }
        }
        if items.is_empty() && !p.contains_key(arr) && !t.contains_key(arr) {
            out.remove(arr);
        } else {
            out.insert(arr.into(), Value::Array(items));
        }
    }
    let keybinds = categories.map(|c| c.contains(&Category::Keybinds) || c.contains(&Category::AgentKeybinds)).unwrap_or(true);
    for arr in MAPPING_ARRAYS {
        let src = if keybinds { p } else { t };
        match src.get(arr) {
            Some(v) => {
                out.insert(arr.into(), v.clone());
            }
            None => {
                out.remove(arr);
            }
        }
    }
    Value::Object(out)
}

fn as_map(v: &Value) -> BTreeMap<(String, String), Value> {
    entries(v).into_iter().map(|e| ((e.array, e.setting), e.value)).collect()
}

fn same_value(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() <= 1e-6 * x.abs().max(1.0),
        _ => a == b,
    }
}

/// Setting-level differences turning `from` into `to`.
pub fn diff(from: &Value, to: &Value) -> Vec<Change> {
    let a = as_map(from);
    let b = as_map(to);
    let mut keys: BTreeSet<&(String, String)> = a.keys().collect();
    keys.extend(b.keys());
    let mut out = Vec::new();
    for k in keys {
        let (old, new) = (a.get(k), b.get(k));
        let changed = match (old, new) {
            (Some(x), Some(y)) => !same_value(x, y),
            (None, None) => false,
            _ => true,
        };
        if !changed {
            continue;
        }
        let show = |v: Option<&Value>| -> Vec<String> {
            match v {
                None => Vec::new(),
                Some(Value::Array(arr)) if MAPPING_ARRAYS.contains(&k.0.as_str()) => vec![format!("{} bindings", arr.len())],
                Some(v) => vec![value_text(v)],
            }
        };
        let category = new.or(old).map(|v| if MAPPING_ARRAYS.contains(&k.0.as_str()) { Category::Keybinds } else { category_of(&k.1, v) }).unwrap_or(Category::Other);
        out.push(Change {
            file: CLOUD_REL.into(),
            section: k.0.clone(),
            key: short_name(&k.1).to_string(),
            old: show(old),
            new: show(new),
            category,
            scope: Scope::Cloud,
        });
    }
    out.sort_by(|x, y| x.category.cmp(&y.category).then(x.key.cmp(&y.key)));
    out
}

/// True when the server copy matches what was uploaded (setting by setting).
pub fn equivalent(expected: &Value, actual: &Value) -> bool {
    diff(expected, actual).is_empty()
}

/// The handful of values players care about most, for summaries.
pub fn highlights(v: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for e in entries(v) {
        let name = short_name(&e.setting);
        let label = match name {
            "MouseSensitivity" => "Sensitivity",
            "MouseSensitivityADS" => "ADS sensitivity",
            "MouseSensitivityZoomed" => "Scoped sensitivity",
            _ => continue,
        };
        out.push((label.to_string(), value_text(&e.value)));
    }
    let n = entries(v).iter().filter(|e| e.category == Category::Crosshair).count();
    if n > 0 {
        out.push(("Crosshair".into(), format!("{n} setting(s)")));
    }
    if let Some(a) = v.get("actionMappings").and_then(Value::as_array) {
        out.push(("Keybinds".into(), format!("{} bindings", a.len())));
    }
    out
}

// --------------------------------------------------------------------- backends

/// The real backend: the account signed into the Riot Client.
pub struct RiotCloud {
    session: std::cell::RefCell<crate::riot::Session>,
    logs_dir: std::path::PathBuf,
}

impl RiotCloud {
    pub fn connect(logs_dir: &std::path::Path, known_region: Option<&str>) -> Result<RiotCloud> {
        let session = crate::riot::block_on(crate::riot::Session::connect(logs_dir, known_region))?;
        Ok(RiotCloud { session: std::cell::RefCell::new(session), logs_dir: logs_dir.to_path_buf() })
    }

    pub fn session(&self) -> std::cell::Ref<'_, crate::riot::Session> {
        self.session.borrow()
    }

    /// Tokens expire after about an hour; reconnect once on 401/403.
    fn with_retry<T>(&self, f: impl Fn(&crate::riot::Session) -> Result<T>) -> Result<T> {
        match f(&self.session.borrow()) {
            Err(e) if matches!(crate::riot::session::status_of(&e), Some(401 | 403)) => {}
            other => return other,
        }
        let region = self.session.borrow().region.clone();
        let fresh = crate::riot::block_on(crate::riot::Session::connect(&self.logs_dir, Some(&region)))?;
        if fresh.puuid != self.session.borrow().puuid {
            anyhow::bail!("the Riot Client switched to another account; start the action again");
        }
        *self.session.borrow_mut() = fresh;
        f(&self.session.borrow())
    }
}

impl CloudSettings for RiotCloud {
    fn account(&self) -> Option<String> {
        Some(self.session.borrow().puuid.clone())
    }
    fn fetch(&self) -> Result<Value> {
        self.with_retry(|s| crate::riot::block_on(s.get_settings()))
    }
    fn store(&self, v: &Value) -> Result<()> {
        self.with_retry(|s| crate::riot::block_on(async {
            // Make sure the preference host is known before writing.
            if s.get_settings().await.is_err() {
                s.get_settings().await?;
            }
            s.put_settings(v).await
        }))
    }
}

/// In-memory backend for tests and benchmarks.
#[derive(Clone, Default)]
pub struct MemCloud {
    pub state: std::rc::Rc<std::cell::RefCell<MemState>>,
}

#[derive(Default)]
pub struct MemState {
    pub signed_in: Option<String>,
    pub docs: std::collections::HashMap<String, Value>,
    /// Fail the next N store() calls.
    pub fail_stores: usize,
    /// Store something different from what was requested (server-side mangling).
    pub mangle_next_store: bool,
    pub stores: usize,
}

impl MemCloud {
    pub fn sign_in(&self, puuid: &str) {
        self.state.borrow_mut().signed_in = Some(puuid.to_string());
    }
    pub fn set(&self, puuid: &str, v: Value) {
        self.state.borrow_mut().docs.insert(puuid.to_string(), v);
    }
    pub fn get(&self, puuid: &str) -> Value {
        self.state.borrow().docs.get(puuid).cloned().unwrap_or(Value::Object(Map::new()))
    }
}

impl CloudSettings for MemCloud {
    fn account(&self) -> Option<String> {
        self.state.borrow().signed_in.clone()
    }
    fn fetch(&self) -> Result<Value> {
        let st = self.state.borrow();
        let who = st.signed_in.clone().ok_or_else(|| anyhow::anyhow!("not signed in"))?;
        Ok(st.docs.get(&who).cloned().unwrap_or(Value::Object(Map::new())))
    }
    fn store(&self, v: &Value) -> Result<()> {
        let mut st = self.state.borrow_mut();
        st.stores += 1;
        if st.fail_stores > 0 {
            st.fail_stores -= 1;
            anyhow::bail!("simulated network failure");
        }
        let who = st.signed_in.clone().ok_or_else(|| anyhow::anyhow!("not signed in"))?;
        let mut v = v.clone();
        if st.mangle_next_store {
            st.mangle_next_store = false;
            v = Value::Object(Map::new());
        }
        st.docs.insert(who, v);
        Ok(())
    }
}

/// Sample settings document (tests / benchmarks).
pub fn sample_doc(sens: f64, crosshair: &str) -> Value {
    serde_json::json!({
        "floatSettings": [
            {"settingEnum": "EAresFloatSettingName::MouseSensitivity", "value": sens},
            {"settingEnum": "EAresFloatSettingName::MouseSensitivityZoomed", "value": 1.0},
            {"settingEnum": "EAresFloatSettingName::OverallVolume", "value": 0.6}
        ],
        "boolSettings": [
            {"settingEnum": "EAresBoolSettingName::MinimapRotates", "value": false},
            {"settingEnum": "EAresBoolSettingName::HasSeenTutorial", "value": true}
        ],
        "intSettings": [
            {"settingEnum": "EAresIntSettingName::ColorBlindMode", "value": 0}
        ],
        "stringSettings": [
            {"settingEnum": "EAresStringSettingName::SavedCrosshairProfileData", "value": crosshair},
            {"settingEnum": "EAresStringSettingName::VoiceDeviceCaptureHandle", "value": "device-of-this-pc"}
        ],
        "actionMappings": [
            {"alt": false, "bindIndex": 0, "characterName": "None", "cmd": false, "ctrl": false, "name": "Ability1", "shift": false, "key": "Q"}
        ],
        "roamingSetttingsVersion": 1
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_full_keeps_target_machine_entries() {
        let a = sample_doc(0.176, "A");
        let mut b = sample_doc(0.25, "B");
        b["stringSettings"][1]["value"] = Value::String("device-of-pc-B".into());
        let m = merge(&sanitize(&a), &b, None);
        let d = diff(&b, &m);
        assert!(d.iter().any(|c| c.key == "MouseSensitivity" && c.new == vec!["0.176"]));
        assert!(d.iter().any(|c| c.key == "SavedCrosshairProfileData"));
        assert!(!d.iter().any(|c| c.key == "VoiceDeviceCaptureHandle"), "{d:?}");
        assert!(!d.iter().any(|c| c.key == "HasSeenTutorial"));
    }

    #[test]
    fn merge_selected_categories_only() {
        let a = sample_doc(0.176, "A");
        let b = sample_doc(0.25, "B");
        let cats: BTreeSet<Category> = [Category::MouseSensitivity].into_iter().collect();
        let m = merge(&a, &b, Some(&cats));
        let d = diff(&b, &m);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].key, "MouseSensitivity");
    }

    #[test]
    fn float_tolerance() {
        let a = serde_json::json!({"floatSettings":[{"settingEnum":"X::S","value":0.1}]});
        let b = serde_json::json!({"floatSettings":[{"settingEnum":"X::S","value":0.10000000149}]});
        assert!(equivalent(&a, &b));
    }
}
