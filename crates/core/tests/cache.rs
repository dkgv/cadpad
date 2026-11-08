use cadpad_core::{Core, Job, JobState, worker::WorkerConfig};
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
async fn build(core: &Core, ws: &str, base: &str, width: u32) -> String {
    let draft = core
        .update_source(
            ws,
            base,
            None,
            BTreeMap::from([(
                "model.py".into(),
                format!(
                    "import cadquery as cq\nshow(cq.Workplane().box({width},20,6),name='part')"
                ),
            )]),
            "user",
        )
        .await
        .unwrap();
    let job = terminal(
        core,
        ws,
        core.build(ws, base, &draft.id, "user").await.unwrap(),
    )
    .await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    job.resulting_revision.unwrap()
}
async fn collect(core: &Core) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let report = core.collect_cache().await.unwrap();
            assert!(report.errors.is_empty(), "{report:?}");
            if !report.already_running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn absent(core: &Core, path: &Path) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            collect(core).await;
            if !path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn derived_cleanup_preserves_current_data_native_exports_and_reproducible_undo() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let a = build(&core, &ws, &base, 10).await;
    let export = core
        .query(&ws, &a, "export", json!({"format":"STEP"}))
        .await
        .unwrap();
    let frozen = core
        .freeze_export(&ws, &a, export["handle"].as_str().unwrap())
        .await
        .unwrap();
    let old = root.join("artifacts").join(&ws).join(&a);
    let b = build(&core, &ws, &a, 30).await;
    collect(&core).await;
    assert!(
        old.exists(),
        "A native Save dialog pins its selected export"
    );
    assert!(root.join("artifacts").join(&ws).join(&b).exists());
    let destination = temp.path().join("old-pose.step");
    frozen.save_to(&destination).unwrap();
    assert!(
        std::fs::read(destination)
            .unwrap()
            .starts_with(b"ISO-10303-21;")
    );
    absent(&core, &old).await;
    let orphan = "f".repeat(64);
    std::fs::create_dir_all(root.join("blobs")).unwrap();
    std::fs::write(root.join("blobs").join(&orphan), b"unpublished").unwrap();
    let temporary = root
        .join("workspaces")
        .join(&ws)
        .join(format!("{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, b"abandoned atomic write").unwrap();
    let trash = root
        .join("runs")
        .join(&ws)
        .join(format!(".gc-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&trash).unwrap();
    std::fs::write(trash.join("leftover"), b"interrupted cache removal").unwrap();
    let unmanaged = root
        .join("runs")
        .join(&ws)
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&unmanaged).unwrap();
    std::fs::write(unmanaged.join("legacy"), b"unleased legacy data").unwrap();
    collect(&core).await;
    assert!(!root.join("blobs").join(orphan).exists());
    assert!(!temporary.exists());
    assert!(!trash.exists());
    assert!(unmanaged.exists());
    let query = core
        .query(
            &ws,
            &b,
            "inspect",
            json!({"entities":[{"revision":b,"instance_id":"part","entity_id":"part:0"}]}),
        )
        .await
        .unwrap();
    assert!((query["entities"][0]["volume"].as_f64().unwrap() - 3600.).abs() < 1e-6);
    let undone = terminal(&core, &ws, core.undo(&ws, &b, "user").await.unwrap()).await;
    assert_eq!(undone.state, JobState::Succeeded);
    assert!(
        (core.get_project(&ws).await.unwrap()["model"]["parts"][0]["volume"]
            .as_f64()
            .unwrap()
            - 1200.)
            .abs()
            < 1e-6
    );
}
#[tokio::test]
async fn concurrent_blob_publication_and_recording_references_survive_collection() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let rev = build(&core, &ws, &base, 10).await;
    for i in 0..12 {
        let bytes = vec![i; 4096];
        let name = format!("asset-{i}.bin");
        let (draft, _) = tokio::join!(
            core.stage_asset(&ws, &rev, None, &name, bytes.clone(), "user"),
            core.collect_cache()
        );
        let draft = draft.unwrap();
        let hash = &draft.inputs.assets[&format!("asset-{i}.bin")];
        assert_eq!(std::fs::read(root.join("blobs").join(hash)).unwrap(), bytes);
    }
    let context = cadpad_core::tools::ToolContext {
        actor: "user".into(),
    };
    async fn control(
        core: &Core,
        context: &cadpad_core::tools::ToolContext,
        ws: &str,
        rev: &str,
        version: u64,
        command: Value,
    ) -> Value {
        core.dispatch(context,"control_playback",json!({"workspace_id":ws,"revision":rev,"expected_pose_version":version,"command":command})).await.unwrap()
    }
    control(
        &core,
        &context,
        &ws,
        &rev,
        0,
        json!({"action":"start_recording","name":"Retained"}),
    )
    .await;
    let saved = control(
        &core,
        &context,
        &ws,
        &rev,
        1,
        json!({"action":"finish_recording"}),
    )
    .await;
    let digest = saved["recordings"][0]["digest"].as_str().unwrap();
    collect(&core).await;
    assert!(root.join("blobs").join(digest).exists());
    let id = saved["recordings"][0]["id"].as_str().unwrap();
    control(
        &core,
        &context,
        &ws,
        &rev,
        2,
        json!({"action":"delete","recording_id":id}),
    )
    .await;
    collect(&core).await;
    assert!(!root.join("blobs").join(digest).exists());
    drop(core);
    let core = Core::open(root.clone(), config()).await.unwrap();
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], rev);
    for i in 0..12 {
        let bytes = vec![i; 4096];
        assert!(
            root.join("blobs")
                .join(cadpad_core::portable::bytes_hash(&bytes))
                .exists()
        );
    }
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "subprocess helper for abrupt-parent-exit lease test"]
async fn orphan_worker_helper() {
    let Some(root) = std::env::var_os("CADPAD_CACHE_LEASE_TEST") else {
        return;
    };
    let root = PathBuf::from(root);
    let core = Core::open(root.join("app"), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let marker = serde_json::to_string(&root.join("worker.pid").to_str().unwrap()).unwrap();
    let guardian_marker =
        serde_json::to_string(&root.join("guardian.pid").to_str().unwrap()).unwrap();
    let source = format!(
        "import os,time\nopen({guardian_marker},'w').write(str(os.getppid()))\nopen({marker},'w').write(str(os.getpid()))\ntime.sleep(30)\n"
    );
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
    let draft = core
        .stage_asset(
            &ws,
            &base,
            Some(&draft.id),
            "payload.bin",
            b"old immutable asset".to_vec(),
            "user",
        )
        .await
        .unwrap();
    let snapshot_asset = draft.inputs.assets["payload.bin"].clone();
    let job = core.build(&ws, &base, &draft.id, "user").await.unwrap();
    while !root.join("worker.pid").exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    core.update_source(
        &ws,
        &base,
        Some(&draft.id),
        BTreeMap::from([("model.py".into(), "# newer mutable draft\n".into())]),
        "user",
    )
    .await
    .unwrap();
    core.stage_asset(
        &ws,
        &base,
        Some(&draft.id),
        "payload.bin",
        b"new mutable asset".to_vec(),
        "user",
    )
    .await
    .unwrap();
    std::fs::write(
        root.join("controller.json"),
        json!({"workspace":ws,"revision":base,"job":job,"snapshot_asset":snapshot_asset})
            .to_string(),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_secs(60)).await;
}
#[cfg(unix)]
#[tokio::test]
async fn abrupt_parent_exit_reaps_native_execution_and_recovers_the_original_job_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut parent = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "orphan_worker_helper",
            "--nocapture",
        ])
        .env("CADPAD_CACHE_LEASE_TEST", root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !root.join("controller.json").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let pid: i32 = std::fs::read_to_string(root.join("worker.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let guardian: i32 = std::fs::read_to_string(root.join("guardian.pid"))
        .unwrap()
        .parse()
        .unwrap();
    struct Cleanup {
        worker: Option<i32>,
        guardian: Option<i32>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            unsafe {
                if let Some(pid) = self.worker {
                    libc::kill(pid, libc::SIGKILL);
                }
                if let Some(pid) = self.guardian {
                    libc::kill(pid, libc::SIGCONT);
                }
            }
        }
    }
    let mut cleanup = Cleanup {
        worker: Some(pid),
        guardian: Some(guardian),
    };
    // Pause only the trusted supervisor to make the lease interval deterministic.
    // The geometry worker is still alive after app death until we resume it.
    assert_eq!(unsafe { libc::kill(guardian, libc::SIGSTOP) }, 0);
    let controller: Value =
        serde_json::from_slice(&std::fs::read(root.join("controller.json")).unwrap()).unwrap();
    let ws = controller["workspace"].as_str().unwrap();
    let job_id = controller["job"]["id"].as_str().unwrap();
    let snapshot = controller["job"]["snapshot_id"].as_str().unwrap();
    parent.kill().await.unwrap();
    parent.wait().await.unwrap();
    let core = Core::open(root.join("app"), config()).await.unwrap();
    let digest = controller["snapshot_asset"].as_str().unwrap();
    assert_eq!(
        std::fs::read(root.join("app/blobs").join(digest)).unwrap(),
        b"old immutable asset"
    );
    let job = core.get_job(ws, job_id).await.unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.diagnostics["code"], "WORKER_UNAVAILABLE");
    let source = core.read_source(ws, snapshot, "model.py").await.unwrap();
    assert!(
        source["source"]
            .as_str()
            .unwrap()
            .contains("time.sleep(30)")
    );
    let run = root.join("app/runs").join(ws).join(job_id);
    assert!(run.exists());
    let report = core.collect_cache().await.unwrap();
    assert!(report.retained_leased >= 2, "{report:?}");
    assert!(run.exists());
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    assert_eq!(unsafe { libc::kill(guardian, libc::SIGCONT) }, 0);
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
    cleanup.worker = None;
    cleanup.guardian = None;
    absent(&core, &run).await;
    drop(core);
    let core = Core::open(root.join("app"), config()).await.unwrap();
    assert_eq!(
        core.get_job(ws, job_id).await.unwrap().state,
        JobState::Failed
    );
    assert!(
        core.read_source(ws, snapshot, "model.py").await.unwrap()["source"]
            .as_str()
            .unwrap()
            .contains("time.sleep(30)")
    );
}
