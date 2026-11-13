use futures::{StreamExt, stream};

use super::*;

/// Files re-read at once after an operation; the scan workers' default.
const REFRESH_PROBES: usize = 4;

impl MkvoRuntime {
    pub async fn get_status(&self) -> RuntimeResult<AppStatus> {
        let tools = self.inner.dependencies.tools.all_statuses().await?;
        let (default_root, library_roots) = self.scan_locations().await;

        // The host's own roots come from how it was launched; the library roots
        // are the user's. Both are shortcuts the browser offers, so callers see
        // one list, with the user's own winning a name collision.
        // An unrestricted host's own media root is the placeholder the runtime
        // needs some path for, not a library anyone chose. Offering it as a
        // shortcut would put an empty internal folder where the user's library
        // belongs, which is the hardcoded root wearing a different hat.
        let placeholder = (self.inner.config.browse_scope == BrowseScope::Unrestricted)
            .then(|| self.inner.config.media_root.clone());
        let mut source_roots: Vec<SourceRoot> = self
            .inner
            .config
            .source_roots
            .iter()
            .filter(|(_, path)| {
                placeholder
                    .as_deref()
                    .is_none_or(|hidden| !same_path(path, hidden))
            })
            .map(|(name, path)| SourceRoot {
                name: name.clone(),
                path: display_path(path),
            })
            .collect();
        for root in &library_roots {
            let path = display_path(&root.path);
            source_roots.retain(|existing| !same_path(Path::new(&existing.path), Path::new(&path)));
            source_roots.push(SourceRoot {
                name: root.name.clone(),
                path,
            });
        }

        Ok(AppStatus {
            name: self.inner.config.app_name.clone(),
            version: self.inner.config.version.clone(),
            // Empty means "no library folder configured", which only an
            // unrestricted host can report. Callers open the volume list.
            media_root: self
                .effective_media_root(default_root.as_ref(), &library_roots)
                .as_deref()
                .map_or_else(String::new, display_path),
            config_root: display_path(&self.inner.config.config_root),
            source_roots,
            tools,
            contract_version: mkvo_domain::CONTRACT_VERSION,
        })
    }

    /// Where browsing starts, or `None` when the user has no library yet.
    ///
    /// A host launched pointing at a mount keeps that as its root. An
    /// unrestricted desktop has no such thing: choosing one on the user's
    /// behalf is exactly what the hardcoded Videos folder did. So it uses the
    /// configured default directory. Older settings that stored Home as the
    /// first library folder retain that fallback until their next save migrates
    /// the value.
    fn effective_media_root(
        &self,
        default_root: Option<&PathBuf>,
        library_roots: &[LibraryRoot],
    ) -> Option<PathBuf> {
        if self.inner.config.browse_scope == BrowseScope::Unrestricted {
            return default_root
                .cloned()
                .or_else(|| library_roots.first().map(|root| root.path.clone()));
        }
        Some(self.inner.config.media_root.clone())
    }

    async fn scan_locations(&self) -> (Option<PathBuf>, Vec<LibraryRoot>) {
        self.settings_service()
            .load()
            .await
            .map(|loaded| {
                (
                    loaded.settings.scan.default_root,
                    loaded.settings.scan.library_roots,
                )
            })
            .unwrap_or_default()
    }

    pub(super) async fn browse_file_system_impl(
        &self,
        path: Option<String>,
    ) -> RuntimeResult<FileSystemResponse> {
        // An explicitly empty path means "the top" — the volume list — and is
        // distinct from no path at all, which means "wherever the host starts".
        // Collapsing the two would make the volume list unreachable, since the
        // media root would answer for both.
        let unrestricted = self.inner.config.browse_scope == BrowseScope::Unrestricted;
        if unrestricted && path.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Ok(volume_listing());
        }

        // A bare `\\server` is not a directory, so `read_dir` would only ever
        // report that the network name cannot be found. Its shares have to be
        // enumerated instead, which is how the user reaches a share that no
        // drive letter points at.
        if unrestricted
            && let Some(requested) = path.as_deref()
            && let Some(UncTarget::Server(server)) = classify_unc(requested)
        {
            return server_listing(&server);
        }

        let requested = match path.filter(|value| !value.trim().is_empty()) {
            Some(value) => PathBuf::from(value),
            None => {
                let (default_root, library_roots) = self.scan_locations().await;
                match self.effective_media_root(default_root.as_ref(), &library_roots) {
                    Some(root) => root,
                    // Nothing configured: show what there is to choose from
                    // rather than inventing a folder.
                    None => return Ok(volume_listing()),
                }
            }
        };
        // Listing is read-only. An unrestricted host still validates every
        // mutation against the authorized roots, so widening what the browser
        // may *show* does not widen what it may change.
        let authorized = if unrestricted {
            requested.clone()
        } else {
            self.inner
                .dependencies
                .paths
                .authorize_read(&requested)
                .await?
        };

        if !authorized.is_dir() {
            return Err(RuntimeError::invalid(format!(
                "browse path is not a directory: {}",
                display_path(&authorized)
            )));
        }

        let mut entries = Vec::new();
        let mut directory = tokio::fs::read_dir(&authorized).await?;
        while let Some(entry) = directory.next_entry().await? {
            let metadata = match entry.metadata().await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            let kind = if metadata.is_dir() {
                FileSystemEntryKind::Folder
            } else if metadata.is_file() {
                FileSystemEntryKind::File
            } else {
                continue;
            };
            entries.push(FileSystemEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: display_path(&entry.path()),
                kind,
                size_bytes: metadata.is_file().then_some(metadata.len()),
                modified_utc: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH).into(),
            });
        }
        entries.sort_by(|left, right| {
            let left_folder = left.kind == FileSystemEntryKind::Folder;
            let right_folder = right.kind == FileSystemEntryKind::Folder;
            right_folder
                .cmp(&left_folder)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        let parent_path = match self.inner.config.browse_scope {
            // Confined hosts stop at their roots: offering a parent the caller
            // cannot then list would be a dead end.
            BrowseScope::AuthorizedRootsOnly => authorized.parent().and_then(|parent| {
                self.inner
                    .config
                    .authorized_roots
                    .iter()
                    .any(|(root, _)| parent.starts_with(root))
                    .then(|| display_path(parent))
            }),
            // Above a volume root is the volume list, addressed as the empty
            // path, so the user can always navigate all the way out.
            BrowseScope::Unrestricted => Some(match authorized.parent() {
                Some(parent) => display_path(parent),
                // Rust folds `\\server\share` into a single path prefix, so a
                // share root reports no parent even though the server above it
                // is real and listable. Falling through to the volume list here
                // would skip a level that a file manager shows.
                None => match classify_unc(&display_path(&authorized)) {
                    Some(UncTarget::Share { server }) => format!(r"\\{server}"),
                    _ => String::new(),
                },
            }),
        };
        Ok(FileSystemResponse {
            path: display_path(&authorized),
            parent_path,
            entries,
        })
    }

    pub async fn start_scan(&self, mut request: ScanRequest) -> RuntimeResult<ScanJobResponse> {
        if request.max_workers.is_none() {
            request.max_workers = Some(
                self.settings_service()
                    .load()
                    .await?
                    .settings
                    .workers
                    .max_scan_workers,
            );
        }
        if request.all_sources().is_empty() {
            let (default_root, library_roots) = self.scan_locations().await;
            let root = self
                .effective_media_root(default_root.as_ref(), &library_roots)
                .ok_or_else(|| {
                    RuntimeError::invalid(
                        "no folder to scan: choose one, or set the default directory in Settings",
                    )
                })?;
            request.source_path = Some(display_path(&root));
        }
        let request_fingerprint = stable_fingerprint(&request)
            .map_err(|error| RuntimeError::internal(error.to_string()))?;
        let key = IdempotencyKey::generate();
        let scan = Arc::clone(&self.inner.scan);
        let jobs = Arc::clone(&self.inner.jobs);
        let current = Arc::clone(&self.inner.current_scan);
        // Scanning is the most-run operation, so it has to reach the operation
        // log too; otherwise the Logs page stays empty through normal use.
        let logs = Arc::clone(&self.inner.dependencies.logs);
        let spec = JobSpec {
            kind: JobKind::Scan,
            idempotency_key: key,
            request_fingerprint,
            plan_id: None,
            total: 0,
            resources: request
                .all_sources()
                .into_iter()
                .map(mkvo_domain::ResourceClaim::read)
                .collect(),
        };
        let accepted = self
            .inner
            .jobs
            .start(spec, move |context| async move {
                let outcome = scan
                    .scan(&request, context.cancellation_token(), &context)
                    .await?;
                let state = scan_result_state(&outcome);
                let summary = outcome.summary;
                if request.keep_working_set {
                    // Attached before the scan is named, so an audit finds the files.
                    jobs.attach(context.job_id(), Arc::new(outcome.files)).await;
                    current.write().await.overview_job = Some(context.job_id());
                } else {
                    let mut current = current.write().await;
                    current.updated_utc = Some(Utc::now());
                    let replaced_job = current.scan_job.replace(context.job_id());
                    let replaced = current.apply_scan(outcome.files, summary);
                    // The earlier scan's client may not have asked for its
                    // files yet. Moved, not copied.
                    if let Some(job) = replaced_job {
                        jobs.attach(job, Arc::new(replaced)).await;
                    }
                }
                let detail = format!(
                    "{} file(s): {} MKV, {} MP4, {} cached, {} failed",
                    summary.total, summary.mkv, summary.mp4, summary.cached, summary.failed
                );
                if let Err(error) = logs
                    .append(&mkvo_contracts::OperationLogEntry {
                        timestamp_utc: Utc::now(),
                        correlation_id: context.correlation_id(),
                        area: "Scan".to_owned(),
                        level: if summary.failed > 0 {
                            mkvo_contracts::LogLevel::Warning
                        } else {
                            mkvo_contracts::LogLevel::Information
                        },
                        message: "Scan completed".to_owned(),
                        detail,
                    })
                    .await
                {
                    tracing::warn!(%error, "scan completion could not be logged");
                }
                Ok(JobCompletion {
                    result: Some(serde_json::to_value(state).map_err(|error| {
                        ApplicationError::Internal(format!(
                            "scan result serialization failed: {error}"
                        ))
                    })?),
                    message: Some("Scan completed".to_owned()),
                })
            })
            .await?;
        let snapshot = self
            .inner
            .jobs
            .get(accepted.id)
            .await?
            .ok_or_else(|| RuntimeError::internal("new scan job was not persisted"))?;
        self.scan_job_response(&snapshot).await
    }

    pub async fn get_scan_job(&self, id: &str) -> RuntimeResult<ScanJobResponse> {
        let id = parse_job_id(id)?;
        let snapshot = self
            .inner
            .jobs
            .get(id)
            .await?
            .ok_or_else(|| RuntimeError::not_found(format!("scan job {id}")))?;
        if snapshot.kind != JobKind::Scan {
            return Err(RuntimeError::not_found(format!("scan job {id}")));
        }
        self.scan_job_response(&snapshot).await
    }

    pub async fn cancel_scan(&self, id: &str) -> RuntimeResult<ScanJobResponse> {
        let id = parse_job_id(id)?;
        let job = self.inner.jobs.get(id).await?;
        if job.is_none_or(|job| job.kind != JobKind::Scan) {
            return Err(RuntimeError::not_found(format!("scan job {id}")));
        }
        let snapshot = self.inner.jobs.cancel(id).await?;
        self.scan_job_response(&snapshot).await
    }

    pub async fn get_current_scan_files(&self) -> RuntimeResult<CurrentScanResponse> {
        let state = self.inner.current_scan.read().await;
        Ok(current_scan_response(&state))
    }

    /// Replace the selected working set.
    ///
    /// Paths outside the current scan are rejected rather than stored: a
    /// selection is only meaningful against files the backend actually knows
    /// about, and accepting an unknown path would let a stale or crafted
    /// request reach an operation.
    ///
    /// The selection lives exactly as long as the working set it refers to.
    /// Both are in-memory, so a page reload keeps them and a host restart
    /// clears them together — persisting the selection alone would restore a
    /// set of paths with no files behind them.
    pub async fn set_file_selection(
        &self,
        request: mkvo_contracts::FileSelectionRequest,
    ) -> RuntimeResult<mkvo_contracts::FileSelectionResponse> {
        if request.replace_working_set {
            return Ok(self.replace_working_set(&request.paths).await);
        }
        let mut state = self.inner.current_scan.write().await;
        let available: BTreeSet<String> = state
            .files
            .iter()
            .map(|file| mkvo_application::paths::path_key(&file.path))
            .collect();

        let mut selected = BTreeSet::new();
        let mut unknown = Vec::new();
        for path in &request.paths {
            let key = mkvo_application::paths::path_key(std::path::Path::new(path));
            if available.contains(&key) {
                selected.insert(key);
            } else {
                unknown.push(path.clone());
            }
        }
        if !unknown.is_empty() {
            return Err(RuntimeError::invalid(format!(
                "{} selected path(s) are not in the current scan: {}",
                unknown.len(),
                unknown.join(", ")
            )));
        }

        state.selected = selected;
        Ok(mkvo_contracts::FileSelectionResponse {
            selected_paths: state.selected_display_paths(),
        })
    }

    /// Make exactly these files the working set, all of them selected.
    ///
    /// A title sent from the Library arrives here. Its scan left the working
    /// set alone, so a file the set does not hold is read: from the probe
    /// cache when that is current. One that cannot be read is left out.
    async fn replace_working_set(&self, paths: &[String]) -> mkvo_contracts::FileSelectionResponse {
        let key = mkvo_application::paths::path_key;
        let wanted: BTreeSet<String> = paths.iter().map(|path| key(Path::new(path))).collect();
        let mut files = self.working_set_by_key(key, &wanted).await;
        let missing: Vec<_> = paths
            .iter()
            .map(PathBuf::from)
            .filter(|path| !files.contains_key(&key(path)))
            .collect();
        let mut read = stream::iter(missing)
            .map(|path| self.read_file(path))
            .buffer_unordered(REFRESH_PROBES);
        while let Some(file) = read.next().await {
            if let Some(file) = file {
                files.insert(key(&file.path), file);
            }
        }
        let mut files: Vec<_> = files.into_values().collect();
        files.sort_by(|left, right| left.path.cmp(&right.path));

        let mut state = self.inner.current_scan.write().await;
        let replaced = state.replace_files(files);
        // The scan that produced them may not have been asked for its files yet.
        if let Some(job) = state.scan_job.take() {
            self.inner.jobs.attach(job, Arc::new(replaced)).await;
        }
        state.updated_utc = Some(Utc::now());
        mkvo_contracts::FileSelectionResponse {
            selected_paths: state.selected_display_paths(),
        }
    }

    /// One file as a scan would report it, or `None` when it cannot be read.
    async fn read_file(&self, path: PathBuf) -> Option<MediaFile> {
        let dependencies = &self.inner.dependencies;
        let read = async {
            let path = dependencies.paths.authorize_read(&path).await?;
            let fingerprint = dependencies.file_system.fingerprint(&path).await?;
            if let Some(cached) = dependencies.cache.get_valid(&fingerprint).await? {
                return Ok(cached);
            }
            let cancel = CancellationToken::new();
            let mut file = dependencies.probe.inspect(&path, cancel).await?;
            file.fingerprint = fingerprint;
            if let Err(error) = dependencies.cache.upsert(&file).await {
                tracing::warn!(path = %path.display(), %error, "file could not be cached");
            }
            Ok::<_, mkvo_application::PortError>(file)
        };
        read.await
            .inspect_err(|error| {
                tracing::debug!(path = %path.display(), %error, "file could not be read");
            })
            .ok()
    }

    /// Move the working set with files a rename just moved on disk.
    ///
    /// Without this the set still names paths that no longer exist, so the next
    /// operation resolves nothing and the dashboard shows the old names until a
    /// rescan.
    pub(crate) async fn apply_renames_to_working_set(&self, renames: &[(PathBuf, PathBuf)]) {
        {
            let mut state = self.inner.current_scan.write().await;
            state.apply_renames(renames);
            state.updated_utc = Some(Utc::now());
        }
        for (source, _) in renames {
            if let Err(error) = self.inner.dependencies.cache.remove(source).await {
                tracing::warn!(
                    path = %source.display(),
                    %error,
                    "renamed file's old cache entry could not be removed"
                );
            }
        }
        let targets: Vec<_> = renames.iter().map(|(_, target)| target.clone()).collect();
        self.refresh_working_set(&targets).await;
    }

    /// Re-read files an operation just changed, so the working set describes
    /// them as they are now rather than as they were when scanned.
    ///
    /// A property edit rewrites track names and languages in place: the paths
    /// are unchanged, so nothing else would notice, and the app would keep
    /// showing the old metadata until the user scanned again by hand. A file
    /// that can no longer be read is left as it was rather than dropped --
    /// a transient failure should not empty someone's working set.
    pub(crate) async fn refresh_working_set(&self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }

        let probed: Vec<MediaFile> = stream::iter(paths.to_vec())
            .map(|path| {
                let probe = Arc::clone(&self.inner.dependencies.probe);
                async move {
                    let inspected = probe.inspect(&path, CancellationToken::new()).await;
                    inspected
                        .inspect_err(|error| {
                            tracing::debug!(
                                path = %path.display(),
                                %error,
                                "file could not be re-read after an operation"
                            );
                        })
                        .ok()
                }
            })
            .buffer_unordered(REFRESH_PROBES)
            .filter_map(|file| async move { file })
            .collect()
            .await;

        for file in &probed {
            if let Err(error) = self.inner.dependencies.cache.upsert(file).await {
                tracing::warn!(
                    path = %file.path.display(),
                    %error,
                    "refreshed file metadata could not be cached"
                );
            }
        }

        let mut state = self.inner.current_scan.write().await;
        state.merge(probed);
        state.updated_utc = Some(Utc::now());
    }

    pub async fn clear_current_scan_files(&self) -> RuntimeResult<CurrentScanResponse> {
        let mut state = self.inner.current_scan.write().await;
        let cleared = std::mem::take(&mut *state);
        state.overview_job = cleared.overview_job;
        if let Some(job) = cleared.scan_job {
            self.inner.jobs.attach(job, Arc::new(cleared.files)).await;
        }
        Ok(current_scan_response(&state))
    }

    async fn scan_job_response(&self, snapshot: &JobSnapshot) -> RuntimeResult<ScanJobResponse> {
        let state = scan_state_from_snapshot(snapshot).unwrap_or_default();
        // The job keeps the counts. The files come from the working set while
        // it is still this scan's, and afterwards from what was handed to the
        // job, which lasts until the job leaves memory. The lock is held
        // across both lookups so a hand-over cannot fall between them.
        let current = self.inner.current_scan.read().await;
        let files = if snapshot.status != JobStatus::Completed {
            Vec::new()
        } else if current.scan_job == Some(snapshot.id) {
            current.files.iter().map(MediaFileRow::from).collect()
        } else {
            let handed_over = self.inner.jobs.attachment::<Vec<MediaFile>>(snapshot.id);
            handed_over
                .await
                .map(|files| files.iter().map(MediaFileRow::from).collect())
                .unwrap_or_default()
        };
        Ok(ScanJobResponse::from_snapshot(
            snapshot,
            files,
            state.skipped,
            state.summary,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use mkvo_application::{MediaProbe, PortError};
    use mkvo_domain::{MediaTrack, TrackKind};
    use mkvo_infra_sqlite::SqliteStore;

    use super::super::tests::media_at;
    use super::*;
    use crate::MkvoRuntimeBuilder;

    /// Stands in for mkvmerge: every file has the same three audio tracks,
    /// Japanese until `english` says the files were changed on disk.
    #[derive(Default)]
    struct Probe {
        english: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl MediaProbe for Probe {
        async fn inspect(
            &self,
            path: &Path,
            _cancel: CancellationToken,
        ) -> Result<MediaFile, PortError> {
            let english = self.english.load(std::sync::atomic::Ordering::SeqCst);
            let language = if english { "eng" } else { "jpn" };
            let mut file = media_at(&path.to_string_lossy());
            file.tracks = (0..3)
                .map(|id| MediaTrack {
                    mkvmerge_id: id,
                    propedit_track_number: 1,
                    kind: TrackKind::Audio,
                    codec: "AAC".to_owned(),
                    codec_id: None,
                    language: Some(language.to_owned()),
                    name: Some("Stereo".to_owned()),
                    resolution: None,
                    bit_depth: None,
                    hdr: None,
                    channels: Some(2),
                    sampling_frequency_hz: None,
                    default: false,
                    forced: false,
                    enabled: true,
                })
                .collect();
            Ok(file)
        }
    }

    /// A probe that takes a moment and counts how many run at once.
    #[derive(Default)]
    struct SlowProbe {
        running: std::sync::atomic::AtomicUsize,
        most_at_once: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl MediaProbe for SlowProbe {
        async fn inspect(
            &self,
            path: &Path,
            _cancel: CancellationToken,
        ) -> Result<MediaFile, PortError> {
            use std::sync::atomic::Ordering;
            let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.most_at_once.fetch_max(running, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.running.fetch_sub(1, Ordering::SeqCst);
            Ok(media_at(&path.to_string_lossy()))
        }
    }

    struct Host {
        _directory: tempfile::TempDir,
        library: PathBuf,
        database: PathBuf,
        runtime: MkvoRuntime,
    }

    fn host(files: usize) -> Host {
        host_with(files, Arc::new(Probe::default()))
    }

    /// A real database and a real folder of `files` MKVs; only the probe is faked.
    fn host_with(files: usize, probe: Arc<dyn MediaProbe>) -> Host {
        let directory = tempfile::tempdir().unwrap();
        let library = directory.path().join("library");
        let config = directory.path().join("config");
        std::fs::create_dir_all(&library).unwrap();
        for index in 0..files {
            std::fs::write(library.join(format!("episode-{index:04}.mkv")), b"mkv").unwrap();
        }
        let composed = MkvoRuntimeBuilder::new(&library, &config)
            .app_name("test")
            .disable_legacy_migration()
            .authorized_root(&library, true)
            .build()
            .unwrap();
        let mut dependencies = composed.inner.dependencies.clone();
        dependencies.probe = probe;
        Host {
            _directory: directory,
            library,
            database: config.join("mkvo.db"),
            runtime: MkvoRuntime::from_parts(composed.inner.config.clone(), dependencies),
        }
    }

    async fn start(host: &Host, folder: &Path) -> JobId {
        let request = ScanRequest {
            source_path: Some(display_path(folder)),
            ..ScanRequest::default()
        };
        host.runtime.start_scan(request).await.unwrap().id
    }

    async fn finished(host: &Host, id: JobId) -> ScanJobResponse {
        for _ in 0..4_000 {
            let job = host.runtime.get_scan_job(&id.to_string()).await.unwrap();
            if job.status.is_terminal() {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("scan did not finish");
    }

    async fn scan(host: &Host) -> ScanJobResponse {
        let id = start(host, &host.library).await;
        finished(host, id).await
    }

    /// The scan status still answers with every file, but what the scan
    /// leaves in the database must not depend on how many there were.
    #[tokio::test]
    async fn what_a_scan_stores_does_not_grow_with_the_library() {
        let mut stored = Vec::new();
        for files in [3, 300] {
            let host = host(files);
            let job = scan(&host).await;
            assert_eq!(job.status, JobStatus::Completed);
            assert_eq!(job.files.len(), files);
            assert_eq!(job.summary.total, files as u64);

            // The status is answered from memory a moment before the final
            // state and its event reach the database.
            let store = SqliteStore::open(&host.database).unwrap();
            let id = job.id.to_string();
            for _ in 0..400 {
                if store.count_job_events(&id).unwrap() > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let row = store.get_job(&id).unwrap().unwrap();
            assert_eq!(row.state, "\"Completed\"");
            stored.push((
                serde_json::to_string(&row.result).unwrap().len(),
                store.count_job_events(&id).unwrap(),
            ));
        }
        let [(small_row, small_events), (large_row, large_events)] = stored[..] else {
            unreachable!()
        };
        assert_eq!((small_events, large_events), (1, 1), "job_events rows");
        assert!(
            large_row < small_row + 256,
            "the job row grew from {small_row} to {large_row} bytes"
        );
    }

    /// Every checkbox click is one of these, so it must not carry the library.
    #[tokio::test]
    async fn a_selection_update_answers_with_the_selection_only() {
        let mut sizes = Vec::new();
        for files in [3, 300] {
            let host = host(files);
            let first = scan(&host).await.files.swap_remove(0).path;
            let request = mkvo_contracts::FileSelectionRequest {
                paths: vec![first.clone()],
                ..Default::default()
            };
            let response = host.runtime.set_file_selection(request).await.unwrap();
            assert_eq!(response.selected_paths, [first]);
            sizes.push(serde_json::to_string(&response).unwrap().len());
        }
        assert!(sizes[1] < sizes[0] + 64, "{sizes:?}");
    }

    #[tokio::test]
    async fn a_finished_scan_is_still_readable_after_a_restart() {
        let host = host(3);
        let job = scan(&host).await;

        let restarted = MkvoRuntime::from_parts(
            host.runtime.inner.config.clone(),
            host.runtime.inner.dependencies.clone(),
        );
        let after = restarted.get_scan_job(&job.id.to_string()).await.unwrap();
        assert_eq!(after.status, JobStatus::Completed);
        assert_eq!(after.summary, job.summary);
        // The working set did not survive the restart, so there are no files.
        assert!(after.files.is_empty());
    }

    /// Two clients, two scans. Whichever finishes first has lost the working
    /// set to the other by the time its client asks, and an empty answer
    /// there would be saved by the Library page as an empty library.
    #[tokio::test]
    async fn scans_that_finish_together_each_serve_their_own_files() {
        let host = host(0);
        let mut scans = Vec::new();
        for (name, files) in [("anime", 2), ("films", 3)] {
            let folder = host.library.join(name);
            std::fs::create_dir_all(&folder).unwrap();
            for index in 0..files {
                std::fs::write(folder.join(format!("{index}.mkv")), b"mkv").unwrap();
            }
            scans.push((start(&host, &folder).await, name, files));
        }
        for (id, _, _) in &scans {
            finished(&host, *id).await;
        }

        let assert_own_files = || async {
            for (id, name, files) in &scans {
                let job = host.runtime.get_scan_job(&id.to_string()).await.unwrap();
                assert_eq!(job.files.len(), *files, "{name}");
                assert!(job.files.iter().all(|file| file.path.contains(name)));
            }
        };
        assert_own_files().await;
        // Clearing the working set must not empty an answer either.
        host.runtime.clear_current_scan_files().await.unwrap();
        assert_own_files().await;
    }

    /// `files` MKVs in a folder of their own under the library.
    fn folder(host: &Host, name: &str, files: usize) -> PathBuf {
        let folder = host.library.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        for index in 0..files {
            std::fs::write(folder.join(format!("{index}.mkv")), b"mkv").unwrap();
        }
        folder
    }

    /// The Library's overview scan, which leaves the working set alone.
    async fn overview(host: &Host, folder: &Path) -> ScanJobResponse {
        let request = ScanRequest {
            source_path: Some(display_path(folder)),
            keep_working_set: true,
            ..ScanRequest::default()
        };
        let id = host.runtime.start_scan(request).await.unwrap().id;
        finished(host, id).await
    }

    /// The Dashboard's files and selection are the user's work in progress;
    /// reading every library for the overview used to replace them.
    #[tokio::test]
    async fn a_library_overview_scan_leaves_the_working_set_alone() {
        let host = host(0);
        let dashboard = folder(&host, "dashboard", 2);
        let library = folder(&host, "films", 3);
        let id = start(&host, &dashboard).await;
        let mut working = finished(&host, id).await.files;
        let selection = mkvo_contracts::FileSelectionRequest {
            paths: vec![working.swap_remove(0).path],
            ..Default::default()
        };
        host.runtime.set_file_selection(selection).await.unwrap();
        let before = host.runtime.get_current_scan_files().await.unwrap();

        let scanned = overview(&host, &library).await;

        assert_eq!(scanned.files.len(), 3);
        assert!(scanned.files.iter().all(|file| file.path.contains("films")));
        assert_eq!(host.runtime.get_current_scan_files().await.unwrap(), before);
        assert_eq!(before.files.len(), 2);
        assert_eq!(before.selected_paths.len(), 1);
    }

    /// The Library audits what it just scanned by path alone.
    #[tokio::test]
    async fn the_files_of_an_overview_scan_can_be_audited_by_path() {
        let host = host(0);
        let scanned = overview(&host, &folder(&host, "films", 3)).await;
        let request = crate::compat::LibraryAuditRequest {
            paths: scanned.files.into_iter().map(|file| file.path).collect(),
            files: Vec::new(),
        };

        let audit = host.runtime.run_library_audit(request).await.unwrap();

        assert_eq!(audit.summary.files, 3);
    }

    /// A title sent to the Dashboard leaves copies in the working set. When
    /// the Library rebuilds it later, the audit is of what was just read.
    #[tokio::test]
    async fn an_audit_reads_the_overview_scan_before_the_working_set() {
        let probe = Arc::new(Probe::default());
        let host = host_with(0, probe.clone());
        let films = folder(&host, "films", 2);
        let id = start(&host, &films).await;
        finished(&host, id).await;

        probe
            .english
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let rebuild = ScanRequest {
            source_path: Some(display_path(&films)),
            keep_working_set: true,
            force_refresh: true,
            ..ScanRequest::default()
        };
        let id = host.runtime.start_scan(rebuild).await.unwrap().id;
        let request = crate::compat::LibraryAuditRequest {
            paths: finished(&host, id)
                .await
                .files
                .into_iter()
                .map(|file| file.path)
                .collect(),
            files: Vec::new(),
        };
        let audit = host.runtime.run_library_audit(request).await.unwrap();

        let audio = &audit.items[0].standard_audio;
        assert!(audio.contains("eng") && !audio.contains("jpn"), "{audio}");
    }

    /// Sending a title from the Library to the Dashboard: those files become
    /// the working set, wherever the runtime has to get them from.
    #[tokio::test]
    async fn a_handoff_replaces_the_working_set_with_the_files_it_names() {
        let host = host(0);
        let dashboard = folder(&host, "dashboard", 2);
        let id = start(&host, &dashboard).await;
        finished(&host, id).await;
        let mut title = overview(&host, &folder(&host, "films", 3)).await.files;
        title.truncate(2);
        let mut paths: Vec<_> = title.iter().map(|file| file.path.clone()).collect();
        let handoff = mkvo_contracts::FileSelectionRequest {
            paths: paths.clone(),
            replace_working_set: true,
        };
        // After a restart nothing is in memory, and the files are read again.
        let restarted = MkvoRuntime::from_parts(
            host.runtime.inner.config.clone(),
            host.runtime.inner.dependencies.clone(),
        );

        for runtime in [&host.runtime, &restarted] {
            let adopted = runtime.set_file_selection(handoff.clone()).await.unwrap();
            assert_eq!(adopted.selected_paths, paths);
            let working = runtime.get_current_scan_files().await.unwrap();
            let files: Vec<_> = working.files.into_iter().map(|file| file.path).collect();
            assert_eq!(files, paths);
            assert_eq!(working.selected_paths, paths);
            assert_eq!(working.summary.total, 2);
        }
        // The scan that lost the working set still answers with its own files.
        let earlier = host.runtime.get_scan_job(&id.to_string()).await.unwrap();
        assert_eq!(earlier.files.len(), 2);
        assert!(
            earlier
                .files
                .iter()
                .all(|file| file.path.contains("dashboard"))
        );

        // A file that is gone is left out, not an error.
        paths.push(display_path(&dashboard.join("gone.mkv")));
        let handoff = mkvo_contracts::FileSelectionRequest {
            paths,
            replace_working_set: true,
        };
        let adopted = host.runtime.set_file_selection(handoff).await.unwrap();
        assert_eq!(adopted.selected_paths.len(), 2);
    }

    /// A job of `kind` that runs until the returned sender is dropped.
    async fn held_job(host: &Host, kind: JobKind) -> (JobId, tokio::sync::oneshot::Sender<()>) {
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let spec = JobSpec {
            kind,
            idempotency_key: IdempotencyKey::generate(),
            request_fingerprint: "held".to_owned(),
            plan_id: None,
            total: 0,
            resources: Vec::new(),
        };
        let task = |_| async move {
            let _ = released.await;
            Ok(JobCompletion::default())
        };
        let accepted = host.runtime.inner.jobs.start(spec, task).await.unwrap();
        (accepted.id, release)
    }

    #[tokio::test]
    async fn cancelling_through_the_other_kind_of_endpoint_cancels_nothing() {
        let host = host(0);
        let (scan, _hold_scan) = held_job(&host, JobKind::Scan).await;
        let (remux, _hold_remux) = held_job(&host, JobKind::Remux).await;

        let as_operation = host.runtime.cancel_operation_job(&scan.to_string()).await;
        let as_scan = host.runtime.cancel_scan(&remux.to_string()).await;

        for error in [as_operation.unwrap_err(), as_scan.unwrap_err()] {
            assert_eq!(error.code, mkvo_contracts::ApiErrorCode::NotFound);
        }
        for id in [scan, remux] {
            let job = host.runtime.inner.jobs.get(id).await.unwrap().unwrap();
            assert!(
                !matches!(job.status, JobStatus::Canceling | JobStatus::Canceled),
                "{:?} was canceled",
                job.kind
            );
        }
    }

    #[tokio::test]
    async fn files_changed_by_an_operation_are_re_read_a_few_at_a_time() {
        let probe = Arc::new(SlowProbe::default());
        let host = host_with(0, probe.clone());
        let paths: Vec<_> = (0..12)
            .map(|index| host.library.join(format!("{index:02}.mkv")))
            .collect();

        host.runtime.refresh_working_set(&paths).await;

        let most_at_once = probe.most_at_once.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            (2..=REFRESH_PROBES).contains(&most_at_once),
            "{most_at_once}"
        );
        let working_set = host.runtime.get_current_scan_files().await.unwrap();
        assert_eq!(working_set.files.len(), 12);
        assert!(
            working_set
                .files
                .is_sorted_by(|left, right| left.path <= right.path)
        );
    }

    #[test]
    fn a_long_skipped_list_is_stored_truncated() {
        let outcome = mkvo_application::ScanOutcome {
            skipped: (0..620)
                .map(|index| mkvo_application::ScanSkip {
                    path: PathBuf::from(format!("/library/{index}.mkv")),
                    reason: "unreadable".to_owned(),
                })
                .collect(),
            ..mkvo_application::ScanOutcome::default()
        };
        let state = scan_result_state(&outcome);
        assert_eq!(state.skipped.len(), 501);
        assert_eq!(state.skipped[500], "... and 120 more");
    }

    /// Older builds stored every file, twice, in the job result.
    #[tokio::test]
    async fn a_result_stored_by_an_older_build_still_loads() {
        let host = host(0);
        let file = media_at("/library/old.mkv");
        let snapshot = JobSnapshot {
            id: JobId::new(),
            kind: JobKind::Scan,
            status: JobStatus::Completed,
            correlation_id: mkvo_domain::CorrelationId::new(),
            idempotency_key: IdempotencyKey::generate(),
            request_fingerprint: "request".to_owned(),
            plan_id: None,
            created_utc: Utc::now(),
            started_utc: None,
            completed_utc: Some(Utc::now()),
            completed: 1,
            failed: 0,
            skipped: 0,
            total: 1,
            current_file: String::new(),
            current_file_percent: 0,
            lines: Vec::new(),
            result: Some(serde_json::json!({
                "files": [&file],
                "rows": [MediaFileRow::from(&file)],
                "skipped": ["/library/broken.mkv: unreadable"],
                "summary": { "total": 1, "mkv": 1, "mp4": 0, "failed": 1, "cached": 0 },
            })),
            error: None,
            revision: 3,
        };
        host.runtime
            .inner
            .dependencies
            .jobs
            .insert(&snapshot)
            .await
            .unwrap();

        let job = host
            .runtime
            .get_scan_job(&snapshot.id.to_string())
            .await
            .unwrap();
        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.summary.total, 1);
        assert_eq!(job.skipped, ["/library/broken.mkv: unreadable"]);
        assert!(job.files.is_empty());
    }
}
