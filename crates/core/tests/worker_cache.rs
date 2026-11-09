#![cfg(target_os = "macos")]
use cadpad_core::{Core, Job, JobState, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

const SOURCE: &str = r#"
import cadquery as cq
import json, os, sys
from pathlib import Path
cache=Path(sys.argv[sys.argv.index('--cache')+1])
with (cache/'native-pids.jsonl').open('a') as stream:
    stream.write(json.dumps(dict(worker=os.getpid(),guardian=os.getppid()))+'\n')
show(cq.Workplane().box(10,20,30),name='box')
"#;
const BLOCK: &str = r#"
import time
from cadpad_worker.model import Model
real_inspect=Model.inspect
def waiting_inspect(self,args):
    (cache/'query-entered').write_text('entered')
    while not (cache/'release-query').exists(): time.sleep(0.01)
    return real_inspect(self,args)
Model.inspect=waiting_inspect
"#;
fn config() -> WorkerConfig {
    // Explicit QA mode only: use the same assertions with the actual packaged
    // runtime. A missing/broken bundle must fail, never fall back to the repo.
    if let Some(resources) = std::env::var_os("CADPAD_TEST_PACKAGED_RESOURCES") {
        assert_eq!(std::env::var("PATH").as_deref(), Ok(""));
        return cadpad_core::runtime::PackagedRuntime::open(Path::new(&resources))
            .unwrap()
            .worker_config();
    }
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    WorkerConfig::development(&repo)
}
async fn terminal(core: &Core, ws: &str, job: Job) -> Job {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let current = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(current.state, JobState::Queued | JobState::Running) {
                return current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn model(core: &Core, source: &str) -> (String, String) {
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), source.into())]),
            "user",
        )
        .await
        .unwrap();
    let job = terminal(
        core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    assert_eq!(job.state, JobState::Succeeded, "{}", job.diagnostics);
    (ws, job.resulting_revision.unwrap())
}
fn cache(root: &Path, ws: &str, rev: &str) -> PathBuf {
    root.join("artifacts").join(ws).join(rev)
}
fn starts(root: &Path, ws: &str, rev: &str) -> Vec<Value> {
    std::fs::read_to_string(cache(root, ws, rev).join("native-pids.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn alive(pid: &Value) -> bool {
    if unsafe { libc::kill(pid.as_i64().unwrap() as i32, 0) } == 0 {
        return true;
    }
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    false
}
async fn gone(pids: &Value) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !alive(&pids["worker"]) && !alive(&pids["guardian"]) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn inspect(core: &Core, ws: &str, rev: &str) -> Value {
    core.query(
        ws,
        rev,
        "inspect",
        json!({"entities":[{"revision":rev,"instance_id":"box","entity_id":"part:0"}]}),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn idle_registry_eviction_is_lru_and_exact_recovery_preserves_source_motion_meshes_and_exports()
 {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let first = model(&core, SOURCE).await;
    let before = core.get_project(&first.0).await.unwrap();
    let exported = core
        .query(&first.0, &first.1, "export", json!({"format":"STEP"}))
        .await
        .unwrap();
    let artifact = core
        .artifact(&first.0, &first.1, exported["handle"].as_str().unwrap())
        .await
        .unwrap();
    let mut workspaces = vec![first];
    let mut original = vec![starts(&root, &workspaces[0].0, &workspaces[0].1)[0].clone()];
    for _ in 0..4 {
        let pair = model(&core, SOURCE).await;
        original.push(starts(&root, &pair.0, &pair.1)[0].clone());
        workspaces.push(pair);
    }
    gone(&original[0]).await;
    let (ws, rev) = &workspaces[0];
    // Context reads and downloading an already produced artifact do not execute
    // source, even when its native registry was removed from the warm cache.
    assert_eq!(
        core.get_project(ws).await.unwrap()["model"],
        before["model"]
    );
    assert_eq!(
        core.read_source(ws, rev, "model.py").await.unwrap()["source"],
        SOURCE
    );
    assert_eq!(
        core.artifact(ws, rev, exported["handle"].as_str().unwrap())
            .await
            .unwrap(),
        artifact
    );
    assert_eq!(starts(&root, ws, rev).len(), 1);
    // Touch the next-oldest worker, so recovering the evicted first workspace
    // must choose workspace 2 rather than workspace 1 as the next idle victim.
    assert_eq!(
        inspect(&core, &workspaces[1].0, &workspaces[1].1).await["entities"][0]["volume"],
        6000.0
    );
    assert_eq!(
        inspect(&core, ws, rev).await["entities"][0]["volume"],
        6000.0
    );
    let rebuilt = starts(&root, ws, rev);
    assert_eq!(rebuilt.len(), 2);
    assert_ne!(rebuilt[0]["worker"], rebuilt[1]["worker"]);
    gone(&original[2]).await;
    assert!(alive(&original[1]["worker"]));
    assert!(alive(&rebuilt[1]["worker"]));
    let after = core.get_project(ws).await.unwrap();
    for key in ["revision", "model", "parameters", "activity", "motion"] {
        assert_eq!(after[key], before[key], "{key}");
    }
}

#[tokio::test]
async fn an_entered_exact_query_is_pinned_during_pressure_and_becomes_evictable_after_completion() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let (ws, rev) = model(&core, &format!("{SOURCE}\n{BLOCK}")).await;
    let pids = starts(&root, &ws, &rev)[0].clone();
    let pending = {
        let core = core.clone();
        let ws = ws.clone();
        let rev = rev.clone();
        tokio::spawn(async move { inspect(&core, &ws, &rev).await })
    };
    let directory = cache(&root, &ws, &rev);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !directory.join("query-entered").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut oldest = None;
    for _ in 0..5 {
        let (other, revision) = model(&core, SOURCE).await;
        if oldest.is_none() {
            oldest = Some(starts(&root, &other, &revision)[0].clone());
        }
    }
    gone(&oldest.unwrap()).await;
    assert!(alive(&pids["worker"]));
    assert!(!pending.is_finished());
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], rev);
    std::fs::write(directory.join("release-query"), b"go").unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["entities"][0]["volume"], 6000.0);
    // Its last acquisition preceded the five new builds; after the use lease
    // drops it is the oldest idle registry and maintenance can reap it safely.
    gone(&pids).await;
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], rev);
}

const FD_RECORD: &str = r#"
open_fds=[]
for fd in range(3,4096):
    try:
        info=os.fstat(fd)
        open_fds.append([info.st_dev,info.st_ino])
    except OSError: pass
(cache/'open-fds.json').write_text(json.dumps(open_fds))
"#;
async fn entered(path: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
fn run_count(root: &Path) -> usize {
    std::fs::read_dir(root.join("runs"))
        .unwrap()
        .map(|ws| std::fs::read_dir(ws.unwrap().path()).unwrap().count())
        .sum()
}

#[tokio::test]
async fn native_admission_bounds_active_registries_and_preserves_warm_repair_and_cancellation() {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let cold = model(&core, SOURCE).await;
    let cold_pid = starts(&root, &cold.0, &cold.1)[0].clone();
    let mut pinned = Vec::new();
    for _ in 0..cadpad_core::native_admission::MAX_WORKERS {
        let (ws, rev) = model(&core, &format!("{SOURCE}\n{FD_RECORD}\n{BLOCK}")).await;
        let pids = starts(&root, &ws, &rev)[0].clone();
        let query = {
            let core = core.clone();
            let ws = ws.clone();
            let rev = rev.clone();
            tokio::spawn(async move { inspect(&core, &ws, &rev).await })
        };
        entered(&cache(&root, &ws, &rev).join("query-entered")).await;
        pinned.push((ws, rev, pids, query));
    }
    gone(&cold_pid).await;
    for (_, _, pids, _) in &pinned {
        assert!(alive(&pids["worker"]));
    }
    let capacity = std::fs::read_dir(root.join("native-capacity"))
        .unwrap()
        .map(|entry| {
            let metadata = entry.unwrap().metadata().unwrap();
            // Widen both sides: Python may emit signed device ids or large inodes.
            (metadata.dev() as i128, metadata.ino() as i128)
        })
        .collect::<Vec<_>>();
    assert_eq!(capacity.len(), cadpad_core::native_admission::MAX_WORKERS);
    fn json_id(value: &Value) -> i128 {
        value
            .as_i64()
            .map(i128::from)
            .or_else(|| value.as_u64().map(i128::from))
            .unwrap_or_else(|| panic!("expected integer identity {value}"))
    }
    for (ws, rev, _, _) in &pinned {
        let descriptors: Value =
            serde_json::from_slice(&std::fs::read(cache(&root, ws, rev).join("open-fds.json")).unwrap())
                .unwrap();
        let descriptors = descriptors
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                let pair = entry.as_array().unwrap();
                (json_id(&pair[0]), json_id(&pair[1]))
            })
            .collect::<Vec<_>>();
        assert!(
            capacity.iter().all(|entry| !descriptors.contains(entry)),
            "Generated model code must not inherit the capacity lease"
        );
    }
    let runs = run_count(&root);
    let cold_query = {
        let core = core.clone();
        let ws = cold.0.clone();
        let rev = cold.1.clone();
        tokio::spawn(async move {
            core.query_as_actor(
                "cold-waiter",
                &ws,
                &rev,
                "inspect",
                json!({"entities":[{"revision":rev,"instance_id":"box","entity_id":"part:0"}]}),
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!cold_query.is_finished());
    assert_eq!(run_count(&root), runs);
    assert_eq!(core.get_project(&cold.0).await.unwrap()["revision"], cold.1);
    core.revoke_actor("cold-waiter").await;
    let error = tokio::time::timeout(Duration::from_secs(2), cold_query)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), "CANCELLED");
    assert_eq!(run_count(&root), runs);
    let mut queued = Vec::new();
    for _ in 0..2 {
        let ws = core.create_workspace("queued").await.unwrap();
        let rev = core.get_project(&ws).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let draft = core
            .update_source(
                &ws,
                &rev,
                None,
                BTreeMap::from([("model.py".into(), SOURCE.into())]),
                "queued",
            )
            .await
            .unwrap();
        let job = core.build(&ws, &rev, &draft.id, "queued").await.unwrap();
        queued.push((ws, job));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    for (ws, job) in &queued {
        assert!(matches!(
            core.get_job(ws, &job.id).await.unwrap().state,
            JobState::Queued | JobState::Running
        ));
        assert!(!root.join("runs").join(ws).join(&job.id).exists());
    }
    assert!(run_count(&root) <= runs);
    let (cancelled_ws, cancelled_job) = queued.pop().unwrap();
    core.cancel_job(&cancelled_ws, &cancelled_job.id)
        .await
        .unwrap();
    assert_eq!(
        terminal(&core, &cancelled_ws, cancelled_job).await.state,
        JobState::Cancelled
    );
    assert!(run_count(&root) <= runs);
    // Warm repair needs construction capacity while retaining its live registry.
    // It must not deadlock behind source jobs waiting for native admission.
    let (ws, rev, _, _) = &pinned[0];
    let source_before = core.read_source(ws, rev, "model.py").await.unwrap();
    let project = core.get_project(ws).await.unwrap();
    let handle = project["model"]["parts"][0]["mesh"]["buffers"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()["handle"]
        .as_str()
        .unwrap();
    std::fs::remove_file(cache(&root, ws, rev).join(handle)).unwrap();
    let repair = core
        .repair_display(
            "warm-repair",
            cadpad_core::recovery::RepairDisplay {
                workspace_id: ws.clone(),
                revision: rev.clone(),
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while core.get_job(ws, &repair.id).await.unwrap().state == JobState::Queued {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    std::fs::write(cache(&root, ws, rev).join("release-query"), b"go").unwrap();
    let repaired = terminal(&core, ws, repair).await;
    assert_eq!(
        repaired.state,
        JobState::Succeeded,
        "{}",
        repaired.diagnostics
    );
    for (ws, job) in queued {
        core.cancel_job(&ws, &job.id).await.unwrap();
        assert!(matches!(
            terminal(&core, &ws, job).await.state,
            JobState::Cancelled | JobState::Succeeded
        ));
    }
    for (ws, rev, _, _) in &pinned {
        std::fs::write(cache(&root, ws, rev).join("release-query"), b"go").unwrap();
    }
    let (first_ws, first_rev) = (pinned[0].0.clone(), pinned[0].1.clone());
    for (_, _, _, query) in pinned {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), query)
                .await
                .unwrap()
                .unwrap()["entities"][0]["volume"],
            6000.0
        );
    }
    assert_eq!(
        core.read_source(&first_ws, &first_rev, "model.py")
            .await
            .unwrap(),
        source_before
    );
    assert_eq!(
        core.get_project(&first_ws).await.unwrap()["revision"],
        first_rev
    );
}

#[tokio::test]
#[ignore = "Subprocess host for native admission guardian-lifetime test"]
async fn native_admission_owner_helper() {
    let root = PathBuf::from(std::env::var_os("CADPAD_ADMISSION_HOST_ROOT").unwrap());
    let core = Core::open(root.clone(), config()).await.unwrap();
    let (ws, rev) = model(&core, &format!("{SOURCE}\n{FD_RECORD}")).await;
    let pids = starts(&root, &ws, &rev)[0].clone();
    std::fs::write(
        root.join("owner-marker.json"),
        serde_json::to_vec(&json!({"workspace":ws,"revision":rev,"pids":pids})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        unsafe { libc::kill(pids["guardian"].as_i64().unwrap() as i32, libc::SIGSTOP) },
        0
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(root.join("guardian-stopped"), b"ready").unwrap();
    std::future::pending::<()>().await;
}

#[tokio::test]
async fn native_admission_lease_releases_after_owner_sigkill_cleans_up_a_stopped_guardian() {
    use std::process::Stdio;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "native_admission_owner_helper",
            "--nocapture",
        ])
        .env("CADPAD_ADMISSION_HOST_ROOT", &root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    entered(&root.join("owner-marker.json")).await;
    let marker: Value =
        serde_json::from_slice(&std::fs::read(root.join("owner-marker.json")).unwrap()).unwrap();
    let pids = &marker["pids"];
    entered(&root.join("guardian-stopped")).await;
    let leases = std::fs::read_dir(root.join("native-capacity"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(leases.len(), 1);
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&leases[0])
        .unwrap();
    assert!(matches!(
        lease.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    // A stopped guardian must not outlive app loss: the native anchor reaps it
    // and releases the capacity lease. Resume only if a race left it stopped.
    gone(pids).await;
    if alive(&pids["guardian"]) {
        let _ = unsafe {
            libc::kill(pids["guardian"].as_i64().unwrap() as i32, libc::SIGCONT)
        };
        gone(pids).await;
    }
    assert!(
        matches!(lease.try_lock(), Ok(())),
        "App-loss cleanup must release the capacity lease once the guardian is gone"
    );
    let reopened = Core::open(root.clone(), config()).await.unwrap();
    assert_eq!(
        reopened
            .get_project(marker["workspace"].as_str().unwrap())
            .await
            .unwrap()["revision"],
        marker["revision"]
    );
    drop(reopened);
}
