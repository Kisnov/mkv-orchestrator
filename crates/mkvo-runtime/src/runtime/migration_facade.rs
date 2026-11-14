use super::*;

impl MkvoRuntime {
    pub async fn migrate_legacy_data(&self) -> RuntimeResult<LegacyMigrationReport> {
        let report = self.migrate_dotnet_legacy_data().await?;
        self.migrate_compatibility_settings().await?;
        Ok(report)
    }

    async fn migrate_dotnet_legacy_data(&self) -> RuntimeResult<LegacyMigrationReport> {
        let legacy_batches = self.inner.legacy_rename_history.len();
        let Some(source) = self.inner.config.resolved_legacy_settings_path() else {
            return Ok(LegacyMigrationReport {
                status: LegacyMigrationStatus::SourceMissing,
                source_path: None,
                backup_path: None,
                settings_revision: None,
                secrets_imported: 0,
                cache_rebuild_required: false,
                legacy_rename_batches: legacy_batches,
            });
        };
        let (_, current_revision) = self.inner.dependencies.settings.load().await?;
        if current_revision > 0 {
            return Ok(LegacyMigrationReport {
                status: LegacyMigrationStatus::SkippedExisting,
                source_path: Some(display_path(&source)),
                backup_path: None,
                settings_revision: Some(current_revision),
                secrets_imported: 0,
                cache_rebuild_required: false,
                legacy_rename_batches: legacy_batches,
            });
        }

        let import_source = source.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            let store = SqliteStore::open_in_memory()?;
            let mut secrets = std::collections::BTreeMap::new();
            let outcome = import_legacy_settings(&store, &import_source, |key, value| {
                secrets.insert(key.to_owned(), value.to_owned());
                Ok::<_, std::convert::Infallible>(())
            })?;
            let settings = store
                .get_setting_with_revision::<AppSettings>("app")?
                .map(|(_, _, settings)| settings);
            Ok::<_, mkvo_infra_sqlite::LegacyImportError>((outcome, settings, secrets))
        })
        .await
        .map_err(|error| RuntimeError::internal(format!("legacy import task failed: {error}")))?
        .map_err(|error| RuntimeError::internal(error.to_string()))?;

        let (outcome, settings, secrets) = prepared;
        let (backup_path, cache_rebuild_required) = match &outcome {
            LegacyImportOutcome::Imported {
                backup_path,
                cache_rebuild_required,
                ..
            } => (Some(display_path(backup_path)), *cache_rebuild_required),
            LegacyImportOutcome::SourceMissing => {
                return Ok(LegacyMigrationReport {
                    status: LegacyMigrationStatus::SourceMissing,
                    source_path: Some(display_path(&source)),
                    backup_path: None,
                    settings_revision: None,
                    secrets_imported: 0,
                    cache_rebuild_required: false,
                    legacy_rename_batches: legacy_batches,
                });
            }
            LegacyImportOutcome::SkippedExisting => unreachable!(
                "the isolated conversion store cannot already contain application settings"
            ),
        };

        let mut secrets_imported = 0;
        for (key, value) in secrets {
            if self.inner.dependencies.secrets.get(&key).await?.is_none() {
                self.inner.dependencies.secrets.set(&key, &value).await?;
                secrets_imported += 1;
            }
        }
        let settings = settings.ok_or_else(|| {
            RuntimeError::internal("legacy conversion completed without converted settings")
        })?;
        let revision = self
            .inner
            .dependencies
            .settings
            .save(&settings, Some(0))
            .await?;
        Ok(LegacyMigrationReport {
            status: LegacyMigrationStatus::Imported,
            source_path: Some(display_path(&source)),
            backup_path,
            settings_revision: Some(revision),
            secrets_imported,
            cache_rebuild_required,
            legacy_rename_batches: legacy_batches,
        })
    }

    async fn migrate_compatibility_settings(&self) -> RuntimeResult<()> {
        const CURRENT_SETTINGS_SCHEMA: u32 = 2;
        let path = self
            .inner
            .config
            .config_root
            .join("web-settings-extra.json");
        let (mut settings, revision) = self.inner.dependencies.settings.load().await?;
        if revision > 0 && settings.schema_version >= CURRENT_SETTINGS_SCHEMA {
            return Ok(());
        }

        if path.exists() {
            let import_path = path.clone();
            settings.presets = tokio::task::spawn_blocking(move || {
                let bytes = std::fs::read(&import_path)?;
                serde_json::from_slice::<PresetSettings>(&bytes)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            })
            .await
            .map_err(|error| {
                RuntimeError::internal(format!("compatibility settings import failed: {error}"))
            })??;
        }
        settings.schema_version = CURRENT_SETTINGS_SCHEMA;
        settings = settings.normalized();
        self.inner
            .dependencies
            .settings
            .save(&settings, Some(revision))
            .await?;

        if path.exists() {
            let archive = self
                .inner
                .config
                .config_root
                .join("web-settings-extra.migrated.json");
            if let Err(error) = std::fs::rename(&path, &archive) {
                tracing::warn!(
                    path = %path.display(),
                    archive = %archive.display(),
                    %error,
                    "compatibility settings were imported but the legacy file could not be archived"
                );
            }
        }
        Ok(())
    }

    /// Classifies non-terminal jobs left behind by a previous process without
    /// changing persisted state.
    pub async fn classify_startup_recovery(&self) -> RuntimeResult<StartupRecoveryReport> {
        self.startup_recovery(false).await
    }

    /// Terminalizes stale persisted jobs according to their durable journal.
    /// Completed mutations become completed jobs; clean retries and ambiguous
    /// partial mutations become failed jobs with an explicit recovery reason.
    pub async fn recover_startup_state(&self) -> RuntimeResult<StartupRecoveryReport> {
        self.startup_recovery(true).await
    }

    async fn startup_recovery(&self, persist: bool) -> RuntimeResult<StartupRecoveryReport> {
        let inspected_utc = Utc::now();
        let mut report = StartupRecoveryReport {
            inspected_utc,
            completed: 0,
            clean_retry: 0,
            manual_review: 0,
            items: Vec::new(),
            orphan_journals: Vec::new(),
            journal_enumeration_supported: false,
            limitations: Vec::new(),
        };
        for mut snapshot in self.inner.dependencies.jobs.list_unfinished().await? {
            let previous_status = snapshot.status;
            let journal = self
                .inner
                .dependencies
                .journal
                .get(&snapshot.idempotency_key)
                .await?;
            let (disposition, reason) = classify_recovery(&snapshot, journal.as_ref());
            match disposition {
                RecoveryDisposition::Completed => report.completed += 1,
                RecoveryDisposition::CleanRetry => report.clean_retry += 1,
                RecoveryDisposition::ManualReview => report.manual_review += 1,
            }
            let mut persisted_status_updated = false;
            if persist {
                snapshot.status = match disposition {
                    RecoveryDisposition::Completed => JobStatus::Completed,
                    RecoveryDisposition::CleanRetry | RecoveryDisposition::ManualReview => {
                        JobStatus::Failed
                    }
                };
                snapshot.completed_utc = Some(inspected_utc);
                snapshot.current_file_percent = 0;
                snapshot.lines.push(format!("Startup recovery: {reason}"));
                if disposition == RecoveryDisposition::Completed {
                    snapshot.error = None;
                } else {
                    snapshot.error = Some(reason.clone());
                }
                snapshot.revision = snapshot.revision.saturating_add(1);
                self.inner.dependencies.jobs.update(&snapshot).await?;
                persisted_status_updated = true;
            }
            report.items.push(StartupRecoveryItem {
                job_id: snapshot.id,
                job_kind: snapshot.kind,
                previous_status,
                disposition,
                reason,
                journal_status: journal.as_ref().map(|record| record.status),
                journal_step: journal.as_ref().map(|record| record.step),
                persisted_status_updated,
            });
        }
        match self.inner.dependencies.journal.list_incomplete().await? {
            Some(journals) => {
                report.journal_enumeration_supported = true;
                if persist {
                    self.clean_stale_temporaries(&journals).await;
                }
                for journal in journals {
                    if self
                        .inner
                        .dependencies
                        .jobs
                        .find_by_idempotency(&journal.idempotency_key)
                        .await?
                        .is_some()
                    {
                        continue;
                    }
                    let (disposition, reason) = if journal.step == 0
                        && journal
                            .items
                            .iter()
                            .all(|item| item.status == mkvo_application::JournalItemStatus::Pending)
                    {
                        (
                            RecoveryDisposition::CleanRetry,
                            "orphan journal has no completed item mutations".to_owned(),
                        )
                    } else {
                        (
                            RecoveryDisposition::ManualReview,
                            "orphan journal records completed or uncertain item mutations"
                                .to_owned(),
                        )
                    };
                    match disposition {
                        RecoveryDisposition::CleanRetry => report.clean_retry += 1,
                        RecoveryDisposition::ManualReview => report.manual_review += 1,
                        RecoveryDisposition::Completed => report.completed += 1,
                    }
                    report.orphan_journals.push(OrphanJournalItem {
                        idempotency_key: journal.idempotency_key,
                        plan_id: journal.plan_id,
                        status: journal.status,
                        step: journal.step,
                        disposition,
                        reason,
                        items: journal.items,
                    });
                }
            }
            None => report.limitations.push(
                "The configured operation journal adapter cannot enumerate orphan journals."
                    .to_owned(),
            ),
        }
        Ok(report)
    }

    /// Removes the temporaries an interrupted job left next to an intact original, and
    /// keeps (with a warning) any that may be the only copy. Reads only the folders the
    /// journals claim.
    async fn clean_stale_temporaries(&self, journals: &[mkvo_application::JournalRecord]) {
        let file_system = &self.inner.dependencies.file_system;
        for journal in journals {
            let claimed: BTreeSet<_> = journal
                .resources
                .iter()
                .map(|claim| mkvo_domain::path_key(&claim.path))
                .collect();
            let folders: BTreeSet<_> = journal
                .resources
                .iter()
                .filter_map(|claim| claim.path.parent())
                .collect();
            for folder in folders {
                let Ok(listed) = file_system.list_files(folder).await else {
                    continue;
                };
                for name in listed.iter().filter_map(|path| path.file_name()?.to_str()) {
                    let Some((original, extraction)) = temporary_original(name) else {
                        continue;
                    };
                    let original = folder.join(original);
                    let key = mkvo_domain::path_key(&original);
                    if !claimed.contains(&key) {
                        continue;
                    }
                    let temporary = folder.join(name);
                    // An extraction temporary is keyed by the subtitle it replaces, not the item.
                    let completed = !extraction
                        && journal.items.iter().any(|item| {
                            item.status == mkvo_application::JournalItemStatus::Completed
                                && mkvo_domain::path_key(Path::new(&item.key)) == key
                        });
                    if completed || !file_system.exists(&original).await.unwrap_or(false) {
                        tracing::warn!(
                            path = %temporary.display(),
                            "an interrupted job left this file and it may be the only copy; it was kept"
                        );
                    } else if let Err(error) = file_system.remove_file(&temporary).await {
                        tracing::warn!(path = %temporary.display(), %error, "an interrupted job left this file and it could not be removed");
                    } else {
                        tracing::info!(path = %temporary.display(), "removed a file an interrupted job left behind");
                    }
                }
            }
        }
    }
}

/// The file a remux or extraction temporary was written for, and whether it is an extraction.
fn temporary_original(name: &str) -> Option<(&str, bool)> {
    let inner = name.strip_prefix('.')?.strip_suffix(".tmp")?;
    if let Some(original) = inner.strip_suffix(".mkvo-remux") {
        return Some((original, false));
    }
    let (original, track) = inner.rsplit_once(".mkvo-extract-")?;
    (!track.is_empty() && track.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some((original, true))
}
