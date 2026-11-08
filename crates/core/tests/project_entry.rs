use cadpad_core::{Core, Job, JobState, portable, project_entry, worker::WorkerConfig};
use serde_json::Value;
use std::{path::PathBuf, time::Duration};
fn config() -> WorkerConfig {
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
    .uncontained_for_trusted_tests()
}
async fn done(core: &Core, ws: &str, job: Job) -> Job {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
fn code(plan: &Value) -> &str {
    plan["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|option| option["id"] == "build")
        .unwrap()["source"]
        .as_str()
        .unwrap()
}
const SOURCE: &str = r#"from pathlib import Path
import cadquery as cq

def build():
    segments = [((0, 90), cq.Workplane().box(10, 20, 3))]
    splices = [(90, cq.Workplane().box(3, 4, 5))]
    return segments, splices

def requires_size(size):
    return cq.Workplane().box(size, size, size)

def main():
    (Path(__file__).parent / 'out').mkdir()
    raise RuntimeError('export main was executed')

if __name__ == '__main__':
    main()
"#;

#[tokio::test]
async fn scaffold_imports_geometry_builder_without_running_export_main_or_rewriting_source() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("math.py"), SOURCE).unwrap();
    portable::initialize_folder(&root, "math.py").unwrap();
    let before = std::fs::read(root.join("math.py")).unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let plan = core
        .analyze_project_entry(root.clone(), "math.py".into())
        .await
        .unwrap();
    assert_eq!(plan["recommended"], "build");
    assert_eq!(plan["has_main"], true);
    assert!(!root.join("out").exists());
    assert_eq!(
        plan["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["id"] == "requires_size")
            .unwrap()["available"],
        false
    );
    // Existing entry fails, while setup remains available for the configured folder.
    let opened = core.open_project(root.clone(), "user").await.unwrap();
    let job = done(
        &core,
        &opened.workspace_id,
        core.build(
            &opened.workspace_id,
            &opened.base_revision,
            &opened.draft.id,
            "user",
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(
        core.project_directory(&opened.workspace_id).await.unwrap(),
        root.canonicalize().unwrap()
    );

    let baseline = project_entry::manifest_hash(&root).unwrap();
    let entry = project_entry::configure_entry(
        &root,
        "math.py",
        baseline.as_deref(),
        Some((plan["source_hash"].as_str().unwrap(), code(&plan))),
    )
    .unwrap();
    assert_eq!(entry, "cadpad_entry.py");
    assert_eq!(std::fs::read(root.join("math.py")).unwrap(), before);
    let opened = core.open_project(root.clone(), "user").await.unwrap();
    let job = done(
        &core,
        &opened.workspace_id,
        core.build(
            &opened.workspace_id,
            &opened.base_revision,
            &opened.draft.id,
            "user",
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    assert_eq!(
        core.get_project(&opened.workspace_id).await.unwrap()["model"]["parts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(!root.join("out").exists());
}

#[tokio::test]
async fn entry_analysis_never_executes_source_and_adapter_creation_checks_its_preview() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir(&root).unwrap();
    let source = "raise RuntimeError('analysis executed project code')\ndef build(): return None\n";
    std::fs::write(root.join("main.py"), source).unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let plan = core
        .analyze_project_entry(root.clone(), "main.py".into())
        .await
        .unwrap();
    assert_eq!(plan["recommended"], "build");
    assert!(!root.join(".cadpad").exists());
    std::fs::write(root.join("main.py"), "# external edit").unwrap();
    assert_eq!(
        project_entry::configure_entry(
            &root,
            "main.py",
            None,
            Some((plan["source_hash"].as_str().unwrap(), code(&plan)))
        )
        .unwrap_err()
        .code(),
        "REVISION_CONFLICT"
    );
    assert!(!root.join("cadpad_entry.py").exists());
    std::fs::write(root.join("main.py"), source).unwrap();
    std::fs::write(root.join("cadpad_entry.py"), "# keep existing file").unwrap();
    let selected = project_entry::configure_entry(
        &root,
        "main.py",
        None,
        Some((plan["source_hash"].as_str().unwrap(), code(&plan))),
    )
    .unwrap();
    assert_eq!(selected, "cadpad_entry_1.py");
    assert_eq!(
        std::fs::read_to_string(root.join("cadpad_entry.py")).unwrap(),
        "# keep existing file"
    );
    let baseline = project_entry::manifest_hash(&root).unwrap();
    std::fs::write(root.join(".cadpad/project.json"), "{}").unwrap();
    assert_eq!(
        project_entry::configure_entry(&root, "main.py", baseline.as_deref(), None)
            .unwrap_err()
            .code(),
        "REVISION_CONFLICT"
    );
}
