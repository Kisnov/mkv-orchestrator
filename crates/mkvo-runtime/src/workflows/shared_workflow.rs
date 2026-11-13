use super::*;

impl MkvoRuntime {
    pub(super) async fn run_library_audit_impl(
        &self,
        request: LibraryAuditRequest,
    ) -> RuntimeResult<LibraryAuditResponse> {
        let files = if request.paths.is_empty() {
            self.resolve_rows(&request.files, &[]).await?
        } else {
            let requested: BTreeSet<_> = request.paths.iter().map(|path| path_key(path)).collect();
            let mut found = self.working_set_by_key(file_key, &requested).await;
            // A Library scan leaves the working set alone; its files are with its job.
            if found.len() < requested.len()
                && let Some(overview) = self.overview_files().await
            {
                for file in overview.iter() {
                    let key = file_key(&file.path);
                    if requested.contains(&key) {
                        found.entry(key).or_insert_with(|| file.clone());
                    }
                }
            }
            let mut files: Vec<_> = found.into_values().collect();
            files.sort_by(|left, right| left.path.cmp(&right.path));
            files
        };
        let audit = LibraryAuditService.build(&self.config().media_root, &files, &[]);
        let items = audit
            .groups
            .iter()
            .map(|group| LibraryAuditRow {
                folder_path: display_path(&self.config().media_root.join(&group.relative_folder)),
                folder_name: if group.season_folder.is_empty() {
                    group.show_name.clone()
                } else {
                    format!("{} / {}", group.show_name, group.season_folder)
                },
                file_count: group.all_file_paths.len(),
                standard_video: group.standard.video.clone(),
                standard_audio: group.standard.audio.clone(),
                standard_subtitles: group.standard.subtitles.clone(),
                template_file_path: group
                    .template_file_path
                    .as_deref()
                    .map_or_else(String::new, display_path),
                template_file_name: group
                    .template_file_path
                    .as_deref()
                    .map_or_else(String::new, file_name),
                has_issues: group.has_issues(),
                issue_summary: if group.issues.is_empty() {
                    "Standard".to_owned()
                } else {
                    format!("{} issue(s)", group.issues.len())
                },
                issues: group
                    .issues
                    .iter()
                    .map(|issue| issue.message.clone())
                    .collect(),
                issue_file_paths: group
                    .issue_file_paths
                    .iter()
                    .map(|path| display_path(path))
                    .collect(),
                all_file_paths: group
                    .all_file_paths
                    .iter()
                    .map(|path| display_path(path))
                    .collect(),
            })
            .collect();
        Ok(LibraryAuditResponse {
            summary: LibraryAuditSummary {
                groups: audit.summary.season_folders,
                files: audit.summary.files,
                issue_groups: audit.summary.issue_groups,
                standard_groups: audit
                    .summary
                    .season_folders
                    .saturating_sub(audit.summary.issue_groups),
            },
            items,
        })
    }

    /// The rows a request selected. Selecting nothing is an error, never
    /// "every file".
    pub(super) async fn resolve_selected_rows(
        &self,
        rows: &[mkvo_contracts::MediaFileRow],
        selected_paths: &[String],
    ) -> RuntimeResult<Vec<MediaFile>> {
        if selected_paths.is_empty() {
            return Err(RuntimeError::invalid("No files selected."));
        }
        self.resolve_rows(rows, selected_paths).await
    }

    /// Every row, for requests that carry no selection: pass `&[]`.
    pub(super) async fn resolve_rows(
        &self,
        rows: &[mkvo_contracts::MediaFileRow],
        selected_paths: &[String],
    ) -> RuntimeResult<Vec<MediaFile>> {
        let selected: BTreeSet<_> = selected_paths.iter().map(|path| path_key(path)).collect();
        let source_rows: Vec<_> = rows
            .iter()
            .filter(|row| selected.is_empty() || selected.contains(&path_key(&row.path)))
            .collect();
        let wanted: BTreeSet<_> = source_rows.iter().map(|row| path_key(&row.path)).collect();
        let current = self.working_set_by_key(file_key, &wanted).await;
        let mut files = Vec::with_capacity(source_rows.len());
        for row in source_rows {
            if let Some(file) = current.get(&path_key(&row.path)) {
                // The row is the track layout the user is looking at. Keep
                // the runtime's fingerprint and rich file metadata, but plan
                // track operations against the exact IDs displayed in the UI
                // so a stale cached domain projection cannot target a
                // different track or incorrectly report no change.
                let mut resolved = file.clone();
                resolved.tracks = tracks_from_row(row);
                files.push(resolved);
                continue;
            }
            let path = PathBuf::from(&row.path);
            let fingerprint = self.dependencies().file_system.fingerprint(&path).await?;
            files.push(media_from_row(row, fingerprint));
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        files.dedup_by(|left, right| same_path(&left.path, &right.path));
        Ok(files)
    }

    pub(super) async fn persist_plan<T: Serialize>(
        &self,
        plan: &OperationPlan<T>,
    ) -> RuntimeResult<()> {
        let stored = StoredPlan::try_from(plan)?;
        self.dependencies().plans.save(&stored).await?;
        Ok(())
    }

    pub(super) async fn load_valid_plan<T: DeserializeOwned + Serialize>(
        &self,
        id: PlanId,
        fingerprint: &str,
        key: &IdempotencyKey,
    ) -> RuntimeResult<OperationPlan<T>> {
        let plan = self.load_referenced_plan(id, fingerprint, key).await?;
        let settings = self.dependencies().settings.load().await?.0;
        let actual_settings = stable_fingerprint(&settings)
            .map_err(|error| RuntimeError::internal(error.to_string()))?;
        if plan.context.settings_fingerprint != actual_settings {
            return Err(RuntimeError::new(
                mkvo_contracts::ApiErrorCode::PlanStale,
                "settings changed after the preview was created",
            ));
        }
        for (name, expected) in &plan.context.tool_fingerprints {
            let actual = self.dependencies().tools.status(name).await?;
            if !actual.available
                || !same_path(&expected.executable, Path::new(&actual.resolved_path))
                || (!expected.version.is_empty() && expected.version != actual.version)
            {
                return Err(RuntimeError::new(
                    mkvo_contracts::ApiErrorCode::PlanStale,
                    format!("tool `{name}` changed or became unavailable after preview"),
                ));
            }
        }
        for expected in &plan.context.input_fingerprints {
            self.revalidate_fingerprint(expected).await?;
        }
        Ok(plan)
    }

    pub(super) async fn load_referenced_plan<T: DeserializeOwned + Serialize>(
        &self,
        id: PlanId,
        fingerprint: &str,
        key: &IdempotencyKey,
    ) -> RuntimeResult<OperationPlan<T>> {
        let stored = self
            .dependencies()
            .plans
            .get(id)
            .await?
            .ok_or_else(|| RuntimeError::not_found(format!("operation plan {id}")))?;
        if stored.metadata.fingerprint != fingerprint {
            return Err(RuntimeError::new(
                mkvo_contracts::ApiErrorCode::PlanTampered,
                "plan fingerprint does not match the stored immutable plan",
            ));
        }
        if &stored.metadata.idempotency_key != key {
            return Err(RuntimeError::new(
                mkvo_contracts::ApiErrorCode::PlanTampered,
                "idempotency key does not belong to this plan",
            ));
        }
        let plan: OperationPlan<T> = serde_json::from_value(serde_json::to_value(stored)?)?;
        plan.validate_integrity(Utc::now())
            .map_err(mkvo_application::ApplicationError::from)?;
        Ok(plan)
    }

    pub(super) async fn revalidate_fingerprint(
        &self,
        expected: &FileFingerprint,
    ) -> RuntimeResult<()> {
        let actual = self
            .dependencies()
            .file_system
            .fingerprint(&expected.path)
            .await?;
        if !expected.matches(&actual) {
            return Err(RuntimeError::new(
                mkvo_contracts::ApiErrorCode::PlanStale,
                format!("input changed after preview: {}", expected.path.display()),
            ));
        }
        Ok(())
    }

    pub(super) async fn current_existing_paths(&self) -> BTreeSet<PathBuf> {
        self.working_set_paths().await
    }

    pub(super) fn authorized_root_paths(&self) -> Vec<PathBuf> {
        if let Some(roots) = self.dependencies().authorized_roots.as_ref() {
            let roots: Vec<PathBuf> = roots
                .roots()
                .into_iter()
                .map(|root| root.path().to_path_buf())
                .collect();
            if !roots.is_empty() {
                return roots;
            }
        }
        self.config()
            .authorized_roots
            .iter()
            .map(|(path, _)| path.clone())
            .collect()
    }

    pub(super) async fn tool_fingerprints(&self) -> RuntimeResult<ToolFingerprints> {
        let statuses = self.dependencies().tools.all_statuses().await?;
        Ok(statuses
            .into_iter()
            .filter(|status| status.available)
            .map(|status| {
                (
                    status.name.clone(),
                    ToolFingerprint {
                        name: status.name,
                        executable: PathBuf::from(status.resolved_path),
                        version: status.version,
                    },
                )
            })
            .collect())
    }

    pub(super) async fn append_log(
        &self,
        area: &str,
        message: &str,
        detail: &str,
    ) -> RuntimeResult<()> {
        self.dependencies()
            .logs
            .append(&OperationLogEntry {
                timestamp_utc: Utc::now(),
                correlation_id: mkvo_domain::CorrelationId::new(),
                area: area.to_owned(),
                level: LogLevel::Information,
                message: message.to_owned(),
                detail: detail.to_owned(),
            })
            .await?;
        Ok(())
    }

    pub(super) async fn validate_plan_reference(
        &self,
        id: PlanId,
        fingerprint: &str,
        key: &IdempotencyKey,
    ) -> RuntimeResult<String> {
        let stored = self
            .dependencies()
            .plans
            .get(id)
            .await?
            .ok_or_else(|| RuntimeError::not_found(format!("operation plan {id}")))?;
        if stored.metadata.fingerprint != fingerprint || &stored.metadata.idempotency_key != key {
            return Err(RuntimeError::new(
                mkvo_contracts::ApiErrorCode::PlanTampered,
                "plan reference, fingerprint, and idempotency key must match",
            ));
        }
        Ok(stored.metadata.request_fingerprint)
    }

    pub(super) async fn begin_journal(
        &self,
        plan_id: PlanId,
        key: &IdempotencyKey,
        resources: &[mkvo_domain::ResourceClaim],
        items: impl IntoIterator<Item = String>,
    ) -> Result<mkvo_application::JournalRecord, ApplicationError> {
        let mut record = mkvo_application::JournalRecord {
            idempotency_key: key.clone(),
            plan_id,
            step: 0,
            status: mkvo_application::JournalStatus::Prepared,
            resources: resources.to_vec(),
            items: items
                .into_iter()
                .map(|key| mkvo_application::JournalItemOutcome {
                    key,
                    status: mkvo_application::JournalItemStatus::Pending,
                    detail: None,
                })
                .collect(),
            detail: None,
            updated_utc: Utc::now(),
        };
        self.dependencies().journal.begin(&record).await?;
        record.status = mkvo_application::JournalStatus::Running;
        self.dependencies().journal.advance(&record).await?;
        Ok(record)
    }

    pub(super) async fn tool_path(&self, name: &str) -> Result<PathBuf, ApplicationError> {
        let status = self.dependencies().tools.status(name).await?;
        if !status.available || status.resolved_path.trim().is_empty() {
            return Err(ApplicationError::InvalidRequest(format!(
                "required tool `{name}` is unavailable"
            )));
        }
        Ok(PathBuf::from(status.resolved_path))
    }
}

/// `path_key` of a scanned file, so it matches the key of a row's path.
fn file_key(path: &Path) -> String {
    path_key(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::MkvoRuntimeBuilder;

    /// A request naming one real MKV in `files`, with `selected` as its selection.
    fn request(library: &Path, selected: &[&str]) -> serde_json::Value {
        let path = display_path(&library.join("episode.mkv"));
        json!({
            "files": [{
                "path": path, "fileName": "episode.mkv", "extension": ".mkv",
                "status": "Scanned", "reader": "mkvmerge", "codec": "", "resolution": "",
                "bitDepth": "", "hdr": "", "videoSummary": "", "audioSummary": "",
                "subtitleSummary": "", "attachmentSummary": "",
            }],
            "selectedPaths": selected,
            "removeUnwantedAudioLanguages": false, "keepAudioLanguages": "",
            "removeUnwantedSubtitleLanguages": false, "keepSubtitleLanguages": "",
            "removeUnwantedTrackIds": false, "removeTrackIdsText": "",
            "preserveChapters": true, "preserveAttachments": true,
            "muxMatchingExternalSubtitles": false, "externalSubtitleLanguage": "und",
            "externalSubtitleFormats": "", "preserveExternalSubtitleFiles": true,
            "skipMuxIfSubtitleAlreadyExists": false, "extractSubtitles": false,
            "extractSubtitleLanguages": "", "extractOverwriteExistingFiles": false,
            "convertMp4ToMkv": false, "deleteMp4AfterConvert": false,
            "containerTitleMode": "remove", "customContainerTitle": "",
            "videoTitleMode": "keep", "customVideoTitle": "",
            "selectedDefaultAudio": "", "selectedForcedAudio": "",
            "selectedDefaultSubtitle": "", "selectedForcedSubtitle": "",
        })
    }

    /// An empty selection used to mean every file in the request.
    #[tokio::test]
    async fn a_preview_with_nothing_selected_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let library = directory.path().join("library");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(library.join("episode.mkv"), b"mkv").unwrap();
        let runtime = MkvoRuntimeBuilder::new(&library, directory.path().join("config"))
            .disable_legacy_migration()
            .authorized_root(&library, true)
            .build()
            .unwrap();
        let nothing = request(&library, &[]);

        let mux = runtime
            .build_mux_preview(serde_json::from_value(nothing.clone()).unwrap())
            .await;
        let propedit = runtime
            .build_propedit_preview(serde_json::from_value(nothing).unwrap())
            .await;

        for error in [mux.unwrap_err(), propedit.unwrap_err()] {
            assert_eq!(error.code, mkvo_contracts::ApiErrorCode::InvalidRequest);
            assert_eq!(error.message, "No files selected.");
        }
    }
}
