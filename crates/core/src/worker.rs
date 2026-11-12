use crate::{Error, Result};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::watch,
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Containment {
    Required,
    UncontainedForTrustedTests,
}
#[derive(Clone)]
pub struct WorkerConfig {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub module_path: Option<PathBuf>,
    pub containment: Containment,
    pub guardian_executable: PathBuf,
    pub limits: crate::worker_limits::Limits,
}
impl WorkerConfig {
    fn default_guardian() -> PathBuf {
        let executable = std::env::current_exe().unwrap_or_default();
        let mut directory = executable.parent().unwrap_or(Path::new("."));
        if matches!(
            directory.file_name().and_then(|v| v.to_str()),
            Some("deps" | "examples")
        ) {
            directory = directory.parent().unwrap_or(directory);
        }
        let sibling = directory.join("cadpad-worker-guard");
        if sibling.is_file() {
            return sibling;
        }
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/cadpad-worker-guard")
    }
    pub fn guarded_by(mut self, executable: PathBuf) -> Self {
        self.guardian_executable = executable;
        self
    }
    /// Development always uses the pinned project virtual environment explicitly.
    pub fn development(repository: &Path) -> Self {
        Self {
            executable: repository.join(if cfg!(windows) {
                ".venv/Scripts/python.exe"
            } else {
                ".venv/bin/python"
            }),
            args: vec!["-m".into(), "cadpad_worker".into()],
            module_path: Some(repository.join("python")),
            containment: Containment::Required,
            guardian_executable: Self::default_guardian(),
            limits: Default::default(),
        }
    }
    /// Trusted fixture scripts may need host-side markers outside their run.
    /// The desktop and shared tools never expose this test-only bypass.
    pub fn uncontained_for_trusted_tests(mut self) -> Self {
        self.containment = Containment::UncontainedForTrustedTests;
        self
    }
    pub fn bundled(executable: PathBuf) -> Self {
        Self {
            executable,
            args: vec![],
            module_path: None,
            containment: Containment::Required,
            guardian_executable: Self::default_guardian(),
            limits: Default::default(),
        }
    }
}

// Large arrays use artifact transport. JSON metadata has a separate hard bound.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
async fn read_response(output: &mut BufReader<ChildStdout>) -> Result<String> {
    let mut bytes = Vec::new();
    loop {
        let available = output.fill_buf().await?;
        if available.is_empty() {
            return Err(Error::domain(
                "WORKER_UNAVAILABLE",
                "Geometry worker exited before a complete reply",
            ));
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if bytes.len() + count > MAX_RESPONSE_BYTES {
            return Err(Error::domain(
                "WORKER_UNAVAILABLE",
                "Geometry worker reply exceeds the 8 MiB metadata capacity",
            ));
        }
        let complete = available[count - 1] == b'\n';
        bytes.extend_from_slice(&available[..count]);
        output.consume(count);
        if complete {
            return String::from_utf8(bytes).map_err(|_| {
                Error::domain("WORKER_UNAVAILABLE", "Geometry worker reply is not UTF-8")
            });
        }
    }
}

pub struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    generation: String,
    sequence: u64,
    pub cache: PathBuf,
    _leases: Vec<crate::cache::Lease>,
    _admission: Option<crate::native_admission::Slot>,
    limits: crate::worker_limits::Limits,
    #[cfg(unix)]
    liveness: Option<std::os::unix::net::UnixStream>,
    /// Interpreter session leader, reported privately by the guardian. Used to
    /// reap the native group when both guardian and anchor are already gone.
    #[cfg(unix)]
    interpreter_pid: Option<i32>,
}
impl crate::Core {
    pub(crate) async fn spawn_managed_worker(
        &self,
        cache: PathBuf,
        run: crate::cache::Run,
        admission: crate::native_admission::Slot,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Worker> {
        if *cancel.borrow() {
            return Err(Error::domain(
                "CANCELLED",
                "Cancelled before worker initialization",
            ));
        }
        // Collection must not reserve the just-created .lease between open
        // and the initial shared lock. Worker ownership takes over thereafter.
        let _publication = tokio::select! { biased;
            _ = cancel.changed() => return Err(Error::domain("CANCELLED", "Cancelled while waiting for cache initialization")),
            guard = self.display_publication.read() => guard,
        };
        Worker::spawn_admitted(&self.config, cache, Some(run), admission).await
    }
}

impl Worker {
    pub async fn spawn(config: &WorkerConfig, cache: PathBuf) -> Result<Self> {
        Self::spawn_with_run(config, cache, None).await
    }
    pub(crate) async fn spawn_with_run(
        config: &WorkerConfig,
        cache: PathBuf,
        run: Option<crate::cache::Run>,
    ) -> Result<Self> {
        Self::spawn_inner(config, cache, run, None).await
    }
    pub(crate) async fn spawn_admitted(
        config: &WorkerConfig,
        cache: PathBuf,
        run: Option<crate::cache::Run>,
        admission: crate::native_admission::Slot,
    ) -> Result<Self> {
        Self::spawn_inner(config, cache, run, Some(admission)).await
    }
    async fn spawn_inner(
        config: &WorkerConfig,
        cache: PathBuf,
        run: Option<crate::cache::Run>,
        admission: Option<crate::native_admission::Slot>,
    ) -> Result<Self> {
        config.limits.validate()?;
        let source = run.as_ref().map(|run| run.source.clone());
        let mut leases = vec![crate::cache::Lease::shared(&cache)?];
        if let Some(run) = run {
            leases.push(run.lease);
        }
        tokio::fs::create_dir_all(&cache).await?;
        let generation = Uuid::new_v4().to_string();
        let mut command = crate::containment::command(config, &cache, source.as_deref())?;
        if config.containment == Containment::UncontainedForTrustedTests {
            command.env_clear();
        }
        command
            .args(&config.args)
            .arg("--cache")
            .arg(&cache)
            .arg("--generation")
            .arg(&generation)
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONNOUSERSITE", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("PYTHONSAFEPATH", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(!cfg!(unix));
        if let Some(path) = &config.module_path {
            command.env("PYTHONPATH", path);
        }
        #[cfg(unix)]
        let (guardian_liveness, mut liveness) = std::os::unix::net::UnixStream::pair()?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let target = command.as_std();
            let mut guarded = tokio::process::Command::new(&config.guardian_executable);
            guarded
                .arg("--liveness-fd")
                .arg(guardian_liveness.as_raw_fd().to_string())
                .arg("--cpu-time-ms")
                .arg(config.limits.cpu_time_ms.to_string())
                .arg("--memory-bytes")
                .arg(config.limits.memory_bytes.to_string());
            if let Some(slot) = &admission {
                guarded.args(["--admission-fd", &slot.lease.file.as_raw_fd().to_string()]);
            }
            guarded
                .arg("--")
                .arg(target.get_program())
                .args(target.get_args());
            guarded.env_clear();
            for (key, value) in target.get_envs() {
                if let Some(value) = value {
                    guarded.env(key, value);
                }
            }
            if let Some(directory) = target.get_current_dir() {
                guarded.current_dir(directory);
            }
            guarded
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            command = guarded;
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut descriptors = leases
                .iter()
                .map(|lease| lease.file.as_raw_fd())
                .collect::<Vec<_>>();
            descriptors.push(guardian_liveness.as_raw_fd());
            if let Some(slot) = &admission {
                descriptors.push(slot.lease.file.as_raw_fd());
            }
            let contained = config.containment == Containment::Required;
            // SAFETY: pre_exec only calls async-signal-safe setsid/fcntl/getrlimit/setrlimit. The parent
            // retains each descriptor until spawn completes; children keep the
            // advisory locks even after an abrupt app exit.
            unsafe {
                command.pre_exec(move || {
                    // A separate session also prevents terminal/group signals
                    // and orphaned stopped-group SIGHUP from killing the guard.
                    // No terminal is acquired; all protocol I/O remains piped.
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if contained {
                        for (resource, limit) in [
                            (libc::RLIMIT_CORE, 0),
                            (libc::RLIMIT_NOFILE, 512),
                            (libc::RLIMIT_FSIZE, 1024 * 1024 * 1024),
                        ] {
                            let mut current = libc::rlimit {
                                rlim_cur: 0,
                                rlim_max: 0,
                            };
                            if libc::getrlimit(resource, &mut current) < 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                            let maximum = current.rlim_max.min(limit);
                            let limits = libc::rlimit {
                                rlim_cur: current.rlim_cur.min(maximum),
                                rlim_max: maximum,
                            };
                            if libc::setrlimit(resource, &limits) < 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                        }
                    }
                    for fd in &descriptors {
                        if libc::fcntl(*fd, libc::F_SETFD, 0) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
        }
        // No provider credentials or parent environment are inherited.
        let mut child = command.spawn().map_err(|e| {
            Error::domain(
                "WORKER_UNAVAILABLE",
                format!("CAD worker guardian {}: {e}. Build cadpad-worker-guard before starting native CAD execution", config.guardian_executable.display()),
            )
        })?;
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        #[cfg(unix)]
        let interpreter_pid = {
            use std::io::Read;
            liveness.set_nonblocking(true)?;
            let started = std::time::Instant::now();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 256];
            let pid = loop {
                match liveness.read(&mut chunk) {
                    Ok(0) => break None,
                    Ok(read) => {
                        bytes.extend_from_slice(&chunk[..read]);
                        if let Some(line) = bytes.split(|&b| b == b'\n').next()
                            && let Ok(value) = serde_json::from_slice::<Value>(line)
                            && value["kind"] == "started"
                        {
                            break value["pid"]
                                .as_u64()
                                .and_then(|pid| i32::try_from(pid).ok().filter(|pid| *pid > 1));
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if started.elapsed() > std::time::Duration::from_secs(2) {
                            break None;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break None,
                }
            };
            let _ = liveness.set_nonblocking(false);
            pid
        };
        Ok(Self {
            child,
            input,
            output,
            generation,
            sequence: 0,
            cache,
            _leases: leases,
            _admission: admission,
            limits: config.limits,
            #[cfg(unix)]
            liveness: Some(liveness),
            #[cfg(unix)]
            interpreter_pid,
        })
    }
    #[cfg(unix)]
    fn reap_interpreter_group(pid: i32) {
        // Session leader PGID equals PID. Best-effort: ESRCH is fine.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
            libc::kill(pid, libc::SIGKILL);
        }
    }
    pub async fn request(
        &mut self,
        revision: &str,
        method: &str,
        params: Value,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Value> {
        if *cancel.borrow() {
            self.stop().await?;
            return Err(Error::domain("CANCELLED", "Operation cancelled"));
        }
        self.sequence += 1;
        let request = json!({"jsonrpc":"2.0", "id":self.sequence,"protocol_version":1,
            "worker_generation":self.generation,"revision":revision,"method":method,"params":params});
        let bytes = serde_json::to_vec(&request)?;
        let input = &mut self.input;
        let output = &mut self.output;
        let operation = async {
            input.write_all(&bytes).await?;
            input.write_all(b"\n").await?;
            input.flush().await?;
            read_response(output).await
        };
        let received = tokio::select! {
            result = operation => Some(result),
            _ = async { loop { if *cancel.borrow_and_update() { break; } else if cancel.changed().await.is_err() { std::future::pending::<()>().await; } } } => None,
            _ = tokio::time::sleep(std::time::Duration::from_millis(self.limits.operation_timeout_ms)) => {
                self.stop().await?;
                return Err(Error::Domain { code: "RESOURCE_LIMIT".into(), message: "CAD operation exceeded its time budget; simplify the model or retry".into(), details: json!({"resource":"operation_time_ms","limit":self.limits.operation_timeout_ms}) });
            },
        };
        let line = match received {
            None => {
                self.stop().await?;
                return Err(Error::domain("CANCELLED", "Worker terminated and reaped"));
            }
            Some(Err(error)) => {
                if let Some(report) = self.stop_report().await? {
                    return Err(resource_error(report));
                }
                return Err(error);
            }
            Some(Ok(line)) => line,
        };
        let response: Value = match serde_json::from_str(&line) {
            Ok(response) => response,
            Err(_) => {
                self.stop().await?;
                return Err(Error::domain(
                    "WORKER_UNAVAILABLE",
                    "Geometry worker returned invalid JSON",
                ));
            }
        };
        if response["jsonrpc"] != "2.0"
            || response.get("result").is_some() == response.get("error").is_some()
            || response["id"] != self.sequence
            || response["worker_generation"] != self.generation
            || response["revision"] != revision
            || response["protocol_version"] != 1
        {
            self.stop().await?;
            return Err(Error::domain(
                "WORKER_UNAVAILABLE",
                "Mismatched worker response",
            ));
        }
        if let Some(error) = response.get("error") {
            return Err(Error::Domain {
                code: error["data"]["code"]
                    .as_str()
                    .unwrap_or("BUILD_FAILED")
                    .into(),
                message: error["message"]
                    .as_str()
                    .unwrap_or("Geometry failed")
                    .into(),
                details: error["data"].clone(),
            });
        }
        let mut result = json!({"data":response["result"],"logs":response["logs"],"logs_truncated":response["logs_truncated"],"display_cache_policy":response["display_cache_policy"]});
        if method == "build" {
            result["geometry_cache"] = response["geometry_cache"].clone();
        }
        Ok(result)
    }
    pub async fn stop(&mut self) -> Result<()> {
        self.stop_report().await?;
        Ok(())
    }
    async fn stop_report(&mut self) -> Result<Option<crate::worker_limits::Report>> {
        #[cfg(unix)]
        let mut liveness = self.liveness.take();
        #[cfg(unix)]
        if let Some(stream) = &liveness {
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // This is still our unreaped child, so its PID cannot be recycled.
            // Resume a stopped supervisor so its normal shutdown path can reap.
            unsafe {
                libc::kill(pid as i32, libc::SIGCONT);
            }
        }
        #[cfg(not(unix))]
        if self.child.try_wait()?.is_none() {
            self.child.start_kill()?;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(1), self.child.wait()).await {
            Ok(result) => {
                result?;
            }
            Err(_) => {
                self.child.start_kill()?;
            }
        }
        self.child.wait().await?;
        #[cfg(unix)]
        if let Some(pid) = self.interpreter_pid.take() {
            Self::reap_interpreter_group(pid);
        }
        #[cfg(unix)]
        if let Some(stream) = liveness.as_mut() {
            use std::io::Read;
            stream.set_nonblocking(true)?;
            let mut bytes = [0u8; 1024];
            let mut length = 0;
            loop {
                let read = match stream.read(&mut bytes[length..]) {
                    Ok(read) => read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    // The report writer has exited, so every report byte is
                    // already queued. The passive anchor can briefly retain
                    // the peer while the kernel delivers its group termination.
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error.into()),
                };
                if read == 0 {
                    break;
                }
                length += read;
                if length == bytes.len() {
                    return Err(Error::domain(
                        "WORKER_UNAVAILABLE",
                        "Invalid guardian report size",
                    ));
                }
            }
            if length > 0 {
                return serde_json::from_slice(&bytes[..length])
                    .map(Some)
                    .map_err(|_| {
                        Error::domain("WORKER_UNAVAILABLE", "Invalid guardian resource report")
                    });
            }
        }
        Ok(None)
    }
}
#[cfg(unix)]
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(pid) = self.interpreter_pid.take() {
            Self::reap_interpreter_group(pid);
        }
    }
}
fn resource_error(report: crate::worker_limits::Report) -> Error {
    use crate::worker_limits::{Report, Resource};
    let (code, message) = match &report {
        Report::Limit {
            resource: Resource::CpuTimeMs,
            ..
        } => (
            "RESOURCE_LIMIT",
            "CAD worker exceeded its lifetime CPU budget; simplify the model or retry",
        ),
        Report::Limit {
            resource: Resource::MemoryBytes,
            ..
        } => (
            "RESOURCE_LIMIT",
            "CAD worker exceeded its memory budget; simplify the model or retry",
        ),
        Report::MonitorUnavailable { .. } => (
            "WORKER_UNAVAILABLE",
            "Native worker resource monitoring failed; the worker was terminated",
        ),
    };
    Error::Domain {
        code: code.into(),
        message: message.into(),
        details: json!(report),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod framing_tests {
    use super::*;
    use std::time::Duration;
    fn config(code: &str) -> WorkerConfig {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let mut config = WorkerConfig::development(&repo);
        config.args = vec!["-c".into(), code.into()];
        config
    }
    #[tokio::test]
    async fn oversized_and_invalid_replies_are_bounded_and_reaped() {
        for code in [
            "import os,time\nfor _ in range(2049): os.write(1,b'x'*4096)\ntime.sleep(60)",
            "import os,time\nos.write(1,b'not-json\\n')\ntime.sleep(60)",
            "import os\nos.write(1,b'partial')",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut worker = Worker::spawn(&config(code), temp.path().join("cache"))
                .await
                .unwrap();
            let (_send, cancel) = watch::channel(false);
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                worker.request("r1", "build", json!({}), cancel),
            )
            .await
            .unwrap();
            assert!(matches!(result,Err(error) if error.code()=="WORKER_UNAVAILABLE"));
            assert!(worker.child.try_wait().unwrap().is_some());
        }
    }
    #[tokio::test]
    async fn cancellation_interrupts_a_blocked_request_write_and_reaps_the_worker() {
        let temp = tempfile::tempdir().unwrap();
        let mut worker = Worker::spawn(
            &config("import time\ntime.sleep(60)"),
            temp.path().join("cache"),
        )
        .await
        .unwrap();
        let (send, cancel) = watch::channel(false);
        let cancel_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            send.send_replace(true);
        });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            worker.request(
                "r1",
                "build",
                json!({"padding":"x".repeat(1024*1024)}),
                cancel,
            ),
        )
        .await
        .unwrap();
        assert!(matches!(result,Err(error) if error.code()=="CANCELLED"));
        assert!(worker.child.try_wait().unwrap().is_some());
        cancel_task.await.unwrap();
    }
}
