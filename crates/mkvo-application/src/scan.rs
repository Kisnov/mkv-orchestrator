use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeDelta, Utc};
use futures::{StreamExt, stream::FuturesUnordered};
use mkvo_contracts::{MediaFileDto, ScanRequest, ScanResponse, ScanSummary};
use mkvo_domain::{FileFingerprint, MediaFile};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::{
    ApplicationError, ApplicationResult, AuthorizedPathPolicy, EnumeratedMedia, JobContext,
    MediaCatalog, MediaEnumerationRequest, MediaProbe, MetadataCache,
};

const MEDIA_CACHE_RETENTION_DAYS: i64 = 7;
const CACHE_BATCH: usize = 200;
const CACHE_FLUSH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanSkip {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanOutcome {
    pub files: Vec<MediaFile>,
    pub skipped: Vec<ScanSkip>,
    pub summary: ScanSummary,
}

impl ScanOutcome {
    #[must_use]
    pub fn into_response(self) -> ScanResponse {
        ScanResponse {
            files: self.files.iter().map(MediaFileDto::from).collect(),
            skipped: self
                .skipped
                .into_iter()
                .map(|skip| format!("{}: {}", skip.path.display(), skip.reason))
                .collect(),
            summary: self.summary,
        }
    }
}

#[async_trait]
pub trait ScanEventSink: Send + Sync {
    async fn progress(&self, completed: u64, total: u64, current: &str) -> ApplicationResult<()>;
    async fn skipped(&self, path: &str, reason: &str) -> ApplicationResult<()>;
}

pub struct NoopScanEventSink;

#[async_trait]
impl ScanEventSink for NoopScanEventSink {
    async fn progress(
        &self,
        _completed: u64,
        _total: u64,
        _current: &str,
    ) -> ApplicationResult<()> {
        Ok(())
    }

    async fn skipped(&self, _path: &str, _reason: &str) -> ApplicationResult<()> {
        Ok(())
    }
}

#[async_trait]
impl ScanEventSink for JobContext {
    async fn progress(&self, completed: u64, total: u64, current: &str) -> ApplicationResult<()> {
        JobContext::progress(self, completed, total, current, 100).await
    }

    async fn skipped(&self, path: &str, reason: &str) -> ApplicationResult<()> {
        self.record_skipped().await?;
        self.log(
            mkvo_contracts::JobLogLevel::Warning,
            format!("Skipped {path}: {reason}"),
        )
        .await
    }
}

/// Cache writes held back so that a batch is one SQLite transaction.
#[derive(Default)]
struct CacheWrites {
    probed: Vec<MediaFile>,
    seen: Vec<PathBuf>,
    probed_since: Option<Instant>,
}

impl CacheWrites {
    fn is_due(&self) -> bool {
        self.probed.len() + self.seen.len() >= CACHE_BATCH
            || self
                .probed_since
                .is_some_and(|since| since.elapsed() >= CACHE_FLUSH_INTERVAL)
    }
}

pub struct ScanService {
    catalog: Arc<dyn MediaCatalog>,
    probe: Arc<dyn MediaProbe>,
    cache: Arc<dyn MetadataCache>,
    paths: Arc<dyn AuthorizedPathPolicy>,
    default_workers: usize,
}

impl ScanService {
    #[must_use]
    pub fn new(
        catalog: Arc<dyn MediaCatalog>,
        probe: Arc<dyn MediaProbe>,
        cache: Arc<dyn MetadataCache>,
        paths: Arc<dyn AuthorizedPathPolicy>,
        default_workers: usize,
    ) -> Self {
        Self {
            catalog,
            probe,
            cache,
            paths,
            default_workers: default_workers.clamp(1, 8),
        }
    }

    pub async fn scan(
        &self,
        request: &ScanRequest,
        cancel: CancellationToken,
        sink: &dyn ScanEventSink,
    ) -> ApplicationResult<ScanOutcome> {
        let requested_sources = request.all_sources();
        if requested_sources.is_empty() {
            return Err(ApplicationError::InvalidRequest(
                "at least one scan source is required".to_owned(),
            ));
        }

        let mut roots = Vec::with_capacity(requested_sources.len());
        for source in requested_sources {
            if source.trim().is_empty() {
                continue;
            }
            let authorized = self
                .paths
                .authorize_read(PathBuf::from(source).as_path())
                .await?;
            roots.push(authorized);
        }
        roots.sort();
        roots.dedup();
        if roots.is_empty() {
            return Err(ApplicationError::InvalidRequest(
                "scan sources were blank".to_owned(),
            ));
        }

        let enumeration = MediaEnumerationRequest {
            roots,
            ignored_folder_names: request
                .ignored_folder_names
                .iter()
                .map(|value| value.trim().to_lowercase())
                .filter(|value| !value.is_empty())
                .collect(),
            supported_extensions: ["mkv", "mka", "webm", "mp4", "m4v"]
                .into_iter()
                .map(str::to_owned)
                .collect::<BTreeSet<_>>(),
        };
        let mut outcome = ScanOutcome::default();
        let mut writes = CacheWrites::default();
        let scanned = self
            .collect(
                &enumeration,
                request,
                &cancel,
                sink,
                &mut outcome,
                &mut writes,
            )
            .await;
        // A canceled scan still keeps what it probed.
        self.flush(&mut writes).await?;
        scanned?;

        // Files that moved away must not pile up in the cache. Everything found was
        // just marked seen, so this drops only what has been gone from these roots
        // for the whole retention.
        let cutoff = Utc::now() - TimeDelta::days(MEDIA_CACHE_RETENTION_DAYS);
        for root in &enumeration.roots {
            self.cache.remove_unseen_under(root, cutoff).await?;
        }
        outcome
            .files
            .sort_by(|left, right| left.path.cmp(&right.path));
        Ok(outcome)
    }

    async fn flush(&self, writes: &mut CacheWrites) -> ApplicationResult<()> {
        if !writes.probed.is_empty() {
            self.cache.upsert_many(&writes.probed).await?;
        }
        if !writes.seen.is_empty() {
            self.cache.mark_seen(&writes.seen, Utc::now()).await?;
        }
        *writes = CacheWrites::default();
        Ok(())
    }

    async fn collect(
        &self,
        enumeration: &MediaEnumerationRequest,
        request: &ScanRequest,
        cancel: &CancellationToken,
        sink: &dyn ScanEventSink,
        outcome: &mut ScanOutcome,
        writes: &mut CacheWrites,
    ) -> ApplicationResult<()> {
        let workers = request
            .max_workers
            .unwrap_or(self.default_workers)
            .clamp(1, 8);
        let (entries, mut found) = mpsc::unbounded_channel();
        let enumerating = async {
            self.catalog
                .enumerate_streaming(enumeration, cancel.clone(), entries)
                .await
                .map_err(ApplicationError::from)
        };
        // Probing starts on the first file found; `discovered` is the total so far.
        let probing = async {
            let mut queued = VecDeque::new();
            let mut work = FuturesUnordered::new();
            let mut discovered = 0_u64;
            let mut enumerated = false;
            loop {
                while work.len() < workers
                    && let Some(fingerprint) = queued.pop_front()
                {
                    work.push(self.resolve(fingerprint, request.force_refresh, cancel));
                }
                if enumerated && work.is_empty() {
                    return Ok(());
                }
                let result = tokio::select! {
                    biased;
                    () = cancel.cancelled() => return Err(ApplicationError::Canceled),
                    entry = found.recv(), if !enumerated => match entry {
                        Some(EnumeratedMedia::File(fingerprint)) => {
                            discovered += 1;
                            queued.push_back(fingerprint);
                            continue;
                        }
                        Some(EnumeratedMedia::Skipped { path, reason }) => {
                            discovered += 1;
                            Err((path, ApplicationError::AdapterFailure(reason)))
                        }
                        None => {
                            enumerated = true;
                            continue;
                        }
                    },
                    Some(result) = work.next() => result,
                };
                self.record(result, discovered, sink, outcome, writes)
                    .await?;
            }
        };
        tokio::try_join!(enumerating, probing).map(|_| ())
    }

    async fn resolve(
        &self,
        fingerprint: FileFingerprint,
        force_refresh: bool,
        cancel: &CancellationToken,
    ) -> Result<(MediaFile, bool), (PathBuf, ApplicationError)> {
        if !force_refresh {
            match self.cache.get_valid(&fingerprint).await {
                Ok(Some(file)) => return Ok((file, true)),
                Ok(None) => {}
                Err(error) => return Err((fingerprint.path, error.into())),
            }
        }
        match self.probe.inspect(&fingerprint.path, cancel.clone()).await {
            Ok(mut file) => {
                file.fingerprint = fingerprint;
                Ok((file, false))
            }
            Err(error) => Err((fingerprint.path, error.into())),
        }
    }

    async fn record(
        &self,
        result: Result<(MediaFile, bool), (PathBuf, ApplicationError)>,
        total: u64,
        sink: &dyn ScanEventSink,
        outcome: &mut ScanOutcome,
        writes: &mut CacheWrites,
    ) -> ApplicationResult<()> {
        match result {
            Ok((file, cached)) => {
                outcome.summary.total += 1;
                if cached {
                    outcome.summary.cached += 1;
                    writes.seen.push(file.fingerprint.path.clone());
                } else {
                    writes.probed.push(file.clone());
                    writes.probed_since.get_or_insert_with(Instant::now);
                }
                let extension = file.extension();
                if extension.eq_ignore_ascii_case("mkv") {
                    outcome.summary.mkv += 1;
                } else if extension.eq_ignore_ascii_case("mp4")
                    || extension.eq_ignore_ascii_case("m4v")
                {
                    outcome.summary.mp4 += 1;
                }
                let completed = outcome.summary.total + outcome.summary.failed;
                sink.progress(completed, total, &file.file_name()).await?;
                outcome.files.push(file);
            }
            Err((_path, ApplicationError::Canceled)) => {
                return Err(ApplicationError::Canceled);
            }
            Err((path, error)) => {
                outcome.summary.failed += 1;
                let reason = error.to_string();
                sink.skipped(&path.to_string_lossy(), &reason).await?;
                let completed = outcome.summary.total + outcome.summary.failed;
                sink.progress(completed, total, &path.to_string_lossy())
                    .await?;
                outcome.skipped.push(ScanSkip { path, reason });
            }
        }
        if writes.is_due() {
            self.flush(writes).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use mkvo_domain::{ContainerMetadata, FileFingerprint, MediaStatus};
    use tokio::sync::{Notify, RwLock};

    use super::*;
    use crate::PortError;

    struct Catalog(Vec<FileFingerprint>);
    #[async_trait]
    impl MediaCatalog for Catalog {
        async fn enumerate(
            &self,
            _request: &MediaEnumerationRequest,
            _cancel: CancellationToken,
        ) -> Result<Vec<FileFingerprint>, PortError> {
            Ok(self.0.clone())
        }
    }

    /// Withholds the rest of the listing until the first file is being probed.
    struct SlowCatalog(Arc<Probe>);
    #[async_trait]
    impl MediaCatalog for SlowCatalog {
        async fn enumerate(
            &self,
            _request: &MediaEnumerationRequest,
            _cancel: CancellationToken,
        ) -> Result<Vec<FileFingerprint>, PortError> {
            unreachable!("the scan streams")
        }
        async fn enumerate_streaming(
            &self,
            _request: &MediaEnumerationRequest,
            _cancel: CancellationToken,
            entries: mpsc::UnboundedSender<EnumeratedMedia>,
        ) -> Result<(), PortError> {
            let _ = entries.send(EnumeratedMedia::File(fingerprint("lib/1.mkv")));
            self.0.started.notified().await;
            let _ = entries.send(EnumeratedMedia::Skipped {
                path: PathBuf::from("lib/locked"),
                reason: "denied".to_owned(),
            });
            let _ = entries.send(EnumeratedMedia::File(fingerprint("lib/2.mkv")));
            Ok(())
        }
    }

    #[derive(Default)]
    struct Probe {
        calls: AtomicUsize,
        delay: Duration,
        started: Notify,
    }
    #[async_trait]
    impl MediaProbe for Probe {
        async fn inspect(
            &self,
            path: &Path,
            _cancel: CancellationToken,
        ) -> Result<MediaFile, PortError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.started.notify_one();
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            Ok(media(path))
        }
    }

    /// Entries carry the time they were last seen, like `scanned_at_ms`.
    #[derive(Default)]
    struct Cache {
        files: RwLock<HashMap<PathBuf, (MediaFile, DateTime<Utc>)>>,
        writes: AtomicUsize,
    }
    impl Cache {
        async fn seed(&self, path: &str, seen_at: DateTime<Utc>) {
            let file = media(Path::new(path));
            self.files
                .write()
                .await
                .insert(file.path.clone(), (file, seen_at));
        }
        async fn contains(&self, path: &str) -> bool {
            self.files.read().await.contains_key(Path::new(path))
        }
    }
    #[async_trait]
    impl MetadataCache for Cache {
        async fn get_valid(
            &self,
            fingerprint: &FileFingerprint,
        ) -> Result<Option<MediaFile>, PortError> {
            let files = self.files.read().await;
            Ok(files.get(&fingerprint.path).map(|(file, _)| file.clone()))
        }
        async fn upsert(&self, file: &MediaFile) -> Result<(), PortError> {
            self.upsert_many(std::slice::from_ref(file)).await
        }
        async fn upsert_many(&self, batch: &[MediaFile]) -> Result<(), PortError> {
            self.writes.fetch_add(1, Ordering::Relaxed);
            let mut files = self.files.write().await;
            for file in batch {
                files.insert(file.path.clone(), (file.clone(), Utc::now()));
            }
            Ok(())
        }
        async fn remove(&self, path: &Path) -> Result<bool, PortError> {
            Ok(self.files.write().await.remove(path).is_some())
        }
        async fn remove_under(&self, _root: &Path) -> Result<u64, PortError> {
            Ok(0)
        }
        async fn remove_older_than(&self, cutoff: DateTime<Utc>) -> Result<u64, PortError> {
            self.files
                .write()
                .await
                .retain(|_, (_, seen_at)| *seen_at >= cutoff);
            Ok(0)
        }
        async fn mark_seen(
            &self,
            paths: &[PathBuf],
            seen_at: DateTime<Utc>,
        ) -> Result<(), PortError> {
            let mut files = self.files.write().await;
            for path in paths {
                if let Some(entry) = files.get_mut(path) {
                    entry.1 = seen_at;
                }
            }
            Ok(())
        }
        async fn remove_unseen_under(
            &self,
            root: &Path,
            cutoff: DateTime<Utc>,
        ) -> Result<u64, PortError> {
            self.files
                .write()
                .await
                .retain(|path, (_, seen_at)| !path.starts_with(root) || *seen_at >= cutoff);
            Ok(0)
        }
        async fn count(&self) -> Result<u64, PortError> {
            Ok(self.files.read().await.len() as u64)
        }
        async fn list_under(&self, _root: &Path) -> Result<Vec<MediaFile>, PortError> {
            let files = self.files.read().await;
            Ok(files.values().map(|(file, _)| file.clone()).collect())
        }
    }

    struct Paths;
    #[async_trait]
    impl AuthorizedPathPolicy for Paths {
        async fn authorize_read(&self, path: &Path) -> Result<PathBuf, PortError> {
            Ok(path.to_owned())
        }
        async fn authorize_write(&self, path: &Path) -> Result<PathBuf, PortError> {
            Ok(path.to_owned())
        }
    }

    fn fingerprint(path: &str) -> FileFingerprint {
        FileFingerprint {
            path: PathBuf::from(path),
            size_bytes: 10,
            modified_at: Utc::now(),
            quick_hash: None,
        }
    }

    fn media(path: &Path) -> MediaFile {
        MediaFile {
            path: path.to_owned(),
            original_file_name: None,
            watch_root: None,
            relative_path: None,
            fingerprint: fingerprint(&path.to_string_lossy()),
            container: ContainerMetadata::default(),
            tracks: Vec::new(),
            attachments: Vec::new(),
            episode: None,
            provider_match: None,
            status: MediaStatus::Ready,
        }
    }

    fn service(found: &[&str], cache: &Arc<Cache>, probe: &Arc<Probe>) -> ScanService {
        let catalog = Catalog(found.iter().map(|path| fingerprint(path)).collect());
        ScanService::new(
            Arc::new(catalog),
            probe.clone(),
            cache.clone(),
            Arc::new(Paths),
            2,
        )
    }

    fn request(source: &str) -> ScanRequest {
        ScanRequest {
            source_path: Some(source.to_owned()),
            ..ScanRequest::default()
        }
    }

    async fn scan(
        source: &str,
        found: &[&str],
        cache: &Arc<Cache>,
        probe: &Arc<Probe>,
    ) -> ScanOutcome {
        service(found, cache, probe)
            .scan(
                &request(source),
                CancellationToken::new(),
                &NoopScanEventSink,
            )
            .await
            .unwrap()
    }

    struct CancelOnProgress(CancellationToken);
    #[async_trait]
    impl ScanEventSink for CancelOnProgress {
        async fn progress(&self, _done: u64, _total: u64, _current: &str) -> ApplicationResult<()> {
            self.0.cancel();
            Ok(())
        }
        async fn skipped(&self, _path: &str, _reason: &str) -> ApplicationResult<()> {
            Ok(())
        }
    }

    fn days_ago(days: i64) -> DateTime<Utc> {
        Utc::now() - TimeDelta::days(days)
    }

    #[tokio::test]
    async fn scan_uses_cache_and_probes_misses() {
        let cache = Arc::new(Cache::default());
        let probe = Arc::new(Probe::default());
        cache.seed("one.mkv", Utc::now()).await;
        let outcome = scan(".", &["one.mkv", "two.mp4"], &cache, &probe).await;
        assert_eq!(outcome.files.len(), 2);
        assert_eq!(outcome.summary.cached, 1);
        assert_eq!(outcome.summary.mkv, 1);
        assert_eq!(outcome.summary.mp4, 1);
        assert_eq!(probe.calls.load(Ordering::Relaxed), 1);
        assert!(cache.contains("two.mp4").await);
    }

    #[tokio::test]
    async fn probe_results_are_written_to_the_cache_in_batches() {
        let cache = Arc::new(Cache::default());
        let probe = Arc::new(Probe::default());
        let found: Vec<_> = (0..450).map(|n| format!("lib/{n:03}.mkv")).collect();
        let found: Vec<_> = found.iter().map(String::as_str).collect();

        let outcome = scan("lib", &found, &cache, &probe).await;
        assert_eq!(outcome.files.len(), 450);
        assert_eq!(cache.count().await.unwrap(), 450);
        assert_eq!(cache.writes.load(Ordering::Relaxed), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_probes_are_written_without_waiting_for_a_full_batch() {
        let cache = Arc::new(Cache::default());
        let probe = Arc::new(Probe {
            delay: CACHE_FLUSH_INTERVAL,
            ..Probe::default()
        });
        let found = ["lib/1.mkv", "lib/2.mkv", "lib/3.mkv", "lib/4.mkv"];

        scan("lib", &found, &cache, &probe).await;
        assert_eq!(cache.writes.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn probing_starts_while_enumeration_runs_and_unreadable_folders_are_skipped() {
        let cache = Arc::new(Cache::default());
        let probe = Arc::new(Probe::default());
        let service = ScanService::new(
            Arc::new(SlowCatalog(probe.clone())),
            probe,
            cache,
            Arc::new(Paths),
            2,
        );

        let request = request("lib");
        let scan = service.scan(&request, CancellationToken::new(), &NoopScanEventSink);
        let outcome = tokio::time::timeout(Duration::from_secs(10), scan)
            .await
            .expect("probing must not wait for the enumeration to end")
            .unwrap();
        assert_eq!(outcome.files.len(), 2);
        assert_eq!(outcome.summary.failed, 1);
        assert_eq!(
            outcome.skipped,
            [ScanSkip {
                path: PathBuf::from("lib/locked"),
                reason: "adapter failure: denied".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn a_canceled_scan_keeps_what_it_already_probed() {
        let cache = Arc::new(Cache::default());
        let probe = Arc::new(Probe::default());
        let cancel = CancellationToken::new();
        let found = ["lib/1.mkv", "lib/2.mkv", "lib/3.mkv", "lib/4.mkv"];

        let result = service(&found, &cache, &probe)
            .scan(&request("lib"), cancel.clone(), &CancelOnProgress(cancel))
            .await;
        assert!(matches!(result, Err(ApplicationError::Canceled)));
        assert!(cache.count().await.unwrap() >= 1);
    }

    #[tokio::test]
    async fn an_unchanged_file_is_not_probed_again_however_old_its_entry() {
        let cache = Arc::new(Cache::default());
        let probe = Arc::new(Probe::default());
        cache
            .seed("lib/one.mkv", days_ago(MEDIA_CACHE_RETENTION_DAYS + 1))
            .await;

        let outcome = scan("lib", &["lib/one.mkv"], &cache, &probe).await;
        assert_eq!(outcome.summary.cached, 1);
        assert_eq!(probe.calls.load(Ordering::Relaxed), 0);
        assert!(cache.contains("lib/one.mkv").await);
    }

    #[tokio::test]
    async fn scanning_one_folder_does_not_expire_the_cache_of_another() {
        let cache = Arc::new(Cache::default());
        let probe = Arc::new(Probe::default());
        cache
            .seed("lib/one.mkv", days_ago(MEDIA_CACHE_RETENTION_DAYS + 1))
            .await;

        scan("work", &["work/two.mkv"], &cache, &probe).await;
        assert!(cache.contains("lib/one.mkv").await);
    }

    #[tokio::test]
    async fn a_file_gone_from_a_scanned_folder_is_pruned_after_the_retention() {
        let cache = Arc::new(Cache::default());
        let probe = Arc::new(Probe::default());
        cache
            .seed("lib/gone.mkv", days_ago(MEDIA_CACHE_RETENTION_DAYS + 1))
            .await;
        cache.seed("lib/unreachable-today.mkv", days_ago(1)).await;

        scan("lib", &["lib/one.mkv"], &cache, &probe).await;
        assert!(!cache.contains("lib/gone.mkv").await);
        assert!(cache.contains("lib/unreachable-today.mkv").await);
    }
}
