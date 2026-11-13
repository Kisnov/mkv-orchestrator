mod auth;
mod config;
mod error;
mod request_id;
mod routes;

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use mkvo_runtime::{MkvoRuntime, MkvoRuntimeBuilder, RuntimeResult};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing::info_span;

pub use config::ServerConfig;
use request_id::CORRELATION_ID_HEADER;

#[derive(Clone)]
pub(crate) struct AppState {
    pub runtime: Arc<MkvoRuntime>,
}

pub fn build_runtime(config: &ServerConfig) -> RuntimeResult<MkvoRuntime> {
    let mut builder = MkvoRuntimeBuilder::new(&config.media_root, &config.config_dir)
        .app_name("MKV Orchestrator Web")
        .version(env!("CARGO_PKG_VERSION"));
    for (key, value) in &config.provider_secret_overrides {
        builder = builder.secret_override(key.clone(), value.clone());
    }
    for source in &config.source_roots {
        if source.path != config.media_root {
            builder = builder.source_root(source.label.clone(), &source.path);
        }
        builder = builder.authorized_root(&source.path, true);
    }
    for directory in &config.tool_directories {
        builder = builder.tool_directory(directory);
    }
    builder.build()
}

pub fn build_router(
    config: Arc<ServerConfig>,
    runtime: Arc<MkvoRuntime>,
) -> anyhow::Result<Router> {
    let auth = auth::AuthState::load(
        &config.config_dir,
        config.auth.as_ref().map(|auth| auth.password.as_str()),
        &config.auth_username,
        config.secure_cookies,
    )?
    .with_authentication(config.auth_enabled)
    .with_allowed_hosts(config.allowed_hosts.clone());
    let state = AppState { runtime };
    let protected = routes::api_router(state)
        .merge(auth::protected_router(auth.clone()))
        .layer(middleware::from_fn_with_state(
            auth.clone(),
            auth::require_auth,
        ));

    Ok(Router::new()
        .route("/api/health", axum::routing::get(routes::health))
        .merge(auth::public_router(auth.clone()))
        .merge(protected)
        .fallback_service(
            ServeDir::new(&config.ui_dir)
                .fallback(ServeFile::new(config.ui_dir.join("index.html"))),
        )
        .layer(DefaultBodyLimit::max(config.request_body_limit_bytes))
        .layer(middleware::from_fn_with_state(
            RequestTimeout(Duration::from_secs(120)),
            request_timeout,
        ))
        .layer(middleware::from_fn_with_state(auth, auth::check_origin))
        .layer(middleware::from_fn(security_headers))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request| {
                let correlation_id = request
                    .headers()
                    .get(&CORRELATION_ID_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("unknown");
                info_span!(
                    "http.request",
                    method = %request.method(),
                    uri = %request.uri(),
                    correlation_id
                )
            }),
        )
        .layer(middleware::from_fn(request_id::attach_correlation_id)))
}

// The built SPA has no inline scripts; React and Tailwind set styles inline.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'";

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "same-origin"),
        (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

#[derive(Clone, Copy)]
struct RequestTimeout(Duration);

/// Previews, applies, audits and media-server calls run inline for many minutes and
/// must not be cut halfway, so only the remaining endpoints are timed.
async fn request_timeout(
    State(RequestTimeout(limit)): State<RequestTimeout>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if [
        "/api/media-servers/",
        "/api/rename/",
        "/api/mux/",
        "/api/propedit/",
        "/api/library/",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
    {
        return next.run(request).await;
    }
    match tokio::time::timeout(limit, next.run(request)).await {
        Ok(response) => response,
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "detail": format!("The server stopped this request after {} seconds.", limit.as_secs())
            })),
        )
            .into_response(),
    }
}

pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, net::SocketAddr, sync::Arc};

    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use http_body_util::BodyExt;
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;
    use crate::config::{BasicAuth, SourceRoot};

    struct TestHost {
        _directory: TempDir,
        config: Arc<ServerConfig>,
        runtime: Arc<MkvoRuntime>,
    }

    impl TestHost {
        fn new(auth: Option<BasicAuth>) -> Self {
            Self::new_with_provider_secrets(auth, BTreeMap::new())
        }

        fn new_with_provider_secrets(
            auth: Option<BasicAuth>,
            provider_secret_overrides: BTreeMap<String, String>,
        ) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let media_root = directory.path().join("media");
            let config_dir = directory.path().join("config");
            let ui_dir = directory.path().join("ui");
            fs::create_dir_all(&media_root).unwrap();
            fs::create_dir_all(&config_dir).unwrap();
            fs::create_dir_all(&ui_dir).unwrap();
            fs::write(ui_dir.join("index.html"), "<main>MKVO test UI</main>").unwrap();
            let config = Arc::new(ServerConfig {
                bind: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
                media_root: media_root.clone(),
                source_roots: vec![SourceRoot {
                    label: "media".to_owned(),
                    path: media_root,
                }],
                config_dir,
                ui_dir,
                auth_enabled: auth.is_some(),
                auth_username: auth
                    .as_ref()
                    .map_or("admin", |auth| auth.username.as_str())
                    .to_owned(),
                secure_cookies: false,
                allowed_hosts: Vec::new(),
                tool_directories: Vec::new(),
                auth,
                provider_secret_overrides,
                request_body_limit_bytes: 1024,
                graceful_shutdown_seconds: 1,
            });
            let runtime = Arc::new(build_runtime(&config).unwrap());
            Self {
                _directory: directory,
                config,
                runtime,
            }
        }

        fn router(&self) -> Router {
            build_router(Arc::clone(&self.config), Arc::clone(&self.runtime)).unwrap()
        }
    }

    #[tokio::test]
    async fn container_provider_secrets_appear_configured_in_web_settings() {
        let host = TestHost::new_with_provider_secrets(
            None,
            BTreeMap::from([
                (
                    "provider.tvdb.api_key".to_owned(),
                    "container-tvdb-key".to_owned(),
                ),
                (
                    "provider.tvdb.pin".to_owned(),
                    "container-tvdb-pin".to_owned(),
                ),
                (
                    "provider.tmdb.api_key".to_owned(),
                    "container-tmdb-key".to_owned(),
                ),
            ]),
        );

        let settings = host.runtime.get_web_settings().await.unwrap();
        assert!(settings.has_tvdb_api_key);
        assert!(settings.has_tvdb_pin);
        assert!(settings.has_tmdb_api_key);
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn session_login_logout_and_security_http_contract() {
        let host = TestHost::new(Some(BasicAuth {
            username: "curator".to_owned(),
            password: "secret".to_owned(),
        }));
        let router = host.router();
        for path in ["/api/status", "/api/security/settings", "/api/unknown"] {
            let response = router
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
        let response = router
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = router
            .clone()
            .oneshot(
                Request::get("/api/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let status: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(status["username"], "curator");
        assert_eq!(status["authenticated"], false);
        assert_eq!(status["password_required"], true);
        assert!(status["idle_timeout_minutes"].is_null());
        assert!(status.get("password").is_none());
        for (username, password) in [
            ("admin", "secret"),
            ("CURATOR", "secret"),
            ("curator", "wrong"),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::post("/api/auth/login")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::json!({"username": username, "password": password})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                body_text(response).await,
                r#"{"detail":"Incorrect username or password."}"#
            );
        }
        let response = router
            .clone()
            .oneshot(
                Request::post("/api/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"curator","password":"secret"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let cookie = response.headers()[header::SET_COOKIE].clone();
        for invalid in [
            r#"{"idle_timeout_minutes":0,"local_network_bypass":false}"#,
            r#"{"idle_timeout_minutes":1441,"local_network_bypass":false}"#,
            r#"{"idle_timeout_minutes":1.5,"local_network_bypass":false}"#,
            r#"{"idle_timeout_minutes":"30","local_network_bypass":false}"#,
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::put("/api/security/settings")
                        .header(header::COOKIE, &cookie)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(invalid))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        let response = router
            .clone()
            .oneshot(
                Request::put("/api/security/settings")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"idle_timeout_minutes":30,"local_network_bypass":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = router
            .clone()
            .oneshot(
                Request::post("/api/auth/activity")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let response = router
            .clone()
            .oneshot(
                Request::post("/api/auth/logout")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        let response = router
            .clone()
            .oneshot(
                Request::get("/api/status")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = router
            .oneshot(
                Request::post("/api/auth/activity")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let persisted: serde_json::Value = serde_json::from_slice(
            &fs::read(host.config.config_dir.join("security-settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted["idle_timeout_minutes"], 30);
    }

    fn login_request(password: &str) -> Request<Body> {
        Request::post("/api/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({"username": "admin", "password": password}).to_string(),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn repeated_failed_sign_ins_lock_remote_clients_but_not_the_lan() {
        let host = TestHost::new(Some(BasicAuth {
            username: "admin".to_owned(),
            password: "correct-password".to_owned(),
        }));
        let router = host.router();
        for _ in 0..5 {
            let response = router
                .clone()
                .oneshot(login_request("guess"))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = router
            .clone()
            .oneshot(login_request("correct-password"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "30");
        assert!(!response.headers().contains_key(header::SET_COOKIE));

        // The owner on the LAN has a lock of their own, so outside guessing cannot keep them out.
        let from = |peer: &str, forwarded: bool| {
            let mut request = login_request("correct-password");
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                peer.parse::<SocketAddr>().unwrap(),
            ));
            if forwarded {
                request
                    .headers_mut()
                    .insert("x-forwarded-for", "192.168.1.2".parse().unwrap());
            }
            request
        };
        for (peer, forwarded, expected) in [
            ("8.8.8.8:1234", false, StatusCode::TOO_MANY_REQUESTS),
            ("192.168.1.2:1234", true, StatusCode::TOO_MANY_REQUESTS),
            ("192.168.1.2:1234", false, StatusCode::OK),
        ] {
            let response = router.clone().oneshot(from(peer, forwarded)).await.unwrap();
            assert_eq!(response.status(), expected, "{peer} {forwarded}");
        }
    }

    #[tokio::test]
    async fn disabled_login_has_no_barrier_and_no_idle_timeout() {
        let host = TestHost::new(None);
        fs::write(
            host.config.config_dir.join("security-settings.json"),
            r#"{"idle_timeout_minutes":1,"local_network_bypass":false}"#,
        )
        .unwrap();
        let router = host.router();
        let response = router
            .clone()
            .oneshot(
                Request::get("/api/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(status["authenticated"], true);
        assert_eq!(status["password_required"], false);
        assert!(status["idle_timeout_minutes"].is_null());
        let response = router
            .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn lan_bypass_needs_a_direct_local_peer_and_fails_closed_without_one() {
        let host = TestHost::new(Some(BasicAuth {
            username: "admin".to_owned(),
            password: "secret".to_owned(),
        }));
        fs::write(
            host.config.config_dir.join("security-settings.json"),
            r#"{"idle_timeout_minutes":null,"local_network_bypass":true}"#,
        )
        .unwrap();
        let router = host.router();
        // A proxy in front makes every visitor look local, so any forwarding header turns the bypass off.
        for forwarded in [
            None,
            Some(("X-Forwarded-For", "127.0.0.1")),
            Some(("X-Forwarded-Proto", "https")),
            Some(("Forwarded", "for=192.168.1.9")),
            Some(("X-Real-IP", "192.168.1.9")),
        ] {
            for peer in [
                None,
                Some("8.8.8.8:1234"),
                Some("100.64.0.1:1234"),
                Some("192.168.1.2:1234"),
            ] {
                let mut request = Request::get("/api/status").header("Host", "localhost");
                if let Some((name, value)) = forwarded {
                    request = request.header(name, value);
                }
                let mut request = request.body(Body::empty()).unwrap();
                if let Some(peer) = peer {
                    request.extensions_mut().insert(axum::extract::ConnectInfo(
                        peer.parse::<SocketAddr>().unwrap(),
                    ));
                }
                let response = router.clone().oneshot(request).await.unwrap();
                assert_eq!(
                    response.status(),
                    if forwarded.is_none() && peer == Some("192.168.1.2:1234") {
                        StatusCode::OK
                    } else {
                        StatusCode::UNAUTHORIZED
                    },
                    "{forwarded:?} {peer:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn health_is_public_and_has_legacy_contract() {
        let host = TestHost::new(Some(BasicAuth {
            username: "mkvo".to_owned(),
            password: "secret".to_owned(),
        }));
        let response = host
            .router()
            .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_text(response).await, r#"{"status":"ok"}"#);
    }

    #[tokio::test]
    async fn protects_api_and_accepts_valid_session_cookie() {
        let host = TestHost::new(Some(BasicAuth {
            username: "mkvo".to_owned(),
            password: "secret".to_owned(),
        }));
        let router = host.router();
        let unauthorized = router
            .clone()
            .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert!(
            !unauthorized
                .headers()
                .contains_key(header::WWW_AUTHENTICATE)
        );
        assert!(
            body_text(unauthorized)
                .await
                .contains("\"detail\":\"Authentication required.\"")
        );

        let login = router
            .clone()
            .oneshot(
                Request::post("/api/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"username":"mkvo","password":"secret"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let cookie = login.headers()[header::SET_COOKIE].clone();
        assert!(
            cookie
                .to_str()
                .unwrap()
                .contains("HttpOnly; SameSite=Strict")
        );
        let authorized = router
            .oneshot(
                Request::get("/api/status")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);
        let status: serde_json::Value = serde_json::from_str(&body_text(authorized).await).unwrap();
        assert_eq!(status["name"], "MKV Orchestrator Web");
        assert!(status["sourceRoots"].is_array());
        assert!(status["tools"].is_array());
        assert!(status["contractVersion"].is_number());
    }

    #[tokio::test]
    async fn unknown_api_route_returns_json_instead_of_spa() {
        let host = TestHost::new(None);
        let response = host
            .router()
            .oneshot(
                Request::get("/api/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("application/json")
        );
        let body = body_text(response).await;
        assert!(body.contains("\"code\":\"not_found\""));
        assert!(!body.contains("MKVO test UI"));
    }

    #[tokio::test]
    async fn runtime_errors_use_typed_json_and_status_mapping() {
        let host = TestHost::new(None);
        let request_correlation = "018f0000-0000-7000-8000-000000000999";
        let response = host
            .router()
            .oneshot(
                Request::get("/api/scans/not-a-job-id")
                    .header("x-correlation-id", request_correlation)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let header_correlation = response.headers()["x-correlation-id"]
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(header_correlation, request_correlation);
        let error: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(error["code"], "invalid_request");
        assert_eq!(error["correlationId"], header_correlation);
        assert_eq!(error["retryable"], false);
    }

    #[tokio::test]
    async fn serves_spa_for_unknown_non_api_paths() {
        let host = TestHost::new(None);
        let response = host
            .router()
            .oneshot(Request::get("/rename").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("MKVO test UI"));
    }

    #[tokio::test]
    async fn rejects_json_bodies_over_the_configured_limit() {
        let mut host = TestHost::new(None);
        Arc::get_mut(&mut host.config)
            .unwrap()
            .request_body_limit_bytes = 64;
        let body = format!(r#"{{"sourcePath":"{}"}}"#, "a".repeat(256));
        let response = host
            .router()
            .oneshot(
                Request::post("/api/scans")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    fn request(method: &str, path: &str, headers: &[(&str, &str)]) -> Request<Body> {
        let mut request = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn state_changes_from_another_origin_are_refused() {
        let mut host = TestHost::new(None);
        Arc::get_mut(&mut host.config).unwrap().allowed_hosts =
            vec!["nas.local".to_owned(), "mkvo.example.com:443".to_owned()];
        let router = host.router();
        for (headers, expected) in [
            (vec![], StatusCode::OK),
            // A listed name without a port only matches on the port MKVO is served on,
            // so another app on the same NAS cannot borrow it.
            (
                vec![
                    ("Host", "10.0.0.2:8886"),
                    ("Origin", "http://nas.local:9000"),
                ],
                StatusCode::FORBIDDEN,
            ),
            (
                vec![
                    ("Host", "127.0.0.1:8080"),
                    ("Origin", "https://mkvo.example.com"),
                ],
                StatusCode::OK,
            ),
            (
                vec![
                    ("Host", "127.0.0.1:8080"),
                    ("Origin", "https://mkvo.example.com:8443"),
                ],
                StatusCode::FORBIDDEN,
            ),
            (
                vec![
                    ("Host", "192.168.1.10:8886"),
                    ("Origin", "http://192.168.1.10:8886"),
                ],
                StatusCode::OK,
            ),
            (
                vec![
                    ("Host", "127.0.0.1:8080"),
                    ("X-Forwarded-Host", "mkvo.example.com"),
                    ("Origin", "https://mkvo.example.com"),
                ],
                StatusCode::OK,
            ),
            (
                vec![
                    ("Host", "10.0.0.2:8886"),
                    ("Origin", "http://NAS.local:8886"),
                ],
                StatusCode::OK,
            ),
            (
                vec![
                    ("Host", "192.168.1.10:8886"),
                    ("Origin", "http://evil.example"),
                ],
                StatusCode::FORBIDDEN,
            ),
            (
                vec![
                    ("Host", "192.168.1.10:8886"),
                    ("Origin", "http://192.168.1.10:9999"),
                ],
                StatusCode::FORBIDDEN,
            ),
            (
                vec![("Host", "192.168.1.10:8886"), ("Origin", "null")],
                StatusCode::FORBIDDEN,
            ),
        ] {
            let response = router
                .clone()
                .oneshot(request("POST", "/api/auth/logout", &headers))
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{headers:?}");
        }
        // Reads are not state changes; the Host check below covers them.
        let response = router
            .oneshot(request(
                "GET",
                "/api/auth/status",
                &[("Origin", "http://evil.example")],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn password_free_access_only_answers_known_host_names() {
        let mut host = TestHost::new(None);
        Arc::get_mut(&mut host.config).unwrap().allowed_hosts = vec!["nas.local".to_owned()];
        let router = host.router();
        for (host_header, expected) in [
            ("192.168.1.10:8886", StatusCode::OK),
            ("[fd00::2]:8886", StatusCode::OK),
            ("localhost:8080", StatusCode::OK),
            ("nas.local:8886", StatusCode::OK),
            ("rebound.evil.example:8886", StatusCode::FORBIDDEN),
        ] {
            let response = router
                .clone()
                .oneshot(request("GET", "/api/status", &[("Host", host_header)]))
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{host_header}");
        }

        // A signed-in session is not reachable by a rebound page, so any name works.
        let host = TestHost::new(Some(BasicAuth {
            username: "admin".to_owned(),
            password: "correct-password".to_owned(),
        }));
        let router = host.router();
        let login = router
            .clone()
            .oneshot(login_request("correct-password"))
            .await
            .unwrap();
        let cookie = login.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_owned();
        let response = router
            .clone()
            .oneshot(request(
                "GET",
                "/api/status",
                &[("Host", "mkvo.example.com"), ("Cookie", &cookie)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // The LAN bypass skips the password only for names a rebound page cannot use;
        // any other name asks for the password, and a session works by every name.
        fs::write(
            host.config.config_dir.join("security-settings.json"),
            r#"{"idle_timeout_minutes":null,"local_network_bypass":true}"#,
        )
        .unwrap();
        let router = host.router();
        let lan = |mut request: Request<Body>| {
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                "192.168.1.2:1234".parse::<SocketAddr>().unwrap(),
            ));
            request
        };
        let login = router
            .clone()
            .oneshot(login_request("correct-password"))
            .await
            .unwrap();
        let cookie = login.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_owned();
        for (headers, expected) in [
            (vec![("Host", "192.168.1.10:8886")], StatusCode::OK),
            (vec![("Host", "tower:8886")], StatusCode::UNAUTHORIZED),
            (
                vec![("Host", "tower:8886"), ("Cookie", &cookie)],
                StatusCode::OK,
            ),
        ] {
            let response = router
                .clone()
                .oneshot(lan(request("GET", "/api/status", &headers)))
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{headers:?}");
        }
        let response = router
            .clone()
            .oneshot(lan(request(
                "GET",
                "/api/auth/status",
                &[("Host", "tower:8886")],
            )))
            .await
            .unwrap();
        let status: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(status["password_required"], true);
        assert_eq!(status["authenticated"], false);
        let mut login = lan(login_request("correct-password"));
        login
            .headers_mut()
            .insert(header::HOST, "tower:8886".parse().unwrap());
        let response = router.oneshot(login).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let status: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(status["password_required"], true);
    }

    #[tokio::test]
    async fn responses_carry_security_headers_and_a_same_origin_csp() {
        let host = TestHost::new(None);
        for path in ["/", "/api/status", "/api/does-not-exist"] {
            let response = host
                .router()
                .oneshot(request("GET", path, &[]))
                .await
                .unwrap();
            let headers = response.headers();
            assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
            assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY", "{path}");
            assert_eq!(headers[header::REFERRER_POLICY], "same-origin", "{path}");
            let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
            for directive in [
                "default-src 'self'",
                "script-src 'self'",
                "img-src 'self' data: blob:",
                "frame-ancestors 'none'",
            ] {
                assert!(csp.contains(directive), "{csp}");
            }
            assert!(!csp.contains("unsafe-eval"), "{csp}");
        }
    }

    #[tokio::test]
    async fn https_proxies_get_a_secure_session_cookie() {
        let host = TestHost::new(Some(BasicAuth {
            username: "admin".to_owned(),
            password: "correct-password".to_owned(),
        }));
        let mut login = login_request("correct-password");
        login
            .headers_mut()
            .insert("x-forwarded-proto", "https".parse().unwrap());
        let response = host.router().oneshot(login).await.unwrap();
        assert!(
            response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .ends_with("; Secure")
        );
        let response = host
            .router()
            .oneshot(login_request("correct-password"))
            .await
            .unwrap();
        assert!(
            !response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("Secure")
        );
    }

    #[tokio::test]
    async fn the_request_timeout_spares_long_operations() {
        async fn slow() -> &'static str {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            "done"
        }
        let router = Router::new()
            .route("/api/status", axum::routing::get(slow))
            .route("/api/mux/apply", axum::routing::post(slow))
            .route("/api/rename/apply", axum::routing::post(slow))
            .route("/api/propedit/preview", axum::routing::post(slow))
            .route("/api/library/catalog", axum::routing::post(slow))
            .route("/api/media-servers/test", axum::routing::post(slow))
            .layer(middleware::from_fn_with_state(
                RequestTimeout(std::time::Duration::from_millis(50)),
                request_timeout,
            ));
        let response = router
            .clone()
            .oneshot(request("GET", "/api/status", &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        for path in [
            "/api/mux/apply",
            "/api/rename/apply",
            "/api/propedit/preview",
            "/api/library/catalog",
            "/api/media-servers/test",
        ] {
            let response = router
                .clone()
                .oneshot(request("POST", path, &[]))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
    }

    async fn sse_frames(response: Response) -> Vec<String> {
        let body = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            response.into_body().collect(),
        )
        .await
        .expect("the stream ends with the job")
        .unwrap()
        .to_bytes();
        String::from_utf8(body.to_vec())
            .unwrap()
            .trim_end()
            .split("\n\n")
            .map(str::to_owned)
            .collect()
    }

    #[tokio::test]
    async fn job_events_of_an_ended_job_are_just_its_state() {
        let host = TestHost::new(None);
        let job = host
            .runtime
            .start_scan(mkvo_contracts::ScanRequest {
                source_path: Some(host.config.media_root.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let id = job.id.to_string();
        while !host
            .runtime
            .get_scan_job(&id)
            .await
            .unwrap()
            .status
            .is_terminal()
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let response = host
            .router()
            .oneshot(request("GET", &format!("/api/scans/{id}/events"), &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let frames = sse_frames(response).await;
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert!(frames[0].lines().any(|line| line == "event: snapshot"));
        assert!(frames[0].contains(&format!(r#""id":"{id}""#)));

        let response = host
            .router()
            .oneshot(request("GET", &format!("/api/operations/{id}/events"), &[]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn job_events_of_a_running_job_end_with_the_event_that_ends_it() {
        let (sender, receiver) = tokio::sync::broadcast::channel(8);
        for (sequence, event) in [
            serde_json::json!({ "type": "status_changed", "status": "Running" }),
            serde_json::json!({ "type": "failed", "message": "probe failed" }),
            serde_json::json!({ "type": "status_changed", "status": "Running" }),
        ]
        .into_iter()
        .enumerate()
        {
            let envelope =
                serde_json::from_value::<mkvo_contracts::JobEventEnvelope>(serde_json::json!({
                    "jobId": "01a1d606-70be-7350-9879-a59a34f6367a",
                    "correlationId": "01a1d606-70be-7350-9879-a5a367cd3fc5",
                    "sequence": sequence,
                    "emittedUtc": "2026-11-13T17:48:28Z",
                    "event": event,
                }))
                .unwrap();
            sender.send(envelope).unwrap();
        }
        let snapshot = axum::response::sse::Event::default()
            .event("snapshot")
            .data("running");
        let stream = routes::job_event_stream(snapshot, false, receiver);
        // The sender stays open: only the failed event may end the stream.
        let frames = sse_frames(axum::response::Sse::new(stream).into_response()).await;
        assert_eq!(frames.len(), 3, "{frames:?}");
        assert!(frames[0].lines().any(|line| line == "event: snapshot"));
        assert!(frames[1].contains(r#""status":"Running""#));
        assert!(frames[2].contains(r#""type":"failed""#));
        drop(sender);
    }
}
