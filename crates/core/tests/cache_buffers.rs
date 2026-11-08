//! Real contained CAD workers; no desktop or application data is opened.
use cadpad_core::{Core, Job, JobState, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};
fn config() -> WorkerConfig {
    if let Some(resources) = std::env::var_os("CADPAD_TEST_PACKAGED_RESOURCES") {
        assert_eq!(std::env::var_os("PATH").unwrap_or_default(), "");
        return cadpad_core::runtime::PackagedRuntime::open(Path::new(&resources))
            .unwrap()
            .worker_config();
    }
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
}
async fn terminal(core: &Core, ws: &str, job: &Job) -> Job {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn build(core: &Core, ws: &str, source: String) -> String {
    let base = core.get_project(ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let draft = core
        .update_source(
            ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), source)]),
            "cache-buffer-test",
        )
        .await
        .unwrap();
    let job = core
        .build(ws, &base, &draft.id, "cache-buffer-test")
        .await
        .unwrap();
    let job = terminal(core, ws, &job).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    job.resulting_revision.unwrap()
}
async fn collect(core: &Core) -> cadpad_core::cache::Report {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let report = core.collect_cache().await.unwrap();
            assert!(report.errors.is_empty(), "{report:?}");
            if !report.already_running {
                return report;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
fn handles(project: &Value) -> HashSet<String> {
    project["model"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|part| {
            part["mesh"]["buffers"]
                .as_object()
                .unwrap()
                .values()
                .map(|buffer| buffer["handle"].as_str().unwrap().to_owned())
        })
        .collect()
}
async fn verify_buffers(core: &Core, ws: &str, project: &Value) {
    let rev = project["revision"].as_str().unwrap();
    for part in project["model"]["parts"].as_array().unwrap() {
        for info in part["mesh"]["buffers"].as_object().unwrap().values() {
            let bytes = core
                .artifact(ws, rev, info["handle"].as_str().unwrap())
                .await
                .unwrap();
            assert_eq!(bytes.len() as u64, info["bytes"].as_u64().unwrap());
            assert_eq!(
                cadpad_core::portable::bytes_hash(&bytes),
                info["sha256"].as_str().unwrap()
            );
        }
    }
}
#[tokio::test]
async fn current_buffer_cleanup_retains_source_recordings_exports_readers_and_exact_geometry() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let ws = core.create_workspace("cache-buffer-test").await.unwrap();
    let rev = build(
        &core,
        &ws,
        "import cadquery as cq\nshow(cq.Workplane().box(10,20,6),name='part')\n".into(),
    )
    .await;
    let directory = root.join("artifacts").join(&ws).join(&rev);
    let command = |version, action: &str| cadpad_core::playback::Control {
        workspace_id: ws.clone(),
        revision: rev.clone(),
        expected_pose_version: version,
        command: serde_json::from_value(json!({"action":action,"name":"Retained recording"}))
            .unwrap(),
    };
    // List/finish commands deny unknown fields, so construct each independently.
    core.control_playback("cache-buffer-test", command(0, "start_recording"))
        .await
        .unwrap();
    let finish:cadpad_core::playback::Control=serde_json::from_value(json!({"workspace_id":ws,"revision":rev,"expected_pose_version":1,"command":{"action":"finish_recording"}})).unwrap();
    let saved = core
        .control_playback("cache-buffer-test", finish)
        .await
        .unwrap();
    assert_eq!(saved.recordings.len(), 1);
    let export = core
        .query(
            &ws,
            &rev,
            "export",
            json!({"format":"STEP","pose_version":saved.snapshot.pose_version}),
        )
        .await
        .unwrap();
    let frozen = core
        .freeze_export(&ws, &rev, export["handle"].as_str().unwrap())
        .await
        .unwrap();
    let before = core.get_project(&ws).await.unwrap();
    let journal = std::fs::read(root.join("workspaces").join(&ws).join("workspace.json")).unwrap();
    let orphan = format!(
        "{}.bin",
        cadpad_core::portable::bytes_hash(b"unused buffer")
    );
    let reading = format!(
        "{}.bin",
        cadpad_core::portable::bytes_hash(b"old displayed buffer")
    );
    assert!(!handles(&before).contains(&orphan));
    assert!(!handles(&before).contains(&reading));
    std::fs::write(directory.join(&orphan), b"unused buffer").unwrap();
    std::fs::write(directory.join(&reading), b"old displayed buffer").unwrap();
    let pin = std::fs::File::open(directory.join(&reading)).unwrap();
    pin.try_lock_shared().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = temp.path().join("outside.bin");
        std::fs::write(&outside, b"outside cache authority").unwrap();
        symlink(outside, directory.join(format!("{}.bin", "f".repeat(64)))).unwrap();
        let fifo = std::ffi::CString::new(
            directory
                .join(format!("{}.bin", "e".repeat(64)))
                .to_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    }
    let report = collect(&core).await;
    assert_eq!(report.removed_mesh_buffers, 1, "{report:?}");
    assert!(report.retained_leased >= 1);
    assert!(!directory.join(&orphan).exists());
    assert!(directory.join(&reading).exists());
    assert_eq!(core.get_project(&ws).await.unwrap(), before);
    assert_eq!(
        std::fs::read(root.join("workspaces").join(&ws).join("workspace.json")).unwrap(),
        journal
    );
    verify_buffers(&core, &ws, &before).await;
    let inspect = core
        .query(
            &ws,
            &rev,
            "inspect",
            json!({"pose_version":before["motion"]["pose_version"],"entities":[{"revision":rev,"instance_id":"part","entity_id":"part:0"}]}),
        )
        .await
        .unwrap();
    assert!((inspect["entities"][0]["volume"].as_f64().unwrap() - 1200.).abs() < 1e-8);
    let read = core
        .export_resource(
            "cache-buffer-test",
            &ws,
            &rev,
            export["handle"].as_str().unwrap(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(read["sha256"], export["sha256"]);
    let destination = temp.path().join("retained.step");
    frozen.save_to(&destination).unwrap();
    assert!(
        std::fs::read(destination)
            .unwrap()
            .starts_with(b"ISO-10303-21;")
    );
    drop(pin);
    assert_eq!(collect(&core).await.removed_mesh_buffers, 1);
    assert!(!directory.join(reading).exists());
    #[cfg(unix)]
    assert_eq!(
        std::fs::read(temp.path().join("outside.bin")).unwrap(),
        b"outside cache authority"
    );
    drop(core);
    let core = Core::open(root, config()).await.unwrap();
    let reopened = core.get_project(&ws).await.unwrap();
    for key in [
        "revision",
        "model",
        "parameters",
        "activity",
        "recordings",
        "dirty",
        "files",
        "assets",
    ] {
        assert_eq!(reopened[key], before[key], "{key}");
    }
    // Runtime pose versions reset on restart; saved recording data does not.
    assert_eq!(reopened["motion"]["pose_version"], 0);
    assert_eq!(reopened["motion"]["poses"], before["motion"]["poses"]);
    assert_ne!(reopened["display_generation"], before["display_generation"]);
    verify_buffers(&core, &ws, &before).await;
    // Repairing byte-identical descriptors must still publish a new epoch.
    let descriptors = handles(&reopened);
    std::fs::remove_file(directory.join(descriptors.iter().next().unwrap())).unwrap();
    let job = core
        .repair_display(
            "cache-buffer-test",
            cadpad_core::recovery::RepairDisplay {
                workspace_id: ws.clone(),
                revision: rev.clone(),
            },
        )
        .await
        .unwrap();
    let job = terminal(&core, &ws, &job).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    let repaired = core.get_project(&ws).await.unwrap();
    assert_eq!(handles(&repaired), descriptors);
    assert_ne!(
        repaired["display_generation"],
        reopened["display_generation"]
    );
    assert_eq!(repaired["motion"], reopened["motion"]);
    assert_eq!(repaired["recordings"], reopened["recordings"]);
    verify_buffers(&core, &ws, &repaired).await;
}

#[tokio::test]
async fn current_buffer_cleanup_waits_for_native_remesh_publication_without_blocking_context_reads()
{
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let ws = core.create_workspace("cache-buffer-test").await.unwrap();
    let source = r#"import cadquery as cq
import struct, sys, time
from pathlib import Path
from cadpad_worker.model import Model
original_mesh = Model.mesh
cache = Path(sys.argv[sys.argv.index('--cache') + 1])
def controlled_mesh(self, *args, **kwargs):
    result = original_mesh(self, *args, **kwargs)
    if (cache / 'arm-remesh').exists():
        data = bytearray((cache / result['buffers']['normals']['handle']).read_bytes())
        # +0 and -0 have identical geometric meaning but distinct buffer hashes.
        for offset in range(0, len(data), 4):
            if data[offset:offset+4] == b'\0\0\0\0':
                struct.pack_into('<I', data, offset, 0x80000000)
                break
        else:
            raise RuntimeError('Expected a zero component in box normals')
        result['buffers']['normals'] = self.artifact(bytes(data), '.bin')
        (cache / 'remesh-entered').write_text(result['buffers']['normals']['handle'])
        deadline = time.monotonic() + 30
        while not (cache / 'release-remesh').exists():
            if time.monotonic() > deadline:
                raise RuntimeError('Timed out waiting for cache publication test')
            time.sleep(0.01)
    return result
Model.mesh = controlled_mesh
show(cq.Workplane().box(10,20,6), name='part')
"#
    .to_owned();
    let rev = build(&core, &ws, source.clone()).await;
    let before = core.get_project(&ws).await.unwrap();
    let directory = root.join("artifacts").join(&ws).join(&rev);
    let missing = before["model"]["parts"][0]["mesh"]["buffers"]["normals"]["handle"]
        .as_str()
        .unwrap();
    std::fs::remove_file(directory.join(missing)).unwrap();
    std::fs::write(directory.join("arm-remesh"), b"arm").unwrap();
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, b"release");
        }
    }
    let release = Release(directory.join("release-remesh"));
    let job = core
        .repair_display(
            "cache-buffer-test",
            cadpad_core::recovery::RepairDisplay {
                workspace_id: ws.clone(),
                revision: rev.clone(),
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !directory.join("remesh-entered").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pending = std::fs::read_to_string(directory.join("remesh-entered")).unwrap();
    assert!(!handles(&before).contains(&pending));
    assert!(directory.join(&pending).is_file());
    let collector = core.clone();
    let mut task = tokio::spawn(async move { collect(&collector).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut task)
            .await
            .is_err(),
        "Collection must wait for pending buffer roots"
    );
    tokio::time::timeout(Duration::from_secs(1), core.get_motion(&ws, &rev))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        core.read_source(&ws, &rev, "model.py").await.unwrap()["source"],
        source
    );
    // The old committed descriptor is still readable during repair publication.
    assert!(!core.artifact(&ws, &rev, missing).await.unwrap().is_empty());
    drop(release);
    let reader = core.clone();
    let reader_ws = ws.clone();
    let context_reads = tokio::spawn(async move {
        let mut generations = HashSet::new();
        for _ in 0..64 {
            let project = reader.get_project(&reader_ws).await.unwrap();
            assert!(
                project["display_error"].is_null(),
                "A checked old epoch must not taint new metadata: {}",
                project["display_error"]
            );
            generations.insert(project["display_generation"].as_str().unwrap().to_owned());
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        generations
    });
    let repaired = terminal(&core, &ws, &job).await;
    assert_eq!(
        repaired.state,
        JobState::Succeeded,
        "{:?}",
        repaired.diagnostics
    );
    let report = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    assert!(report.errors.is_empty());
    assert_eq!(report.removed_mesh_buffers, 1, "{report:?}");
    assert!(!directory.join(missing).exists());
    let after = core.get_project(&ws).await.unwrap();
    assert_eq!(after["revision"], rev);
    assert_eq!(after["motion"], before["motion"]);
    assert_eq!(after["parameters"], before["parameters"]);
    assert_ne!(after["display_generation"], before["display_generation"]);
    assert!(
        context_reads
            .await
            .unwrap()
            .contains(after["display_generation"].as_str().unwrap())
    );
    assert!(handles(&after).contains(&pending));
    assert!(directory.join(pending).exists());
    verify_buffers(&core, &ws, &after).await;
    let exact = core
        .query(
            &ws,
            &rev,
            "inspect",
            json!({"entities":[{"revision":rev,"instance_id":"part","entity_id":"part:0"}]}),
        )
        .await
        .unwrap();
    assert!((exact["entities"][0]["volume"].as_f64().unwrap() - 1200.).abs() < 1e-8);
}

#[tokio::test]
async fn display_budget_evicts_cold_publications_preserves_pinned_readers_and_recovers_explicitly()
{
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let a = core.create_workspace("budget-test").await.unwrap();
    let ar = build(
        &core,
        &a,
        "import cadquery as cq\nshow(cq.Workplane().box(10,20,6),name='part')\n".into(),
    )
    .await;
    let b = core.create_workspace("budget-test").await.unwrap();
    let br = build(
        &core,
        &b,
        "import cadquery as cq\nshow(cq.Workplane().box(10,20,12),name='part')\n".into(),
    )
    .await;
    for (version, action) in [(0, "start_recording"), (1, "finish_recording")] {
        let mut command = json!({"action":action});
        if action == "start_recording" {
            command["name"] = json!("Keep this recording");
        }
        core.control_playback("budget-test",serde_json::from_value(json!({"workspace_id":a,"revision":ar,"expected_pose_version":version,"command":command})).unwrap()).await.unwrap();
    }
    let before_a = core.get_project(&a).await.unwrap();
    let before_b = core.get_project(&b).await.unwrap();
    let ah = handles(&before_a);
    let bh = handles(&before_b);
    let ad = root.join("artifacts").join(&a).join(&ar);
    let bd = root.join("artifacts").join(&b).join(&br);
    let bytes = |directory: &Path, names: &HashSet<String>| {
        names
            .iter()
            .map(|name| std::fs::metadata(directory.join(name)).unwrap().len())
            .sum::<u64>()
    };
    let a_bytes = bytes(&ad, &ah);
    let b_bytes = bytes(&bd, &bh);
    assert!(a_bytes > 0 && b_bytes > 0);
    let journals = [&a, &b]
        .map(|ws| std::fs::read(root.join("workspaces").join(ws).join("workspace.json")).unwrap());
    let blobs = std::fs::read_dir(root.join("blobs"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.path(), std::fs::read(entry.path()).unwrap())
        })
        .collect::<Vec<_>>();
    assert!(!blobs.is_empty());
    let export = core
        .query(&a, &ar, "export", json!({"format":"STEP","pose_version":2}))
        .await
        .unwrap();
    let frozen = core
        .freeze_export(&a, &ar, export["handle"].as_str().unwrap())
        .await
        .unwrap();
    // Actual artifact use makes A newer than B; the directory lease held by the
    // export protects exports but does not prohibit collecting mesh-only data.
    core.artifact(&a, &ar, ah.iter().next().unwrap())
        .await
        .unwrap();
    let pin = std::fs::File::open(ad.join(ah.iter().next().unwrap())).unwrap();
    pin.try_lock_shared().unwrap();
    let mut events = core.subscribe_activity();
    let report = core
        .collect_cache_with_display_target(a_bytes)
        .await
        .unwrap();
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.display_bytes_before, a_bytes + b_bytes);
    assert_eq!(report.display_bytes_after, a_bytes);
    assert_eq!(report.evicted_display_revisions, 1);
    assert!(!report.display_budget_exceeded);
    let event = events.recv().await.unwrap();
    assert_eq!(event.kind, "refresh");
    assert_eq!(event.workspace_id, b);
    assert!(bh.iter().all(|name| !bd.join(name).exists()));
    verify_buffers(&core, &a, &before_a).await;
    let missing_b = core.get_project(&b).await.unwrap();
    assert_eq!(missing_b["display_error"]["code"], "DISPLAY_CACHE_MISSING");
    assert_ne!(
        missing_b["display_generation"],
        before_b["display_generation"]
    );
    let report = core.collect_cache_with_display_target(0).await.unwrap();
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.display_bytes_after, a_bytes);
    assert!(report.display_budget_exceeded);
    assert_eq!(report.evicted_display_revisions, 0);
    assert!(ah.iter().all(|name| ad.join(name).exists()));
    drop(pin);
    let report = core.collect_cache_with_display_target(0).await.unwrap();
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.display_bytes_after, 0);
    assert!(!report.display_budget_exceeded);
    assert_eq!(report.evicted_display_revisions, 1);
    assert!(ah.iter().all(|name| !ad.join(name).exists()));
    for (index, ws) in [&a, &b].iter().enumerate() {
        assert_eq!(
            std::fs::read(root.join("workspaces").join(ws).join("workspace.json")).unwrap(),
            journals[index]
        );
    }
    for (path, content) in &blobs {
        assert_eq!(&std::fs::read(path).unwrap(), content);
    }
    let saved = temp.path().join("retained.step");
    frozen.save_to(&saved).unwrap();
    assert!(std::fs::read(saved).unwrap().starts_with(b"ISO-10303-21;"));
    let export_read = core
        .export_resource(
            "budget-test",
            &a,
            &ar,
            export["handle"].as_str().unwrap(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(export_read["sha256"], export["sha256"]);
    let missing_a = core.get_project(&a).await.unwrap();
    assert_eq!(missing_a["display_error"]["code"], "DISPLAY_CACHE_MISSING");
    assert_ne!(
        missing_a["display_generation"],
        before_a["display_generation"]
    );
    for key in ["revision", "model", "parameters", "recordings", "motion"] {
        assert_eq!(missing_a[key], before_a[key], "{key}");
    }
    drop(core);
    let core = Core::open(root.clone(), config()).await.unwrap();
    let reopened = core.get_project(&a).await.unwrap();
    assert_eq!(reopened["display_error"]["code"], "DISPLAY_CACHE_MISSING");
    assert_eq!(reopened["recordings"], before_a["recordings"]);
    let inspected=core.query(&a,&ar,"inspect",json!({"pose_version":0,"entities":[{"revision":ar,"instance_id":"part","entity_id":"part:0"}]})).await.unwrap();
    assert!((inspected["entities"][0]["volume"].as_f64().unwrap() - 1200.).abs() < 1e-8);
    assert!(
        ah.iter().all(|name| !ad.join(name).exists()),
        "Exact recovery must not silently tessellate"
    );
    let job = core
        .repair_display(
            "budget-test",
            cadpad_core::recovery::RepairDisplay {
                workspace_id: a.clone(),
                revision: ar.clone(),
            },
        )
        .await
        .unwrap();
    let job = terminal(&core, &a, &job).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    let repaired = core.get_project(&a).await.unwrap();
    assert!(repaired["display_error"].is_null());
    assert_eq!(repaired["revision"], ar);
    assert_eq!(handles(&repaired), ah);
    assert_eq!(repaired["recordings"], before_a["recordings"]);
    verify_buffers(&core, &a, &repaired).await;
}

#[tokio::test]
async fn hard_disk_quota_admits_under_budget_and_rejects_after_managed_data_fills_it() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open_with_disk_quota(temp.path().join("app"), config(), 1)
        .await
        .unwrap();
    let empty = core.disk_usage().unwrap();
    assert_eq!(empty.managed_bytes, 0);
    assert!(empty.within_quota);
    assert_eq!(empty.quota_bytes, 1);
    let ws = core.create_workspace("quota-test").await.unwrap();
    let first = build(
        &core,
        &ws,
        "import cadquery as cq\nshow(cq.Workplane().box(10,10,10),name='block')".into(),
    )
    .await;
    let filled = core.disk_usage().unwrap();
    assert!(filled.managed_bytes > 1);
    assert!(!filled.within_quota);
    assert!(filled.artifacts_bytes > 0);
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(base, first);
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([(
                "model.py".into(),
                "import cadquery as cq\nshow(cq.Workplane().box(12,12,12),name='block')".into(),
            )]),
            "quota-test",
        )
        .await
        .unwrap();
    let job = core
        .build(&ws, &base, &draft.id, "quota-test")
        .await
        .unwrap();
    let job = terminal(&core, &ws, &job).await;
    assert_eq!(job.state, JobState::Failed, "{:?}", job.diagnostics);
    assert_eq!(job.diagnostics["code"], "DISK_QUOTA_EXCEEDED");
    assert_eq!(
        core.get_project(&ws).await.unwrap()["revision"],
        first
    );
    let still = core.disk_usage().unwrap();
    assert!(!still.within_quota);
    assert_eq!(still.quota_bytes, 1);
}
