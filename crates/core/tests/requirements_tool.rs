//! `control_requirements`: catalog storage, CAS, drafts, and the user-only
//! approval boundary.
use cadpad_core::requirements::storage::{ApproveRequest, StoredHashes};
use cadpad_core::{Core, tools, worker::WorkerConfig};
use serde_json::{Value, json};

const R1: &str = include_str!("fixtures/requirements/R1.json");

fn context() -> tools::ToolContext {
    tools::ToolContext {
        actor: "agent".into(),
    }
}

async fn open() -> (tempfile::TempDir, Core, String) {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(
        temp.path().join("app"),
        WorkerConfig::bundled("unused".into()),
    )
    .await
    .unwrap();
    let ws = core.create_workspace("test").await.unwrap();
    (temp, core, ws)
}

async fn call(core: &Core, ws: &str, expected: Option<&str>, command: Value) -> cadpad_core::Result<Value> {
    core.dispatch(
        &context(),
        "control_requirements",
        json!({"workspace_id": ws, "expected_catalog": expected, "command": command}),
    )
    .await
}

#[test]
fn approve_is_not_an_agent_command() {
    let tool = tools::definition("control_requirements").unwrap();
    let schema = tool.input_schema.to_string();
    for action in ["list", "compile", "save", "library", "run", "start_run", "cancel_run"] {
        assert!(schema.contains(&format!("\"{action}\"")), "{action}");
    }
    assert!(!schema.contains("\"approve\""), "approve must not be a command");
    assert!(!tool.description.contains("approve command exists"));
    assert!(tools::definitions().iter().all(|t| !t.name.contains("approve")));
}

#[tokio::test]
async fn dispatch_rejects_approve_and_unknown_commands() {
    let (_temp, core, ws) = open().await;
    for command in [
        json!({"action": "approve", "id": "x"}),
        json!({"action": "delete", "id": "x"}),
    ] {
        let err = call(&core, &ws, None, command).await.unwrap_err();
        assert_eq!(err.code(), "INVALID_REQUEST");
    }
    assert!(core.dispatch(&context(), "approve_requirement", json!({})).await.is_err());
}

#[tokio::test]
async fn save_stores_a_draft_with_cas_and_replies_match_the_schema() {
    let (_temp, core, ws) = open().await;
    let schema = tools::definition("control_requirements")
        .unwrap()
        .output_schema
        .clone()
        .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let listed = call(&core, &ws, None, json!({"action": "list"})).await.unwrap();
    assert_eq!(listed["requirements"], json!([]));
    assert!(validator.is_valid(&listed), "{listed}");
    let catalog = listed["catalog"].as_str().unwrap().to_owned();

    let ir: Value = serde_json::from_str(R1).unwrap();
    // Compile saves nothing; the workspace has no built model, so binding fails.
    let compiled = call(&core, &ws, None, json!({"action": "compile", "ir": ir}))
        .await
        .unwrap();
    assert_eq!(compiled["compiled"]["ok"], false);
    assert!(validator.is_valid(&compiled), "{compiled}");
    let relisted = call(&core, &ws, None, json!({"action": "list"})).await.unwrap();
    assert_eq!(relisted["catalog"], catalog);

    // Save needs the catalog token, and refuses a stale one.
    let err = call(&core, &ws, None, json!({"action": "save", "ir": ir})).await.unwrap_err();
    assert_eq!(err.code(), "REVISION_CONFLICT");
    let err = call(&core, &ws, Some("stale"), json!({"action": "save", "ir": ir}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "REVISION_CONFLICT");

    let saved = call(&core, &ws, Some(&catalog), json!({"action": "save", "ir": ir}))
        .await
        .unwrap();
    assert!(validator.is_valid(&saved), "{saved}");
    let id = ir["id"].as_str().unwrap();
    let entry = &saved["requirements"][0];
    assert_eq!(entry["id"], id);
    assert_eq!(entry["status"], "draft");
    assert_eq!(entry["approval"], "draft");
    assert!(entry["approved"].is_null());
    assert!(!entry["diagnostics"].as_array().unwrap().is_empty());
    assert_ne!(saved["catalog"], json!(catalog));
    // The old token no longer applies.
    let err = call(&core, &ws, Some(&catalog), json!({"action": "save", "ir": ir}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "REVISION_CONFLICT");

    // Motion tests keep working in the same catalog.
    let tests = core
        .dispatch(
            &context(),
            "control_motion_tests",
            json!({"workspace_id": ws, "revision": "", "expected_catalog": null, "command": {"action": "list"}}),
        )
        .await
        .unwrap();
    assert_eq!(tests["tests"], json!([]));
    assert_eq!(tests["catalog"], saved["catalog"]);

    let lib = call(&core, &ws, None, json!({"action": "library"})).await.unwrap();
    assert!(validator.is_valid(&lib), "{lib}");
    assert!(lib["library"]["rules"].as_array().is_some_and(|r| !r.is_empty()));
}

#[tokio::test]
async fn agents_cannot_run_drafts_and_approval_refuses_errors() {
    let (_temp, core, ws) = open().await;
    let ir: Value = serde_json::from_str(R1).unwrap();
    let catalog = call(&core, &ws, None, json!({"action": "list"})).await.unwrap()["catalog"]
        .as_str()
        .unwrap()
        .to_owned();
    let saved = call(&core, &ws, Some(&catalog), json!({"action": "save", "ir": ir}))
        .await
        .unwrap();
    let catalog = saved["catalog"].as_str().unwrap().to_owned();
    let id = ir["id"].as_str().unwrap();

    let err = call(&core, &ws, None, json!({"action": "run", "id": id})).await.unwrap_err();
    assert_eq!(err.code(), "REQUIREMENT_NOT_APPROVED");
    let err = call(&core, &ws, None, json!({"action": "run", "id": "nope"})).await.unwrap_err();
    assert_eq!(err.code(), "INVALID_REQUIREMENT");

    // The user's approval compiles again and refuses while errors remain.
    let request = ApproveRequest {
        workspace_id: ws.clone(),
        expected_catalog: Some(catalog.clone()),
        id: id.to_owned(),
        hashes: StoredHashes {
            requirement: String::new(),
            manifest: String::new(),
            plan: String::new(),
        },
    };
    let err = core.approve_requirement(request).await.unwrap_err();
    assert_eq!(err.code(), "REQUIREMENT_HAS_ERRORS");
    let after = call(&core, &ws, None, json!({"action": "list"})).await.unwrap();
    assert_eq!(after["catalog"], json!(catalog));
    assert_eq!(after["requirements"][0]["approval"], "draft");
}
