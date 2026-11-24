//! Minimal async Riot API client with SQLite-backed response caching.

use super::models::*;
use crate::db::Db;
use serde::de::DeserializeOwned;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("no Riot API key configured (set RIOT_API_KEY or `valsync config set riot_api_key <key>`). Note: Riot only serves VALORANT data to approved production keys.")]
    NoKey,
    #[error("Riot API rejected the request ({0}). Personal and development keys cannot access VALORANT APIs; a production key from an approved product is required, and keys expire if not renewed.")]
    Unauthorized(u16),
    #[error("not found (404)")]
    NotFound,
    #[error("rate limited by the Riot API; retry in {0}s")]
    RateLimited(u64),
    #[error("Riot API error {0}: {1}")]
    Http(u16, String),
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected response format: {0}")]
    Decode(String),
}

#[derive(Clone)]
pub struct RiotApi {
    client: reqwest::Client,
    key: Option<String>,
    /// Test/proxy override: requests go to `{base}/{route}{path}` instead of
    /// `https://{route}.api.riotgames.com{path}`.
    base_override: Option<String>,
}

impl RiotApi {
    pub fn new(key: Option<String>) -> RiotApi {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("VALSYNC/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        RiotApi { client, key, base_override: std::env::var("VALSYNC_RIOT_BASE").ok().filter(|s| !s.is_empty()) }
    }

    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    fn url(&self, route: &str, path: &str) -> String {
        match &self.base_override {
            Some(b) => format!("{}/{route}{path}", b.trim_end_matches('/')),
            None => format!("https://{route}.api.riotgames.com{path}"),
        }
    }

    async fn get_raw(&self, route: &str, path: &str, bearer: Option<&str>) -> Result<String, ApiError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut req = self.client.get(self.url(route, path));
            req = match bearer {
                Some(tok) => req.bearer_auth(tok),
                None => req.header("X-Riot-Token", self.key.as_deref().ok_or(ApiError::NoKey)?),
            };
            let resp = req.send().await.map_err(|e| ApiError::Network(e.to_string()))?;
            let status = resp.status().as_u16();
            match status {
                200 => return resp.text().await.map_err(|e| ApiError::Network(e.to_string())),
                401 | 403 => return Err(ApiError::Unauthorized(status)),
                404 => return Err(ApiError::NotFound),
                429 => {
                    let wait = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(10);
                    if attempt == 1 && wait <= 5 {
                        tokio::time::sleep(Duration::from_secs(wait)).await;
                        continue;
                    }
                    return Err(ApiError::RateLimited(wait));
                }
                500..=599 if attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    continue;
                }
                _ => {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(ApiError::Http(status, body.chars().take(200).collect()));
                }
            }
        }
    }

    async fn get<T: DeserializeOwned>(&self, route: &str, path: &str, bearer: Option<&str>) -> Result<T, ApiError> {
        let text = self.get_raw(route, path, bearer).await?;
        serde_json::from_str(&text).map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// GET with a cache entry in SQLite (`ttl` seconds).
    async fn cached<T: DeserializeOwned + serde::Serialize>(&self, db: &mut Db, key: &str, ttl: i64, route: &str, path: &str) -> Result<T, ApiError> {
        if let Ok(Some(v)) = db.cache_get(key)
            && let Ok(t) = serde_json::from_str::<T>(&v)
        {
            return Ok(t);
        }
        let t: T = self.get(route, path, None).await?;
        if let Ok(s) = serde_json::to_string(&t) {
            let _ = db.cache_put(key, &s, ttl);
        }
        Ok(t)
    }

    fn enc(s: &str) -> String {
        let mut out = String::new();
        for b in s.as_bytes() {
            match *b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    // ------------------------------------------------------------ ACCOUNT-V1
    pub async fn account_by_puuid(&self, db: &mut Db, region: &str, puuid: &str) -> Result<AccountDto, ApiError> {
        self.cached(db, &format!("acct:puuid:{puuid}"), 86_400, region, &format!("/riot/account/v1/accounts/by-puuid/{}", Self::enc(puuid))).await
    }

    pub async fn account_by_riot_id(&self, db: &mut Db, region: &str, name: &str, tag: &str) -> Result<AccountDto, ApiError> {
        let key = format!("acct:rid:{}#{}", name.to_lowercase(), tag.to_lowercase());
        self.cached(db, &key, 3_600, region, &format!("/riot/account/v1/accounts/by-riot-id/{}/{}", Self::enc(name), Self::enc(tag))).await
    }

    /// RSO: the account that authorised the access token (never cached).
    pub async fn account_me(&self, region: &str, access_token: &str) -> Result<AccountDto, ApiError> {
        self.get(region, "/riot/account/v1/accounts/me", Some(access_token)).await
    }

    pub async fn active_shard(&self, db: &mut Db, region: &str, puuid: &str) -> Result<ActiveShardDto, ApiError> {
        self.cached(db, &format!("shard:{puuid}"), 86_400, region, &format!("/riot/account/v1/active-shards/by-game/val/by-puuid/{}", Self::enc(puuid))).await
    }

    // ------------------------------------------------------------ VAL-CONTENT-V1 / VAL-STATUS-V1
    pub async fn content(&self, db: &mut Db, shard: &str, locale: &str) -> Result<Content, ApiError> {
        let c: Content = self.cached(db, &format!("content:{shard}:{locale}"), 86_400, shard, &format!("/val/content/v1/contents?locale={}", Self::enc(locale))).await?;
        let _ = db.meta_set("content_version", &c.version);
        Ok(c)
    }

    pub async fn platform_status(&self, db: &mut Db, shard: &str) -> Result<PlatformData, ApiError> {
        self.cached(db, &format!("status:{shard}"), 300, shard, "/val/status/v1/platform-data").await
    }

    // ------------------------------------------------------------ VAL-MATCH-V1
    pub async fn matchlist(&self, db: &mut Db, shard: &str, puuid: &str) -> Result<Matchlist, ApiError> {
        self.cached(db, &format!("matchlist:{shard}:{puuid}"), 60, shard, &format!("/val/match/v1/matchlists/by-puuid/{}", Self::enc(puuid))).await
    }

    pub async fn match_by_id(&self, shard: &str, match_id: &str) -> Result<(Match, String), ApiError> {
        let raw = self.get_raw(shard, &format!("/val/match/v1/matches/{}", Self::enc(match_id)), None).await?;
        let m: Match = serde_json::from_str(&raw).map_err(|e| ApiError::Decode(e.to_string()))?;
        Ok((m, raw))
    }

    /// Fetch up to `limit` matches from the matchlist that are not stored yet.
    /// Returns the number of newly stored matches.
    pub async fn sync_matches(&self, db: &mut Db, shard: &str, api_puuid: &str, owner_local: &str, limit: usize) -> Result<usize, ApiError> {
        let list = self.matchlist(db, shard, api_puuid).await?;
        let mut entries = list.history.clone();
        entries.sort_by_key(|e| std::cmp::Reverse(e.game_start_time_millis));
        let mut new = 0;
        for e in entries.into_iter().take(limit) {
            if db.has_match(&e.match_id, owner_local).unwrap_or(false) {
                continue;
            }
            let (m, _raw) = self.match_by_id(shard, &e.match_id).await?;
            // Store our compact re-serialisation (drops large unused fields).
            let compact = serde_json::to_string(&m).map_err(|e| ApiError::Decode(e.to_string()))?;
            let _ = db.store_match(&e.match_id, owner_local, m.match_info.game_start_millis, &m.match_info.queue_id, &compact);
            new += 1;
        }
        Ok(new)
    }
}
