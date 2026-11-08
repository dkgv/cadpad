use cadpad_core::{Core, Job, JobState, kinematics, tools::ToolContext, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
fn config() -> WorkerConfig {
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
    .uncontained_for_trusted_tests()
}
fn context() -> ToolContext {
    ToolContext {
        actor: "external:linkage".into(),
    }
}
fn definition() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/four_bar.mechanism.json")).unwrap()
}
async fn terminal(core: &Core, ws: &str, job: Job) -> Job {
    tokio::time::timeout(Duration::from_secs(90), async {
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
async fn done(core: &Core, ws: &str, job: Job) -> String {
    let job = terminal(core, ws, job).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    job.resulting_revision.unwrap()
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
                include_str!("../../../fixtures/four_bar.py").into(),
            )]),
            "user",
        )
        .await
        .unwrap();
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            kinematics::decode(&definition()).unwrap(),
            "user",
        )
        .await
        .unwrap();
    let revision = done(
        core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    (ws, revision)
}
async fn drive(core: &Core, ws: &str, rev: &str, version: u64, target: f64) -> Value {
    core.dispatch(&context(),"set_joint_coordinate",json!({"workspace_id":ws,"revision":rev,"expected_pose_version":version,"joint_id":"crank","coordinate":target,"unit":"rad"})).await.unwrap()
}
async fn drive_preview(core: &Core, ws: &str, rev: &str, version: u64, target: f64) -> Value {
    core.dispatch(&context(),"set_joint_coordinate",json!({"workspace_id":ws,"revision":rev,"expected_pose_version":version,"joint_id":"crank","coordinate":target,"unit":"rad","check_collision":false})).await.unwrap()
}
#[tokio::test]
async fn closed_linkage_moves_without_rebuilding_and_exact_bore_centers_follow_the_closure() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, revision) = model(&core).await;
    let before = core.get_project(&ws).await.unwrap();
    let result = drive(&core, &ws, &revision, 0, 1.4).await;
    assert_eq!(result["collision"]["blocked"], false);
    assert_eq!(result["snapshot"]["constraints"]["path_certified"], true);
    assert!(
        result["snapshot"]["constraints"]["position_error_m"]
            .as_f64()
            .unwrap()
            < 1e-6
    );
    assert_eq!(result["snapshot"]["joints"][1]["drivable"], false);
    let mut entities = vec![];
    for name in ["coupler", "rocker"] {
        let part = before["model"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|part| part["instance_id"] == name)
            .unwrap();
        assert_eq!(
            part["features"]["end_bore"].as_array().unwrap().len(),
            1,
            "{part}"
        );
        entities.push(json!({"revision":revision,"instance_id":name,"entity_id":part["features"]["end_bore"][0]}));
    }
    let measured = core
        .query(
            &ws,
            &revision,
            "inspect",
            json!({"entities":entities,"pose_version":1}),
        )
        .await
        .unwrap();
    let a = &measured["entities"][0]["world_center"];
    let b = &measured["entities"][1]["world_center"];
    let distance = ((a[0].as_f64().unwrap() - b[0].as_f64().unwrap()).powi(2)
        + (a[1].as_f64().unwrap() - b[1].as_f64().unwrap()).powi(2))
    .sqrt();
    assert!(distance < 0.001, "{measured}");
    assert_eq!(
        core.get_project(&ws).await.unwrap()["activity"],
        before["activity"]
    );
    let blocked = drive(&core, &ws, &revision, 1, 2.2).await;
    assert_eq!(blocked["collision"]["blocked"], true);
    assert_eq!(blocked["collision"]["trajectory_enclosure_used"], true);
    assert!(
        blocked["applied"].as_f64().unwrap() > 1.4 && blocked["applied"].as_f64().unwrap() < 2.2
    );
    let checked=core.dispatch(&context(),"check_interference",json!({"workspace_id":ws,"revision":revision,"pose_version":2,"pairs":[["rocker","stop"]]})).await.unwrap();
    assert_eq!(checked["exact"]["pairs"][0]["interfering"], false);
    assert!(
        checked["exact"]["pairs"][0]["distance_mm"]
            .as_f64()
            .unwrap()
            > 0.009
    );
    assert!(
        core.get_motion(&ws, &revision)
            .await
            .unwrap()
            .evaluation
            .constraints
            .unwrap()
            .max_normalized_residual
            < 0.501
    );
}
#[tokio::test]
async fn interactive_preview_skips_swept_collision_then_commit_blocks_from_the_validated_pose() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, revision) = model(&core).await;
    let preview = drive_preview(&core, &ws, &revision, 0, 2.2).await;
    assert!(preview["collision"].is_null());
    assert_eq!(preview["snapshot"]["collision_checked"], false);
    assert_eq!(preview["snapshot"]["constraints"]["path_certified"], false);
    assert!((preview["applied"].as_f64().unwrap() - 2.2).abs() < 1e-12);
    let commit = drive(&core, &ws, &revision, 1, 2.2).await;
    assert_eq!(commit["collision"]["blocked"], true);
    assert!(commit["applied"].as_f64().unwrap() < 2.2);
    assert_eq!(commit["snapshot"]["collision_checked"], true);
    assert_eq!(commit["snapshot"]["constraints"]["path_certified"], true);
}
#[tokio::test]
async fn long_interactive_drags_commit_without_path_budget_errors() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, revision) = model(&core).await;
    let mut version = 0u64;
    for target in [2.0_f64, 3.5, 5.0, 6.5, 7.0] {
        let preview = drive_preview(&core, &ws, &revision, version, target).await;
        version = preview["snapshot"]["pose_version"].as_u64().unwrap();
        assert_eq!(preview["snapshot"]["collision_checked"], false);
    }
    let commit = drive(&core, &ws, &revision, version, 7.0).await;
    assert_eq!(commit["collision"]["blocked"], true);
    assert!(commit["applied"].as_f64().unwrap() < 7.0);
    assert_eq!(commit["snapshot"]["collision_checked"], true);
}
#[tokio::test]
async fn invalid_residual_definitions_and_passive_writes_preserve_the_committed_scene() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let (ws, revision) = model(&core).await;
    let error=core.dispatch(&context(),"set_joint_coordinate",json!({"workspace_id":ws,"revision":revision,"expected_pose_version":0,"joint_id":"rocker","coordinate":2,"unit":"rad"})).await.unwrap_err();
    assert_eq!(error.code(), "INVALID_CONSTRAINT");
    assert_eq!(
        core.get_motion(&ws, &revision).await.unwrap().pose_version,
        0
    );
    let mut invalid = definition();
    invalid["constraints"]["equations"][0]["b"]["instance_id"] = json!("missing");
    let job = core
        .dispatch(
            &context(),
            "define_mechanism",
            json!({"workspace_id":ws,"expected_base_revision":revision,"definition":invalid}),
        )
        .await
        .unwrap();
    let job = terminal(&core, &ws, serde_json::from_value(job).unwrap()).await;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.diagnostics["code"], "INVALID_CONSTRAINT");
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
}
#[tokio::test]
async fn source_history_and_portable_reopen_reconstruct_the_selected_initial_branch() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, revision) = model(&core).await;
    drive(&core, &ws, &revision, 0, 1.4).await;
    let directory = temp.path().join("portable");
    core.save_project(&ws, &revision, Some(directory.clone()))
        .await
        .unwrap();
    let other = Core::open(temp.path().join("other"), config())
        .await
        .unwrap();
    let opened = other.open_project(directory, "user").await.unwrap();
    let revision = done(
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
    let motion = other
        .get_motion(&opened.workspace_id, &revision)
        .await
        .unwrap();
    assert_eq!(motion.pose_version, 0);
    assert_eq!(motion.evaluation.joints[0].coordinate, 0.8);
    assert!(motion.evaluation.joints[2].coordinate > 0.);
    assert!(motion.evaluation.constraints.unwrap().path_certified);
}
