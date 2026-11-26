//! Golden coverage for the retired `control_motion_tests` catalog path (P4.6–P4.7):
//! saved tests left in a workspace migrate to requirement drafts on the first
//! requirements read, model-less orphans are dropped, and the migrated catalog
//! persists through save/reopen. Engine-level simulation coverage lives in
//! `motion_tests::tests`; end-to-end requirement runs live in `requirements_runs`.
use cadpad_core::{Core, tools, worker::WorkerConfig};
use serde_json::{Value, json};
use std::path::PathBuf;

fn config() -> WorkerConfig {
    if let Some(resources) = std::env::var_os("CADPAD_TEST_PACKAGED_RESOURCES") {
        assert_eq!(std::env::var("PATH").as_deref(), Ok(""));
        return cadpad_core::runtime::PackagedRuntime::open(std::path::Path::new(&resources))
            .unwrap()
            .worker_config();
    }
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
}

fn legacy_test(model: Option<Value>) -> Value {
    let mut definition = json!({
        "name":"Plate falls","intent":"The plate drops at least 1 mm",
        "initial_coordinates":{},
        "simulation":{"gravity_m_s2":[0,0,-9.81],"bodies":{"plate":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.1},"friction":0.5,"restitution":0}}},
        "actions":[{"ticks":10,"motors":[],"loads":{}}],
        "checks":[{"type":"displacement","body":"plate","direction":[0,0,-1],"minimum_mm":1}]
    });
    if let Some(model) = model {
        definition["model"] = model;
    }
    json!({"definition": definition, "last_run": null})
}

fn workspace_json(app: &std::path::Path, ws: &str) -> PathBuf {
    app.join("workspaces").join(ws).join("workspace.json")
}

#[tokio::test]
async fn saved_tests_migrate_to_requirement_drafts_and_orphans_are_dropped() {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("app");
    let core = Core::open(app.clone(), config()).await.unwrap();
    let ws = core.create_workspace("test").await.unwrap();
    drop(core);

    // Seed a legacy catalog: one model-bound test (migrates) and one orphan
    // (dropped, per the P4.6 decision that model-less tests have no home).
    let path = workspace_json(&app, &ws);
    let mut stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    stored["motion_tests"] = json!({
        "bound": legacy_test(Some(json!({"entry":"model.py","builder":"lever"}))),
        "orphan": legacy_test(None),
    });
    std::fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();

    let core = Core::open(app.clone(), config()).await.unwrap();
    async fn list(core: &Core, ws: &str) -> cadpad_core::Result<Value> {
        core.dispatch(
            &tools::ToolContext {
                actor: "agent".into(),
            },
            "control_requirements",
            json!({"workspace_id": ws, "expected_catalog": null,
                   "command": {"action": "list"}}),
        )
        .await
    }
    let listed = list(&core, &ws).await.unwrap();
    let ids: Vec<&str> = listed["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["bound"], "{listed}");
    assert_eq!(listed["requirements"][0]["status"], "draft");

    // The migration was committed once: the stored catalog no longer holds the
    // orphan and the bound entry is a requirement, and a second read is stable.
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let tests = stored["motion_tests"].as_object().unwrap();
    assert!(!tests.contains_key("orphan"));
    assert_eq!(tests["bound"]["kind"], "requirement");
    let again = list(&core, &ws).await.unwrap();
    assert_eq!(again["catalog"], listed["catalog"]);

    // Save/Open keeps the migrated catalog.
    let directory = temp.path().join("portable.cadpad");
    let revision = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    core.save_project(&ws, &revision, Some(directory.clone()))
        .await
        .unwrap();
    let other = Core::open(temp.path().join("app-b"), config())
        .await
        .unwrap();
    let opened = other.open_project(directory.clone(), "test").await.unwrap();
    let imported = other
        .dispatch(
            &tools::ToolContext {
                actor: "agent".into(),
            },
            "control_requirements",
            json!({"workspace_id": opened.workspace_id, "expected_catalog": null,
                   "command": {"action": "list"}}),
        )
        .await
        .unwrap();
    let imported_ids: Vec<&str> = imported["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(imported_ids, ["bound"], "{imported}");
}

#[tokio::test]
async fn orphan_only_catalogs_are_still_cleaned_up() {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("app");
    let core = Core::open(app.clone(), config()).await.unwrap();
    let ws = core.create_workspace("test").await.unwrap();
    drop(core);
    let path = workspace_json(&app, &ws);
    let mut stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    stored["motion_tests"] = json!({"orphan": legacy_test(None)});
    std::fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();

    let core = Core::open(app, config()).await.unwrap();
    let listed = core
        .dispatch(
            &tools::ToolContext {
                actor: "agent".into(),
            },
            "control_requirements",
            json!({"workspace_id": ws, "expected_catalog": null,
                   "command": {"action": "list"}}),
        )
        .await
        .unwrap();
    assert!(listed["requirements"].as_array().unwrap().is_empty());
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        stored["motion_tests"].as_object().unwrap().is_empty(),
        "{stored}"
    );
}

/// A corrupt catalog archive must still fail Save/Open rather than silently
/// dropping requirements.
#[tokio::test]
async fn corrupt_catalog_archive_fails_portable_open() {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("app");
    let core = Core::open(app.clone(), config()).await.unwrap();
    let ws = core.create_workspace("test").await.unwrap();
    drop(core);
    // Seed a legacy entry, migrate it, then save: the archive must exist.
    let path = workspace_json(&app, &ws);
    let mut stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    stored["motion_tests"] = json!({
        "bound": legacy_test(Some(json!({"entry":"model.py","builder":"lever"}))),
    });
    std::fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
    let core = Core::open(app, config()).await.unwrap();
    core.dispatch(
        &tools::ToolContext {
            actor: "agent".into(),
        },
        "control_requirements",
        json!({"workspace_id": ws, "expected_catalog": null,
               "command": {"action":"list"}}),
    )
    .await
    .unwrap();
    let revision = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let directory = temp.path().join("portable.cadpad");
    core.save_project(&ws, &revision, Some(directory.clone()))
        .await
        .unwrap();
    // Corrupt the catalog archive: open must fail on the digest mismatch.
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(directory.join("project.json")).unwrap()).unwrap();
    let digest = manifest["motion_tests"].as_str().unwrap();
    std::fs::write(directory.join(format!("motion-tests/{digest}.json")), b"{}").unwrap();
    let other = Core::open(temp.path().join("app-b"), config())
        .await
        .unwrap();
    assert!(other.open_project(directory.clone(), "test").await.is_err());
    assert!(core.save_project(&ws, &revision, None).await.is_err());
}
