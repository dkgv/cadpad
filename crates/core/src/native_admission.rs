//! Admission for Core-owned interpreter families, separate from construction and
//! idle-cache limits. File leases also cover a guardian surviving its Core.
//! Each Core-admitted slot also reserves one per-worker memory budget against the
//! aggregate RAM quota until the lease drops.
use crate::{Core, Error, Result, cache::Lease};
use std::{
    fs::{self, File},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

pub const MAX_WORKERS: usize = 6;
pub(crate) struct Slot {
    pub(crate) lease: Lease,
    _ram: Option<RamHold>,
}

/// Releases one family reservation when the admitted worker (or test holder) drops.
pub(crate) struct RamHold {
    reserved: Arc<AtomicU64>,
    bytes: u64,
}
impl Drop for RamHold {
    fn drop(&mut self) {
        self.reserved.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

fn try_lease(root: &Path) -> Result<Option<Lease>> {
    let path = root.join("native-capacity");
    match fs::create_dir(&path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    if fs::symlink_metadata(&path)?.file_type().is_symlink() || !path.is_dir() {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Native capacity directory must be a managed directory",
        ));
    }
    #[cfg(unix)]
    let directory = crate::artifacts::open_scoped_kind(root, &["native-capacity"], true)?;
    for index in 0..MAX_WORKERS {
        #[cfg(unix)]
        let file = {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = std::ffi::CString::new(format!("{index}.lease")).unwrap();
            // SAFETY: open relative to the pinned application directory. The
            // name is generated here; no symlinks or special files are followed.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_NOFOLLOW
                        | libc::O_NONBLOCK
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            unsafe { File::from_raw_fd(fd) }
        };
        #[cfg(not(unix))]
        let file = {
            let name = path.join(format!("{index}.lease"));
            if fs::symlink_metadata(&name).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Native capacity lease cannot be a symlink",
                ));
            }
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(name)?
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != 0 {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Native capacity lease must be an empty regular file",
            ));
        }
        match file.try_lock() {
            Ok(()) => {
                return Ok(Some(Lease { file }));
            }
            Err(std::fs::TryLockError::WouldBlock) => (),
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    Ok(None)
}

#[cfg(test)]
fn try_slot(root: &Path) -> Result<Option<Slot>> {
    Ok(try_lease(root)?.map(|lease| Slot {
        lease,
        _ram: None,
    }))
}

impl Core {
    pub fn ram_usage(&self) -> crate::cache::RamUsage {
        let reserved_bytes = self.reserved_ram_bytes.load(Ordering::Acquire);
        let quota_bytes = self.ram_quota_bytes;
        let family_bytes = self.config.limits.memory_bytes;
        crate::cache::RamUsage {
            quota_bytes,
            reserved_bytes,
            family_bytes,
            within_quota: reserved_bytes <= quota_bytes,
        }
    }

    fn try_reserve_ram(&self) -> Result<RamHold> {
        let bytes = self.config.limits.memory_bytes;
        let quota = self.ram_quota_bytes;
        loop {
            let current = self.reserved_ram_bytes.load(Ordering::Acquire);
            if current.saturating_add(bytes) > quota {
                return Err(Error::domain(
                    "RAM_QUOTA_EXCEEDED",
                    format!(
                        "Admitted native workers already reserve {current} bytes; admitting another family needs {bytes} bytes against the {quota}-byte aggregate RAM quota. Stop idle work or raise the quota before starting another native job"
                    ),
                ));
            }
            match self.reserved_ram_bytes.compare_exchange(
                current,
                current + bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(RamHold {
                        reserved: self.reserved_ram_bytes.clone(),
                        bytes,
                    });
                }
                Err(_) => continue,
            }
        }
    }

    fn admit_lease(&self, lease: Lease) -> Result<Slot> {
        Ok(Slot {
            lease,
            _ram: Some(self.try_reserve_ram()?),
        })
    }

    /// Reserve both resources without retaining either while waiting for the
    /// other. Queued construction must not evict a warm registry or reserve an
    /// interpreter slot until construction can actually start.
    pub(crate) async fn native_construction(
        &self,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<(Slot, tokio::sync::OwnedSemaphorePermit)> {
        loop {
            let construction = self.construction_slot(cancel.clone()).await?;
            let root = self.root.clone();
            let owner = self._process_lock.clone();
            let lease = tokio::task::spawn_blocking(move || {
                let _owner = owner;
                try_lease(&root)
            })
            .await
            .map_err(|_| Error::domain("WORKER_UNAVAILABLE", "Native capacity check failed"))??;
            if *cancel.borrow() {
                return Err(Error::domain("CANCELLED", "Native admission cancelled"));
            }
            if let Some(lease) = lease {
                match self.admit_lease(lease) {
                    Ok(slot) => return Ok((slot, construction)),
                    Err(error) if error.code() == "RAM_QUOTA_EXCEEDED" => {
                        drop(construction);
                        if let Some(mut worker) = self.worker_cache.retire_oldest().await {
                            let mut stopped = tokio::spawn(async move { worker.stop().await });
                            tokio::select! {biased;
                                _=cancel.changed()=>return Err(Error::domain("CANCELLED","Cancelled while retiring idle geometry")),
                                result=&mut stopped=>{result.map_err(|_|Error::domain("WORKER_UNAVAILABLE","Idle native shutdown failed"))??;},
                            }
                            continue;
                        }
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                }
            }
            drop(construction);
            // Wait/retire without holding construction capacity. A warm repair
            // may itself need that capacity before releasing its registry.
            drop(self.native_slot(cancel.clone()).await?);
        }
    }

    pub(crate) async fn native_slot(&self, mut cancel: watch::Receiver<bool>) -> Result<Slot> {
        if *cancel.borrow() {
            return Err(Error::domain("CANCELLED", "Native admission cancelled"));
        }
        // FIFO within a Core; filesystem leases remain authoritative across
        // guardian shutdown and a later Core opening the same application data.
        let _gate = tokio::select! {biased;
            _=cancel.changed()=>return Err(Error::domain("CANCELLED","Cancelled while waiting for native admission")),
            guard=self.native_admission_gate.lock()=>guard,
        };
        loop {
            if *cancel.borrow() {
                return Err(Error::domain("CANCELLED", "Native admission cancelled"));
            }
            let root = self.root.clone();
            let owner = self._process_lock.clone();
            let lease = tokio::task::spawn_blocking(move || {
                let _owner = owner;
                try_lease(&root)
            })
            .await
            .map_err(|_| Error::domain("WORKER_UNAVAILABLE", "Native capacity check failed"))??;
            if let Some(lease) = lease {
                if *cancel.borrow() {
                    return Err(Error::domain("CANCELLED", "Native admission cancelled"));
                }
                match self.admit_lease(lease) {
                    Ok(slot) => return Ok(slot),
                    Err(error) if error.code() == "RAM_QUOTA_EXCEEDED" => {
                        if let Some(mut worker) = self.worker_cache.retire_oldest().await {
                            let mut stopped = tokio::spawn(async move { worker.stop().await });
                            tokio::select! {biased;
                                _=cancel.changed()=>return Err(Error::domain("CANCELLED","Cancelled while retiring idle geometry")),
                                result=&mut stopped=>{result.map_err(|_|Error::domain("WORKER_UNAVAILABLE","Idle native shutdown failed"))??;},
                            }
                            continue;
                        }
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                }
            }
            if let Some(mut worker) = self.worker_cache.retire_oldest().await {
                // Cancellation abandons only the wait. The stop task retains
                // the interpreter and its capacity lease until native shutdown.
                let mut stopped = tokio::spawn(async move { worker.stop().await });
                tokio::select! {biased;
                    _=cancel.changed()=>return Err(Error::domain("CANCELLED","Cancelled while retiring idle geometry")),
                    result=&mut stopped=>{result.map_err(|_|Error::domain("WORKER_UNAVAILABLE","Idle native shutdown failed"))??;},
                }
            } else {
                tokio::select! {biased;
                    _=cancel.changed()=>return Err(Error::domain("CANCELLED","Cancelled while waiting for native capacity")),
                    _=tokio::time::sleep(Duration::from_millis(25))=>{},
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JobState, worker::WorkerConfig};
    async fn waiting(core: &Core) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while core.native_admission_gate.try_lock().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    fn fill(root: &Path) -> Vec<Slot> {
        (0..MAX_WORKERS)
            .map(|_| try_slot(root).unwrap().unwrap())
            .collect()
    }
    #[tokio::test]
    async fn construction_queue_does_not_reserve_native_capacity() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::open(
            temp.path().join("app"),
            WorkerConfig::bundled("unused-worker".into()),
        )
        .await
        .unwrap();
        let (cancel, receive) = watch::channel(false);
        let first = core.construction_slot(receive.clone()).await.unwrap();
        let second = core.construction_slot(receive.clone()).await.unwrap();
        let task = {
            let core = core.clone();
            tokio::spawn(async move { core.native_construction(receive).await })
        };
        // Cancellation after the queue has had a chance to run must release
        // only its construction wait; all six native leases remain available.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!task.is_finished());
        let held = fill(&core.root);
        assert!(core.native_admission_gate.try_lock().is_ok());
        cancel.send(true).unwrap();
        assert!(task.await.unwrap().is_err());
        drop((first, second, held));
    }
    #[test]
    fn file_capacity_is_shared_by_fresh_openers_and_released_without_a_counter() {
        let temp = tempfile::tempdir().unwrap();
        let mut held = fill(temp.path());
        assert!(try_slot(temp.path()).unwrap().is_none());
        drop(held.pop());
        let replacement = try_slot(temp.path()).unwrap().unwrap();
        assert!(try_slot(temp.path()).unwrap().is_none());
        drop(replacement);
        drop(held);
        assert!(try_slot(temp.path()).unwrap().is_some());
    }
    #[tokio::test]
    async fn waiting_build_does_not_hold_construction_capacity_or_materialize_and_cancels() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::open(
            temp.path().join("app"),
            WorkerConfig::bundled("unused-worker".into()),
        )
        .await
        .unwrap();
        let _held = fill(&core.root);
        let ws = core.create_workspace("test").await.unwrap();
        let project = core.get_project(&ws).await.unwrap();
        let rev = project["revision"].as_str().unwrap();
        let draft = core
            .update_source(&ws, rev, None, std::collections::BTreeMap::new(), "test")
            .await
            .unwrap();
        let job = core.build(&ws, rev, &draft.id, "test").await.unwrap();
        waiting(&core).await;
        assert_eq!(
            core.construction_slots.available_permits(),
            crate::construction::MAX_CONSTRUCTIONS
        );
        assert_eq!(
            core.get_job(&ws, &job.id).await.unwrap().state,
            JobState::Queued
        );
        assert_eq!(core.get_project(&ws).await.unwrap()["revision"], rev);
        assert!(!core.root.join("runs").exists());
        assert!(!core.root.join("artifacts").exists());
        core.cancel_job(&ws, &job.id).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while core.get_job(&ws, &job.id).await.unwrap().state != JobState::Cancelled {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!core.root.join("runs").exists());
    }
    #[tokio::test]
    async fn recovery_wait_rechecks_revision_and_cancel_before_source_materialization() {
        for stale in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let core = Core::open(
                temp.path().join("app"),
                WorkerConfig::bundled("unused-worker".into()),
            )
            .await
            .unwrap();
            let mut held = fill(&core.root);
            let ws = core.create_workspace("test").await.unwrap();
            let rev = core.get_project(&ws).await.unwrap()["revision"]
                .as_str()
                .unwrap()
                .to_owned();
            let (cancel, receive) = watch::channel(false);
            let task = {
                let core = core.clone();
                let ws = ws.clone();
                let rev = rev.clone();
                tokio::spawn(async move { core.verified_worker(&ws, &rev, receive).await })
            };
            waiting(&core).await;
            assert_eq!(
                core.construction_slots.available_permits(),
                crate::construction::MAX_CONSTRUCTIONS
            );
            assert!(!core.root.join("runs").exists());
            if stale {
                let mut workspaces = core.workspaces.lock().await;
                let ws = workspaces.get_mut(&ws).unwrap();
                let mut next = ws.stored.revisions[&rev].clone();
                next.id = crate::id();
                ws.stored.current = next.id.clone();
                ws.stored.revisions.insert(next.id.clone(), next);
                drop(workspaces);
                drop(held.pop());
            } else {
                cancel.send(true).unwrap();
            }
            let error = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .err()
                .expect("Admission must fail before execution");
            assert_eq!(
                error.code(),
                if stale { "STALE_ENTITY" } else { "CANCELLED" }
            );
            assert!(!core.root.join("runs").exists());
            assert!(!core.root.join("artifacts").exists());
        }
    }
    #[tokio::test]
    async fn aggregate_ram_quota_admits_one_family_and_rejects_when_busy_budget_is_full() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = WorkerConfig::bundled("unused-worker".into());
        config.limits.memory_bytes = 64 * 1024 * 1024;
        let family = config.limits.memory_bytes;
        let core = Core::open_with_quotas(
            temp.path().join("app"),
            config,
            crate::cache::DEFAULT_DISK_QUOTA_BYTES,
            family,
        )
        .await
        .unwrap();
        let empty = core.ram_usage();
        assert_eq!(empty.reserved_bytes, 0);
        assert!(empty.within_quota);
        assert_eq!(empty.quota_bytes, family);
        assert_eq!(empty.family_bytes, family);
        let (_alive, receive) = watch::channel(false);
        let held = core.native_slot(receive.clone()).await.unwrap();
        let reserved = core.ram_usage();
        assert_eq!(reserved.reserved_bytes, family);
        assert!(reserved.within_quota);
        let error = match core.native_slot(receive.clone()).await {
            Ok(_) => panic!("second family must not admit over the aggregate RAM quota"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "RAM_QUOTA_EXCEEDED");
        assert_eq!(core.ram_usage().reserved_bytes, family);
        drop(held);
        assert_eq!(core.ram_usage().reserved_bytes, 0);
        let again = core.native_slot(receive).await.unwrap();
        assert_eq!(core.ram_usage().reserved_bytes, family);
        drop(again);
        drop(_alive);
    }
    #[cfg(unix)]
    #[test]
    fn capacity_files_do_not_follow_symlinked_directories_or_leases() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let root = temp.path().join("app");
        fs::create_dir(&root).unwrap();
        symlink(&outside, root.join("native-capacity")).unwrap();
        assert!(try_slot(&root).is_err());
        fs::remove_file(root.join("native-capacity")).unwrap();
        fs::create_dir(root.join("native-capacity")).unwrap();
        let marker = outside.join("marker");
        fs::write(&marker, b"preserve").unwrap();
        symlink(&marker, root.join("native-capacity/0.lease")).unwrap();
        assert!(try_slot(&root).is_err());
        assert_eq!(fs::read(marker).unwrap(), b"preserve");
    }
}
