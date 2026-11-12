#![cfg(target_os = "macos")]
use cadpad_core::{Core, JobState, worker::WorkerConfig};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
fn development() -> WorkerConfig {
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
}
async fn terminal(core: &Core, ws: &str, id: &str) -> cadpad_core::Job {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let job = core.get_job(ws, id).await.unwrap();
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
async fn exercise(config: WorkerConfig) {
    let temp = tempfile::Builder::new()
        .prefix("cadpad-\") (allow network*) (")
        .tempdir()
        .unwrap();
    let root = temp.path().join("app");
    let secret = temp.path().join("outside-secret");
    std::fs::write(&secret, "host-canary").unwrap();
    let output = temp.path().join("outside-write");
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    tcp.set_nonblocking(true).unwrap();
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.set_nonblocking(true).unwrap();
    let socket = temp.path().join("private.sock");
    let unix = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    unix.set_nonblocking(true).unwrap();
    let script = format!(
        r#"
import errno, os, socket, resource, subprocess, tempfile
from pathlib import Path
import cadquery as cq
from helper import width

def denied(action):
    try: action()
    except OSError as error:
        assert error.errno in (errno.EPERM,errno.EACCES,errno.EROFS), repr(error)
    else: raise RuntimeError('Containment probe unexpectedly succeeded')

outside=Path({secret}); target=Path({output})
denied(lambda:outside.read_text())
denied(lambda:target.write_text('escaped'))
denied(lambda:Path(__file__).write_text('mutated source'))
denied(lambda:Path(cq.__file__).with_name('cadpad-write-probe').write_text('mutated runtime'))
scratch=Path(tempfile.mkdtemp()); (scratch/'allowed').write_text('ok')
assert (scratch/'allowed').read_text()=='ok'
(scratch/'symlink').symlink_to(outside)
denied(lambda:(scratch/'symlink').read_text())
(scratch/'symlink').unlink()
denied(lambda:os.link(outside,scratch/'hardlink'))

def connect(kind,address):
    with socket.socket(kind,socket.SOCK_STREAM) as connection:
        connection.settimeout(1);connection.connect(address)
denied(lambda:connect(socket.AF_INET,('127.0.0.1',{tcp})))
denied(lambda:connect(socket.AF_UNIX,{unix}))
def send():
    with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as connection:connection.sendto(b'escape',('127.0.0.1',{udp}))
denied(send)
denied(lambda:subprocess.run(['/bin/echo','escaped'],check=True))
def fork():
    pid=os.fork()
    if pid==0:os._exit(0)
    os.waitpid(pid,0)
denied(fork)
assert resource.getrlimit(resource.RLIMIT_CORE)==(0,0)
assert resource.getrlimit(resource.RLIMIT_NOFILE)[1]<=512
assert resource.getrlimit(resource.RLIMIT_FSIZE)[1]<=1024**3
assert 'ANTHROPIC_API_KEY' not in os.environ
print('containment-probes-passed')
show(cq.Workplane().box(width,20,6).faces('>Z').workplane().hole(4),name='plate')
"#,
        secret = serde_json::to_string(secret.to_str().unwrap()).unwrap(),
        output = serde_json::to_string(output.to_str().unwrap()).unwrap(),
        tcp = tcp.local_addr().unwrap().port(),
        udp = udp.local_addr().unwrap().port(),
        unix = serde_json::to_string(socket.to_str().unwrap()).unwrap()
    );
    let core = Core::open(root.clone(), config).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let revision = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let draft = core
        .update_source(
            &ws,
            &revision,
            None,
            BTreeMap::from([
                ("model.py".into(), script),
                ("helper.py".into(), "width=30".into()),
            ]),
            "user",
        )
        .await
        .unwrap();
    let job = core.build(&ws, &revision, &draft.id, "user").await.unwrap();
    let job = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let job = core.get_job(&ws, &job.id).await.unwrap();
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    assert!(
        job.diagnostics["logs"]
            .as_str()
            .unwrap()
            .contains("containment-probes-passed")
    );
    let revision = job.resulting_revision.unwrap();
    let inspected = core
        .query(
            &ws,
            &revision,
            "inspect",
            json!({"entities":[{"revision":revision,"instance_id":"plate","entity_id":"part:0"}]}),
        )
        .await
        .unwrap();
    assert!(
        (inspected["entities"][0]["volume"].as_f64().unwrap()
            - (30. * 20. * 6. - std::f64::consts::PI * 4. * 6.))
            .abs()
            < 1e-8
    );
    let exported = core
        .query(&ws, &revision, "export", json!({"format":"STEP"}))
        .await
        .unwrap();
    let bytes = core
        .artifact(&ws, &revision, exported["handle"].as_str().unwrap())
        .await
        .unwrap();
    assert!(bytes.starts_with(b"ISO-10303-21;"));
    // An uncaught denial is a normal failed transaction, not a replacement of
    // the committed model or a silent retry without containment.
    let forbidden = format!(
        "from pathlib import Path\nPath({}).read_text()",
        serde_json::to_string(secret.to_str().unwrap()).unwrap()
    );
    let draft = core
        .update_source(
            &ws,
            &revision,
            None,
            BTreeMap::from([("model.py".into(), forbidden)]),
            "user",
        )
        .await
        .unwrap();
    let failed = core.build(&ws, &revision, &draft.id, "user").await.unwrap();
    let failed = terminal(&core, &ws, &failed.id).await;
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
    let stuck = "import os,time\nfrom pathlib import Path\n(Path(os.environ['TMPDIR'])/'entered').write_text(str(os.getpid()))\nwhile True: time.sleep(0.1)";
    let draft = core
        .update_source(
            &ws,
            &revision,
            None,
            BTreeMap::from([("model.py".into(), stuck.into())]),
            "user",
        )
        .await
        .unwrap();
    let running = core.build(&ws, &revision, &draft.id, "user").await.unwrap();
    let pid = tokio::time::timeout(Duration::from_secs(60), async {
        'found: loop {
            let job = core.get_job(&ws, &running.id).await.unwrap();
            assert!(
                matches!(job.state, JobState::Queued | JobState::Running),
                "worker ended before marker: {:?}",
                job.diagnostics
            );
            if let Ok(entries) = std::fs::read_dir(root.join("artifacts").join(&ws)) {
                for entry in entries.flatten() {
                    if let Ok(pid) =
                        std::fs::read_to_string(entry.path().join(".sandbox-tmp/entered"))
                    {
                        break 'found pid.parse::<i32>().unwrap();
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    core.dispatch(
        &cadpad_core::tools::ToolContext {
            actor: "user".into(),
        },
        "cancel_job",
        json!({"workspace_id":ws,"job_id":running.id}),
    )
    .await
    .unwrap();
    let cancelled = terminal(&core, &ws, &running.id).await;
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "host-canary");
    assert!(!output.exists());
    assert!(tcp.accept().is_err());
    assert!(unix.accept().is_err());
    assert!(udp.recv(&mut [0; 16]).is_err());
}
#[tokio::test]
async fn default_development_worker_contains_real_model_code() {
    exercise(development()).await;
}
#[tokio::test]
#[ignore = "Requires the explicit PyInstaller bundle experiment"]
async fn bundled_worker_contains_real_model_code() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    exercise(WorkerConfig::bundled(
        root.join("target/worker-bundle/cadpad-worker/cadpad-worker"),
    ))
    .await;
}

#[test]
fn worker_does_not_inherit_host_credentials() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "credential_environment_host",
            "--ignored",
            "--nocapture",
        ])
        .env("CADPAD_CONTAINMENT_HOST", "1")
        .env("ANTHROPIC_API_KEY", "cadpad-test-only-canary")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("environment-canary-host-passed"));
}
#[tokio::test]
#[ignore = "Subprocess fixture launched by worker_does_not_inherit_host_credentials"]
async fn credential_environment_host() {
    if std::env::var("CADPAD_CONTAINMENT_HOST").as_deref() != Ok("1") {
        return;
    }
    assert_eq!(
        std::env::var("ANTHROPIC_API_KEY").unwrap(),
        "cadpad-test-only-canary"
    );
    exercise(development()).await;
    println!("environment-canary-host-passed");
}

#[tokio::test]
async fn contained_rail_example_collision_validation_and_native_restart_recovery() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), development()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([(
                "model.py".into(),
                std::fs::read_to_string(repo.join("fixtures/rail_carriage.py")).unwrap(),
            )]),
            "user",
        )
        .await
        .unwrap();
    let definition = serde_json::from_slice(
        &std::fs::read(repo.join("fixtures/rail_carriage.mechanism.json")).unwrap(),
    )
    .unwrap();
    let job = core
        .define_mechanism(
            "user",
            cadpad_core::motion::DefineMechanism {
                workspace_id: ws.clone(),
                expected_base_revision: base,
                draft_id: Some(draft.id),
                definition: cadpad_core::kinematics::decode(&definition).unwrap(),
            },
        )
        .await
        .unwrap();
    let job = terminal(&core, &ws, &job.id).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    let revision = job.resulting_revision.unwrap();
    let before = core.get_project(&ws).await.unwrap();
    assert_eq!(before["model"]["parts"].as_array().unwrap().len(), 2);
    assert_eq!(
        before["model"]["collision_validation"]["method"],
        "brep_boolean_coverage"
    );
    drop(core);
    let core = Core::open(root, development()).await.unwrap();
    let inspected=core.query(&ws,&revision,"inspect",json!({"entities":[{"revision":revision,"instance_id":"rail","entity_id":"part:0"}],"pose_version":0})).await.unwrap();
    assert_eq!(inspected["entities"][0]["bounds"]["size"][0], 200.0);
    assert_eq!(
        core.get_project(&ws).await.unwrap()["model"],
        before["model"]
    );
}
