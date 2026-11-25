//! An authenticated session for the account signed into the Riot Client.

use super::local::{self, LocalClient};
use super::models::*;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

/// `{"platformType":"PC","platformOS":"Windows",...}` base64 — the value every
/// client sends.
const CLIENT_PLATFORM: &str = "ew0KCSJwbGF0Zm9ybVR5cGUiOiAiUEMiLA0KCSJwbGF0Zm9ybU9TIjogIldpbmRvd3MiLA0KCSJwbGF0Zm9ybU9TVmVyc2lvbiI6ICIxMC4wLjE5MDQyLjEuMjU2LjY0Yml0IiwNCgkicGxhdGZvcm1DaGlwc2V0IjogIlVua25vd24iDQp9";
const FALLBACK_CLIENT_VERSION: &str = "release-11.10-shipping-14-3812345";
pub const SETTINGS_TYPE: &str = "Ares.PlayerSettings";

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("Riot service returned HTTP {0} for {1}")]
    Status(u16, String),
}

pub fn status_of(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<RemoteError>().map(|RemoteError::Status(s, _)| *s)
}

pub struct Session {
    pub puuid: String,
    pub game_name: Option<String>,
    pub tag_line: Option<String>,
    pub region: String,
    pub shard: String,
    pub client_version: String,
    access_token: String,
    entitlement: String,
    http: reqwest::Client,
    pref_host: Mutex<Option<&'static str>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print tokens.
        f.debug_struct("Session").field("puuid", &self.puuid).field("region", &self.region).finish_non_exhaustive()
    }
}

/// Decode the `data` field of a preferences response (base64 of raw DEFLATE).
pub fn decode_settings(data_b64: &str) -> Result<Value> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64.trim()).context("settings data is not base64")?;
    let mut raw = Vec::new();
    if flate2::read::DeflateDecoder::new(&bytes[..]).read_to_end(&mut raw).is_ok()
        && let Ok(v) = serde_json::from_slice(&raw)
    {
        return Ok(v);
    }
    let mut z = Vec::new();
    flate2::read::ZlibDecoder::new(&bytes[..]).read_to_end(&mut z).context("settings data is not deflate-compressed")?;
    serde_json::from_slice(&z).context("settings data is not JSON")
}

pub fn encode_settings(v: &Value) -> Result<String> {
    let json = serde_json::to_vec(v)?;
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&json)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(enc.finish()?))
}

/// Head and tail of the game log (it can grow to many MB; the version line is
/// near the start, the latest server hosts near the end).
fn read_log(path: &Path) -> String {
    use std::io::{Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    const CHUNK: u64 = 1 << 20;
    let mut buf = Vec::new();
    if len <= 2 * CHUNK {
        let _ = f.read_to_end(&mut buf);
    } else {
        let _ = (&mut f).take(CHUNK).read_to_end(&mut buf);
        buf.push(b'\n');
        if f.seek(SeekFrom::Start(len - CHUNK)).is_ok() {
            let _ = f.read_to_end(&mut buf);
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

impl Session {
    /// Connect to the Riot Client and build a session for the signed-in account.
    /// `logs_dir` is VALORANT's log folder; `known_region` a previously learned region.
    pub async fn connect(logs_dir: &Path, known_region: Option<&str>) -> Result<Session> {
        let local = LocalClient::connect()?;
        let ent = local.entitlements().await?;
        let chat = local.chat_session().await;
        let log = read_log(&logs_dir.join("ShooterGame.log"));
        let (region, shard) = match local.region_from_sessions().await {
            Some(rs) => rs,
            None => match local::region_from_log(&log) {
                Some(rs) => rs,
                None => {
                    let r = known_region.and_then(super::normalize_region).unwrap_or_else(|| "na".into());
                    let s = super::shard_for_region(&r).to_string();
                    (r, s)
                }
            },
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .connect_timeout(Duration::from_secs(6))
            .user_agent("ShooterGame/13 Windows/10.0.19043.1.256.64bit")
            .build()?;
        let client_version = match local::client_version_from_log(&log) {
            Some(v) => v,
            None => super::content::client_version(&http).await.unwrap_or_else(|| FALLBACK_CLIENT_VERSION.to_string()),
        };
        Ok(Session {
            puuid: ent.subject.to_ascii_lowercase(),
            game_name: chat.as_ref().map(|c| c.game_name.clone()),
            tag_line: chat.as_ref().map(|c| c.game_tag.clone()),
            region,
            shard,
            client_version,
            access_token: ent.access_token,
            entitlement: ent.token,
            http,
            pref_host: Mutex::new(None),
        })
    }

    pub fn riot_id(&self) -> Option<String> {
        match (&self.game_name, &self.tag_line) {
            (Some(n), Some(t)) if !n.is_empty() => Some(format!("{n}#{t}")),
            _ => None,
        }
    }

    fn pd(&self, path: &str) -> String {
        super::remote_url(&format!("pd.{}.a.pvp.net", self.shard), path)
    }

    fn glz(&self, path: &str) -> String {
        let region = if self.region == "pbe" { "na" } else { self.region.as_str() };
        super::remote_url(&format!("glz-{region}-1.{}.a.pvp.net", self.shard), path)
    }

    fn with_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.bearer_auth(&self.access_token)
            .header("X-Riot-Entitlements-JWT", &self.entitlement)
            .header("X-Riot-ClientPlatform", CLIENT_PLATFORM)
            .header("X-Riot-ClientVersion", &self.client_version)
    }

    async fn send(&self, req: reqwest::RequestBuilder, what: &str) -> Result<reqwest::Response> {
        let resp = self.with_headers(req).send().await.with_context(|| format!("network error ({what})"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(RemoteError::Status(status, what.to_string()).into());
        }
        Ok(resp)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, url: String, what: &str) -> Result<T> {
        let resp = self.send(self.http.get(url), what).await?;
        resp.json().await.with_context(|| format!("unexpected response format ({what})"))
    }

    // ------------------------------------------------------------ settings

    /// The account's cloud settings JSON (`Ares.PlayerSettings`).
    pub async fn get_settings(&self) -> Result<Value> {
        let hosts: Vec<&'static str> = match *self.pref_host.lock().unwrap() {
            Some(h) => vec![h],
            None => super::preference_hosts(&self.region),
        };
        let mut last = anyhow!("no preference host");
        for host in hosts {
            let url = super::remote_url(host, &format!("/playerPref/v3/getPreference/{SETTINGS_TYPE}"));
            match self.send(self.http.get(url), "get settings").await {
                Ok(resp) => {
                    let env: Value = resp.json().await.context("unexpected settings response")?;
                    let data = env.get("data").and_then(Value::as_str).unwrap_or_default();
                    if data.is_empty() {
                        // A brand-new account has no saved settings yet.
                        *self.pref_host.lock().unwrap() = Some(host);
                        return Ok(Value::Object(Default::default()));
                    }
                    let v = decode_settings(data)?;
                    *self.pref_host.lock().unwrap() = Some(host);
                    return Ok(v);
                }
                Err(e) => last = e,
            }
        }
        Err(last.context("could not read your settings from Riot"))
    }

    /// Replace the account's cloud settings.
    pub async fn put_settings(&self, v: &Value) -> Result<()> {
        let host = match *self.pref_host.lock().unwrap() {
            Some(h) => h,
            None => bail!("settings must be read before they are written"),
        };
        let body = serde_json::json!({ "type": SETTINGS_TYPE, "data": encode_settings(v)? });
        let url = super::remote_url(host, "/playerPref/v3/savePreference");
        self.send(self.http.put(url).json(&body), "save settings").await?;
        Ok(())
    }

    // ------------------------------------------------------------ pd

    pub async fn match_history(&self, count: usize) -> Result<MatchHistory> {
        let url = self.pd(&format!("/match-history/v1/history/{}?startIndex=0&endIndex={}", self.puuid, count.clamp(1, 20)));
        self.get(url, "match history").await
    }

    pub async fn match_details(&self, match_id: &str) -> Result<(Match, String)> {
        let resp = self.send(self.http.get(self.pd(&format!("/match-details/v1/matches/{match_id}"))), "match details").await?;
        let raw = resp.text().await?;
        let m: Match = serde_json::from_str(&raw).context("unexpected match format")?;
        Ok((m, raw))
    }

    pub async fn mmr(&self) -> Result<Mmr> {
        self.get(self.pd(&format!("/mmr/v1/players/{}", self.puuid)), "rank").await
    }

    pub async fn account_level(&self) -> Option<i32> {
        let xp: AccountXp = self.get(self.pd(&format!("/account-xp/v1/players/{}", self.puuid)), "level").await.ok()?;
        (xp.progress.level > 0).then_some(xp.progress.level)
    }

    pub async fn names(&self, puuids: &[String]) -> Result<Vec<NameEntry>> {
        if puuids.is_empty() {
            return Ok(Vec::new());
        }
        let resp = self.send(self.http.put(self.pd("/name-service/v2/players")).json(puuids), "names").await?;
        Ok(resp.json().await.unwrap_or_default())
    }

    // ------------------------------------------------------------ glz (live)

    async fn optional<T: serde::de::DeserializeOwned>(&self, url: String, what: &str) -> Result<Option<T>> {
        match self.get::<T>(url, what).await {
            Ok(v) => Ok(Some(v)),
            Err(e) if status_of(&e) == Some(404) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn current_game(&self) -> Result<Option<CoreGameMatch>> {
        let Some(r) = self.optional::<CurrentMatchRef>(self.glz(&format!("/core-game/v1/players/{}", self.puuid)), "current game").await? else {
            return Ok(None);
        };
        if r.match_id.is_empty() {
            return Ok(None);
        }
        self.optional(self.glz(&format!("/core-game/v1/matches/{}", r.match_id)), "current match").await
    }

    pub async fn current_loadouts(&self, match_id: &str) -> Result<Loadouts> {
        self.get(self.glz(&format!("/core-game/v1/matches/{match_id}/loadouts")), "loadouts").await
    }

    pub async fn pregame(&self) -> Result<Option<PregameMatch>> {
        let Some(r) = self.optional::<CurrentMatchRef>(self.glz(&format!("/pregame/v1/players/{}", self.puuid)), "agent select").await? else {
            return Ok(None);
        };
        if r.match_id.is_empty() {
            return Ok(None);
        }
        self.optional(self.glz(&format!("/pregame/v1/matches/{}", r.match_id)), "agent select match").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_codec_roundtrip() {
        let v = serde_json::json!({"floatSettings":[{"settingEnum":"EAresFloatSettingName::MouseSensitivity","value":0.25}]});
        let enc = encode_settings(&v).unwrap();
        assert_eq!(decode_settings(&enc).unwrap(), v);
        // zlib-wrapped data (some tools write it) is accepted too.
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(serde_json::to_string(&v).unwrap().as_bytes()).unwrap();
        let zb = base64::engine::general_purpose::STANDARD.encode(z.finish().unwrap());
        assert_eq!(decode_settings(&zb).unwrap(), v);
    }
}
