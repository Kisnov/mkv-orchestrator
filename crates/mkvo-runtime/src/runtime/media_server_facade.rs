use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use mkvo_application::PortError;
use mkvo_contracts::ApiErrorCode;
use std::time::{Duration, Instant};

/// How long a refused credential is remembered before the server is asked again.
const CREDENTIAL_REJECTION_MEMORY: Duration = Duration::from_secs(5 * 60);

/// A credential a media server refused, kept so it is not retried per poster.
pub(super) struct CredentialRejection {
    fingerprint: String,
    message: String,
    at: Instant,
}

impl CredentialRejection {
    /// A new key or URL is a different connection, so saving one needs no reset.
    fn covers(&self, fingerprint: &str) -> bool {
        self.fingerprint == fingerprint && self.at.elapsed() < CREDENTIAL_REJECTION_MEMORY
    }
}

/// Hash of kind, URL and key: what the memory keeps in place of the key.
fn connection_fingerprint(connection: &MediaServerConnection) -> RuntimeResult<String> {
    stable_fingerprint(connection).map_err(|error| RuntimeError::internal(error.to_string()))
}

/// Records a refusal. False when this credential's refusal was already known.
fn remember_rejection(
    rejections: &mut HashMap<MediaServerId, CredentialRejection>,
    id: MediaServerId,
    fingerprint: &str,
    message: &str,
) -> bool {
    if rejections
        .get(&id)
        .is_some_and(|rejection| rejection.covers(fingerprint))
    {
        return false;
    }
    rejections.insert(
        id,
        CredentialRejection {
            fingerprint: fingerprint.to_owned(),
            message: message.to_owned(),
            at: Instant::now(),
        },
    );
    true
}

/// A 403: the server knows the key but will not let it do this.
fn insufficient_credential_message(server: &str, detail: &str) -> String {
    format!(
        "{server} accepted the API key but refused the request ({detail}). The key needs administrator rights."
    )
}

impl MkvoRuntime {
    pub async fn get_library_catalog(
        &self,
        request: LibraryCatalogRequest,
    ) -> RuntimeResult<LibraryCatalogResponse> {
        let loaded = self.settings_service().load().await?;
        let server = loaded
            .settings
            .media_servers
            .iter()
            .find(|server| {
                server
                    .id
                    .to_string()
                    .eq_ignore_ascii_case(&request.server_id)
            })
            .ok_or_else(|| {
                RuntimeError::not_found(format!("media server {}", request.server_id))
            })?;
        let server_paths = server
            .libraries
            .iter()
            .filter(|library| {
                library.enabled && library.name.eq_ignore_ascii_case(&request.library_name)
            })
            .map(|library| library.server_path.clone())
            .collect::<Vec<_>>();
        if server_paths.is_empty() {
            return Err(RuntimeError::not_found(format!(
                "library {:?} on media server {}",
                request.library_name, request.server_id
            )));
        }
        let connection = self.media_server_connection(server).await?;
        self.known_credential_rejection(server, &connection).await?;
        let client = self.media_server_client(server.kind, &loaded.settings);
        let items = client
            .discover_items(
                &connection,
                &request.library_name,
                &server_paths,
                CancellationToken::new(),
            )
            .await;
        let items = self
            .credential_rejection(server, &connection, items)
            .await?
            .into_iter()
            .map(|item| LibraryCatalogItem {
                id: item.id,
                title: item.title,
                year: item.year,
                media_type: item.media_type,
                has_poster: item.has_poster,
            })
            .collect();
        Ok(LibraryCatalogResponse { items })
    }

    pub async fn get_library_artwork(
        &self,
        request: LibraryArtworkRequest,
    ) -> RuntimeResult<LibraryArtworkResponse> {
        if request.item_id.is_empty()
            || request.item_id.len() > 128
            || !request
                .item_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(RuntimeError::invalid("invalid media-server item id"));
        }
        let loaded = self.settings_service().load().await?;
        let server = loaded
            .settings
            .media_servers
            .iter()
            .find(|server| {
                server
                    .id
                    .to_string()
                    .eq_ignore_ascii_case(&request.server_id)
            })
            .ok_or_else(|| {
                RuntimeError::not_found(format!("media server {}", request.server_id))
            })?;
        let connection = self.media_server_connection(server).await?;
        self.known_credential_rejection(server, &connection).await?;
        let artwork = self
            .media_server_client(server.kind, &loaded.settings)
            .fetch_artwork(&connection, &request.item_id, CancellationToken::new())
            .await;
        let artwork = self
            .credential_rejection(server, &connection, artwork)
            .await?;
        if !artwork
            .content_type
            .to_ascii_lowercase()
            .starts_with("image/")
        {
            return Err(RuntimeError::invalid(
                "media server returned a non-image poster response",
            ));
        }
        Ok(LibraryArtworkResponse {
            content_type: artwork.content_type,
            data_base64: STANDARD.encode(artwork.bytes),
        })
    }

    pub async fn get_library_local_artwork(
        &self,
        request: LibraryLocalArtworkRequest,
    ) -> RuntimeResult<LibraryArtworkResponse> {
        if request.folder_paths.is_empty() || request.folder_paths.len() > 256 {
            return Err(RuntimeError::invalid(
                "local artwork requires between 1 and 256 folder paths",
            ));
        }

        let mut directories = Vec::<PathBuf>::new();
        for folder_path in request.folder_paths {
            let Ok(folder) = self
                .inner
                .dependencies
                .paths
                .authorize_read(Path::new(&folder_path))
                .await
            else {
                continue;
            };
            if !folder.is_dir() {
                continue;
            }

            // A series audit is grouped by season folder, while poster.jpg is
            // normally stored beside those seasons in the series directory.
            // Prefer that shared series poster, then allow a season poster.
            if is_season_directory(&folder)
                && let Some(parent) = folder.parent()
                && let Ok(parent) = self.inner.dependencies.paths.authorize_read(parent).await
            {
                push_unique_path(&mut directories, parent);
            }
            push_unique_path(&mut directories, folder);
        }

        for directory in directories {
            let Some(candidate) = find_local_poster(&directory).await? else {
                continue;
            };
            let poster = self
                .inner
                .dependencies
                .paths
                .authorize_read(&candidate)
                .await?;
            let metadata = tokio::fs::metadata(&poster).await?;
            if metadata.len() == 0 || metadata.len() > 10 * 1024 * 1024 {
                continue;
            }
            let bytes = tokio::fs::read(&poster).await?;
            return Ok(LibraryArtworkResponse {
                content_type: local_poster_content_type(&poster).to_owned(),
                data_base64: STANDARD.encode(bytes),
            });
        }

        Err(RuntimeError::not_found(
            "no local poster artwork was found for this library title",
        ))
    }

    async fn media_server_connection(
        &self,
        server: &MediaServerSettings,
    ) -> RuntimeResult<MediaServerConnection> {
        let key = server
            .credential
            .secret_reference
            .as_deref()
            .unwrap_or("mediaServer:missing");
        let legacy_key = format!("mediaServer:{}", server.id);
        let credential = self
            .secret_alias(&[key, &legacy_key])
            .await?
            .unwrap_or_default();
        Ok(MediaServerConnection {
            kind: server.kind,
            base_url: server.server_url.clone(),
            credential,
        })
    }

    /// Fails without a request while the server is known to refuse this credential.
    async fn known_credential_rejection(
        &self,
        server: &MediaServerSettings,
        connection: &MediaServerConnection,
    ) -> RuntimeResult<()> {
        let fingerprint = connection_fingerprint(connection)?;
        match self
            .inner
            .media_server_rejections
            .read()
            .await
            .get(&server.id)
        {
            Some(rejection) if rejection.covers(&fingerprint) => Err(RuntimeError::new(
                ApiErrorCode::CredentialRejected,
                rejection.message.clone(),
            )),
            _ => Ok(()),
        }
    }

    async fn forget_credential_rejection(&self, id: MediaServerId) {
        self.inner.media_server_rejections.write().await.remove(&id);
    }

    /// Reports a key the media server refused as its own error, naming the
    /// server, and remembers the refusal. Logs it once, not once per request.
    async fn credential_rejection<T>(
        &self,
        server: &MediaServerSettings,
        connection: &MediaServerConnection,
        result: Result<T, PortError>,
    ) -> RuntimeResult<T> {
        match result {
            Err(PortError::CredentialRejected(detail)) => {
                let message = if connection.credential.trim().is_empty() {
                    format!(
                        "No API key is stored for {} ({detail}). Enter it in Settings.",
                        server.name
                    )
                } else {
                    format!(
                        "{} rejected the stored API key ({detail}). Enter the key again in Settings.",
                        server.name
                    )
                };
                if remember_rejection(
                    &mut *self.inner.media_server_rejections.write().await,
                    server.id,
                    &connection_fingerprint(connection)?,
                    &message,
                ) {
                    tracing::warn!(
                        server = %server.name,
                        server_id = %server.id,
                        kind = server_kind_name(server.kind),
                        url = %connection.base_url,
                        key_length = connection.credential.trim().len(),
                        detail = %detail,
                        "media server rejected the stored credential; not asking it again for 5 minutes"
                    );
                }
                Err(RuntimeError::new(ApiErrorCode::CredentialRejected, message))
            }
            // Not remembered: a key refused here may still be allowed elsewhere.
            Err(PortError::CredentialInsufficient(detail)) => Err(RuntimeError::new(
                ApiErrorCode::CredentialInsufficient,
                insufficient_credential_message(&server.name, &detail),
            )),
            other => Ok(other?),
        }
    }

    pub async fn test_media_server_connection(
        &self,
        request: MediaServerConnectionRequest,
    ) -> RuntimeResult<MediaServerTestResponse> {
        let loaded = self.settings_service().load().await?;
        let stored = request.id.as_deref().and_then(|id| {
            loaded
                .settings
                .media_servers
                .iter()
                .find(|server| server.id.to_string().eq_ignore_ascii_case(id))
        });
        let kind = request
            .server_type
            .as_deref()
            .map(parse_server_kind)
            .transpose()?
            .or_else(|| stored.map(|server| server.kind))
            .unwrap_or(MediaServerKind::Jellyfin);
        let requested_url = request
            .server_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        // A blank key means "use the stored one": the form reloads blank after a save.
        let api_key = request.api_key.filter(|value| !value.trim().is_empty());
        if api_key.is_none()
            && let (Some(requested_url), Some(stored)) = (requested_url, stored)
            && !media_server_urls_equivalent(requested_url, &stored.server_url)
        {
            return Err(RuntimeError::invalid(
                "Changing a media server URL requires its API key to be entered again.",
            ));
        }
        let url = requested_url
            .map(str::to_owned)
            .or_else(|| stored.map(|server| server.server_url.clone()))
            .ok_or_else(|| RuntimeError::invalid("Enter a server URL."))?;
        validate_media_server_url(&url)?;
        let credential = match (api_key, stored) {
            (Some(value), _) => value,
            (None, Some(stored)) => self.media_server_connection(stored).await?.credential,
            (None, None) => String::new(),
        };
        let client = self.media_server_client(kind, &loaded.settings);
        let connection = MediaServerConnection {
            kind,
            base_url: url,
            credential,
        };
        let result = client
            .discover_libraries(&connection, CancellationToken::new())
            .await;
        if let (Ok(_), Some(stored)) = (&result, stored) {
            self.forget_credential_rejection(stored.id).await;
        }
        match result {
            Ok(libraries) => Ok(MediaServerTestResponse {
                success: true,
                status: if libraries.is_empty() {
                    "Connection succeeded, but no library paths were returned.".to_owned()
                } else {
                    format!(
                        "Connection succeeded. Found {} library path(s).",
                        libraries.len()
                    )
                },
                library_count: libraries.len(),
            }),
            Err(PortError::CredentialRejected(detail))
                if connection.credential.trim().is_empty() =>
            {
                Ok(MediaServerTestResponse {
                    success: false,
                    status: format!(
                        "No API key was entered and none is stored for this server ({detail})."
                    ),
                    library_count: 0,
                })
            }
            Err(PortError::CredentialInsufficient(detail)) => Ok(MediaServerTestResponse {
                success: false,
                status: insufficient_credential_message("The server", &detail),
                library_count: 0,
            }),
            Err(error) => Ok(MediaServerTestResponse {
                success: false,
                status: error.to_string(),
                library_count: 0,
            }),
        }
    }

    pub async fn sync_media_server_libraries(
        &self,
        id: &str,
    ) -> RuntimeResult<MediaServerSyncResponse> {
        let loaded = self.settings_service().load().await?;
        let mut settings = loaded.settings;
        let index = settings
            .media_servers
            .iter()
            .position(|server| server.id.to_string().eq_ignore_ascii_case(id))
            .ok_or_else(|| RuntimeError::not_found(format!("media server {id}")))?;
        let server = settings.media_servers[index].clone();
        let key = server
            .credential
            .secret_reference
            .as_deref()
            .unwrap_or("mediaServer:missing");
        let legacy_key = format!("mediaServer:{}", server.id);
        let credential = self
            .secret_alias(&[key, &legacy_key])
            .await?
            .unwrap_or_default();
        let connection = MediaServerConnection {
            kind: server.kind,
            base_url: server.server_url.clone(),
            credential,
        };
        let libraries = self
            .media_server_client(server.kind, &settings)
            .discover_libraries(&connection, CancellationToken::new())
            .await;
        let mut libraries = self
            .credential_rejection(&server, &connection, libraries)
            .await?;
        self.forget_credential_rejection(server.id).await;
        resolve_media_server_local_paths(&mut libraries, &self.inner.config.media_root);
        settings.media_servers[index]
            .libraries
            .clone_from(&libraries);
        settings.media_servers[index].last_synced_at = Some(Utc::now());
        let saved = self
            .settings_service()
            .save(mkvo_contracts::SaveSettingsRequest {
                settings,
                secrets: Vec::new(),
                expected_revision: Some(loaded.revision),
            })
            .await?;
        let server = web_media_server(&saved.settings.media_servers[index]);
        let status = format!(
            "Sync complete for {}: {} library path(s).",
            server.name,
            libraries.len()
        );
        Ok(MediaServerSyncResponse {
            libraries: server.libraries.clone(),
            server,
            status,
        })
    }

    /// Render the operation log as plain text for download.
    ///
    /// The host writes the file, so this returns the content and a suggested
    /// name rather than touching the filesystem itself — the browser and the
    /// desktop save it in different ways.
    pub(super) async fn export_logs_impl(&self) -> RuntimeResult<LogExport> {
        let entries = self
            .inner
            .dependencies
            .logs
            .query(&LogQuery::default())
            .await?;
        let mut content = String::new();
        for entry in &entries {
            content.push_str(&format!(
                "{timestamp}\t{level:?}\t{area}\t{correlation}\t{message}",
                timestamp = entry.timestamp_utc.to_rfc3339(),
                level = entry.level,
                area = entry.area,
                correlation = entry.correlation_id,
                message = entry.message,
            ));
            if !entry.detail.trim().is_empty() {
                // Keep one entry on one line so the export stays greppable.
                content.push('\t');
                content.push_str(&entry.detail.replace(['\r', '\n'], " "));
            }
            content.push('\n');
        }
        Ok(LogExport {
            file_name: format!("mkvo-logs-{}.txt", Utc::now().format("%Y%m%d-%H%M%S")),
            entry_count: entries.len(),
            content,
        })
    }

    /// Recent jobs across kinds, newest first, for the Logs and Jobs views.
    pub(super) async fn list_recent_jobs_impl(
        &self,
        limit: Option<usize>,
    ) -> RuntimeResult<RecentJobsResponse> {
        let limit = limit.unwrap_or(50).clamp(1, 500);
        let jobs = self.inner.dependencies.jobs.list_recent(limit).await?;
        Ok(RecentJobsResponse { jobs })
    }
}

const LOCAL_POSTER_NAMES: [&str; 9] = [
    "poster.jpg",
    "poster.jpeg",
    "poster.png",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
];

fn is_season_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let normalized = name.trim().to_ascii_lowercase();
    let exact = normalized.strip_prefix("season").is_some_and(|suffix| {
        suffix
            .trim()
            .chars()
            .all(|character| character.is_ascii_digit())
    }) || normalized.strip_prefix('s').is_some_and(|suffix| {
        !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
    });
    exact
        || ["season", "s"].into_iter().any(|marker| {
            let Some(index) = normalized.rfind(marker) else {
                return false;
            };
            let boundary_before = index == 0
                || normalized[..index]
                    .chars()
                    .next_back()
                    .is_some_and(|character| !character.is_ascii_alphanumeric());
            let digits = normalized[index + marker.len()..].trim();
            boundary_before
                && !digits.is_empty()
                && digits.chars().all(|character| character.is_ascii_digit())
        })
}

fn push_unique_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.iter().any(|existing| same_path(existing, &candidate)) {
        paths.push(candidate);
    }
}

async fn find_local_poster(directory: &Path) -> RuntimeResult<Option<PathBuf>> {
    let mut matches = HashMap::<String, PathBuf>::new();
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_file() {
            matches.insert(
                entry.file_name().to_string_lossy().to_ascii_lowercase(),
                entry.path(),
            );
        }
    }
    Ok(LOCAL_POSTER_NAMES
        .iter()
        .find_map(|name| matches.get(*name).cloned()))
}

fn local_poster_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        _ => "image/jpeg",
    }
}

#[cfg(test)]
mod media_server_credential_tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use mkvo_contracts::{ApiErrorCode, WebMediaServerLibraryPath, WebMediaServerRequest};

    use super::*;
    use crate::MkvoRuntimeBuilder;

    /// A Jellyfin stand-in that only answers requests carrying the key `k`.
    struct FakeJellyfin {
        url: String,
        paths: Arc<Mutex<Vec<String>>>,
        refuse_everything: Arc<AtomicBool>,
        forbid_libraries: Arc<AtomicBool>,
        slow_first_answer: Arc<AtomicBool>,
    }

    impl FakeJellyfin {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake server");
            let url = format!("http://{}", listener.local_addr().expect("fake address"));
            let paths = Arc::new(Mutex::new(Vec::new()));
            let seen = Arc::clone(&paths);
            let refuse_everything = Arc::new(AtomicBool::new(false));
            let refusing = Arc::clone(&refuse_everything);
            let forbid_libraries = Arc::new(AtomicBool::new(false));
            let forbidding = Arc::clone(&forbid_libraries);
            let slow_first_answer = Arc::new(AtomicBool::new(false));
            let slow = Arc::clone(&slow_first_answer);
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut reader = BufReader::new(stream);
                    let mut path = String::new();
                    let mut authorized = false;
                    let mut line = String::new();
                    while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
                        if line.trim_end().is_empty() {
                            break;
                        }
                        if path.is_empty() {
                            path = line.split(' ').nth(1).unwrap_or_default().to_owned();
                        } else if let Some((name, value)) = line.split_once(':') {
                            authorized |= name.eq_ignore_ascii_case("authorization")
                                && value.trim() == r#"MediaBrowser Token="k""#;
                        }
                        line.clear();
                    }
                    seen.lock().expect("path log").push(path.clone());
                    if slow.swap(false, Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    let (status, content_type, body) = if !authorized
                        || refusing.load(Ordering::SeqCst)
                    {
                        ("401 Unauthorized", "text/plain", "")
                    } else if path.starts_with("/Library/VirtualFolders")
                        && forbidding.load(Ordering::SeqCst)
                    {
                        ("403 Forbidden", "text/plain", "")
                    } else if path.starts_with("/Library/VirtualFolders") {
                        (
                            "200 OK",
                            "application/json",
                            r#"[{"Name":"Shows","ItemId":"lib-1","CollectionType":"tvshows","Locations":["/data/tv"]}]"#,
                        )
                    } else if path.contains("/Images/") {
                        ("200 OK", "image/jpeg", "poster")
                    } else {
                        (
                            "200 OK",
                            "application/json",
                            r#"{"Items":[{"Id":"show-1","Name":"Example Show","Type":"Series","Path":"/data/tv/Example Show","ImageTags":{"Primary":"tag"}}]}"#,
                        )
                    };
                    let _ = reader.get_mut().write_all(
                        format!(
                            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                }
            });
            Self {
                url,
                paths,
                refuse_everything,
                forbid_libraries,
                slow_first_answer,
            }
        }

        fn paths(&self) -> Vec<String> {
            self.paths.lock().expect("path log").clone()
        }
    }

    struct Host {
        _directory: tempfile::TempDir,
        runtime: MkvoRuntime,
    }

    fn host() -> Host {
        let directory = tempfile::tempdir().expect("temporary directory");
        let media = directory.path().join("media");
        std::fs::create_dir_all(&media).expect("media root");
        let runtime = MkvoRuntimeBuilder::new(&media, directory.path().join("config"))
            .app_name("test")
            .disable_legacy_migration()
            .authorized_root(&media, true)
            .build()
            .expect("runtime");
        Host {
            _directory: directory,
            runtime,
        }
    }

    /// Saves one Jellyfin server the way the Settings page does.
    async fn save_server(
        runtime: &MkvoRuntime,
        id: Option<&str>,
        url: &str,
        api_key: Option<&str>,
    ) -> WebMediaServer {
        let saved = runtime
            .save_web_settings(WebSettingsRequest {
                media_servers: Some(vec![WebMediaServerRequest {
                    id: id.map(str::to_owned),
                    name: Some("Home".to_owned()),
                    server_type: Some("jellyfin".to_owned()),
                    server_url: Some(url.to_owned()),
                    api_key: api_key.map(str::to_owned),
                    is_default: true,
                    libraries: Some(vec![WebMediaServerLibraryPath {
                        id: "library-1".to_owned(),
                        name: "Shows".to_owned(),
                        media_type: "tvshows".to_owned(),
                        server_path: "/data/tv".to_owned(),
                        container_path: "/media/tv".to_owned(),
                        is_enabled: true,
                    }]),
                }]),
                ..WebSettingsRequest::default()
            })
            .await
            .expect("save settings");
        saved
            .media_servers
            .into_iter()
            .next()
            .expect("saved server")
    }

    fn test_request(
        server: &WebMediaServer,
        url: &str,
        api_key: &str,
    ) -> MediaServerConnectionRequest {
        MediaServerConnectionRequest {
            id: Some(server.id.clone()),
            name: Some(server.name.clone()),
            server_type: Some(server.server_type.clone()),
            server_url: Some(url.to_owned()),
            api_key: Some(api_key.to_owned()),
        }
    }

    /// The Settings form reloads with a blank key field after a save, so Test
    /// sends `""`. That is "use the stored key", not a credential.
    #[tokio::test]
    async fn test_with_a_blank_key_uses_the_stored_one() {
        let jellyfin = FakeJellyfin::start();
        let host = host();
        let server = save_server(&host.runtime, None, &jellyfin.url, Some("k")).await;

        let result = host
            .runtime
            .test_media_server_connection(test_request(&server, &jellyfin.url, ""))
            .await
            .expect("test");

        assert!(result.success, "{}", result.status);
        assert_eq!(result.library_count, 1);
    }

    #[tokio::test]
    async fn test_with_a_blank_key_never_sends_the_stored_one_to_another_url() {
        let jellyfin = FakeJellyfin::start();
        let other = FakeJellyfin::start();
        let host = host();
        let server = save_server(&host.runtime, None, &jellyfin.url, Some("k")).await;

        let error = host
            .runtime
            .test_media_server_connection(test_request(&server, &other.url, ""))
            .await
            .expect_err("a changed URL needs the key again");

        assert_eq!(error.code, ApiErrorCode::InvalidRequest);
        assert!(other.paths().is_empty());
    }

    /// The whole stored-key path: saved through Settings (with the non-UUID id
    /// a browser without `crypto.randomUUID` sends and stray whitespace around
    /// the key), read back, and sent as the Jellyfin header on every endpoint.
    #[tokio::test]
    async fn a_saved_key_authorizes_catalog_and_artwork_requests() {
        let jellyfin = FakeJellyfin::start();
        let host = host();
        let server = save_server(&host.runtime, Some("local-1"), &jellyfin.url, Some(" k\n")).await;

        let catalog = host
            .runtime
            .get_library_catalog(LibraryCatalogRequest {
                server_id: server.id.clone(),
                library_name: "Shows".to_owned(),
            })
            .await
            .expect("catalog");
        assert_eq!(catalog.items.len(), 1);

        let artwork = host
            .runtime
            .get_library_artwork(LibraryArtworkRequest {
                server_id: server.id,
                item_id: catalog.items[0].id.clone(),
            })
            .await
            .expect("artwork");
        assert_eq!(artwork.content_type, "image/jpeg");
    }

    fn artwork_request(server: &WebMediaServer) -> LibraryArtworkRequest {
        LibraryArtworkRequest {
            server_id: server.id.clone(),
            item_id: "show-1".to_owned(),
        }
    }

    /// A refused key is the user's to fix, so it must not look like an outage,
    /// and it has to say which server and which endpoint refused it.
    #[tokio::test]
    async fn a_refused_key_names_the_server_and_the_endpoint() {
        let jellyfin = FakeJellyfin::start();
        let host = host();
        let server = save_server(&host.runtime, None, &jellyfin.url, Some("wrong")).await;

        let error = host
            .runtime
            .get_library_artwork(artwork_request(&server))
            .await
            .expect_err("refused key");

        assert_eq!(error.code, ApiErrorCode::CredentialRejected);
        assert!(!error.retryable);
        assert!(
            error
                .message
                .starts_with("Home rejected the stored API key"),
            "{error}"
        );
        assert!(
            error
                .message
                .contains("HTTP 401 Unauthorized for Items/show-1/Images/Primary"),
            "{error}"
        );
    }

    /// Sync, catalog and artwork also accept the key under its legacy name, so
    /// Test must too, or a server that syncs is told it has no key.
    #[tokio::test]
    async fn test_finds_a_key_stored_only_under_its_legacy_name() {
        let jellyfin = FakeJellyfin::start();
        let host = host();
        let server = save_server(&host.runtime, None, &jellyfin.url, Some("k")).await;
        host.runtime
            .inner
            .dependencies
            .secrets
            .remove(&format!("media_server.{}.api_key", server.id))
            .await
            .expect("remove the current secret name");

        let result = host
            .runtime
            .test_media_server_connection(test_request(&server, &jellyfin.url, ""))
            .await
            .expect("test");

        assert!(result.success, "{}", result.status);
    }

    /// With nothing to send, the server's 401 is about a missing key, not a wrong one.
    #[tokio::test]
    async fn a_missing_key_is_reported_as_missing() {
        let jellyfin = FakeJellyfin::start();
        let host = host();
        let server = save_server(&host.runtime, None, &jellyfin.url, None).await;

        let result = host
            .runtime
            .test_media_server_connection(test_request(&server, &jellyfin.url, ""))
            .await
            .expect("test");
        assert!(!result.success);
        assert!(
            result
                .status
                .starts_with("No API key was entered and none is stored for this server"),
            "{}",
            result.status
        );

        let error = host
            .runtime
            .get_library_artwork(artwork_request(&server))
            .await
            .expect_err("no key");
        assert_eq!(error.code, ApiErrorCode::CredentialRejected);
        assert!(
            error.message.starts_with("No API key is stored for Home"),
            "{error}"
        );
    }

    /// The refusal is logged when it is recorded, so recording it once per
    /// credential is what keeps a poster grid from writing a line per card.
    #[test]
    fn a_refusal_is_recorded_once_per_credential() {
        let id = MediaServerId::default();
        let connection = |credential: &str| MediaServerConnection {
            kind: MediaServerKind::Jellyfin,
            base_url: "http://media.local".to_owned(),
            credential: credential.to_owned(),
        };
        let fingerprint =
            |credential: &str| connection_fingerprint(&connection(credential)).expect("hash");
        let mut rejections = HashMap::new();

        assert!(remember_rejection(
            &mut rejections,
            id,
            &fingerprint("wrong"),
            "refused"
        ));
        assert!(!remember_rejection(
            &mut rejections,
            id,
            &fingerprint("wrong"),
            "refused"
        ));
        // The memory outlives the request, so it must not hold the key itself.
        assert!(!rejections[&id].fingerprint.contains("wrong"));
        assert!(remember_rejection(
            &mut rejections,
            id,
            &fingerprint("other"),
            "refused"
        ));
    }

    /// One poster per Library card used to mean one failing request and one
    /// error line per card, on every visit, for as long as the key was wrong.
    #[tokio::test]
    async fn a_refused_key_is_asked_once_until_it_changes() {
        let jellyfin = FakeJellyfin::start();
        let host = host();
        let server = save_server(&host.runtime, None, &jellyfin.url, Some("wrong")).await;

        let first = host
            .runtime
            .get_library_artwork(artwork_request(&server))
            .await
            .expect_err("refused key");
        let second = host
            .runtime
            .get_library_artwork(artwork_request(&server))
            .await
            .expect_err("remembered refusal");
        let catalog = host
            .runtime
            .get_library_catalog(LibraryCatalogRequest {
                server_id: server.id.clone(),
                library_name: "Shows".to_owned(),
            })
            .await
            .expect_err("remembered refusal");

        assert_eq!(second.code, ApiErrorCode::CredentialRejected);
        assert_eq!(second.message, first.message);
        assert_eq!(catalog.message, first.message);
        assert_eq!(jellyfin.paths().len(), 1, "{:?}", jellyfin.paths());

        // Saving a different key is a different credential: ask again.
        let server = save_server(&host.runtime, Some(&server.id), &jellyfin.url, Some("k")).await;
        host.runtime
            .get_library_artwork(artwork_request(&server))
            .await
            .expect("artwork with the new key");
    }

    /// Jellyfin answers 403 on its admin-only library list for a valid user
    /// token. That key still fetches posters, so it must not be remembered as
    /// refused, and the message has to say what is actually wrong.
    #[tokio::test]
    async fn a_key_without_admin_rights_is_not_remembered_as_refused() {
        let jellyfin = FakeJellyfin::start();
        let host = host();
        let server = save_server(&host.runtime, None, &jellyfin.url, Some("k")).await;
        jellyfin.forbid_libraries.store(true, Ordering::SeqCst);

        let error = host
            .runtime
            .sync_media_server_libraries(&server.id)
            .await
            .expect_err("forbidden library list");
        assert_eq!(error.code, ApiErrorCode::CredentialInsufficient);
        assert!(
            error
                .message
                .starts_with("Home accepted the API key but refused the request"),
            "{error}"
        );
        assert!(
            error
                .message
                .contains("HTTP 403 Forbidden for Library/VirtualFolders"),
            "{error}"
        );

        let result = host
            .runtime
            .test_media_server_connection(test_request(&server, &jellyfin.url, ""))
            .await
            .expect("test");
        assert!(!result.success);
        assert!(
            result.status.contains("needs administrator rights"),
            "{}",
            result.status
        );

        host.runtime
            .get_library_artwork(artwork_request(&server))
            .await
            .expect("artwork still works with this key");
    }

    /// Server ids the facade warned about. Process-wide on purpose: tracing
    /// caches whether a callsite is enabled per process, so a per-test
    /// subscriber misses events when another test reached the callsite first.
    static WARNED_SERVERS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    struct RejectionWarnings;

    impl tracing::Subscriber for RejectionWarnings {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() == tracing::Level::WARN
                && metadata.target().ends_with("media_server_facade")
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct ServerId(Option<String>);
            impl tracing::field::Visit for ServerId {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "server_id" {
                        self.0 = Some(format!("{value:?}"));
                    }
                }
            }
            let mut server_id = ServerId(None);
            event.record(&mut server_id);
            WARNED_SERVERS
                .lock()
                .expect("warning log")
                .extend(server_id.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// A poster grid asks for its posters at once. The ones already on their
    /// way when the first 401 arrives are not held back, so the server can
    /// still see one request per card; the log line is what must stay single.
    #[tokio::test]
    async fn concurrent_refusals_are_logged_once() {
        static CAPTURE: std::sync::Once = std::sync::Once::new();
        CAPTURE.call_once(|| {
            tracing::subscriber::set_global_default(RejectionWarnings)
                .expect("no other global subscriber in this test binary");
        });
        let jellyfin = FakeJellyfin::start();
        let host = host();
        let server = save_server(&host.runtime, None, &jellyfin.url, Some("wrong")).await;
        jellyfin.slow_first_answer.store(true, Ordering::SeqCst);

        let results = futures::future::join_all(
            (0..8).map(|_| host.runtime.get_library_artwork(artwork_request(&server))),
        )
        .await;

        for result in results {
            let error = result.expect_err("refused key");
            assert_eq!(error.code, ApiErrorCode::CredentialRejected);
        }
        let warnings = WARNED_SERVERS
            .lock()
            .expect("warning log")
            .iter()
            .filter(|id| **id == server.id)
            .count();
        assert_eq!(warnings, 1);
    }

    #[tokio::test]
    async fn a_successful_test_forgets_the_refusal() {
        let jellyfin = FakeJellyfin::start();
        let host = host();
        let server = save_server(&host.runtime, None, &jellyfin.url, Some("k")).await;

        jellyfin.refuse_everything.store(true, Ordering::SeqCst);
        host.runtime
            .get_library_artwork(artwork_request(&server))
            .await
            .expect_err("refused key");
        jellyfin.refuse_everything.store(false, Ordering::SeqCst);
        host.runtime
            .get_library_artwork(artwork_request(&server))
            .await
            .expect_err("remembered refusal");

        let result = host
            .runtime
            .test_media_server_connection(test_request(&server, &jellyfin.url, ""))
            .await
            .expect("test");
        assert!(result.success, "{}", result.status);
        host.runtime
            .get_library_artwork(artwork_request(&server))
            .await
            .expect("artwork after a successful test");
    }
}

#[cfg(test)]
mod local_artwork_tests {
    use super::*;

    #[test]
    fn recognizes_common_season_directory_names() {
        assert!(is_season_directory(Path::new("/shows/Example/Season 01")));
        assert!(is_season_directory(Path::new("/shows/Example/S2")));
        assert!(is_season_directory(Path::new(
            "/shows/Example/Example - Season 03"
        )));
        assert!(!is_season_directory(Path::new("/shows/Example")));
        assert!(!is_season_directory(Path::new("/movies/Seven")));
    }

    #[tokio::test]
    async fn finds_poster_names_case_insensitively() {
        let directory = tempfile::tempdir().expect("temporary directory");
        tokio::fs::write(directory.path().join("Poster.JPG"), b"poster")
            .await
            .expect("write poster");
        let found = find_local_poster(directory.path())
            .await
            .expect("search")
            .expect("poster");
        assert_eq!(
            found.file_name().and_then(|value| value.to_str()),
            Some("Poster.JPG")
        );
    }
}
