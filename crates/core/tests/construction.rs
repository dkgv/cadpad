#![cfg(target_os = "macos")]
use cadpad_core::{Core, Draft, Job, JobState, worker::WorkerConfig};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

const GOOD: &str = "import cadquery as cq\nshow(cq.Workplane().box(10,20,30),name='box')";
const BLOCK: &str = r#"
import os, sys, time
from pathlib import Path
cache = Path(sys.argv[sys.argv.index('--cache') + 1])
(cache / 'entered').write_text(str(os.getpid()))
while not (cache / 'release').exists(): time.sleep(0.01)
import cadquery as cq
show(cq.Workplane().box(10,20,30),name='box')
"#;

async fn stage(core: &Core, ws: &str, revision: &str, source: &str, actor: &str) -> Draft {
    core.update_source(
        ws,
        revision,
        None,
        BTreeMap::from([("model.py".into(), source.into())]),
        actor,
    )
    .await
    .unwrap()
}
async fn build(core: &Core, ws: &str, revision: &str, source: &str, actor: &str) -> Job {
    let draft = stage(core, ws, revision, source, actor).await;
    core.build(ws, revision, &draft.id, actor).await.unwrap()
}
async fn terminal(core: &Core, ws: &str, job: &Job) -> Job {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let result = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(result.state, JobState::Queued | JobState::Running) {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
async fn entered(root: &Path, ws: &str) -> PathBuf {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(directories) = std::fs::read_dir(root.join("artifacts").join(ws)) {
                for directory in directories {
                    let path = directory.unwrap().path();
                    if path.join("entered").exists() {
                        return path;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn queued_without_source_execution(core: &Core, root: &Path, ws: &str, job: &Job) {
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(matches!(
        core.get_job(ws, &job.id).await.unwrap().state,
        JobState::Queued | JobState::Running
    ));
    assert!(
        !root.join("runs").join(ws).join(&job.id).exists(),
        "queued work must not materialize or spawn"
    );
}

#[tokio::test]
async fn bounded_native_construction_preserves_navigation_cancellation_and_immutable_queue_inputs()
{
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let core = Core::open(root.clone(), WorkerConfig::development(&repo))
        .await
        .unwrap();
    let mut workspaces = Vec::new();
    for _ in 0..3 {
        let ws = core.create_workspace("user").await.unwrap();
        let base = core.get_project(&ws).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let good = terminal(&core, &ws, &build(&core, &ws, &base, GOOD, "user").await).await;
        assert_eq!(good.state, JobState::Succeeded, "{}", good.diagnostics);
        workspaces.push((ws, good.resulting_revision.unwrap()));
    }
    let (a, ar) = &workspaces[0];
    let (b, br) = &workspaces[1];
    let (c, cr) = &workspaces[2];
    let first = build(&core, a, ar, BLOCK, "first-agent").await;
    let first_cache = entered(&root, a).await;
    let second = build(&core, b, br, BLOCK, "second-agent").await;
    let _second_cache = entered(&root, b).await;
    let changed = format!("{GOOD}\n# Force native construction");
    let pending = build(&core, c, cr, &changed, "queued-agent").await;
    queued_without_source_execution(&core, &root, c, &pending).await;

    // The committed interpreter is usable while both construction slots are busy.
    let project = tokio::time::timeout(Duration::from_secs(2), core.get_project(c))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(project["revision"], cr.as_str());
    let exact = tokio::time::timeout(
        Duration::from_secs(2),
        core.query(
            c,
            cr,
            "inspect",
            json!({"entities":[{"revision":cr,"instance_id":"box","entity_id":"part:0"}]}),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(exact["entities"][0]["volume"], 6000.0);
    core.cancel_job(c, &pending.id).await.unwrap();
    let cancelled = tokio::time::timeout(Duration::from_secs(2), terminal(&core, c, &pending))
        .await
        .unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert_eq!(cancelled.diagnostics["code"], "CANCELLED");
    assert!(!root.join("runs").join(c).join(&pending.id).exists());

    let disconnected = build(&core, c, cr, &changed, "disconnected-agent").await;
    queued_without_source_execution(&core, &root, c, &disconnected).await;
    assert_eq!(core.revoke_actor("disconnected-agent").await, 1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), terminal(&core, c, &disconnected))
            .await
            .unwrap()
            .state,
        JobState::Cancelled
    );
    assert!(!root.join("runs").join(c).join(&disconnected.id).exists());

    let stale = build(
        &core,
        a,
        ar,
        "raise RuntimeError('stale source must never execute')",
        "stale-agent",
    )
    .await;
    queued_without_source_execution(&core, &root, a, &stale).await;
    let failed = build(
        &core,
        c,
        cr,
        "raise RuntimeError('capacity release fixture')",
        "repair-agent",
    )
    .await;
    queued_without_source_execution(&core, &root, c, &failed).await;
    let source15 = GOOD.replace("box(10,20,30)", "box(15,20,30)");
    let draft = stage(&core, c, cr, &source15, "repair-agent").await;
    let immutable = core.build(c, cr, &draft.id, "repair-agent").await.unwrap();
    queued_without_source_execution(&core, &root, c, &immutable).await;
    let source16 = GOOD.replace("box(10,20,30)", "box(16,20,30)");
    core.update_source(
        c,
        cr,
        Some(&draft.id),
        BTreeMap::from([("model.py".into(), source16.clone())]),
        "repair-agent",
    )
    .await
    .unwrap();

    // One released slot admits both successors, even after an execution error.
    std::fs::write(first_cache.join("release"), b"go").unwrap();
    assert_eq!(terminal(&core, a, &first).await.state, JobState::Succeeded);
    let stale = terminal(&core, a, &stale).await;
    assert_eq!(stale.state, JobState::Failed);
    assert_eq!(stale.diagnostics["code"], "REVISION_CONFLICT");
    assert!(!root.join("runs").join(a).join(&stale.id).exists());
    let failed = terminal(&core, c, &failed).await;
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.diagnostics["code"], "BUILD_FAILED");
    let built = terminal(&core, c, &immutable).await;
    assert_eq!(built.state, JobState::Succeeded, "{}", built.diagnostics);
    let revision = built.resulting_revision.unwrap();
    assert_eq!(
        core.get_project(c).await.unwrap()["model"]["parts"][0]["volume"],
        9000.0
    );
    assert_eq!(
        core.read_source(c, &revision, "model.py").await.unwrap()["source"],
        source15
    );
    assert_eq!(
        core.read_source(c, &draft.id, "model.py").await.unwrap()["source"],
        source16
    );
    assert_eq!(
        core.get_job(b, &second.id).await.unwrap().state,
        JobState::Running
    );
    core.cancel_job(b, &second.id).await.unwrap();
    assert_eq!(terminal(&core, b, &second).await.state, JobState::Cancelled);
    assert_eq!(core.get_project(b).await.unwrap()["revision"], br.as_str());
    let after = terminal(
        &core,
        c,
        &build(&core, c, &revision, &source16, "repair-agent").await,
    )
    .await;
    assert_eq!(after.state, JobState::Succeeded, "{}", after.diagnostics);
    assert_eq!(
        core.get_project(c).await.unwrap()["model"]["parts"][0]["volume"],
        9600.0
    );
}
