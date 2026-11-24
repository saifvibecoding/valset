//! Classification of VALORANT configuration keys into user-facing categories and
//! storage scopes.
//!
//! VALORANT keeps two kinds of settings:
//!
//! * **Local** settings are read from the `.ini` files in
//!   `%LOCALAPPDATA%\VALORANT\Saved\Config` (video/graphics/display settings).
//!   VALSYNC can reliably save, transfer and restore these.
//! * **Cloud** (account-synced) settings are stored by Riot on its servers and
//!   pushed to the client when an account logs in (sensitivity, crosshair,
//!   keybinds, most general/HUD/audio options). Any local copy is only a cache
//!   that the game overwrites from the server. There is no public, supported
//!   Riot API for reading or writing them, so VALSYNC records whatever is visible
//!   in the local cache and presents it as a manual checklist instead of claiming
//!   it was transferred.
//!
//! Some keys are **excluded** entirely: identifiers, authentication data,
//! machine-specific hardware state and cosmetic ownership.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    MouseSensitivity,
    ScopedSensitivity,
    Crosshair,
    Keybinds,
    AgentKeybinds,
    Resolution,
    GraphicsQuality,
    DisplayMode,
    FpsLimits,
    NvidiaReflex,
    Audio,
    Minimap,
    Hud,
    EnemyHighlight,
    Accessibility,
    Gameplay,
    Other,
}

impl Category {
    pub const ALL: [Category; 17] = [
        Category::MouseSensitivity,
        Category::ScopedSensitivity,
        Category::Crosshair,
        Category::Keybinds,
        Category::AgentKeybinds,
        Category::Resolution,
        Category::GraphicsQuality,
        Category::DisplayMode,
        Category::FpsLimits,
        Category::NvidiaReflex,
        Category::Audio,
        Category::Minimap,
        Category::Hud,
        Category::EnemyHighlight,
        Category::Accessibility,
        Category::Gameplay,
        Category::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::MouseSensitivity => "Mouse sensitivity",
            Category::ScopedSensitivity => "Scoped / ADS sensitivity",
            Category::Crosshair => "Crosshair profiles",
            Category::Keybinds => "Keybinds",
            Category::AgentKeybinds => "Agent-specific keybinds",
            Category::Resolution => "Resolution",
            Category::GraphicsQuality => "Graphics quality",
            Category::DisplayMode => "Display mode",
            Category::FpsLimits => "FPS limits",
            Category::NvidiaReflex => "NVIDIA Reflex",
            Category::Audio => "Audio",
            Category::Minimap => "Minimap",
            Category::Hud => "HUD",
            Category::EnemyHighlight => "Enemy highlight color",
            Category::Accessibility => "Accessibility",
            Category::Gameplay => "General gameplay",
            Category::Other => "Other",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Category::MouseSensitivity => "sensitivity",
            Category::ScopedSensitivity => "scoped",
            Category::Crosshair => "crosshair",
            Category::Keybinds => "keybinds",
            Category::AgentKeybinds => "agent-keybinds",
            Category::Resolution => "resolution",
            Category::GraphicsQuality => "graphics",
            Category::DisplayMode => "display",
            Category::FpsLimits => "fps",
            Category::NvidiaReflex => "reflex",
            Category::Audio => "audio",
            Category::Minimap => "minimap",
            Category::Hud => "hud",
            Category::EnemyHighlight => "enemy-highlight",
            Category::Accessibility => "accessibility",
            Category::Gameplay => "gameplay",
            Category::Other => "other",
        }
    }

    pub fn from_slug(s: &str) -> Option<Category> {
        let s = s.trim().to_ascii_lowercase();
        Category::ALL.into_iter().find(|c| c.slug() == s || c.label().eq_ignore_ascii_case(&s))
    }

    /// Video-type settings are machine-local in VALORANT.
    pub fn is_video(self) -> bool {
        matches!(
            self,
            Category::Resolution
                | Category::GraphicsQuality
                | Category::DisplayMode
                | Category::FpsLimits
                | Category::NvidiaReflex
        )
    }

    /// Where VALORANT normally keeps this category (used for labelling even when
    /// no key of the category was found on disk).
    pub fn typical_scope(self) -> Scope {
        if self.is_video() || self == Category::Other { Scope::Local } else { Scope::Cloud }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Stored in local config files and applied by the game from disk.
    Local,
    /// Account-synced on Riot servers; the local value is a cache.
    Cloud,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Local => "local",
            Scope::Cloud => "cloud",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcludeReason {
    Identity,
    MachineSpecific,
    Cosmetics,
    FileMetadata,
}

impl ExcludeReason {
    pub fn label(self) -> &'static str {
        match self {
            ExcludeReason::Identity => "account identifier / authentication data",
            ExcludeReason::MachineSpecific => "machine-specific hardware state",
            ExcludeReason::Cosmetics => "cosmetic ownership / loadout",
            ExcludeReason::FileMetadata => "file format metadata",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    Setting { category: Category, scope: Scope },
    Excluded(ExcludeReason),
}

fn has_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

/// Does this look like a UUID (PUUIDs are UUID formatted)?
pub fn looks_like_uuid(s: &str) -> bool {
    let s = s.trim();
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(n, p)| p.len() == *n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Classify a single configuration entry.
///
/// `file` is the path relative to the account folder (e.g. `Windows/GameUserSettings.ini`).
pub fn classify(file: &str, section: &str, key: &str, value: &str) -> Classification {
    let k = key.trim_start_matches(['+', '-', '.', '!']).to_ascii_lowercase();
    let sec = section.to_ascii_lowercase();
    let file_l = file.to_ascii_lowercase();
    let v = value.to_ascii_lowercase();

    // ---- Exclusions -------------------------------------------------------
    if has_any(
        &k,
        &[
            "puuid", "token", "cookie", "session", "password", "passwd", "auth", "accountid", "account_id",
            "userid", "user_id", "lastknownuser", "subject", "entitlement", "credential", "secret", "email",
            "gamename", "tagline", "riotid",
        ],
    ) || sec.contains("userinfo")
        || v.contains("bearer ")
        || looks_like_uuid(value)
    {
        return Classification::Excluded(ExcludeReason::Identity);
    }
    if has_any(
        &k,
        &[
            "monitordeviceid", "monitorindex", "windowpos", "benchmark", "gpubenchmark", "cpubenchmark",
            "lastrecommended", "adapter", "deviceid", "gpu_name", "lastopened", "hardware",
        ],
    ) || sec.contains("shaderpipelinecache")
    {
        return Classification::Excluded(ExcludeReason::MachineSpecific);
    }
    if has_any(&k, &["loadout", "skin", "chroma", "gunbuddy", "playercard", "playertitle", "spray", "charm", "totem"]) {
        return Classification::Excluded(ExcludeReason::Cosmetics);
    }
    if k == "version" || k == "configversion" || k == "settingsversion" {
        return Classification::Excluded(ExcludeReason::FileMetadata);
    }

    // ---- Categories -------------------------------------------------------
    let category = if k.contains("sensitivity") || k.contains("invertmouse") || k.contains("rawinput") || k.contains("mousesmoothing") {
        if has_any(&k, &["zoom", "ads", "scope", "aim"]) { Category::ScopedSensitivity } else { Category::MouseSensitivity }
    } else if k.contains("crosshair") {
        Category::Crosshair
    } else if has_any(&k, &["minimap"]) {
        Category::Minimap
    } else if has_any(&k, &["enemyhighlight", "enemycolor", "highlightcolor", "enemyoutline"]) {
        Category::EnemyHighlight
    } else if has_any(&k, &["actionmapping", "axismapping", "keybind", "binding", "inputmapping", "keymapping", "speedkeys"])
        || sec.contains("input")
    {
        if has_any(&k, &["agent", "character", "ability", "characterspecific"]) { Category::AgentKeybinds } else { Category::Keybinds }
    } else if has_any(&k, &["reflex", "nvidia", "lowlatency"]) {
        Category::NvidiaReflex
    } else if has_any(&k, &["framerate", "fps", "maxframe"]) {
        Category::FpsLimits
    } else if has_any(&k, &["volume", "audio", "sound", "music", "voicechat", "voice", "speaker", "hrtf", "microphone", "pushtotalk", "vo_"]) {
        Category::Audio
    } else if has_any(&k, &["resolutionsize", "desiredscreen", "screenwidth", "screenheight", "letterbox", "aspectratio", "usedesiredscreenheight"]) {
        Category::Resolution
    } else if has_any(&k, &["fullscreenmode", "windowmode", "displaymode", "hdr", "brightness", "gamma"]) {
        Category::DisplayMode
    } else if sec.contains("scalabilitygroups")
        || k.starts_with("sg.")
        || has_any(
            &k,
            &[
                "quality", "antialias", "anisotrop", "vsync", "bloom", "distortion", "shadow", "clarity",
                "sharpen", "vignette", "dynamicresolution", "multithread", "texture", "detail", "graphics",
                "castshadows", "firstpersonshadows",
            ],
        )
    {
        Category::GraphicsQuality
    } else if has_any(&k, &["colorblind", "accessib", "subtitle", "caption", "highcontrast", "texttospeech", "speechtotext", "flash", "screenshake", "reducedmotion"]) {
        Category::Accessibility
    } else if has_any(&k, &["hud", "overlay", "killfeed", "performancestat", "showfps", "scoreboard", "teammate", "spectat", "networkstat", "corpse", "blood", "uiscale", "ui_", "chat"]) {
        Category::Hud
    } else if k.starts_with("eares") || sec.contains("ares") || sec.contains("shootergame") || file_l.contains("riotusersettings") {
        Category::Gameplay
    } else {
        Category::Other
    };

    let scope = if category.is_video() {
        Scope::Local
    } else if file_l.contains("riotusersettings") {
        Scope::Cloud
    } else if file_l.contains("gameusersettings") || category == Category::Other {
        Scope::Local
    } else {
        Scope::Cloud
    };
    Classification::Setting { category, scope }
}

/// A short static description of how each category is stored, used by `doctor`
/// and the TUI so users understand what can actually be transferred.
pub fn category_support_note(c: Category) -> &'static str {
    match c.typical_scope() {
        Scope::Local => "Stored in local config files: saved, transferred and restored by VALSYNC.",
        Scope::Cloud => {
            "Account-synced by Riot servers: no supported API exists to write it. VALSYNC records values found in the \
             local cache and shows them as a manual checklist (optional best-effort cache write in Settings)."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(file: &str, sec: &str, key: &str, val: &str) -> Option<(Category, Scope)> {
        match classify(file, sec, key, val) {
            Classification::Setting { category, scope } => Some((category, scope)),
            Classification::Excluded(_) => None,
        }
    }

    #[test]
    fn classifies_common_keys() {
        let g = "Windows/GameUserSettings.ini";
        let s = "/Script/ShooterGame.ShooterGameUserSettings";
        assert_eq!(cat(g, s, "ResolutionSizeX", "1920"), Some((Category::Resolution, Scope::Local)));
        assert_eq!(cat(g, s, "FullscreenMode", "0"), Some((Category::DisplayMode, Scope::Local)));
        assert_eq!(cat(g, s, "FrameRateLimit", "240.0"), Some((Category::FpsLimits, Scope::Local)));
        assert_eq!(cat(g, "ScalabilityGroups", "sg.ShadowQuality", "0"), Some((Category::GraphicsQuality, Scope::Local)));
        assert_eq!(cat(g, s, "NvidiaReflexLowLatency", "2"), Some((Category::NvidiaReflex, Scope::Local)));
        let r = "Windows/RiotUserSettings.ini";
        assert_eq!(cat(r, "Settings", "EAresFloatSettingName::MouseSensitivity", "0.25"), Some((Category::MouseSensitivity, Scope::Cloud)));
        assert_eq!(cat(r, "Settings", "EAresFloatSettingName::MouseSensitivityZoomed", "1.0"), Some((Category::ScopedSensitivity, Scope::Cloud)));
        assert_eq!(cat(r, "Settings", "EAresStringSettingName::SavedCrosshairProfileData", "{}"), Some((Category::Crosshair, Scope::Cloud)));
        assert_eq!(cat(r, "Settings", "EAresFloatSettingName::MinimapSize", "1.1"), Some((Category::Minimap, Scope::Cloud)));
    }

    #[test]
    fn excludes_identity_and_machine_data() {
        assert!(matches!(classify("RiotLocalMachine.ini", "UserInfo", "LastKnownUser", "x"), Classification::Excluded(ExcludeReason::Identity)));
        assert!(matches!(classify("a.ini", "S", "SomeKey", "0d1a7e4b-1111-2222-3333-444455556666"), Classification::Excluded(ExcludeReason::Identity)));
        assert!(matches!(classify("a.ini", "S", "DefaultMonitorDeviceID", "PCI"), Classification::Excluded(ExcludeReason::MachineSpecific)));
        assert!(matches!(classify("a.ini", "S", "EquippedSkinVandal", "x"), Classification::Excluded(ExcludeReason::Cosmetics)));
        assert!(matches!(classify("a.ini", "S", "Version", "5"), Classification::Excluded(ExcludeReason::FileMetadata)));
    }
}
