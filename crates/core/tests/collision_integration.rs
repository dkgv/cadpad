use cadpad_core::{Core, Job, JobState, kinematics, tools::ToolContext, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
const SOURCE: &str = include_str!("../../../fixtures/channel_carriage.py");
fn config() -> WorkerConfig {
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
    .uncontained_for_trusted_tests()
}
fn definition() -> Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/channel_carriage.mechanism.json"
    ))
    .unwrap()
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
async fn succeeded(core: &Core, ws: &str, job: Job) -> String {
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
    let revision = succeeded(
        core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    (ws, revision)
}
fn context() -> ToolContext {
    ToolContext {
        actor: "external:collision".into(),
    }
}
async fn target(core: &Core, ws: &str, revision: &str, version: u64, coordinate: f64) -> Value {
    core.dispatch(&context(),"set_joint_coordinate",json!({"workspace_id":ws,"revision":revision,"expected_pose_version":version,"joint_id":"slide","coordinate":coordinate,"unit":"m"})).await.unwrap()
}
#[tokio::test]
async fn real_channel_preserves_opening_and_blocks_the_swept_stop_with_exact_settled_clearance() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, revision) = model(&core, SOURCE, definition()).await;
    let initial = core.get_project(&ws).await.unwrap();
    assert_eq!(initial["motion"]["collision_checked"], true);
    assert_eq!(
        initial["model"]["collision_validation"]["method"],
        "brep_boolean_coverage"
    );
    let moved = target(&core, &ws, &revision, 0, 0.08).await;
    assert_eq!(moved["collision"]["blocked"], true);
    assert_eq!(moved["collision"]["accuracy"], "conservative_proxy");
    let accepted = moved["applied"].as_f64().unwrap();
    assert!(accepted > 0.05898 && accepted < 0.059, "{moved}");
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
    assert_eq!(
        core.get_project(&ws).await.unwrap()["activity"],
        initial["activity"]
    );
    let checked=core.dispatch(&context(),"check_interference",json!({"workspace_id":ws,"revision":revision,"pose_version":1,"pairs":[["carriage","stop"],["carriage","rail"],["rail","stop"]],"exact":true})).await.unwrap();
    let result_schema = jsonschema::validator_for(
        cadpad_core::tools::definition("check_interference")
            .unwrap()
            .output_schema
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert!(result_schema.is_valid(&checked));
    let proxy_only = core.dispatch(&context(), "check_interference", json!({"workspace_id":ws,"revision":revision,"pose_version":1,"pairs":[["carriage","stop"]],"exact":false})).await.unwrap();
    assert!(result_schema.is_valid(&proxy_only));
    assert!(proxy_only["exact"].is_null());
    assert!(proxy_only["proxy"].is_object());
    let gap = checked["exact"]["pairs"][0]["distance_mm"]
        .as_f64()
        .unwrap();
    assert!((0.01..0.012).contains(&gap), "{checked}");
    assert_eq!(checked["exact"]["pairs"][0]["interfering"], false);
    assert_eq!(checked["exact"]["pairs"][0]["intersection_volume_mm3"], 0.);
    assert_eq!(
        checked["proxy"]["checked_pairs"].as_array().unwrap().len(),
        2
    );
    assert_eq!(checked["proxy"]["ignored_pairs"], json!([["rail", "stop"]]));
    let reversed = target(&core, &ws, &revision, 1, -0.08).await;
    assert_eq!(reversed["collision"]["blocked"], false);
    assert_eq!(reversed["applied"], -0.08);
    let changed = succeeded(
        &core,
        &ws,
        core.set_parameter(&ws, &revision, "stop_x", 40., "mm", "user")
            .await
            .unwrap(),
    )
    .await;
    let moved = target(&core, &ws, &changed, 0, 0.08).await;
    assert!(
        moved["applied"].as_f64().unwrap() > 0.02898 && moved["applied"].as_f64().unwrap() < 0.029
    );
    let directory = temp.path().join("portable");
    core.save_project(&ws, &changed, Some(directory.clone()))
        .await
        .unwrap();
    let other = Core::open(temp.path().join("other"), config())
        .await
        .unwrap();
    let opened = other.open_project(directory, "user").await.unwrap();
    let rev = succeeded(
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
    let moved = target(&other, &opened.workspace_id, &rev, 0, 0.08).await;
    assert_eq!(moved["collision"]["blocked"], true);
}
#[tokio::test]
async fn undercoverage_and_a_channel_filling_proxy_fail_without_replacing_the_model() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let (ws, revision) = model(&core, SOURCE, definition()).await;
    for (coarse, code) in [
        (false, "INVALID_COLLISION_PROXY"),
        (true, "COLLISION_INITIAL_OVERLAP"),
    ] {
        let mut bad = definition();
        if coarse {
            bad["collision"]["bodies"]["rail"] = json!([{"shape":{"type":"bounds"}}]);
        } else {
            bad["collision"]["bodies"]["rail"][0]["shape"]["half_extents_m"][0] = json!(0.09999);
        }
        let job = core
            .dispatch(
                &context(),
                "define_mechanism",
                json!({"workspace_id":ws,"expected_base_revision":revision,"definition":bad}),
            )
            .await
            .unwrap();
        let job = terminal(&core, &ws, serde_json::from_value(job).unwrap()).await;
        assert_eq!(job.state, JobState::Failed);
        assert_eq!(job.diagnostics["code"], code, "{:?}", job.diagnostics);
        assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
        assert_eq!(
            core.get_motion(&ws, &revision).await.unwrap().pose_version,
            0
        );
    }
}
#[tokio::test]
async fn concurrent_collision_targets_cannot_overwrite_each_other_and_stale_checks_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let (ws, revision) = model(&core, SOURCE, definition()).await;
    let call = |coordinate| {
        let core = core.clone();
        let ws = ws.clone();
        let revision = revision.clone();
        async move {
            core.dispatch(&context(),"set_joint_coordinate",json!({"workspace_id":ws,"revision":revision,"expected_pose_version":0,"joint_id":"slide","coordinate":coordinate,"unit":"m"})).await
        }
    };
    let (a, b) = tokio::join!(call(0.08), call(-0.08));
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let error = if let Err(error) = a {
        error
    } else {
        b.unwrap_err()
    };
    assert_eq!(error.code(), "POSE_CONFLICT");
    assert_eq!(
        core.get_motion(&ws, &revision).await.unwrap().pose_version,
        1
    );
    assert_eq!(core.dispatch(&context(),"check_interference",json!({"workspace_id":ws,"revision":revision,"pose_version":0,"pairs":[["carriage","stop"]]})).await.unwrap_err().code(),"POSE_CONFLICT");
}
#[tokio::test]
async fn native_hinge_blocks_an_intermediate_arc_and_exact_check_distinguishes_proxy_from_cad() {
    let source = "import cadquery as cq\nshow(cq.Workplane().box(8,4,4).translate((50,0,0)),name='arm')\nshow(cq.Workplane().sphere(3),name='obstacle',position=(0,50,0))";
    let definition = json!({"joints":[{"id":"hinge","parent":null,"child":"arm","kind":{"type":"revolute","axis":[0,0,1]},"initial":0,"limits":[-7,7]}],"collision":{"bodies":{"arm":[{"frame":{"position_m":[0.05,0,0]},"shape":{"type":"box","half_extents_m":[0.004,0.002,0.002]}}],"obstacle":[{"shape":{"type":"ball","radius_m":0.003}}]},"clearance_m":0.00001,"backoff_m":0.000001}});
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let (ws, revision) = model(&core, source, definition).await;
    let moved=core.dispatch(&context(),"set_joint_coordinate",json!({"workspace_id":ws,"revision":revision,"expected_pose_version":0,"joint_id":"hinge","coordinate":std::f64::consts::TAU,"unit":"rad"})).await.unwrap();
    assert_eq!(moved["collision"]["blocked"], true);
    let coordinate = moved["applied"].as_f64().unwrap();
    assert!(coordinate > 1.3 && coordinate < 1.6);
    let checked=core.dispatch(&context(),"check_interference",json!({"workspace_id":ws,"revision":revision,"pose_version":1,"pairs":[["arm","obstacle"]]})).await.unwrap();
    assert_eq!(checked["exact"]["accuracy"], "exact");
    assert_eq!(checked["proxy"]["accuracy"], "conservative_proxy");
    assert_eq!(checked["exact"]["pairs"][0]["interfering"], false);
    assert!(
        checked["exact"]["pairs"][0]["distance_mm"]
            .as_f64()
            .unwrap()
            >= 0.01
    );
}

#[tokio::test]
async fn exact_interference_detects_real_overlap_without_proxy_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let source = "import cadquery as cq\nshow(cq.Workplane().box(10,10,10),name='a')\nshow(cq.Workplane().box(10,10,10),name='b',position=(5,0,0))";
    let (ws, revision) = model(&core, source, json!({})).await;
    let checked = core
        .dispatch(
            &context(),
            "check_interference",
            json!({"workspace_id":ws,"revision":revision,"pose_version":0,"pairs":[["a","b"]]}),
        )
        .await
        .unwrap();
    assert_eq!(checked["proxy"], Value::Null);
    assert_eq!(checked["exact"]["pairs"][0]["interfering"], true);
    assert!(
        (checked["exact"]["pairs"][0]["intersection_volume_mm3"]
            .as_f64()
            .unwrap()
            - 500.)
            .abs()
            < 1e-8
    );
    assert_eq!(checked["exact"]["pairs"][0]["distance_mm"], 0.);
    assert_eq!(core.dispatch(&context(),"check_interference",json!({"workspace_id":ws,"revision":revision,"pose_version":0,"pairs":[["a","b"]],"exact":false})).await.unwrap_err().code(),"COLLISION_NOT_CONFIGURED");
    assert_eq!(core.dispatch(&context(),"check_interference",json!({"workspace_id":ws,"revision":revision,"pose_version":0,"pairs":[["a","b"],["b","a"]]})).await.unwrap_err().code(),"INVALID_REQUEST");
}
