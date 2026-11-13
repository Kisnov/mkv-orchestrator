use mkvo_domain::RenameBatchRecord;

use super::*;

impl SqliteStore {
    pub fn put_journal_entry(&self, entry: &JournalEntry) -> StoreResult<()> {
        let payload = serde_json::to_string(&entry.payload)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO operation_journal
             (job_id, step_index, step_kind, state, payload_json, started_at_ms, completed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(job_id, step_index) DO UPDATE SET step_kind=excluded.step_kind,
             state=excluded.state, payload_json=excluded.payload_json,
             started_at_ms=excluded.started_at_ms, completed_at_ms=excluded.completed_at_ms",
            params![
                entry.job_id,
                entry.step_index,
                entry.step_kind,
                entry.state,
                payload,
                entry.started_at_ms,
                entry.completed_at_ms,
            ],
        )?;
        Ok(())
    }

    pub fn journal_for_job(&self, job_id: &str) -> StoreResult<Vec<JournalEntry>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT step_index, step_kind, state, payload_json, started_at_ms, completed_at_ms
             FROM operation_journal WHERE job_id = ?1 ORDER BY step_index",
        )?;
        let rows = statement.query_map([job_id], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (step_index, step_kind, state, payload, started_at_ms, completed_at_ms) = row?;
            Ok(JournalEntry {
                job_id: job_id.to_owned(),
                step_index,
                step_kind,
                state,
                payload: serde_json::from_str(&payload)?,
                started_at_ms,
                completed_at_ms,
            })
        })
        .collect()
    }

    pub fn record_rename_batch(&self, batch: &RenameBatch) -> StoreResult<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO rename_batches (id, created_at_ms, undone_at_ms, provider, template, total_files)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET created_at_ms=excluded.created_at_ms,
             undone_at_ms=excluded.undone_at_ms, provider=excluded.provider,
             template=excluded.template, total_files=excluded.total_files",
            params![
                batch.id,
                batch.created_at_ms,
                batch.undone_at_ms,
                batch.provider,
                batch.template,
                batch.entries.len(),
            ],
        )?;
        transaction.execute(
            "DELETE FROM rename_entries WHERE batch_id = ?1",
            [&batch.id],
        )?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO rename_entries (batch_id, position, original_path, renamed_path) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (position, entry) in batch.entries.iter().enumerate() {
                insert.execute(params![
                    batch.id,
                    position,
                    path_text(&entry.original_path),
                    path_text(&entry.renamed_path),
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Store a batch and its entries in one transaction.
    pub fn save_rename_record(&self, record: &RenameBatchRecord) -> StoreResult<()> {
        let id = record.id.to_string();
        let provider = record
            .provider
            .map(|provider| serde_json::to_string(&provider))
            .transpose()?
            .unwrap_or_default();
        // The entries are rows of their own.
        let payload = serde_json::to_string(&RenameBatchRecord {
            entries: Vec::new(),
            ..record.clone()
        })?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO rename_batches
             (id, created_at_ms, undone_at_ms, provider, template, total_files, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)
             ON CONFLICT(id) DO UPDATE SET created_at_ms=excluded.created_at_ms,
             undone_at_ms=excluded.undone_at_ms, provider=excluded.provider,
             template=excluded.template, total_files=0, payload_json=excluded.payload_json",
            params![
                id,
                record.created_at.timestamp_millis(),
                record.undone_at.map(|value| value.timestamp_millis()),
                provider,
                record.template,
                payload,
            ],
        )?;
        transaction.execute("DELETE FROM rename_entries WHERE batch_id = ?1", [&id])?;
        for entry in &record.entries {
            append_rename_entry(&transaction, &id, entry)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Add one entry to a stored batch, without touching the ones it has.
    pub fn append_rename_record_entry(
        &self,
        batch_id: &str,
        entry: &mkvo_domain::RenameBatchEntry,
    ) -> StoreResult<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        append_rename_entry(&transaction, batch_id, entry)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn rename_record(&self, id: &str) -> StoreResult<Option<RenameBatchRecord>> {
        let connection = self.connection()?;
        let payload = connection
            .query_row(
                "SELECT payload_json FROM rename_batches WHERE id = ?1",
                [id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        payload
            .map(|payload| rename_record_from(&connection, &payload))
            .transpose()
    }

    pub fn recent_rename_records(&self, limit: usize) -> StoreResult<Vec<RenameBatchRecord>> {
        let limit = i64::try_from(limit).map_err(|_| StoreError::NumericOverflow)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload_json FROM rename_batches WHERE payload_json IS NOT NULL
             ORDER BY created_at_ms DESC LIMIT ?1",
        )?;
        let payloads = statement
            .query_map([limit], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        payloads
            .iter()
            .map(|payload| rename_record_from(&connection, payload))
            .collect()
    }

    pub fn list_rename_batches(&self, limit: usize) -> StoreResult<Vec<RenameBatch>> {
        let limit = i64::try_from(limit).map_err(|_| StoreError::NumericOverflow)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, created_at_ms, undone_at_ms, provider, template
             FROM rename_batches ORDER BY created_at_ms DESC LIMIT ?1",
        )?;
        let batch_rows = statement.query_map([limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut batches = Vec::new();
        for row in batch_rows {
            let (id, created_at_ms, undone_at_ms, provider, template) = row?;
            let entries = load_rename_entries(&connection, &id)?;
            batches.push(RenameBatch {
                id,
                created_at_ms,
                undone_at_ms,
                provider,
                template,
                entries,
            });
        }
        Ok(batches)
    }

    pub fn mark_rename_batch_undone(&self, id: &str, undone_at_ms: i64) -> StoreResult<bool> {
        let connection = self.connection()?;
        Ok(connection.execute(
            "UPDATE rename_batches SET undone_at_ms = ?2 WHERE id = ?1 AND undone_at_ms IS NULL",
            params![id, undone_at_ms],
        )? > 0)
    }

    /// Delete the batches applied before `cutoff_ms`, entries included.
    pub fn delete_rename_batches_before(&self, cutoff_ms: i64) -> StoreResult<u64> {
        let connection = self.connection()?;
        let removed = connection.execute(
            "DELETE FROM rename_batches WHERE created_at_ms < ?1",
            [cutoff_ms],
        )?;
        u64::try_from(removed).map_err(|_| StoreError::NumericOverflow)
    }

    pub fn clear_rename_batches(&self) -> StoreResult<u64> {
        let connection = self.connection()?;
        let removed = connection.execute("DELETE FROM rename_batches", [])?;
        u64::try_from(removed).map_err(|_| StoreError::NumericOverflow)
    }
}

fn append_rename_entry(
    transaction: &Transaction<'_>,
    batch_id: &str,
    entry: &mkvo_domain::RenameBatchEntry,
) -> StoreResult<()> {
    transaction.execute(
        "INSERT INTO rename_entries (batch_id, position, original_path, renamed_path, payload_json)
         SELECT ?1, COALESCE(MAX(position) + 1, 0), ?2, ?3, ?4
         FROM rename_entries WHERE batch_id = ?1",
        params![
            batch_id,
            path_text(&entry.original_path),
            path_text(&entry.renamed_path),
            serde_json::to_string(entry)?,
        ],
    )?;
    transaction.execute(
        "UPDATE rename_batches SET total_files = total_files + 1 WHERE id = ?1",
        [batch_id],
    )?;
    Ok(())
}

/// A batch stored before entries had payloads of their own carries them in its own.
fn rename_record_from(connection: &Connection, payload: &str) -> StoreResult<RenameBatchRecord> {
    let mut record: RenameBatchRecord = serde_json::from_str(payload)?;
    let mut statement = connection.prepare(
        "SELECT payload_json FROM rename_entries
         WHERE batch_id = ?1 AND payload_json IS NOT NULL ORDER BY position",
    )?;
    let entries = statement
        .query_map([record.id.to_string()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if !entries.is_empty() {
        record.entries = entries
            .iter()
            .map(|entry| serde_json::from_str(entry))
            .collect::<Result<_, _>>()?;
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use mkvo_application::RenameHistoryRepository;
    use mkvo_domain::{FileFingerprint, RenameBatchId, RenameSidecar};

    use super::*;
    use crate::SqliteRepositories;

    fn entry(name: &str) -> mkvo_domain::RenameBatchEntry {
        let original = PathBuf::from(format!("/library/{name}.mkv"));
        let fingerprint = |path: &Path| FileFingerprint {
            path: path.to_path_buf(),
            size_bytes: 1,
            modified_at: Utc::now(),
            quick_hash: None,
        };
        mkvo_domain::RenameBatchEntry {
            renamed_path: original.with_extension("renamed.mkv"),
            original_fingerprint: fingerprint(&original),
            renamed_fingerprint: Some(fingerprint(&original.with_extension("renamed.mkv"))),
            sidecars: vec![RenameSidecar {
                source: original.with_extension("srt"),
                target: original.with_extension("renamed.srt"),
            }],
            original_path: original,
        }
    }

    fn batch(entries: Vec<mkvo_domain::RenameBatchEntry>) -> RenameBatchRecord {
        RenameBatchRecord {
            id: RenameBatchId::new(),
            created_at: Utc::now(),
            undone_at: None,
            provider: None,
            template: "{title}".to_owned(),
            entries,
        }
    }

    fn count(repositories: &SqliteRepositories, sql: &str) -> i64 {
        repositories
            .store()
            .connection()
            .expect("connection")
            .query_row(sql, [], |row| row.get(0))
            .expect("count")
    }

    /// Every move used to store the whole batch again: all its rows, then its
    /// payload in a statement of its own.
    #[tokio::test]
    async fn a_move_adds_one_row_and_rewrites_nothing() {
        let repositories =
            SqliteRepositories::from_store(SqliteStore::open_in_memory().expect("store"));
        let mut stored = batch(vec![entry("a")]);
        repositories.add(&stored).await.expect("add");
        let payload = "SELECT length(payload_json) FROM rename_batches";
        let before = count(&repositories, payload);

        for name in ["b", "c"] {
            stored.entries.push(entry(name));
            repositories
                .append(stored.id, &stored.entries[stored.entries.len() - 1])
                .await
                .expect("append");
        }

        assert_eq!(count(&repositories, payload), before);
        assert_eq!(
            count(&repositories, "SELECT COUNT(*) FROM rename_entries"),
            3
        );
        assert_eq!(
            count(&repositories, "SELECT total_files FROM rename_batches"),
            3
        );
        assert_eq!(
            repositories.get(stored.id).await.expect("get"),
            Some(stored.clone())
        );
        assert_eq!(repositories.list_recent(10).await.expect("list"), [stored]);
    }

    #[tokio::test]
    async fn a_move_cannot_join_a_batch_that_is_gone() {
        let repositories =
            SqliteRepositories::from_store(SqliteStore::open_in_memory().expect("store"));
        let appended = repositories.append(RenameBatchId::new(), &entry("a")).await;
        assert!(appended.is_err());
        assert_eq!(
            count(&repositories, "SELECT COUNT(*) FROM rename_entries"),
            0
        );
    }

    /// Batches from before entries had payloads of their own keep them inside
    /// the batch payload, and have to stay undoable.
    #[tokio::test]
    async fn a_batch_stored_by_an_older_build_is_read_and_can_be_marked_undone() {
        let path = std::env::temp_dir().join(format!(
            "mkvo-rename-history-{}-{}.db",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().expect("clock")
        ));
        let mut old = batch(vec![entry("a"), entry("b")]);
        // Sidecars did not exist yet.
        for entry in &mut old.entries {
            entry.sidecars.clear();
        }
        {
            let mut connection = Connection::open(&path).expect("open");
            let transaction = connection.transaction().expect("transaction");
            migration_v1(&transaction).expect("v1");
            migration_v2(&transaction).expect("v2");
            transaction
                .pragma_update(None, "user_version", 2)
                .expect("version");
            transaction
                .execute(
                    "INSERT INTO rename_batches
                     (id, created_at_ms, provider, template, total_files, payload_json)
                     VALUES (?1, ?2, '', '{title}', 2, ?3)",
                    params![
                        old.id.to_string(),
                        old.created_at.timestamp_millis(),
                        serde_json::to_string(&old).expect("payload"),
                    ],
                )
                .expect("batch");
            for (position, entry) in old.entries.iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO rename_entries (batch_id, position, original_path, renamed_path)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![
                            old.id.to_string(),
                            position,
                            path_text(&entry.original_path),
                            path_text(&entry.renamed_path),
                        ],
                    )
                    .expect("entry");
            }
            transaction.commit().expect("commit");
        }

        let repositories = SqliteRepositories::open(&path).expect("migrate");
        assert_eq!(
            repositories.store().schema_version().expect("version"),
            SCHEMA_VERSION
        );
        assert_eq!(
            repositories.get(old.id).await.expect("get"),
            Some(old.clone())
        );
        let undone_at = Utc::now();
        repositories
            .mark_undone(old.id, undone_at)
            .await
            .expect("mark undone");
        old.undone_at = Some(undone_at);
        assert_eq!(
            repositories.list_recent(10).await.expect("list"),
            [old.clone()]
        );
        assert_eq!(
            count(&repositories, "SELECT COUNT(*) FROM rename_entries"),
            2
        );

        drop(repositories);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
