use mkvo_application::PortError;

use super::*;

/// The fingerprint of a batch entry's file under its new name.
fn renamed(entry: &RenameBatchEntry) -> FileFingerprint {
    entry
        .renamed_fingerprint
        .clone()
        .unwrap_or_else(|| FileFingerprint {
            path: entry.renamed_path.clone(),
            ..entry.original_fingerprint.clone()
        })
}

impl MkvoRuntime {
    pub(super) async fn build_rename_preview_impl(
        &self,
        request: RenamePreviewRequest,
    ) -> RuntimeResult<RenamePreviewResponse> {
        let mut files = self.resolve_rows(&request.files, &[]).await?;
        if files.is_empty() {
            return Err(RuntimeError::invalid(
                "Scan and select files before building a rename preview.",
            ));
        }
        let provider = request
            .provider
            .as_deref()
            .map(parse_provider)
            .transpose()?
            .unwrap_or(MetadataProvider::Tvdb);
        let episodes = self
            .load_rename_episodes(
                provider,
                &request.selected_result.media_id(),
                &request.selected_result.name,
                request.language.as_deref(),
            )
            .await?;
        let selected_seasons = selected_seasons(&request.scope_keys);
        let is_movie = request.selected_result.format.eq_ignore_ascii_case("movie");
        if !is_movie {
            files.sort_by(|left, right| {
                mkvo_domain::natural_compare(
                    &left.path.to_string_lossy(),
                    &right.path.to_string_lossy(),
                )
            });
        }
        let ordered_episodes = episodes
            .iter()
            .filter(|episode| {
                selected_seasons.is_empty() || selected_seasons.contains(&episode.season)
            })
            .cloned()
            .collect::<Vec<_>>();
        let ordered_matches = if is_movie {
            Vec::new()
        } else {
            match_by_list_order(&ordered_episodes, files.len())
        };
        let series_title = if !is_movie && !request.custom_series_title.trim().is_empty() {
            request.custom_series_title.trim()
        } else {
            request.selected_result.name.as_str()
        };
        for (index, file) in files.iter_mut().enumerate() {
            // A film has no episode number to parse out of its name, so
            // requiring one left every movie unmatched and every token empty --
            // the rename came out as the bare punctuation of the template. The
            // provider returns exactly one entry for a film, and that is the
            // match.
            let matched = if is_movie {
                episodes.first()
            } else if mkvo_application::parse_season_episode(&file.file_name()).is_some() {
                match_episode_for_file(&file.file_name(), &episodes, &selected_seasons)
            } else {
                ordered_matches
                    .get(index)
                    .map(|matched| &matched.episode)
                    .or_else(|| {
                        match_episode_for_file(&file.file_name(), &episodes, &selected_seasons)
                    })
            };
            if let Some(episode) = matched {
                file.episode = Some(EpisodeIdentity {
                    series_title: Some(series_title.to_owned()),
                    season: Some(episode.season),
                    episode: Some(episode.episode),
                    absolute_episode: episode.absolute_episode,
                    episode_title: Some(episode.title.clone()),
                    year: request.selected_result.year.parse().ok(),
                    is_movie: request.selected_result.format.eq_ignore_ascii_case("movie"),
                });
                file.provider_match = Some(ProviderMatch {
                    provider,
                    media_id: request.selected_result.media_id(),
                    episode_id: Some(episode.id.clone()),
                    title: series_title.to_owned(),
                    episode_title: Some(episode.title.clone()),
                    confidence: Some(100),
                });
            }
        }
        let settings = self.dependencies().settings.load().await?.0;
        let settings_fingerprint = stable_fingerprint(&settings)
            .map_err(|error| RuntimeError::internal(error.to_string()))?;
        let key = request
            .idempotency_key
            .unwrap_or_else(IdempotencyKey::generate);
        let files_for_access = files.clone();
        let plan = RenamePlanner.build_plan(RenamePlanRequest {
            files,
            template: request
                .template
                .unwrap_or_else(|| settings.rename.template.clone()),
            provider: Some(provider),
            check_existing_files: true,
            existing_paths: self.existing_paths_around(&files_for_access).await,
            source_access: self
                .probe_source_access(&files_for_access, RequiredAccess::ReadWrite)
                .await,
            existing_parents: self.existing_parents(&files_for_access).await,
            authorized_roots: self.authorized_root_paths(),
            settings_fingerprint,
            expires_in_seconds: 900,
            idempotency_key: key.clone(),
        })?;
        self.persist_plan(&plan).await?;
        let scopes = scope_rows(&episodes);
        Ok(rename_preview_response(
            &plan,
            scopes,
            key,
            &files_for_access,
        ))
    }

    pub(super) async fn probe_source_access(
        &self,
        files: &[MediaFile],
        access: RequiredAccess,
    ) -> BTreeMap<String, FileAccessState> {
        let mut probed = BTreeMap::new();
        for file in files {
            match self
                .dependencies()
                .file_system
                .probe_access(&file.path, access)
                .await
            {
                Ok(state) => {
                    probed.insert(mkvo_application::paths::path_key(&file.path), state);
                }
                Err(error) => {
                    tracing::debug!(
                        path = %file.path.display(),
                        %error,
                        "source access could not be probed"
                    );
                }
            }
        }
        probed
    }

    /// The working set plus everything in the files' folders, so the planner
    /// sees their sidecars and every name that is already taken.
    pub(super) async fn existing_paths_around(&self, files: &[MediaFile]) -> BTreeSet<PathBuf> {
        let mut existing = self.current_existing_paths().await;
        let folders: BTreeSet<_> = files.iter().filter_map(|file| file.path.parent()).collect();
        for folder in folders {
            match self.dependencies().file_system.list_files(folder).await {
                Ok(listed) => existing.extend(listed),
                Err(error) => tracing::debug!(
                    path = %folder.display(),
                    %error,
                    "folder could not be listed for a rename preview"
                ),
            }
        }
        existing
    }

    pub(super) async fn existing_parents(&self, files: &[MediaFile]) -> BTreeSet<String> {
        let mut parents = BTreeSet::new();
        for parent in files.iter().filter_map(|file| file.path.parent()) {
            if self
                .dependencies()
                .file_system
                .is_directory(parent)
                .await
                .unwrap_or(false)
            {
                parents.insert(mkvo_application::paths::path_key(parent));
            }
        }
        parents
    }

    pub async fn apply_rename_preview(
        &self,
        request: RenameApplyRequest,
    ) -> RuntimeResult<RenameApplyResponse> {
        let (plan_id, fingerprint, key) = require_plan_fields(
            request.plan_id,
            request.plan_fingerprint,
            request.idempotency_key,
        )?;
        let referenced: RenamePlan = self
            .load_referenced_plan(plan_id, &fingerprint, &key)
            .await?;
        let resources = referenced.context.resources.clone();
        let runtime = self.clone();
        let (plan, replay) = self
            .jobs()
            .with_resource_lease(
                &resources,
                tokio_util::sync::CancellationToken::new(),
                || {
                    async move {
                        // Re-check after acquiring the lease. A concurrent request
                        // may have completed while this request was waiting.
                        if let Some(journal) = runtime.dependencies().journal.get(&key).await?
                            && journal.status == mkvo_application::JournalStatus::Completed
                        {
                            return Ok((referenced, true));
                        }
                        let plan: RenamePlan = runtime
                            .load_valid_plan(plan_id, &fingerprint, &key)
                            .await
                            .map_err(runtime_application_error)?;
                        let mut journal = mkvo_application::JournalRecord {
                            idempotency_key: key.clone(),
                            plan_id,
                            step: 0,
                            status: mkvo_application::JournalStatus::Prepared,
                            resources: plan.context.resources.clone(),
                            items: plan
                                .payload
                                .items
                                .iter()
                                .filter(|item| item.can_apply())
                                .map(|item| mkvo_application::JournalItemOutcome {
                                    key: item.source.to_string_lossy().into_owned(),
                                    status: mkvo_application::JournalItemStatus::Pending,
                                    detail: None,
                                })
                                .collect(),
                            detail: None,
                            updated_utc: Utc::now(),
                        };
                        runtime.dependencies().journal.begin(&journal).await?;
                        journal.status = mkvo_application::JournalStatus::Running;
                        runtime.dependencies().journal.advance(&journal).await?;

                        let batch_id = RenameBatchId::new();
                        let batch = RenameBatchRecord {
                            id: batch_id,
                            created_at: Utc::now(),
                            undone_at: None,
                            provider: plan.payload.provider,
                            template: plan.payload.template.clone(),
                            entries: Vec::new(),
                        };
                        let items: Vec<_> = plan
                            .payload
                            .items
                            .iter()
                            .filter(|item| item.can_apply())
                            .collect();
                        let file_system = &runtime.dependencies().file_system;
                        let history = &runtime.dependencies().rename_history;
                        let mut moved: Vec<RenameBatchEntry> = Vec::new();
                        let mutation = async {
                            let sidecars = runtime.preflight_rename(&items).await?;
                            for (item, sidecars) in items.iter().zip(sidecars) {
                                journal.detail = Some(format!(
                                    "prepared move {} -> {}; batchId={batch_id}",
                                    item.source.display(),
                                    item.target.display()
                                ));
                                journal.updated_utc = Utc::now();
                                runtime.dependencies().journal.advance(&journal).await?;
                                file_system.move_file(&item.source, &item.target).await?;
                                let mut entry = RenameBatchEntry {
                                    original_path: item.source.clone(),
                                    renamed_path: item.target.clone(),
                                    original_fingerprint: item.source_fingerprint.clone(),
                                    renamed_fingerprint: file_system
                                        .fingerprint(&item.target)
                                        .await
                                        .ok(),
                                    sidecars: Vec::new(),
                                };
                                let mut sidecars_moved = Ok(());
                                for sidecar in sidecars {
                                    sidecars_moved = file_system
                                        .move_file(&sidecar.source, &sidecar.target)
                                        .await;
                                    if sidecars_moved.is_err() {
                                        break;
                                    }
                                    entry.sidecars.push(sidecar);
                                }
                                // Stored after every file, with the sidecars that did move, so
                                // a batch that stops half-way can still be undone.
                                moved.push(entry.clone());
                                if moved.len() == 1 {
                                    let entries = vec![entry];
                                    let first = RenameBatchRecord {
                                        entries,
                                        ..batch.clone()
                                    };
                                    history.add(&first).await?;
                                } else {
                                    history.append(batch_id, &entry).await?;
                                }
                                sidecars_moved?;
                                journal.step = journal.step.saturating_add(1);
                                journal.complete_item(&item.source.to_string_lossy());
                                journal.detail = Some(format!(
                                    "completed move {} -> {}; batchId={batch_id}",
                                    item.source.display(),
                                    item.target.display()
                                ));
                                journal.updated_utc = Utc::now();
                                runtime.dependencies().journal.advance(&journal).await?;
                            }
                            Ok::<(), ApplicationError>(())
                        }
                        .await;
                        let followed: Vec<_> = moved
                            .iter()
                            .map(|entry| (entry.original_fingerprint.clone(), renamed(entry)))
                            .collect();
                        runtime.follow_renames(&followed).await;
                        if let Err(error) = mutation {
                            journal.fail_first_pending_item(error.to_string());
                            journal.status = mkvo_application::JournalStatus::Failed;
                            journal.detail = Some(format!(
                            "rename failed after {} completed move(s); batchId={batch_id}; {error}",
                            journal.step
                        ));
                            journal.updated_utc = Utc::now();
                            let _ = runtime.dependencies().journal.advance(&journal).await;
                            return Err(error);
                        }
                        journal.status = mkvo_application::JournalStatus::Completed;
                        journal.detail = Some("rename plan completed".to_owned());
                        journal.updated_utc = Utc::now();
                        runtime.dependencies().journal.advance(&journal).await?;
                        runtime
                            .append_log(
                                "Rename",
                                "Rename plan completed",
                                &format!("planId={plan_id}; fingerprint={fingerprint}"),
                            )
                            .await
                            .map_err(runtime_application_error)?;
                        Ok((plan, false))
                    }
                },
            )
            .await?;

        // The working set is what later operations run against, so it moves
        // with the files. A replay renamed nothing this time round, but the
        // paths it describes are still the ones that now exist.
        if replay {
            let moves: Vec<_> = plan
                .payload
                .items
                .iter()
                .filter(|item| item.can_apply())
                .map(|item| {
                    let target = FileFingerprint {
                        path: item.target.clone(),
                        ..item.source_fingerprint.clone()
                    };
                    (item.source_fingerprint.clone(), target)
                })
                .collect();
            self.follow_renames(&moves).await;
        }

        Ok(rename_apply_response(&plan, replay))
    }

    /// Check every move of a batch before the first one, so a collision
    /// cannot leave it half applied. Returns each item's sidecars that are
    /// still there to move.
    async fn preflight_rename(
        &self,
        items: &[&mkvo_domain::RenamePlanItem],
    ) -> Result<Vec<Vec<mkvo_domain::RenameSidecar>>, ApplicationError> {
        let mut taken = Vec::new();
        let mut sidecars = Vec::with_capacity(items.len());
        for item in items {
            self.revalidate_fingerprint(&item.source_fingerprint)
                .await
                .map_err(runtime_application_error)?;
            if !self.target_is_free(&item.source, &item.target).await? {
                taken.push(item.target.display().to_string());
            }
            let mut present = Vec::new();
            for sidecar in &item.sidecars {
                // One deleted since the preview is simply not moved.
                if !self
                    .dependencies()
                    .file_system
                    .exists(&sidecar.source)
                    .await?
                {
                    continue;
                }
                if !self
                    .target_is_free(&sidecar.source, &sidecar.target)
                    .await?
                {
                    taken.push(sidecar.target.display().to_string());
                }
                present.push(sidecar.clone());
            }
            sidecars.push(present);
        }
        if taken.is_empty() {
            Ok(sidecars)
        } else {
            Err(ApplicationError::Conflict(format!(
                "nothing was renamed because {} target(s) already exist: {}",
                taken.len(),
                taken.join(", ")
            )))
        }
    }

    /// Whether `source` can take the name `target`: nothing has it, or it is
    /// `source` itself in another case, which the move sorts out.
    async fn target_is_free(&self, source: &Path, target: &Path) -> Result<bool, PortError> {
        Ok(
            mkvo_domain::case_folded_path_key(source) == mkvo_domain::case_folded_path_key(target)
                || !self.dependencies().file_system.exists(target).await?,
        )
    }

    /// Carry cache rows and the working set from each first fingerprint to the
    /// second. A rename changes no content, so nothing is probed again.
    async fn follow_renames(&self, renames: &[(FileFingerprint, FileFingerprint)]) {
        if renames.is_empty() {
            return;
        }
        let cache = &self.dependencies().cache;
        let mut cached = Vec::new();
        for (before, after) in renames {
            if let Ok(Some(mut file)) = cache.get_valid(before).await {
                file.path = after.path.clone();
                file.fingerprint = after.clone();
                cached.push(file);
            }
            if let Err(error) = cache.remove(&before.path).await {
                tracing::warn!(
                    path = %before.path.display(),
                    %error,
                    "renamed file's old cache entry could not be removed"
                );
            }
        }
        if let Err(error) = cache.upsert_many(&cached).await {
            tracing::warn!(%error, "renamed files could not be cached under their new names");
        }
        let renamed: Vec<_> = renames
            .iter()
            .map(|(before, after)| (before.path.clone(), after.clone()))
            .collect();
        self.move_working_set_files(&renamed).await;
    }

    pub async fn get_rename_batches(&self) -> RuntimeResult<RenameBatchListResponse> {
        let records = self.dependencies().rename_history.list_recent(100).await?;
        let native_ids: BTreeSet<_> = records.iter().map(|record| record.id.to_string()).collect();
        let mut batches: Vec<_> = records.iter().map(rename_batch_dto).collect();
        batches.extend(
            self.legacy_rename_history()
                .iter()
                .filter(|record| !native_ids.contains(&record.id))
                .filter_map(legacy_rename_batch_dto),
        );
        batches.sort_by_key(|batch| std::cmp::Reverse(batch.created_at));
        batches.truncate(100);
        Ok(RenameBatchListResponse { batches })
    }

    pub async fn preview_rename_batch_undo(
        &self,
        id: &str,
    ) -> RuntimeResult<RenameBatchUndoPreviewResponse> {
        if let Some(batch) = self
            .legacy_rename_history()
            .iter()
            .find(|batch| batch.id == id)
        {
            let skipped = batch.entries.len().max(batch.total_files);
            return Ok(RenameBatchUndoPreviewResponse {
                restorable: 0,
                skipped,
                lines: batch
                    .entries
                    .iter()
                    .map(|entry| {
                        format!(
                            "Read-only legacy history: {} -> {} cannot be safely undone because the old record has no file fingerprint",
                            entry.original_path.display(),
                            entry.renamed_path.display()
                        )
                    })
                    .collect(),
                has_skipped_files: skipped > 0,
            });
        }
        let batch = self.rename_batch(id).await?;
        let mut response = RenameBatchUndoPreviewResponse::default();
        for entry in &batch.entries {
            if batch.undone_at.is_none()
                && self
                    .can_restore(&entry.renamed_path, &entry.original_path)
                    .await?
            {
                response.restorable += 1;
                response.lines.push(format!(
                    "Restore {} -> {}",
                    entry.renamed_path.display(),
                    entry.original_path.display()
                ));
                for sidecar in &entry.sidecars {
                    if self.can_restore(&sidecar.target, &sidecar.source).await? {
                        response.lines.push(format!(
                            "Restore {} -> {}",
                            sidecar.target.display(),
                            sidecar.source.display()
                        ));
                    }
                }
            } else {
                response.skipped += 1;
                response.lines.push(format!(
                    "Skip {}: source missing, target exists, or batch already undone",
                    entry.renamed_path.display()
                ));
            }
        }
        response.has_skipped_files = response.skipped > 0;
        Ok(response)
    }

    pub async fn undo_rename_batch(&self, id: &str) -> RuntimeResult<RenameBatchUndoResponse> {
        if self
            .legacy_rename_history()
            .iter()
            .any(|batch| batch.id == id)
        {
            return Err(RuntimeError::invalid(
                "legacy rename history is read-only and cannot be safely undone because it has no file fingerprints",
            ));
        }
        let batch = self.rename_batch(id).await?;
        let resources: Vec<_> = batch
            .entries
            .iter()
            .flat_map(|entry| {
                let sidecars = entry
                    .sidecars
                    .iter()
                    .flat_map(|sidecar| [&sidecar.target, &sidecar.source]);
                [&entry.renamed_path, &entry.original_path]
                    .into_iter()
                    .chain(sidecars)
            })
            .map(|path| mkvo_domain::ResourceClaim::write(path.clone()))
            .collect();
        let runtime = self.clone();
        let id = id.to_owned();
        self.jobs()
            .with_resource_lease(
                &resources,
                tokio_util::sync::CancellationToken::new(),
                || {
                    async move {
                        // Re-read under the lease so concurrent undo calls cannot
                        // both pass the undone check.
                        let batch = runtime
                            .rename_batch(&id)
                            .await
                            .map_err(runtime_application_error)?;
                        if batch.undone_at.is_some() {
                            return Err(ApplicationError::Conflict(
                                "rename batch was already undone".to_owned(),
                            ));
                        }
                        let file_system = &runtime.dependencies().file_system;
                        let mut response = RenameBatchUndoResponse::default();
                        let mut restored = Vec::new();
                        let restoration = async {
                            for entry in &batch.entries {
                                if !runtime
                                    .can_restore(&entry.renamed_path, &entry.original_path)
                                    .await?
                                {
                                    response.skipped += 1;
                                    response
                                        .lines
                                        .push(format!("Skipped {}", entry.renamed_path.display()));
                                    continue;
                                }
                                if let Some(expected) = &entry.renamed_fingerprint {
                                    runtime
                                        .revalidate_fingerprint(expected)
                                        .await
                                        .map_err(runtime_application_error)?;
                                }
                                file_system
                                    .move_file(&entry.renamed_path, &entry.original_path)
                                    .await?;
                                let current = renamed(entry);
                                let original = FileFingerprint {
                                    path: entry.original_path.clone(),
                                    ..current.clone()
                                };
                                restored.push((current, original));
                                response.renamed += 1;
                                response
                                    .lines
                                    .push(format!("Restored {}", entry.original_path.display()));
                                response.restored.push(RenameBatchRestoreMove {
                                    original_path: display_path(&entry.original_path),
                                    renamed_path: display_path(&entry.renamed_path),
                                    original_file_name: file_name(&entry.original_path),
                                });
                                for sidecar in &entry.sidecars {
                                    let line = if runtime
                                        .can_restore(&sidecar.target, &sidecar.source)
                                        .await?
                                    {
                                        file_system
                                            .move_file(&sidecar.target, &sidecar.source)
                                            .await?;
                                        format!("Restored {}", sidecar.source.display())
                                    } else {
                                        format!("Skipped {}", sidecar.target.display())
                                    };
                                    response.lines.push(line);
                                }
                            }
                            Ok::<(), ApplicationError>(())
                        }
                        .await;
                        runtime.follow_renames(&restored).await;
                        restoration?;
                        runtime
                            .dependencies()
                            .rename_history
                            .mark_undone(batch.id, Utc::now())
                            .await?;
                        runtime
                            .append_log("Rename", "Rename batch undo completed", &id)
                            .await
                            .map_err(runtime_application_error)?;
                        Ok(response)
                    }
                },
            )
            .await
            .map_err(RuntimeError::from)
    }

    /// Whether undo can move `current` back to `original`.
    async fn can_restore(&self, current: &Path, original: &Path) -> Result<bool, PortError> {
        Ok(self.dependencies().file_system.exists(current).await?
            && self.target_is_free(current, original).await?)
    }

    pub async fn clear_rename_batches(&self) -> RuntimeResult<RenameBatchListResponse> {
        self.dependencies().rename_history.clear().await?;
        self.get_rename_batches().await
    }

    pub(crate) async fn load_rename_episodes(
        &self,
        provider: MetadataProvider,
        media_id: &str,
        media_name: &str,
        language: Option<&str>,
    ) -> RuntimeResult<Vec<mkvo_domain::EpisodeMetadata>> {
        let client = self
            .provider_client(Some(provider_name(provider)), language)
            .await?;
        let episodes = client
            .episodes(
                media_id,
                language,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .map_err(RuntimeError::from)?;

        if provider == MetadataProvider::AniList
            && episodes.iter().all(|episode| {
                episode
                    .title
                    .strip_prefix("Episode ")
                    .is_some_and(|number| number.parse::<u32>().is_ok())
            })
        {
            if let Ok(tvdb) = self.provider_client(Some("tvdb"), language).await {
                if let Ok(results) = tvdb
                    .search(
                        media_name,
                        language,
                        tokio_util::sync::CancellationToken::new(),
                    )
                    .await
                {
                    if let Some(match_result) = results.into_iter().find(|result| {
                        result.provider == MetadataProvider::Tvdb
                            && result.title.eq_ignore_ascii_case(media_name)
                    }) {
                        if let Ok(fallback) = tvdb
                            .episodes(
                                &match_result.id,
                                language,
                                tokio_util::sync::CancellationToken::new(),
                            )
                            .await
                            && fallback.iter().any(|episode| {
                                episode
                                    .title
                                    .strip_prefix("Episode ")
                                    .is_none_or(|number| number.parse::<u32>().is_err())
                            })
                        {
                            return Ok(fallback);
                        }
                    }
                }
            }
        }

        Ok(episodes)
    }

    pub(super) async fn rename_batch(&self, id: &str) -> RuntimeResult<RenameBatchRecord> {
        let id = id
            .parse::<RenameBatchId>()
            .map_err(|_| RuntimeError::invalid(format!("invalid rename batch id: {id}")))?;
        self.dependencies()
            .rename_history
            .get(id)
            .await?
            .ok_or_else(|| RuntimeError::not_found(format!("rename batch {id}")))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use mkvo_application::MediaProbe;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::MkvoRuntimeBuilder;

    /// Stands in for mkvmerge and counts how often it is asked.
    #[derive(Default)]
    struct CountingProbe(AtomicUsize);

    #[async_trait]
    impl MediaProbe for CountingProbe {
        async fn inspect(
            &self,
            path: &Path,
            _cancel: CancellationToken,
        ) -> Result<MediaFile, PortError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let metadata =
                std::fs::metadata(path).map_err(|error| PortError::Other(error.to_string()))?;
            Ok(MediaFile {
                path: path.to_path_buf(),
                original_file_name: None,
                watch_root: None,
                relative_path: None,
                fingerprint: FileFingerprint {
                    path: path.to_path_buf(),
                    size_bytes: metadata.len(),
                    modified_at: metadata.modified().expect("modified").into(),
                    quick_hash: None,
                },
                container: ContainerMetadata::default(),
                tracks: Vec::new(),
                attachments: Vec::new(),
                episode: None,
                provider_match: None,
                status: MediaStatus::Ready,
            })
        }
    }

    struct Library {
        _directory: tempfile::TempDir,
        root: PathBuf,
        runtime: MkvoRuntime,
        probe: Arc<CountingProbe>,
    }

    impl Library {
        /// A library holding `names`, with its `.mkv` files scanned.
        async fn with(names: &[&str]) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("library");
            std::fs::create_dir_all(&root).unwrap();
            let root = std::fs::canonicalize(root).unwrap();
            for name in names {
                std::fs::write(root.join(name), name.as_bytes()).unwrap();
            }
            let built = MkvoRuntimeBuilder::new(&root, directory.path().join("config"))
                .app_name("test")
                .disable_legacy_migration()
                .authorized_root(&root, true)
                .build()
                .unwrap();
            let probe = Arc::new(CountingProbe::default());
            let mut dependencies = built.dependencies().clone();
            dependencies.probe = probe.clone();
            let runtime = MkvoRuntime::from_parts(built.config().clone(), dependencies);
            let videos: Vec<_> = names
                .iter()
                .filter(|name| name.ends_with(".mkv"))
                .map(|name| root.join(name))
                .collect();
            runtime.refresh_working_set(&videos).await;
            Self {
                _directory: directory,
                root,
                runtime,
                probe,
            }
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<_> = std::fs::read_dir(&self.root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        async fn working_set(&self) -> Vec<PathBuf> {
            let files = self.runtime.current_domain_files().await;
            for file in &files {
                assert_eq!(file.fingerprint.path, file.path);
            }
            files.into_iter().map(|file| file.path).collect()
        }

        /// Plan the rename of each scanned file to the name given for it.
        async fn plan(&self, new_names: &[(&str, &str)]) -> RenameApplyRequest {
            let mut files = self.runtime.current_domain_files().await;
            files.retain_mut(|file| {
                let new_name = new_names
                    .iter()
                    .find(|(old, _)| file.path == self.root.join(old))
                    .map(|(_, new)| *new);
                file.episode = new_name.map(|title| EpisodeIdentity {
                    series_title: None,
                    season: None,
                    episode: None,
                    absolute_episode: None,
                    episode_title: Some(title.to_owned()),
                    year: None,
                    is_movie: false,
                });
                new_name.is_some()
            });
            let settings = self.runtime.dependencies().settings.load().await.unwrap().0;
            let plan = RenamePlanner
                .build_plan(RenamePlanRequest {
                    existing_paths: self.runtime.existing_paths_around(&files).await,
                    files,
                    template: "{episodeTitle}".to_owned(),
                    provider: None,
                    check_existing_files: true,
                    source_access: BTreeMap::new(),
                    existing_parents: BTreeSet::new(),
                    authorized_roots: Vec::new(),
                    settings_fingerprint: stable_fingerprint(&settings).unwrap(),
                    expires_in_seconds: 60,
                    idempotency_key: IdempotencyKey::generate(),
                })
                .unwrap();
            assert!(!plan.payload.has_blocking_issues(), "{:?}", plan.payload);
            self.runtime.persist_plan(&plan).await.unwrap();
            RenameApplyRequest {
                plan_id: Some(plan.metadata.id),
                plan_fingerprint: Some(plan.metadata.fingerprint.clone()),
                idempotency_key: Some(plan.metadata.idempotency_key.clone()),
                ..RenameApplyRequest::default()
            }
        }

        async fn batches(&self) -> Vec<RenameBatchRecord> {
            let history = &self.runtime.dependencies().rename_history;
            history.list_recent(10).await.unwrap()
        }
    }

    #[tokio::test]
    async fn a_batch_takes_its_sidecars_along_and_undo_brings_them_back() {
        let library = Library::with(&[
            "Show - 01.mkv",
            "Show - 01.srt",
            "Show - 01.en.ass",
            "Show - 02.mkv",
            "Show - 02.srt",
        ])
        .await;
        let request = library
            .plan(&[("Show - 01.mkv", "Pilot"), ("Show - 02.mkv", "Second")])
            .await;
        // Gone between preview and apply: nothing to move, and no reason to stop.
        std::fs::remove_file(library.root.join("Show - 02.srt")).unwrap();

        library.runtime.apply_rename_preview(request).await.unwrap();

        assert_eq!(
            library.names(),
            ["Pilot.en.ass", "Pilot.mkv", "Pilot.srt", "Second.mkv"]
        );
        let batches = library.batches().await;
        assert_eq!(batches.len(), 1);
        let sidecars: Vec<_> = batches[0]
            .entries
            .iter()
            .map(|entry| entry.sidecars.len())
            .collect();
        assert_eq!(sidecars, [2, 0]);

        let undone = library
            .runtime
            .undo_rename_batch(&batches[0].id.to_string())
            .await
            .unwrap();
        assert_eq!((undone.renamed, undone.skipped), (2, 0));
        assert_eq!(
            library.names(),
            [
                "Show - 01.en.ass",
                "Show - 01.mkv",
                "Show - 01.srt",
                "Show - 02.mkv"
            ]
        );
        assert!(library.batches().await[0].undone_at.is_some());
    }

    /// Each renamed file used to be handed to mkvmerge again, one at a time.
    #[tokio::test]
    async fn renamed_files_are_followed_without_being_probed_again() {
        let library = Library::with(&["a.mkv", "b.mkv", "c.mkv"]).await;
        let request = library
            .plan(&[("a.mkv", "Pilot"), ("b.mkv", "Second")])
            .await;
        assert_eq!(library.probe.0.load(Ordering::SeqCst), 3);

        library.runtime.apply_rename_preview(request).await.unwrap();

        let renamed = ["Pilot.mkv", "Second.mkv", "c.mkv"].map(|name| library.root.join(name));
        assert_eq!(library.working_set().await, renamed);
        let cache = &library.runtime.dependencies().cache;
        for file in library.runtime.current_domain_files().await {
            assert!(cache.get_valid(&file.fingerprint).await.unwrap().is_some());
        }
        assert_eq!(cache.count().await.unwrap(), 3);

        let batch = library.batches().await[0].id.to_string();
        library.runtime.undo_rename_batch(&batch).await.unwrap();

        let original = ["a.mkv", "b.mkv", "c.mkv"].map(|name| library.root.join(name));
        assert_eq!(library.working_set().await, original);
        assert_eq!(cache.count().await.unwrap(), 3);
        assert_eq!(library.probe.0.load(Ordering::SeqCst), 3);
    }

    /// The batch used to stop at the collision with the files before it
    /// already renamed.
    #[tokio::test]
    async fn a_target_that_appeared_since_the_preview_stops_the_batch_before_it_starts() {
        let library = Library::with(&["a.mkv", "a.srt", "b.mkv", "c.mkv"]).await;
        let request = library
            .plan(&[("a.mkv", "Pilot"), ("b.mkv", "Second"), ("c.mkv", "Third")])
            .await;
        std::fs::write(library.root.join("Third.mkv"), b"someone else's").unwrap();
        std::fs::write(library.root.join("Pilot.srt"), b"someone else's").unwrap();

        let error = library
            .runtime
            .apply_rename_preview(request)
            .await
            .expect_err("collision");

        assert_eq!(error.code, mkvo_contracts::ApiErrorCode::Conflict);
        assert!(error.message.contains("Third.mkv"), "{}", error.message);
        assert!(error.message.contains("Pilot.srt"), "{}", error.message);
        assert_eq!(
            library.names(),
            ["Pilot.srt", "Third.mkv", "a.mkv", "a.srt", "b.mkv", "c.mkv"]
        );
        assert_eq!(
            std::fs::read(library.root.join("Third.mkv")).unwrap(),
            b"someone else's"
        );
        assert_eq!(library.batches().await, []);
    }

    #[tokio::test]
    async fn a_rename_that_only_changes_case_is_applied_and_undone() {
        let library = Library::with(&["ep.mkv", "ep.srt"]).await;
        let request = library.plan(&[("ep.mkv", "Ep")]).await;

        library.runtime.apply_rename_preview(request).await.unwrap();
        assert_eq!(library.names(), ["Ep.mkv", "Ep.srt"]);
        assert_eq!(library.working_set().await, [library.root.join("Ep.mkv")]);

        let batch = library.batches().await[0].id.to_string();
        let preview = library
            .runtime
            .preview_rename_batch_undo(&batch)
            .await
            .unwrap();
        assert_eq!((preview.restorable, preview.skipped), (1, 0));
        library.runtime.undo_rename_batch(&batch).await.unwrap();
        assert_eq!(library.names(), ["ep.mkv", "ep.srt"]);
    }
}
