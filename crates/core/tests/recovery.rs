use cadpad_core::{Core, Job, JobState, tools::ToolContext, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
fn config() -> WorkerConfig {
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
    .uncontained_for_trusted_tests()
}
async fn terminal(core: &Core, ws: &str, job: Job) -> Job {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
async fn built(core: &Core, ws: &str, base: &str, draft: &str) -> String {
    let job = terminal(core, ws, core.build(ws, base, draft, "user").await.unwrap()).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    job.resulting_revision.unwrap()
}
async fn model(core: &Core, source: String) -> (String, String) {
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
            BTreeMap::from([("model.py".into(), source)]),
            "user",
        )
        .await
        .unwrap();
    let rev = built(core, &ws, &base, &draft.id).await;
    (ws, rev)
}
fn quoted(path: &Path) -> String {
    serde_json::to_string(path.to_str().unwrap()).unwrap()
}
fn counted(marker: &Path) -> String {
    format!(
        "from pathlib import Path\np=Path({})\np.write_text(str(int(p.read_text())+1) if p.exists() else '1')\nimport cadquery as cq\nshow(cq.Workplane().box(10,20,6),name='part')",
        quoted(marker)
    )
}
fn buffer(project: &Value) -> String {
    project["model"]["parts"][0]["mesh"]["buffers"]["vertices"]["handle"]
        .as_str()
        .unwrap()
        .to_string()
}
fn bins(root: &Path, ws: &str, rev: &str) -> usize {
    std::fs::read_dir(root.join("artifacts").join(ws).join(rev))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|e| e == "bin")
        })
        .count()
}
async fn inspect(core: &Core, ws: &str, rev: &str) -> cadpad_core::Result<Value> {
    core.query(
        ws,
        rev,
        "inspect",
        json!({"entities":[{"revision":rev,"instance_id":"part","entity_id":"part:0"}]}),
    )
    .await
}
async fn repair(core: &Core, ws: &str, rev: &str) -> Job {
    terminal(
        core,
        ws,
        core.repair_display(
            "user",
            cadpad_core::recovery::RepairDisplay {
                workspace_id: ws.into(),
                revision: rev.into(),
            },
        )
        .await
        .unwrap(),
    )
    .await
}
#[tokio::test]
async fn coalesced_native_recovery_reuses_display_bytes_and_context_reads_never_execute_source() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let marker = temp.path().join("executions");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let (ws, rev) = model(&core, counted(&marker)).await;
    let before = core.get_project(&ws).await.unwrap();
    let count = bins(&root, &ws, &rev);
    drop(core);
    let core = Core::open(root.clone(), config()).await.unwrap();
    assert!(core.get_project(&ws).await.unwrap()["display_error"].is_null());
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "1");
    let (a, b) = tokio::join!(inspect(&core, &ws, &rev), inspect(&core, &ws, &rev));
    a.unwrap();
    b.unwrap();
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "2");
    assert_eq!(bins(&root, &ws, &rev), count);
    assert_eq!(
        core.get_project(&ws).await.unwrap()["model"],
        before["model"]
    );
}
#[tokio::test]
async fn missing_and_corrupt_display_data_restore_without_design_or_pose_changes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let marker = temp.path().join("executions");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let (ws, mut rev) = model(&core, counted(&marker)).await;
    let definition=cadpad_core::kinematics::decode(&json!({"joints":[{"id":"slide","parent":null,"child":"part","kind":{"type":"prismatic","axis":[1,0,0]},"initial":0,"limits":[0,0.1]}]})).unwrap();
    let job = terminal(
        &core,
        &ws,
        core.define_mechanism(
            "user",
            cadpad_core::motion::DefineMechanism {
                workspace_id: ws.clone(),
                expected_base_revision: rev.clone(),
                draft_id: None,
                definition,
            },
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(job.state, JobState::Succeeded);
    rev = job.resulting_revision.unwrap();
    core.set_coordinate(
        "user",
        cadpad_core::motion::SetCoordinate {
            workspace_id: ws.clone(),
            revision: rev.clone(),
            expected_pose_version: 0,
            joint_id: "slide".into(),
            coordinate: 0.03,
            unit: "m".into(),
            check_collision: true,
    },
    )
    .await
    .unwrap();
    let executions: u32 = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
    let before = core.get_project(&ws).await.unwrap();
    let handle = buffer(&before);
    let path = root.join("artifacts").join(&ws).join(&rev).join(&handle);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        core.get_project(&ws).await.unwrap()["display_error"]["code"],
        "DISPLAY_CACHE_MISSING"
    );
    let job = repair(&core, &ws, &rev).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    assert_eq!(job.resulting_revision.as_deref(), Some(rev.as_str()));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        executions.to_string()
    );
    std::fs::write(&path, vec![0x5a; bytes.len()]).unwrap();
    assert_eq!(
        core.get_project(&ws).await.unwrap()["display_error"]["code"],
        "DISPLAY_CACHE_MISSING"
    );
    let job = repair(&core, &ws, &rev).await;
    assert_eq!(job.state, JobState::Succeeded);
    let after = core.get_project(&ws).await.unwrap();
    assert!(after["display_error"].is_null());
    assert_eq!(after["revision"], rev);
    assert_eq!(after["dirty"], before["dirty"]);
    assert_eq!(after["motion"], before["motion"]);
    assert_eq!(after["model"], before["model"]);
    drop(core);
    std::fs::remove_file(&path).unwrap();
    let core = Core::open(root, config()).await.unwrap();
    let context = core.get_project(&ws).await.unwrap();
    assert_eq!(context["display_error"]["code"], "DISPLAY_CACHE_MISSING");
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        executions.to_string()
    );
    let job = repair(&core, &ws, &rev).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        (executions + 1).to_string()
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}
#[tokio::test]
async fn equal_bounds_and_volume_do_not_allow_changed_geometry_under_a_committed_revision() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let flag = temp.path().join("changed");
    let code = format!(
        "from pathlib import Path\nimport cadquery as cq\nx=10 if Path({}).exists() else -10\nshow(cq.Workplane().box(50,30,6).faces('>Z').workplane().pushPoints([(x,0)]).hole(4),name='part')",
        quoted(&flag)
    );
    let core = Core::open(root.clone(), config()).await.unwrap();
    let (ws, rev) = model(&core, code).await;
    let before = core.get_project(&ws).await.unwrap();
    drop(core);
    std::fs::write(flag, b"changed").unwrap();
    let core = Core::open(root.clone(), config()).await.unwrap();
    assert_eq!(
        inspect(&core, &ws, &rev).await.unwrap_err().code(),
        "RECOVERY_GEOMETRY_CHANGED"
    );
    assert_eq!(
        core.get_project(&ws).await.unwrap()["model"],
        before["model"]
    );
    let path = root
        .join("artifacts")
        .join(&ws)
        .join(&rev)
        .join(buffer(&before));
    std::fs::remove_file(path).unwrap();
    let failed = repair(&core, &ws, &rev).await;
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.diagnostics["code"], "RECOVERY_GEOMETRY_CHANGED");
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], rev);
    let draft = core
        .update_source(&ws, &rev, None, BTreeMap::new(), "user")
        .await
        .unwrap();
    let next = built(&core, &ws, &rev, &draft.id).await;
    let after = core.get_project(&ws).await.unwrap();
    assert_ne!(next, rev);
    assert_ne!(
        after["model"]["parts"][0]["geometry_fingerprint"],
        before["model"]["parts"][0]["geometry_fingerprint"]
    );
    assert_eq!(
        after["model"]["parts"][0]["bounds"],
        before["model"]["parts"][0]["bounds"]
    );
    assert!(
        (after["model"]["parts"][0]["volume"].as_f64().unwrap()
            - before["model"]["parts"][0]["volume"].as_f64().unwrap())
        .abs()
            < 1e-6
    );
}
#[tokio::test]
async fn restore_job_cancels_recovery_that_has_actually_entered_execution() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let flag = temp.path().join("block");
    let entered = temp.path().join("entered");
    let code = format!(
        "import os,time\nfrom pathlib import Path\nif Path({}).exists():\n Path({}).write_text(str(os.getpid()))\n while True: time.sleep(0.1)\nimport cadquery as cq\nshow(cq.Workplane().box(10,20,6),name='part')",
        quoted(&flag),
        quoted(&entered)
    );
    let core = Core::open(root.clone(), config()).await.unwrap();
    let (ws, rev) = model(&core, code).await;
    let handle = buffer(&core.get_project(&ws).await.unwrap());
    drop(core);
    std::fs::write(flag, b"block").unwrap();
    std::fs::remove_file(root.join("artifacts").join(&ws).join(&rev).join(handle)).unwrap();
    let core = Core::open(root, config()).await.unwrap();
    let job = core
        .repair_display(
            "user",
            cadpad_core::recovery::RepairDisplay {
                workspace_id: ws.clone(),
                revision: rev.clone(),
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !entered.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    core.dispatch(
        &ToolContext {
            actor: "user".into(),
        },
        "cancel_job",
        json!({"workspace_id":ws,"job_id":job.id}),
    )
    .await
    .unwrap();
    let job = terminal(&core, &ws, job).await;
    assert_eq!(job.state, JobState::Cancelled);
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], rev);
    #[cfg(unix)]
    {
        let pid: i32 = std::fs::read_to_string(entered).unwrap().parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }
}

#[tokio::test]
async fn legacy_revisions_require_explicit_rebuild_instead_of_unproven_native_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let marker = temp.path().join("executions");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let (ws, rev) = model(&core, counted(&marker)).await;
    drop(core);
    let path = root.join("workspaces").join(&ws).join("workspace.json");
    let mut stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    stored["revisions"][&rev]["model"]["parts"][0]
        .as_object_mut()
        .unwrap()
        .remove("geometry_fingerprint");
    std::fs::write(path, serde_json::to_vec(&stored).unwrap()).unwrap();
    let core = Core::open(root, config()).await.unwrap();
    let context = core.get_project(&ws).await.unwrap();
    assert!(context["display_error"].is_null(), "{context}");
    assert_eq!(
        inspect(&core, &ws, &rev).await.unwrap_err().code(),
        "RECOVERY_VERIFICATION_REQUIRED"
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "1");
    let draft = core
        .update_source(&ws, &rev, None, BTreeMap::new(), "user")
        .await
        .unwrap();
    let next = built(&core, &ws, &rev, &draft.id).await;
    assert_ne!(next, rev);
    inspect(&core, &ws, &next).await.unwrap();
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "2");
}

#[tokio::test]
async fn old_display_format_with_valid_checksums_requires_verified_same_revision_remeshing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let marker = temp.path().join("executions");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let (ws, rev) = model(
        &core,
        format!(
            "{}\nshow(cq.Workplane().sphere(25),name='other')",
            counted(&marker)
        ),
    )
    .await;
    let expected = core.get_project(&ws).await.unwrap()["model"].clone();
    assert_ne!(
        expected["parts"][0]["mesh"]["buffers"]["vertices"],
        expected["parts"][1]["mesh"]["buffers"]["vertices"]
    );
    drop(core);
    let path = root.join("workspaces").join(&ws).join("workspace.json");
    let mut stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    // Version 1 could publish the first shape's valid bytes for a distinct part.
    let mut old = stored["revisions"][&rev]["model"]["parts"][0]["mesh"].clone();
    old["format"] = json!("cadpad-mesh-v1");
    for part in stored["revisions"][&rev]["model"]["parts"]
        .as_array_mut()
        .unwrap()
    {
        part["mesh"] = old.clone();
    }
    std::fs::write(path, serde_json::to_vec(&stored).unwrap()).unwrap();
    let core = Core::open(root, config()).await.unwrap();
    assert_eq!(
        core.get_project(&ws).await.unwrap()["display_error"]["code"],
        "DISPLAY_CACHE_MISSING"
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "1");
    let repaired = repair(&core, &ws, &rev).await;
    assert_eq!(
        repaired.state,
        JobState::Succeeded,
        "{:?}",
        repaired.diagnostics
    );
    assert_eq!(repaired.resulting_revision.as_deref(), Some(rev.as_str()));
    let project = core.get_project(&ws).await.unwrap();
    assert!(project["display_error"].is_null());
    assert_eq!(project["model"], expected);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "2");
}
