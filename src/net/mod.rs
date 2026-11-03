//! HTTP client: timeouts, retries, backoff, redirect allowlist and API key handling.

use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{RequestBuilder, Response, StatusCode, Url, redirect};
use sha1::{Digest, Sha1};
use tokio::io::AsyncWriteExt;

use crate::cancel;
use crate::error::AppError;

pub const ENV_API_KEY: &str = "WOWADDONS_CURSEFORGE_API_KEY";
const MAX_REDIRECTS: usize = 5;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// An API key. Never printed: `Debug` and `Display` show `***`.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Option<ApiKey> {
        let key = key.into().trim().to_string();
        (!key.is_empty()).then_some(ApiKey(key))
    }

    pub fn from_env() -> Option<ApiKey> {
        std::env::var(ENV_API_KEY).ok().and_then(ApiKey::new)
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

/// Hosts that may receive the API key.
#[derive(Clone, Debug)]
pub struct HostAllowlist {
    exact: Vec<String>,
    suffixes: Vec<String>,
}

impl HostAllowlist {
    /// Built-in list: the CurseForge API and its CDN.
    pub fn curseforge() -> Self {
        HostAllowlist {
            exact: vec!["api.curseforge.com".into(), "forgecdn.net".into()],
            suffixes: vec![".forgecdn.net".into()],
        }
    }

    pub fn with_host(mut self, host: &str) -> Self {
        self.exact.push(host.to_ascii_lowercase());
        self
    }

    /// HTTPS to an allowed host. Plain HTTP only for loopback hosts (tests).
    pub fn allows(&self, url: &Url) -> bool {
        let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
            return false;
        };
        let host_ok = self.exact.contains(&host) || self.suffixes.iter().any(|s| host.ends_with(s));
        let scheme_ok = match url.scheme() {
            "https" => true,
            "http" => is_loopback(&host),
            _ => false,
        };
        host_ok && scheme_ok
    }
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1"
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("authentication refused by {host} (HTTP {status})")]
    Auth { status: u16, host: String },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("rate limited by the server")]
    RateLimited,
    #[error("request timed out")]
    Timeout,
    #[error("network error: {0}")]
    Connect(String),
    #[error("HTTP {status} from {url}")]
    Http { status: u16, url: String },
    #[error("refused to contact a host that is not allowed: {0}")]
    HostNotAllowed(String),
    #[error("download is larger than the limit ({limit_mb} MB)")]
    TooLarge { limit_mb: u64 },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("the author does not allow third-party downloads of this file")]
    DownloadNotAllowed,
    #[error("I/O error: {0}")]
    Io(String),
    #[error("cancelled")]
    Cancelled,
}

impl NetError {
    /// Errors that must stop the whole run (never work around 401/403).
    pub fn is_fatal(&self) -> bool {
        matches!(self, NetError::Auth { .. } | NetError::Cancelled)
    }
}

impl From<NetError> for AppError {
    fn from(e: NetError) -> Self {
        match e {
            NetError::Auth { host, status } => AppError::Auth(format!("{host} (HTTP {status})")),
            NetError::RateLimited => AppError::RateLimited,
            NetError::Timeout => AppError::Network("request timed out".into()),
            NetError::Connect(m) => AppError::Network(m),
            NetError::Cancelled => AppError::Cancelled,
            other => AppError::Remote(other.to_string()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NetConfig {
    pub timeout: Duration,
    pub max_retries: u32,
    /// First backoff delay; doubles at each retry.
    pub retry_base: Duration,
}

impl Default for NetConfig {
    fn default() -> Self {
        NetConfig {
            timeout: Duration::from_secs(30),
            max_retries: 3,
            retry_base: Duration::from_millis(500),
        }
    }
}

pub struct HttpClient {
    client: reqwest::Client,
    key: Option<ApiKey>,
    allowlist: Arc<HostAllowlist>,
    cfg: NetConfig,
}

impl HttpClient {
    pub fn new(
        cfg: NetConfig,
        key: Option<ApiKey>,
        allowlist: HostAllowlist,
    ) -> Result<Self, NetError> {
        let allowlist = Arc::new(allowlist);
        let policy_list = allowlist.clone();
        // reqwest only strips standard sensitive headers on cross-host redirects,
        // not `x-api-key`, so every redirect target must be on the allowlist.
        let policy = redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if !policy_list.allows(attempt.url()) {
                let host = attempt.url().host_str().unwrap_or("?").to_string();
                attempt.error(format!("redirect to a host that is not allowed: {host}"))
            } else {
                attempt.follow()
            }
        });
        let client = reqwest::Client::builder()
            .user_agent(concat!("wowaddons/", env!("CARGO_PKG_VERSION")))
            .timeout(cfg.timeout)
            .connect_timeout(cfg.timeout)
            .redirect(policy)
            .build()
            .map_err(|e| NetError::Connect(e.to_string()))?;
        Ok(HttpClient {
            client,
            key,
            allowlist,
            cfg,
        })
    }

    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    fn authorize(&self, url: &Url, req: RequestBuilder) -> Result<RequestBuilder, NetError> {
        if !self.allowlist.allows(url) {
            return Err(NetError::HostNotAllowed(
                url.host_str().unwrap_or("?").to_string(),
            ));
        }
        Ok(match &self.key {
            Some(k) => req.header("x-api-key", k.expose()),
            None => req,
        })
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let base = self.cfg.retry_base.saturating_mul(1 << attempt.min(10));
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let jitter_ms =
            (nanos % 1000) as u64 * (self.cfg.retry_base.as_millis() as u64).min(250) / 1000;
        base + Duration::from_millis(jitter_ms)
    }

    /// Send with retries on transient errors (timeouts, connection errors, 429, 5xx).
    async fn send(
        &self,
        url: &Url,
        make: impl Fn() -> RequestBuilder,
    ) -> Result<Response, NetError> {
        let mut attempt = 0u32;
        loop {
            if cancel::is_cancelled() {
                return Err(NetError::Cancelled);
            }
            let req = self.authorize(url, make())?;
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(resp);
                    }
                    let transient =
                        status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                    if transient && attempt < self.cfg.max_retries {
                        let delay = retry_after(&resp).unwrap_or_else(|| self.backoff(attempt));
                        tracing::debug!(%status, ?delay, "retrying");
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(status_error(status, resp.url()));
                }
                Err(e) if e.is_redirect() => {
                    return Err(NetError::HostNotAllowed(error_chain(&e)));
                }
                Err(e) => {
                    let transient = e.is_timeout() || e.is_connect();
                    if transient && attempt < self.cfg.max_retries {
                        tokio::time::sleep(self.backoff(attempt)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(if e.is_timeout() {
                        NetError::Timeout
                    } else {
                        NetError::Connect(error_chain(&e))
                    });
                }
            }
        }
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T, NetError> {
        let resp = self.send(&url, || self.client.get(url.clone())).await?;
        decode(resp).await
    }

    pub async fn post_json<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        url: Url,
        body: &B,
    ) -> Result<T, NetError> {
        let resp = self
            .send(&url, || self.client.post(url.clone()).json(body))
            .await?;
        decode(resp).await
    }

    /// Stream a file to `dest`. Returns its size and lowercase hex SHA-1.
    pub async fn download(
        &self,
        url: Url,
        dest: &Path,
        max_bytes: u64,
        progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<(u64, String), NetError> {
        let limit_mb = max_bytes / (1024 * 1024);
        let resp = self.send(&url, || self.client.get(url.clone())).await?;
        if resp.content_length().is_some_and(|len| len > max_bytes) {
            return Err(NetError::TooLarge { limit_mb });
        }
        let mut file = tokio::fs::File::create(dest)
            .await
            .map_err(|e| NetError::Io(format!("{}: {e}", dest.display())))?;
        let mut hasher = Sha1::new();
        let mut total = 0u64;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if cancel::is_cancelled() {
                return Err(NetError::Cancelled);
            }
            let chunk = chunk.map_err(|e| {
                if e.is_timeout() {
                    NetError::Timeout
                } else {
                    NetError::Connect(error_chain(&e))
                }
            })?;
            total += chunk.len() as u64;
            if total > max_bytes {
                return Err(NetError::TooLarge { limit_mb });
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| NetError::Io(format!("{}: {e}", dest.display())))?;
            progress(chunk.len() as u64);
        }
        file.flush()
            .await
            .and(file.sync_all().await)
            .map_err(|e| NetError::Io(format!("{}: {e}", dest.display())))?;
        Ok((total, hex::encode(hasher.finalize())))
    }
}

async fn decode<T: serde::de::DeserializeOwned>(resp: Response) -> Result<T, NetError> {
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| NetError::Connect(error_chain(&e)))?;
    serde_json::from_slice(&bytes).map_err(|e| NetError::InvalidResponse(e.to_string()))
}

fn status_error(status: StatusCode, url: &Url) -> NetError {
    let host = url.host_str().unwrap_or("?").to_string();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => NetError::Auth {
            status: status.as_u16(),
            host,
        },
        StatusCode::NOT_FOUND => NetError::NotFound(url.path().to_string()),
        StatusCode::TOO_MANY_REQUESTS => NetError::RateLimited,
        _ => NetError::Http {
            status: status.as_u16(),
            url: format!("{host}{}", url.path()),
        },
    }
}

fn retry_after(resp: &Response) -> Option<Duration> {
    let secs: u64 = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// Error message with its causes. URLs never contain the key, so this is safe to show.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut msg = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        let text = s.to_string();
        if !msg.contains(&text) {
            msg.push_str(": ");
            msg.push_str(&text);
        }
        source = s.source();
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_masked() {
        let k = ApiKey::new("secret-value").unwrap();
        assert_eq!(format!("{k:?}"), "ApiKey(***)");
        assert_eq!(format!("{k}"), "***");
        assert!(ApiKey::new("  ").is_none());
    }

    #[test]
    fn allowlist() {
        let a = HostAllowlist::curseforge();
        let ok = |u: &str| a.allows(&Url::parse(u).unwrap());
        assert!(ok("https://api.curseforge.com/v1/mods/1"));
        assert!(ok("https://edge.forgecdn.net/files/1/2/a.zip"));
        assert!(ok("https://mediafilez.forgecdn.net/files/a.zip"));
        assert!(!ok("http://api.curseforge.com/v1"));
        assert!(!ok("https://evil.example.com/"));
        assert!(!ok("https://forgecdn.net.evil.com/"));
        assert!(!ok("http://127.0.0.1:1234/"));
        let a = a.with_host("127.0.0.1");
        assert!(a.allows(&Url::parse("http://127.0.0.1:1234/x").unwrap()));
    }
}
