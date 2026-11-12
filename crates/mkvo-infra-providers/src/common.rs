use std::{fmt, future::Future, time::Duration};

use async_trait::async_trait;
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{sync::Mutex, time::Instant};
use tokio_util::sync::CancellationToken;

pub(crate) fn provider_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        // Provider requests can contain bearer tokens or API keys. Following a
        // redirect could forward them to a host the application did not choose.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("provider HTTP client configuration must be valid")
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

#[derive(Debug, Clone)]
pub struct ProviderCredentials {
    pub api_key: SecretString,
    pub pin: Option<SecretString>,
}

impl ProviderCredentials {
    pub fn api_key(api_key: impl Into<String>) -> Self {
        Self {
            api_key: SecretString::new(api_key),
            pin: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Tvdb,
    Tmdb,
    AniDb,
    AniList,
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Tvdb => "TVDB",
            Self::Tmdb => "TMDB",
            Self::AniDb => "AniDB",
            Self::AniList => "AniList",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Series,
    Movie,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub provider: ProviderKind,
    pub id: u64,
    pub kind: MediaKind,
    pub name: String,
    pub year: Option<u16>,
    pub overview: Option<String>,
    pub database_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedMedia {
    pub id: u64,
    pub kind: MediaKind,
    pub name: String,
}

impl From<&SearchResult> for SelectedMedia {
    fn from(value: &SearchResult) -> Self {
        Self {
            id: value.id,
            kind: value.kind,
            name: value.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeMetadata {
    pub provider: ProviderKind,
    pub id: u64,
    pub season_number: u32,
    pub episode_number: u32,
    pub absolute_number: Option<u32>,
    pub name: String,
    pub scope_name: String,
    pub air_date: Option<String>,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("{provider} credentials are not configured")]
    MissingCredentials { provider: ProviderKind },
    #[error("{provider} request could not be sent: {message}")]
    Network {
        provider: ProviderKind,
        message: String,
    },
    #[error("{provider} returned HTTP {status}")]
    Status {
        provider: ProviderKind,
        status: StatusCode,
    },
    #[error("{provider} returned an invalid response: {message}")]
    InvalidResponse {
        provider: ProviderKind,
        message: String,
    },
    #[error("{provider} authentication failed")]
    Authentication { provider: ProviderKind },
    #[error("{provider} request was cancelled")]
    Cancelled { provider: ProviderKind },
}

impl ProviderError {
    pub(crate) fn network(provider: ProviderKind, error: reqwest::Error) -> Self {
        // `without_url` prevents query credentials from entering logs or crash reports.
        Self::Network {
            provider,
            message: error.without_url().to_string(),
        }
    }

    pub(crate) fn invalid(provider: ProviderKind, error: impl fmt::Display) -> Self {
        Self::InvalidResponse {
            provider,
            message: error.to_string(),
        }
    }
}

#[async_trait]
pub trait MetadataProviderClient: Send + Sync {
    fn provider(&self) -> ProviderKind;

    async fn search(
        &self,
        credentials: &ProviderCredentials,
        query: &str,
        language: &str,
        cancellation: CancellationToken,
    ) -> Result<Vec<SearchResult>, ProviderError>;

    async fn episodes(
        &self,
        credentials: &ProviderCredentials,
        selected: &SelectedMedia,
        language: &str,
        cancellation: CancellationToken,
    ) -> Result<Vec<EpisodeMetadata>, ProviderError>;
}

/// Spaces requests at least `interval` apart across every clone of a client.
#[derive(Debug)]
pub(crate) struct RateLimiter {
    interval: Duration,
    next: Mutex<Instant>,
}

impl RateLimiter {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            next: Mutex::new(Instant::now()),
        }
    }

    async fn acquire(&self) {
        let slot = {
            let mut next = self.next.lock().await;
            let slot = (*next).max(Instant::now());
            *next = slot + self.interval;
            slot
        };
        tokio::time::sleep_until(slot).await;
    }
}

pub(crate) async fn send_with_retry<F>(
    provider: ProviderKind,
    cancellation: &CancellationToken,
    build: F,
) -> Result<Response, ProviderError>
where
    F: FnMut() -> RequestBuilder,
{
    send_rate_limited(provider, cancellation, None, build).await
}

/// `send_with_retry`, with every attempt waiting for its turn in `limiter`.
pub(crate) async fn send_rate_limited<F>(
    provider: ProviderKind,
    cancellation: &CancellationToken,
    limiter: Option<&RateLimiter>,
    mut build: F,
) -> Result<Response, ProviderError>
where
    F: FnMut() -> RequestBuilder,
{
    const MAX_ATTEMPTS: u32 = 3;
    for attempt in 0..MAX_ATTEMPTS {
        let last_attempt = attempt + 1 == MAX_ATTEMPTS;
        if let Some(limiter) = limiter {
            wait_or_cancel(provider, cancellation, limiter.acquire()).await?;
        }
        let send = build().send();
        let result = tokio::select! {
            () = cancellation.cancelled() => return Err(ProviderError::Cancelled { provider }),
            result = send => result,
        };
        let retry_after = match result {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let retryable = response.status() == StatusCode::TOO_MANY_REQUESTS
                    || response.status().is_server_error();
                if !retryable || last_attempt {
                    return Err(ProviderError::Status {
                        provider,
                        status: response.status(),
                    });
                }
                response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<u64>().ok())
            }
            // A timeout already cost the caller its full wait, so only failures
            // that return quickly (refused, reset) are sent again.
            Err(error) if last_attempt || error.is_timeout() || error.is_builder() => {
                return Err(ProviderError::network(provider, error));
            }
            Err(_) => None,
        };
        let delay = retry_delay(retry_after, attempt);
        wait_or_cancel(provider, cancellation, tokio::time::sleep(delay)).await?;
    }
    unreachable!("retry loop always returns")
}

/// The server's `Retry-After` when it sends one, else exponential backoff.
fn retry_delay(retry_after_secs: Option<u64>, attempt: u32) -> Duration {
    retry_after_secs
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_millis(250 * 2_u64.pow(attempt)))
        .min(Duration::from_secs(60))
}

async fn wait_or_cancel<F>(
    provider: ProviderKind,
    cancellation: &CancellationToken,
    wait: F,
) -> Result<(), ProviderError>
where
    F: Future<Output = ()>,
{
    tokio::pin!(wait);
    tokio::select! {
        () = cancellation.cancelled() => Err(ProviderError::Cancelled { provider }),
        () = &mut wait => Ok(()),
    }
}

/// TMDB's `language` form: ISO 639-1, optionally followed by a country.
pub fn normalize_language(value: &str) -> String {
    let value = value.trim().replace('_', "-").to_ascii_lowercase();
    if value.len() == 5 && value.as_bytes()[2] == b'-' {
        return format!("{}-{}", &value[..2], value[3..].to_ascii_uppercase());
    }
    match crate::iso639::lookup(&value) {
        Some(("en", _)) | None => "en-US".to_owned(),
        Some(("ja", _)) => "ja-JP".to_owned(),
        Some(("es", _)) => "es-ES".to_owned(),
        Some(("fr", _)) => "fr-FR".to_owned(),
        Some(("de", _)) => "de-DE".to_owned(),
        Some((language, _)) => language.to_owned(),
    }
}

pub(crate) fn year_from_date(value: Option<&str>) -> Option<u16> {
    value?
        .split('-')
        .next()
        .and_then(|year| year.parse::<u16>().ok())
}

pub(crate) async fn response_json(
    provider: ProviderKind,
    response: Response,
) -> Result<serde_json::Value, ProviderError> {
    response
        .json()
        .await
        .map_err(|error| ProviderError::invalid(provider, error.without_url()))
}

/// Serve `router` on a loopback port and return its base URL.
#[cfg(test)]
pub(crate) async fn serve_stub(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub server");
    let address = listener.local_addr().expect("stub address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{address}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn a_dropped_connection_is_retried() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}/", listener.local_addr().expect("address"));
        tokio::spawn(async move {
            drop(listener.accept().await);
            let (mut stream, _) = listener.accept().await.expect("second connection");
            let _ = stream.read(&mut [0_u8; 1024]).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
                .await;
        });

        let client = provider_http_client();
        let response = send_with_retry(ProviderKind::Tmdb, &CancellationToken::new(), || {
            client.get(&url)
        })
        .await
        .expect("retried request");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limiter_spaces_requests_by_its_interval() {
        let limiter = RateLimiter::new(Duration::from_secs(2));
        let started = Instant::now();
        limiter.acquire().await;
        assert_eq!(started.elapsed(), Duration::ZERO);
        limiter.acquire().await;
        limiter.acquire().await;
        assert_eq!(started.elapsed(), Duration::from_secs(4));
    }

    #[test]
    fn retry_after_is_honoured_up_to_a_minute() {
        assert_eq!(retry_delay(Some(30), 0), Duration::from_secs(30));
        assert_eq!(retry_delay(Some(3600), 0), Duration::from_secs(60));
        assert_eq!(retry_delay(None, 0), Duration::from_millis(250));
        assert_eq!(retry_delay(None, 1), Duration::from_millis(500));
    }

    #[test]
    fn credentials_are_redacted_from_debug() {
        let credentials = ProviderCredentials {
            api_key: SecretString::new("highly-secret-key"),
            pin: Some(SecretString::new("secret-pin")),
        };
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("highly-secret-key"));
        assert!(!debug.contains("secret-pin"));
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn normalizes_provider_languages() {
        assert_eq!(normalize_language("eng"), "en-US");
        assert_eq!(normalize_language("ja_jp"), "ja-JP");
        assert_eq!(normalize_language("unknown"), "en-US");
    }

    #[test]
    fn languages_beyond_the_big_five_keep_their_own_code() {
        assert_eq!(normalize_language("cat"), "ca");
        assert_eq!(normalize_language("ita"), "it");
        assert_eq!(normalize_language("ko"), "ko");
        assert_eq!(normalize_language("dut"), "nl");
        assert_eq!(normalize_language("pt-br"), "pt-BR");
        assert_eq!(normalize_language("fre"), "fr-FR");
        assert_eq!(normalize_language("und"), "en-US");
    }
}
