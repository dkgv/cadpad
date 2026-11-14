#![cfg(target_os = "macos")]
use cadpad_core::{
    Core, Job, JobState,
    tools::ToolContext,
    worker::{Worker, WorkerConfig},
    worker_limits::Limits,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::sync::watch;

fn config() -> WorkerConfig {
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
}
fn scripted(code: &str, limits: Limits) -> WorkerConfig {
    let mut config = config();
    config.args = vec!["-c".into(), code.into()];
    config.limits = limits;
    config
}
const PID: &str = "import os,sys,time\nfrom pathlib import Path\ncache=Path(sys.argv[sys.argv.index('--cache')+1])\n(cache/'pid').write_text(str(os.getpid()))\n";
fn reaped(cache: &Path) {
    let pid: i32 = std::fs::read_to_string(cache.join("pid"))
        .unwrap()
        .parse()
        .unwrap();
    reaped_pid(pid);
}
fn reaped_pid(pid: i32) {
    // If the anchor terminates the supervisor, the OS may briefly retain the
    // orphaned interpreter as a zombie before reaping it.
    let deadline = Instant::now() + Duration::from_secs(5);
    while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "worker {pid} still exists"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
#[tokio::test]
async fn native_guard_limits_cpu_and_touched_memory_and_reaps_the_owned_process() {
    let again = format!(
        "{PID}\np=cache/'exec-count'\np.write_text(str(int(p.read_text())+1 if p.exists() else 1))\nwhile time.process_time()<0.02: pass\nos.execv(sys.executable,[sys.executable,str(cache/'again.py'),'--cache',str(cache)])"
    );
    let exec = format!(
        "{PID}\n(cache/'again.py').write_text({})\nos.execv(sys.executable,[sys.executable,str(cache/'again.py'),'--cache',str(cache)])",
        json!(again)
    );
    for (code, resource, limit) in [
        (format!("{PID}\nwhile True: pass"), "cpu_time_ms", 200),
        (exec, "cpu_time_ms", 200),
        (
            format!(
                "{PID}\nblocks=[]\nfor _ in range(32):\n b=bytearray(b'x'*(4*1024*1024));blocks.append(b);time.sleep(0.025)\ntime.sleep(60)"
            ),
            "memory_bytes",
            48 * 1024 * 1024,
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let mut limits = Limits {
            operation_timeout_ms: 8000,
            cpu_time_ms: 5000,
            memory_bytes: 256 * 1024 * 1024,
        };
        if resource == "cpu_time_ms" {
            limits.cpu_time_ms = limit;
        } else {
            limits.memory_bytes = limit;
        }
        let mut worker = Worker::spawn(&scripted(&code, limits), cache.clone())
            .await
            .unwrap();
        let (_send, cancel) = watch::channel(false);
        let started = Instant::now();
        let error = tokio::time::timeout(
            Duration::from_secs(12),
            worker.request("revision", "build", json!({}), cancel),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.code(), "RESOURCE_LIMIT");
        let details = cadpad_core::error_value(&error)["details"].clone();
        assert_eq!(details["kind"], "limit");
        assert_eq!(details["resource"], resource);
        assert_eq!(details["limit"], limit);
        assert!(details["observed"].as_u64().unwrap() >= limit);
        reaped(&cache);
        let exec_count = if cache.join("exec-count").is_file() {
            std::fs::read_to_string(cache.join("exec-count"))
                .unwrap()
                .parse::<u64>()
                .unwrap()
        } else {
            0
        };
        if code.contains("os.execv") {
            assert!(
                exec_count >= 2,
                "self-exec probe must run the interpreter repeatedly"
            );
        }
        eprintln!(
            "RESOURCE_PROBE {}",
            json!({"probe":resource,"report":details,"elapsed_ms":started.elapsed().as_millis(),"self_exec_count":exec_count,"reaped":true})
        );
        worker.stop().await.unwrap();
    }
}
#[tokio::test]
async fn operation_timeout_includes_pipe_wait_and_closes_without_a_python_reply() {
    for params in [json!({}), json!({"padding":"x".repeat(1024*1024)})] {
        let blocked_write = params.get("padding").is_some();
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let mut worker = Worker::spawn(
            &scripted(
                &format!("{PID}\ntime.sleep(60)"),
                Limits {
                    operation_timeout_ms: 750,
                    ..Default::default()
                },
            ),
            cache.clone(),
        )
        .await
        .unwrap();
        let (_send, cancel) = watch::channel(false);
        let start = Instant::now();
        let error = worker
            .request("revision", "build", params, cancel)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "RESOURCE_LIMIT");
        assert_eq!(
            cadpad_core::error_value(&error)["details"]["resource"],
            "operation_time_ms"
        );
        assert!(start.elapsed() < Duration::from_secs(6));
        reaped(&cache);
        eprintln!(
            "RESOURCE_PROBE {}",
            json!({"probe":"operation_timeout","blocked_write":blocked_write,"elapsed_ms":start.elapsed().as_millis(),"limit_ms":750,"reaped":true})
        );
    }
}
#[tokio::test]
async fn idle_cpu_is_still_guarded_and_the_private_report_socket_is_not_inherited() {
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let code = format!(
        "{PID}\nimport json,stat\nsockets=[]\nfor fd in range(3,512):\n try:\n  if stat.S_ISSOCK(os.fstat(fd).st_mode): sockets.append(fd)\n except OSError: pass\n(cache/'sockets').write_text(json.dumps(sockets))\nr=json.loads(sys.stdin.readline());r.pop('method');r.pop('params');r.update(result={{}},logs='',logs_truncated=False);print(json.dumps(r),flush=True)\nwhile True: pass"
    );
    let mut worker = Worker::spawn(
        &scripted(
            &code,
            Limits {
                cpu_time_ms: 300,
                operation_timeout_ms: 5000,
                ..Default::default()
            },
        ),
        cache.clone(),
    )
    .await
    .unwrap();
    let (_send, cancel) = watch::channel(false);
    worker
        .request("revision", "build", json!({}), cancel.clone())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(cache.join("sockets")).unwrap(),
        "[]"
    );
    let pid: i32 = std::fs::read_to_string(cache.join("pid"))
        .unwrap()
        .parse()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    // The next request observes the authentic guardian report even if writing
    // to the already-dead interpreter fails before trying to read its stdout.
    let error = worker
        .request("revision", "inspect", json!({}), cancel)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "RESOURCE_LIMIT");
    assert_eq!(
        cadpad_core::error_value(&error)["details"]["resource"],
        "cpu_time_ms"
    );
    reaped(&cache);
    eprintln!(
        "RESOURCE_PROBE {}",
        json!({"probe":"idle_cpu","report":cadpad_core::error_value(&error)["details"],"inherited_socket_fds":0,"reaped":true})
    );
}
async fn terminal(core: &Core, ws: &str, job: Job) -> Job {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let job = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn timed_out_candidate_preserves_the_committed_model_and_diagnostic_contract() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = config();
    // This budget includes a cold CAD import on the CI runner. The bad source
    // below sleeps for 60 seconds, so timeout enforcement remains observable.
    const OPERATION_TIMEOUT_MS: u64 = 15_000;
    config.limits.operation_timeout_ms = OPERATION_TIMEOUT_MS;
    let core = Core::open(temp.path().into(), config).await.unwrap();
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
            BTreeMap::from([(
                "model.py".into(),
                "import cadquery as cq\nshow(cq.Workplane().box(10,20,30),name='box')".into(),
            )]),
            "user",
        )
        .await
        .unwrap();
    let good = terminal(
        &core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    assert_eq!(good.state, JobState::Succeeded, "{}", good.diagnostics);
    let revision = good.resulting_revision.unwrap();
    let bad = core
        .update_source(
            &ws,
            &revision,
            None,
            BTreeMap::from([("model.py".into(), "import time\ntime.sleep(60)".into())]),
            "user",
        )
        .await
        .unwrap();
    let failed = terminal(
        &core,
        &ws,
        core.build(&ws, &revision, &bad.id, "user").await.unwrap(),
    )
    .await;
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.diagnostics["code"], "RESOURCE_LIMIT");
    assert_eq!(
        failed.diagnostics["details"]["resource"],
        "operation_time_ms"
    );
    assert_eq!(failed.diagnostics["details"]["limit"], OPERATION_TIMEOUT_MS);
    let project = core
        .dispatch(
            &ToolContext {
                actor: "user".into(),
            },
            "get_project",
            json!({"workspace_id":ws}),
        )
        .await
        .unwrap();
    assert_eq!(project["revision"], revision);
    assert_eq!(project["model"]["parts"][0]["volume"], 6000.);
    let schema = cadpad_core::tools::definition("get_project")
        .unwrap()
        .output_schema
        .as_ref()
        .unwrap();
    assert!(
        jsonschema::validator_for(schema)
            .unwrap()
            .is_valid(&project)
    );
    let exact: Value = core
        .query(
            &ws,
            &revision,
            "inspect",
            json!({"entities":[{"revision":revision,"instance_id":"box","entity_id":"part:0"}]}),
        )
        .await
        .unwrap();
    assert_eq!(exact["entities"][0]["volume"], 6000.);
}

#[tokio::test]
async fn quota_failed_exact_workers_are_reaped_and_recovered_without_retargeting_the_revision() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = config();
    // Recovery must allow a cold native import. Trigger the intended quota via
    // touched memory instead of making healthy builds race a short wall timer.
    const MEMORY_LIMIT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    config.limits.operation_timeout_ms = 30_000;
    config.limits.memory_bytes = MEMORY_LIMIT_BYTES;
    let core = Core::open(temp.path().into(), config).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = r#"
import os, time
from pathlib import Path
import cadquery as cq
from cadpad_worker.model import Model
p = Path(os.environ['TMPDIR']) / 'native-pids'
with p.open('a') as f:
    f.write(str(os.getpid()) + '\n')
def exhaust_memory(self, args):
    blocks = []
    for _ in range(256):
        blocks.append(bytearray(b'x' * (8 * 1024 * 1024)))
        time.sleep(0.01)
    while True:
        time.sleep(1)
Model.inspect = exhaust_memory
show(cq.Workplane().box(10,20,30), name='box')
"#;
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
    let good = terminal(
        &core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    assert_eq!(good.state, JobState::Succeeded, "{}", good.diagnostics);
    let revision = good.resulting_revision.unwrap();
    for _ in 0..2 {
        let error = core
            .query(&ws, &revision, "inspect", json!({"entities":[]}))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "RESOURCE_LIMIT");
        let details = cadpad_core::error_value(&error)["details"].clone();
        assert_eq!(details["resource"], "memory_bytes");
        assert_eq!(details["limit"], MEMORY_LIMIT_BYTES);
        assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
    }
    let pids = std::fs::read_to_string(
        temp.path()
            .join("artifacts")
            .join(&ws)
            .join(&revision)
            .join(".sandbox-tmp/native-pids"),
    )
    .unwrap();
    let pids: Vec<i32> = pids.lines().map(|line| line.parse().unwrap()).collect();
    assert_eq!(
        pids.len(),
        2,
        "the second query must rebuild a fresh native worker"
    );
    assert_ne!(pids[0], pids[1]);
    for pid in pids {
        reaped_pid(pid);
    }
    eprintln!(
        "RESOURCE_PROBE {}",
        json!({"probe":"exact_query_recovery","distinct_workers":2,"revision_preserved":true,"reaped":true})
    );
}
