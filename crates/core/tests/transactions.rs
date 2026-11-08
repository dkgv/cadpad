use cadpad_core::{Core, Job, JobState, worker::WorkerConfig};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use tempfile::TempDir;

fn config() -> WorkerConfig {
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
    .uncontained_for_trusted_tests()
}
async fn setup() -> (TempDir, Core, String, String) {
    let directory = TempDir::new().unwrap();
    let core = Core::open(directory.path().to_path_buf(), config())
        .await
        .unwrap();
    let workspace = core.create_workspace("user").await.unwrap();
    let revision = core.get_project(&workspace).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    (directory, core, workspace, revision)
}
async fn terminal(core: &Core, workspace: &str, job: &Job) -> Job {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let current = core.get_job(workspace, &job.id).await.unwrap();
            if matches!(
                current.state,
                JobState::Succeeded | JobState::Failed | JobState::Cancelled
            ) {
                return current;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("job should terminate")
}
const BOX: &str = "import cadquery as cq\nw=parameter('width',10,minimum=1,maximum=100)\nshow(cq.Workplane().box(w,20,30),name='box')";
async fn stage(
    core: &Core,
    workspace: &str,
    base: &str,
    source: &str,
    actor: &str,
) -> cadpad_core::Draft {
    core.update_source(
        workspace,
        base,
        None,
        BTreeMap::from([("model.py".into(), source.into())]),
        actor,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn real_build_failure_parameter_undo_recovery_and_export() {
    let (directory, core, workspace, base) = setup().await;
    let draft = stage(&core, &workspace, &base, BOX, "agent").await;
    let job = core
        .build(&workspace, &base, &draft.id, "agent")
        .await
        .unwrap();
    let job = terminal(&core, &workspace, &job).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    let first = job.resulting_revision.unwrap();
    let project = core.get_project(&workspace).await.unwrap();
    assert_eq!(project["model"]["parts"][0]["volume"], 6000.0);
    let invalid = stage(
        &core,
        &workspace,
        &first,
        "raise ValueError('intentional failure')",
        "external",
    )
    .await;
    let failed = core
        .build(&workspace, &first, &invalid.id, "external")
        .await
        .unwrap();
    let failed = terminal(&core, &workspace, &failed).await;
    assert_eq!(failed.state, JobState::Failed);
    assert!(
        failed
            .diagnostics
            .to_string()
            .contains("intentional failure")
    );
    assert_eq!(
        core.get_project(&workspace).await.unwrap()["revision"],
        first
    );
    assert!(
        core.read_source(&workspace, &invalid.id, "model.py")
            .await
            .unwrap()["source"]
            .as_str()
            .unwrap()
            .contains("intentional")
    );
    let change = core
        .set_parameter(&workspace, &first, "width", 25.0, "mm", "user")
        .await
        .unwrap();
    let change = terminal(&core, &workspace, &change).await;
    assert_eq!(
        change.state,
        JobState::Succeeded,
        "{:?}",
        change.diagnostics
    );
    let second = change.resulting_revision.unwrap();
    assert_eq!(
        core.get_project(&workspace).await.unwrap()["model"]["parts"][0]["volume"],
        15000.0
    );
    assert_eq!(
        core.query(&workspace, &first, "inspect", json!({}))
            .await
            .unwrap_err()
            .code(),
        "STALE_ENTITY"
    );
    let undo = core.undo(&workspace, &second, "user").await.unwrap();
    let undo = terminal(&core, &workspace, &undo).await;
    assert_eq!(undo.state, JobState::Succeeded, "{:?}", undo.diagnostics);
    let restored = undo.resulting_revision.unwrap();
    assert_ne!(restored, first);
    assert_eq!(
        core.get_project(&workspace).await.unwrap()["model"]["parts"][0]["volume"],
        6000.0
    );
    drop(core);
    let recovered = Core::open(directory.path().to_path_buf(), config())
        .await
        .unwrap();
    assert_eq!(
        recovered.get_project(&workspace).await.unwrap()["revision"],
        restored
    );
    let exported = recovered
        .query(&workspace, &restored, "export", json!({"format":"STEP"}))
        .await
        .unwrap();
    let bytes = recovered
        .artifact(&workspace, &restored, exported["handle"].as_str().unwrap())
        .await
        .unwrap();
    assert!(bytes.starts_with(b"ISO-10303-21;"));
    assert!(
        recovered
            .artifact(&workspace, &restored, "../../workspace.json")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn concurrent_drafts_conflict_and_do_not_retarget_workspaces() {
    let (_directory, core, workspace, base) = setup().await;
    let other = core.create_workspace("user").await.unwrap();
    let a = stage(&core, &workspace, &base, BOX, "a").await;
    let b = stage(
        &core,
        &workspace,
        &base,
        &format!("{BOX}\n# alternate"),
        "b",
    )
    .await;
    assert!(core.build(&workspace, &base, &a.id, "b").await.is_err());
    let ja = core.build(&workspace, &base, &a.id, "a").await.unwrap();
    let jb = core.build(&workspace, &base, &b.id, "b").await.unwrap();
    core.focus_workspace(&other).await.unwrap();
    let (ja, jb) = tokio::join!(
        terminal(&core, &workspace, &ja),
        terminal(&core, &workspace, &jb)
    );
    assert_eq!(
        [&ja, &jb]
            .iter()
            .filter(|j| j.state == JobState::Succeeded)
            .count(),
        1,
        "{ja:?} {jb:?}"
    );
    let loser = if ja.state == JobState::Failed { ja } else { jb };
    assert_eq!(loser.diagnostics["code"], "REVISION_CONFLICT");
    assert_eq!(
        core.focused_workspace().await.as_deref(),
        Some(other.as_str())
    );
    assert_eq!(
        core.get_project(&other).await.unwrap()["model"]["parts"],
        json!([])
    );
    assert_eq!(
        core.update_source(&workspace, &base, None, BTreeMap::new(), "a")
            .await
            .unwrap_err()
            .code(),
        "REVISION_CONFLICT"
    );
}

#[tokio::test]
async fn cancel_reaps_stuck_worker_preserves_source_and_rejects_path_escape() {
    let (_directory, core, workspace, base) = setup().await;
    for path in [
        "../escape.py",
        "/absolute.py",
        "nested/../../escape.py",
        ".venv/escape.py",
        "assets/x.py",
        "x\\y.py",
    ] {
        assert!(
            core.update_source(
                &workspace,
                &base,
                None,
                BTreeMap::from([(path.into(), "".into())]),
                "user"
            )
            .await
            .is_err()
        );
    }
    let draft = stage(
        &core,
        &workspace,
        &base,
        "import time\ntime.sleep(120)",
        "agent",
    )
    .await;
    let job = core
        .build(&workspace, &base, &draft.id, "agent")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let requested = core.cancel_job(&workspace, &job.id).await.unwrap();
    assert!(matches!(
        requested.state,
        JobState::Queued | JobState::Running
    ));
    let done = terminal(&core, &workspace, &job).await;
    assert_eq!(done.state, JobState::Cancelled);
    assert_eq!(
        core.get_project(&workspace).await.unwrap()["revision"],
        base
    );
    let source = core
        .read_source(&workspace, &base, "model.py")
        .await
        .unwrap();
    assert!(!source["source"].as_str().unwrap().contains("sleep"));
}

#[tokio::test]
async fn immutable_build_snapshot_survives_later_draft_edits_and_undo_to_empty() {
    let (_directory, core, workspace, base) = setup().await;
    let source = format!("import time\ntime.sleep(0.5)\n{BOX}");
    let draft = stage(&core, &workspace, &base, &source, "agent").await;
    let job = core
        .build(&workspace, &base, &draft.id, "agent")
        .await
        .unwrap();
    core.update_source(
        &workspace,
        &base,
        Some(&draft.id),
        BTreeMap::from([("model.py".into(), "raise ValueError('later draft')".into())]),
        "agent",
    )
    .await
    .unwrap();
    let done = terminal(&core, &workspace, &job).await;
    assert_eq!(done.state, JobState::Succeeded, "{:?}", done.diagnostics);
    let revision = done.resulting_revision.unwrap();
    assert_eq!(
        core.read_source(&workspace, &revision, "model.py")
            .await
            .unwrap()["source"],
        source
    );
    assert!(
        core.read_source(&workspace, &draft.id, "model.py")
            .await
            .unwrap()["source"]
            .as_str()
            .unwrap()
            .contains("later draft")
    );
    let undo = core.undo(&workspace, &revision, "user").await.unwrap();
    let undo = terminal(&core, &workspace, &undo).await;
    assert_eq!(undo.state, JobState::Succeeded, "{:?}", undo.diagnostics);
    assert_eq!(
        core.get_project(&workspace).await.unwrap()["model"]["parts"],
        json!([])
    );
}

#[tokio::test]
async fn native_worker_exit_does_not_replace_committed_model() {
    let (_directory, core, workspace, base) = setup().await;
    let draft = stage(&core, &workspace, &base, "import os\nos._exit(7)", "agent").await;
    let job = core
        .build(&workspace, &base, &draft.id, "agent")
        .await
        .unwrap();
    let done = terminal(&core, &workspace, &job).await;
    assert_eq!(done.state, JobState::Failed);
    assert_eq!(done.diagnostics["code"], "WORKER_UNAVAILABLE");
    assert_eq!(
        core.get_project(&workspace).await.unwrap()["revision"],
        base
    );
}
