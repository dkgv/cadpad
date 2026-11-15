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
    let job = core.build(&ws, &base, &draft.id, "user").await.unwrap();
    let job = terminal(core, &ws, job).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    (ws, job.resulting_revision.unwrap())
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
fn start() -> Value {
    json!({"action":"start","definition":{"gravity_m_s2":[0,0,-9.81],"bodies":{"rail":{"type":"fixed","friction":0.5,"restitution":0},"carriage":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.1},"friction":0.5,"restitution":0}}}})
}

async fn control(
    core: &Core,
    ws: &str,
    rev: &str,
    version: u64,
    command: Value,
) -> cadpad_core::Result<Value> {
    let result=core.dispatch(
        &ToolContext {
            actor: "external:dynamics".into(),
        },
        "control_simulation",
        json!({"workspace_id":ws,"revision":rev,"expected_pose_version":version,"command":command}),
    )
    .await;
    let value = match &result {
        Ok(value) => value.clone(),
        Err(error) => cadpad_core::error_value(error),
    };
    let schema = cadpad_core::tools::definition("control_simulation")
        .unwrap()
        .output_schema
        .as_ref()
        .unwrap();
    let validator = jsonschema::validator_for(schema).unwrap();
    assert!(
        validator.is_valid(&value),
        "{}",
        validator
            .iter_errors(&value)
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
    result
}

#[tokio::test]
async fn simulated_native_pose_is_shared_exportable_and_does_not_edit_the_design() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, rev) = model(&core).await;
    let before = core.get_project(&ws).await.unwrap();
    let result = control(&core, &ws, &rev, 0, start()).await.unwrap();
    assert_eq!(result["mode"], "simulate");
    assert_eq!(result["collision_checked"], false);
    assert_eq!(
        control(&core, &ws, &rev, 0, json!({"action":"advance","steps":1}))
            .await
            .unwrap_err()
            .code(),
        "POSE_CONFLICT"
    );
    assert_eq!(core.dispatch(&ToolContext{actor:"user".into()},"set_joint_coordinate",json!({"workspace_id":ws,"revision":rev,"expected_pose_version":1,"joint_id":"carriage_slide","coordinate":0.03,"unit":"m"})).await.unwrap_err().code(),"MODE_CONFLICT");
    control(&core,&ws,&rev,1,json!({"action":"motor","target":{"joint_id":"carriage_slide","target":0.04,"unit":"m","stiffness":100,"damping":10,"max_force":2}})).await.unwrap();
    let snapshot = control(&core, &ws, &rev, 2, json!({"action":"advance","steps":120}))
        .await
        .unwrap();
    assert_eq!(snapshot["simulation"]["current"]["tick"], 120);
    let x = snapshot["poses"]["carriage"]["position"][0]
        .as_f64()
        .unwrap();
    assert!((x - 40.).abs() < 0.1, "{x} mm");
    let exact=core.query(&ws,&rev,"inspect",json!({"pose_version":3,"query":"properties","coordinate_frame":"world","entities":[{"revision":rev,"instance_id":"carriage","entity_id":"part:0"}]})).await.unwrap();
    assert!(
        (exact["entities"][0]["world_center"][0].as_f64().unwrap() - x).abs() < 1e-6,
        "{exact}"
    );
    let exported = core
        .query(
            &ws,
            &rev,
            "export",
            json!({"format":"STEP","pose_version":3}),
        )
        .await
        .unwrap();
    assert!(exported["bytes"].as_u64().unwrap() > 1000);
    let after = core.get_project(&ws).await.unwrap();
    assert_eq!(before["revision"], after["revision"]);
    assert_eq!(before["activity"], after["activity"]);
    let stopped = control(&core, &ws, &rev, 3, json!({"action":"stop"}))
        .await
        .unwrap();
    assert_eq!(stopped["mode"], "manipulate");
    assert_eq!(stopped["poses"], before["motion"]["poses"]);
}
