use super::*;

impl SqliteStore {
    pub fn put_plan(&self, plan: &StoredPlan) -> StoreResult<()> {
        let payload = serde_json::to_string(&plan.payload)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO plans (id, kind, version, fingerprint, payload_json, created_at_ms, expires_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, version=excluded.version,
             fingerprint=excluded.fingerprint, payload_json=excluded.payload_json,
             created_at_ms=excluded.created_at_ms, expires_at_ms=excluded.expires_at_ms",
            params![
                plan.id,
                plan.kind,
                plan.version,
                plan.fingerprint,
                payload,
                plan.created_at_ms,
                plan.expires_at_ms,
            ],
        )?;
        Ok(())
    }

    pub fn get_plan(&self, id: &str, at_ms: i64) -> StoreResult<Option<StoredPlan>> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT kind, version, fingerprint, payload_json, created_at_ms, expires_at_ms
                 FROM plans WHERE id = ?1 AND (expires_at_ms IS NULL OR expires_at_ms > ?2)",
                params![id, at_ms],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(kind, version, fingerprint, payload, created_at_ms, expires_at_ms)| {
                Ok(StoredPlan {
                    id: id.to_owned(),
                    kind,
                    version,
                    fingerprint,
                    payload: serde_json::from_str(&payload)?,
                    created_at_ms,
                    expires_at_ms,
                })
            },
        )
        .transpose()
    }

    pub fn upsert_job(&self, job: &StoredJob) -> StoreResult<()> {
        let request = serde_json::to_string(&job.request)?;
        let result = job.result.as_ref().map(serde_json::to_string).transpose()?;
        let error = job.error.as_ref().map(serde_json::to_string).transpose()?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO jobs
             (id, kind, state, request_json, result_json, idempotency_key, error_json, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, state=excluded.state,
             request_json=excluded.request_json, result_json=excluded.result_json,
             idempotency_key=excluded.idempotency_key, error_json=excluded.error_json,
             updated_at_ms=excluded.updated_at_ms",
            params![
                job.id,
                job.kind,
                job.state,
                request,
                result,
                job.idempotency_key,
                error,
                job.created_at_ms,
                job.updated_at_ms,
            ],
        )?;
        Ok(())
    }

    pub fn get_job(&self, id: &str) -> StoreResult<Option<StoredJob>> {
        self.query_job("id", id)
    }

    pub fn count_job_events(&self, job_id: &str) -> StoreResult<u64> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM job_events WHERE job_id = ?1",
            [job_id],
            |row| row.get(0),
        )?;
        u64::try_from(count).map_err(|_| StoreError::NumericOverflow)
    }

    /// Delete one batch of job events that are not the last of their job.
    ///
    /// Only the event that ends a job is stored now. Older builds stored one
    /// or more per scanned file, so this walks the index, not the payloads.
    pub fn delete_superseded_job_events(&self, limit: usize) -> StoreResult<u64> {
        let limit = i64::try_from(limit).map_err(|_| StoreError::NumericOverflow)?;
        let connection = self.connection()?;
        let removed = connection.execute(
            "DELETE FROM job_events WHERE rowid IN (
                SELECT event.rowid FROM job_events event
                WHERE event.sequence <
                    (SELECT MAX(sequence) FROM job_events WHERE job_id = event.job_id)
                LIMIT ?1)",
            [limit],
        )?;
        u64::try_from(removed).map_err(|_| StoreError::NumericOverflow)
    }

    /// Delete the last events left by jobs that never reported how they ended.
    /// Run it after `strip_scan_file_lists`, when every payload is small.
    pub fn delete_unended_job_events(&self) -> StoreResult<u64> {
        let connection = self.connection()?;
        let removed = connection.execute(
            "DELETE FROM job_events WHERE json_extract(payload_json, '$.event.type')
             NOT IN ('completed', 'failed', 'canceled')",
            [],
        )?;
        u64::try_from(removed).map_err(|_| StoreError::NumericOverflow)
    }

    /// Delete one batch of jobs that finished before `cutoff_ms`, each with
    /// its events and its journal. Jobs that never finished stay whatever
    /// their age: startup recovery reads them.
    pub fn delete_finished_jobs(&self, cutoff_ms: i64, limit: usize) -> StoreResult<u64> {
        let limit = i64::try_from(limit).map_err(|_| StoreError::NumericOverflow)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let jobs = {
            let mut statement = transaction.prepare(
                "SELECT id, idempotency_key FROM jobs
                 WHERE state IN ('\"Completed\"', '\"Failed\"', '\"Skipped\"', '\"Canceled\"')
                 AND updated_at_ms < ?1 LIMIT ?2",
            )?;
            let rows = statement.query_map([cutoff_ms, limit], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut removed = 0;
        for (id, idempotency_key) in jobs {
            removed += transaction.execute(
                "DELETE FROM mutation_journal WHERE idempotency_key = ?1",
                [idempotency_key],
            )?;
            removed += transaction.execute("DELETE FROM jobs WHERE id = ?1", [id])?;
        }
        transaction.commit()?;
        u64::try_from(removed).map_err(|_| StoreError::NumericOverflow)
    }

    /// Delete operation logs older than `cutoff_ms`, and the journals of
    /// operations without a job (renames) that finished before it. A journal
    /// that never finished stays: someone still has to look at it.
    pub fn delete_finished_journals_and_logs(&self, cutoff_ms: i64) -> StoreResult<u64> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut removed = transaction.execute(
            "DELETE FROM mutation_journal
             WHERE updated_at_ms < ?1 AND status NOT IN ('\"prepared\"', '\"running\"')
             AND idempotency_key NOT IN
             (SELECT idempotency_key FROM jobs WHERE idempotency_key IS NOT NULL)",
            [cutoff_ms],
        )?;
        removed += transaction.execute(
            "DELETE FROM operation_logs WHERE timestamp_ms < ?1",
            [cutoff_ms],
        )?;
        transaction.commit()?;
        u64::try_from(removed).map_err(|_| StoreError::NumericOverflow)
    }

    /// Rewrite one batch of scan results and completion events that still
    /// carry the file list older builds stored in them.
    pub fn strip_scan_file_lists(&self, limit: usize) -> StoreResult<u64> {
        let limit = i64::try_from(limit).map_err(|_| StoreError::NumericOverflow)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut stripped = transaction.execute(
            "UPDATE job_events SET payload_json =
                json_remove(payload_json, '$.event.result.files', '$.event.result.rows')
             WHERE rowid IN (
                SELECT event.rowid FROM job_events event
                JOIN jobs ON jobs.id = event.job_id
                WHERE jobs.kind = '\"scan\"'
                AND json_type(event.payload_json, '$.event.result.files') IS NOT NULL
                LIMIT ?1)",
            [limit],
        )?;
        stripped += transaction.execute(
            "UPDATE jobs SET result_json =
                json_remove(result_json, '$.result.files', '$.result.rows')
             WHERE id IN (
                SELECT id FROM jobs
                WHERE kind = '\"scan\"' AND json_type(result_json, '$.result.files') IS NOT NULL
                LIMIT ?1)",
            [limit],
        )?;
        transaction.commit()?;
        u64::try_from(stripped).map_err(|_| StoreError::NumericOverflow)
    }

    pub fn find_job_by_idempotency_key(&self, key: &str) -> StoreResult<Option<StoredJob>> {
        self.query_job("idempotency_key", key)
    }

    fn query_job(&self, column: &str, value: &str) -> StoreResult<Option<StoredJob>> {
        debug_assert!(matches!(column, "id" | "idempotency_key"));
        let sql = format!(
            "SELECT id, kind, state, request_json, result_json, idempotency_key, error_json, created_at_ms, updated_at_ms FROM jobs WHERE {column} = ?1"
        );
        let connection = self.connection()?;
        let row = connection
            .query_row(&sql, [value], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            })
            .optional()?;
        row.map(
            |(
                id,
                kind,
                state,
                request,
                result,
                idempotency_key,
                error,
                created_at_ms,
                updated_at_ms,
            )| {
                Ok(StoredJob {
                    id,
                    kind,
                    state,
                    request: serde_json::from_str(&request)?,
                    result: result.map(|json| serde_json::from_str(&json)).transpose()?,
                    idempotency_key,
                    error: error.map(|json| serde_json::from_str(&json)).transpose()?,
                    created_at_ms,
                    updated_at_ms,
                })
            },
        )
        .transpose()
    }
}
