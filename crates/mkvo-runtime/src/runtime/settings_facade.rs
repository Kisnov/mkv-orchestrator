use std::path::PathBuf;

use mkvo_contracts::{WebSettings, WebSettingsRequest};

use super::{MkvoRuntime, apply_web_settings_request, validate_media_server_url, web_settings};
use crate::{BrowseScope, RuntimeError, RuntimeResult};

impl MkvoRuntime {
    pub async fn get_web_settings(&self) -> RuntimeResult<WebSettings> {
        let response = self.settings_service().load().await?;
        self.web_settings_with_secret_aliases(&response).await
    }

    pub async fn save_web_settings(
        &self,
        request: WebSettingsRequest,
    ) -> RuntimeResult<WebSettings> {
        let loaded = self.settings_service().load().await?;
        let mut settings = loaded.settings;
        // A browser could otherwise point the tools at a share it can write to and
        // have a planted executable run at the next scan; the desktop user owns the machine.
        if self.inner.config.browse_scope != BrowseScope::Unrestricted {
            for (requested, current) in [
                (
                    &request.mkv_tool_nix_directory,
                    &settings.tools.mkvtoolnix_directory,
                ),
                (&request.ffmpeg_directory, &settings.tools.ffmpeg_directory),
            ] {
                let Some(path) = requested
                    .as_ref()
                    .and_then(Option::as_ref)
                    .map(|value| PathBuf::from(value.trim()))
                    .filter(|path| !path.as_os_str().is_empty())
                else {
                    continue;
                };
                if current.as_ref() != Some(&path)
                    && !self.inner.config.tool_directories.contains(&path)
                {
                    return Err(RuntimeError::invalid(format!(
                        "{} is not an allowed tool directory; on the web server tool directories must be listed in MKVO_TOOL_DIRECTORIES",
                        path.display()
                    )));
                }
            }
        }
        let mut secrets = Vec::new();
        apply_web_settings_request(&mut settings, request, &mut secrets)?;
        self.validate_configured_roots(&settings).await?;
        for server in &settings.media_servers {
            validate_media_server_url(&server.server_url)?;
        }
        let response = self
            .settings_service()
            .save(mkvo_contracts::SaveSettingsRequest {
                settings,
                secrets,
                expected_revision: Some(loaded.revision),
            })
            .await?;
        self.authorize_configured_roots(&response.settings);
        if let Some(registry) = self.inner.dependencies.process_tools.as_ref() {
            let (explicit, directories) = crate::composition::tool_configuration(
                &response.settings,
                self.inner.config.tool_directories.iter().cloned(),
            );
            registry.reconfigure(explicit, directories);
        }
        self.web_settings_with_secret_aliases(&response).await
    }

    async fn web_settings_with_secret_aliases(
        &self,
        response: &mkvo_contracts::SettingsResponse,
    ) -> RuntimeResult<WebSettings> {
        let mut view = web_settings(&response.settings, &response.secret_status);
        view.has_tvdb_api_key = self
            .secret_alias(&["provider.tvdb.api_key", "tvdbApiKey"])
            .await?
            .is_some();
        view.has_tvdb_pin = self
            .secret_alias(&["provider.tvdb.pin", "tvdbPin"])
            .await?
            .is_some();
        view.has_tmdb_api_key = self
            .secret_alias(&["provider.tmdb.api_key", "tmdbApiKey"])
            .await?
            .is_some();
        view.has_anidb_client = self
            .secret_alias(&["provider.anidb.client", "anidbClient"])
            .await?
            .is_some();
        Ok(view)
    }
}

#[cfg(test)]
mod tests {
    use mkvo_contracts::{ApiErrorCode, WebSettingsRequest};

    use crate::MkvoRuntimeBuilder;

    fn tools(mkvtoolnix: Option<&str>, ffmpeg: Option<&str>) -> WebSettingsRequest {
        WebSettingsRequest {
            mkv_tool_nix_directory: Some(mkvtoolnix.map(str::to_owned)),
            ffmpeg_directory: Some(ffmpeg.map(str::to_owned)),
            ..WebSettingsRequest::default()
        }
    }

    #[tokio::test]
    async fn web_hosts_only_accept_tool_directories_they_were_given() {
        let directory = tempfile::tempdir().unwrap();
        let media = directory.path().join("media");
        std::fs::create_dir_all(&media).unwrap();
        let fixed = directory.path().join("tools");
        let planted = media.join("bin");
        let web = MkvoRuntimeBuilder::new(&media, directory.path().join("web"))
            .disable_legacy_migration()
            .authorized_root(&media, true)
            .tool_directory(&fixed)
            .build()
            .unwrap();
        let error = web
            .save_web_settings(tools(Some(&planted.to_string_lossy()), None))
            .await
            .unwrap_err();
        assert_eq!(error.code, ApiErrorCode::InvalidRequest);
        assert!(
            error.message.contains("MKVO_TOOL_DIRECTORIES"),
            "{}",
            error.message
        );
        let error = web
            .save_web_settings(tools(None, Some(&planted.to_string_lossy())))
            .await
            .unwrap_err();
        assert_eq!(error.code, ApiErrorCode::InvalidRequest);
        assert!(
            web.get_web_settings()
                .await
                .unwrap()
                .mkv_tool_nix_directory
                .is_none()
        );

        let fixed_text = fixed.to_string_lossy().into_owned();
        let saved = web
            .save_web_settings(tools(Some(&fixed_text), Some(&fixed_text)))
            .await
            .unwrap();
        assert_eq!(saved.mkv_tool_nix_directory.as_deref(), Some(&*fixed_text));
        // The form sends every field back on each save, and clearing is always allowed.
        web.save_web_settings(tools(Some(&fixed_text), None))
            .await
            .unwrap();

        let desktop = MkvoRuntimeBuilder::new(&media, directory.path().join("desktop"))
            .disable_legacy_migration()
            .unrestricted_browsing()
            .build()
            .unwrap();
        let saved = desktop
            .save_web_settings(tools(Some(&planted.to_string_lossy()), None))
            .await
            .unwrap();
        assert_eq!(
            saved.mkv_tool_nix_directory.as_deref(),
            Some(&*planted.to_string_lossy())
        );
    }
}
