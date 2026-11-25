//! Agent, map and skin names from valorant-api.com (free, keyless, used by
//! virtually every VALORANT tool). Cached in the database for a few days.

use crate::db::Db;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

const CACHE_KEY: &str = "valorant-api:content:v2";
const TTL_SECS: i64 = 3 * 86_400;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Content {
    pub agents: HashMap<String, String>,
    /// mapUrl (`/Game/Maps/Ascent/Ascent`) and uuid → name.
    pub maps: HashMap<String, String>,
    pub skins: HashMap<String, String>,
    pub chromas: HashMap<String, String>,
}

impl Content {
    pub fn agent_name(&self, id: &str) -> Option<String> {
        self.agents.get(&id.to_ascii_lowercase()).cloned()
    }
    pub fn map_name(&self, id: &str) -> Option<String> {
        self.maps.get(&id.to_ascii_lowercase()).cloned()
    }
    pub fn skin_name(&self, id: &str) -> Option<String> {
        self.skins.get(&id.to_ascii_lowercase()).cloned()
    }
    pub fn chroma_name(&self, id: &str) -> Option<String> {
        self.chromas.get(&id.to_ascii_lowercase()).cloned()
    }
}

fn base() -> String {
    std::env::var("VALSYNC_CONTENT_BASE").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "https://valorant-api.com".into())
}

async fn data(http: &reqwest::Client, path: &str) -> Option<Vec<Value>> {
    let v: Value = http.get(format!("{}{path}", base())).send().await.ok()?.json().await.ok()?;
    v.get("data")?.as_array().cloned()
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// Latest client version string (fallback for the `X-Riot-ClientVersion` header).
pub async fn client_version(http: &reqwest::Client) -> Option<String> {
    let v: Value = http.get(format!("{}/v1/version", base())).send().await.ok()?.json().await.ok()?;
    v.get("data")?.get("riotClientVersion")?.as_str().map(str::to_string)
}

pub fn cached(db: &Db) -> Option<Content> {
    db.cache_get(CACHE_KEY).ok().flatten().and_then(|v| serde_json::from_str(&v).ok())
}

/// Cached content, downloading it when missing or stale. Never fails: unknown
/// names simply show as "Unknown".
pub async fn load(db: &Db) -> Content {
    if let Some(c) = cached(db) {
        return c;
    }
    let Ok(http) = reqwest::Client::builder().timeout(Duration::from_secs(15)).build() else { return Content::default() };
    let mut c = Content::default();
    for a in data(&http, "/v1/agents?isPlayableCharacter=true").await.unwrap_or_default() {
        c.agents.insert(s(&a, "uuid").to_ascii_lowercase(), s(&a, "displayName"));
    }
    for m in data(&http, "/v1/maps").await.unwrap_or_default() {
        let name = s(&m, "displayName");
        c.maps.insert(s(&m, "mapUrl").to_ascii_lowercase(), name.clone());
        c.maps.insert(s(&m, "uuid").to_ascii_lowercase(), name);
    }
    for sk in data(&http, "/v1/weapons/skins").await.unwrap_or_default() {
        c.skins.insert(s(&sk, "uuid").to_ascii_lowercase(), s(&sk, "displayName"));
        for ch in sk.get("chromas").and_then(Value::as_array).into_iter().flatten() {
            c.chromas.insert(s(ch, "uuid").to_ascii_lowercase(), s(ch, "displayName"));
        }
    }
    if !c.agents.is_empty()
        && let Ok(j) = serde_json::to_string(&c)
    {
        let _ = db.cache_put(CACHE_KEY, &j, TTL_SECS);
    }
    c
}
