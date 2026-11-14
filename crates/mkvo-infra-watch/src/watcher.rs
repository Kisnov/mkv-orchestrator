use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    ReconcileChangeKind, Snapshot, diff_snapshots, is_supported_media_path, snapshot_roots,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    Auto,
    Native,
    Polling,
}

#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub roots: Vec<PathBuf>,
    pub mode: WatchMode,
    pub debounce: Duration,
    pub poll_interval: Duration,
    pub media_only: bool,
    pub channel_capacity: usize,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            mode: WatchMode::Auto,
            debounce: Duration::from_secs(3),
            poll_interval: Duration::from_secs(30 * 60),
            media_only: true,
            channel_capacity: 4096,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchEventKind {
    Created,
    Modified,
    Removed,
    Renamed,
    RescanRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchEvent {
    pub kind: WatchEventKind,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Error)]
pub enum WatchStartError {
    #[error("at least one watch root is required")]
    NoRoots,
    #[error("watch root is not a directory: `{0}`")]
    InvalidRoot(PathBuf),
    #[error("native watcher failed: {0}")]
    Notify(#[from] notify::Error),
    #[error("initial polling snapshot failed: {0}")]
    Snapshot(#[from] crate::SnapshotError),
}

pub struct WatchHandle {
    receiver: mpsc::Receiver<WatchEvent>,
    native: Option<RecommendedWatcher>,
    bridge_task: Option<JoinHandle<()>>,
    cancellation: CancellationToken,
    active_mode: WatchMode,
}

impl std::fmt::Debug for WatchHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WatchHandle")
            .field("active_mode", &self.active_mode)
            .finish_non_exhaustive()
    }
}

impl WatchHandle {
    pub fn start(options: WatchOptions) -> Result<Self, WatchStartError> {
        validate_roots(&options.roots)?;
        match options.mode {
            WatchMode::Native => Self::start_native(options),
            WatchMode::Polling => Self::start_polling(options),
            WatchMode::Auto => match Self::start_native(options.clone()) {
                Ok(handle) => Ok(handle),
                Err(error) => {
                    tracing::warn!(%error, "native filesystem watch unavailable; using polling reconciliation");
                    Self::start_polling(options)
                }
            },
        }
    }

    pub const fn active_mode(&self) -> WatchMode {
        self.active_mode
    }

    pub async fn recv(&mut self) -> Option<WatchEvent> {
        self.receiver.recv().await
    }

    pub fn try_recv(&mut self) -> Result<WatchEvent, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn stop(&self) {
        self.cancellation.cancel();
    }

    fn start_native(options: WatchOptions) -> Result<Self, WatchStartError> {
        let (raw_sender, raw_receiver) = mpsc::channel(options.channel_capacity.max(1));
        let (sender, receiver) = mpsc::channel(options.channel_capacity.max(1));
        let callback_sender = raw_sender.clone();
        let overflowed = Arc::new(AtomicBool::new(false));
        let callback_overflowed = Arc::clone(&overflowed);
        let mut watcher = notify::recommended_watcher(move |event| {
            if matches!(
                callback_sender.try_send(event),
                Err(mpsc::error::TrySendError::Full(_))
            ) {
                callback_overflowed.store(true, Ordering::Release);
            }
        })?;
        for root in &options.roots {
            // Events are reported under the watched spelling; cache rows use the canonical one.
            let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            watcher.watch(&root, RecursiveMode::Recursive)?;
        }
        let cancellation = CancellationToken::new();
        let bridge_task = tokio::spawn(debounce_native_events(
            raw_receiver,
            sender,
            cancellation.clone(),
            options.debounce,
            options.poll_interval,
            options.media_only,
            overflowed,
        ));
        Ok(Self {
            receiver,
            native: Some(watcher),
            bridge_task: Some(bridge_task),
            cancellation,
            active_mode: WatchMode::Native,
        })
    }

    fn start_polling(mut options: WatchOptions) -> Result<Self, WatchStartError> {
        // Snapshot keys are canonical, so roots are matched against them in that spelling.
        for root in &mut options.roots {
            if let Ok(canonical) = std::fs::canonicalize(&*root) {
                *root = canonical;
            }
        }
        let initial = snapshot_roots(&options.roots, options.media_only)?;
        let (sender, receiver) = mpsc::channel(options.channel_capacity.max(1));
        let cancellation = CancellationToken::new();
        let bridge_task = tokio::spawn(poll_loop(options, initial, sender, cancellation.clone()));
        Ok(Self {
            receiver,
            native: None,
            bridge_task: Some(bridge_task),
            cancellation,
            active_mode: WatchMode::Polling,
        })
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.native.take();
        if let Some(task) = self.bridge_task.take() {
            task.abort();
        }
    }
}

fn validate_roots(roots: &[PathBuf]) -> Result<(), WatchStartError> {
    if roots.is_empty() {
        return Err(WatchStartError::NoRoots);
    }
    if let Some(root) = roots.iter().find(|root| !root.is_dir()) {
        return Err(WatchStartError::InvalidRoot(root.clone()));
    }
    Ok(())
}

/// A path waiting to be reported, with its size and mtime at the last check.
#[derive(Debug)]
struct Pending {
    kind: WatchEventKind,
    at: Instant,
    seen: Option<(u64, Option<SystemTime>)>,
}

fn settle_tick(debounce: Duration) -> Duration {
    debounce
        .min(Duration::from_secs(1))
        .max(Duration::from_millis(50))
}

/// Takes the paths quiet for `debounce` whose size and mtime also held still
/// since the previous check, so a file still being copied is not probed.
async fn take_settled(
    pending: &mut BTreeMap<PathBuf, Pending>,
    debounce: Duration,
    now: Instant,
) -> Vec<WatchEvent> {
    let due: Vec<PathBuf> = pending
        .iter()
        .filter(|(_, entry)| {
            entry.kind != WatchEventKind::Removed && now.duration_since(entry.at) >= debounce
        })
        .map(|(path, _)| path.clone())
        .collect();
    // Off the async workers: a hung network mount must not stall the runtime.
    let Ok(states) = tokio::task::spawn_blocking(move || {
        due.into_iter()
            .map(|path| {
                let state = std::fs::metadata(&path)
                    .ok()
                    .map(|metadata| (metadata.len(), metadata.modified().ok()));
                (path, state)
            })
            .collect::<BTreeMap<_, _>>()
    })
    .await
    else {
        return Vec::new();
    };
    let mut ready = Vec::new();
    pending.retain(|path, entry| {
        if now.duration_since(entry.at) < debounce {
            return true;
        }
        if let Some(state) = states.get(path) {
            if state.is_some() && *state != entry.seen {
                entry.seen = *state;
                entry.at = now;
                return true;
            }
        }
        ready.push(WatchEvent {
            kind: entry.kind,
            paths: vec![path.clone()],
        });
        false
    });
    ready
}

fn queue(
    pending: &mut BTreeMap<PathBuf, Pending>,
    path: PathBuf,
    kind: WatchEventKind,
    at: Instant,
) {
    pending
        .entry(path)
        .and_modify(|entry| {
            entry.kind = kind;
            entry.at = at;
        })
        .or_insert(Pending {
            kind,
            at,
            seen: None,
        });
}

async fn debounce_native_events(
    mut raw_receiver: mpsc::Receiver<notify::Result<Event>>,
    sender: mpsc::Sender<WatchEvent>,
    cancellation: CancellationToken,
    debounce: Duration,
    reconciliation: Duration,
    media_only: bool,
    overflowed: Arc<AtomicBool>,
) {
    let mut tick = tokio::time::interval(settle_tick(debounce));
    let mut reconcile = tokio::time::interval(reconciliation.max(Duration::from_secs(1)));
    reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    reconcile.tick().await;
    let mut pending = BTreeMap::new();
    loop {
        tokio::select! {
            () = cancellation.cancelled() => break,
            raw = raw_receiver.recv() => {
                let Some(raw) = raw else { break; };
                match raw {
                    Ok(event) => queue_notify_event(event, media_only, &mut pending),
                    Err(error) => {
                        tracing::warn!(%error, "filesystem watcher reported an error; reconciliation required");
                        if sender.send(WatchEvent { kind: WatchEventKind::RescanRequired, paths: Vec::new() }).await.is_err() {
                            break;
                        }
                    }
                }
            }
            _ = tick.tick() => {
                if overflowed.swap(false, Ordering::AcqRel)
                    && sender.send(WatchEvent { kind: WatchEventKind::RescanRequired, paths: Vec::new() }).await.is_err()
                {
                    return;
                }
                for event in take_settled(&mut pending, debounce, Instant::now()).await {
                    if sender.send(event).await.is_err() {
                        return;
                    }
                }
            }
            _ = reconcile.tick() => {
                if sender.send(WatchEvent { kind: WatchEventKind::RescanRequired, paths: Vec::new() }).await.is_err() {
                    return;
                }
            }
        }
    }
}

fn queue_notify_event(event: Event, media_only: bool, pending: &mut BTreeMap<PathBuf, Pending>) {
    let kind = match event.kind {
        EventKind::Create(_) => WatchEventKind::Created,
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => WatchEventKind::Renamed,
        EventKind::Modify(_) => WatchEventKind::Modified,
        EventKind::Remove(_) => WatchEventKind::Removed,
        EventKind::Any | EventKind::Other | EventKind::Access(_) => return,
    };
    let at = Instant::now();
    for path in event.paths {
        if media_only && path.extension().is_some() && !is_supported_media_path(&path) {
            continue;
        }
        queue(pending, path, kind, at);
    }
}

/// A root that listed files and now lists none is a share that dropped, not a
/// deleted library: it keeps its previous entries until it lists files again.
fn keep_unavailable_roots(roots: &[PathBuf], previous: &Snapshot, current: &mut Snapshot) {
    for root in roots {
        let under = |path: &&PathBuf| path.starts_with(root);
        if current.keys().any(|path| under(&path)) || !previous.keys().any(|path| under(&path)) {
            continue;
        }
        tracing::warn!(root = %root.display(), "watch root lists no media; treating it as unavailable");
        current.extend(
            previous
                .iter()
                .filter(|(path, _)| under(path))
                .map(|(path, identity)| (path.clone(), identity.clone())),
        );
    }
}

async fn poll_loop(
    options: WatchOptions,
    mut previous: Snapshot,
    sender: mpsc::Sender<WatchEvent>,
    cancellation: CancellationToken,
) {
    let mut interval = tokio::time::interval(options.poll_interval.max(Duration::from_secs(1)));
    interval.tick().await;
    let mut tick = tokio::time::interval(settle_tick(options.debounce));
    let mut pending = BTreeMap::new();
    loop {
        tokio::select! {
            () = cancellation.cancelled() => break,
            _ = interval.tick() => {
                let roots = options.roots.clone();
                let media_only = options.media_only;
                let snapshot = tokio::task::spawn_blocking(move || snapshot_roots(&roots, media_only)).await;
                match snapshot {
                    Ok(Ok(mut current)) => {
                        keep_unavailable_roots(&options.roots, &previous, &mut current);
                        let now = Instant::now();
                        for change in diff_snapshots(&previous, &current) {
                            let kind = match change.kind {
                                ReconcileChangeKind::Created => WatchEventKind::Created,
                                ReconcileChangeKind::Modified => WatchEventKind::Modified,
                                ReconcileChangeKind::Removed => WatchEventKind::Removed,
                            };
                            queue(&mut pending, change.path, kind, now);
                        }
                        previous = current;
                    }
                    Ok(Err(error)) => tracing::warn!(%error, "polling filesystem reconciliation failed"),
                    Err(error) => tracing::warn!(%error, "polling filesystem task failed"),
                }
            }
            _ = tick.tick() => {
                for event in take_settled(&mut pending, options.debounce, Instant::now()).await {
                    if sender.send(event).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileIdentity;

    #[test]
    fn rejects_empty_roots() {
        assert!(matches!(
            WatchHandle::start(WatchOptions::default()),
            Err(WatchStartError::NoRoots)
        ));
    }

    /// Cache rows are keyed by canonical paths, so events must arrive in that
    /// spelling whatever form the root was configured in.
    #[tokio::test]
    async fn native_events_report_canonical_paths() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("mkvo-watch-native-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(directory.join("sub")).expect("sub dir");
        let mut handle = WatchHandle::start(WatchOptions {
            roots: vec![directory.join("sub").join("..")],
            mode: WatchMode::Native,
            debounce: Duration::from_millis(50),
            ..WatchOptions::default()
        })
        .expect("start native watcher");
        let created = directory.join("episode.mkv");
        std::fs::write(&created, b"episode").expect("create media file");

        let expected = std::fs::canonicalize(&created).expect("canonical path");
        let event = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = handle.recv().await.expect("watch channel closed");
                if event.paths.iter().any(|path| path.ends_with("episode.mkv")) {
                    return event;
                }
            }
        })
        .await
        .expect("native event timed out");
        drop(handle);
        let _ = std::fs::remove_dir_all(&directory);
        assert_eq!(event.paths, [expected]);
    }

    /// A polled share that dropped lists nothing; that must not become one
    /// removal per file, while a root that still lists files reports its own.
    #[test]
    fn a_root_that_went_empty_keeps_its_previous_snapshot() {
        let identity = FileIdentity {
            size: 1,
            modified_at_ns: 1,
        };
        let (share, local) = (PathBuf::from("share"), PathBuf::from("local"));
        let previous: Snapshot = [
            (share.join("a.mkv"), identity.clone()),
            (share.join("b.mkv"), identity.clone()),
            (local.join("c.mkv"), identity.clone()),
            (local.join("d.mkv"), identity.clone()),
        ]
        .into();
        let mut current: Snapshot = [(local.join("c.mkv"), identity)].into();

        keep_unavailable_roots(&[share, local.clone()], &previous, &mut current);

        let changes = diff_snapshots(&previous, &current);
        assert_eq!(
            changes
                .iter()
                .map(|change| (change.path.clone(), change.kind))
                .collect::<Vec<_>>(),
            [(local.join("d.mkv"), ReconcileChangeKind::Removed)]
        );
    }

    /// A file still being copied keeps growing between checks, and is only
    /// reported once its size and mtime hold still across one.
    #[tokio::test]
    async fn a_growing_file_is_held_until_it_settles() {
        let directory = std::env::temp_dir().join(format!(
            "mkvo-watch-settle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("directory");
        let copying = directory.join("copying.mkv");
        std::fs::write(&copying, b"part").expect("first chunk");
        let mut pending = BTreeMap::new();
        queue(
            &mut pending,
            copying.clone(),
            WatchEventKind::Created,
            Instant::now(),
        );
        queue(
            &mut pending,
            directory.join("gone.mkv"),
            WatchEventKind::Removed,
            Instant::now(),
        );

        let first = take_settled(&mut pending, Duration::ZERO, Instant::now()).await;
        assert_eq!(first.len(), 1, "a removal needs no settling");
        assert_eq!(first[0].kind, WatchEventKind::Removed);

        std::fs::write(&copying, b"part and more").expect("second chunk");
        assert!(
            take_settled(&mut pending, Duration::ZERO, Instant::now())
                .await
                .is_empty()
        );

        let settled = take_settled(&mut pending, Duration::ZERO, Instant::now()).await;
        let _ = std::fs::remove_dir_all(&directory);
        assert_eq!(
            settled,
            [WatchEvent {
                kind: WatchEventKind::Created,
                paths: vec![copying],
            }]
        );
    }

    #[test]
    fn event_filter_keeps_media_and_directories() {
        let mut pending = BTreeMap::new();
        queue_notify_event(
            Event {
                kind: EventKind::Create(notify::event::CreateKind::File),
                paths: vec![PathBuf::from("cover.jpg"), PathBuf::from("episode.mkv")],
                attrs: notify::event::EventAttributes::new(),
            },
            true,
            &mut pending,
        );
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key(&PathBuf::from("episode.mkv")));
    }
}
