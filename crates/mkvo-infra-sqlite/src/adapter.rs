use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mkvo_application::{
    JobRepository, JournalRecord, MetadataCache, OperationJournal, OperationLog, PlanRepository,
    PortError, RenameHistoryRepository, SettingsRepository,
};
use mkvo_contracts::{
    JobEventEnvelope, JobSnapshot, LogLevel, LogQuery, OperationLogEntry, RecentJob,
};
use mkvo_domain::{
    AppSettings, FileFingerprint, IdempotencyKey, JobId, MediaFile, PlanId, RenameBatchId,
    RenameBatchRecord, StoredPlan as DomainStoredPlan,
};
use rusqlite::{OptionalExtension, params};

use crate::{
    CacheFingerprint, CachedMedia, RenameBatch, RenameBatchEntry, SqliteStore, StoreError,
    StoredJob, StoredPlan,
};

const SETTINGS_KEY: &str = "app";
/// Rows deleted per transaction while pruning.
const PRUNE_BATCH: usize = 1_000;
/// Scan payloads rewritten per transaction; one can be tens of megabytes.
const STRIP_BATCH: usize = 2;

/// Composition-friendly bundle. A single WAL-configured store can be shared by
/// desktop and server hosts and registered for each application repository port.
#[derive(Debug, Clone)]
pub struct SqliteRepositories {
    store: Arc<SqliteStore>,
}

impl SqliteRepositories {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self {
            store: Arc::new(SqliteStore::open(path)?),
        })
    }

    pub fn from_store(store: SqliteStore) -> Self {
        Self {
            store: Arc::new(store),
        }
    }

    pub fn store(&self) -> &SqliteStore {
        &self.store
    }

    pub fn shared_store(&self) -> Arc<SqliteStore> {
        Arc::clone(&self.store)
    }

    async fn blocking<T, F>(&self, operation: F) -> Result<T, PortError>
    where
        T: Send + 'static,
        F: FnOnce(SqliteStore) -> Result<T, StoreError> + Send + 'static,
    {
        let store = (*self.store).clone();
        tokio::task::spawn_blocking(move || operation(store))
            .await
            .map_err(|error| PortError::Other(format!("SQLite task failed: {error}")))?
            .map_err(store_port_error)
    }

    /// Run `batch` until it finds nothing left. Each run is its own short
    /// transaction, so other requests get the connection in between.
    async fn repeat<F>(&self, batch: F) -> Result<u64, PortError>
    where
        F: Fn(SqliteStore) -> Result<u64, StoreError> + Clone + Send + 'static,
    {
        let mut total = 0;
        loop {
            let done = self.blocking(batch.clone()).await?;
            if done == 0 {
                return Ok(total);
            }
            total += done;
        }
    }
}

#[async_trait]
impl MetadataCache for SqliteRepositories {
    async fn get_valid(
        &self,
        fingerprint: &FileFingerprint,
    ) -> Result<Option<MediaFile>, PortError> {
        let fingerprint = to_cache_fingerprint(fingerprint);
        self.blocking(move |store| {
            store
                .get_valid_media(&fingerprint)?
                .map(|cached| serde_json::from_value(cached.payload).map_err(StoreError::from))
                .transpose()
        })
        .await
    }

    async fn upsert(&self, file: &MediaFile) -> Result<(), PortError> {
        let file = file.clone();
        self.blocking(move |store| {
            store.upsert_media(&CachedMedia {
                fingerprint: to_cache_fingerprint(&file.fingerprint),
                scanned_at_ms: Utc::now().timestamp_millis(),
                payload: serde_json::to_value(file)?,
            })
        })
        .await
    }

    async fn remove(&self, path: &Path) -> Result<bool, PortError> {
        let path = path.to_path_buf();
        self.blocking(move |store| store.remove_media(&path)).await
    }

    async fn remove_under(&self, root: &Path) -> Result<u64, PortError> {
        let root = root.to_path_buf();
        self.blocking(move |store| store.remove_media_under(&root))
            .await
    }

    async fn remove_older_than(&self, cutoff: DateTime<Utc>) -> Result<u64, PortError> {
        self.blocking(move |store| store.remove_media_older_than(cutoff.timestamp_millis()))
            .await
    }

    async fn count(&self) -> Result<u64, PortError> {
        self.blocking(|store| store.cache_count()).await
    }

    async fn list_under(&self, root: &Path) -> Result<Vec<MediaFile>, PortError> {
        let root = root.to_path_buf();
        self.blocking(move |store| {
            store
                .list_media_under(&root)?
                .into_iter()
                .map(|cached| serde_json::from_value(cached.payload).map_err(StoreError::from))
                .collect()
        })
        .await
    }
}

#[async_trait]
impl SettingsRepository for SqliteRepositories {
    async fn load(&self) -> Result<(AppSettings, u64), PortError> {
        self.blocking(|store| {
            Ok(store
                .get_setting_with_revision::<AppSettings>(SETTINGS_KEY)?
                .map_or_else(
                    || (AppSettings::default(), 0),
                    |(_, revision, value)| (value, revision),
                ))
        })
        .await
    }

    async fn save(
        &self,
        settings: &AppSettings,
        expected_revision: Option<u64>,
    ) -> Result<u64, PortError> {
        let settings = settings.clone().normalized();
        self.blocking(move |store| {
            store.save_setting_optimistic(
                SETTINGS_KEY,
                settings.schema_version,
                &settings,
                expected_revision,
            )
        })
        .await
    }
}

#[async_trait]
impl PlanRepository for SqliteRepositories {
    async fn save(&self, plan: &DomainStoredPlan) -> Result<(), PortError> {
        let plan = plan.clone();
        self.blocking(move |store| {
            store.put_plan(&StoredPlan {
                id: plan.metadata.id.to_string(),
                kind: serde_json::to_string(&plan.metadata.kind)?,
                version: plan.metadata.contract_version,
                fingerprint: plan.metadata.fingerprint.clone(),
                payload: serde_json::to_value(&plan)?,
                created_at_ms: plan.metadata.created_at.timestamp_millis(),
                expires_at_ms: Some(plan.metadata.expires_at.timestamp_millis()),
            })
        })
        .await
    }

    async fn get(&self, id: PlanId) -> Result<Option<DomainStoredPlan>, PortError> {
        self.blocking(move |store| {
            store
                .get_plan(&id.to_string(), Utc::now().timestamp_millis())?
                .map(|plan| serde_json::from_value(plan.payload).map_err(StoreError::from))
                .transpose()
        })
        .await
    }

    async fn remove_expired(&self, before: DateTime<Utc>) -> Result<u64, PortError> {
        self.blocking(move |store| {
            let connection = store.connection()?;
            let removed = connection.execute(
                "DELETE FROM plans WHERE expires_at_ms IS NOT NULL AND expires_at_ms <= ?1",
                [before.timestamp_millis()],
            )?;
            u64::try_from(removed).map_err(|_| StoreError::NumericOverflow)
        })
        .await
    }
}

#[async_trait]
impl JobRepository for SqliteRepositories {
    async fn insert(&self, snapshot: &JobSnapshot) -> Result<(), PortError> {
        save_job(self, snapshot).await
    }

    async fn update(&self, snapshot: &JobSnapshot) -> Result<(), PortError> {
        save_job(self, snapshot).await
    }

    async fn get(&self, id: JobId) -> Result<Option<JobSnapshot>, PortError> {
        self.blocking(move |store| decode_job(store.get_job(&id.to_string())?))
            .await
    }

    async fn find_by_idempotency(
        &self,
        key: &IdempotencyKey,
    ) -> Result<Option<JobSnapshot>, PortError> {
        let key = key.to_string();
        self.blocking(move |store| decode_job(store.find_job_by_idempotency_key(&key)?))
            .await
    }

    async fn list_recent(&self, limit: usize) -> Result<Vec<RecentJob>, PortError> {
        self.blocking(move |store| {
            let limit = i64::try_from(limit).map_err(|_| StoreError::NumericOverflow)?;
            let connection = store.connection()?;
            // SQLite drops the result payload, so it is never loaded here.
            let mut statement = connection.prepare(
                "SELECT json_remove(result_json, '$.result') FROM jobs
                 ORDER BY updated_at_ms DESC LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| {
                let snapshot: JobSnapshot = serde_json::from_str(&row?)?;
                Ok(RecentJob::from(&snapshot))
            })
            .collect()
        })
        .await
    }

    async fn list_unfinished(&self) -> Result<Vec<JobSnapshot>, PortError> {
        self.blocking(|store| {
            let connection = store.connection()?;
            let mut statement = connection.prepare(
                "SELECT result_json FROM jobs
                 WHERE state IN ('\"Queued\"', '\"WaitingForResources\"', '\"Running\"', '\"Canceling\"')
                 ORDER BY updated_at_ms",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| serde_json::from_str(&row?).map_err(StoreError::from))
                .collect()
        })
        .await
    }

    async fn append_event(&self, event: &JobEventEnvelope) -> Result<(), PortError> {
        let event = event.clone();
        self.blocking(move |store| {
            let payload = serde_json::to_string(&event)?;
            let sequence =
                i64::try_from(event.sequence).map_err(|_| StoreError::NumericOverflow)?;
            let connection = store.connection()?;
            connection.execute(
                "INSERT INTO job_events (job_id, sequence, payload_json, emitted_at_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(job_id, sequence) DO UPDATE SET payload_json=excluded.payload_json,
                 emitted_at_ms=excluded.emitted_at_ms",
                params![
                    event.job_id.to_string(),
                    sequence,
                    payload,
                    event.emitted_utc.timestamp_millis(),
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn prune_history(&self, cutoff: Option<DateTime<Utc>>) -> Result<u64, PortError> {
        let mut removed = self
            .repeat(|store| store.delete_superseded_job_events(PRUNE_BATCH))
            .await?;
        if let Some(cutoff) = cutoff {
            let cutoff_ms = cutoff.timestamp_millis();
            removed += self
                .repeat(move |store| store.delete_finished_jobs(cutoff_ms, PRUNE_BATCH))
                .await?;
            removed += self
                .blocking(move |store| store.delete_finished_journals_and_logs(cutoff_ms))
                .await?;
        }
        // After the deletes, so nothing about to go is rewritten first.
        self.repeat(|store| store.strip_scan_file_lists(STRIP_BATCH))
            .await?;
        removed += self
            .blocking(|store| store.delete_unended_job_events())
            .await?;
        Ok(removed)
    }
}

async fn save_job(
    repositories: &SqliteRepositories,
    snapshot: &JobSnapshot,
) -> Result<(), PortError> {
    let snapshot = snapshot.clone();
    repositories
        .blocking(move |store| {
            store.upsert_job(&StoredJob {
                id: snapshot.id.to_string(),
                kind: serde_json::to_string(&snapshot.kind)?,
                state: serde_json::to_string(&snapshot.status)?,
                request: serde_json::Value::Null,
                result: Some(serde_json::to_value(&snapshot)?),
                idempotency_key: Some(snapshot.idempotency_key.to_string()),
                error: snapshot
                    .error
                    .as_ref()
                    .map(|message| serde_json::Value::String(message.clone())),
                created_at_ms: snapshot.created_utc.timestamp_millis(),
                updated_at_ms: Utc::now().timestamp_millis(),
            })
        })
        .await
}

fn decode_job(job: Option<StoredJob>) -> Result<Option<JobSnapshot>, StoreError> {
    job.map(|job| {
        let payload = job.result.ok_or_else(|| {
            StoreError::Json(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "job snapshot payload is missing",
            )))
        })?;
        serde_json::from_value(payload).map_err(StoreError::from)
    })
    .transpose()
}

#[async_trait]
impl RenameHistoryRepository for SqliteRepositories {
    async fn add(&self, record: &RenameBatchRecord) -> Result<(), PortError> {
        let record = record.clone();
        self.blocking(move |store| {
            let local = RenameBatch {
                id: record.id.to_string(),
                created_at_ms: record.created_at.timestamp_millis(),
                undone_at_ms: record.undone_at.map(|value| value.timestamp_millis()),
                provider: record
                    .provider
                    .map(|provider| serde_json::to_string(&provider))
                    .transpose()?
                    .unwrap_or_default(),
                template: record.template.clone(),
                entries: record
                    .entries
                    .iter()
                    .map(|entry| RenameBatchEntry {
                        original_path: entry.original_path.clone(),
                        renamed_path: entry.renamed_path.clone(),
                    })
                    .collect(),
            };
            store.record_rename_batch(&local)?;
            let payload = serde_json::to_string(&record)?;
            let connection = store.connection()?;
            connection.execute(
                "UPDATE rename_batches SET payload_json = ?2 WHERE id = ?1",
                params![record.id.to_string(), payload],
            )?;
            Ok(())
        })
        .await
    }

    async fn get(&self, id: RenameBatchId) -> Result<Option<RenameBatchRecord>, PortError> {
        self.blocking(move |store| load_rename_payload(&store, &id.to_string()))
            .await
    }

    async fn list_recent(&self, limit: usize) -> Result<Vec<RenameBatchRecord>, PortError> {
        self.blocking(move |store| {
            let limit = i64::try_from(limit).map_err(|_| StoreError::NumericOverflow)?;
            let connection = store.connection()?;
            let mut statement = connection.prepare(
                "SELECT payload_json FROM rename_batches WHERE payload_json IS NOT NULL
                 ORDER BY created_at_ms DESC LIMIT ?1",
            )?;
            let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
            rows.map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(StoreError::from)
            })
            .collect()
        })
        .await
    }

    async fn mark_undone(&self, id: RenameBatchId, at: DateTime<Utc>) -> Result<(), PortError> {
        let mut record = RenameHistoryRepository::get(self, id)
            .await?
            .ok_or_else(|| PortError::NotFound(format!("rename batch {id}")))?;
        record.undone_at = Some(at);
        RenameHistoryRepository::add(self, &record).await
    }

    async fn clear(&self) -> Result<u64, PortError> {
        self.blocking(|store| store.clear_rename_batches()).await
    }
}

fn load_rename_payload(
    store: &SqliteStore,
    id: &str,
) -> Result<Option<RenameBatchRecord>, StoreError> {
    let connection = store.connection()?;
    let payload = connection
        .query_row(
            "SELECT payload_json FROM rename_batches WHERE id = ?1",
            [id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    payload
        .map(|payload| serde_json::from_str(&payload).map_err(StoreError::from))
        .transpose()
}

#[async_trait]
impl OperationJournal for SqliteRepositories {
    async fn begin(&self, record: &JournalRecord) -> Result<(), PortError> {
        save_journal(self, record).await
    }

    async fn advance(&self, record: &JournalRecord) -> Result<(), PortError> {
        save_journal(self, record).await
    }

    async fn get(&self, key: &IdempotencyKey) -> Result<Option<JournalRecord>, PortError> {
        let key = key.to_string();
        self.blocking(move |store| {
            let connection = store.connection()?;
            let payload = connection
                .query_row(
                    "SELECT payload_json FROM mutation_journal WHERE idempotency_key = ?1",
                    [key],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            payload
                .map(|payload| serde_json::from_str(&payload).map_err(StoreError::from))
                .transpose()
        })
        .await
    }

    async fn list_incomplete(&self) -> Result<Option<Vec<JournalRecord>>, PortError> {
        self.blocking(|store| {
            let connection = store.connection()?;
            let mut statement = connection.prepare(
                "SELECT payload_json FROM mutation_journal
                 WHERE status IN ('\"prepared\"', '\"running\"')
                 ORDER BY updated_at_ms",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| {
                let payload = row?;
                serde_json::from_str(&payload).map_err(StoreError::from)
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
        })
        .await
    }
}

async fn save_journal(
    repositories: &SqliteRepositories,
    record: &JournalRecord,
) -> Result<(), PortError> {
    let record = record.clone();
    repositories
        .blocking(move |store| {
            let payload = serde_json::to_string(&record)?;
            let step = i64::try_from(record.step).map_err(|_| StoreError::NumericOverflow)?;
            let connection = store.connection()?;
            connection.execute(
                "INSERT INTO mutation_journal
                 (idempotency_key, plan_id, step, status, payload_json, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(idempotency_key) DO UPDATE SET plan_id=excluded.plan_id,
                 step=excluded.step, status=excluded.status, payload_json=excluded.payload_json,
                 updated_at_ms=excluded.updated_at_ms",
                params![
                    record.idempotency_key.to_string(),
                    record.plan_id.to_string(),
                    step,
                    serde_json::to_string(&record.status)?,
                    payload,
                    record.updated_utc.timestamp_millis(),
                ],
            )?;
            Ok(())
        })
        .await
}

#[async_trait]
impl OperationLog for SqliteRepositories {
    async fn append(&self, entry: &OperationLogEntry) -> Result<(), PortError> {
        let entry = entry.clone();
        self.blocking(move |store| {
            let connection = store.connection()?;
            connection.execute(
                "INSERT INTO operation_logs
                 (timestamp_ms, correlation_id, area, level, message, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    entry.timestamp_utc.timestamp_millis(),
                    entry.correlation_id.to_string(),
                    entry.area,
                    serde_json::to_string(&entry.level)?,
                    entry.message,
                    entry.detail,
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn query(&self, query: &LogQuery) -> Result<Vec<OperationLogEntry>, PortError> {
        let query = query.clone();
        self.blocking(move |store| {
            let connection = store.connection()?;
            let limit = i64::try_from(query.limit).map_err(|_| StoreError::NumericOverflow)?;
            let mut statement = connection.prepare(
                "SELECT timestamp_ms, correlation_id, area, level, message, detail
                 FROM operation_logs
                 WHERE (?1 IS NULL OR area = ?1)
                 ORDER BY timestamp_ms DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![query.area, limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?;
            let mut entries = Vec::new();
            for row in rows {
                let (timestamp, correlation, area, level, message, detail) = row?;
                let level: LogLevel = serde_json::from_str(&level)?;
                if query
                    .minimum_level
                    .is_some_and(|minimum| log_rank(level) < log_rank(minimum))
                {
                    continue;
                }
                entries.push(OperationLogEntry {
                    timestamp_utc: DateTime::from_timestamp_millis(timestamp).ok_or_else(|| {
                        StoreError::Json(serde_json::Error::io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid log timestamp",
                        )))
                    })?,
                    correlation_id: correlation.parse().map_err(|error| {
                        StoreError::Json(serde_json::Error::io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            error,
                        )))
                    })?,
                    area,
                    level,
                    message,
                    detail,
                });
            }
            Ok(entries)
        })
        .await
    }

    async fn clear(&self) -> Result<u64, PortError> {
        self.blocking(|store| {
            let connection = store.connection()?;
            let removed = connection.execute("DELETE FROM operation_logs", [])?;
            u64::try_from(removed).map_err(|_| StoreError::NumericOverflow)
        })
        .await
    }
}

const fn log_rank(level: LogLevel) -> u8 {
    match level {
        LogLevel::Trace => 0,
        LogLevel::Debug => 1,
        LogLevel::Information => 2,
        LogLevel::Warning => 3,
        LogLevel::Error => 4,
    }
}

fn to_cache_fingerprint(value: &FileFingerprint) -> CacheFingerprint {
    CacheFingerprint {
        path: value.path.clone(),
        file_size: value.size_bytes,
        modified_at_ns: value.modified_at.timestamp_nanos_opt().unwrap_or_else(|| {
            value
                .modified_at
                .timestamp_millis()
                .saturating_mul(1_000_000)
        }),
        quick_hash: value.quick_hash.clone(),
        tool_fingerprint: String::new(),
    }
}

fn store_port_error(error: StoreError) -> PortError {
    match error {
        StoreError::RevisionConflict { expected, actual } => PortError::Conflict(format!(
            "settings revision changed (expected {expected:?}, actual {actual})"
        )),
        StoreError::Sqlite(error)
            if matches!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ) =>
        {
            PortError::unavailable("SQLite database is busy", true)
        }
        error => PortError::Other(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn settings_repository_enforces_revisions() {
        let repositories =
            SqliteRepositories::from_store(SqliteStore::open_in_memory().expect("store"));
        let settings = AppSettings::default();
        let revision = SettingsRepository::save(&repositories, &settings, None)
            .await
            .expect("first save");
        assert_eq!(revision, 1);
        let error = SettingsRepository::save(&repositories, &settings, None)
            .await
            .expect_err("create-only save conflicts");
        assert!(matches!(error, PortError::Conflict(_)));
    }

    #[tokio::test]
    async fn unfinished_jobs_exclude_terminal_ones() {
        use mkvo_contracts::{JobKind, JobStatus};

        let repositories =
            SqliteRepositories::from_store(SqliteStore::open_in_memory().expect("store"));
        let statuses = [
            JobStatus::Queued,
            JobStatus::WaitingForResources,
            JobStatus::Running,
            JobStatus::Canceling,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Skipped,
            JobStatus::Canceled,
        ];
        for status in statuses {
            let snapshot = JobSnapshot {
                id: JobId::new(),
                kind: JobKind::Scan,
                status,
                correlation_id: mkvo_domain::CorrelationId::new(),
                idempotency_key: IdempotencyKey::generate(),
                request_fingerprint: String::new(),
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
            JobRepository::insert(&repositories, &snapshot)
                .await
                .expect("insert");
        }

        let unfinished = JobRepository::list_unfinished(&repositories)
            .await
            .expect("list");
        let found: std::collections::HashSet<_> = unfinished.iter().map(|job| job.status).collect();
        let expected: std::collections::HashSet<_> = statuses
            .into_iter()
            .filter(|status| !status.is_terminal())
            .collect();
        assert_eq!(unfinished.len(), expected.len());
        assert_eq!(found, expected);
    }

    fn job(status: mkvo_contracts::JobStatus) -> JobSnapshot {
        JobSnapshot {
            id: JobId::new(),
            kind: mkvo_contracts::JobKind::Scan,
            status,
            correlation_id: mkvo_domain::CorrelationId::new(),
            idempotency_key: IdempotencyKey::generate(),
            request_fingerprint: String::new(),
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
        }
    }

    #[tokio::test]
    async fn recent_jobs_are_listed_without_their_results() {
        use mkvo_contracts::JobStatus;

        let repositories =
            SqliteRepositories::from_store(SqliteStore::open_in_memory().expect("store"));
        let mut failed = job(JobStatus::Failed);
        failed.lines = vec!["first".to_owned(), "Skipped a.mkv: unreadable".to_owned()];
        failed.error = Some("mkvmerge exited with 2".to_owned());
        failed.result = Some(serde_json::json!({ "files": ["a".repeat(100_000)] }));
        failed.completed = 4;
        failed.total = 9;
        JobRepository::insert(&repositories, &failed)
            .await
            .expect("insert");

        let recent = JobRepository::list_recent(&repositories, 10)
            .await
            .expect("list");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, failed.id);
        assert_eq!(recent[0].status, JobStatus::Failed);
        assert_eq!((recent[0].completed, recent[0].total), (4, 9));
        assert_eq!(recent[0].summary, "Skipped a.mkv: unreadable");
        assert_eq!(recent[0].error.as_deref(), Some("mkvmerge exited with 2"));
        assert!(serde_json::to_string(&recent).expect("json").len() < 1_000);
    }

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    /// Stores `snapshot` as if it had last changed `age_days` ago.
    fn store_job_aged(repositories: &SqliteRepositories, snapshot: &JobSnapshot, age_days: i64) {
        repositories
            .store()
            .upsert_job(&StoredJob {
                id: snapshot.id.to_string(),
                kind: serde_json::to_string(&snapshot.kind).expect("kind"),
                state: serde_json::to_string(&snapshot.status).expect("state"),
                request: serde_json::Value::Null,
                result: Some(serde_json::to_value(snapshot).expect("snapshot")),
                idempotency_key: Some(snapshot.idempotency_key.to_string()),
                error: None,
                created_at_ms: 0,
                updated_at_ms: Utc::now().timestamp_millis() - age_days * DAY_MS,
            })
            .expect("store job");
    }

    fn journal(
        key: &IdempotencyKey,
        status: mkvo_application::JournalStatus,
        age_days: i64,
    ) -> JournalRecord {
        JournalRecord {
            idempotency_key: key.clone(),
            plan_id: PlanId::new(),
            step: 0,
            status,
            resources: Vec::new(),
            items: Vec::new(),
            detail: None,
            updated_utc: Utc::now() - chrono::TimeDelta::days(age_days),
        }
    }

    fn log(message: &str, age_days: i64) -> OperationLogEntry {
        OperationLogEntry {
            timestamp_utc: Utc::now() - chrono::TimeDelta::days(age_days),
            correlation_id: mkvo_domain::CorrelationId::new(),
            area: "Scan".to_owned(),
            level: LogLevel::Information,
            message: message.to_owned(),
            detail: String::new(),
        }
    }

    #[tokio::test]
    async fn pruning_removes_only_history_that_finished_before_the_cutoff() {
        use mkvo_application::JournalStatus;
        use mkvo_contracts::{JobEvent, JobStatus};

        let repositories =
            SqliteRepositories::from_store(SqliteStore::open_in_memory().expect("store"));

        let old_done = job(JobStatus::Completed);
        let old_failed = job(JobStatus::Failed);
        let old_running = job(JobStatus::Running);
        let recent_done = job(JobStatus::Completed);
        for (snapshot, age_days) in [
            (&old_done, 100),
            (&old_failed, 100),
            (&old_running, 100),
            (&recent_done, 1),
        ] {
            store_job_aged(&repositories, snapshot, age_days);
            JobRepository::append_event(
                &repositories,
                &JobEventEnvelope {
                    job_id: snapshot.id,
                    correlation_id: snapshot.correlation_id,
                    sequence: 0,
                    emitted_utc: Utc::now(),
                    event: JobEvent::Canceled {
                        message: String::new(),
                    },
                },
            )
            .await
            .expect("event");
        }

        let renamed = IdempotencyKey::parse("rename-finished").unwrap();
        let rename_crashed = IdempotencyKey::parse("rename-crashed").unwrap();
        let kept_journals = [
            // Recovery pairs an unfinished job with its journal, however old.
            journal(&old_running.idempotency_key, JournalStatus::Completed, 100),
            journal(&recent_done.idempotency_key, JournalStatus::Completed, 100),
            // A rename that stopped midway still needs a person to look at it.
            journal(&rename_crashed, JournalStatus::Running, 100),
        ];
        let pruned_journals = [
            journal(&old_done.idempotency_key, JournalStatus::Completed, 100),
            // Its job is forgotten, so it must not resurface as an orphan.
            journal(&old_failed.idempotency_key, JournalStatus::Running, 100),
            journal(&renamed, JournalStatus::Completed, 100),
        ];
        for record in kept_journals.iter().chain(&pruned_journals) {
            OperationJournal::begin(&repositories, record)
                .await
                .expect("journal");
        }
        for entry in [log("old", 100), log("recent", 1)] {
            OperationLog::append(&repositories, &entry)
                .await
                .expect("log");
        }

        let cutoff = Utc::now() - chrono::TimeDelta::days(90);
        let removed = JobRepository::prune_history(&repositories, Some(cutoff))
            .await
            .expect("prune");
        assert_eq!(removed, 6);

        for (snapshot, kept) in [
            (&old_done, false),
            (&old_failed, false),
            (&old_running, true),
            (&recent_done, true),
        ] {
            let stored = JobRepository::get(&repositories, snapshot.id)
                .await
                .expect("get");
            assert_eq!(stored.is_some(), kept, "{:?} job", snapshot.status);
            let events = repositories
                .store()
                .count_job_events(&snapshot.id.to_string())
                .expect("events");
            assert_eq!(events, u64::from(kept), "{:?} job events", snapshot.status);
        }
        for (records, kept) in [(&kept_journals, true), (&pruned_journals, false)] {
            for record in records {
                let stored = OperationJournal::get(&repositories, &record.idempotency_key)
                    .await
                    .expect("journal");
                assert_eq!(stored.is_some(), kept, "journal {}", record.idempotency_key);
            }
        }
        let logs = OperationLog::query(
            &repositories,
            &LogQuery {
                area: None,
                minimum_level: None,
                limit: 10,
            },
        )
        .await
        .expect("logs");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message, "recent");
    }

    fn event(
        job: &JobSnapshot,
        sequence: u64,
        event: mkvo_contracts::JobEvent,
    ) -> JobEventEnvelope {
        JobEventEnvelope {
            job_id: job.id,
            correlation_id: job.correlation_id,
            sequence,
            emitted_utc: Utc::now(),
            event,
        }
    }

    async fn stored_events(
        repositories: &SqliteRepositories,
        job: &JobSnapshot,
    ) -> Vec<serde_json::Value> {
        let id = job.id.to_string();
        repositories
            .blocking(move |store| {
                let connection = store.connection()?;
                let mut statement = connection.prepare(
                    "SELECT payload_json FROM job_events WHERE job_id = ?1 ORDER BY sequence",
                )?;
                let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
                rows.map(|row| serde_json::from_str(&row?).map_err(StoreError::from))
                    .collect()
            })
            .await
            .expect("events")
    }

    /// A database written by an older build: an event or two per scanned
    /// file, and the whole file list in both the result and the final event.
    /// None of it is read, so it goes even when history is kept forever.
    #[tokio::test]
    async fn pruning_clears_what_older_builds_stored_whatever_the_retention() {
        use mkvo_contracts::{JobEvent, JobLogLevel, JobStatus, JobSummary};

        let repositories =
            SqliteRepositories::from_store(SqliteStore::open_in_memory().expect("store"));
        let fat_result = serde_json::json!({
            "files": ["a".repeat(100_000)],
            "rows": ["b".repeat(100_000)],
            "skipped": ["broken.mkv: unreadable"],
            "summary": { "total": 1, "mkv": 1, "mp4": 0, "failed": 1, "cached": 0 },
        });
        let slim_result = serde_json::json!({
            "skipped": ["broken.mkv: unreadable"],
            "summary": { "total": 1, "mkv": 1, "mp4": 0, "failed": 1, "cached": 0 },
        });
        let mut scanned = job(JobStatus::Completed);
        scanned.result = Some(fat_result.clone());
        // The process died mid-scan, so there is no final event.
        let crashed = job(JobStatus::Failed);
        let current = job(JobStatus::Completed);
        for snapshot in [&scanned, &crashed, &current] {
            JobRepository::insert(&repositories, snapshot)
                .await
                .expect("insert");
        }

        let mut per_file = vec![JobEvent::StatusChanged {
            status: JobStatus::Running,
        }];
        for file in 1..=12 {
            per_file.push(JobEvent::Progress {
                completed: file,
                total: 12,
                current_file: "a.mkv".to_owned(),
                current_file_percent: 100,
            });
            per_file.push(JobEvent::Log {
                level: JobLogLevel::Warning,
                line: "Skipped a.mkv".to_owned(),
            });
        }
        let completed = |result| JobEvent::Completed {
            summary: JobSummary::default(),
            result,
        };
        let mut events = Vec::new();
        for (sequence, per_file_event) in (0..).zip(&per_file) {
            events.push(event(&scanned, sequence, per_file_event.clone()));
            events.push(event(&crashed, sequence, per_file_event.clone()));
        }
        events.push(event(&scanned, 25, completed(Some(fat_result))));
        events.push(event(&current, 7, completed(None)));
        for envelope in &events {
            JobRepository::append_event(&repositories, envelope)
                .await
                .expect("event");
        }

        let removed = JobRepository::prune_history(&repositories, None)
            .await
            .expect("prune");
        assert_eq!(removed, 50);

        let kept = stored_events(&repositories, &scanned).await;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["event"]["type"], "completed");
        assert_eq!(kept[0]["event"]["result"], slim_result);
        assert!(stored_events(&repositories, &crashed).await.is_empty());
        assert_eq!(stored_events(&repositories, &current).await.len(), 1);

        let stored = JobRepository::get(&repositories, scanned.id)
            .await
            .expect("get")
            .expect("history is kept forever, so the job stays");
        assert_eq!(stored.result, Some(slim_result));
    }

    /// The first prune of an old database has hundreds of thousands of rows
    /// to delete while requests are being served on the same connection.
    #[tokio::test]
    async fn job_events_are_deleted_in_bounded_batches() {
        use mkvo_contracts::{JobEvent, JobStatus};

        let repositories =
            SqliteRepositories::from_store(SqliteStore::open_in_memory().expect("store"));
        let scanned = job(JobStatus::Canceled);
        JobRepository::insert(&repositories, &scanned)
            .await
            .expect("insert");
        for sequence in 0..13 {
            let last = JobEvent::Canceled {
                message: String::new(),
            };
            let status = JobEvent::StatusChanged {
                status: JobStatus::Running,
            };
            let envelope = event(
                &scanned,
                sequence,
                if sequence == 12 { last } else { status },
            );
            JobRepository::append_event(&repositories, &envelope)
                .await
                .expect("event");
        }

        let store = repositories.store();
        let batches: Vec<u64> = (0..4)
            .map(|_| store.delete_superseded_job_events(5).expect("batch"))
            .collect();
        assert_eq!(batches, [5, 5, 2, 0]);
        let kept = stored_events(&repositories, &scanned).await;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["event"]["type"], "canceled");
    }

    #[tokio::test]
    async fn expired_plans_are_removed() {
        let repositories =
            SqliteRepositories::from_store(SqliteStore::open_in_memory().expect("store"));
        for (id, expires_at_ms) in [("expired", Some(10)), ("current", Some(i64::MAX))] {
            repositories
                .store()
                .put_plan(&StoredPlan {
                    id: id.to_owned(),
                    kind: "rename".to_owned(),
                    version: 1,
                    fingerprint: "abc".to_owned(),
                    payload: serde_json::json!({}),
                    created_at_ms: 1,
                    expires_at_ms,
                })
                .expect("plan");
        }
        let removed = PlanRepository::remove_expired(&repositories, Utc::now())
            .await
            .expect("remove");
        assert_eq!(removed, 1);
        assert!(
            repositories
                .store()
                .get_plan("current", 20)
                .expect("plan")
                .is_some()
        );
    }

    #[tokio::test]
    async fn incomplete_journals_are_enumerable_with_item_outcomes() {
        let repositories =
            SqliteRepositories::from_store(SqliteStore::open_in_memory().expect("store"));
        let mut record = JournalRecord {
            idempotency_key: IdempotencyKey::parse("journal-list").unwrap(),
            plan_id: PlanId::new(),
            step: 0,
            status: mkvo_application::JournalStatus::Running,
            resources: Vec::new(),
            items: vec![mkvo_application::JournalItemOutcome {
                key: "episode.mkv".to_owned(),
                status: mkvo_application::JournalItemStatus::Pending,
                detail: None,
            }],
            detail: None,
            updated_utc: Utc::now(),
        };
        OperationJournal::begin(&repositories, &record)
            .await
            .expect("begin");
        let incomplete = OperationJournal::list_incomplete(&repositories)
            .await
            .expect("list")
            .expect("supported");
        assert_eq!(incomplete, [record.clone()]);

        record.status = mkvo_application::JournalStatus::Completed;
        OperationJournal::advance(&repositories, &record)
            .await
            .expect("complete");
        assert!(
            OperationJournal::list_incomplete(&repositories)
                .await
                .expect("list")
                .expect("supported")
                .is_empty()
        );
    }
}
