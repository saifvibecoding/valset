//! Realistic synthetic VALORANT config folders, used by the test-suite and by
//! `valsync bench` (so benchmarks never touch real game files).

use std::path::Path;

pub struct SampleAccount<'a> {
    pub puuid: &'a str,
    pub shard: &'a str,
    pub sensitivity: &'a str,
    pub resolution: (u32, u32),
    pub fps_limit: &'a str,
    pub shadow_quality: u8,
}

pub fn game_user_settings(a: &SampleAccount) -> String {
    format!(
        "[/Script/ShooterGame.ShooterGameUserSettings]\r\n\
DefaultMonitorDeviceID=\\\\?\\DISPLAY#GSM5BBF#5&2a5b1c3&0&UID4352\r\n\
DefaultMonitorIndex=0\r\n\
bShouldLetterbox=False\r\n\
bUseVSync=False\r\n\
bUseDynamicResolution=False\r\n\
ResolutionSizeX={x}\r\n\
ResolutionSizeY={y}\r\n\
LastUserConfirmedResolutionSizeX={x}\r\n\
LastUserConfirmedResolutionSizeY={y}\r\n\
WindowPosX=-1\r\n\
WindowPosY=-1\r\n\
FullscreenMode=0\r\n\
LastConfirmedFullscreenMode=0\r\n\
PreferredFullscreenMode=0\r\n\
Version=5\r\n\
AudioQualityLevel=0\r\n\
FrameRateLimit={fps}\r\n\
DesiredScreenWidth={x}\r\n\
DesiredScreenHeight={y}\r\n\
LastCPUBenchmarkResult=-1.000000\r\n\
LastGPUBenchmarkResult=-1.000000\r\n\
NvidiaReflexLowLatencySetting=2\r\n\
bUseHDRDisplayOutput=False\r\n\
\r\n\
[ScalabilityGroups]\r\n\
sg.ResolutionQuality=100.000000\r\n\
sg.ViewDistanceQuality=3\r\n\
sg.AntiAliasingQuality=1\r\n\
sg.ShadowQuality={sh}\r\n\
sg.PostProcessQuality=0\r\n\
sg.TextureQuality=0\r\n\
sg.EffectsQuality=0\r\n\
sg.FoliageQuality=0\r\n\
sg.ShadingQuality=0\r\n\
\r\n\
[ShaderPipelineCache.CacheFile]\r\n\
LastOpened=ShooterGame\r\n",
        x = a.resolution.0,
        y = a.resolution.1,
        fps = a.fps_limit,
        sh = a.shadow_quality
    )
}

pub fn riot_user_settings(a: &SampleAccount) -> String {
    format!(
        "[Settings]\r\n\
EAresFloatSettingName::MouseSensitivity={sens}\r\n\
EAresFloatSettingName::MouseSensitivityZoomed=1.000000\r\n\
EAresFloatSettingName::MouseSensitivityADS=1.000000\r\n\
EAresStringSettingName::SavedCrosshairProfileData=0;P;c;5;h;0;f;0;0l;4;0o;2;0a;1;0f;0;1b;0\r\n\
EAresFloatSettingName::MinimapSize=1.100000\r\n\
EAresBoolSettingName::MinimapRotates=False\r\n\
EAresFloatSettingName::OverallVolume=0.600000\r\n\
EAresIntSettingName::ColorBlindMode=0\r\n\
EAresIntSettingName::EnemyHighlightColor=3\r\n\
EAresBoolSettingName::ShowCorpses=False\r\n\
EAresBoolSettingName::ShowBlood=True\r\n\
EAresStringSettingName::PlayerUuid={puuid}\r\n\
\r\n\
[Input]\r\n\
+ActionMappings=(ActionName=\"Ability1\",Key=Q)\r\n\
+ActionMappings=(ActionName=\"Ability2\",Key=E)\r\n",
        sens = a.sensitivity,
        puuid = a.puuid
    )
}

/// Create `<config>/<puuid>-<shard>/Windows/{GameUserSettings,RiotUserSettings}.ini`.
pub fn write_account(config_root: &Path, a: &SampleAccount) -> std::io::Result<std::path::PathBuf> {
    let dir = config_root.join(format!("{}-{}", a.puuid, a.shard));
    let win = dir.join("Windows");
    std::fs::create_dir_all(&win)?;
    std::fs::write(win.join("GameUserSettings.ini"), game_user_settings(a))?;
    std::fs::write(win.join("RiotUserSettings.ini"), riot_user_settings(a))?;
    Ok(dir)
}

pub fn set_last_known_user(config_root: &Path, puuid: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(config_root)?;
    std::fs::write(config_root.join("RiotLocalMachine.ini"), format!("[UserInfo]\r\nLastKnownUser={puuid}\r\n"))
}

pub const PUUID_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
pub const PUUID_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";

pub fn account_a() -> SampleAccount<'static> {
    SampleAccount { puuid: PUUID_A, shard: "eu", sensitivity: "0.176000", resolution: (1920, 1080), fps_limit: "240.000000", shadow_quality: 0 }
}

pub fn account_b() -> SampleAccount<'static> {
    SampleAccount { puuid: PUUID_B, shard: "eu", sensitivity: "0.250000", resolution: (1280, 960), fps_limit: "0.000000", shadow_quality: 3 }
}
