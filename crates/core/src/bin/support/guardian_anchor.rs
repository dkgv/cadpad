//! Independent native shutdown path. Runs no CAD or project code.
use std::{
    ffi::CString,
    io,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, process::CommandExt},
    },
    process::Command,
};

pub struct Lease {
    _writer: std::os::unix::net::UnixStream,
    _reader: std::os::unix::net::UnixStream,
    _ready_reader: std::os::unix::net::UnixStream,
    _ready_writer: std::os::unix::net::UnixStream,
}
pub fn prepare(command: &mut Command, owner: i32) -> io::Result<Lease> {
    let (reader, writer) = std::os::unix::net::UnixStream::pair()?;
    let (ready_reader, ready_writer) = std::os::unix::net::UnixStream::pair()?;
    let watch = reader.as_raw_fd();
    let parent = writer.as_raw_fd();
    let ready = ready_writer.as_raw_fd();
    let ack = ready_reader.as_raw_fd();
    let executable = CString::new(std::env::current_exe()?.as_os_str().as_bytes())?;
    let args = [
        executable,
        CString::new("--anchor")?,
        CString::new(owner.to_string())?,
        CString::new(watch.to_string())?,
        CString::new(ready.to_string())?,
    ];
    // This single-threaded native program has no concurrent descriptor creators.
    // Command's later exec-error pipe is CLOEXEC and closes when the anchor execs.
    let descriptors = std::fs::read_dir("/dev/fd")?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<i32>().ok())
        .collect::<Vec<_>>();
    // SAFETY: child setup uses only async-signal-safe syscalls and stack data.
    // CString storage and descriptor lists were allocated before spawning.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            libc::close(parent);
            let child = libc::fork();
            if child < 0 {
                return Err(io::Error::last_os_error());
            }
            if child == 0 {
                for fd in &descriptors {
                    if *fd != owner && *fd != watch && *fd != ready {
                        libc::close(*fd);
                    }
                }
                for fd in [owner, watch, ready] {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        libc::_exit(126);
                    }
                }
                let argv = [
                    args[0].as_ptr(),
                    args[1].as_ptr(),
                    args[2].as_ptr(),
                    args[3].as_ptr(),
                    args[4].as_ptr(),
                    std::ptr::null(),
                ];
                libc::execv(args[0].as_ptr(), argv.as_ptr());
                libc::_exit(127);
            }
            libc::close(ready);
            libc::close(watch);
            let mut event = libc::pollfd {
                fd: ack,
                events: libc::POLLIN,
                revents: 0,
            };
            let mut byte = 0u8;
            let ok = libc::poll(&mut event, 1, 2000) > 0
                && libc::read(ack, (&mut byte as *mut u8).cast(), 1) == 1
                && byte == b'R';
            libc::close(ack);
            if !ok {
                libc::kill(child, libc::SIGKILL);
                while libc::waitpid(child, std::ptr::null_mut(), 0) < 0
                    && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                {
                }
                return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
            }
            // The CAD image must not inherit the app or CI runner's sockets.
            // python.org's bin/python posix_spawns Python.app and preserves
            // descriptor numbers, so a runner socket stays visible at fd 131.
            // CLOEXEC closes them on exec without touching the spawn error pipe,
            // which is created after this snapshot.
            for fd in &descriptors {
                if *fd <= 2 {
                    continue;
                }
                if libc::fcntl(*fd, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EBADF) {
                        return Err(error);
                    }
                }
            }
            Ok(())
        });
    }
    Ok(Lease {
        _writer: writer,
        _reader: reader,
        _ready_reader: ready_reader,
        _ready_writer: ready_writer,
    })
}

pub fn run() -> ! {
    let fds = std::env::args()
        .skip(2)
        .map(|value| value.parse::<i32>())
        .collect::<Result<Vec<_>, _>>();
    let Ok(fds) = fds else { std::process::exit(2) };
    if fds.len() != 3
        || fds.iter().any(|fd| *fd <= 2)
        || fds[0] == fds[1]
        || fds[0] == fds[2]
        || fds[1] == fds[2]
    {
        std::process::exit(2)
    }
    // A session leader cannot move groups; its already-exec'd child cannot be
    // moved by that parent either. Our membership keeps this group identity live.
    // SAFETY: getters and fcntl operate on this process and inherited descriptors.
    let group = unsafe { libc::getpgrp() };
    if group <= 1
        || group != unsafe { libc::getsid(0) }
        || group != unsafe { libc::getppid() }
        || group == unsafe { libc::getpid() }
    {
        std::process::exit(2)
    }
    // Resolve the primary guardian (parent of this session leader) so a stopped
    // supervisor can still be reaped after app loss. It lives outside our group.
    let guardian = {
        // Matches Darwin struct proc_bsdinfo (136 bytes); only pid/ppid are read.
        #[repr(C)]
        struct Info {
            pbi_flags: u32,
            pbi_status: u32,
            pbi_xstatus: u32,
            pbi_pid: u32,
            pbi_ppid: u32,
            rest: [u8; 116],
        }
        const PROC_PIDTBSDINFO: i32 = 3;
        if std::mem::size_of::<Info>() != 136 {
            std::process::exit(2);
        }
        let mut info = Info {
            pbi_flags: 0,
            pbi_status: 0,
            pbi_xstatus: 0,
            pbi_pid: 0,
            pbi_ppid: 0,
            rest: [0; 116],
        };
        // SAFETY: fixed BSD info buffer for this session-leader interpreter.
        let size = unsafe {
            libc::proc_pidinfo(
                group,
                PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut Info).cast(),
                std::mem::size_of::<Info>() as i32,
            )
        };
        let pid = info.pbi_ppid as i32;
        (size == std::mem::size_of::<Info>() as i32
            && info.pbi_pid == group as u32
            && pid > 1
            && pid != group
            && pid != unsafe { libc::getpid() })
        .then_some(pid)
    };
    for fd in &fds {
        if unsafe { libc::fcntl(*fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            std::process::exit(2)
        }
    }
    // Signal readiness only after this trusted executable has started. Generated
    // code is never exec'd before the anchor is armed and protected by P_EXEC.
    let byte = b'R';
    if unsafe { libc::write(fds[2], (&byte as *const u8).cast(), 1) } != 1 {
        std::process::exit(2)
    }
    unsafe {
        libc::close(fds[2]);
    }
    let mut events = [
        libc::pollfd {
            fd: fds[0],
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: fds[1],
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let result = unsafe { libc::poll(events.as_mut_ptr(), 2, -1) };
        if result < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if result != 0 {
            // Membership proves the group cannot have vanished/reused this ID.
            // SIGKILL the primary guardian first so a stopped supervisor cannot
            // outlive app loss; then destroy this session group (including us).
            unsafe {
                if let Some(pid) = guardian {
                    libc::kill(pid, libc::SIGKILL);
                }
                libc::kill(-group, libc::SIGKILL);
                libc::_exit(0);
            }
        }
    }
}
pub fn kill_group(pid: u32) -> io::Result<()> {
    // The session's trusted anchor retains group membership through shutdown,
    // including when the interpreter exited just before its parent's wait.
    if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}
