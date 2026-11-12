use cadpad_core::{
    Core,
    tools::{ToolContext, definitions},
    worker::WorkerConfig,
};
use serde_json::{Value, json};

#[test]
fn versioned_schema_matches_rust_contract() {
    let locked: Value = serde_json::from_str(include_str!("fixtures/tool-schema-v1.json")).unwrap();
    assert_eq!(locked, json!({"protocol_version":1,"tools":definitions()}));
    for tool in definitions() {
        assert_eq!(tool.input_schema["additionalProperties"], false);
    }
}
#[tokio::test]
async fn transport_identity_and_explicit_workspace_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(
        dir.path().into(),
        WorkerConfig::bundled("unused-worker".into()),
    )
    .await
    .unwrap();
    let context = ToolContext {
        actor: "external-session".into(),
    };
    let ws = core.create_workspace("local-user").await.unwrap();
    let project = core
        .dispatch(&context, "get_project", json!({"workspace_id":ws}))
        .await
        .unwrap();
    let args = json!({"workspace_id":ws,"expected_base_revision":project["revision"],"changes":{"model.py":"print('candidate')"},"actor":"local-user"});
    assert_eq!(
        core.dispatch(&context, "update_source", args)
            .await
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );
    assert!(
        core.dispatch(&context, "get_project", json!({}))
            .await
            .unwrap()["workspace_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(ws))
    );
    assert_eq!(
        core.dispatch(&ToolContext { actor: "".into() }, "get_project", json!({}))
            .await
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );
    assert_eq!(
        core.dispatch(
            &context,
            "read_source",
            json!({"version":project["revision"],"path":"model.py"})
        )
        .await
        .unwrap_err()
        .code(),
        "INVALID_REQUEST"
    );
}
