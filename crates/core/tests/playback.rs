use cadpad_core::{Core, Job, JobState, kinematics, tools::ToolContext, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
fn config() -> WorkerConfig {
    if let Some(resources) = std::env::var_os("CADPAD_TEST_PACKAGED_RESOURCES") {
        assert_eq!(
            std::env::var("PATH").unwrap_or_default(),
            "",
            "Packaged QA requires empty PATH"
        );
        return cadpad_core::runtime::PackagedRuntime::open(&PathBuf::from(resources))
            .unwrap()
            .worker_config();
    }
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
async fn model(core: &Core) -> (String, String) {
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
                include_str!("../../../fixtures/rail_carriage.py").into(),
            )]),
            "user",
        )
        .await
        .unwrap();
    let definition: Value = serde_json::from_str(include_str!(
        "../../../fixtures/rail_carriage.mechanism.json"
    ))
    .unwrap();
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            kinematics::decode(&definition).unwrap(),
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
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    (ws, job.resulting_revision.unwrap())
}
async fn playback(
    core: &Core,
    ws: &str,
    rev: &str,
    version: u64,
    command: Value,
) -> cadpad_core::Result<Value> {
    core.dispatch(
        &ToolContext {
            actor: "user".into(),
        },
        "control_playback",
        json!({"workspace_id":ws,"revision":rev,"expected_pose_version":version,"command":command}),
    )
    .await
}
async fn simulate(core: &Core, ws: &str, rev: &str, version: u64, command: Value) -> Value {
    core.dispatch(
        &ToolContext {
            actor: "user".into(),
        },
        "control_simulation",
        json!({"workspace_id":ws,"revision":rev,"expected_pose_version":version,"command":command}),
    )
    .await
    .unwrap()
}
async fn drive(
    core: &Core,
    ws: &str,
    rev: &str,
    version: u64,
    target: f64,
) -> cadpad_core::Result<Value> {
    core.dispatch(&ToolContext{actor:"user".into()},"set_joint_coordinate",json!({"workspace_id":ws,"revision":rev,"expected_pose_version":version,"joint_id":"carriage_slide","coordinate":target,"unit":"m"})).await
}
fn start() -> Value {
    json!({"action":"start","definition":{"gravity_m_s2":[0,0,-9.81],"bodies":{"rail":{"type":"fixed","friction":0.5,"restitution":0},"carriage":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.1},"friction":0.5,"restitution":0}}}})
}

#[tokio::test]
async fn portable_recordings_move_between_clean_app_roots_and_rebind_only_verified_open_builds() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app-a"), config())
        .await
        .unwrap();
    let (ws, rev) = model(&core).await;
    let mut activity = core.subscribe_activity();
    playback(
        &core,
        &ws,
        &rev,
        0,
        json!({"action":"start_recording","name":"Portable manipulation"}),
    )
    .await
    .unwrap();
    drive(&core, &ws, &rev, 1, 0.03).await.unwrap();
    let finished = playback(&core, &ws, &rev, 2, json!({"action":"finish_recording"}))
        .await
        .unwrap();
    let manipulation_id = finished["recordings"][0]["id"].as_str().unwrap().to_owned();
    let event = std::iter::from_fn(|| activity.try_recv().ok())
        .find(|event| event.kind == "recordings_changed")
        .expect("Completed recording must notify every connected view");
    assert_eq!(event.kind, "recordings_changed");
    assert_eq!(event.workspace_id, ws);
    simulate(&core, &ws, &rev, 3, start()).await;
    playback(
        &core,
        &ws,
        &rev,
        4,
        json!({"action":"start_recording","name":"Portable simulation"}),
    )
    .await
    .unwrap();
    simulate(&core,&ws,&rev,5,json!({"action":"motor","target":{"joint_id":"carriage_slide","target":0.04,"unit":"m","stiffness":100,"damping":10,"max_force":2}})).await;
    simulate(&core, &ws, &rev, 6, json!({"action":"advance","steps":60})).await;
    let finished = playback(&core, &ws, &rev, 7, json!({"action":"finish_recording"}))
        .await
        .unwrap();
    let simulation_id = finished["recordings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["source_mode"] == "simulate")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let selected = playback(
        &core,
        &ws,
        &rev,
        8,
        json!({"action":"select","recording_id":simulation_id,"frame_index":30}),
    )
    .await
    .unwrap();
    let expected_poses = selected["snapshot"]["poses"].clone();
    let original_source = core.read_source(&ws, &rev, "model.py").await.unwrap();
    let original_model = core.get_project(&ws).await.unwrap()["model"].clone();
    let folder = temp.path().join("portable");
    core.save_project(&ws, &rev, Some(folder.clone()))
        .await
        .unwrap();
    assert_eq!(core.get_project(&ws).await.unwrap()["dirty"], false);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(folder.join("project.json")).unwrap()).unwrap();
    assert!(manifest["recordings"].as_str().unwrap().len() == 64);
    assert_eq!(manifest["schema_version"], 2);
    let moved = temp.path().join("moved-project");
    std::fs::rename(folder, &moved).unwrap();
    drop(core);
    let other = Core::open(temp.path().join("app-b"), config())
        .await
        .unwrap();
    let opened = other.open_project(moved.clone(), "user").await.unwrap();
    let unloaded = other.get_project(&opened.workspace_id).await.unwrap();
    assert_eq!(unloaded["recordings"].as_array().unwrap().len(), 2);
    assert!(unloaded["motion"]["poses"].as_object().unwrap().is_empty());
    assert!(
        !temp.path().join("app-b/runs").exists(),
        "Open must not execute source"
    );
    assert_eq!(
        playback(
            &other,
            &opened.workspace_id,
            &opened.base_revision,
            0,
            json!({"action":"select","recording_id":simulation_id,"frame_index":30})
        )
        .await
        .unwrap_err()
        .code(),
        "UNSUPPORTED_OPERATION"
    );
    let job = terminal(
        &other,
        &opened.workspace_id,
        other
            .build(
                &opened.workspace_id,
                &opened.base_revision,
                &opened.draft.id,
                "user",
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    let new_rev = job.resulting_revision.unwrap();
    assert_ne!(new_rev, rev);
    let context = other.get_project(&opened.workspace_id).await.unwrap();
    fn without_mesh(mut model: Value) -> Value {
        model.as_object_mut().unwrap().remove("revision");
        for part in model["parts"].as_array_mut().unwrap() {
            part.as_object_mut().unwrap().remove("mesh");
        }
        model
    }
    assert_eq!(
        without_mesh(original_model),
        without_mesh(context["model"].clone())
    );
    assert!(
        context["recordings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["revision"] == new_rev)
    );
    assert_eq!(
        context["dirty"], true,
        "Rebound recording metadata needs Save"
    );
    assert_eq!(
        other
            .read_source(&opened.workspace_id, &new_rev, "model.py")
            .await
            .unwrap()["source"],
        original_source["source"]
    );
    let replay = playback(
        &other,
        &opened.workspace_id,
        &new_rev,
        0,
        json!({"action":"select","recording_id":simulation_id,"frame_index":30}),
    )
    .await
    .unwrap();
    assert_eq!(replay["snapshot"]["poses"], expected_poses);
    let exact=other.query(&opened.workspace_id,&new_rev,"inspect",json!({"pose_version":1,"entities":[{"revision":new_rev,"instance_id":"carriage","entity_id":"part:0"}]})).await.unwrap();
    assert!(
        (exact["entities"][0]["world_center"][0].as_f64().unwrap()
            - expected_poses["carriage"]["position"][0].as_f64().unwrap())
        .abs()
            < 1e-6
    );
    assert!(
        other
            .query(
                &opened.workspace_id,
                &new_rev,
                "export",
                json!({"format":"STEP","pose_version":1})
            )
            .await
            .unwrap()["bytes"]
            .as_u64()
            .unwrap()
            > 1000
    );
    playback(
        &other,
        &opened.workspace_id,
        &new_rev,
        1,
        json!({"action":"leave"}),
    )
    .await
    .unwrap();
    let replay = playback(
        &other,
        &opened.workspace_id,
        &new_rev,
        2,
        json!({"action":"select","recording_id":manipulation_id,"frame_index":1}),
    )
    .await
    .unwrap();
    assert_eq!(replay["snapshot"]["joints"][0]["coordinate"], 0.03);
    playback(
        &other,
        &opened.workspace_id,
        &new_rev,
        3,
        json!({"action":"leave"}),
    )
    .await
    .unwrap();
    other
        .save_project(&opened.workspace_id, &new_rev, None)
        .await
        .unwrap();
    assert_eq!(
        other.get_project(&opened.workspace_id).await.unwrap()["dirty"],
        false
    );
    playback(
        &other,
        &opened.workspace_id,
        &new_rev,
        4,
        json!({"action":"delete","recording_id":manipulation_id}),
    )
    .await
    .unwrap();
    assert_eq!(
        other.get_project(&opened.workspace_id).await.unwrap()["dirty"],
        true
    );
    other
        .save_project(&opened.workspace_id, &new_rev, None)
        .await
        .unwrap();
    drop(other);
    let third = Core::open(temp.path().join("app-c"), config())
        .await
        .unwrap();
    let opened = third.open_project(moved, "user").await.unwrap();
    let job = terminal(
        &third,
        &opened.workspace_id,
        third
            .build(
                &opened.workspace_id,
                &opened.base_revision,
                &opened.draft.id,
                "user",
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    let final_rev = job.resulting_revision.unwrap();
    let replay = playback(
        &third,
        &opened.workspace_id,
        &final_rev,
        0,
        json!({"action":"select","recording_id":simulation_id,"frame_index":30}),
    )
    .await
    .unwrap();
    assert_eq!(replay["recordings"].as_array().unwrap().len(), 1);
    assert_eq!(replay["snapshot"]["poses"], expected_poses);
    let draft = third
        .update_source(
            &opened.workspace_id,
            &final_rev,
            None,
            BTreeMap::from([(
                "model.py".into(),
                format!(
                    "{}\n# Ordinary source rebuild\n",
                    original_source["source"].as_str().unwrap()
                ),
            )]),
            "user",
        )
        .await
        .unwrap();
    let job = terminal(
        &third,
        &opened.workspace_id,
        third
            .build(&opened.workspace_id, &final_rev, &draft.id, "user")
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(job.state, JobState::Succeeded);
    assert_eq!(
        playback(
            &third,
            &opened.workspace_id,
            &job.resulting_revision.unwrap(),
            0,
            json!({"action":"select","recording_id":simulation_id,"frame_index":0})
        )
        .await
        .unwrap_err()
        .code(),
        "STALE_ENTITY",
        "Ordinary rebuild must not silently rebind a recording"
    );
}

fn archive(folder: &std::path::Path) -> (Value, Value) {
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(folder.join("project.json")).unwrap()).unwrap();
    let index = folder.join(format!(
        ".cadpad-recordings/index/{}.json",
        manifest["recordings"].as_str().unwrap()
    ));
    let data = serde_json::from_slice(&std::fs::read(index).unwrap()).unwrap();
    (manifest, data)
}
fn replace_archive(folder: &std::path::Path, mut manifest: Value, data: &Value) {
    let bytes = serde_json::to_vec(data).unwrap();
    let digest = cadpad_core::portable::bytes_hash(&bytes);
    std::fs::write(
        folder.join(format!(".cadpad-recordings/index/{digest}.json")),
        bytes,
    )
    .unwrap();
    manifest["recordings"] = json!(digest);
    std::fs::write(
        folder.join("project.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn portable_recordings_reject_corrupt_missing_duplicate_oversized_and_escaped_archives_without_execution()
 {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app-a"), config())
        .await
        .unwrap();
    let (ws, rev) = model(&core).await;
    playback(
        &core,
        &ws,
        &rev,
        0,
        json!({"action":"start_recording","name":"Integrity"}),
    )
    .await
    .unwrap();
    drive(&core, &ws, &rev, 1, 0.02).await.unwrap();
    playback(&core, &ws, &rev, 2, json!({"action":"finish_recording"}))
        .await
        .unwrap();
    let folder = temp.path().join("portable");
    core.save_project(&ws, &rev, Some(folder.clone()))
        .await
        .unwrap();
    let (manifest, data) = archive(&folder);
    let other = Core::open(temp.path().join("app-b"), config())
        .await
        .unwrap();
    for case in ["duplicate", "oversized", "wrong_source", "wrong_revision"] {
        let mut changed = data.clone();
        match case {
            "duplicate" => {
                let entry = changed["recordings"][0].clone();
                changed["recordings"].as_array_mut().unwrap().push(entry);
            }
            "oversized" => {
                changed["recordings"][0]["metadata"]["bytes"] = json!(8 * 1024 * 1024 + 1)
            }
            "wrong_source" => {
                changed["recordings"][0]["metadata"]["input_fingerprint"] = json!("0".repeat(64))
            }
            _ => {
                changed["recordings"][0]["metadata"]["revision"] =
                    json!(uuid::Uuid::new_v4().to_string())
            }
        }
        replace_archive(&folder, manifest.clone(), &changed);
        assert!(
            other.open_project(folder.clone(), "user").await.is_err(),
            "{case}"
        );
        assert!(other.list_workspaces().await.is_empty());
    }
    replace_archive(&folder, manifest.clone(), &data);
    let payload = folder.join(format!(
        ".cadpad-recordings/blobs/{}.json",
        data["recordings"][0]["metadata"]["digest"]
            .as_str()
            .unwrap()
    ));
    let bytes = std::fs::read(&payload).unwrap();
    std::fs::write(&payload, b"corrupt").unwrap();
    assert!(other.open_project(folder.clone(), "user").await.is_err());
    std::fs::remove_file(&payload).unwrap();
    assert!(other.open_project(folder.clone(), "user").await.is_err());
    #[cfg(unix)]
    {
        let outside = temp.path().join("outside.json");
        std::fs::write(&outside, &bytes).unwrap();
        std::os::unix::fs::symlink(outside, &payload).unwrap();
        assert!(other.open_project(folder.clone(), "user").await.is_err());
        std::fs::remove_file(&payload).unwrap();
        let name = std::ffi::CString::new(payload.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(other.open_project(folder.clone(), "user").await.is_err());
        std::fs::remove_file(&payload).unwrap();
    }
    std::fs::write(&payload, bytes).unwrap();
    let mut changed = manifest.clone();
    changed["recordings"] = json!("../../outside");
    std::fs::write(
        folder.join("project.json"),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    assert!(other.open_project(folder.clone(), "user").await.is_err());
    assert!(!temp.path().join("app-b/runs").exists());
    // A changed archive pointer cannot be overwritten by a bound Save.
    replace_archive(&folder, manifest, &data);
    let (manifest, mut changed) = archive(&folder);
    changed["recordings"][0]["metadata"]["name"] = json!("External name");
    replace_archive(&folder, manifest, &changed);
    assert_eq!(
        core.save_project(&ws, &rev, None).await.unwrap_err().code(),
        "REVISION_CONFLICT"
    );
}

#[tokio::test]
async fn portable_recordings_do_not_rebind_to_changed_source_or_mismatched_geometry() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app-a"), config())
        .await
        .unwrap();
    let (ws, rev) = model(&core).await;
    playback(
        &core,
        &ws,
        &rev,
        0,
        json!({"action":"start_recording","name":"Scoped"}),
    )
    .await
    .unwrap();
    let recorded = playback(&core, &ws, &rev, 1, json!({"action":"finish_recording"}))
        .await
        .unwrap();
    let id = recorded["recordings"][0]["id"].as_str().unwrap();
    let folder = temp.path().join("portable");
    core.save_project(&ws, &rev, Some(folder.clone()))
        .await
        .unwrap();
    let (manifest, data) = archive(&folder);
    for scenario in ["source", "geometry"] {
        if scenario == "geometry" {
            let mut changed = data.clone();
            changed["recordings"][0]["model_fingerprint"] = json!("0".repeat(64));
            replace_archive(&folder, manifest.clone(), &changed);
        }
        let other = Core::open(temp.path().join(format!("app-{scenario}")), config())
            .await
            .unwrap();
        let opened = other.open_project(folder.clone(), "user").await.unwrap();
        if scenario == "source" {
            let source = format!(
                "{}\n# changed input",
                include_str!("../../../fixtures/rail_carriage.py")
            );
            other
                .update_source(
                    &opened.workspace_id,
                    &opened.base_revision,
                    Some(&opened.draft.id),
                    BTreeMap::from([("model.py".into(), source)]),
                    "user",
                )
                .await
                .unwrap();
        }
        let job = terminal(
            &other,
            &opened.workspace_id,
            other
                .build(
                    &opened.workspace_id,
                    &opened.base_revision,
                    &opened.draft.id,
                    "user",
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
        let current = job.resulting_revision.unwrap();
        assert_eq!(
            playback(
                &other,
                &opened.workspace_id,
                &current,
                0,
                json!({"action":"select","recording_id":id,"frame_index":0})
            )
            .await
            .unwrap_err()
            .code(),
            "STALE_ENTITY",
            "{scenario}"
        );
    }
}
#[tokio::test]
async fn records_each_simulation_tick_replays_exact_native_poses_exports_and_restores_live_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let (ws, rev) = model(&core).await;
    let design = core.get_project(&ws).await.unwrap();
    simulate(&core, &ws, &rev, 0, start()).await;
    playback(
        &core,
        &ws,
        &rev,
        1,
        json!({"action":"start_recording","name":"Slide motion"}),
    )
    .await
    .unwrap();
    simulate(&core,&ws,&rev,2,json!({"action":"motor","target":{"joint_id":"carriage_slide","target":0.04,"unit":"m","stiffness":100,"damping":10,"max_force":2}})).await;
    let live = simulate(&core, &ws, &rev, 3, json!({"action":"advance","steps":120})).await;
    assert_eq!(live["recording"]["frame_count"], 121);
    let recorded = playback(&core, &ws, &rev, 4, json!({"action":"finish_recording"}))
        .await
        .unwrap();
    let meta = &recorded["recordings"][0];
    let id = meta["id"].as_str().unwrap().to_string();
    assert_eq!(meta["duration_s"], 1.);
    assert_eq!(meta["revision"], rev);
    let selected = playback(
        &core,
        &ws,
        &rev,
        5,
        json!({"action":"select","recording_id":id,"frame_index":30}),
    )
    .await
    .unwrap();
    assert_eq!(selected["snapshot"]["playback"]["time_s"], 0.25);
    assert!(selected["snapshot"].get("simulation").is_none());
    assert_eq!(selected["frame_times_s"].as_array().unwrap().len(), 121);
    let x = selected["snapshot"]["poses"]["carriage"]["position"][0]
        .as_f64()
        .unwrap();
    assert!(x > 1. && x < 39.9, "{x}");
    assert_eq!(
        drive(&core, &ws, &rev, 6, 0.08).await.unwrap_err().code(),
        "MODE_CONFLICT"
    );
    let exact=core.query(&ws,&rev,"inspect",json!({"query":"properties","pose_version":6,"entities":[{"revision":rev,"instance_id":"carriage","entity_id":"part:0"}]})).await.unwrap();
    assert!((exact["entities"][0]["world_center"][0].as_f64().unwrap() - x).abs() < 1e-6);
    let exported = core
        .query(
            &ws,
            &rev,
            "export",
            json!({"format":"STEP","pose_version":6}),
        )
        .await
        .unwrap();
    assert!(exported["bytes"].as_u64().unwrap() > 1000);
    let left = playback(&core, &ws, &rev, 6, json!({"action":"leave"}))
        .await
        .unwrap();
    assert_eq!(left["snapshot"]["poses"], live["poses"]);
    assert_eq!(left["snapshot"]["simulation"]["current"]["tick"], 120);
    assert_eq!(
        core.get_project(&ws).await.unwrap()["activity"],
        design["activity"]
    );
    drop(core);
    let core = Core::open(root, config()).await.unwrap();
    let list = playback(&core, &ws, &rev, 0, json!({"action":"list"}))
        .await
        .unwrap();
    assert_eq!(list["recordings"][0]["id"], id);
    assert_eq!(list["snapshot"]["pose_version"], 0);
    let selected = playback(
        &core,
        &ws,
        &rev,
        0,
        json!({"action":"select","recording_id":id,"frame_index":30}),
    )
    .await
    .unwrap();
    assert_eq!(selected["snapshot"]["poses"]["carriage"]["position"][0], x);
    let left = playback(&core, &ws, &rev, 1, json!({"action":"leave"}))
        .await
        .unwrap();
    assert_eq!(left["snapshot"]["poses"], design["motion"]["poses"]);
}
#[tokio::test]
async fn manipulation_recordings_keep_design_geometry_and_reject_cross_workspace_stale_and_corrupt_data()
 {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let (ws, rev) = model(&core).await;
    playback(
        &core,
        &ws,
        &rev,
        0,
        json!({"action":"start_recording","name":"Three poses"}),
    )
    .await
    .unwrap();
    drive(&core, &ws, &rev, 1, 0.03).await.unwrap();
    drive(&core, &ws, &rev, 2, -0.02).await.unwrap();
    let recorded = playback(&core, &ws, &rev, 3, json!({"action":"finish_recording"}))
        .await
        .unwrap();
    let meta = &recorded["recordings"][0];
    assert_eq!(meta["frame_count"], 3);
    let id = meta["id"].as_str().unwrap();
    let selected = playback(
        &core,
        &ws,
        &rev,
        4,
        json!({"action":"select","recording_id":id,"frame_index":1}),
    )
    .await
    .unwrap();
    assert_eq!(selected["snapshot"]["joints"][0]["coordinate"], 0.03);
    assert_eq!(selected["snapshot"]["joints"][0]["drivable"], false);
    assert!(
        playback(
            &core,
            &ws,
            &rev,
            5,
            json!({"action":"select","recording_id":id,"frame_index":99})
        )
        .await
        .is_err()
    );
    assert_eq!(core.get_motion(&ws, &rev).await.unwrap().pose_version, 5);
    playback(&core, &ws, &rev, 5, json!({"action":"leave"}))
        .await
        .unwrap();
    let other = core.create_workspace("user").await.unwrap();
    let other_rev = core.get_project(&other).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        playback(
            &core,
            &other,
            &other_rev,
            0,
            json!({"action":"select","recording_id":id,"frame_index":1})
        )
        .await
        .is_err()
    );
    let digest = meta["digest"].as_str().unwrap();
    std::fs::write(root.join("blobs").join(digest), b"corrupt").unwrap();
    assert_eq!(
        playback(
            &core,
            &ws,
            &rev,
            6,
            json!({"action":"select","recording_id":id,"frame_index":0})
        )
        .await
        .unwrap_err()
        .code(),
        "INVALID_RECORDING"
    );
    assert_eq!(core.get_motion(&ws, &rev).await.unwrap().pose_version, 6);
    let mut definition: Value = serde_json::from_str(include_str!(
        "../../../fixtures/rail_carriage.mechanism.json"
    ))
    .unwrap();
    definition["joints"][0]["initial"] = json!(0.01);
    let job = core
        .define_mechanism(
            "user",
            cadpad_core::motion::DefineMechanism {
                workspace_id: ws.clone(),
                expected_base_revision: rev.clone(),
                draft_id: None,
                definition: kinematics::decode(&definition).unwrap(),
            },
        )
        .await
        .unwrap();
    let job = terminal(&core, &ws, job).await;
    assert_eq!(job.state, JobState::Succeeded);
    let new_rev = job.resulting_revision.unwrap();
    assert_eq!(
        playback(
            &core,
            &ws,
            &new_rev,
            0,
            json!({"action":"select","recording_id":id,"frame_index":0})
        )
        .await
        .unwrap_err()
        .code(),
        "STALE_ENTITY"
    );
    playback(
        &core,
        &ws,
        &new_rev,
        0,
        json!({"action":"delete","recording_id":id}),
    )
    .await
    .unwrap();
    assert!(
        playback(&core, &ws, &new_rev, 1, json!({"action":"list"}))
            .await
            .unwrap()["recordings"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn active_recording_blocks_mode_switch_and_cancel_discards_without_saving() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().to_path_buf(), config())
        .await
        .unwrap();
    let (ws, rev) = model(&core).await;
    playback(
        &core,
        &ws,
        &rev,
        0,
        json!({"action":"start_recording","name":"Cancelled"}),
    )
    .await
    .unwrap();
    assert!(
        core.dispatch(
            &ToolContext {
                actor: "user".into()
            },
            "control_simulation",
            json!({"workspace_id":ws,"revision":rev,"expected_pose_version":1,"command":start()})
        )
        .await
        .is_err()
    );
    assert_eq!(
        playback(&core, &ws, &rev, 0, json!({"action":"cancel_recording"}))
            .await
            .unwrap_err()
            .code(),
        "POSE_CONFLICT"
    );
    let cancelled = playback(&core, &ws, &rev, 1, json!({"action":"cancel_recording"}))
        .await
        .unwrap();
    assert!(cancelled["snapshot"].get("recording").is_none());
    assert!(cancelled["recordings"].as_array().unwrap().is_empty());
    core.revoke_actor("user").await;
    assert!(
        playback(
            &core,
            &ws,
            &rev,
            2,
            json!({"action":"start_recording","name":"Revoked"})
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn folder_recordings_reopen_without_a_project_revision_directory() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app-folder-a"), config())
        .await
        .unwrap();
    let (ws, rev) = model(&core).await;
    playback(
        &core,
        &ws,
        &rev,
        0,
        json!({"action":"start_recording","name":"Folder motion"}),
    )
    .await
    .unwrap();
    drive(&core, &ws, &rev, 1, 0.03).await.unwrap();
    let recorded = playback(&core, &ws, &rev, 2, json!({"action":"finish_recording"}))
        .await
        .unwrap();
    let recording_id = recorded["recordings"][0]["id"].as_str().unwrap();
    let folder = temp.path().join("design");
    core.save_project_folder(&ws, &rev, Some(folder.clone()))
        .await
        .unwrap();
    assert!(!folder.join("revisions").exists());
    assert!(!folder.join(".cadpad/revisions").exists());
    let other = Core::open(temp.path().join("app-folder-b"), config())
        .await
        .unwrap();
    let opened = other.open_project(folder.clone(), "user").await.unwrap();
    let job = terminal(
        &other,
        &opened.workspace_id,
        other
            .build(
                &opened.workspace_id,
                &opened.base_revision,
                &opened.draft.id,
                "user",
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    let revision = job.resulting_revision.unwrap();
    let replay = playback(
        &other,
        &opened.workspace_id,
        &revision,
        0,
        json!({"action":"select","recording_id":recording_id,"frame_index":1}),
    )
    .await
    .unwrap();
    assert_eq!(replay["snapshot"]["joints"][0]["coordinate"], 0.03);
    core.save_project_folder(&ws, &rev, None).await.unwrap();
}
