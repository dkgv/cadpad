use cadpad_core::{Core, Job, JobState, kinematics, tools::ToolContext, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
const SOURCE: &str = include_str!("../../../fixtures/curved_carriage.py");
fn definition() -> Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/curved_carriage.mechanism.json"
    ))
    .unwrap()
}
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
        actor: "external:track".into(),
    }
}
async fn terminal(core: &Core, ws: &str, job: Job) -> Job {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(job.state, JobState::Running | JobState::Queued) {
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
async fn model(core: &Core, source: &str, definition: Value) -> (String, String) {
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
            BTreeMap::from([("model.py".into(), source.into())]),
            "user",
        )
        .await
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
    let revision = done(
        core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    (ws, revision)
}
#[tokio::test]
async fn a_circular_carriage_checks_the_arc_and_exact_cad_gap_at_the_blocked_pose() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let (ws, revision) = model(&core, SOURCE, definition()).await;
    let project = core.get_project(&ws).await.unwrap();
    let length = project["motion"]["joints"][0]["path"]["length_m"]
        .as_f64()
        .unwrap();
    assert!((length - std::f64::consts::TAU * 0.05).abs() < 1e-14);
    let moved=core.dispatch(&context(),"set_joint_coordinate",json!({"workspace_id":ws,"revision":revision,"expected_pose_version":0,"joint_id":"track","coordinate":length,"unit":"m"})).await.unwrap();
    assert_eq!(moved["collision"]["blocked"], true);
    let coordinate = moved["applied"].as_f64().unwrap();
    assert!(coordinate > 0.065 && coordinate < 0.08, "{moved}");
    assert_eq!(
        core.get_project(&ws).await.unwrap()["activity"],
        project["activity"]
    );
    let checked=core.dispatch(&context(),"check_interference",json!({"workspace_id":ws,"revision":revision,"pose_version":1,"pairs":[["carriage","stop"]]})).await.unwrap();
    assert_eq!(checked["exact"]["pairs"][0]["interfering"], false);
    assert!(
        checked["exact"]["pairs"][0]["distance_mm"]
            .as_f64()
            .unwrap()
            >= 0.01
    );
    let snapshot = core.get_motion(&ws, &revision).await.unwrap();
    let point = snapshot.evaluation.poses["carriage"].position;
    assert!((point[0].hypot(point[1]) - 50.).abs() < 1e-8);
}
#[tokio::test]
async fn curved_world_properties_and_step_export_use_the_exact_analytic_frame() {
    let mut definition = definition();
    definition.as_object_mut().unwrap().remove("collision");
    let source =
        "import cadquery as cq\nshow(cq.Workplane().box(8,4,4),name='carriage',position=(50,0,0))";
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let (ws, revision) = model(&core, source, definition).await;
    let coordinate = std::f64::consts::FRAC_PI_4 * 0.05;
    core.dispatch(&context(),"set_joint_coordinate",json!({"workspace_id":ws,"revision":revision,"expected_pose_version":0,"joint_id":"track","coordinate":coordinate,"unit":"m"})).await.unwrap();
    let inspected=core.query(&ws,&revision,"inspect",json!({"entities":[{"revision":revision,"instance_id":"carriage","entity_id":"part:0"}],"pose_version":1})).await.unwrap();
    let entity = &inspected["entities"][0];
    let center = 50. * std::f64::consts::FRAC_1_SQRT_2;
    for (actual, expected) in entity["world_center"]
        .as_array()
        .unwrap()
        .iter()
        .zip([center, center, 0.])
    {
        assert!((actual.as_f64().unwrap() - expected).abs() < 1e-8);
    }
    for (actual, expected) in entity["bounds"]["size"]
        .as_array()
        .unwrap()
        .iter()
        .zip([8., 4., 4.])
    {
        assert!((actual.as_f64().unwrap() - expected).abs() < 1e-8);
    }
    let export = core
        .query(
            &ws,
            &revision,
            "export",
            json!({"format":"STEP","pose_version":1}),
        )
        .await
        .unwrap();
    let data = core
        .artifact(&ws, &revision, export["handle"].as_str().unwrap())
        .await
        .unwrap();
    let draft = core
        .stage_asset(&ws, &revision, None, "posed.step", data, "user")
        .await
        .unwrap();
    let draft = core
        .update_source(
            &ws,
            &revision,
            Some(&draft.id),
            BTreeMap::from([(
                "model.py".into(),
                "show(import_step('posed.step'),name='roundtrip')".into(),
            )]),
            "user",
        )
        .await
        .unwrap();
    let draft = core
        .stage_mechanism(&ws, &revision, Some(&draft.id), Default::default(), "user")
        .await
        .unwrap();
    done(
        &core,
        &ws,
        core.build(&ws, &revision, &draft.id, "user").await.unwrap(),
    )
    .await;
    let project = core.get_project(&ws).await.unwrap();
    let bounds = &project["model"]["parts"][0]["bounds"];
    for axis in 0..2 {
        assert!(
            ((bounds["min"][axis].as_f64().unwrap() + bounds["max"][axis].as_f64().unwrap()) * 0.5
                - center)
                .abs()
                < 1e-8
        );
    }
    assert!((project["model"]["parts"][0]["volume"].as_f64().unwrap() - 128.).abs() < 1e-7);
}
#[tokio::test]
async fn saved_path_reopens_and_sharp_corner_repair_cannot_replace_the_valid_revision() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, revision) = model(&core, SOURCE, definition()).await;
    let mut invalid = definition();
    invalid["joints"][0]["kind"]["path"]["segments"] =
        json!([{"type":"line","end_m":[0.1,0,0]},{"type":"line","end_m":[0.1,0.1,0]}]);
    invalid["joints"][0]["kind"]["path"]["closed"] = json!(false);
    assert_eq!(
        core.dispatch(
            &context(),
            "define_mechanism",
            json!({"workspace_id":ws,"expected_base_revision":revision,"definition":invalid})
        )
        .await
        .unwrap_err()
        .code(),
        "INVALID_PATH"
    );
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
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
    assert!(motion.evaluation.joints[0].path.as_ref().unwrap().closed);
    assert_eq!(motion.pose_version, 0);
    assert!(motion.collision_checked);
}
