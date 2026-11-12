#![cfg(target_os = "macos")]
use cadpad_core::worker::{Worker, WorkerConfig};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

fn config() -> WorkerConfig {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut config = WorkerConfig::development(&repository)
        .guarded_by(env!("CARGO_BIN_EXE_cadpad-worker-guard").into());
    config.args=vec!["-c".into(),r#"
import ctypes,errno,json,os,signal,stat,sys
from pathlib import Path
cache=Path(sys.argv[sys.argv.index('--cache')+1])
sockets=[]
for fd in range(3,512):
    try:
        if stat.S_ISSOCK(os.fstat(fd).st_mode): sockets.append(fd)
    except OSError: pass
try:
    os.kill(os.getppid(),signal.SIGCONT)
    parent_signal_denied=False
except PermissionError:
    parent_signal_denied=True
session_change_denied=False
try: os.setsid()
except PermissionError: session_change_denied=True
group_change_denied=False
try: os.setpgid(0,0)
except PermissionError: group_change_denied=True
lib=ctypes.CDLL('/usr/lib/libproc.dylib')
lib.proc_listchildpids.argtypes=[ctypes.c_int,ctypes.c_void_p,ctypes.c_int]
children=(ctypes.c_int*8)()
child_count=lib.proc_listchildpids(os.getpid(),children,ctypes.sizeof(children))
anchor_group_denied=False
anchor_signal_denied=False
if child_count==1:
    try: os.setpgid(children[0],children[0])
    except OSError as error: anchor_group_denied=error.errno==errno.EACCES
    try: os.kill(children[0],signal.SIGCONT)
    except PermissionError: anchor_signal_denied=True
(cache/'starting').write_text(json.dumps(dict(worker=os.getpid(),guardian=os.getppid(),sockets=sockets,session=os.getsid(0),parent_signal_denied=parent_signal_denied,session_change_denied=session_change_denied,group_change_denied=group_change_denied,anchor_group_denied=anchor_group_denied,anchor_signal_denied=anchor_signal_denied)))
os.replace(cache/'starting',cache/'pids.json')
while True: pass
"#.into()];
    config
}
async fn pids(cache: &Path) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(bytes) = std::fs::read(cache.join("pids.json")) {
                let mut value: Value = serde_json::from_slice(&bytes).unwrap();
                let worker = value["worker"].as_i64().unwrap() as i32;
                let mut children = [0i32; 8];
                let count = unsafe {
                    libc::proc_listchildpids(
                        worker,
                        children.as_mut_ptr().cast(),
                        std::mem::size_of_val(&children) as i32,
                    )
                };
                assert_eq!(
                    count, 1,
                    "one already-exec'd native anchor must be armed before Python starts"
                );
                value["anchor"] = serde_json::json!(children[0]);
                break value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn gone(pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if unsafe { libc::kill(pid, 0) } < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn lease_released(cache: &Path) {
    let marker = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(cache.join(".lease"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match marker.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn missing_guardian_fails_closed_before_starting_model_code() {
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let config = config().guarded_by(temp.path().join("missing-guardian"));
    let error = Worker::spawn(&config, cache.clone()).await.err().unwrap();
    assert_eq!(error.code(), "WORKER_UNAVAILABLE");
    assert!(!cache.join("pids.json").exists());
}

#[tokio::test]
async fn explicit_stop_reaps_guardian_and_busy_contained_interpreter_without_socket_leaks() {
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let mut worker = Worker::spawn(&config(), cache.clone()).await.unwrap();
    let pids = pids(&cache).await;
    // CPython may keep private runtime socketpairs (currently fds 3 and 4). The
    // guardian must not leak additional private lease/liveness sockets.
    let sockets = pids["sockets"].as_array().unwrap();
    assert!(
        sockets
            .iter()
            .all(|value| matches!(value.as_u64(), Some(3 | 4))),
        "unexpected inherited sockets: {pids}"
    );
    assert_eq!(pids["parent_signal_denied"], true);
    for key in [
        "session_change_denied",
        "group_change_denied",
        "anchor_group_denied",
        "anchor_signal_denied",
    ] {
        assert_eq!(pids[key], true, "{key}");
    }
    assert_eq!(pids["session"], pids["worker"]);
    tokio::time::timeout(Duration::from_secs(5), worker.stop())
        .await
        .unwrap()
        .unwrap();
    gone(pids["worker"].as_i64().unwrap() as i32).await;
    gone(pids["guardian"].as_i64().unwrap() as i32).await;
    gone(pids["anchor"].as_i64().unwrap() as i32).await;
}

#[tokio::test]
async fn dropping_worker_terminates_and_reaps_a_stopped_interpreter() {
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let worker = Worker::spawn(&config(), cache.clone()).await.unwrap();
    let ids = pids(&cache).await;
    assert_eq!(
        unsafe { libc::kill(ids["worker"].as_i64().unwrap() as i32, libc::SIGSTOP) },
        0
    );
    drop(worker);
    for key in ["worker", "guardian", "anchor"] {
        gone(ids[key].as_i64().unwrap() as i32).await;
    }
    lease_released(&cache).await;
}

#[tokio::test]
async fn guardian_sigkill_terminates_busy_and_stopped_interpreters_and_their_anchors() {
    for stop in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let worker = Worker::spawn(&config(), cache.clone()).await.unwrap();
        let ids = pids(&cache).await;
        if stop {
            assert_eq!(
                unsafe { libc::kill(ids["worker"].as_i64().unwrap() as i32, libc::SIGSTOP) },
                0
            );
        }
        assert_eq!(
            unsafe { libc::kill(ids["guardian"].as_i64().unwrap() as i32, libc::SIGKILL) },
            0
        );
        drop(worker);
        for key in ["worker", "guardian", "anchor"] {
            gone(ids[key].as_i64().unwrap() as i32).await;
        }
        lease_released(&cache).await;
    }
}

#[tokio::test]
async fn simultaneous_guardian_and_anchor_sigkill_still_reaps_the_interpreter_and_releases_leases()
{
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let worker = Worker::spawn(&config(), cache.clone()).await.unwrap();
    let ids = pids(&cache).await;
    assert_eq!(
        unsafe { libc::kill(ids["guardian"].as_i64().unwrap() as i32, libc::SIGKILL) },
        0
    );
    assert_eq!(
        unsafe { libc::kill(ids["anchor"].as_i64().unwrap() as i32, libc::SIGKILL) },
        0
    );
    drop(worker);
    for key in ["worker", "guardian", "anchor"] {
        gone(ids[key].as_i64().unwrap() as i32).await;
    }
    lease_released(&cache).await;
}

#[tokio::test]
async fn stop_resumes_a_stopped_guardian_and_terminates_the_stopped_native_group() {
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let mut worker = Worker::spawn(&config(), cache.clone()).await.unwrap();
    let ids = pids(&cache).await;
    for key in ["worker", "guardian"] {
        assert_eq!(
            unsafe { libc::kill(ids[key].as_i64().unwrap() as i32, libc::SIGSTOP) },
            0
        );
    }
    tokio::time::timeout(Duration::from_secs(4), worker.stop())
        .await
        .unwrap()
        .unwrap();
    for key in ["worker", "guardian", "anchor"] {
        gone(ids[key].as_i64().unwrap() as i32).await;
    }
    drop(worker);
    lease_released(&cache).await;
}

#[tokio::test]
async fn an_unresponsive_supervisor_front_is_killed_after_the_shutdown_grace_period() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let fixture = temp.path().join("unresponsive-guardian");
    let python = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
        .join(".venv/bin/python");
    // This trusted test front deliberately ignores liveness. Its real native
    // child and interpreter retain the production guardian/containment behavior.
    let text = format!(
        "#!{}\nimport os,subprocess,sys,time\nfrom pathlib import Path\nPath({}).write_text(str(os.getpid()))\nsubprocess.Popen([{},*sys.argv[1:]],pass_fds=(int(sys.argv[2]),))\nwhile True: time.sleep(1)\n",
        python.display(),
        serde_json::json!(temp.path().join("front.pid")),
        serde_json::json!(env!("CARGO_BIN_EXE_cadpad-worker-guard"))
    );
    std::fs::write(&fixture, text).unwrap();
    std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut worker = Worker::spawn(&config().guarded_by(fixture), cache.clone())
        .await
        .unwrap();
    let ids = pids(&cache).await;
    let front: i32 = std::fs::read_to_string(temp.path().join("front.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(4), worker.stop())
        .await
        .unwrap()
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "the fixture must exercise forced shutdown, not normal guardian exit"
    );
    gone(front).await;
    for key in ["worker", "guardian", "anchor"] {
        gone(ids[key].as_i64().unwrap() as i32).await;
    }
}

#[tokio::test]
async fn a_candidate_guardian_crash_preserves_committed_geometry_and_job_diagnostics() {
    use cadpad_core::{Core, Job, JobState};
    use std::collections::BTreeMap;
    async fn terminal(core: &Core, ws: &str, job: Job) -> Job {
        tokio::time::timeout(Duration::from_secs(20), async {
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
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let core = Core::open(
        root.clone(),
        WorkerConfig::development(&repository)
            .guarded_by(env!("CARGO_BIN_EXE_cadpad-worker-guard").into()),
    )
    .await
    .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = "import cadquery as cq\nshow(cq.Workplane().box(10,20,30),name='box')\n";
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
    let built = terminal(
        &core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    assert_eq!(built.state, JobState::Succeeded);
    let revision = built.resulting_revision.unwrap();
    let bad = format!(
        "{source}\nimport json,os\nfrom pathlib import Path\np=Path(os.environ['TMPDIR']).parent/'fault.json'\np.write_text(json.dumps(dict(worker=os.getpid(),guardian=os.getppid())))\nwhile True: pass\n"
    );
    let draft = core
        .update_source(
            &ws,
            &revision,
            None,
            BTreeMap::from([("model.py".into(), bad)]),
            "user",
        )
        .await
        .unwrap();
    let job = core.build(&ws, &revision, &draft.id, "user").await.unwrap();
    let ids: Value = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            for entry in std::fs::read_dir(root.join("artifacts").join(&ws)).unwrap() {
                let path = entry.unwrap().path().join("fault.json");
                if let Ok(bytes) = std::fs::read(path)
                    && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                {
                    return value;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut anchor = [0i32; 1];
    assert_eq!(
        unsafe {
            libc::proc_listchildpids(
                ids["worker"].as_i64().unwrap() as i32,
                anchor.as_mut_ptr().cast(),
                std::mem::size_of_val(&anchor) as i32,
            )
        },
        1
    );
    assert_eq!(
        unsafe { libc::kill(ids["guardian"].as_i64().unwrap() as i32, libc::SIGKILL) },
        0
    );
    let failed = terminal(&core, &ws, job).await;
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.diagnostics["code"], "WORKER_UNAVAILABLE");
    gone(ids["worker"].as_i64().unwrap() as i32).await;
    gone(ids["guardian"].as_i64().unwrap() as i32).await;
    gone(anchor[0]).await;
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
    let inspected=core.query(&ws,&revision,"inspect",serde_json::json!({"entities":[{"revision":revision,"instance_id":"box","entity_id":"part:0"}]})).await.unwrap();
    assert_eq!(inspected["entities"][0]["volume"], 6000.);
}

#[tokio::test]
async fn app_sigkill_cleans_up_after_a_stopped_supervisor() {
    let temp = tempfile::tempdir().unwrap();
    let mut parent = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "parent_death_helper"])
        .env("CADPAD_WORKER_LIFECYCLE_TEST", temp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()
        .unwrap();
    let pids = pids(&temp.path().join("cache")).await;
    let guardian = pids["guardian"].as_i64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(guardian, libc::SIGSTOP) }, 0);
    // App loss while the primary supervisor is stopped: the independent anchor
    // must still observe the app socket closing and terminate the native group
    // plus the stopped supervisor.
    assert_eq!(
        unsafe { libc::kill(-(parent.id().unwrap() as i32), libc::SIGKILL) },
        0
    );
    parent.wait().await.unwrap();
    gone(pids["worker"].as_i64().unwrap() as i32).await;
    gone(guardian).await;
    gone(pids["anchor"].as_i64().unwrap() as i32).await;
}

#[tokio::test]
#[ignore = "subprocess helper for abrupt parent-death test"]
async fn parent_death_helper() {
    let Some(root) = std::env::var_os("CADPAD_WORKER_LIFECYCLE_TEST") else {
        return;
    };
    let cache = PathBuf::from(root).join("cache");
    let _worker = Worker::spawn(&config(), cache.clone()).await.unwrap();
    pids(&cache).await;
    tokio::time::sleep(Duration::from_secs(60)).await;
}

#[tokio::test]
async fn app_sigkill_terminates_a_stopped_contained_worker_without_cooperative_python() {
    let temp = tempfile::tempdir().unwrap();
    let mut parent = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "parent_death_helper"])
        .env("CADPAD_WORKER_LIFECYCLE_TEST", temp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()
        .unwrap();
    let pids = pids(&temp.path().join("cache")).await;
    let pid = pids["worker"].as_i64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    // Kill the whole app group, as terminal/job-control termination may do.
    // The guardian is in a separate native group and must survive to reap CAD.
    assert_eq!(
        unsafe { libc::kill(-(parent.id().unwrap() as i32), libc::SIGKILL) },
        0
    );
    parent.wait().await.unwrap();
    gone(pid).await;
    gone(pids["guardian"].as_i64().unwrap() as i32).await;
    gone(pids["anchor"].as_i64().unwrap() as i32).await;
}
