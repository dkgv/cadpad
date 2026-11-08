//! Native lifetime owner for one CAD process. No Python, project state or RPC
//! parsing runs here. stdout/stdin connect the app directly to the worker.
#[cfg(unix)]
#[path = "support/guardian_anchor.rs"]
mod guardian_anchor;
#[path = "../worker_limits.rs"]
mod worker_limits;
#[cfg(unix)]
fn run() -> std::io::Result<()> {
    use std::{
        io,
        io::Write,
        os::fd::{FromRawFd, RawFd},
        process::{Command, Stdio},
    };
    let mut args = std::env::args_os().skip(1);
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected --liveness-fd <fd> [--cpu-time-ms <n>] [--memory-bytes <n>] [--admission-fd <fd>] -- <absolute executable> [args]",
        )
    };
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--liveness-fd")) {
        return Err(invalid());
    }
    let fd: RawFd = args
        .next()
        .and_then(|v| v.into_string().ok())
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 2)
        .ok_or_else(invalid)?;
    let mut limits = worker_limits::Limits::default();
    let mut admission_fd = None;
    loop {
        let next = args.next().ok_or_else(invalid)?;
        if next == "--" {
            break;
        }
        let value: u64 = args
            .next()
            .and_then(|v| v.into_string().ok())
            .and_then(|v| v.parse().ok())
            .ok_or_else(invalid)?;
        match next.to_str() {
            Some("--cpu-time-ms") => limits.cpu_time_ms = value,
            Some("--memory-bytes") => limits.memory_bytes = value,
            Some("--admission-fd") if value > 2 && value <= i32::MAX as u64 => {
                admission_fd = Some(value as i32)
            }
            _ => return Err(invalid()),
        }
    }
    limits.validate()?;
    let executable = std::path::PathBuf::from(args.next().ok_or_else(invalid)?);
    if !executable.is_absolute() {
        return Err(invalid());
    }
    // This socket belongs only to the guardian. Its peer belongs only to the
    // app; neither the CAD interpreter nor a later exec can keep it alive.
    // SAFETY: fcntl validates the inherited descriptor and changes only its flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if let Some(admission) = admission_fd {
        // Capacity belongs to the app and trusted guardian only. Generated CAD
        // code must not inherit a descriptor it could close or unlock.
        if admission == fd || unsafe { libc::fcntl(admission, libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        {
            return Err(invalid());
        }
    }
    // SAFETY: fd was inherited exclusively for this guardian and is now owned
    // by this stream. CLOEXEC keeps it out of the CAD child.
    let mut owner = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    owner.set_write_timeout(Some(std::time::Duration::from_millis(100)))?;
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let _anchor = guardian_anchor::prepare(&mut command, fd)?;
    let mut child = command.spawn()?;
    // Tell the app the session-leader PID over the private liveness socket so it
    // can reap the group if both this guardian and the anchor are killed.
    if let Ok(bytes) = serde_json::to_vec(&serde_json::json!({
        "kind": "started",
        "pid": child.id()
    })) {
        let _ = owner.write_all(&bytes);
        let _ = owner.write_all(b"\n");
    }
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut pending_report: Option<worker_limits::Report> = None;
    let mut already_reaped = false;
    let result = (|| {
        loop {
            if let Some(status) = child.try_wait()? {
                already_reaped = true;
                return Ok(status.code().unwrap_or(1));
            }
            #[cfg(target_os = "macos")]
            {
                let report = match worker_limits::sample(child.id(), limits) {
                    Ok(report) => report,
                    Err(error) => {
                        // Exit can race the sample. Never mistake that normal
                        // transition for loss of monitoring or act on a reused PID.
                        if let Some(status) = child.try_wait()? {
                            already_reaped = true;
                            return Ok(status.code().unwrap_or(1));
                        }
                        Some(worker_limits::Report::MonitorUnavailable {
                            message: error.to_string(),
                        })
                    }
                };
                if let Some(report) = report {
                    pending_report = Some(report);
                    return Ok(0);
                }
            }
            let mut event = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // A closed peer is persistent kernel state, including SIGKILL/app
            // crash. No polling of a reusable PID or Python thread is involved.
            // SAFETY: poll receives one live pollfd for the inherited socket.
            let ready = unsafe { libc::poll(&mut event, 1, 100) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if ready > 0 {
                return Ok(0);
            }
        }
    })();
    // The worker is our child and remains owned until wait reaps it; its PID
    // cannot be recycled between the liveness event and this kill. Production
    // containment denies fork, so there is no uncontrolled descendant tree.
    // If try_wait already reaped the interpreter, its numeric PID may now be
    // reused. The anchor observes our control socket closing and handles its own
    // still-live group; never signal that old ID from this process after reaping.
    let killed = if already_reaped {
        Ok(())
    } else {
        guardian_anchor::kill_group(child.id())
    };
    child.wait()?;
    // Kill/reap before reporting: even a slow/closed report reader must not
    // delay stopping native execution. Python cannot forge this private socket.
    if let Some(report) = pending_report
        && let Ok(bytes) = serde_json::to_vec(&report)
    {
        let _ = owner.write_all(&bytes);
    }
    match killed {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
        Err(error) => return Err(error),
    }
    // Close inherited cache leases only after native execution is gone.
    let code = result?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn main() {
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref() == Some("--anchor") {
        guardian_anchor::run();
    }
    #[cfg(unix)]
    if let Err(error) = run() {
        eprintln!("CAD worker guardian: {error}");
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        eprintln!("CAD worker guardian requires Unix process support");
        std::process::exit(1);
    }
}
