use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tracing::{info, warn};

const COOKIE_NAME: &str = "mkvo_session";
const PASSWORD_FILE: &str = "admin-password.txt";
const PASSWORD_HASH_FILE: &str = "admin-password.hash";
const MIN_PASSWORD_LENGTH: usize = 12;
const FREE_LOGIN_FAILURES: u32 = 5;

pub fn public_router(auth: AuthState) -> Router {
    Router::new()
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .layer(axum::middleware::from_fn(no_store))
        .with_state(auth)
}

pub fn protected_router(auth: AuthState) -> Router {
    Router::new()
        .route("/api/auth/activity", post(auth_activity))
        .route(
            "/api/security/settings",
            get(security_settings).put(update_security_settings),
        )
        .layer(axum::middleware::from_fn(no_store))
        .with_state(auth)
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Serialize)]
struct AuthStatus {
    username: String,
    authenticated: bool,
    password_required: bool,
    idle_timeout_minutes: Option<u32>,
}

fn status(auth: &AuthState, peer: Option<SocketAddr>, authenticated: bool) -> AuthStatus {
    let password_required = auth.password_required(peer);
    AuthStatus {
        username: auth.username().to_owned(),
        authenticated,
        password_required,
        idle_timeout_minutes: if password_required {
            auth.security_settings().idle_timeout_minutes
        } else {
            None
        },
    }
}

fn peer(request: &Request) -> Option<SocketAddr> {
    direct_peer(
        request.headers(),
        request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0),
    )
}

/// The TCP peer, unless a proxy relayed the request: then it is the proxy, not the visitor.
fn direct_peer(headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<SocketAddr> {
    let forwarded = headers.keys().any(|name| {
        let name = name.as_str();
        name == "forwarded" || name == "x-real-ip" || name.starts_with("x-forwarded-")
    });
    peer.filter(|_| !forwarded)
}

async fn auth_status(State(auth): State<AuthState>, request: Request) -> Json<AuthStatus> {
    Json(status(
        &auth,
        peer(&request),
        !auth.password_required(peer(&request)) || auth.authenticated(request.headers()),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: String,
}

fn error(code: axum::http::StatusCode, detail: &str) -> Response {
    (code, Json(serde_json::json!({ "detail": detail }))).into_response()
}

async fn auth_login(
    State(auth): State<AuthState>,
    peer: Option<axum::Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    input: Result<Json<LoginRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(input)) = input else {
        return error(
            axum::http::StatusCode::BAD_REQUEST,
            "Username and password are required.",
        );
    };
    // Held through the check so concurrent guesses cannot slip past the lock.
    let mut throttle = auth.throttle.lock().await;
    if let Some(wait) = throttle.retry_after(Instant::now()) {
        let mut response = error(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "Too many failed sign-in attempts. Try again in a few minutes.",
        );
        response.headers_mut().insert(
            header::RETRY_AFTER,
            (wait.as_secs() + u64::from(wait.subsec_nanos() > 0)).into(),
        );
        return response;
    }
    let Some((_, cookie)) = auth.login(&input.username, &input.password) else {
        throttle.fail(Instant::now());
        return error(
            axum::http::StatusCode::UNAUTHORIZED,
            "Incorrect username or password.",
        );
    };
    *throttle = Throttle::default();
    drop(throttle);
    (
        [(header::SET_COOKIE, cookie)],
        Json(status(
            &auth,
            direct_peer(&headers, peer.map(|info| info.0.0)),
            true,
        )),
    )
        .into_response()
}

async fn auth_logout(State(auth): State<AuthState>, request: Request) -> Response {
    (
        [(header::SET_COOKIE, auth.logout(request.headers()))],
        Json(status(&auth, peer(&request), false)),
    )
        .into_response()
}

pub async fn require_auth(State(auth): State<AuthState>, request: Request, next: Next) -> Response {
    if !auth.password_required(peer(&request)) || auth.authenticated(request.headers()) {
        next.run(request).await
    } else {
        unauthorized()
    }
}

async fn auth_activity(State(auth): State<AuthState>, request: Request) -> Response {
    if !auth.password_required(peer(&request)) || auth.activity(request.headers()) {
        axum::http::StatusCode::NO_CONTENT.into_response()
    } else {
        unauthorized()
    }
}

async fn security_settings(State(auth): State<AuthState>) -> Json<SecuritySettings> {
    Json(auth.security_settings())
}

async fn update_security_settings(
    State(auth): State<AuthState>,
    input: Result<Json<SecuritySettings>, JsonRejection>,
) -> Response {
    let Ok(Json(settings)) = input else {
        return error(
            axum::http::StatusCode::BAD_REQUEST,
            "Invalid security settings.",
        );
    };
    if let Err(detail) = settings.validate() {
        return error(axum::http::StatusCode::BAD_REQUEST, detail);
    }
    if let Err(cause) = auth.set_security_settings(settings.clone()) {
        tracing::error!(%cause, "could not save security settings");
        return error(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save security settings.",
        );
    }
    Json(settings).into_response()
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecuritySettings {
    pub idle_timeout_minutes: Option<u32>,
    pub local_network_bypass: bool,
}

impl SecuritySettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .idle_timeout_minutes
            .is_some_and(|minutes| !(1..=1440).contains(&minutes))
        {
            return Err("Inactivity timeout must be between 1 and 1440 minutes.");
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct AuthState {
    auth_enabled: bool,
    username: Arc<str>,
    credential: Option<Arc<RwLock<Credential>>>,
    data_dir: Option<PathBuf>,
    throttle: Arc<tokio::sync::Mutex<Throttle>>,
    sessions: Arc<RwLock<HashMap<String, Instant>>>,
    secure_cookie: bool,
    settings: Arc<RwLock<SecuritySettings>>,
    settings_path: Option<PathBuf>,
}

impl AuthState {
    pub fn load(
        data_dir: &Path,
        configured_password: Option<&str>,
        username: &str,
        secure_cookie: bool,
    ) -> std::io::Result<Self> {
        if username.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Username cannot be empty",
            ));
        }
        std::fs::create_dir_all(data_dir)?;
        let settings_path = data_dir.join("security-settings.json");
        let settings: SecuritySettings = match std::fs::read(&settings_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                SecuritySettings::default()
            }
            Err(error) => return Err(error),
        };
        settings.validate().map_err(std::io::Error::other)?;
        let text_path = data_dir.join(PASSWORD_FILE);
        let hash_path = data_dir.join(PASSWORD_HASH_FILE);
        let credential = if let Some(password) = configured_password {
            if password.trim().is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "MKVO_PASSWORD cannot be empty",
                ));
            }
            Credential::Configured(password.to_owned())
        } else if text_path.exists() {
            let password = std::fs::read_to_string(&text_path)?.trim().to_owned();
            if password.is_empty() {
                return Err(std::io::Error::other(
                    "Administrator password file is empty",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&text_path, std::fs::Permissions::from_mode(0o600))?;
            }
            Credential::Generated(password)
        } else if hash_path.exists() {
            let hash = std::fs::read_to_string(&hash_path)?.trim().to_owned();
            PasswordHash::new(&hash).map_err(|_| {
                std::io::Error::other(format!(
                    "{} is not a password hash; delete it and restart to generate a new password",
                    hash_path.display()
                ))
            })?;
            Credential::Hashed(hash)
        } else {
            let password = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            write_private(&text_path, &format!("{password}\n"))?;
            warn!(path = %text_path.display(), "Generated the MKVO administrator password; retrieve it from this file");
            Credential::Generated(password)
        };
        if let Credential::Configured(password) | Credential::Generated(password) = &credential
            && password.chars().count() < MIN_PASSWORD_LENGTH
        {
            warn!(
                "The administrator password is shorter than {MIN_PASSWORD_LENGTH} characters; set a longer MKVO_PASSWORD"
            );
        }
        Ok(Self {
            auth_enabled: true,
            username: username.trim().into(),
            credential: Some(Arc::new(RwLock::new(credential))),
            data_dir: Some(data_dir.to_owned()),
            throttle: Arc::default(),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            secure_cookie,
            settings: Arc::new(RwLock::new(settings)),
            settings_path: Some(settings_path),
        })
    }

    #[cfg(test)]
    pub fn for_tests(password: &str) -> Self {
        Self {
            auth_enabled: true,
            username: "admin".into(),
            credential: (!password.is_empty())
                .then(|| Arc::new(RwLock::new(Credential::Configured(password.to_owned())))),
            data_dir: None,
            throttle: Arc::default(),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            secure_cookie: false,
            settings: Arc::new(RwLock::new(SecuritySettings::default())),
            settings_path: None,
        }
    }

    pub fn authenticated(&self, headers: &HeaderMap) -> bool {
        if !self.auth_enabled || self.credential.is_none() {
            return true;
        }
        let Some(token) = cookie(headers, COOKIE_NAME) else {
            return false;
        };
        let timeout = self.security_settings().idle_timeout_minutes;
        let Ok(mut sessions) = self.sessions.write() else {
            return false;
        };
        sessions.retain(|_, last_activity| !expired(*last_activity, timeout));
        sessions.contains_key(token)
    }

    pub fn security_settings(&self) -> SecuritySettings {
        self.settings
            .read()
            .expect("security settings lock poisoned")
            .clone()
    }

    pub fn with_authentication(mut self, enabled: bool) -> Self {
        self.auth_enabled = enabled;
        if !enabled {
            warn!(
                "Administrator login is disabled: all connections, including reverse proxies, have full access"
            );
        } else if let (Some(credential), Some(directory)) = (&self.credential, &self.data_dir) {
            match &*credential.read().expect("credential lock poisoned") {
                Credential::Configured(_) => info!(
                    username = %self.username,
                    "Administrator login is required: sign in with MKVO_PASSWORD"
                ),
                Credential::Generated(_) => warn!(
                    username = %self.username,
                    "Administrator login is required: sign in with the password in {} (or set MKVO_PASSWORD); after the first sign-in only its hash is kept",
                    directory.join(PASSWORD_FILE).display()
                ),
                Credential::Hashed(_) => info!(
                    username = %self.username,
                    "Administrator login is required: sign in with the password you used before, or set MKVO_PASSWORD; to generate a new one delete {} and restart",
                    directory.join(PASSWORD_HASH_FILE).display()
                ),
            }
        }
        self
    }

    pub fn password_required(&self, peer: Option<SocketAddr>) -> bool {
        self.auth_enabled && !self.local_bypass(peer)
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn set_security_settings(&self, settings: SecuritySettings) -> std::io::Result<()> {
        settings.validate().map_err(std::io::Error::other)?;
        let mut current = self
            .settings
            .write()
            .map_err(|_| std::io::Error::other("security settings lock poisoned"))?;
        if let Some(path) = &self.settings_path {
            let temp = path.with_extension("tmp");
            std::fs::write(&temp, serde_json::to_vec_pretty(&settings)?)?;
            std::fs::rename(&temp, path)?;
        }
        *current = settings;
        Ok(())
    }

    // Callers pass the direct TCP peer only; forwarded requests arrive as `None`.
    pub fn local_bypass(&self, peer: Option<SocketAddr>) -> bool {
        self.security_settings().local_network_bypass
            && peer.is_some_and(|peer| is_local(peer.ip()))
    }

    pub fn activity(&self, headers: &HeaderMap) -> bool {
        let timeout = self.security_settings().idle_timeout_minutes;
        let Some(token) = cookie(headers, COOKIE_NAME) else {
            return false;
        };
        let Ok(mut sessions) = self.sessions.write() else {
            return false;
        };
        sessions.retain(|_, last_activity| !expired(*last_activity, timeout));
        if let Some(last_activity) = sessions.get_mut(token) {
            *last_activity = Instant::now();
            return true;
        }
        false
    }

    pub fn login(&self, username: &str, candidate: &str) -> Option<(String, HeaderValue)> {
        let credential = self.credential.as_ref()?;
        let current = credential.read().ok()?.clone();
        let password_match = match &current {
            Credential::Configured(password) | Credential::Generated(password) => {
                bool::from(password.as_bytes().ct_eq(candidate.as_bytes()))
            }
            Credential::Hashed(hash) => PasswordHash::new(hash).is_ok_and(|hash| {
                Argon2::default()
                    .verify_password(candidate.as_bytes(), &hash)
                    .is_ok()
            }),
        };
        if !bool::from(self.username.as_bytes().ct_eq(username.as_bytes())) || !password_match {
            return None;
        }
        if let Credential::Generated(password) = &current {
            match self.hash_generated_password(password) {
                Ok(hash) => *credential.write().ok()? = Credential::Hashed(hash),
                Err(cause) => {
                    tracing::error!(%cause, "could not replace the password file with a hash")
                }
            }
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        let timeout = self.security_settings().idle_timeout_minutes;
        let mut sessions = self.sessions.write().ok()?;
        sessions.retain(|_, last_activity| !expired(*last_activity, timeout));
        sessions.insert(token.clone(), Instant::now());
        let secure = if self.secure_cookie { "; Secure" } else { "" };
        let value = HeaderValue::from_str(&format!(
            "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict{secure}"
        ))
        .ok()?;
        Some((token, value))
    }

    /// Swaps the readable password file for its hash once the admin has used it.
    fn hash_generated_password(&self, password: &str) -> std::io::Result<String> {
        let directory = self
            .data_dir
            .as_ref()
            .ok_or_else(|| std::io::Error::other("no data directory"))?;
        let hash_error =
            |error: argon2::password_hash::Error| std::io::Error::other(error.to_string());
        let salt = SaltString::encode_b64(uuid::Uuid::new_v4().as_bytes()).map_err(hash_error)?;
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(hash_error)?
            .to_string();
        let path = directory.join(PASSWORD_HASH_FILE);
        let temp = path.with_extension("tmp");
        write_private(&temp, &hash)?;
        std::fs::rename(&temp, &path)?;
        std::fs::remove_file(directory.join(PASSWORD_FILE))?;
        Ok(hash)
    }

    pub fn logout(&self, headers: &HeaderMap) -> HeaderValue {
        if let Some(token) = cookie(headers, COOKIE_NAME)
            && let Ok(mut sessions) = self.sessions.write()
        {
            sessions.remove(token);
        }
        HeaderValue::from_static("mkvo_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
    }
}

#[derive(Clone)]
enum Credential {
    /// `MKVO_PASSWORD`, kept in memory only.
    Configured(String),
    /// `admin-password.txt`, readable until the first sign-in hashes it.
    Generated(String),
    /// Argon2 hash from `admin-password.hash`.
    Hashed(String),
}

/// One lock for every client, so rotating addresses gains nothing.
#[derive(Default)]
struct Throttle {
    failures: u32,
    last_failure: Option<Instant>,
    locked_until: Option<Instant>,
}

impl Throttle {
    fn retry_after(&self, now: Instant) -> Option<Duration> {
        self.locked_until
            .map(|until| until.saturating_duration_since(now))
            .filter(|wait| !wait.is_zero())
    }

    fn fail(&mut self, now: Instant) {
        if self
            .last_failure
            .is_some_and(|last| now.saturating_duration_since(last) > Duration::from_secs(3600))
        {
            self.failures = 0;
        }
        self.failures += 1;
        self.last_failure = Some(now);
        if let Some(over) = self.failures.checked_sub(FREE_LOGIN_FAILURES) {
            let wait = Duration::from_secs(30) * 2u32.pow(over.min(5));
            self.locked_until = Some(now + wait.min(Duration::from_secs(900)));
        }
    }
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn expired(last_activity: Instant, timeout: Option<u32>) -> bool {
    timeout.is_some_and(|minutes| {
        last_activity.elapsed() >= Duration::from_secs(u64::from(minutes) * 60)
    })
}

fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(|ip| is_local(IpAddr::V4(ip)))
            .unwrap_or_else(|| {
                ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
            }),
    }
}

pub fn unauthorized() -> Response {
    (
        axum::http::StatusCode::UNAUTHORIZED,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        r#"{"detail":"Authentication required."}"#,
    )
        .into_response()
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabling_login_preserves_credentials_and_sessions_do_not_survive_restart() {
        let directory = tempfile::tempdir().unwrap();
        let auth = AuthState::load(directory.path(), Some("password"), "admin", true).unwrap();
        let (_, cookie) = auth.login("admin", "password").unwrap();
        assert!(cookie.to_str().unwrap().ends_with("; Secure"));
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie);
        let disabled = auth.clone().with_authentication(false);
        assert!(disabled.authenticated(&HeaderMap::new()));
        assert!(!disabled.password_required(None));
        let enabled = disabled.with_authentication(true);
        assert!(enabled.login("admin", "password").is_some());
        let restarted = AuthState::load(directory.path(), Some("password"), "admin", true).unwrap();
        assert!(!restarted.authenticated(&headers));
    }

    #[test]
    fn failed_settings_write_does_not_change_effective_policy() {
        let directory = tempfile::tempdir().unwrap();
        let auth = AuthState::load(directory.path(), Some("password"), "admin", false).unwrap();
        std::fs::create_dir(directory.path().join("security-settings.tmp")).unwrap();
        assert!(
            auth.set_security_settings(SecuritySettings {
                idle_timeout_minutes: Some(30),
                local_network_bypass: true,
            })
            .is_err()
        );
        assert!(!auth.security_settings().local_network_bypass);
        assert_eq!(auth.security_settings().idle_timeout_minutes, None);
    }

    #[test]
    fn login_requires_the_configured_username_and_password() {
        let directory = tempfile::tempdir().unwrap();
        let auth = AuthState::load(directory.path(), Some("password"), "curator", false).unwrap();
        assert!(auth.login("curator", "password").is_some());
        assert!(auth.login("admin", "password").is_none());
        assert!(auth.login("CURATOR", "password").is_none());
        assert!(auth.login("curator", "wrong").is_none());
        assert!(auth.login("", "password").is_none());
        assert!(AuthState::load(directory.path(), Some("password"), " ", false).is_err());
    }

    #[test]
    fn local_bypass_is_opt_in_and_uses_only_local_peers() {
        let auth = AuthState::for_tests("password");
        let lan = Some("192.168.1.5:1234".parse().unwrap());
        assert!(!auth.local_bypass(lan));
        auth.set_security_settings(SecuritySettings {
            local_network_bypass: true,
            idle_timeout_minutes: None,
        })
        .unwrap();
        for address in [
            "192.168.1.5:1234",
            "10.0.0.2:80",
            "172.18.0.5:80",
            "127.0.0.1:80",
            "[::1]:80",
            "[fd00::1]:80",
            "[::ffff:192.168.1.5]:80",
        ] {
            assert!(
                auth.local_bypass(Some(address.parse().unwrap())),
                "{address}"
            );
        }
        for address in ["8.8.8.8:80", "100.64.0.2:80", "[2606:4700::1111]:80"] {
            assert!(
                !auth.local_bypass(Some(address.parse().unwrap())),
                "{address}"
            );
        }
        assert!(!auth.local_bypass(None));
    }

    #[test]
    fn inactivity_expires_sessions_but_background_checks_do_not_extend_them() {
        let auth = AuthState::for_tests("password");
        auth.set_security_settings(SecuritySettings {
            idle_timeout_minutes: Some(1),
            local_network_bypass: false,
        })
        .unwrap();
        let (token, value) = auth.login("admin", "password").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, value);
        let old = Instant::now() - Duration::from_secs(30);
        auth.sessions.write().unwrap().insert(token.clone(), old);
        assert!(auth.authenticated(&headers));
        assert_eq!(auth.sessions.read().unwrap()[&token], old);
        assert!(auth.activity(&headers));
        assert!(auth.sessions.read().unwrap()[&token] > old);
        auth.sessions
            .write()
            .unwrap()
            .insert(token.clone(), Instant::now() - Duration::from_secs(61));
        assert!(!auth.activity(&headers));
        assert!(!auth.authenticated(&headers));
        assert!(!auth.sessions.read().unwrap().contains_key(&token));
    }

    #[test]
    fn disabled_timeout_keeps_sessions_until_logout() {
        let auth = AuthState::for_tests("password");
        let (token, value) = auth.login("admin", "password").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, value);
        auth.sessions.write().unwrap().insert(
            token,
            Instant::now()
                .checked_sub(Duration::from_secs(86400))
                .unwrap_or_else(Instant::now),
        );
        assert!(auth.authenticated(&headers));
        auth.logout(&headers);
        assert!(!auth.authenticated(&headers));
    }

    #[test]
    fn security_settings_survive_restarts_and_reject_invalid_values() {
        let directory = tempfile::tempdir().unwrap();
        let auth = AuthState::load(directory.path(), Some("password"), "admin", false).unwrap();
        auth.set_security_settings(SecuritySettings {
            idle_timeout_minutes: Some(20),
            local_network_bypass: true,
        })
        .unwrap();
        let reloaded = AuthState::load(directory.path(), Some("password"), "admin", false).unwrap();
        assert_eq!(reloaded.security_settings().idle_timeout_minutes, Some(20));
        assert!(reloaded.security_settings().local_network_bypass);
        for minutes in [0, 1441] {
            assert!(
                reloaded
                    .set_security_settings(SecuritySettings {
                        idle_timeout_minutes: Some(minutes),
                        local_network_bypass: false
                    })
                    .is_err()
            );
        }
        reloaded
            .set_security_settings(SecuritySettings::default())
            .unwrap();
        let reset = AuthState::load(directory.path(), Some("password"), "admin", false).unwrap();
        assert!(!reset.local_bypass(Some("192.168.1.5:80".parse().unwrap())));
        assert!(reset.login("admin", "password").is_some());
    }

    #[test]
    fn first_run_password_is_persisted_and_reused() {
        let directory = tempfile::tempdir().unwrap();
        let first = AuthState::load(directory.path(), None, "admin", false).unwrap();
        let password =
            std::fs::read_to_string(directory.path().join("admin-password.txt")).unwrap();
        assert!(password.trim().len() >= 32);
        assert!(first.login("admin", password.trim()).is_some());

        let second = AuthState::load(directory.path(), None, "admin", false).unwrap();
        assert!(second.login("admin", password.trim()).is_some());
    }

    #[test]
    fn stored_password_is_hashed_after_the_first_sign_in() {
        let directory = tempfile::tempdir().unwrap();
        let text = directory.path().join("admin-password.txt");
        let hash = directory.path().join("admin-password.hash");
        // An existing install may hold a short password someone typed into the file.
        std::fs::write(&text, "short\n").unwrap();
        let auth = AuthState::load(directory.path(), None, "admin", false).unwrap();
        assert!(auth.login("admin", "wrong").is_none());
        assert!(text.exists() && !hash.exists());
        assert!(auth.login("admin", "short").is_some());
        assert!(!text.exists());
        let stored = std::fs::read_to_string(&hash).unwrap();
        assert!(stored.starts_with("$argon2"), "{stored}");
        assert!(!stored.contains("short"));
        assert!(auth.login("admin", "short").is_some());

        let restarted = AuthState::load(directory.path(), None, "admin", false).unwrap();
        assert!(restarted.login("admin", "short").is_some());
        assert!(restarted.login("admin", "wrong").is_none());
        assert!(restarted.login("other", "short").is_none());
        // MKVO_PASSWORD still wins and is never written to disk.
        let configured =
            AuthState::load(directory.path(), Some("configured-pass"), "admin", false).unwrap();
        assert!(configured.login("admin", "configured-pass").is_some());
        assert!(configured.login("admin", "short").is_none());
        assert_eq!(std::fs::read_to_string(&hash).unwrap(), stored);
    }

    #[test]
    fn repeated_failures_lock_sign_in_with_a_growing_delay() {
        let start = Instant::now();
        let mut throttle = Throttle::default();
        for _ in 0..4 {
            throttle.fail(start);
            assert_eq!(throttle.retry_after(start), None);
        }
        throttle.fail(start);
        assert_eq!(throttle.retry_after(start), Some(Duration::from_secs(30)));
        let later = start + Duration::from_secs(31);
        assert_eq!(throttle.retry_after(later), None);
        throttle.fail(later);
        assert_eq!(throttle.retry_after(later), Some(Duration::from_secs(60)));
        for _ in 0..10 {
            throttle.fail(later);
        }
        assert_eq!(throttle.retry_after(later), Some(Duration::from_secs(900)));
        // A failure long after the last one starts counting again.
        let next_day = later + Duration::from_secs(86_400);
        throttle.fail(next_day);
        assert_eq!(throttle.retry_after(next_day), None);
    }
}
