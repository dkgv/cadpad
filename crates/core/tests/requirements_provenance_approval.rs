//! Prove that user approval stores the provenance-checked, inferred IR.
use cadpad_core::{
    Core, portable,
    requirements::storage::{ApproveRequest, StoredHashes},
    tools,
    worker::WorkerConfig,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const E5: &str = include_str!("fixtures/requirements/E5.json");

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn copy_fixture(to: &Path) {
    for entry in std::fs::read_dir(repository().join("fixtures/requirements/bracket")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

#[tokio::test]
async fn approval_hashes_the_inferred_value_and_confirms_it() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir(&root).unwrap();
    copy_fixture(&root);
    portable::initialize_project_browser(&root).unwrap();
    let core = Core::open(
        temp.path().join("app"),
        WorkerConfig::development(&repository()).uncontained_for_trusted_tests(),
    )
    .await
    .unwrap();
    let workspace = core.open_project(root, "user").await.unwrap().workspace_id;
    let context = tools::ToolContext {
        actor: "agent".into(),
    };

    let mut ir: Value = serde_json::from_str(E5).unwrap();
    ir["id"] = json!("provenance-approval");
    ir["claims"][0]["args"]["bed"]["list"][0]["quantity"]["value"] = json!(300.0);
    let compiled = core
        .dispatch(
            &context,
            "control_requirements",
            json!({"workspace_id": workspace, "command": {"action": "compile", "ir": ir}}),
        )
        .await
        .unwrap();
    let report = &compiled["compiled"];
    assert_eq!(report["ok"], true, "{}", report["diagnostics"]);
    assert_eq!(
        report["requirement"]["claims"][0]["args"]["bed"]["list"][0]["quantity"]["source"],
        "inferred"
    );
    assert!(
        report["downgrades"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["at"] == "claims[0].args.bed[0]")
    );
    let hashes: StoredHashes = serde_json::from_value(report["hashes"].clone()).unwrap();

    let listed = core
        .dispatch(
            &context,
            "control_requirements",
            json!({"workspace_id": workspace, "command": {"action": "list"}}),
        )
        .await
        .unwrap();
    let saved = core.dispatch(
        &context,
        "control_requirements",
        json!({"workspace_id": workspace, "expected_catalog": listed["catalog"], "command": {"action": "save", "ir": ir}}),
    ).await.unwrap();
    let approved = core
        .approve_requirement(ApproveRequest {
            workspace_id: workspace,
            expected_catalog: saved["catalog"].as_str().map(str::to_owned),
            id: "provenance-approval".into(),
            hashes: hashes.clone(),
        })
        .await
        .unwrap();
    let entry = approved
        .requirements
        .iter()
        .find(|r| r.id == "provenance-approval")
        .unwrap();
    let stored = entry.approved.as_ref().expect("user approval stored");
    assert_eq!(stored.hashes, hashes);
    assert_eq!(stored.print.render(), report["rendered"].as_str().unwrap());
}
