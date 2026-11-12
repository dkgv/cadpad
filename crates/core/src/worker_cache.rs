//! Bounded idle native registries. Busy leases and candidate construction have
//! separate lifetimes; Core native-family admission is tracked independently.
//! Retiring an idle registry releases its capacity lease and RAM reservation;
//! the count cap alone is not the aggregate RAM quota.
use crate::{Worker, Workspace};
use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
    sync::{
        Arc, RwLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::Mutex;

pub(crate) const MAX_IDLE_WORKERS: usize = 4;
type Workspaces = Mutex<HashMap<String, Workspace>>;

pub(crate) struct WorkerCache {
    workspaces: Weak<Workspaces>,
    clock: AtomicU64,
    queued: AtomicBool,
    retained: RwLock<Option<HashSet<String>>>,
}
struct QueuedMaintenance(Arc<WorkerCache>);
impl Drop for QueuedMaintenance {
    fn drop(&mut self) {
        self.0.queued.store(false, Ordering::Release);
    }
}
impl WorkerCache {
    pub(crate) fn new(workspaces: &Arc<Workspaces>) -> Self {
        Self {
            workspaces: Arc::downgrade(workspaces),
            clock: AtomicU64::new(1),
            queued: AtomicBool::new(false),
            retained: RwLock::new(None),
        }
    }
    pub(crate) fn retain(self: &Arc<Self>, workspaces: Vec<String>) {
        *self.retained.write().unwrap() = Some(workspaces.into_iter().collect());
        self.schedule();
    }
    pub(crate) fn next_use(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }
    pub(crate) fn acquire(self: &Arc<Self>, workspace: &mut Workspace) -> Option<WorkerUse> {
        let worker = workspace.worker.as_ref()?.clone();
        workspace.worker_last_used = self.next_use();
        Some(WorkerUse {
            worker: Some(worker),
            cache: self.clone(),
        })
    }
    pub(crate) async fn retire_oldest(&self) -> Option<Worker> {
        let workspaces = self.workspaces.upgrade()?;
        let worker = {
            let mut workspaces = workspaces.lock().await;
            let id = workspaces
                .iter()
                .filter(|(_, ws)| {
                    ws.worker
                        .as_ref()
                        .is_some_and(|worker| Arc::strong_count(worker) == 1)
                })
                .min_by_key(|(_, ws)| ws.worker_last_used)
                .map(|(id, _)| id.clone())?;
            workspaces.get_mut(&id)?.worker.take()?
        };
        Arc::try_unwrap(worker).ok().map(Mutex::into_inner)
    }
    pub(crate) fn schedule(self: &Arc<Self>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.queued.swap(true, Ordering::AcqRel) {
            return;
        }
        let cache = self.clone();
        let queued = QueuedMaintenance(cache.clone());
        runtime.spawn(async move {
            // Coalesce queued maintenance. New idle workers during native stop
            // can schedule a later pass, without retaining the Core/app lock.
            drop(queued);
            cache.trim().await;
        });
    }
    async fn trim(&self) {
        let Some(workspaces) = self.workspaces.upgrade() else {
            return;
        };
        let retired = {
            let mut workspaces = workspaces.lock().await;
            let mut idle = workspaces
                .iter()
                .filter_map(|(id, workspace)| {
                    workspace
                        .worker
                        .as_ref()
                        .filter(|worker| Arc::strong_count(worker) == 1)
                        .map(|_| (workspace.worker_last_used, id.clone()))
                })
                .collect::<Vec<_>>();
            idle.sort_unstable_by(|a, b| b.cmp(a));
            let retained = self.retained.read().unwrap();
            let mut kept = 0;
            idle.into_iter()
                .filter(|(_, id)| {
                    if kept < MAX_IDLE_WORKERS
                        && retained.as_ref().is_none_or(|ids| ids.contains(id))
                    {
                        kept += 1;
                        false
                    } else {
                        true
                    }
                })
                .filter_map(|(_, id)| workspaces.get_mut(&id).unwrap().worker.take())
                .collect::<Vec<_>>()
        };
        // Acquisition/removal share the workspace lock. No borrower can acquire
        // an evicted registry between the idle check and taking its sole Arc.
        for worker in retired {
            if let Ok(worker) = Arc::try_unwrap(worker) {
                let mut worker = worker.into_inner();
                let _ = worker.stop().await;
            }
        }
    }
}

/// Every native-use path keeps the cached registry pinned. Dropping any query or
/// repair future also makes maintenance eligible, including cancellation/errors.
pub(crate) struct WorkerUse {
    worker: Option<Arc<Mutex<Worker>>>,
    cache: Arc<WorkerCache>,
}
impl Deref for WorkerUse {
    type Target = Arc<Mutex<Worker>>;
    fn deref(&self) -> &Self::Target {
        self.worker.as_ref().expect("live native use lease")
    }
}
impl Drop for WorkerUse {
    fn drop(&mut self) {
        drop(self.worker.take());
        self.cache.schedule();
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn retained_maintenance_controller_does_not_keep_application_lock_or_target_reopened_state()
     {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let core = crate::Core::open(
            root.clone(),
            crate::worker::WorkerConfig::bundled("unused".into()),
        )
        .await
        .unwrap();
        let cache = core.worker_cache.clone();
        cache.schedule();
        drop(core);
        let reopened =
            crate::Core::open(root, crate::worker::WorkerConfig::bundled("unused".into()))
                .await
                .unwrap();
        let ws = reopened.create_workspace("user").await.unwrap();
        cache.trim().await;
        assert!(cache.workspaces.upgrade().is_none());
        assert!(reopened.list_workspaces().await.contains(&ws));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn hidden_workers_retire_after_the_last_busy_lease_without_deleting_projects() {
        use super::*;
        let temp = tempfile::tempdir().unwrap();
        let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let mut config =
            crate::worker::WorkerConfig::development(&repo).uncontained_for_trusted_tests();
        // A controlled interpreter proves process reaping without running a CAD build.
        config.args = vec![
            "-c".into(),
            "import time;data=bytearray(32*1024*1024);time.sleep(30)".into(),
        ];
        config.module_path = None;
        let core = crate::Core::open(temp.path().join("app"), config.clone())
            .await
            .unwrap();
        let visible = core.create_workspace("user").await.unwrap();
        let hidden = core.create_workspace("user").await.unwrap();
        let mut pids = Vec::new();
        for id in [&visible, &hidden] {
            let marker = temp.path().join(format!("{id}.pid"));
            let code = format!(
                "import os,time;from pathlib import Path;data=bytearray(32*1024*1024);Path({}).write_text(str(os.getpid()));time.sleep(30)",
                serde_json::to_string(&marker).unwrap()
            );
            config.args = vec!["-c".into(), code];
            let worker = crate::Worker::spawn(&config, temp.path().join(id))
                .await
                .unwrap();
            core.workspaces.lock().await.get_mut(id).unwrap().worker =
                Some(Arc::new(Mutex::new(worker)));
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !marker.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            pids.push(
                std::fs::read_to_string(marker)
                    .unwrap()
                    .parse::<i32>()
                    .unwrap(),
            );
        }
        let lease = {
            let mut all = core.workspaces.lock().await;
            core.worker_cache
                .acquire(all.get_mut(&hidden).unwrap())
                .unwrap()
        };
        core.retain_workspace_memory(vec![visible.clone()]);
        core.worker_cache.trim().await;
        assert!(
            core.workspaces.lock().await[&hidden].worker.is_some(),
            "busy operations survive hiding a tab"
        );
        assert!(core.workspaces.lock().await[&visible].worker.is_some());
        drop(lease);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if core.workspaces.lock().await[&hidden].worker.is_none() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            core.list_workspaces().await.contains(&hidden),
            "project state remains available for recovery"
        );
        core.retain_workspace_memory(vec![]);
        core.worker_cache.trim().await;
        assert!(core.workspaces.lock().await[&visible].worker.is_none());
        assert!(core.list_workspaces().await.contains(&visible));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for pid in pids {
                while unsafe { libc::kill(pid, 0) } == 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        })
        .await
        .expect("both memory-owning interpreters must actually be reaped");
    }
}
