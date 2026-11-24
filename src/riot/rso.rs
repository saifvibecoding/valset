//! Riot Sign On (RSO) — the OAuth 2.0 authorization-code flow Riot requires for
//! players to opt in to sharing VALORANT data with a third-party product.
//!
//! Requirements (issued by Riot to approved products only):
//! * an RSO `client_id` and `client_secret`;
//! * the redirect URI `http://127.0.0.1:<port>/oauth-callback` registered for
//!   the client.
//!
//! VALSYNC opens the official Riot login page in the user's browser; the user
//! signs in **on Riot's page** (VALSYNC never sees the password). Riot redirects
//! back to a one-shot local listener with an authorization code, which is
//! exchanged for an access token used exactly once to call
//! `ACCOUNT-V1 /accounts/me`. The token is then dropped: VALSYNC stores only the
//! PUUID, Riot ID and the fact/time of consent.

use super::models::{AccountDto, TokenResponse};
use super::RiotApi;
use anyhow::{Context, Result, anyhow, bail};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const AUTH_BASE: &str = "https://auth.riotgames.com";

#[derive(Debug, Clone)]
pub struct RsoConfig {
    pub client_id: String,
    pub client_secret: String,
    pub port: u16,
    pub region: String,
}

impl RsoConfig {
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/oauth-callback", self.port)
    }
}

fn auth_base() -> String {
    std::env::var("VALSYNC_RSO_BASE").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| AUTH_BASE.to_string())
}

fn random_state() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(format!("{:?}", std::time::SystemTime::now()).as_bytes());
    h.update(std::process::id().to_le_bytes());
    let local = 0u8;
    h.update(format!("{:p}", &local).as_bytes());
    hex::encode(h.finalize())[..32].to_string()
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                if let Some(v) = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

pub fn authorize_url(cfg: &RsoConfig, state: &str) -> String {
    format!(
        "{}/authorize?client_id={}&redirect_uri={}&response_type=code&scope=openid&state={}",
        auth_base(),
        urlencode(&cfg.client_id),
        urlencode(&cfg.redirect_uri()),
        state
    )
}

/// Open a URL in the default browser without going through a shell parser.
pub fn open_browser(url: &str) -> bool {
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    r.is_ok()
}

/// Parse `GET /oauth-callback?code=..&state=.. HTTP/1.1`.
pub fn parse_callback(request: &str) -> Result<(String, String)> {
    let line = request.lines().next().unwrap_or_default();
    let path = line.split_whitespace().nth(1).context("malformed callback request")?;
    let query = path.split_once('?').map(|x| x.1).unwrap_or("");
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "code" => code = Some(urldecode(v)),
            "state" => state = Some(urldecode(v)),
            "error" => error = Some(urldecode(v)),
            _ => {}
        }
    }
    if let Some(e) = error {
        bail!("Riot Sign On returned an error: {e}");
    }
    Ok((code.context("callback did not contain an authorization code")?, state.unwrap_or_default()))
}

/// Run the complete RSO flow. `on_url` receives the login URL (to print it).
pub async fn login(cfg: &RsoConfig, api: &RiotApi, open: bool, on_url: impl Fn(&str)) -> Result<AccountDto> {
    let state = random_state();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", cfg.port))
        .await
        .with_context(|| format!("cannot listen on 127.0.0.1:{} for the RSO redirect", cfg.port))?;
    let url = authorize_url(cfg, &state);
    on_url(&url);
    if open {
        open_browser(&url);
    }
    let code = tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            let (mut sock, _) = listener.accept().await?;
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await?;
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            if !req.starts_with("GET /oauth-callback") {
                let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                continue;
            }
            let result = parse_callback(&req);
            let body = match &result {
                Ok((_, s)) if s == &state => "<html><body style='font-family:sans-serif'><h2>VALSYNC: sign-in complete</h2><p>You can close this window and return to the terminal.</p></body></html>",
                _ => "<html><body style='font-family:sans-serif'><h2>VALSYNC: sign-in failed</h2><p>Return to the terminal for details.</p></body></html>",
            };
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
            let (code, got_state) = result?;
            if got_state != state {
                bail!("RSO state mismatch (possible CSRF); sign-in aborted");
            }
            return Ok::<String, anyhow::Error>(code);
        }
    })
    .await
    .map_err(|_| anyhow!("timed out waiting for the Riot Sign On redirect (5 minutes)"))??;

    let client = reqwest::Client::builder().timeout(Duration::from_secs(15)).build()?;
    let resp = client
        .post(format!("{}/token", auth_base()))
        .basic_auth(&cfg.client_id, Some(&cfg.client_secret))
        .form(&[("grant_type", "authorization_code"), ("code", code.as_str()), ("redirect_uri", cfg.redirect_uri().as_str())])
        .send()
        .await
        .context("token exchange request failed")?;
    if !resp.status().is_success() {
        bail!("RSO token exchange failed with HTTP {} (check client id/secret and redirect URI registration)", resp.status().as_u16());
    }
    let tok: TokenResponse = resp.json().await.context("invalid token response")?;
    let account = api.account_me(&cfg.region, &tok.access_token).await.map_err(|e| anyhow!("{e}"))?;
    drop(tok); // never persisted
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_parsing() {
        let (c, s) = parse_callback("GET /oauth-callback?code=ab%2Fc&state=xyz HTTP/1.1\r\nHost: x\r\n").unwrap();
        assert_eq!(c, "ab/c");
        assert_eq!(s, "xyz");
        assert!(parse_callback("GET /oauth-callback?error=access_denied HTTP/1.1").is_err());
    }
    #[test]
    fn url_has_redirect() {
        let cfg = RsoConfig { client_id: "id".into(), client_secret: "s".into(), port: 5000, region: "americas".into() };
        let u = authorize_url(&cfg, "st");
        assert!(u.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2Foauth-callback"));
        assert!(u.contains("scope=openid"));
    }
}
