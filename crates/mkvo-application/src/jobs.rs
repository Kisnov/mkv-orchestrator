use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use mkvo_contracts::{
    JobAccepted, JobCompletion, JobEvent, JobEventEnvelope, JobKind, JobLogLevel, JobSnapshot,
    JobStatus,
};
use mkvo_domain::{CorrelationId, IdempotencyKey, JobId, PlanId, ResourceAccess, ResourceClaim};
use tokio::sync::{Mutex, Notify, RwLock, broadcast};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{ApplicationError, ApplicationResult, JobRepository, PortError};

/// Progress lives in memory. While a job runs the repository is brought up to
/// date at most this often, and always when the job's status changes.
const PROGRESS_PERSIST_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub struct JobSpec {
    pub kind: JobKind,
    pub idempotency_key: IdempotencyKey,
    pub request_fingerprint: String,
    pub plan_id: Option<PlanId>,
    pub total: u64,
    pub resources: Vec<ResourceClaim>,
}

impl JobSpec {
    pub fn validate(&self) -> ApplicationResult<()> {
        if self.request_fingerprint.trim().is_empty() {
            return Err(ApplicationError::InvalidRequest(
                "job request fingerprint must not be empty".to_owned(),
            ));
        }
        Ok(())
    }
}

pub struct JobSupervisor {
    repository: Arc<dyn JobRepository>,
    registration: Mutex<()>,
    leases: Arc<ResourceLeaseManager>,
    live: RwLock<HashMap<JobId, Arc<LiveJob>>>,
    retained_lines: usize,
    completed_job_retention: Duration,
}

impl JobSupervisor {
    #[must_use]
    pub fn new(repository: Arc<dyn JobRepository>) -> Self {
        Self {
            repository,
            registration: Mutex::new(()),
            leases: Arc::new(ResourceLeaseManager::default()),
            live: RwLock::new(HashMap::new()),
            retained_lines: 500,
            completed_job_retention: Duration::from_secs(300),
        }
    }

    #[must_use]
    pub fn in_memory() -> Arc<Self> {
        Arc::new(Self::new(Arc::new(InMemoryJobRepository::default())))
    }

    #[must_use]
    pub fn with_retained_lines(mut self, retained_lines: usize) -> Self {
        self.retained_lines = retained_lines.max(1);
        self
    }

    /// Keep terminal jobs available for late event subscribers briefly, then
    /// rely on the durable repository for history and idempotent replay.
    #[must_use]
    pub fn with_completed_job_retention(mut self, retention: Duration) -> Self {
        self.completed_job_retention = retention;
        self
    }

    /// Run an application-owned operation while holding the same normalized
    /// read/write resource leases used by supervised background jobs.
    ///
    /// This is intended for short synchronous mutation workflows (for example,
    /// rename apply/undo) that must participate in job-level path exclusion but
    /// do not need their own persisted job record.
    pub async fn with_resource_lease<T, F, Fut>(
        &self,
        resources: &[ResourceClaim],
        cancellation: CancellationToken,
        operation: F,
    ) -> ApplicationResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ApplicationResult<T>>,
    {
        if cancellation.is_cancelled() {
            return Err(ApplicationError::Canceled);
        }
        let lease = self.leases.acquire(resources, cancellation.clone()).await?;
        if cancellation.is_cancelled() {
            drop(lease);
            return Err(ApplicationError::Canceled);
        }
        let result = operation().await;
        drop(lease);
        result
    }

    /// Register and spawn a job. A repeated idempotency key returns the original
    /// job without running `task` a second time.
    pub async fn start<F, Fut>(
        self: &Arc<Self>,
        spec: JobSpec,
        task: F,
    ) -> ApplicationResult<JobAccepted>
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = ApplicationResult<JobCompletion>> + Send + 'static,
    {
        spec.validate()?;
        // Registration is intentionally serialized: repository uniqueness and
        // the in-memory idempotency index become visible as one logical step.
        let _registration = self.registration.lock().await;

        if let Some(existing) = self
            .repository
            .find_by_idempotency(&spec.idempotency_key)
            .await?
        {
            if existing.request_fingerprint != spec.request_fingerprint {
                return Err(ApplicationError::Conflict(
                    "The idempotency key is already bound to a different request fingerprint."
                        .to_owned(),
                ));
            }
            return Ok(JobAccepted {
                id: existing.id,
                correlation_id: existing.correlation_id,
                status: existing.status,
                idempotent_replay: true,
            });
        }

        let id = JobId::new();

        let correlation_id = CorrelationId::new();
        let now = Utc::now();
        let snapshot = JobSnapshot {
            id,
            kind: spec.kind,
            status: JobStatus::Queued,
            correlation_id,
            idempotency_key: spec.idempotency_key.clone(),
            request_fingerprint: spec.request_fingerprint.clone(),
            plan_id: spec.plan_id,
            created_utc: now,
            started_utc: None,
            completed_utc: None,
            completed: 0,
            failed: 0,
            skipped: 0,
            total: spec.total,
            current_file: String::new(),
            current_file_percent: 0,
            lines: Vec::new(),
            result: None,
            error: None,
            revision: 0,
        };
        let (events, _) = broadcast::channel(256);
        let live = Arc::new(LiveJob {
            id,
            correlation_id,
            snapshot: Mutex::new(snapshot.clone()),
            cancel: CancellationToken::new(),
            events,
            sequence: AtomicU64::new(0),
            repository: Arc::clone(&self.repository),
            retained_lines: self.retained_lines,
            saved_at: Mutex::new(Instant::now()),
        });

        self.repository.insert(&snapshot).await?;
        self.live.write().await.insert(id, Arc::clone(&live));
        live.publish(JobEvent::Snapshot {
            snapshot: snapshot.clone(),
        });

        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            supervisor.run(live, spec.resources, task).await;
        });

        Ok(JobAccepted {
            id,
            correlation_id,
            status: JobStatus::Queued,
            idempotent_replay: false,
        })
    }

    async fn run<F, Fut>(
        self: Arc<Self>,
        live: Arc<LiveJob>,
        resources: Vec<ResourceClaim>,
        task: F,
    ) where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = ApplicationResult<JobCompletion>> + Send + 'static,
    {
        let id = live.id;
        // A terminal state the repository refused is only readable from memory.
        if Arc::clone(&self)
            .run_to_completion(live, resources, task)
            .await
            .is_err()
        {
            return;
        }
        tokio::time::sleep(self.completed_job_retention).await;
        self.live.write().await.remove(&id);
    }

    /// Runs the job to a terminal state. An error means that state could not
    /// be written to the repository.
    async fn run_to_completion<F, Fut>(
        self: Arc<Self>,
        live: Arc<LiveJob>,
        resources: Vec<ResourceClaim>,
        task: F,
    ) -> ApplicationResult<()>
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = ApplicationResult<JobCompletion>> + Send + 'static,
    {
        if !resources.is_empty()
            && let Err(error) = live.transition(JobStatus::WaitingForResources).await
        {
            return live.finish_unstarted(error).await;
        }

        let lease = match self.leases.acquire(&resources, live.cancel.clone()).await {
            Ok(lease) => lease,
            Err(ApplicationError::Canceled) => {
                return live
                    .finish_canceled("Canceled before resources became available")
                    .await;
            }
            Err(error) => return live.finish_failed(error.to_string()).await,
        };

        if let Err(error) = live.transition(JobStatus::Running).await {
            return live.finish_unstarted(error).await;
        }

        let context = JobContext {
            live: Arc::clone(&live),
        };
        // Spawned so a panic in the task is seen here instead of leaving the
        // job running forever.
        let outcome = match tokio::spawn(async move { task(context).await }).await {
            Ok(outcome) => outcome,
            Err(error) => Err(ApplicationError::Internal(format!("job {error}"))),
        };
        let finished = live.finish(outcome).await;
        drop(lease);
        finished
    }

    pub async fn get(&self, id: JobId) -> ApplicationResult<Option<JobSnapshot>> {
        if let Some(live) = self.live.read().await.get(&id).cloned() {
            return Ok(Some(live.snapshot.lock().await.clone()));
        }
        self.repository.get(id).await.map_err(Into::into)
    }

    pub async fn list_recent(&self, limit: usize) -> ApplicationResult<Vec<JobSnapshot>> {
        self.repository
            .list_recent(limit.clamp(1, 1_000))
            .await
            .map_err(Into::into)
    }

    pub async fn cancel(&self, id: JobId) -> ApplicationResult<JobSnapshot> {
        let live = self
            .live
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| ApplicationError::NotFound(format!("job {id}")))?;
        live.request_cancel().await
    }

    pub async fn subscribe(
        &self,
        id: JobId,
    ) -> ApplicationResult<broadcast::Receiver<JobEventEnvelope>> {
        self.live
            .read()
            .await
            .get(&id)
            .map(|job| job.events.subscribe())
            .ok_or_else(|| ApplicationError::NotFound(format!("live job {id}")))
    }
}

struct LiveJob {
    id: JobId,
    /// Duplicated from the snapshot so a running task can correlate its log
    /// entries without taking the snapshot lock on every write.
    correlation_id: CorrelationId,
    snapshot: Mutex<JobSnapshot>,
    cancel: CancellationToken,
    events: broadcast::Sender<JobEventEnvelope>,
    sequence: AtomicU64,
    repository: Arc<dyn JobRepository>,
    retained_lines: usize,
    /// When the snapshot last reached the repository. Held across each write
    /// so an older snapshot can never land after a newer one.
    saved_at: Mutex<Instant>,
}

impl LiveJob {
    async fn transition(&self, status: JobStatus) -> ApplicationResult<()> {
        let now = Utc::now();
        self.update(|snapshot| {
            if !snapshot.status.can_transition_to(status) {
                return Err(ApplicationError::Conflict(format!(
                    "invalid job transition {:?} -> {status:?}",
                    snapshot.status
                )));
            }
            snapshot.status = status;
            if status == JobStatus::Running && snapshot.started_utc.is_none() {
                snapshot.started_utc = Some(now);
            }
            Ok(())
        })
        .await?;
        self.publish(JobEvent::StatusChanged { status });
        Ok(())
    }

    async fn update(
        &self,
        mutate: impl FnOnce(&mut JobSnapshot) -> ApplicationResult<()>,
    ) -> ApplicationResult<JobSnapshot> {
        {
            let mut snapshot = self.snapshot.lock().await;
            mutate(&mut snapshot)?;
            snapshot.revision = snapshot.revision.saturating_add(1);
        }
        self.persist().await
    }

    /// Change progress in memory, saving it only once the last write is old
    /// enough: a scan reports every file, and a write per file starves it.
    async fn record(&self, mutate: impl FnOnce(&mut JobSnapshot)) -> ApplicationResult<()> {
        {
            let mut snapshot = self.snapshot.lock().await;
            mutate(&mut snapshot);
            snapshot.revision = snapshot.revision.saturating_add(1);
        }
        // A write already in flight is recent enough.
        if let Ok(mut saved_at) = self.saved_at.try_lock()
            && saved_at.elapsed() >= PROGRESS_PERSIST_INTERVAL
        {
            self.write(&mut saved_at).await?;
        }
        Ok(())
    }

    async fn persist(&self) -> ApplicationResult<JobSnapshot> {
        let mut saved_at = self.saved_at.lock().await;
        self.write(&mut saved_at).await
    }

    async fn write(&self, saved_at: &mut Instant) -> ApplicationResult<JobSnapshot> {
        let snapshot = self.snapshot.lock().await.clone();
        self.repository.update(&snapshot).await?;
        *saved_at = Instant::now();
        Ok(snapshot)
    }

    fn envelope(&self, event: JobEvent) -> JobEventEnvelope {
        JobEventEnvelope {
            job_id: self.id,
            correlation_id: self.correlation_id,
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            emitted_utc: Utc::now(),
            event,
        }
    }

    /// Tell live subscribers. Only the event that ends a job is stored: a
    /// scan would otherwise leave rows for every file it looked at.
    fn publish(&self, event: JobEvent) {
        let _ = self.events.send(self.envelope(event));
    }

    /// Cancel under the snapshot lock, where the runner also decides how the
    /// job ends, so neither can act on a status the other is about to change.
    async fn request_cancel(&self) -> ApplicationResult<JobSnapshot> {
        {
            let mut snapshot = self.snapshot.lock().await;
            if snapshot.status.is_terminal() {
                return Ok(snapshot.clone());
            }
            self.cancel.cancel();
            snapshot.status = JobStatus::Canceling;
            snapshot.revision = snapshot.revision.saturating_add(1);
        }
        let snapshot = self.persist().await?;
        self.publish(JobEvent::StatusChanged {
            status: JobStatus::Canceling,
        });
        Ok(snapshot)
    }

    /// Reach a terminal state. Subscribers are told even when the repository
    /// write fails; that error is returned so the job can stay in memory.
    async fn terminate(
        &self,
        apply: impl FnOnce(&mut JobSnapshot) -> JobEvent,
    ) -> ApplicationResult<()> {
        let event = {
            let mut snapshot = self.snapshot.lock().await;
            let event = apply(&mut snapshot);
            snapshot.completed_utc = Some(Utc::now());
            snapshot.current_file.clear();
            snapshot.current_file_percent = 0;
            snapshot.revision = snapshot.revision.saturating_add(1);
            event
        };
        let persisted = self.persist().await.map(drop);
        let envelope = self.envelope(event);
        let stored = self.repository.append_event(&envelope).await;
        let _ = self.events.send(envelope);
        persisted.and(stored.map_err(Into::into))
    }

    async fn finish(&self, outcome: ApplicationResult<JobCompletion>) -> ApplicationResult<()> {
        match outcome {
            Ok(completion) => {
                self.terminate(|snapshot| {
                    // A cancel that arrived while the task was returning.
                    if self.cancel.is_cancelled() {
                        snapshot.status = JobStatus::Canceled;
                        return JobEvent::Canceled {
                            message: completion
                                .message
                                .unwrap_or_else(|| "Canceled during execution".to_owned()),
                        };
                    }
                    snapshot.status = JobStatus::Completed;
                    snapshot.result.clone_from(&completion.result);
                    snapshot.lines.extend(completion.message);
                    JobEvent::Completed {
                        summary: snapshot.summary(),
                        result: completion.result,
                    }
                })
                .await
            }
            Err(ApplicationError::Canceled) => self.finish_canceled("Canceled").await,
            Err(error) => self.finish_failed(error.to_string()).await,
        }
    }

    /// The job could not move towards running: a cancel got there first, or
    /// the repository refused the write.
    async fn finish_unstarted(&self, error: ApplicationError) -> ApplicationResult<()> {
        if self.cancel.is_cancelled() {
            self.finish_canceled("Canceled before execution").await
        } else {
            self.finish_failed(error.to_string()).await
        }
    }

    async fn finish_failed(&self, message: String) -> ApplicationResult<()> {
        self.terminate(|snapshot| {
            snapshot.status = JobStatus::Failed;
            snapshot.error = Some(message.clone());
            JobEvent::Failed { message }
        })
        .await
    }

    async fn finish_canceled(&self, message: &str) -> ApplicationResult<()> {
        self.terminate(|snapshot| {
            snapshot.status = JobStatus::Canceled;
            JobEvent::Canceled {
                message: message.to_owned(),
            }
        })
        .await
    }
}

#[derive(Clone)]
pub struct JobContext {
    live: Arc<LiveJob>,
}

impl JobContext {
    /// Correlation identifier for this job, so work it performs can be traced
    /// back to the request that started it across logs, events, and errors.
    #[must_use]
    pub fn correlation_id(&self) -> CorrelationId {
        self.live.correlation_id
    }

    #[must_use]
    pub fn job_id(&self) -> JobId {
        self.live.id
    }

    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.live.cancel.clone()
    }

    pub fn ensure_not_canceled(&self) -> ApplicationResult<()> {
        if self.live.cancel.is_cancelled() {
            Err(ApplicationError::Canceled)
        } else {
            Ok(())
        }
    }

    pub async fn progress(
        &self,
        completed: u64,
        total: u64,
        current_file: impl Into<String>,
        current_file_percent: u8,
    ) -> ApplicationResult<()> {
        let current_file = current_file.into();
        self.live
            .record(|snapshot| {
                snapshot.completed = completed.min(total);
                snapshot.total = total;
                snapshot.current_file.clone_from(&current_file);
                snapshot.current_file_percent = current_file_percent.min(100);
            })
            .await?;
        self.live.publish(JobEvent::Progress {
            completed: completed.min(total),
            total,
            current_file,
            current_file_percent: current_file_percent.min(100),
        });
        Ok(())
    }

    pub async fn record_completed(&self) -> ApplicationResult<()> {
        self.live
            .record(|snapshot| {
                snapshot.completed = snapshot.completed.saturating_add(1).min(snapshot.total);
            })
            .await
    }

    pub async fn record_failed(&self) -> ApplicationResult<()> {
        self.live
            .record(|snapshot| {
                snapshot.failed = snapshot.failed.saturating_add(1);
            })
            .await
    }

    pub async fn record_skipped(&self) -> ApplicationResult<()> {
        self.live
            .record(|snapshot| {
                snapshot.skipped = snapshot.skipped.saturating_add(1);
            })
            .await
    }

    pub async fn log(&self, level: JobLogLevel, line: impl Into<String>) -> ApplicationResult<()> {
        let line = line.into();
        let limit = self.live.retained_lines;
        self.live
            .record(|snapshot| {
                snapshot.lines.push(line.clone());
                if snapshot.lines.len() > limit {
                    snapshot.lines.drain(0..snapshot.lines.len() - limit);
                }
            })
            .await?;
        self.live.publish(JobEvent::Log { level, line });
        Ok(())
    }

    pub async fn emit(&self, event: JobEvent) -> ApplicationResult<()> {
        self.live.publish(event);
        Ok(())
    }
}

#[derive(Default)]
struct ResourceLeaseManager {
    state: StdMutex<BTreeMap<PathBuf, LeaseState>>,
    changed: Notify,
}

#[derive(Clone, Copy, Debug, Default)]
struct LeaseState {
    readers: usize,
    writer: bool,
}

impl ResourceLeaseManager {
    async fn acquire(
        self: &Arc<Self>,
        claims: &[ResourceClaim],
        cancel: CancellationToken,
    ) -> ApplicationResult<ResourceLease> {
        let claims = merge_claims(claims);
        loop {
            let changed = self.changed.notified();
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if claims_available(&state, &claims) {
                    for (path, access) in &claims {
                        let lease = state.entry(path.clone()).or_default();
                        match access {
                            ResourceAccess::Read => lease.readers += 1,
                            ResourceAccess::Write => lease.writer = true,
                        }
                    }
                    return Ok(ResourceLease {
                        manager: Arc::clone(self),
                        claims,
                    });
                }
            }
            tokio::select! {
                () = cancel.cancelled() => return Err(ApplicationError::Canceled),
                () = changed => {}
            }
        }
    }

    fn release(&self, claims: &[(PathBuf, ResourceAccess)]) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (path, access) in claims {
            if let Some(lease) = state.get_mut(path) {
                match access {
                    ResourceAccess::Read => lease.readers = lease.readers.saturating_sub(1),
                    ResourceAccess::Write => lease.writer = false,
                }
                if lease.readers == 0 && !lease.writer {
                    state.remove(path);
                }
            }
        }
        drop(state);
        self.changed.notify_waiters();
    }
}

struct ResourceLease {
    manager: Arc<ResourceLeaseManager>,
    claims: Vec<(PathBuf, ResourceAccess)>,
}

impl Drop for ResourceLease {
    fn drop(&mut self) {
        self.manager.release(&self.claims);
    }
}

fn merge_claims(claims: &[ResourceClaim]) -> Vec<(PathBuf, ResourceAccess)> {
    let mut merged = BTreeMap::new();
    for claim in claims {
        let path = normalize_resource_path(&claim.path);
        merged
            .entry(path)
            .and_modify(|access| {
                if claim.access == ResourceAccess::Write {
                    *access = ResourceAccess::Write;
                }
            })
            .or_insert(claim.access);
    }
    merged.into_iter().collect()
}

fn claims_available(
    current: &BTreeMap<PathBuf, LeaseState>,
    requested: &[(PathBuf, ResourceAccess)],
) -> bool {
    requested.iter().all(|(path, access)| {
        current.iter().all(|(held_path, held)| {
            if !paths_overlap(path, held_path) {
                return true;
            }
            match access {
                ResourceAccess::Read => !held.writer,
                ResourceAccess::Write => !held.writer && held.readers == 0,
            }
        })
    })
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn normalize_resource_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    #[cfg(windows)]
    {
        PathBuf::from(normalized.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        normalized
    }
}

#[derive(Default)]
pub struct InMemoryJobRepository {
    jobs: RwLock<HashMap<JobId, JobSnapshot>>,
    keys: RwLock<HashMap<IdempotencyKey, JobId>>,
    events: RwLock<HashMap<JobId, Vec<JobEventEnvelope>>>,
}

#[async_trait]
impl JobRepository for InMemoryJobRepository {
    async fn insert(&self, snapshot: &JobSnapshot) -> Result<(), PortError> {
        let mut keys = self.keys.write().await;
        if let Some(existing) = keys.get(&snapshot.idempotency_key)
            && *existing != snapshot.id
        {
            return Err(PortError::Conflict(format!(
                "idempotency key already belongs to job {existing}"
            )));
        }
        keys.insert(snapshot.idempotency_key.clone(), snapshot.id);
        self.jobs
            .write()
            .await
            .insert(snapshot.id, snapshot.clone());
        Ok(())
    }

    async fn update(&self, snapshot: &JobSnapshot) -> Result<(), PortError> {
        let mut jobs = self.jobs.write().await;
        let current = jobs
            .get(&snapshot.id)
            .ok_or_else(|| PortError::NotFound(format!("job {}", snapshot.id)))?;
        if snapshot.revision < current.revision {
            return Err(PortError::Conflict("stale job revision".to_owned()));
        }
        jobs.insert(snapshot.id, snapshot.clone());
        Ok(())
    }

    async fn get(&self, id: JobId) -> Result<Option<JobSnapshot>, PortError> {
        Ok(self.jobs.read().await.get(&id).cloned())
    }

    async fn find_by_idempotency(
        &self,
        key: &IdempotencyKey,
    ) -> Result<Option<JobSnapshot>, PortError> {
        let id = self.keys.read().await.get(key).copied();
        Ok(match id {
            Some(id) => self.jobs.read().await.get(&id).cloned(),
            None => None,
        })
    }

    async fn list_recent(&self, limit: usize) -> Result<Vec<JobSnapshot>, PortError> {
        let mut jobs: Vec<_> = self.jobs.read().await.values().cloned().collect();
        jobs.sort_by_key(|job| std::cmp::Reverse(job.created_utc));
        jobs.truncate(limit);
        Ok(jobs)
    }

    async fn list_unfinished(&self) -> Result<Vec<JobSnapshot>, PortError> {
        Ok(self
            .jobs
            .read()
            .await
            .values()
            .filter(|job| !job.status.is_terminal())
            .cloned()
            .collect())
    }

    async fn append_event(&self, event: &JobEventEnvelope) -> Result<(), PortError> {
        self.events
            .write()
            .await
            .entry(event.job_id)
            .or_default()
            .push(event.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    fn spec(key: &str, resources: Vec<ResourceClaim>) -> JobSpec {
        JobSpec {
            kind: JobKind::Scan,
            idempotency_key: IdempotencyKey::parse(key).unwrap(),
            request_fingerprint: "request-hash".to_owned(),
            plan_id: None,
            total: 1,
            resources,
        }
    }

    #[tokio::test]
    async fn repeated_key_runs_task_once() {
        let supervisor = JobSupervisor::in_memory();
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&runs);
        let first = supervisor
            .start(spec("same-key", Vec::new()), move |_| async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(JobCompletion::default())
            })
            .await
            .unwrap();
        let second = supervisor
            .start(spec("same-key", Vec::new()), |_| async move {
                panic!("duplicate task must not run");
            })
            .await
            .unwrap();
        assert_eq!(first.id, second.id);
        assert!(second.idempotent_replay);
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn repeated_key_with_different_fingerprint_is_rejected() {
        let supervisor = JobSupervisor::in_memory();
        supervisor
            .start(spec("conflicting-key", Vec::new()), |_| async move {
                Ok(JobCompletion::default())
            })
            .await
            .unwrap();

        let mut conflicting = spec("conflicting-key", Vec::new());
        conflicting.request_fingerprint = "different-request-hash".to_owned();
        let error = supervisor
            .start(conflicting, |_| async move {
                panic!("conflicting task must not run");
            })
            .await
            .unwrap_err();

        assert!(matches!(error, ApplicationError::Conflict(_)));
        assert_eq!(
            error.to_string(),
            "conflict: The idempotency key is already bound to a different request fingerprint."
        );
    }

    #[tokio::test]
    async fn completed_jobs_leave_live_memory_but_remain_durable() {
        let supervisor = Arc::new(
            JobSupervisor::new(Arc::new(InMemoryJobRepository::default()))
                .with_completed_job_retention(Duration::from_millis(5)),
        );
        let accepted = supervisor
            .start(spec("evicted-key", Vec::new()), |_| async move {
                Ok(JobCompletion::default())
            })
            .await
            .unwrap();

        for _ in 0..20 {
            if !supervisor.live.read().await.contains_key(&accepted.id) {
                let snapshot = supervisor.get(accepted.id).await.unwrap().unwrap();
                assert_eq!(snapshot.status, JobStatus::Completed);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("terminal job was not evicted from live memory");
    }

    #[tokio::test]
    async fn cancellation_reaches_running_task() {
        let supervisor = JobSupervisor::in_memory();
        let accepted = supervisor
            .start(spec("cancel-key", Vec::new()), |context| async move {
                context.cancellation_token().cancelled().await;
                Err(ApplicationError::Canceled)
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        supervisor.cancel(accepted.id).await.unwrap();
        for _ in 0..20 {
            let snapshot = supervisor.get(accepted.id).await.unwrap().unwrap();
            if snapshot.status == JobStatus::Canceled {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("job did not reach canceled state");
    }

    async fn wait_for_status(supervisor: &JobSupervisor, id: JobId, status: JobStatus) {
        for _ in 0..400 {
            if supervisor.get(id).await.unwrap().unwrap().status == status {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let snapshot = supervisor.get(id).await.unwrap().unwrap();
        panic!("job stayed {:?} instead of {status:?}", snapshot.status);
    }

    #[tokio::test]
    async fn cancel_before_the_runner_starts_ends_canceled() {
        let supervisor = JobSupervisor::in_memory();
        let accepted = supervisor
            .start(
                spec("early-cancel", vec![ResourceClaim::read("library/show")]),
                |_| async move { panic!("canceled task must not run") },
            )
            .await
            .unwrap();
        // The runner has not been polled yet, so it finds the job Canceling.
        supervisor.cancel(accepted.id).await.unwrap();
        wait_for_status(&supervisor, accepted.id, JobStatus::Canceled).await;
    }

    #[tokio::test]
    async fn a_panicking_task_fails_the_job_and_frees_its_resources() {
        let supervisor = JobSupervisor::in_memory();
        let claim = vec![ResourceClaim::write("library/show")];
        let accepted = supervisor
            .start(spec("panic-key", claim.clone()), |_| async move {
                panic!("probe exploded")
            })
            .await
            .unwrap();
        wait_for_status(&supervisor, accepted.id, JobStatus::Failed).await;
        let snapshot = supervisor.get(accepted.id).await.unwrap().unwrap();
        assert!(snapshot.error.unwrap().contains("panicked"));

        let next = supervisor
            .start(spec("after-panic", claim), |_| async move {
                Ok(JobCompletion::default())
            })
            .await
            .unwrap();
        wait_for_status(&supervisor, next.id, JobStatus::Completed).await;
    }

    /// Counts snapshot writes and can refuse the terminal one.
    #[derive(Default)]
    struct ObservedRepository {
        inner: InMemoryJobRepository,
        updates: AtomicUsize,
        refuse_terminal: bool,
    }

    #[async_trait]
    impl JobRepository for ObservedRepository {
        async fn insert(&self, snapshot: &JobSnapshot) -> Result<(), PortError> {
            self.inner.insert(snapshot).await
        }

        async fn update(&self, snapshot: &JobSnapshot) -> Result<(), PortError> {
            if self.refuse_terminal && snapshot.status.is_terminal() {
                return Err(PortError::Other("disk full".to_owned()));
            }
            self.updates.fetch_add(1, Ordering::SeqCst);
            self.inner.update(snapshot).await
        }

        async fn get(&self, id: JobId) -> Result<Option<JobSnapshot>, PortError> {
            self.inner.get(id).await
        }

        async fn find_by_idempotency(
            &self,
            key: &IdempotencyKey,
        ) -> Result<Option<JobSnapshot>, PortError> {
            self.inner.find_by_idempotency(key).await
        }

        async fn list_recent(&self, limit: usize) -> Result<Vec<JobSnapshot>, PortError> {
            self.inner.list_recent(limit).await
        }

        async fn list_unfinished(&self) -> Result<Vec<JobSnapshot>, PortError> {
            self.inner.list_unfinished().await
        }

        async fn append_event(&self, event: &JobEventEnvelope) -> Result<(), PortError> {
            self.inner.append_event(event).await
        }
    }

    #[tokio::test]
    async fn an_unsaved_terminal_state_is_still_announced_and_readable() {
        let repository = Arc::new(ObservedRepository {
            refuse_terminal: true,
            ..ObservedRepository::default()
        });
        let supervisor = Arc::new(
            JobSupervisor::new(repository.clone())
                .with_completed_job_retention(Duration::from_millis(5)),
        );
        let accepted = supervisor
            .start(spec("unsaved-key", Vec::new()), |_| async move {
                Ok(JobCompletion::default())
            })
            .await
            .unwrap();
        let mut events = supervisor.subscribe(accepted.id).await.unwrap();

        let announced = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let JobEvent::Completed { .. } = events.recv().await.unwrap().event {
                    return;
                }
            }
        })
        .await;
        assert!(announced.is_ok(), "subscribers never saw the job finish");

        tokio::time::sleep(Duration::from_millis(50)).await;
        let stored = repository.get(accepted.id).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Running);
        let snapshot = supervisor.get(accepted.id).await.unwrap().unwrap();
        assert_eq!(snapshot.status, JobStatus::Completed);
    }

    fn discovered(path: &str) -> JobEvent {
        let file = mkvo_domain::MediaFile {
            path: PathBuf::from(path),
            original_file_name: None,
            watch_root: None,
            relative_path: None,
            fingerprint: mkvo_domain::FileFingerprint {
                path: PathBuf::from(path),
                size_bytes: 1,
                modified_at: Utc::now(),
                quick_hash: None,
            },
            container: mkvo_domain::ContainerMetadata::default(),
            tracks: Vec::new(),
            attachments: Vec::new(),
            episode: None,
            provider_match: None,
            status: mkvo_domain::MediaStatus::Ready,
        };
        JobEvent::MediaDiscovered {
            file: mkvo_contracts::MediaFileDto::from(&file),
        }
    }

    /// A scan reports every file. Neither the number of repository writes nor
    /// the stored events may grow with the number of files.
    #[tokio::test]
    async fn per_file_progress_is_not_written_per_file() {
        const FILES: u64 = 2_000;
        let repository = Arc::new(ObservedRepository::default());
        let supervisor = Arc::new(JobSupervisor::new(repository.clone()));
        let accepted = supervisor
            .start(spec("many-files", Vec::new()), |context| async move {
                for index in 1..=FILES {
                    context.emit(discovered("library/show/a.mkv")).await?;
                    context.progress(index, FILES, "a.mkv", 100).await?;
                    context.record_skipped().await?;
                    context.log(JobLogLevel::Warning, "skipped").await?;
                }
                Ok(JobCompletion::default())
            })
            .await
            .unwrap();
        let mut events = supervisor.subscribe(accepted.id).await.unwrap();
        wait_for_status(&supervisor, accepted.id, JobStatus::Completed).await;

        // Running, at most a handful of timed saves, and the terminal state.
        let updates = repository.updates.load(Ordering::SeqCst);
        assert!(updates <= 5, "{updates} snapshot writes for {FILES} files");
        let stored = repository.get(accepted.id).await.unwrap().unwrap();
        assert_eq!(stored.status, JobStatus::Completed);
        assert_eq!((stored.completed, stored.skipped), (FILES, FILES));

        let stored_events = repository.inner.events.read().await;
        let stored_events = &stored_events[&accepted.id];
        assert_eq!(stored_events.len(), 1);
        assert!(matches!(stored_events[0].event, JobEvent::Completed { .. }));

        // Live subscribers still get the stream.
        let mut live_events = 0;
        while let Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) = events.try_recv() {
            live_events += 1;
        }
        assert!(live_events > 1);
    }

    #[tokio::test(start_paused = true)]
    async fn progress_is_saved_once_the_last_write_is_old_enough() {
        let repository = Arc::new(ObservedRepository::default());
        let supervisor = Arc::new(JobSupervisor::new(repository.clone()));
        let (step, mut steps) = tokio::sync::mpsc::channel::<()>(1);
        let accepted = supervisor
            .start(spec("slow-file", Vec::new()), |context| async move {
                context.progress(1, 3, "a.mkv", 100).await?;
                tokio::time::sleep(PROGRESS_PERSIST_INTERVAL).await;
                context.progress(2, 3, "b.mkv", 100).await?;
                step.send(()).await.unwrap();
                std::future::pending::<()>().await;
                Ok(JobCompletion::default())
            })
            .await
            .unwrap();
        steps.recv().await.unwrap();

        let stored = repository.get(accepted.id).await.unwrap().unwrap();
        assert_eq!(stored.completed, 2);
        let live = supervisor.get(accepted.id).await.unwrap().unwrap();
        assert_eq!(live.current_file, "b.mkv");
    }

    #[tokio::test]
    async fn writer_waits_for_overlapping_reader() {
        let manager = Arc::new(ResourceLeaseManager::default());
        let reader = manager
            .acquire(
                &[ResourceClaim::read("library/show")],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let writer_manager = Arc::clone(&manager);
        let waiting = tokio::spawn(async move {
            writer_manager
                .acquire(
                    &[ResourceClaim::write("library/show/episode.mkv")],
                    CancellationToken::new(),
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!waiting.is_finished());
        drop(reader);
        assert!(waiting.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn synchronous_operation_uses_supervisor_resource_leases() {
        let supervisor = JobSupervisor::in_memory();
        let reader = supervisor
            .leases
            .acquire(
                &[ResourceClaim::read("library/show")],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let ran = Arc::new(AtomicUsize::new(0));
        let ran_in_operation = Arc::clone(&ran);
        let waiting_supervisor = Arc::clone(&supervisor);
        let waiting = tokio::spawn(async move {
            waiting_supervisor
                .with_resource_lease(
                    &[ResourceClaim::write("library/show/episode.mkv")],
                    CancellationToken::new(),
                    || async move {
                        ran_in_operation.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    },
                )
                .await
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        drop(reader);
        waiting.await.unwrap().unwrap();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn synchronous_operation_honors_cancellation_while_waiting() {
        let supervisor = JobSupervisor::in_memory();
        let writer = supervisor
            .leases
            .acquire(
                &[ResourceClaim::write("library/show")],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let cancellation = CancellationToken::new();
        let waiting_cancellation = cancellation.clone();
        let waiting_supervisor = Arc::clone(&supervisor);
        let waiting = tokio::spawn(async move {
            waiting_supervisor
                .with_resource_lease(
                    &[ResourceClaim::write("library/show/episode.mkv")],
                    waiting_cancellation,
                    || async {
                        Err::<(), _>(ApplicationError::Internal(
                            "canceled operation unexpectedly ran".to_owned(),
                        ))
                    },
                )
                .await
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        cancellation.cancel();
        assert!(matches!(
            waiting.await.unwrap(),
            Err(ApplicationError::Canceled)
        ));
        drop(writer);
    }
}
