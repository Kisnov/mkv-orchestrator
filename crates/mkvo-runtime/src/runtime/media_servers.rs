use mkvo_domain::MediaServerKind;
use url::Url;

use crate::{RuntimeError, RuntimeResult};

pub(super) fn parse_server_kind(value: &str) -> RuntimeResult<MediaServerKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "emby" => Ok(MediaServerKind::Emby),
        "jellyfin" => Ok(MediaServerKind::Jellyfin),
        "plex" => Ok(MediaServerKind::Plex),
        _ => Err(RuntimeError::invalid(format!(
            "unknown media server type: {value}"
        ))),
    }
}

pub(super) fn validate_media_server_url(value: &str) -> RuntimeResult<Url> {
    let url = Url::parse(value.trim())
        .map_err(|error| RuntimeError::invalid(format!("invalid media server URL: {error}")))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(RuntimeError::invalid(
            "media server URLs require HTTP(S), a host, and no credentials, query, or fragment",
        ));
    }
    // Link-local is where cloud metadata services live; LAN addresses stay allowed.
    let link_local = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_link_local(),
        Some(url::Host::Ipv6(ip)) => {
            ip.is_unicast_link_local() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_link_local())
        }
        _ => false,
    };
    if link_local {
        return Err(RuntimeError::invalid(
            "media server URLs cannot use link-local addresses (169.254.0.0/16, fe80::/10)",
        ));
    }
    Ok(url)
}

pub(super) fn media_server_urls_equivalent(left: &str, right: &str) -> bool {
    let (Ok(left), Ok(right)) = (
        validate_media_server_url(left),
        validate_media_server_url(right),
    ) else {
        return false;
    };
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && left
            .host_str()
            .zip(right.host_str())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.port_or_known_default() == right.port_or_known_default()
        && left.path().trim_end_matches('/') == right.path().trim_end_matches('/')
}

pub(super) fn server_kind_name(value: MediaServerKind) -> &'static str {
    match value {
        MediaServerKind::Emby => "emby",
        MediaServerKind::Jellyfin => "jellyfin",
        MediaServerKind::Plex => "plex",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_server_urls_stay_on_http_and_off_link_local_addresses() {
        for allowed in [
            "http://192.168.1.10:8096",
            "http://10.0.0.5/jellyfin",
            "http://172.17.0.1:8096",
            "http://localhost:8096",
            "http://jellyfin:8096",
            "https://media.example.com",
            "http://[fd00::10]:8096",
        ] {
            assert!(validate_media_server_url(allowed).is_ok(), "{allowed}");
        }
        for refused in [
            "http://169.254.169.254/latest/meta-data",
            "http://2852039166/",
            "http://[fe80::1]:8096",
            "http://[::ffff:169.254.169.254]/",
            "file:///config/secrets.json",
            "gopher://192.168.1.10:70",
            "ftp://192.168.1.10",
        ] {
            assert!(validate_media_server_url(refused).is_err(), "{refused}");
        }
    }
}
