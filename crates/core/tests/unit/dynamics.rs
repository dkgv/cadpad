use crate::{Core, tools::ToolContext};
use serde_json::{Value, json};

fn start() -> Value {
    json!({"action":"start","definition":{"gravity_m_s2":[0,0,-9.81],"bodies":{"rail":{"type":"fixed","friction":0.5,"restitution":0},"carriage":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.1},"friction":0.5,"restitution":0}}}})
}

async fn control(
    core: &Core,
    ws: &str,
    rev: &str,
    version: u64,
    command: Value,
) -> crate::Result<Value> {
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
        Err(error) => crate::error_value(error),
    };
    let schema = crate::tools::definition("control_simulation")
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
async fn shared_load_commands_are_pose_scoped_atomic_and_preserve_design_geometry() {
    let storage = tempfile::tempdir().unwrap();
    let (core, ws, rev) = super::rail(storage.path()).await;
    let before = core.get_project(&ws).await.unwrap();
    let mut definition = start();
    definition["definition"]["gravity_m_s2"] = json!([0., 0., 0.]);
    control(&core, &ws, &rev, 0, definition).await.unwrap();
    let load =
        json!({"action":"loads","loads":{"carriage":{"force_n":[0.1,0,0],"torque_nm":[0,0,0]}}});
    let applied = control(&core, &ws, &rev, 1, load.clone()).await.unwrap();
    assert_eq!(applied["simulation"]["current"]["tick"], 0);
    assert_eq!(
        applied["simulation"]["external_loads"]["carriage"]["force_n"],
        json!([0.1, 0., 0.])
    );
    assert_eq!(
        control(&core, &ws, &rev, 1, load.clone())
            .await
            .unwrap_err()
            .code(),
        "POSE_CONFLICT"
    );
    assert_eq!(
        control(
            &core,
            &ws,
            &rev,
            2,
            json!({"action":"loads","loads":{"rail":{"force_n":[1,0,0],"torque_nm":[0,0,0]}}})
        )
        .await
        .unwrap_err()
        .code(),
        "INVALID_SIMULATION"
    );
    let first = control(&core, &ws, &rev, 2, json!({"action":"advance","steps":10}))
        .await
        .unwrap();
    let velocity = first["simulation"]["current"]["linear_velocity_m_s"]["carriage"][0]
        .as_f64()
        .unwrap();
    assert!((velocity - 10. / 120.).abs() < 1e-7, "{first}");
    assert_eq!(
        first["simulation"]["external_loads"],
        applied["simulation"]["external_loads"]
    );
    assert!(first["poses"]["carriage"]["position"][0].as_f64().unwrap() > 1.);
    control(&core, &ws, &rev, 3, load).await.unwrap();
    let second = control(&core, &ws, &rev, 4, json!({"action":"advance","steps":10}))
        .await
        .unwrap();
    assert!(
        (second["simulation"]["current"]["linear_velocity_m_s"]["carriage"][0]
            .as_f64()
            .unwrap()
            - 2. * velocity)
            .abs()
            < 1e-7
    );
    control(&core, &ws, &rev, 5, json!({"action":"loads","loads":{}}))
        .await
        .unwrap();
    let cleared = control(&core, &ws, &rev, 6, json!({"action":"advance","steps":10}))
        .await
        .unwrap();
    assert_eq!(cleared["simulation"]["external_loads"], json!({}));
    assert!(
        (cleared["simulation"]["current"]["linear_velocity_m_s"]["carriage"][0]
            .as_f64()
            .unwrap()
            - 2. * velocity)
            .abs()
            < 1e-7
    );
    let stopped = control(&core, &ws, &rev, 7, json!({"action":"stop"}))
        .await
        .unwrap();
    assert_eq!(stopped["mode"], "manipulate");
    let after = core.get_project(&ws).await.unwrap();
    assert_eq!(after["revision"], before["revision"]);
    assert_eq!(after["activity"], before["activity"]);
    assert_eq!(after["parameters"], before["parameters"]);
}

#[tokio::test]
async fn concurrent_steps_commit_one_candidate_and_failed_inputs_preserve_the_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let (core, ws, rev) = super::rail(temp.path().to_path_buf()).await;
    control(&core, &ws, &rev, 0, start()).await.unwrap();
    let (a, b) = tokio::join!(
        control(&core, &ws, &rev, 1, json!({"action":"advance","steps":2})),
        control(&core, &ws, &rev, 1, json!({"action":"advance","steps":3}))
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let winner = a.as_ref().or(b.as_ref()).unwrap();
    let tick = winner["simulation"]["current"]["tick"].as_u64().unwrap();
    assert_eq!(a.err().or(b.err()).unwrap().code(), "POSE_CONFLICT");
    assert!(
        control(&core, &ws, &rev, 2, json!({"action":"advance","steps":121}))
            .await
            .is_err()
    );
    let after = control(&core, &ws, &rev, 2, json!({"action":"advance","steps":1}))
        .await
        .unwrap();
    assert_eq!(after["simulation"]["current"]["tick"], tick + 1);
    core.revoke_actor("external:dynamics").await;
    assert!(
        control(&core, &ws, &rev, 3, json!({"action":"advance","steps":1}))
            .await
            .is_err()
    );
    assert_eq!(core.get_motion(&ws, &rev).await.unwrap().pose_version, 3);
    drop(core);
    let recovered = Core::open(
        temp.path().to_path_buf(),
        crate::worker::WorkerConfig::bundled(temp.path().join("no-native-worker")),
    )
    .await
    .unwrap();
    let restored = recovered.get_motion(&ws, &rev).await.unwrap();
    assert_eq!(restored.mode, "manipulate");
    assert_eq!(restored.pose_version, 0);
    assert!(restored.simulation.is_none());
}
