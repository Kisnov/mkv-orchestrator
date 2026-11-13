use std::path::PathBuf;

use async_trait::async_trait;
use mkvo_application::{
    MediaServerArtwork, MediaServerCatalogItem, MediaServerClient as MediaServerPort,
    MediaServerConnection, PortError,
};
use mkvo_domain::{MediaServerKind as DomainServerKind, MediaServerLibrary as DomainLibrary};
use tokio_util::sync::CancellationToken;

use crate::{
    MediaServerClient, MediaServerConfig, MediaServerDiscoveryClient, MediaServerError,
    MediaServerKind, MediaServerPathMapping, SecretString,
};

#[derive(Debug, Clone)]
pub struct ConfiguredMediaServerClient {
    kind: DomainServerKind,
    client: MediaServerDiscoveryClient,
    mappings: Vec<MediaServerPathMapping>,
}

impl ConfiguredMediaServerClient {
    pub fn new(
        kind: DomainServerKind,
        client: MediaServerDiscoveryClient,
        mappings: Vec<MediaServerPathMapping>,
    ) -> Self {
        Self {
            kind,
            client,
            mappings,
        }
    }
}

#[async_trait]
impl MediaServerPort for ConfiguredMediaServerClient {
    fn kind(&self) -> DomainServerKind {
        self.kind
    }

    async fn test(
        &self,
        connection: &MediaServerConnection,
        cancel: CancellationToken,
    ) -> Result<(), PortError> {
        let config = self.config(connection)?;
        self.client
            .test_connection(&config, cancel)
            .await
            .map(|_| ())
            .map_err(server_port_error)
    }

    async fn discover_libraries(
        &self,
        connection: &MediaServerConnection,
        cancel: CancellationToken,
    ) -> Result<Vec<DomainLibrary>, PortError> {
        let config = self.config(connection)?;
        self.client
            .discover_libraries(&config, &self.mappings, cancel)
            .await
            .map(|libraries| {
                libraries
                    .into_iter()
                    .map(|library| DomainLibrary {
                        id: library.id,
                        name: library.name,
                        media_type: library.collection_type,
                        server_path: PathBuf::from(library.server_path),
                        local_path: library.local_path,
                        enabled: library.enabled,
                    })
                    .collect()
            })
            .map_err(server_port_error)
    }

    async fn discover_items(
        &self,
        connection: &MediaServerConnection,
        library_name: &str,
        server_paths: &[PathBuf],
        cancel: CancellationToken,
    ) -> Result<Vec<MediaServerCatalogItem>, PortError> {
        let config = self.config(connection)?;
        self.client
            .discover_items(&config, library_name, server_paths, cancel)
            .await
            .map(|items| {
                items
                    .into_iter()
                    .map(|item| MediaServerCatalogItem {
                        id: item.id,
                        title: item.title,
                        year: item.year,
                        media_type: item.media_type,
                        has_poster: item.has_poster,
                    })
                    .collect()
            })
            .map_err(server_port_error)
    }

    async fn fetch_artwork(
        &self,
        connection: &MediaServerConnection,
        item_id: &str,
        cancel: CancellationToken,
    ) -> Result<MediaServerArtwork, PortError> {
        let config = self.config(connection)?;
        self.client
            .fetch_artwork(&config, item_id, cancel)
            .await
            .map(|artwork| MediaServerArtwork {
                content_type: artwork.content_type,
                bytes: artwork.bytes,
            })
            .map_err(server_port_error)
    }
}

impl ConfiguredMediaServerClient {
    fn config(&self, connection: &MediaServerConnection) -> Result<MediaServerConfig, PortError> {
        if connection.kind != self.kind {
            return Err(PortError::InvalidData(format!(
                "media-server client {:?} cannot handle {:?}",
                self.kind, connection.kind
            )));
        }
        Ok(MediaServerConfig {
            id: format!("{:?}", self.kind).to_ascii_lowercase(),
            name: format!("{:?}", self.kind),
            kind: local_kind(self.kind),
            server_url: connection.base_url.clone(),
            api_key: SecretString::new(&connection.credential),
        })
    }
}

const fn local_kind(kind: DomainServerKind) -> MediaServerKind {
    match kind {
        DomainServerKind::Emby => MediaServerKind::Emby,
        DomainServerKind::Jellyfin => MediaServerKind::Jellyfin,
        DomainServerKind::Plex => MediaServerKind::Plex,
    }
}

fn server_port_error(error: MediaServerError) -> PortError {
    match error {
        MediaServerError::Cancelled(_) => PortError::Canceled,
        MediaServerError::Network { .. } => PortError::unavailable(error.to_string(), true),
        MediaServerError::Status { status, .. }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() =>
        {
            PortError::unavailable(error.to_string(), true)
        }
        MediaServerError::Status {
            status: reqwest::StatusCode::UNAUTHORIZED,
            ..
        } => PortError::CredentialRejected(error.to_string()),
        MediaServerError::Status {
            status: reqwest::StatusCode::FORBIDDEN,
            ..
        } => PortError::CredentialInsufficient(error.to_string()),
        MediaServerError::Status { .. } => PortError::unavailable(error.to_string(), false),
        error => PortError::InvalidData(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_conversion_is_total() {
        assert_eq!(local_kind(DomainServerKind::Emby), MediaServerKind::Emby);
        assert_eq!(
            local_kind(DomainServerKind::Jellyfin),
            MediaServerKind::Jellyfin
        );
        assert_eq!(local_kind(DomainServerKind::Plex), MediaServerKind::Plex);
    }

    #[test]
    fn a_refused_key_is_not_reported_as_an_unavailable_server() {
        let status = |status| MediaServerError::Status {
            kind: MediaServerKind::Jellyfin,
            status,
            path: "Items/show-1/Images/Primary".to_owned(),
        };
        assert_eq!(
            server_port_error(status(reqwest::StatusCode::UNAUTHORIZED)),
            PortError::CredentialRejected(
                "Jellyfin returned HTTP 401 Unauthorized for Items/show-1/Images/Primary"
                    .to_owned()
            )
        );
        // The key was accepted; it just may not do this. That is not a bad key.
        assert_eq!(
            server_port_error(status(reqwest::StatusCode::FORBIDDEN)),
            PortError::CredentialInsufficient(
                "Jellyfin returned HTTP 403 Forbidden for Items/show-1/Images/Primary".to_owned()
            )
        );
        assert!(matches!(
            server_port_error(status(reqwest::StatusCode::NOT_FOUND)),
            PortError::Unavailable {
                retryable: false,
                ..
            }
        ));
    }
}
