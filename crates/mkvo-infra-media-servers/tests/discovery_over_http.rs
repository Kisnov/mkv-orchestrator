//! Discovery exercised over real HTTP against a stub media server.
//!
//! The unit tests parse fixture payloads directly, which leaves the parts that
//! only exist on the wire untested: URL construction from a user-entered base,
//! the per-vendor auth header, the JSON/XML content split, and status handling.
//! A stub server covers those without requiring a real Emby, Jellyfin, or Plex
//! instance.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use mkvo_infra_media_servers::{
    DiscoveredItem, MediaServerClient, MediaServerConfig, MediaServerDiscoveryClient,
    MediaServerKind, MediaServerPathMapping, SecretString,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct Seen {
    headers: Arc<Mutex<Vec<(String, HeaderMap)>>>,
}

impl Seen {
    fn record(&self, path: &str, headers: &HeaderMap) {
        self.headers
            .lock()
            .expect("header log")
            .push((path.to_owned(), headers.clone()));
    }

    fn header_for(&self, path: &str, name: &str) -> Option<String> {
        self.headers
            .lock()
            .expect("header log")
            .iter()
            .find(|(seen, _)| seen == path)
            .and_then(|(_, headers)| headers.get(name).cloned())
            .and_then(|value| value.to_str().ok().map(str::to_owned))
    }
}

async fn start_stub(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub server");
    let address = listener.local_addr().expect("stub address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{address}"), handle)
}

fn config(kind: MediaServerKind, url: &str) -> MediaServerConfig {
    MediaServerConfig {
        id: "server-1".to_owned(),
        name: "Stub".to_owned(),
        kind,
        server_url: url.to_owned(),
        api_key: SecretString::new("token-123"),
    }
}

#[tokio::test]
async fn jellyfin_connection_and_libraries_round_trip_over_http() {
    let seen = Seen::default();
    let router = Router::new()
        .route(
            "/System/Info",
            get(|State(seen): State<Seen>, headers: HeaderMap| async move {
                seen.record("/System/Info", &headers);
                axum::Json(serde_json::json!({
                    "ServerName": "Basement Jellyfin",
                    "Version": "10.9.11"
                }))
            }),
        )
        .route(
            "/Library/VirtualFolders",
            get(|State(seen): State<Seen>, headers: HeaderMap| async move {
                seen.record("/Library/VirtualFolders", &headers);
                axum::Json(serde_json::json!([
                    {
                        "Name": "Shows",
                        "ItemId": "lib-1",
                        "CollectionType": "tvshows",
                        "Locations": ["/data/tv"]
                    },
                    {
                        "Name": "Films",
                        "ItemId": "lib-2",
                        "CollectionType": "movies",
                        "Locations": ["/data/movies"]
                    }
                ]))
            }),
        )
        .route(
            "/Items",
            get(|State(seen): State<Seen>, headers: HeaderMap| async move {
                seen.record("/Items", &headers);
                axum::Json(serde_json::json!({"Items":[{
                    "Id":"show-1",
                    "Name":"Example Show",
                    "Type":"Series",
                    "ProductionYear":2024,
                    "Path":"/data/tv/Example Show",
                    "ImageTags":{"Primary":"poster-tag"}
                }]}))
            }),
        )
        .route(
            "/Items/{id}/Images/Primary",
            get(
                |seen: State<Seen>, id: AxumPath<String>, headers: HeaderMap| async move {
                    assert_eq!(id.0, "show-1");
                    seen.record("/Items/show-1/Images/Primary", &headers);
                    ([("content-type", "image/jpeg")], vec![1_u8, 2, 3, 4])
                },
            ),
        )
        .with_state(seen.clone());

    // A trailing slash is what a user typically pastes; URL joining must not
    // produce a doubled separator or drop the path.
    let (base, server) = start_stub(router).await;
    let client = MediaServerDiscoveryClient::new();
    let config = config(MediaServerKind::Jellyfin, &format!("{base}/"));

    let info = client
        .test_connection(&config, CancellationToken::new())
        .await
        .expect("connection");
    assert_eq!(info.server_name.as_deref(), Some("Basement Jellyfin"));
    assert_eq!(info.version.as_deref(), Some("10.9.11"));

    let mappings = [MediaServerPathMapping {
        server_path_prefix: "/data".to_owned(),
        local_path_prefix: std::path::PathBuf::from("/mnt/media"),
    }];
    let libraries = client
        .discover_libraries(&config, &mappings, CancellationToken::new())
        .await
        .expect("libraries");

    assert_eq!(libraries.len(), 2);
    let shows = libraries
        .iter()
        .find(|library| library.name == "Shows")
        .expect("shows library");
    assert_eq!(shows.server_path, "/data/tv");
    // The server-to-local mapping is what makes a remote path usable locally.
    // The joined remainder uses the host separator on purpose: a Linux media
    // server reporting `/data/tv` must become `D:\Media\tv` for a Windows
    // client, so the expected value is built the same way rather than hardcoded.
    assert_eq!(
        shows.local_path.as_deref(),
        Some(std::path::PathBuf::from("/mnt/media").join("tv").as_path())
    );

    // Jellyfin 12 rejects the old X-Emby-Token and X-MediaBrowser-Token
    // mechanisms when legacy authorization is disabled. The standard scheme
    // keeps the key out of URLs and works across supported Jellyfin versions.
    assert_eq!(
        seen.header_for("/Library/VirtualFolders", "authorization")
            .as_deref(),
        Some("MediaBrowser Token=\"token-123\"")
    );
    assert_eq!(
        seen.header_for("/Library/VirtualFolders", "x-emby-token"),
        None
    );
    assert_eq!(
        seen.header_for("/Library/VirtualFolders", "x-mediabrowser-token"),
        None
    );

    let items = client
        .discover_items(
            &config,
            "Shows",
            &[std::path::PathBuf::from("/data/tv")],
            CancellationToken::new(),
        )
        .await
        .expect("catalog items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "Example Show");
    assert!(items[0].has_poster);
    let artwork = client
        .fetch_artwork(&config, "show-1", CancellationToken::new())
        .await
        .expect("poster artwork");
    assert_eq!(artwork.content_type, "image/jpeg");
    assert_eq!(artwork.bytes, vec![1, 2, 3, 4]);

    // The catalog and poster requests carry the same credential as discovery.
    for path in ["/System/Info", "/Items", "/Items/show-1/Images/Primary"] {
        assert_eq!(
            seen.header_for(path, "authorization").as_deref(),
            Some("MediaBrowser Token=\"token-123\""),
            "{path}"
        );
    }
    server.abort();
}

#[tokio::test]
async fn plex_reads_xml_rather_than_json() {
    let seen = Seen::default();
    let router = Router::new()
        .route(
            "/",
            get(|State(seen): State<Seen>, headers: HeaderMap| async move {
                seen.record("/", &headers);
                (
                    [("content-type", "application/xml")],
                    r#"<MediaContainer machineIdentifier="abc123" version="1.40.0"/>"#,
                )
            }),
        )
        .route(
            "/library/sections",
            get(|| async {
                (
                    [("content-type", "application/xml")],
                    r#"<MediaContainer>
                         <Directory key="1" type="show" title="Anime">
                           <Location id="1" path="/srv/anime"/>
                         </Directory>
                       </MediaContainer>"#,
                )
            }),
        )
        .with_state(seen.clone());

    let (base, server) = start_stub(router).await;
    let client = MediaServerDiscoveryClient::new();
    let config = config(MediaServerKind::Plex, &base);

    let info = client
        .test_connection(&config, CancellationToken::new())
        .await
        .expect("connection");
    assert_eq!(info.server_name.as_deref(), Some("abc123"));

    let libraries = client
        .discover_libraries(&config, &[], CancellationToken::new())
        .await
        .expect("libraries");
    assert_eq!(libraries.len(), 1);
    assert_eq!(libraries[0].name, "Anime");
    assert_eq!(libraries[0].server_path, "/srv/anime");

    // Plex uses its own token header rather than the Emby one.
    assert_eq!(
        seen.header_for("/", "x-plex-token").as_deref(),
        Some("token-123")
    );
    server.abort();
}

#[tokio::test]
async fn emby_keeps_its_legacy_token_headers() {
    let seen = Seen::default();
    let router = Router::new()
        .route(
            "/Library/VirtualFolders",
            get(|State(seen): State<Seen>, headers: HeaderMap| async move {
                seen.record("/Library/VirtualFolders", &headers);
                axum::Json(serde_json::json!([]))
            }),
        )
        .with_state(seen.clone());
    let (base, server) = start_stub(router).await;

    MediaServerDiscoveryClient::new()
        .discover_libraries(
            &config(MediaServerKind::Emby, &base),
            &[],
            CancellationToken::new(),
        )
        .await
        .expect("Emby libraries");

    assert_eq!(
        seen.header_for("/Library/VirtualFolders", "x-emby-token")
            .as_deref(),
        Some("token-123")
    );
    assert_eq!(
        seen.header_for("/Library/VirtualFolders", "x-mediabrowser-token")
            .as_deref(),
        Some("token-123")
    );
    assert_eq!(
        seen.header_for("/Library/VirtualFolders", "authorization"),
        None
    );
    server.abort();
}

#[tokio::test]
async fn an_unauthorized_response_is_reported_rather_than_parsed() {
    let router = Router::new().route(
        "/System/Info",
        get(|| async { (StatusCode::UNAUTHORIZED, "nope").into_response() }),
    );
    let (base, server) = start_stub(router).await;

    let error = MediaServerDiscoveryClient::new()
        .test_connection(
            &config(MediaServerKind::Emby, &base),
            CancellationToken::new(),
        )
        .await
        .expect_err("unauthorized");

    // A wrong API key must say so, not surface as a parse failure.
    assert!(error.to_string().contains("401"), "{error}");
    server.abort();
}

#[tokio::test]
async fn a_malformed_body_is_reported_as_an_invalid_response() {
    let router = Router::new().route(
        "/System/Info",
        get(|| async { ([("content-type", "application/json")], "{ not json") }),
    );
    let (base, server) = start_stub(router).await;

    let error = MediaServerDiscoveryClient::new()
        .test_connection(
            &config(MediaServerKind::Jellyfin, &base),
            CancellationToken::new(),
        )
        .await
        .expect_err("invalid body");

    assert!(error.to_string().contains("invalid response"), "{error}");
    server.abort();
}

#[tokio::test]
async fn test_connection_fails_when_the_server_rejects_the_key() {
    let has = |headers: &HeaderMap, name: &str, expected: &str| {
        headers.get(name).and_then(|value| value.to_str().ok()) == Some(expected)
    };
    // The public endpoints answer anyone, as on the real servers.
    let router = Router::new()
        .route(
            "/System/Info/Public",
            get(|| async { axum::Json(serde_json::json!({"ServerName": "Open"})) }),
        )
        .route(
            "/System/Info",
            get(move |headers: HeaderMap| async move {
                if has(&headers, "authorization", "MediaBrowser Token=\"token-123\"")
                    || has(&headers, "x-emby-token", "token-123")
                {
                    axum::Json(serde_json::json!({"ServerName": "Private"})).into_response()
                } else {
                    StatusCode::UNAUTHORIZED.into_response()
                }
            }),
        )
        .route(
            "/identity",
            get(|| async { r#"<MediaContainer machineIdentifier="open"/>"# }),
        )
        .route(
            "/",
            get(move |headers: HeaderMap| async move {
                if has(&headers, "x-plex-token", "token-123") {
                    r#"<MediaContainer machineIdentifier="private"><Directory key="library"/></MediaContainer>"#
                        .into_response()
                } else {
                    StatusCode::UNAUTHORIZED.into_response()
                }
            }),
        );
    let (base, server) = start_stub(router).await;
    let client = MediaServerDiscoveryClient::new();

    for (kind, name) in [
        (MediaServerKind::Jellyfin, "Private"),
        (MediaServerKind::Emby, "Private"),
        (MediaServerKind::Plex, "private"),
    ] {
        let mut config = config(kind, &base);
        let info = client
            .test_connection(&config, CancellationToken::new())
            .await
            .expect("accepted key");
        assert_eq!(info.server_name.as_deref(), Some(name));

        config.api_key = SecretString::new("wrong");
        let error = client
            .test_connection(&config, CancellationToken::new())
            .await
            .expect_err("rejected key");
        assert!(error.to_string().contains("401"), "{kind}: {error}");
    }
    server.abort();
}

/// A Jellyfin library whose `/Items` honours `StartIndex` and `Limit`.
#[derive(Clone)]
struct Catalog {
    item_count: usize,
    reports_total: bool,
    max_page: usize,
    /// How many items of the previous page each later page repeats.
    overlap: usize,
    requests: Arc<Mutex<Vec<(usize, usize)>>>,
    sort_by: Arc<Mutex<Vec<String>>>,
}

impl Catalog {
    fn new(item_count: usize, reports_total: bool, max_page: usize) -> Self {
        Self {
            item_count,
            reports_total,
            max_page,
            overlap: 0,
            requests: Arc::default(),
            sort_by: Arc::default(),
        }
    }

    fn requests(&self) -> Vec<(usize, usize)> {
        self.requests.lock().expect("request log").clone()
    }
}

async fn catalog_page(
    State(catalog): State<Catalog>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> axum::response::Response {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("MediaBrowser Token=\"token-123\"")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let number = |name: &str| {
        query
            .get(name)
            .and_then(|value| value.parse::<usize>().ok())
    };
    let start = number("StartIndex").unwrap_or(0);
    let limit = number("Limit").unwrap_or(usize::MAX);
    catalog
        .requests
        .lock()
        .expect("request log")
        .push((start, limit));
    catalog
        .sort_by
        .lock()
        .expect("sort log")
        .extend(query.get("SortBy").cloned());
    let end = catalog
        .item_count
        .min(start.saturating_add(limit.min(catalog.max_page)));
    let items: Vec<_> = (start.saturating_sub(catalog.overlap)..end)
        .map(|index| {
            serde_json::json!({
                "Id": format!("item-{index}"),
                "Name": format!("Show {index:05}"),
                "Type": "Series",
                "Path": format!("/data/tv/Show {index:05}")
            })
        })
        .collect();
    let mut document = serde_json::json!({ "Items": items });
    if catalog.reports_total {
        document["TotalRecordCount"] = catalog.item_count.into();
    }
    axum::Json(document).into_response()
}

async fn discover_catalog(catalog: &Catalog) -> Vec<DiscoveredItem> {
    let router = Router::new()
        .route(
            "/Library/VirtualFolders",
            get(|| async {
                axum::Json(serde_json::json!([
                    {"Name": "Shows", "ItemId": "lib-1", "Locations": ["/data/tv"]}
                ]))
            }),
        )
        .route("/Items", get(catalog_page))
        .with_state(catalog.clone());
    let (base, server) = start_stub(router).await;

    let items = MediaServerDiscoveryClient::new()
        .discover_items(
            &config(MediaServerKind::Jellyfin, &base),
            "Shows",
            &[std::path::PathBuf::from("/data/tv")],
            CancellationToken::new(),
        )
        .await
        .expect("catalog items");
    server.abort();
    items
}

#[tokio::test]
async fn a_library_larger_than_ten_thousand_items_is_not_truncated() {
    let catalog = Catalog::new(10_001, true, usize::MAX);

    let items = discover_catalog(&catalog).await;

    assert_eq!(items.len(), 10_001);
    assert_eq!(items[10_000].id, "item-10000");
    let requests = catalog.requests();
    assert_eq!(requests.len(), 21);
    assert_eq!(requests[0], (0, 500));
    assert_eq!(requests[20], (10_000, 500));
}

/// Items sharing a name (a remake, a US and a UK version) need a second key, or a
/// page boundary between them can skip one and repeat the other.
#[tokio::test]
async fn pages_are_sorted_past_the_name() {
    let catalog = Catalog::new(10, true, usize::MAX);

    discover_catalog(&catalog).await;

    let sorts = catalog.sort_by.lock().expect("sort log").clone();
    assert_eq!(sorts, ["SortName,DateCreated"]);
}

/// A tie the server orders differently on the next request shows up as an item repeated
/// across a page boundary.
#[tokio::test]
async fn an_item_repeated_across_pages_is_listed_once() {
    let catalog = Catalog {
        overlap: 1,
        ..Catalog::new(1_000, true, usize::MAX)
    };

    let items = discover_catalog(&catalog).await;

    assert_eq!(items.len(), 1_000);
    let ids: std::collections::HashSet<_> = items.iter().map(|item| item.id.clone()).collect();
    assert_eq!(ids.len(), 1_000);
}

#[tokio::test]
async fn paging_stops_at_the_reported_total_without_an_extra_request() {
    let catalog = Catalog::new(1_000, true, usize::MAX);

    assert_eq!(discover_catalog(&catalog).await.len(), 1_000);
    assert_eq!(catalog.requests(), [(0, 500), (500, 500)]);
}

#[tokio::test]
async fn paging_without_a_total_stops_on_a_short_or_empty_page() {
    let short = Catalog::new(700, false, usize::MAX);
    assert_eq!(discover_catalog(&short).await.len(), 700);
    assert_eq!(short.requests(), [(0, 500), (500, 500)]);

    let exact = Catalog::new(1_000, false, usize::MAX);
    assert_eq!(discover_catalog(&exact).await.len(), 1_000);
    assert_eq!(exact.requests(), [(0, 500), (500, 500), (1_000, 500)]);
}

#[tokio::test]
async fn paging_follows_the_total_when_the_server_returns_smaller_pages() {
    let catalog = Catalog::new(450, true, 200);

    assert_eq!(discover_catalog(&catalog).await.len(), 450);
    assert_eq!(catalog.requests(), [(0, 500), (200, 500), (400, 500)]);
}

/// Answers one request with `head`, then either streams zeroes forever or stalls.
async fn start_poster_stub(
    head: &'static str,
    endless_body: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub server");
    let address = listener.local_addr().expect("stub address");
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = [0_u8; 4096];
        let _ = socket.read(&mut request).await;
        let _ = socket.write_all(head.as_bytes()).await;
        // One 64 KiB chunk in chunked transfer encoding.
        let mut chunk = b"10000\r\n".to_vec();
        chunk.resize(chunk.len() + 0x10000, 0);
        chunk.extend_from_slice(b"\r\n");
        while endless_body && socket.write_all(&chunk).await.is_ok() {}
        std::future::pending::<()>().await;
    });
    (format!("http://{address}"), handle)
}

async fn oversized_poster_error(head: &'static str, endless_body: bool) -> String {
    let (base, server) = start_poster_stub(head, endless_body).await;
    let client = MediaServerDiscoveryClient::new();
    let config = config(MediaServerKind::Jellyfin, &base);
    // Half the client's own 20 s timeout, which is what used to end these downloads.
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        client.fetch_artwork(&config, "show-1", CancellationToken::new()),
    )
    .await
    .expect("an oversized poster must be rejected before it is downloaded");
    server.abort();
    result.expect_err("oversized poster").to_string()
}

#[tokio::test]
async fn an_oversized_poster_is_rejected_while_it_streams() {
    let error = oversized_poster_error(
        "HTTP/1.1 200 OK\r\ncontent-type: image/jpeg\r\ntransfer-encoding: chunked\r\n\r\n",
        true,
    )
    .await;

    assert!(error.contains("10 MiB limit"), "{error}");
}

#[tokio::test]
async fn an_oversized_poster_is_rejected_from_its_content_length() {
    // One byte over the limit is announced and the body never arrives.
    let error = oversized_poster_error(
        "HTTP/1.1 200 OK\r\ncontent-type: image/jpeg\r\ncontent-length: 10485761\r\n\r\n",
        false,
    )
    .await;

    assert!(error.contains("10 MiB limit"), "{error}");
}
