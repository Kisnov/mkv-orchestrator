use std::{
    ffi::OsString,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::mpsc,
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

const DEFAULT_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const READ_BUFFER_BYTES: usize = 8 * 1024;
const MAX_LINE_BYTES: usize = 64 * 1024;
// A killed tool stuck in I/O on a dead network share may never exit.
const KILL_GRACE: Duration = Duration::from_secs(10);

// Everything else, such as the server password and provider API keys, stays out of child tools.
#[cfg(windows)]
const INHERITED_ENVIRONMENT: &[&str] = &[
    "SystemRoot",
    "SystemDrive",
    "windir",
    "PATH",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
];
#[cfg(not(windows))]
const INHERITED_ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TZ",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEvent {
    pub stream: OutputStream,
    pub line: String,
}

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub current_dir: Option<PathBuf>,
    pub timeout: Option<Duration>,
    pub inactivity_timeout: Option<Duration>,
    pub watched_outputs: Vec<PathBuf>,
    pub output_limit: usize,
}

impl ProcessSpec {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            args: Vec::new(),
            current_dir: None,
            timeout: None,
            inactivity_timeout: None,
            watched_outputs: Vec::new(),
            output_limit: DEFAULT_OUTPUT_LIMIT,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(path.into());
        self
    }

    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Stop the tool once it has printed nothing and grown none of `outputs` for `timeout`.
    pub fn inactivity_timeout(
        mut self,
        timeout: Duration,
        outputs: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        self.inactivity_timeout = Some(timeout);
        self.watched_outputs = outputs.into_iter().collect();
        self
    }

    pub const fn output_limit(mut self, bytes: usize) -> Self {
        self.output_limit = bytes;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl ProcessOutput {
    pub const fn success(&self) -> bool {
        matches!(self.exit_code, Some(0))
    }
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("the executable path is empty")]
    EmptyExecutable,
    #[error("could not start external tool `{executable}`: {source}")]
    Start {
        executable: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("external tool I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("external tool task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("external tool execution was cancelled")]
    Cancelled,
    #[error("external tool exceeded its {0:?} timeout")]
    TimedOut(Duration),
    #[error("external tool printed and wrote nothing for {0:?}")]
    Inactive(Duration),
}

#[derive(Debug, Clone, Default)]
pub struct ProcessRunner;

impl ProcessRunner {
    pub async fn run(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        self.run_streaming(spec, cancellation, None).await
    }

    pub async fn run_streaming(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
        events: Option<mpsc::Sender<ProcessEvent>>,
    ) -> Result<ProcessOutput, ProcessError> {
        if spec.executable.as_os_str().is_empty() {
            return Err(ProcessError::EmptyExecutable);
        }

        let mut command = Command::new(&spec.executable);
        command
            .args(&spec.args)
            .env_clear()
            .envs(
                INHERITED_ENVIRONMENT
                    .iter()
                    .filter_map(|name| Some((name, std::env::var_os(name)?))),
            )
            // Media paths and mkvmerge's JSON diagnostics are UTF-8. The
            // Debian slim image may otherwise inherit a C locale, which can
            // truncate non-ASCII filenames such as "Bon Appétit".
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // A release desktop host uses the Windows GUI subsystem, but console
        // executables such as ffmpeg and mkvmerge otherwise create a transient
        // console window every time MKVO probes or runs them. Keep child tools
        // fully headless; their output is already captured through pipes.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.as_std_mut().creation_flags(CREATE_NO_WINDOW);
        }
        if let Some(current_dir) = &spec.current_dir {
            command.current_dir(current_dir);
        }

        let mut child = command.spawn().map_err(|source| ProcessError::Start {
            executable: spec.executable.clone(),
            source,
        })?;
        let stdout = child.stdout.take().expect("stdout is configured as piped");
        let stderr = child.stderr.take().expect("stderr is configured as piped");

        let printed = Arc::new(AtomicU64::new(0));
        let stdout_task = capture_stream(
            stdout,
            OutputStream::Stdout,
            spec.output_limit,
            events.clone(),
            Arc::clone(&printed),
        );
        let stderr_task = capture_stream(
            stderr,
            OutputStream::Stderr,
            spec.output_limit,
            events,
            Arc::clone(&printed),
        );

        let mut wait = Box::pin(child.wait());
        let stopped = tokio::select! {
            result = &mut wait => Ok(result?),
            () = cancellation.cancelled() => Err(ProcessError::Cancelled),
            Some(timeout) = expired(spec.timeout) => Err(ProcessError::TimedOut(timeout)),
            Some(timeout) = inactive(spec.inactivity_timeout, &printed, &spec.watched_outputs) => {
                Err(ProcessError::Inactive(timeout))
            }
        };
        drop(wait);

        let exit_status = match stopped {
            Ok(status) => status,
            Err(error) => {
                terminate(&mut child).await;
                // Whatever inherited the pipes may outlive the tool; do not wait for it.
                stdout_task.abort();
                stderr_task.abort();
                return Err(error);
            }
        };

        let stdout = stdout_task.await??;
        let stderr = stderr_task.await??;
        Ok(ProcessOutput {
            exit_code: exit_status.code(),
            stdout: String::from_utf8_lossy(&stdout.bytes).into_owned(),
            stderr: String::from_utf8_lossy(&stderr.bytes).into_owned(),
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
        })
    }
}

async fn terminate(child: &mut tokio::process::Child) {
    if let Err(error) = child.start_kill() {
        tracing::debug!(%error, "external tool was already stopped or could not be killed");
    }
    if tokio::time::timeout(KILL_GRACE, child.wait())
        .await
        .is_err()
    {
        tracing::warn!("external tool did not exit after being killed");
    }
}

async fn expired(timeout: Option<Duration>) -> Option<Duration> {
    tokio::time::sleep(timeout?).await;
    timeout
}

/// Resolves once neither the tool's output nor its watched files changed for `timeout`.
async fn inactive(
    timeout: Option<Duration>,
    printed: &AtomicU64,
    outputs: &[PathBuf],
) -> Option<Duration> {
    quiet_for(timeout?, printed, outputs, |output| {
        std::fs::metadata(output).map(|metadata| metadata.len())
    })
    .await;
    timeout
}

async fn quiet_for<F>(timeout: Duration, printed: &AtomicU64, outputs: &[PathBuf], probe: F)
where
    F: Fn(PathBuf) -> std::io::Result<u64> + Clone + Send + 'static,
{
    // Few checks per timeout: each one may itself hang on the share being watched.
    let interval = timeout / 6;
    // Per output: its last known size and the probe still running for it, if any.
    let mut watched = outputs.iter().map(|_| (0, None)).collect::<Vec<_>>();
    let mut deadline = Instant::now() + timeout;
    let mut seen = progress(
        printed,
        outputs,
        &mut watched,
        Instant::now() + interval,
        &probe,
    )
    .await;
    while Instant::now() < deadline {
        tokio::time::sleep_until(deadline.min(Instant::now() + interval)).await;
        // A sample that hangs must not carry the wait past the deadline.
        let until = deadline.min(Instant::now() + interval);
        let now = progress(printed, outputs, &mut watched, until, &probe).await;
        if now != seen {
            deadline = Instant::now() + timeout;
            seen = now;
        }
    }
}

type Watched = (u64, Option<JoinHandle<std::io::Result<u64>>>);

async fn progress<F>(
    printed: &AtomicU64,
    outputs: &[PathBuf],
    watched: &mut [Watched],
    until: Instant,
    probe: &F,
) -> (u64, u64)
where
    F: Fn(PathBuf) -> std::io::Result<u64> + Clone + Send + 'static,
{
    let mut written = 0;
    for (output, (size, pending)) in outputs.iter().zip(watched) {
        // A probe that cannot be cancelled is waited on again, never started twice.
        let running = pending.get_or_insert_with(|| {
            let (probe, output) = (probe.clone(), output.clone());
            tokio::task::spawn_blocking(move || probe(output))
        });
        if let Ok(probed) = tokio::time::timeout_at(until, running).await {
            *pending = None;
            *size = probed.ok().and_then(Result::ok).unwrap_or(0);
        }
        written += *size;
    }
    (printed.load(Ordering::Relaxed), written)
}

struct CapturedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

fn capture_stream<R>(
    stream: R,
    kind: OutputStream,
    limit: usize,
    events: Option<mpsc::Sender<ProcessEvent>>,
    printed: Arc<AtomicU64>,
) -> JoinHandle<Result<CapturedBytes, std::io::Error>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut stream = stream;
        let mut buffer = vec![0; READ_BUFFER_BYTES];
        let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
        let mut line = Vec::new();
        let mut truncated = false;

        loop {
            let count = stream.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            let chunk = &buffer[..count];
            printed.fetch_add(count as u64, Ordering::Relaxed);

            let remaining = limit.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..count.min(remaining)]);
            truncated |= count > remaining;

            if let Some(sender) = &events {
                for piece in chunk.split_inclusive(|byte| *byte == b'\n') {
                    line.extend_from_slice(piece);
                    // A tool that never prints a newline must not grow the line without bound.
                    if piece.ends_with(b"\n") || line.len() >= MAX_LINE_BYTES {
                        send_line(sender, kind, &line);
                        line.clear();
                    }
                }
            }
        }
        if let Some(sender) = events.as_ref().filter(|_| !line.is_empty()) {
            send_line(sender, kind, &line);
        }

        Ok(CapturedBytes { bytes, truncated })
    })
}

fn send_line(sender: &mpsc::Sender<ProcessEvent>, stream: OutputStream, line: &[u8]) {
    let _ = sender.try_send(ProcessEvent {
        stream,
        line: String::from_utf8_lossy(line)
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_spec_preserves_argument_boundaries() {
        let spec = ProcessSpec::new("mkvmerge")
            .arg("--identify")
            .arg("a file; rm -rf never.mkv");

        assert_eq!(spec.args.len(), 2);
        assert_eq!(spec.args[1], OsString::from("a file; rm -rf never.mkv"));
    }

    #[cfg(windows)]
    fn shell(script: &str) -> ProcessSpec {
        ProcessSpec::new(std::env::var_os("ComSpec").expect("ComSpec")).args(["/C", script])
    }

    #[cfg(not(windows))]
    fn shell(script: &str) -> ProcessSpec {
        ProcessSpec::new("/bin/sh").args(["-c", script])
    }

    #[tokio::test]
    async fn child_tools_do_not_inherit_the_host_environment() {
        // cargo exports this to the test process; it stands in for MKVO_* secrets.
        assert!(std::env::var_os("CARGO_PKG_NAME").is_some());
        let list_environment = if cfg!(windows) { "set" } else { "env" };

        let output = ProcessRunner
            .run(shell(list_environment), CancellationToken::new())
            .await
            .expect("run");

        assert!(output.success());
        assert!(!output.stdout.contains("CARGO_PKG_NAME"));
        assert!(output.stdout.contains("LANG=C.UTF-8"));
        assert!(output.stdout.to_ascii_uppercase().contains("PATH="));
    }

    #[test]
    fn a_silent_tool_is_stopped_for_inactivity() {
        let wait_quietly = if cfg!(windows) {
            "ping -n 9 127.0.0.1 >NUL"
        } else {
            "sleep 8"
        };
        let spec = shell(wait_quietly).inactivity_timeout(Duration::from_millis(300), []);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let started = std::time::Instant::now();

        let error = runtime
            .block_on(ProcessRunner.run(spec, CancellationToken::new()))
            .expect_err("stopped");

        assert!(matches!(error, ProcessError::Inactive(_)));
        assert!(started.elapsed() < Duration::from_secs(5));
        // On Windows `ping` outlives the killed shell and keeps the output pipes open.
        runtime.shutdown_background();
    }

    #[tokio::test]
    async fn printed_output_and_growing_files_count_as_activity() {
        let directory = tempfile::tempdir().expect("tempdir");
        let output = directory.path().join("output.tmp");
        let printed = AtomicU64::new(0);
        let timeout = Duration::from_millis(900);
        let step = Duration::from_millis(150);
        let started = std::time::Instant::now();
        let work = async {
            for size in 1..=5 {
                tokio::time::sleep(step).await;
                if size % 2 == 0 {
                    printed.fetch_add(1, Ordering::Relaxed);
                } else {
                    std::fs::write(&output, vec![0; size]).expect("grow");
                }
            }
        };

        let (stopped, ()) = tokio::join!(
            inactive(Some(timeout), &printed, std::slice::from_ref(&output)),
            work
        );

        assert_eq!(stopped, Some(timeout));
        assert!(started.elapsed() >= step * 5 + timeout);
    }

    /// A metadata probe that hangs like one on a dead share, counting how often it starts.
    fn stalled_probe() -> (
        impl Fn(PathBuf) -> std::io::Result<u64> + Clone + Send + 'static,
        Arc<AtomicU64>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        let started = Arc::new(AtomicU64::new(0));
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = {
            let (started, release) = (Arc::clone(&started), Arc::clone(&release));
            move |_: PathBuf| {
                started.fetch_add(1, Ordering::Relaxed);
                while !release.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(0)
            }
        };
        (probe, started, release)
    }

    #[tokio::test]
    async fn a_hung_output_keeps_a_single_probe_in_flight() {
        let (probe, started, release) = stalled_probe();
        let printed = AtomicU64::new(0);
        let outputs = [PathBuf::from("hung.tmp")];
        let work = async {
            for _ in 0..12 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                printed.fetch_add(1, Ordering::Relaxed);
            }
        };

        let ((), ()) = tokio::join!(
            quiet_for(Duration::from_millis(600), &printed, &outputs, probe),
            work
        );

        release.store(true, Ordering::Relaxed);
        assert_eq!(started.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn a_hung_output_does_not_extend_the_inactivity_timeout() {
        let (probe, _, release) = stalled_probe();
        let timeout = Duration::from_millis(1200);
        let started = std::time::Instant::now();

        quiet_for(
            timeout,
            &AtomicU64::new(0),
            &[PathBuf::from("hung.tmp")],
            probe,
        )
        .await;

        release.store(true, Ordering::Relaxed);
        // Without the bound the last sample waits a full interval past the deadline.
        assert!(
            started.elapsed() < timeout + timeout / 12,
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn output_is_reported_line_by_line() {
        let (sender, mut receiver) = mpsc::channel(8);
        let stream = std::io::Cursor::new(b"first\r\nsecond\nlast".to_vec());

        let captured = capture_stream(
            stream,
            OutputStream::Stderr,
            1024,
            Some(sender),
            Arc::default(),
        )
        .await
        .expect("join")
        .expect("read");

        assert_eq!(captured.bytes, b"first\r\nsecond\nlast");
        assert!(!captured.truncated);
        let mut lines = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            lines.push(event.line);
        }
        assert_eq!(lines, ["first", "second", "last"]);
    }

    #[tokio::test]
    async fn output_without_newlines_is_reported_in_bounded_lines() {
        let (sender, mut receiver) = mpsc::channel(64);
        let stream = std::io::Cursor::new(vec![b'x'; 1024 * 1024]);

        let captured = capture_stream(
            stream,
            OutputStream::Stdout,
            1024,
            Some(sender),
            Arc::default(),
        )
        .await
        .expect("join")
        .expect("read");

        assert_eq!(captured.bytes.len(), 1024);
        assert!(captured.truncated);
        let mut reported = 0;
        while let Ok(event) = receiver.try_recv() {
            assert!(event.line.len() <= MAX_LINE_BYTES + READ_BUFFER_BYTES);
            reported += event.line.len();
        }
        assert_eq!(reported, 1024 * 1024);
    }

    #[test]
    fn process_output_success_requires_zero_exit_code() {
        let output = ProcessOutput {
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
        };
        assert!(output.success());
    }
}
