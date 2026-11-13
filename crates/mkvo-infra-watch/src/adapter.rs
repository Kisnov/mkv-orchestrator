use std::{
    collections::{BTreeSet, HashSet},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mkvo_application::{
    AuthorizedPathPolicy, EnumeratedMedia, FileAccessState, FileSystem, MediaCatalog,
    MediaEnumerationRequest, PortError, RequiredAccess, WatchBackend, WatchBackendKind,
    WatchChange, WatchChangeKind, WatchHealth,
};
use mkvo_domain::{FileFingerprint, WatchSettings};
use tokio::{
    sync::{Mutex, broadcast, mpsc},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use walkdir::{DirEntry, WalkDir};

use crate::{AccessMode, AuthorizedRoots, WatchHandle, WatchMode, WatchOptions};

#[derive(Debug, Clone)]
pub struct LocalMediaCatalog {
    authorized_roots: Arc<AuthorizedRoots>,
    quick_hash: bool,
}

impl LocalMediaCatalog {
    pub fn new(authorized_roots: AuthorizedRoots, quick_hash: bool) -> Self {
        Self {
            authorized_roots: Arc::new(authorized_roots),
            quick_hash,
        }
    }

    fn authorize(&self, request: &MediaEnumerationRequest) -> Result<Vec<PathBuf>, PortError> {
        request
            .roots
            .iter()
            .map(|root| {
                self.authorized_roots
                    .authorize_existing(root, AccessMode::Read)
                    .map(|path| path.into_path_buf())
                    .map_err(path_port_error)
            })
            .collect()
    }
}

#[async_trait]
impl MediaCatalog for LocalMediaCatalog {
    async fn enumerate(
        &self,
        request: &MediaEnumerationRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<FileFingerprint>, PortError> {
        let roots = self.authorize(request)?;
        let ignored = request.ignored_folder_names.clone();
        let extensions = request.supported_extensions.clone();
        let quick_hash = self.quick_hash;
        tokio::task::spawn_blocking(move || {
            enumerate_files(&roots, &ignored, &extensions, quick_hash, &cancel)
        })
        .await
        .map_err(|error| PortError::Other(format!("media enumeration task failed: {error}")))?
    }

    async fn enumerate_streaming(
        &self,
        request: &MediaEnumerationRequest,
        cancel: CancellationToken,
        entries: mpsc::UnboundedSender<EnumeratedMedia>,
    ) -> Result<(), PortError> {
        let roots = self.authorize(request)?;
        let ignored = request.ignored_folder_names.clone();
        let extensions = request.supported_extensions.clone();
        let quick_hash = self.quick_hash;
        tokio::task::spawn_blocking(move || {
            walk_media(
                &roots,
                &ignored,
                &extensions,
                quick_hash,
                || cancel.is_cancelled() || entries.is_closed(),
                |entry| entries.send(entry).is_ok(),
            )
        })
        .await
        .map_err(|error| PortError::Other(format!("media enumeration task failed: {error}")))?
    }
}

/// Fails on the first entry that cannot be read: callers that prune what is
/// missing from the result must not mistake unreadable for gone.
fn enumerate_files(
    roots: &[PathBuf],
    ignored: &BTreeSet<String>,
    extensions: &BTreeSet<String>,
    quick_hash: bool,
    cancel: &CancellationToken,
) -> Result<Vec<FileFingerprint>, PortError> {
    let mut files = Vec::new();
    let mut failure = None;
    let canceled = || cancel.is_cancelled();
    walk_media(roots, ignored, extensions, quick_hash, canceled, |entry| {
        match entry {
            EnumeratedMedia::File(file) => files.push(file),
            EnumeratedMedia::Skipped { path, reason } => {
                failure = Some(format!("could not read `{}`: {reason}", path.display()));
            }
        }
        failure.is_none()
    })?;
    if let Some(failure) = failure {
        return Err(PortError::Other(failure));
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

/// Hands every media file to `found` until it returns `false`.
fn walk_media(
    roots: &[PathBuf],
    ignored: &BTreeSet<String>,
    extensions: &BTreeSet<String>,
    quick_hash: bool,
    canceled: impl Fn() -> bool,
    mut found: impl FnMut(EnumeratedMedia) -> bool,
) -> Result<(), PortError> {
    let ignored: BTreeSet<_> = ignored
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect();
    let extensions: BTreeSet<_> = extensions
        .iter()
        .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
        .collect();
    // Roots may overlap.
    let mut seen = HashSet::new();
    for root in roots {
        let walker = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| !is_ignored_directory(entry, &ignored));
        for entry in walker {
            if canceled() {
                return Err(PortError::Canceled);
            }
            let entry = match entry {
                Ok(entry)
                    if !is_media_file(&entry, &extensions)
                        || !seen.insert(entry.path().to_path_buf()) =>
                {
                    continue;
                }
                Ok(entry) => match fingerprint_entry(&entry, quick_hash) {
                    Ok(file) => EnumeratedMedia::File(file),
                    Err(error) => EnumeratedMedia::Skipped {
                        path: entry.into_path(),
                        reason: error.to_string(),
                    },
                },
                Err(error) => EnumeratedMedia::Skipped {
                    path: error.path().unwrap_or(root).to_path_buf(),
                    reason: error
                        .io_error()
                        .map_or_else(|| error.to_string(), ToString::to_string),
                },
            };
            if !found(entry) {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn is_media_file(entry: &DirEntry, extensions: &BTreeSet<String>) -> bool {
    let extension = entry
        .path()
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    entry.file_type().is_file() && extensions.contains(&extension)
}

fn is_ignored_directory(entry: &DirEntry, ignored: &BTreeSet<String>) -> bool {
    entry.depth() > 0
        && entry.file_type().is_dir()
        && ignored.contains(&entry.file_name().to_string_lossy().to_ascii_lowercase())
}

#[derive(Debug, Clone)]
pub struct LocalFileSystem {
    authorized_roots: Arc<AuthorizedRoots>,
    quick_hash: bool,
}

impl LocalFileSystem {
    pub fn new(authorized_roots: AuthorizedRoots, quick_hash: bool) -> Self {
        Self {
            authorized_roots: Arc::new(authorized_roots),
            quick_hash,
        }
    }
}

#[async_trait]
impl AuthorizedPathPolicy for AuthorizedRoots {
    async fn authorize_read(&self, path: &Path) -> Result<PathBuf, PortError> {
        self.authorize_existing(path, AccessMode::Read)
            .map(|path| path.into_path_buf())
            .map_err(path_port_error)
    }

    async fn authorize_write(&self, path: &Path) -> Result<PathBuf, PortError> {
        self.authorize_candidate(path, AccessMode::Write)
            .map(|path| path.into_path_buf())
            .map_err(path_port_error)
    }
}

#[async_trait]
impl FileSystem for LocalFileSystem {
    async fn exists(&self, path: &Path) -> Result<bool, PortError> {
        match self
            .authorized_roots
            .authorize_candidate(path, AccessMode::Read)
        {
            Ok(_) => Ok(path.exists()),
            Err(error) => Err(path_port_error(error)),
        }
    }

    async fn is_directory(&self, path: &Path) -> Result<bool, PortError> {
        let path = self
            .authorized_roots
            .authorize_existing(path, AccessMode::Read)
            .map_err(path_port_error)?;
        Ok(path.path().is_dir())
    }

    async fn fingerprint(&self, path: &Path) -> Result<FileFingerprint, PortError> {
        let path = self
            .authorized_roots
            .authorize_existing(path, AccessMode::Read)
            .map_err(path_port_error)?
            .into_path_buf();
        let quick_hash = self.quick_hash;
        tokio::task::spawn_blocking(move || fingerprint_file(&path, quick_hash))
            .await
            .map_err(|error| PortError::Other(format!("fingerprint task failed: {error}")))?
            .map_err(|error| PortError::Other(error.to_string()))
    }

    async fn probe_access(
        &self,
        path: &Path,
        access: RequiredAccess,
    ) -> Result<FileAccessState, PortError> {
        let mode = match access {
            RequiredAccess::Read => AccessMode::Read,
            RequiredAccess::ReadWrite => AccessMode::Write,
        };
        let authorized = match self.authorized_roots.authorize_existing(path, mode) {
            Ok(path) => path.into_path_buf(),
            // A path that cannot be authorized is reported by the caller's own
            // authorization check; here it simply is not reachable.
            Err(crate::PathAuthorizationError::Canonicalize { .. }) => {
                return Ok(FileAccessState::Missing);
            }
            Err(error) => return Err(path_port_error(error)),
        };

        tokio::task::spawn_blocking(move || probe_file_access(&authorized, access))
            .await
            .map_err(|error| PortError::Other(format!("access probe task failed: {error}")))
    }

    async fn move_file(&self, source: &Path, target: &Path) -> Result<(), PortError> {
        let source = self
            .authorized_roots
            .authorize_existing(source, AccessMode::Write)
            .map_err(path_port_error)?
            .into_path_buf();
        // Resolving the target itself would return the spelling already on disk and undo a case-only rename.
        let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
            return Err(PortError::Conflict(format!(
                "target is not a file path: `{}`",
                target.display()
            )));
        };
        let target = self
            .authorized_roots
            .authorize_existing(parent, AccessMode::Write)
            .map_err(path_port_error)?
            .into_path_buf()
            .join(name);
        let moved = {
            let (source, target) = (source.clone(), target.clone());
            tokio::task::spawn_blocking(move || move_without_replacing(&source, &target))
        };
        moved
            .await
            .map_err(|error| PortError::Other(format!("move task failed: {error}")))?
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::AlreadyExists => {
                    PortError::Conflict(format!("target already exists: `{}`", target.display()))
                }
                _ => PortError::Other(format!(
                    "could not move `{}` to `{}`: {error}",
                    source.display(),
                    target.display()
                )),
            })
    }

    async fn remove_file(&self, path: &Path) -> Result<(), PortError> {
        let path = self
            .authorized_roots
            .authorize_existing(path, AccessMode::Write)
            .map_err(path_port_error)?
            .into_path_buf();
        tokio::fs::remove_file(&path).await.map_err(|error| {
            PortError::Other(format!("could not remove `{}`: {error}", path.display()))
        })
    }

    async fn replace_file(&self, source: &Path, target: &Path) -> Result<(), PortError> {
        let source = self
            .authorized_roots
            .authorize_existing(source, AccessMode::Write)
            .map_err(path_port_error)?
            .into_path_buf();
        let target = self
            .authorized_roots
            .authorize_existing(target, AccessMode::Write)
            .map_err(path_port_error)?
            .into_path_buf();
        // After the rename no other copy exists, so the new data has to be on disk before it.
        let flushed = async {
            let file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&source)
                .await?;
            file.sync_all().await
        };
        flushed.await.map_err(|error| {
            PortError::Other(format!(
                "could not flush `{}` to disk: {error}",
                source.display()
            ))
        })?;
        // A rename onto an existing file swaps it atomically on POSIX and on NTFS.
        tokio::fs::rename(&source, &target).await.map_err(|error| {
            PortError::Other(format!(
                "could not replace `{}` with `{}`: {error}",
                target.display(),
                source.display()
            ))
        })
    }

    async fn list_files(&self, directory: &Path) -> Result<Vec<PathBuf>, PortError> {
        let directory = self
            .authorized_roots
            .authorize_existing(directory, AccessMode::Read)
            .map_err(path_port_error)?
            .into_path_buf();
        let listed = {
            let directory = directory.clone();
            tokio::task::spawn_blocking(move || {
                let mut files = Vec::new();
                for entry in std::fs::read_dir(&directory)? {
                    let entry = entry?;
                    if entry.file_type()?.is_file() {
                        files.push(entry.path());
                    }
                }
                Ok::<_, std::io::Error>(files)
            })
        };
        listed
            .await
            .map_err(|error| PortError::Other(format!("directory listing task failed: {error}")))?
            .map_err(|error| {
                PortError::Other(format!("could not list `{}`: {error}", directory.display()))
            })
    }
}

/// Move `source` to `target`, failing if another file is already there.
fn move_without_replacing(source: &Path, target: &Path) -> std::io::Result<()> {
    move_linking_with(
        |source, target| std::fs::hard_link(source, target),
        source,
        target,
    )
}

/// `link` is `hard_link`, the one call in std that fails on an existing target
/// instead of replacing it.
fn move_linking_with(
    link: impl Fn(&Path, &Path) -> std::io::Result<()>,
    source: &Path,
    target: &Path,
) -> std::io::Result<()> {
    // Held so that a source nobody lets us delete fails here, before it has two names.
    #[cfg(windows)]
    let _deletable = {
        use std::os::windows::fs::OpenOptionsExt;
        const DELETE: u32 = 0x0001_0000;
        std::fs::OpenOptions::new()
            .access_mode(DELETE)
            .open(source)?
    };
    match link(source, target) {
        Ok(()) => std::fs::remove_file(source).inspect_err(|_| {
            let _ = std::fs::remove_file(target);
        }),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            respell(source, target, error)
        }
        Err(_) => rename_if_absent(source, target),
    }
}

/// Check, then rename: only for filesystems without hard links, because a file
/// created between the two calls would be replaced.
fn rename_if_absent(source: &Path, target: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(target) {
        Ok(_) => respell(source, target, std::io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::rename(source, target)
        }
        Err(error) => Err(error),
    }
}

/// `target` exists. Where case is ignored it may be `source` itself under the
/// new spelling, and then one rename respells it; any other file is `taken`.
fn respell(source: &Path, target: &Path, taken: std::io::Error) -> std::io::Result<()> {
    if same_file::is_same_file(source, target)? {
        std::fs::rename(source, target)
    } else {
        Err(taken)
    }
}

/// The walk starts at a canonical root and follows no links, so the entry's
/// path is already canonical and its metadata came with the directory read.
fn fingerprint_entry(
    entry: &DirEntry,
    include_quick_hash: bool,
) -> Result<FileFingerprint, std::io::Error> {
    fingerprint(entry.path(), &entry.metadata()?, include_quick_hash)
}

fn fingerprint_file(
    canonical: &Path,
    include_quick_hash: bool,
) -> Result<FileFingerprint, std::io::Error> {
    fingerprint(
        canonical,
        &std::fs::metadata(canonical)?,
        include_quick_hash,
    )
}

fn fingerprint(
    canonical: &Path,
    metadata: &std::fs::Metadata,
    include_quick_hash: bool,
) -> Result<FileFingerprint, std::io::Error> {
    let modified_at: DateTime<Utc> = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH).into();
    let quick_hash = include_quick_hash
        .then(|| quick_hash_file(canonical, metadata.len()))
        .transpose()?;
    Ok(FileFingerprint {
        path: canonical.to_path_buf(),
        size_bytes: metadata.len(),
        modified_at,
        quick_hash,
    })
}

fn quick_hash_file(path: &Path, length: u64) -> Result<String, std::io::Error> {
    const CHUNK: usize = 64 * 1024;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&length.to_le_bytes());
    let mut buffer = vec![0; CHUNK];
    let first = file.read(&mut buffer)?;
    hasher.update(&buffer[..first]);
    if length > CHUNK as u64 {
        file.seek(SeekFrom::End(-(CHUNK as i64)))?;
        let last = file.read(&mut buffer)?;
        hasher.update(&buffer[..last]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn path_port_error(error: crate::PathAuthorizationError) -> PortError {
    match error {
        crate::PathAuthorizationError::OutsideRoots(path)
        | crate::PathAuthorizationError::ReadOnly(path)
        | crate::PathAuthorizationError::Relative(path)
        | crate::PathAuthorizationError::Traversal(path) => {
            PortError::Conflict(format!("unauthorized path `{}`", path.display()))
        }
        error => PortError::Other(error.to_string()),
    }
}

#[derive(Debug)]
struct WatchState {
    task: Option<JoinHandle<()>>,
    cancellation: Option<CancellationToken>,
    running: bool,
    backend: WatchBackendKind,
    watched_roots: usize,
    last_event_utc: Option<DateTime<Utc>>,
    error: Option<String>,
}

impl Default for WatchState {
    fn default() -> Self {
        Self {
            task: None,
            cancellation: None,
            running: false,
            backend: WatchBackendKind::Native,
            watched_roots: 0,
            last_event_utc: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WatchService {
    state: Arc<Mutex<WatchState>>,
    events: broadcast::Sender<WatchChange>,
    options: WatchOptions,
}

impl WatchService {
    pub fn new(options: WatchOptions) -> Self {
        let (events, _) = broadcast::channel(options.channel_capacity.max(16));
        Self {
            state: Arc::new(Mutex::new(WatchState::default())),
            events,
            options,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WatchChange> {
        self.events.subscribe()
    }

    async fn stop_inner(&self) {
        let mut state = self.state.lock().await;
        if let Some(cancellation) = state.cancellation.take() {
            cancellation.cancel();
        }
        if let Some(task) = state.task.take() {
            task.abort();
        }
        state.running = false;
        state.watched_roots = 0;
    }
}

#[async_trait]
impl WatchBackend for WatchService {
    fn subscribe(&self) -> broadcast::Receiver<WatchChange> {
        WatchService::subscribe(self)
    }

    async fn start(&self, settings: &WatchSettings) -> Result<(), PortError> {
        self.stop_inner().await;
        let mut options = self.options.clone();
        options.roots = settings.roots.clone();
        options.debounce = std::time::Duration::from_millis(settings.debounce_millis.max(50));
        if settings.reconciliation_interval_minutes > 0 {
            options.poll_interval = std::time::Duration::from_secs(
                settings.reconciliation_interval_minutes.saturating_mul(60),
            );
        }
        options.mode = if settings.force_polling {
            WatchMode::Polling
        } else {
            WatchMode::Auto
        };
        let mut handle = WatchHandle::start(options).map_err(|error| {
            PortError::unavailable(format!("watcher could not start: {error}"), true)
        })?;
        let backend = if handle.active_mode() == WatchMode::Polling {
            WatchBackendKind::Polling
        } else {
            WatchBackendKind::Native
        };
        let cancellation = CancellationToken::new();
        let task_cancel = cancellation.clone();
        let state = Arc::clone(&self.state);
        let events = self.events.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = task_cancel.cancelled() => {
                        handle.stop();
                        break;
                    }
                    event = handle.recv() => {
                        let Some(event) = event else { break; };
                        let _ = events.send(WatchChange {
                            kind: match event.kind {
                                crate::WatchEventKind::Created => WatchChangeKind::Created,
                                crate::WatchEventKind::Modified => WatchChangeKind::Modified,
                                crate::WatchEventKind::Removed => WatchChangeKind::Removed,
                                crate::WatchEventKind::Renamed => WatchChangeKind::Renamed,
                                crate::WatchEventKind::RescanRequired => WatchChangeKind::RescanRequired,
                            },
                            paths: event.paths,
                        });
                        state.lock().await.last_event_utc = Some(Utc::now());
                    }
                }
            }
            state.lock().await.running = false;
        });
        let mut state = self.state.lock().await;
        state.task = Some(task);
        state.cancellation = Some(cancellation);
        state.running = true;
        state.backend = backend;
        state.watched_roots = settings.roots.len();
        state.error = None;
        Ok(())
    }

    async fn stop(&self) -> Result<(), PortError> {
        self.stop_inner().await;
        Ok(())
    }

    async fn health(&self) -> Result<WatchHealth, PortError> {
        let state = self.state.lock().await;
        Ok(WatchHealth {
            running: state.running,
            backend: state.backend,
            watched_roots: state.watched_roots,
            last_event_utc: state.last_event_utc,
            error: state.error.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn quick_hash_changes_when_edges_change() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("mkvo-hash-{}-{stamp}", std::process::id()));
        std::fs::write(&path, vec![1_u8; 128 * 1024]).expect("file");
        let first = quick_hash_file(&path, 128 * 1024).expect("hash");
        let mut bytes = vec![1_u8; 128 * 1024];
        bytes[0] = 2;
        std::fs::write(&path, bytes).expect("rewrite");
        let second = quick_hash_file(&path, 128 * 1024).expect("hash");
        let _ = std::fs::remove_file(path);
        assert_ne!(first, second);
    }

    fn temp_tree(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mkvo-enumerate-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("temp tree");
        path
    }

    fn walk(roots: &[PathBuf]) -> Vec<EnumeratedMedia> {
        let mut entries = Vec::new();
        walk_media(
            roots,
            &BTreeSet::new(),
            &BTreeSet::from(["mkv".to_owned()]),
            false,
            || false,
            |entry| {
                entries.push(entry);
                true
            },
        )
        .expect("walk");
        entries
    }

    fn is_file(entry: &EnumeratedMedia, path: &Path) -> bool {
        matches!(entry, EnumeratedMedia::File(file) if file.path == path)
    }

    fn is_skipped(entry: &EnumeratedMedia, path: &Path) -> bool {
        matches!(entry, EnumeratedMedia::Skipped { path: skipped, .. } if skipped == path)
    }

    #[cfg(unix)]
    fn link_dir(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).expect("directory symlink");
    }

    /// A junction, because a Windows symlink needs a privilege tests lack.
    #[cfg(windows)]
    fn link_dir(target: &Path, link: &Path) {
        let created = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .args([link, target])
            .output()
            .expect("mklink");
        assert!(created.status.success(), "{created:?}");
    }

    /// The scan root is authorized once; nothing below it is checked again, so
    /// the walk itself must never leave it.
    #[test]
    fn enumeration_does_not_follow_links_out_of_the_root() {
        let tree = temp_tree("links");
        let root = tree.join("root");
        let outside = tree.join("outside");
        std::fs::create_dir_all(root.join("season")).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(root.join("season").join("a.mkv"), b"inside").expect("inside");
        std::fs::write(outside.join("secret.mkv"), b"outside").expect("outside file");
        link_dir(&outside, &root.join("escape"));
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.join("secret.mkv"), root.join("link.mkv"))
            .expect("file symlink");

        let root = std::fs::canonicalize(root).expect("canonical root");
        let found = walk(std::slice::from_ref(&root));
        let _ = std::fs::remove_dir_all(&tree);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(is_file(&found[0], &root.join("season").join("a.mkv")));
    }

    #[test]
    fn what_cannot_be_read_is_skipped_and_the_walk_goes_on() {
        let tree = temp_tree("skip");
        std::fs::write(tree.join("a.mkv"), b"media").expect("file");
        let root = std::fs::canonicalize(&tree).expect("canonical root");
        let roots = [root.join("missing"), root.clone()];

        let found = walk(&roots);
        let listed = enumerate_files(
            &roots,
            &BTreeSet::new(),
            &BTreeSet::from(["mkv".to_owned()]),
            false,
            &CancellationToken::new(),
        );
        let _ = std::fs::remove_dir_all(&tree);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(is_skipped(&found[0], &roots[0]));
        assert!(is_file(&found[1], &root.join("a.mkv")));
        assert!(listed.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_without_read_permission_is_skipped() {
        use std::os::unix::fs::PermissionsExt;

        if !access_probe_tests::permissions_are_enforced_for_this_user() {
            return;
        }
        let tree = temp_tree("locked");
        for folder in ["open", "locked"] {
            std::fs::create_dir(tree.join(folder)).expect("folder");
            std::fs::write(tree.join(folder).join("a.mkv"), b"media").expect("file");
        }
        let root = std::fs::canonicalize(&tree).expect("canonical root");
        let locked = root.join("locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock");

        let found = walk(std::slice::from_ref(&root));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock");
        let _ = std::fs::remove_dir_all(&tree);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().any(|entry| is_skipped(entry, &locked)));
        assert!(
            found
                .iter()
                .any(|entry| is_file(entry, &root.join("open").join("a.mkv")))
        );
    }

    /// Nothing here is ever sent, so only the closed channel can end the walk.
    #[tokio::test]
    async fn the_walk_stops_once_nobody_receives_its_entries() {
        let tree = temp_tree("abandoned");
        std::fs::write(tree.join("notes.txt"), b"text").expect("file");
        let roots = AuthorizedRoots::new([crate::AuthorizedRoot::new(&tree, false).expect("root")])
            .expect("roots");
        let request = MediaEnumerationRequest {
            roots: vec![tree.clone()],
            ignored_folder_names: BTreeSet::new(),
            supported_extensions: BTreeSet::from(["mkv".to_owned()]),
        };
        let (entries, receiver) = mpsc::unbounded_channel();
        drop(receiver);

        let walked = LocalMediaCatalog::new(roots, false)
            .enumerate_streaming(&request, CancellationToken::new(), entries)
            .await;
        let _ = std::fs::remove_dir_all(&tree);
        assert!(matches!(walked, Err(PortError::Canceled)), "{walked:?}");
    }

    #[test]
    fn overlapping_roots_report_a_file_once() {
        let tree = temp_tree("overlap");
        std::fs::create_dir(tree.join("season")).expect("folder");
        std::fs::write(tree.join("season").join("a.mkv"), b"media").expect("file");
        let root = std::fs::canonicalize(&tree).expect("canonical root");

        let found = walk(&[root.clone(), root.join("season")]);
        let _ = std::fs::remove_dir_all(&tree);
        assert_eq!(found.len(), 1, "{found:?}");
    }

    /// Cache rows are keyed by these values, so they must stay what
    /// `canonicalize` and `metadata` report for the file.
    #[test]
    fn enumerated_fingerprints_match_the_canonical_path_and_metadata() {
        let tree = temp_tree("fingerprint");
        let file = tree.join("Show").join("a.mkv");
        std::fs::create_dir_all(tree.join("Show")).expect("folder");
        std::fs::write(&file, b"media").expect("file");
        let canonical = std::fs::canonicalize(&file).expect("canonical file");
        let metadata = std::fs::metadata(&file).expect("metadata");

        let found = walk(&[std::fs::canonicalize(&tree).expect("canonical root")]);
        let _ = std::fs::remove_dir_all(&tree);
        assert_eq!(
            found,
            [EnumeratedMedia::File(FileFingerprint {
                path: canonical,
                size_bytes: metadata.len(),
                modified_at: metadata.modified().expect("modified").into(),
                quick_hash: None,
            })]
        );
    }

    /// A stored interval of 0 keeps the service default, which must not turn
    /// into a full library walk every few seconds.
    #[test]
    fn zero_reconciliation_interval_falls_back_to_the_settings_default() {
        let minutes = WatchSettings::default().reconciliation_interval_minutes;
        assert_eq!(
            WatchOptions::default().poll_interval,
            std::time::Duration::from_secs(minutes * 60)
        );
    }

    #[tokio::test]
    async fn polling_reconciliation_is_delivered_through_watch_backend_subscription() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("mkvo-watch-events-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&root).expect("watch root");
        let service = WatchService::new(WatchOptions {
            mode: WatchMode::Polling,
            poll_interval: std::time::Duration::from_millis(10),
            ..WatchOptions::default()
        });
        let mut changes = WatchBackend::subscribe(&service);
        service
            .start(&WatchSettings {
                enabled: true,
                roots: vec![root.clone()],
                reconciliation_interval_minutes: 0,
                force_polling: true,
                ..WatchSettings::default()
            })
            .await
            .expect("start polling watcher");
        let created = root.join("episode.mkv");
        std::fs::write(&created, b"episode").expect("create media file");

        let change = tokio::time::timeout(std::time::Duration::from_secs(3), changes.recv())
            .await
            .expect("polling reconciliation timed out")
            .expect("watch change channel closed");
        assert_eq!(change.kind, WatchChangeKind::Created);
        assert_eq!(
            change.paths,
            [std::fs::canonicalize(created).expect("canonical created path")]
        );

        service.stop().await.expect("stop watcher");
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Open the file the way a mutation would, and report why that fails.
///
/// The open is exclusive on Windows (`share_mode(0)`), matching what the .NET
/// implementation did: a media file that a media server or player currently has
/// open is exactly the case worth catching, and a shared open would not detect
/// it. Unix has no mandatory locking, so there the probe reports permissions
/// only. The handle is closed immediately.
fn probe_file_access(path: &Path, access: RequiredAccess) -> FileAccessState {
    if !path.exists() {
        return FileAccessState::Missing;
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    if access == RequiredAccess::ReadWrite {
        options.write(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }

    match options.open(path) {
        Ok(_) => FileAccessState::Available,
        Err(error) => match error.kind() {
            std::io::ErrorKind::PermissionDenied => FileAccessState::ReadOnly,
            std::io::ErrorKind::NotFound => FileAccessState::Missing,
            // Windows reports a sharing violation as os error 32; other kinds
            // that reach here mean the file cannot be opened right now, which
            // is the same practical outcome as being locked.
            _ => FileAccessState::Busy,
        },
    }
}

#[cfg(test)]
mod access_probe_tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mkvo-access-{}-{name}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn an_ordinary_file_is_available_for_writing() {
        let directory = temp_dir("plain");
        let file = directory.join("a.mkv");
        std::fs::write(&file, b"data").expect("write");

        assert_eq!(
            probe_file_access(&file, RequiredAccess::ReadWrite),
            FileAccessState::Available
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_missing_file_is_reported_as_missing_not_busy() {
        let directory = temp_dir("missing");
        assert_eq!(
            probe_file_access(&directory.join("nope.mkv"), RequiredAccess::ReadWrite),
            FileAccessState::Missing
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Root bypasses Unix permission checks, so a read-only file really is
    /// writable for it and reporting anything else would be wrong. The MKVO
    /// container runs as root unless PUID is set, and contributors often run
    /// tests in a root container, so this is a normal environment rather than
    /// an exotic one.
    #[cfg(unix)]
    pub(super) fn permissions_are_enforced_for_this_user() -> bool {
        // `id -u` rather than /proc, which macOS does not have. If the uid
        // cannot be determined, assume permissions are enforced: that is the
        // stricter assertion, so an unknown environment fails loudly instead of
        // quietly skipping the check this test exists for.
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .is_none_or(|uid| uid.trim() != "0")
    }

    #[cfg(not(unix))]
    fn permissions_are_enforced_for_this_user() -> bool {
        true
    }

    /// A read-only file passes every other precondition, so without this the
    /// failure only appears once an external tool is already running.
    #[test]
    fn a_read_only_file_blocks_write_access_but_allows_read() {
        let directory = temp_dir("readonly");
        let file = directory.join("a.mkv");
        std::fs::write(&file, b"data").expect("write");
        let mut permissions = std::fs::metadata(&file).expect("metadata").permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&file, permissions).expect("set readonly");

        if permissions_are_enforced_for_this_user() {
            assert_eq!(
                probe_file_access(&file, RequiredAccess::ReadWrite),
                FileAccessState::ReadOnly
            );
        } else {
            // Reporting the file as writable is the correct answer here: this
            // user can in fact rewrite it.
            assert_eq!(
                probe_file_access(&file, RequiredAccess::ReadWrite),
                FileAccessState::Available,
                "root can write a read-only file, so the probe must say so"
            );
        }
        assert_eq!(
            probe_file_access(&file, RequiredAccess::Read),
            FileAccessState::Available,
            "a read-only file is still readable"
        );

        let mut permissions = std::fs::metadata(&file).expect("metadata").permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(&file, permissions);
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// The case this exists for: a media server or player holding the file open
    /// while MKVO plans a mutation against it.
    #[cfg(windows)]
    #[test]
    fn a_file_held_open_by_another_handle_is_busy() {
        let directory = temp_dir("busy");
        let file = directory.join("a.mkv");
        std::fs::write(&file, b"data").expect("write");

        let holder = std::fs::File::open(&file).expect("hold open");
        assert_eq!(
            probe_file_access(&file, RequiredAccess::ReadWrite),
            FileAccessState::Busy
        );

        drop(holder);
        assert_eq!(
            probe_file_access(&file, RequiredAccess::ReadWrite),
            FileAccessState::Available,
            "the file is usable again once the other handle closes"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}

#[cfg(test)]
mod move_tests {
    use super::*;

    fn library(name: &str) -> (PathBuf, LocalFileSystem) {
        let path = std::env::temp_dir().join(format!(
            "mkvo-move-{}-{name}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        let path = std::fs::canonicalize(path).expect("canonical dir");
        let roots = AuthorizedRoots::new([crate::AuthorizedRoot::new(&path, true).expect("root")])
            .expect("roots");
        (path, LocalFileSystem::new(roots, false))
    }

    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(directory)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn a_move_never_replaces_a_file_that_is_already_there() {
        let (directory, file_system) = library("existing");
        std::fs::write(directory.join("a.mkv"), b"moved").expect("source");
        std::fs::write(directory.join("b.mkv"), b"kept").expect("target");

        let moved = file_system
            .move_file(&directory.join("a.mkv"), &directory.join("b.mkv"))
            .await;

        assert!(matches!(moved, Err(PortError::Conflict(_))), "{moved:?}");
        assert_eq!(
            std::fs::read(directory.join("a.mkv")).expect("source"),
            b"moved"
        );
        assert_eq!(
            std::fs::read(directory.join("b.mkv")).expect("target"),
            b"kept"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// On Windows, macOS and case-insensitive shares the new spelling already
    /// "exists": it is the file being renamed.
    #[tokio::test]
    async fn a_rename_that_only_changes_case_reaches_the_disk() {
        let (directory, file_system) = library("case");
        std::fs::write(directory.join("ep.mkv"), b"episode").expect("source");

        file_system
            .move_file(&directory.join("ep.mkv"), &directory.join("Ep.mkv"))
            .await
            .expect("case-only rename");

        assert_eq!(names(&directory), ["Ep.mkv"]);
        assert_eq!(
            std::fs::read(directory.join("Ep.mkv")).expect("moved"),
            b"episode"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// The rename used to go through `<target>.mkvo-rename.tmp`, where a file
    /// could be stranded, and which this one found taken.
    #[tokio::test]
    async fn a_case_only_rename_goes_through_no_other_name() {
        let (directory, file_system) = library("case-direct");
        std::fs::write(directory.join("ep.mkv"), b"episode").expect("source");
        std::fs::write(directory.join(".Ep.mkv.mkvo-rename.tmp"), b"other").expect("other");
        std::fs::write(directory.join("Ep.mkv.mkvo-rename.tmp"), b"other").expect("other");

        file_system
            .move_file(&directory.join("ep.mkv"), &directory.join("Ep.mkv"))
            .await
            .expect("case-only rename");

        assert_eq!(
            names(&directory),
            [
                ".Ep.mkv.mkvo-rename.tmp",
                "Ep.mkv",
                "Ep.mkv.mkvo-rename.tmp"
            ]
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Where case matters the other spelling can be another file, and it stays.
    #[tokio::test]
    async fn a_case_only_rename_does_not_replace_a_different_file() {
        let (directory, file_system) = library("case-taken");
        std::fs::write(directory.join("ep.mkv"), b"moved").expect("source");
        if directory.join("EP.MKV").exists() {
            // This filesystem ignores case, so the two cannot coexist.
            let _ = std::fs::remove_dir_all(&directory);
            return;
        }
        std::fs::write(directory.join("Ep.mkv"), b"kept").expect("target");

        let moved = file_system
            .move_file(&directory.join("ep.mkv"), &directory.join("Ep.mkv"))
            .await;

        assert!(matches!(moved, Err(PortError::Conflict(_))), "{moved:?}");
        assert_eq!(names(&directory), ["Ep.mkv", "ep.mkv"]);
        assert_eq!(
            std::fs::read(directory.join("ep.mkv")).expect("source"),
            b"moved"
        );
        assert_eq!(
            std::fs::read(directory.join("Ep.mkv")).expect("target"),
            b"kept"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// What happens where hard links do not exist (exFAT, some shares): the
    /// move still works, a case-only one too, and still replaces nothing.
    #[test]
    fn a_filesystem_without_hard_links_falls_back_to_a_checked_rename() {
        let (directory, _) = library("fallback");
        std::fs::write(directory.join("a.mkv"), b"moved").expect("source");
        std::fs::write(directory.join("b.mkv"), b"kept").expect("target");
        let unsupported = |_: &Path, _: &Path| Err(std::io::ErrorKind::Unsupported.into());
        let unlinked = |from: &str, to: &str| {
            move_linking_with(unsupported, &directory.join(from), &directory.join(to))
        };

        assert_eq!(
            unlinked("a.mkv", "b.mkv")
                .expect_err("existing target")
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        unlinked("a.mkv", "c.mkv").expect("free target");
        unlinked("c.mkv", "C.mkv").expect("case-only");

        assert_eq!(names(&directory), ["C.mkv", "b.mkv"]);
        assert_eq!(
            std::fs::read(directory.join("C.mkv")).expect("moved"),
            b"moved"
        );
        assert_eq!(
            std::fs::read(directory.join("b.mkv")).expect("target"),
            b"kept"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A player holding the file would let the new name appear and then refuse
    /// to give up the old one, leaving both.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_source_that_cannot_be_removed_is_not_given_a_second_name() {
        use std::os::windows::fs::OpenOptionsExt;

        let (directory, file_system) = library("held");
        std::fs::write(directory.join("a.mkv"), b"episode").expect("source");
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(directory.join("a.mkv"))
            .expect("hold open");

        let moved = file_system
            .move_file(&directory.join("a.mkv"), &directory.join("b.mkv"))
            .await;

        drop(holder);
        assert!(moved.is_err());
        assert_eq!(names(&directory), ["a.mkv"]);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn a_read_only_file_can_still_be_renamed() {
        let (directory, file_system) = library("readonly");
        let source = directory.join("a.mkv");
        std::fs::write(&source, b"episode").expect("source");
        let mut permissions = std::fs::metadata(&source).expect("metadata").permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&source, permissions).expect("set readonly");

        file_system
            .move_file(&source, &directory.join("b.mkv"))
            .await
            .expect("move");

        assert_eq!(names(&directory), ["b.mkv"]);
        let moved = directory.join("b.mkv");
        let mut permissions = std::fs::metadata(&moved).expect("metadata").permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(&moved, permissions);
        let _ = std::fs::remove_dir_all(&directory);
    }
}

#[cfg(test)]
mod list_tests {
    use super::*;

    #[tokio::test]
    async fn listing_names_the_files_of_one_folder_only() {
        let directory = std::env::temp_dir().join(format!(
            "mkvo-list-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(directory.join("Season 2")).expect("folders");
        let directory = std::fs::canonicalize(directory).expect("canonical dir");
        for name in ["a.mkv", "a.srt", "Season 2/b.mkv"] {
            std::fs::write(directory.join(name), b"file").expect("file");
        }
        let roots =
            AuthorizedRoots::new([crate::AuthorizedRoot::new(&directory, false).expect("root")])
                .expect("roots");

        let mut listed = LocalFileSystem::new(roots, false)
            .list_files(&directory)
            .await
            .expect("list");
        listed.sort();

        let _ = std::fs::remove_dir_all(&directory);
        assert_eq!(listed, [directory.join("a.mkv"), directory.join("a.srt")]);
    }
}
