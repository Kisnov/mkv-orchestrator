use std::time::Duration;

use chrono::{TimeDelta, Utc};

use super::MkvoRuntime;
use crate::RuntimeResult;

const PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

impl MkvoRuntime {
    /// Delete expired plans, and the finished jobs, journals and logs older
    /// than `rename.history_retention_days`. Zero days keeps them forever.
    /// Rename batches are left alone: undo reads them.
    pub async fn prune_history(&self) -> RuntimeResult<u64> {
        let dependencies = &self.inner.dependencies;
        let now = Utc::now();
        let mut removed = dependencies.plans.remove_expired(now).await?;
        let (settings, _revision) = dependencies.settings.load().await?;
        let days = settings.rename.history_retention_days;
        if days > 0 {
            let cutoff = now - TimeDelta::days(i64::from(days));
            removed += dependencies.jobs.prune_history(cutoff).await?;
        }
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
