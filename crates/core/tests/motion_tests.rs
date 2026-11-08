use cadpad_core::{
    Inputs, Revision,
    motion_tests::{Definition, execute},
};
use serde_json::json;

fn fixture() -> (Revision, Definition) {
    // Engine-only synthetic coverage. Packaged CAD coverage is a separate check.
    let mechanism = json!({"joints":[
        {"id":"lever_hinge","parent":null,"child":"lever","parent_frame":{"position_m":[0,0,0]},"child_frame":{"position_m":[-0.04,0,0]},"kind":{"type":"revolute","axis":[0,0,1]},"initial":0,"limits":[0,1]},
        {"id":"shoe_slide","parent":null,"child":"shoe","parent_frame":{"position_m":[0.075,0.012,0]},"kind":{"type":"prismatic","axis":[0,1,0]},"initial":0,"limits":[0,0.04]}
    ],"collision":{"bodies":{"lever":[{"shape":{"type":"bounds"}}],"shoe":[{"shape":{"type":"bounds"}}]},"clearance_m":0,"backoff_m":0.000001}});
    let revision = Revision {
        id: uuid::Uuid::new_v4().to_string(),
        parent: None,
        actor: "test".into(),
        restored_from: None,
        inputs: Inputs {
            mechanism,
            ..Inputs::default()
        },
        model: json!({"parts":[
            {"instance_id":"lever","position":[40,0,0],"bounds":{"min":[-40,-5,-5],"max":[40,5,5]},"volume":8000},
            {"instance_id":"shoe","position":[75,12,0],"bounds":{"min":[-10,-5,-5],"max":[10,5,5]},"volume":2000}
        ],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["lever","shoe"]}}),
    };
    let definition:Definition=serde_json::from_value(json!({
        "name":"Lever pushes shoe","intent":"Lever must reach and push the shoe by at least 5 mm",
        "initial_coordinates":{"lever_hinge":0,"shoe_slide":0},
        "simulation":{"gravity_m_s2":[0,0,0],"bodies":{
            "lever":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.2},"friction":0,"restitution":0},
            "shoe":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.1},"friction":0,"restitution":0}
        }},
        "actions":[{"ticks":120,"motors":[{"joint_id":"lever_hinge","target":0.3,"unit":"rad","stiffness":0.5,"damping":0.03,"max_force":0.2}],"loads":{"shoe":{"force_n":[0,-0.5,0],"torque_nm":[0,0,0]}}}],
        "checks":[{"type":"push","pusher":"lever","receiver":"shoe","driver_joint":"lever_hinge","direction":[0,1,0],"minimum_mm":5,"minimum_additional_mm":5,"minimum_sampled_impulse_ns":0.000001}]
    })).unwrap();
    (revision, definition)
}
#[test]
fn lever_contact_push_is_repeatable_and_missing_contact_or_insufficient_travel_fails() {
    let (revision, definition) = fixture();
    let (_sender, cancel) = tokio::sync::watch::channel(false);
    let first = execute(&definition, &revision, &cancel).unwrap();
    assert!(
        first.outcome.passed,
        "{:?} initial {:?} final {:?}",
        first.outcome,
        first.samples.first(),
        first.samples.last()
    );
    assert_eq!(first.samples.len(), 121);
    let second = execute(&definition, &revision, &cancel).unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap()
    );
    let mut short = definition.clone();
    short.actions[0].motors[0].target = 0.001;
    let failed = execute(&short, &revision, &cancel).unwrap();
    assert!(!failed.outcome.passed, "{:?}", failed.outcome);
    let mut separated = revision.clone();
    separated.inputs.mechanism["joints"][1]["parent_frame"]["position_m"] = json!([0.2, 0.012, 0]);
    separated.model["parts"][1]["position"] = json!([200, 12, 0]);
    let failed = execute(&definition, &separated, &cancel).unwrap();
    assert!(!failed.outcome.passed, "{:?}", failed.outcome);
    assert_eq!(failed.outcome.measurements[2].actual, 0.);
}
#[test]
fn unrelated_receiver_motor_cannot_substitute_for_the_selected_driver() {
    let (revision, mut definition) = fixture();
    let (_sender, cancel) = tokio::sync::watch::channel(false);
    let mut shoe = definition.actions[0].motors[0].clone();
    shoe.joint_id = "shoe_slide".into();
    shoe.target = 0.03;
    shoe.unit = "m".into();
    shoe.stiffness = 100.;
    shoe.damping = 1.;
    shoe.max_force = 10.;
    definition.actions[0].motors.push(shoe);
    let run = execute(&definition, &revision, &cancel).unwrap();
    assert!(!run.outcome.passed, "{:?}", run.outcome);
    assert!(!run.outcome.measurements[1].passed);
}
#[test]
fn cancelled_missing_reference_and_unbounded_definitions_never_return_a_pass() {
    let (revision, mut definition) = fixture();
    let (_sender, cancel) = tokio::sync::watch::channel(true);
    assert_eq!(
        execute(&definition, &revision, &cancel).unwrap_err().code(),
        "CANCELLED"
    );
    let (_sender, cancel) = tokio::sync::watch::channel(false);
    definition.initial_coordinates.remove("shoe_slide");
    assert_eq!(
        execute(&definition, &revision, &cancel).unwrap_err().code(),
        "INVALID_MOTION_TEST"
    );
    definition.actions[0].ticks = 1201;
    assert!(definition.validate().is_err());
}

fn config() -> cadpad_core::worker::WorkerConfig {
    if let Some(resources) = std::env::var_os("CADPAD_TEST_PACKAGED_RESOURCES") {
        assert_eq!(std::env::var("PATH").as_deref(), Ok(""));
        return cadpad_core::runtime::PackagedRuntime::open(std::path::Path::new(&resources))
            .unwrap()
            .worker_config();
    }
    cadpad_core::worker::WorkerConfig::development(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
}
async fn built(core: &cadpad_core::Core, ws: &str, rev: &str, draft: &str) -> String {
    let job = core.build(ws, rev, draft, "test").await.unwrap();
    let job = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let result = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(
                result.state,
                cadpad_core::JobState::Queued | cadpad_core::JobState::Running
            ) {
                break result;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        job.state,
        cadpad_core::JobState::Succeeded,
        "{:?}",
        job.diagnostics
    );
    job.resulting_revision.unwrap()
}
async fn command(
    core: &cadpad_core::Core,
    ws: &str,
    rev: &str,
    catalog: Option<&str>,
    command: serde_json::Value,
) -> cadpad_core::Result<serde_json::Value> {
    core.dispatch(
        &cadpad_core::tools::ToolContext {
            actor: "test".into(),
        },
        "control_motion_tests",
        json!({"workspace_id":ws,"revision":rev,"expected_catalog":catalog,"command":command}),
    )
    .await
}
#[tokio::test]
async fn saved_test_tools_preserve_live_motion_replay_measured_poses_and_round_trip_portably() {
    let temp = tempfile::tempdir().unwrap();
    let core = cadpad_core::Core::open(temp.path().join("app-a"), config())
        .await
        .unwrap();
    let ws = core.create_workspace("test").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let (fixture, definition) = fixture();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            std::collections::BTreeMap::from([(
                "model.py".into(),
                include_str!("../../../fixtures/lever_shoe.py").into(),
            )]),
            "test",
        )
        .await
        .unwrap();
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            cadpad_core::kinematics::decode(&fixture.inputs.mechanism).unwrap(),
            "test",
        )
        .await
        .unwrap();
    let rev = built(&core, &ws, &base, &draft.id).await;
    let listed = command(&core, &ws, &rev, None, json!({"action":"list"}))
        .await
        .unwrap();
    let saved = command(
        &core,
        &ws,
        &rev,
        listed["catalog"].as_str(),
        json!({"action":"upsert","id":"lever-push","definition":definition}),
    )
    .await
    .unwrap();
    let outdated = command(
        &core,
        &ws,
        &rev,
        listed["catalog"].as_str(),
        json!({"action":"delete","id":"lever-push"}),
    )
    .await
    .unwrap_err();
    assert_eq!(outdated.code(), "REVISION_CONFLICT");
    core.set_coordinate(
        "test",
        cadpad_core::motion::SetCoordinate {
            workspace_id: ws.clone(),
            revision: rev.clone(),
            expected_pose_version: 0,
            joint_id: "lever_hinge".into(),
            coordinate: 0.01,
            unit: "rad".into(),
            check_collision: true,
        },
    )
    .await
    .unwrap();
    let before = core.get_motion(&ws, &rev).await.unwrap();
    let run = command(
        &core,
        &ws,
        &rev,
        saved["catalog"].as_str(),
        json!({"action":"run","id":"lever-push"}),
    )
    .await
    .unwrap();
    assert_eq!(run["tests"][0]["outcome"]["passed"], true, "{run}");
    let after = core.get_motion(&ws, &rev).await.unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap()
    );
    let replay=command(&core,&ws,&rev,run["catalog"].as_str(),json!({"action":"replay","id":"lever-push","frame_index":120,"expected_pose_version":before.pose_version})).await.unwrap();
    assert!(
        replay["snapshot"]["poses"]["shoe"]["position"][1]
            .as_f64()
            .unwrap()
            > 17.
    );
    let pose_version = replay["snapshot"]["pose_version"].as_u64().unwrap();
    let inspected=core.dispatch(&cadpad_core::tools::ToolContext{actor:"test".into()},"inspect",json!({"workspace_id":ws,"revision":rev,"pose_version":pose_version,"entities":[{"revision":rev,"instance_id":"shoe","entity_id":"part:0"}]})).await.unwrap();
    assert!(
        inspected["entities"][0]["world_bounds"]["min"][1]
            .as_f64()
            .unwrap()
            > 12.,
        "{inspected}"
    );
    core.dispatch(&cadpad_core::tools::ToolContext{actor:"test".into()},"control_playback",json!({"workspace_id":ws,"revision":rev,"expected_pose_version":pose_version,"command":{"action":"leave"}})).await.unwrap();
    let restored = core.get_motion(&ws, &rev).await.unwrap();
    assert_eq!(
        serde_json::to_value(&before.evaluation).unwrap(),
        serde_json::to_value(&restored.evaluation).unwrap()
    );
    let directory = temp.path().join("portable.cadpad");
    core.save_project(&ws, &rev, Some(directory.clone()))
        .await
        .unwrap();
    assert_eq!(core.get_project(&ws).await.unwrap()["dirty"], false);
    let other = cadpad_core::Core::open(temp.path().join("app-b"), config())
        .await
        .unwrap();
    let opened = other.open_project(directory.clone(), "test").await.unwrap();
    assert_eq!(
        other.get_project(&opened.workspace_id).await.unwrap()["dirty"],
        false
    );
    let imported = command(
        &other,
        &opened.workspace_id,
        &opened.base_revision,
        None,
        json!({"action":"list"}),
    )
    .await
    .unwrap();
    assert_eq!(
        imported["tests"][0]["definition"]["name"],
        "Lever pushes shoe"
    );
    assert!(imported["tests"][0]["outcome"].is_null());
    let rev_b = built(
        &other,
        &opened.workspace_id,
        &opened.base_revision,
        &opened.draft.id,
    )
    .await;
    let repeat = command(
        &other,
        &opened.workspace_id,
        &rev_b,
        imported["catalog"].as_str(),
        json!({"action":"run","id":"lever-push"}),
    )
    .await
    .unwrap();
    assert_eq!(repeat["tests"][0]["outcome"]["passed"], true, "{repeat}");
    let folder = temp.path().join("ordinary-project");
    other
        .save_project_folder(&opened.workspace_id, &rev_b, Some(folder.clone()))
        .await
        .unwrap();
    let folder_opened = other.open_project(folder.clone(), "test").await.unwrap();
    let folder_tests = command(
        &other,
        &folder_opened.workspace_id,
        &folder_opened.base_revision,
        None,
        json!({"action":"list"}),
    )
    .await
    .unwrap();
    assert_eq!(
        folder_tests["tests"][0]["definition"]["name"],
        "Lever pushes shoe"
    );
    assert!(!folder.join("revisions").exists());
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("project.json")).unwrap()).unwrap();
    let digest = manifest["motion_tests"].as_str().unwrap();
    std::fs::write(directory.join(format!("motion-tests/{digest}.json")), b"{}").unwrap();
    assert!(other.open_project(directory.clone(), "test").await.is_err());
    assert!(core.save_project(&ws, &rev, None).await.is_err());
}

#[tokio::test]
async fn queued_test_stop_remains_available_during_two_entered_cad_builds_and_preserves_previous_results()
 {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("app");
    let core = cadpad_core::Core::open(app.clone(), config())
        .await
        .unwrap();
    let ws = core.create_workspace("test").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let (reference, definition) = fixture();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            std::collections::BTreeMap::from([(
                "model.py".into(),
                include_str!("../../../fixtures/lever_shoe.py").into(),
            )]),
            "test",
        )
        .await
        .unwrap();
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            cadpad_core::kinematics::decode(&reference.inputs.mechanism).unwrap(),
            "test",
        )
        .await
        .unwrap();
    let rev = built(&core, &ws, &base, &draft.id).await;
    let empty = command(&core, &ws, &rev, None, json!({"action":"list"}))
        .await
        .unwrap();
    let saved = command(
        &core,
        &ws,
        &rev,
        empty["catalog"].as_str(),
        json!({"action":"upsert","id":"push","definition":definition}),
    )
    .await
    .unwrap();
    let seed = command(
        &core,
        &ws,
        &rev,
        saved["catalog"].as_str(),
        json!({"action":"run","id":"push"}),
    )
    .await
    .unwrap();
    assert_eq!(seed["run"]["state"], "completed");
    let before = serde_json::to_value(core.get_motion(&ws, &rev).await.unwrap()).unwrap();
    let mut jobs = Vec::new();
    for _ in 0..2 {
        let other = core.create_workspace("blocker").await.unwrap();
        let base = core.get_project(&other).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let source = "import sys,time\nfrom pathlib import Path\ncache=Path(sys.argv[sys.argv.index('--cache')+1])\n(cache/'test-capacity-entered').write_text('entered')\nwhile True: time.sleep(0.01)";
        let draft = core
            .update_source(
                &other,
                &base,
                None,
                std::collections::BTreeMap::from([("model.py".into(), source.into())]),
                "blocker",
            )
            .await
            .unwrap();
        let job = core
            .build(&other, &base, &draft.id, "blocker")
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                if std::fs::read_dir(app.join("artifacts").join(&other)).is_ok_and(|entries| {
                    entries
                        .filter_map(|entry| entry.ok())
                        .any(|entry| entry.path().join("test-capacity-entered").exists())
                }) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        jobs.push((other, job.id));
    }
    let started = command(
        &core,
        &ws,
        &rev,
        seed["catalog"].as_str(),
        json!({"action":"start_run","id":"push"}),
    )
    .await
    .unwrap();
    let exact=core.dispatch(&cadpad_core::tools::ToolContext { actor:"test".into() },"inspect",json!({"workspace_id":ws,"revision":rev,"pose_version":0,"entities":[{"revision":rev,"instance_id":"shoe","entity_id":"part:0"}]})).await;
    let stopped = command(
        &core,
        &ws,
        &rev,
        None,
        json!({"action":"cancel_run","run_id":started["run"]["id"]}),
    )
    .await;
    let terminal = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let listed = command(&core, &ws, &rev, None, json!({"action":"list"}))
                .await
                .unwrap();
            if matches!(
                listed["run"]["state"].as_str(),
                Some("cancelled" | "failed" | "completed")
            ) {
                break listed;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    // Reap both actual native builders before assertions about the experiment.
    core.revoke_actor("blocker").await;
    for (other, job) in jobs {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if !matches!(
                    core.get_job(&other, &job).await.unwrap().state,
                    cadpad_core::JobState::Queued | cadpad_core::JobState::Running
                ) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(started["run"]["state"], "queued");
    assert_eq!(
        exact.unwrap()["entities"][0]["volume"]
            .as_f64()
            .unwrap()
            .round(),
        2000.
    );
    stopped.unwrap();
    let terminal = terminal.unwrap();
    assert_eq!(terminal["run"]["state"], "cancelled");
    assert_eq!(terminal["catalog"], seed["catalog"]);
    assert_eq!(terminal["tests"], seed["tests"]);
    assert_eq!(
        serde_json::to_value(core.get_motion(&ws, &rev).await.unwrap()).unwrap(),
        before
    );
    let rerun = command(
        &core,
        &ws,
        &rev,
        terminal["catalog"].as_str(),
        json!({"action":"start_run","id":"push"}),
    )
    .await
    .unwrap();
    assert_ne!(rerun["run"]["id"], started["run"]["id"]);
    assert!(
        command(
            &core,
            &ws,
            &rev,
            None,
            json!({"action":"cancel_run","run_id":started["run"]["id"]})
        )
        .await
        .is_err()
    );
    let completed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let reply = command(&core, &ws, &rev, None, json!({"action":"list"}))
                .await
                .unwrap();
            if matches!(
                reply["run"]["state"].as_str(),
                Some("completed" | "failed" | "cancelled")
            ) {
                break reply;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(completed["run"]["state"], "completed");
    assert_eq!(completed["tests"][0]["outcome"]["passed"], true);
}
