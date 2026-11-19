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

#[tokio::test]
async fn large_source_can_be_searched_and_read_in_bounded_pages() {
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
        .dispatch(&context, "get_project", json!({"workspace_id": ws}))
        .await
        .unwrap();
    let first = format!("#{}\r\n", "é".repeat(6000));
    let tail = "show(make_slab(), name='Slab')\r\n# Slab detail";
    let draft = core
        .dispatch(
            &context,
            "update_source",
            json!({
                "workspace_id": ws, "expected_base_revision": project["revision"],
                "changes": {"model.py": format!("{first}{tail}")}
            }),
        )
        .await
        .unwrap();
    let args = json!({"workspace_id": ws, "version": draft["id"], "path": "model.py"});

    let mut search = args.clone();
    search["query"] = json!("Slab");
    search["max_matches"] = json!(1);
    let hit = core
        .dispatch(&context, "search_source", search.clone())
        .await
        .unwrap();
    assert_eq!(hit["matches"][0]["line"], 2);
    assert_eq!(
        hit["matches"][0]["preview"],
        "show(make_slab(), name='Slab')"
    );
    search["start_line"] = hit["next_line"].clone();
    let next = core
        .dispatch(&context, "search_source", search)
        .await
        .unwrap();
    assert_eq!(next["matches"][0]["line"], 3);
    assert!(next["next_line"].is_null());

    let page = core
        .dispatch(&context, "read_source_lines", args.clone())
        .await
        .unwrap();
    assert_eq!(page["hash"], hit["hash"]);
    assert_eq!(page["next_line"], 1);
    let prefix = page["source"].as_str().unwrap();
    assert!(prefix.len() <= 8192);
    let mut continuation = args;
    continuation["start_line"] = page["next_line"].clone();
    continuation["byte_offset"] = page["next_byte_offset"].clone();
    let remainder = core
        .dispatch(&context, "read_source_lines", continuation)
        .await
        .unwrap();
    assert_eq!(
        format!("{prefix}{}", remainder["source"].as_str().unwrap()),
        format!("{first}{tail}")
    );
    assert!(remainder["next_line"].is_null());
}
