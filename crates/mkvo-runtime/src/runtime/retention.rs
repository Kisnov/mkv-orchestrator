use std::time::Duration;

use chrono::{TimeDelta, Utc};

use super::MkvoRuntime;
use crate::RuntimeResult;

const PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// How long a rename can be undone. Fixed, not a setting.
const RENAME_UNDO_DAYS: i64 = 30;

impl MkvoRuntime {
    /// Delete expired plans, and the finished jobs, journals and logs older
    /// than `rename.history_retention_days`. Zero days keeps them forever,
    /// but still clears what older builds stored and nothing reads. Rename
    /// batches go after `RENAME_UNDO_DAYS` whatever that setting says.
    pub async fn prune_history(&self) -> RuntimeResult<u64> {
        let dependencies = &self.inner.dependencies;
        let now = Utc::now();
        let mut removed = dependencies.plans.remove_expired(now).await?;
        let (settings, _revision) = dependencies.settings.load().await?;
        let days = settings.rename.history_retention_days;
        let cutoff = (days > 0).then(|| now - TimeDelta::days(i64::from(days)));
        removed += dependencies.jobs.prune_history(cutoff).await?;
        removed += dependencies
            .rename_history
            .remove_before(now - TimeDelta::days(RENAME_UNDO_DAYS))
            .await?;
        Ok(removed)
    }

    /// Prune now, then once a day for as long as the host runs.
    pub(crate) fn spawn_history_pruning(&self) {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(PRUNE_INTERVAL);
            loop {
                interval.tick().await;
                match runtime.prune_history().await {
                    Ok(removed) => tracing::info!(removed, "history pruned"),
                    Err(error) => tracing::warn!(%error, "history could not be pruned"),
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use mkvo_contracts::{LogLevel, LogQuery, OperationLogEntry};

    use super::*;
    use crate::MkvoRuntimeBuilder;

    async fn runtime_with_logs(retention_days: u32) -> (tempfile::TempDir, MkvoRuntime) {
        let directory = tempfile::tempdir().unwrap();
        let runtime = MkvoRuntimeBuilder::new(directory.path(), directory.path().join("config"))
            .app_name("test")
            .disable_legacy_migration()
            .unrestricted_browsing()
            .build()
            .unwrap();
        let dependencies = &runtime.inner.dependencies;
        let (mut settings, _revision) = dependencies.settings.load().await.unwrap();
        settings.rename.history_retention_days = retention_days;
        dependencies.settings.save(&settings, None).await.unwrap();
        for (message, age_days) in [("old", 40), ("recent", 20)] {
            dependencies
                .logs
                .append(&OperationLogEntry {
                    timestamp_utc: Utc::now() - TimeDelta::days(age_days),
                    correlation_id: mkvo_domain::CorrelationId::new(),
                    area: "Scan".to_owned(),
                    level: LogLevel::Information,
                    message: message.to_owned(),
                    detail: String::new(),
                })
                .await
                .unwrap();
        }
        (directory, runtime)
    }

    async fn log_messages(runtime: &MkvoRuntime) -> Vec<String> {
        let query = LogQuery {
            area: None,
            minimum_level: None,
            limit: 10,
        };
        let entries = runtime.inner.dependencies.logs.query(&query).await.unwrap();
        entries.into_iter().map(|entry| entry.message).collect()
    }

    #[tokio::test]
    async fn history_older_than_the_retention_setting_is_pruned() {
        let (_directory, runtime) = runtime_with_logs(30).await;
        assert_eq!(runtime.prune_history().await.unwrap(), 1);
        assert_eq!(log_messages(&runtime).await, ["recent"]);
    }

    #[tokio::test]
    async fn zero_retention_days_keeps_history() {
        let (_directory, runtime) = runtime_with_logs(0).await;
        assert_eq!(runtime.prune_history().await.unwrap(), 0);
        assert_eq!(log_messages(&runtime).await, ["recent", "old"]);
    }

    /// The per-file events older builds stored are read by nothing, so they
    /// are not history worth keeping even when history is kept forever.
    #[tokio::test]
    async fn zero_retention_days_still_clears_per_file_job_events() {
        use mkvo_contracts::{JobEvent, JobEventEnvelope, JobKind, JobSnapshot, JobStatus};

        let (_directory, runtime) = runtime_with_logs(0).await;
        let jobs = &runtime.inner.dependencies.jobs;
        let scanned = JobSnapshot {
            id: mkvo_domain::JobId::new(),
            kind: JobKind::Scan,
            status: JobStatus::Canceled,
            correlation_id: mkvo_domain::CorrelationId::new(),
            idempotency_key: mkvo_domain::IdempotencyKey::generate(),
            request_fingerprint: "request".to_owned(),
            plan_id: None,
            created_utc: Utc::now(),
            started_utc: None,
            completed_utc: None,
            completed: 0,
            failed: 0,
            skipped: 0,
            total: 0,
            current_file: String::new(),
            current_file_percent: 0,
            lines: Vec::new(),
            result: None,
            error: None,
            revision: 0,
        };
        jobs.insert(&scanned).await.unwrap();
        let per_file = JobEvent::Progress {
            completed: 1,
            total: 2,
            current_file: "a.mkv".to_owned(),
            current_file_percent: 100,
        };
        let ended = JobEvent::Canceled {
            message: String::new(),
        };
        for (sequence, event) in [(0, per_file.clone()), (1, per_file), (2, ended)] {
            jobs.append_event(&JobEventEnvelope {
                job_id: scanned.id,
                correlation_id: scanned.correlation_id,
                sequence,
                emitted_utc: Utc::now(),
                event,
            })
            .await
            .unwrap();
        }

        assert_eq!(runtime.prune_history().await.unwrap(), 2);
        assert_eq!(log_messages(&runtime).await, ["recent", "old"]);
        assert!(jobs.get(scanned.id).await.unwrap().is_some());
    }

    /// Thirty days, whatever the history setting says: past that, undo is
    /// knowingly given up.
    #[tokio::test]
    async fn a_rename_can_be_undone_for_thirty_days() {
        use mkvo_domain::{FileFingerprint, RenameBatchEntry, RenameBatchId, RenameBatchRecord};

        let directory = tempfile::tempdir().unwrap();
        let library = directory.path().join("library");
        std::fs::create_dir_all(&library).unwrap();
        let runtime = MkvoRuntimeBuilder::new(&library, directory.path().join("config"))
            .app_name("test")
            .disable_legacy_migration()
            .authorized_root(&library, true)
            .build()
            .unwrap();
        let dependencies = &runtime.inner.dependencies;
        let (mut settings, _revision) = dependencies.settings.load().await.unwrap();
        settings.rename.history_retention_days = 0;
        dependencies.settings.save(&settings, None).await.unwrap();

        let mut batches = Vec::new();
        for (name, age_days) in [("old", 31), ("recent", 29)] {
            let original = library.join(format!("{name}.mkv"));
            let renamed = library.join(format!("{name}-renamed.mkv"));
            std::fs::write(&renamed, b"mkv").unwrap();
            let batch = RenameBatchRecord {
                id: RenameBatchId::new(),
                created_at: Utc::now() - TimeDelta::days(age_days),
                undone_at: None,
                provider: None,
                template: "{title}".to_owned(),
                entries: vec![RenameBatchEntry {
                    original_fingerprint: FileFingerprint {
                        path: original.clone(),
                        size_bytes: 3,
                        modified_at: Utc::now(),
                        quick_hash: None,
                    },
                    original_path: original,
                    renamed_path: renamed,
                    renamed_fingerprint: None,
                    sidecars: Vec::new(),
                }],
            };
            dependencies.rename_history.add(&batch).await.unwrap();
            batches.push(batch.id.to_string());
        }

        assert_eq!(runtime.prune_history().await.unwrap(), 1);
        assert!(runtime.undo_rename_batch(&batches[0]).await.is_err());
        assert!(!library.join("old.mkv").exists());
        let undone = runtime.undo_rename_batch(&batches[1]).await.unwrap();
        assert_eq!(undone.renamed, 1);
        assert!(library.join("recent.mkv").exists());
    }

    #[tokio::test]
    async fn starting_the_watchers_also_starts_pruning() {
        let (_directory, runtime) = runtime_with_logs(30).await;
        runtime.start_watchers().await.unwrap();
        for _ in 0..400 {
            if log_messages(&runtime).await == ["recent"] {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("old history was not pruned at startup");
    }
}
