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
    let index = core.index_project_folder(root.clone()).await.unwrap();
    assert_eq!(index["files"][0]["path"], "math.py");
    assert_eq!(index["files"][0]["builders"][0]["id"], "build");
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
    let index = core
        .index_project_sources(&opened.workspace_id, &opened.base_revision)
        .await
        .unwrap();
    assert_eq!(index["files"][0]["path"], "math.py");
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

#[test]
fn existing_project_can_grow_beyond_256_sources_and_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..300 {
        std::fs::write(
            temp.path().join(format!("part_{index}.py")),
            "# component\n",
        )
        .unwrap();
    }
    // Dependency environments and generated caches are not project sources.
    std::fs::create_dir(temp.path().join(".venv")).unwrap();
    std::fs::write(temp.path().join(".venv/dependency.py"), "ignored").unwrap();
    portable::initialize_folder(temp.path(), "part_0.py").unwrap();
    let loaded = portable::read_project(temp.path()).unwrap();
    assert_eq!(loaded.inputs.files.len(), 300);
    portable::validate_inputs(&loaded.inputs).unwrap();
    std::fs::write(temp.path().join("part_300.py"), "# newly added\n").unwrap();
    let reloaded = portable::read_project(temp.path()).unwrap();
    assert_eq!(reloaded.inputs.files.len(), 301);
    portable::validate_inputs(&reloaded.inputs).unwrap();
    let mut oversized = loaded.inputs;
    for index in 300..=portable::SOURCE_FILE_LIMIT {
        oversized
            .files
            .insert(format!("part_{index}.py"), String::new());
    }
    assert!(portable::validate_inputs(&oversized).is_err());
}

#[tokio::test]
async fn catalog_builders_switch_in_place_without_touching_project_sources() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir(&root).unwrap();
    let source = "import cadquery as cq\ndef plate(): return cq.Workplane().box(3,4,5)\ndef pin(): return cq.Workplane().circle(2).extrude(8)\ndef needs_size(size): pass\n";
    std::fs::write(root.join("parts.py"), source).unwrap();
    portable::initialize_folder(&root, "parts.py").unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let opened = core.open_project(root.clone(), "user").await.unwrap();
    let ws = &opened.workspace_id;
    let mut revision = opened.base_revision.clone();
    for builder in ["plate", "pin"] {
        let job = core
            .work_on_project_builder(ws, &revision, "parts.py", builder, "user")
            .await
            .unwrap();
        let job = done(&core, ws, job).await;
        assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
        revision = job.resulting_revision.unwrap();
        let project = core.get_project(ws).await.unwrap();
        assert_eq!(project["workspace_id"], *ws);
        assert_eq!(project["model"]["parts"].as_array().unwrap().len(), 1);
        assert_eq!(
            project["files"].as_array().unwrap().len(),
            2,
            "reuse the view adapter"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("parts.py")).unwrap(),
            source
        );
        assert!(
            !root.join("cadpad_view_0.py").exists(),
            "only Save writes the adapter"
        );
    }
    let job = core
        .work_on_project_builder(ws, &revision, "parts.py", "$group", "user")
        .await
        .unwrap();
    let job = done(&core, ws, job).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    revision = job.resulting_revision.unwrap();
    assert_eq!(
        core.get_project(ws).await.unwrap()["model"]["parts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        core.work_on_project_builder(ws, &revision, "parts.py", "needs_size", "user")
            .await
            .is_err()
    );
    assert!(
        core.work_on_project_builder(ws, &opened.base_revision, "parts.py", "plate", "user")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn opening_existing_repository_for_browsing_needs_no_entry_selection_or_execution() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir(&root).unwrap();
    let original = "raise RuntimeError('project must not execute on open')\ndef plate(): pass\n";
    std::fs::write(root.join("model.py"), original).unwrap();
    std::fs::write(root.join("cadpad_view_0.py"), "# user file\n").unwrap();
    portable::initialize_project_browser(&root).unwrap();
    let manifest = portable::read_manifest(&root).unwrap();
    assert_eq!(manifest.entry_point, "cadpad_view_1.py");
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let opened = core.open_project(root.clone(), "user").await.unwrap();
    let project = core.get_project(&opened.workspace_id).await.unwrap();
    assert_eq!(project["files"].as_array().unwrap().len(), 3);
    assert!(project["model"]["parts"].as_array().unwrap().is_empty());
    let index = core
        .index_project_sources(&opened.workspace_id, &opened.base_revision)
        .await
        .unwrap();
    let paths: Vec<_> = index["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["cadpad_view_0.py", "model.py"],
        "hide the generated browser entry while retaining similarly named user files"
    );
    // Rebuild is safe too: the original script and its exports are never the entry.
    let job = core
        .build(
            &opened.workspace_id,
            &opened.base_revision,
            &opened.draft.id,
            "user",
        )
        .await
        .unwrap();
    assert_eq!(
        done(&core, &opened.workspace_id, job).await.state,
        JobState::Succeeded
    );
    assert_eq!(
        std::fs::read_to_string(root.join("model.py")).unwrap(),
        original
    );
    assert_eq!(
        std::fs::read_to_string(root.join("cadpad_view_0.py")).unwrap(),
        "# user file\n"
    );
}

#[test]
fn generated_adapters_expose_their_source_entry_for_explorer_selection() {
    let source = portable::PROJECT_BROWSER_SOURCE; // not a concrete builder
    assert!(project_entry::generated_model_entry(source).is_none());
    let adapter = r#"# Generated Cadpad entry point. Original source files stay unchanged.
# Importing the module skips its `if __name__ == '__main__'` export script.
import importlib
import importlib.util
import sys
from pathlib import Path
import cadquery as cq

def build(context):
    path = Path(__file__).parent / "alpha/output/tower/reference.py"
    spec = importlib.util.spec_from_file_location('cadpad_source', path)
    module = importlib.util.module_from_spec(spec)
    module.__package__ = "alpha.output.tower"
    sys.modules[spec.name] = module
    root = Path(__file__).parent
    sys.path[:0] = [str(parent) for parent in path.parents if parent.is_relative_to(root)]
    spec.loader.exec_module(module)
    result = getattr(module, "chassis")()
    count = 0
"#;
    assert_eq!(
        project_entry::generated_model_entry(adapter),
        Some(("alpha/output/tower/reference.py".into(), "chassis".into()))
    );
}

#[tokio::test]
async fn model_tabs_keep_independent_geometry_inside_one_project_without_writing_sources() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir(&root).unwrap();
    let source = "import cadquery as cq\ndef plate(): return cq.Workplane().box(2,3,4)\ndef pin(): return cq.Workplane().box(1,1,8)\n";
    std::fs::write(root.join("geometry.py"), source).unwrap();
    portable::initialize_project_browser(&root).unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let project = core.open_project(root.clone(), "user").await.unwrap();
    let (plate, job) = core
        .open_project_model(
            &project.workspace_id,
            &project.base_revision,
            "geometry.py",
            "plate",
            "user",
        )
        .await
        .unwrap();
    assert_eq!(done(&core, &plate, job).await.state, JobState::Succeeded);
    let (pin, job) = core
        .open_project_model(
            &project.workspace_id,
            &project.base_revision,
            "geometry.py",
            "pin",
            "user",
        )
        .await
        .unwrap();
    assert_eq!(done(&core, &pin, job).await.state, JobState::Succeeded);
    assert_ne!(plate, pin);
    assert_eq!(
        core.get_project(&project.workspace_id).await.unwrap()["revision"],
        project.base_revision
    );
    let plate_model = core.get_project(&plate).await.unwrap();
    let pin_model = core.get_project(&pin).await.unwrap();
    let index = core
        .index_project_sources(&plate, plate_model["revision"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(index["files"].as_array().unwrap().len(), 1);
    assert_eq!(
        index["files"][0]["path"], "geometry.py",
        "built model adapters stay out of the source catalog"
    );
    assert_eq!(
        plate_model["model"]["parts"][0]["bounds"]["size"],
        serde_json::json!([2.0, 3.0, 4.0])
    );
    assert_eq!(
        pin_model["model"]["parts"][0]["bounds"]["size"],
        serde_json::json!([1.0, 1.0, 8.0])
    );
    assert_eq!(
        core.model_parent(&plate).await,
        Some(project.workspace_id.clone())
    );
    assert_eq!(
        core.model_parent(&pin).await,
        Some(project.workspace_id.clone())
    );
    let summaries = core.workspace_summaries().await;
    let ids = summaries
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["workspace_id"].as_str().unwrap().to_string())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        ids.contains(&project.workspace_id) && ids.contains(&plate) && ids.contains(&pin),
        "recent summaries must keep the project root and every model tab: {ids:?}"
    );
    assert!(!core.model_source_dirty(&plate).await);
    assert!(!core.model_source_dirty(&pin).await);
    assert_eq!(
        core.project_directory(&plate).await.unwrap(),
        root.canonicalize().unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(root.join("geometry.py")).unwrap(),
        source
    );
    assert_eq!(
        std::fs::read_to_string(root.join("cadpad_view_0.py")).unwrap(),
        portable::PROJECT_BROWSER_SOURCE
    );
    let changed = source.replace("box(2,3,4)", "box(3,3,4)");
    let base = plate_model["revision"].as_str().unwrap();
    let draft = core
        .update_source(
            &plate,
            base,
            None,
            std::collections::BTreeMap::from([("geometry.py".into(), changed.clone())]),
            "user",
        )
        .await
        .unwrap();
    let job = core.build(&plate, base, &draft.id, "user").await.unwrap();
    let job = done(&core, &plate, job).await;
    assert_eq!(job.state, JobState::Succeeded);
    assert!(
        core.model_source_dirty(&plate).await,
        "real source edits remain eligible for saving"
    );
    assert!(!core.model_source_dirty(&pin).await);
    core.save_project_folder(&plate, job.resulting_revision.as_deref().unwrap(), None)
        .await
        .unwrap();
    assert!(!core.model_source_dirty(&plate).await);
    assert_eq!(
        std::fs::read_to_string(root.join("geometry.py")).unwrap(),
        changed
    );
    assert_eq!(
        core.get_project(&pin).await.unwrap()["model"],
        pin_model["model"],
        "saving another model preserves this tab's geometry"
    );
}

#[tokio::test]
async fn browsing_a_model_in_the_original_workspace_does_not_mark_user_content_unsaved() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir(&root).unwrap();
    let source = "import cadquery as cq\ndef plate(): return cq.Workplane().box(2,3,4)\n";
    std::fs::write(root.join("geometry.py"), source).unwrap();
    portable::initialize_project_browser(&root).unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let opened = core.open_project(root, "user").await.unwrap();
    let job = core
        .work_on_project_builder(
            &opened.workspace_id,
            &opened.base_revision,
            "geometry.py",
            "plate",
            "user",
        )
        .await
        .unwrap();
    assert_eq!(
        done(&core, &opened.workspace_id, job).await.state,
        JobState::Succeeded
    );
    let project = core.get_project(&opened.workspace_id).await.unwrap();
    assert!(core.model_parent(&opened.workspace_id).await.is_none());
    assert_eq!(
        project["dirty"], true,
        "the internal revision changes while browsing"
    );
    assert_eq!(
        project["unsaved_changes"], false,
        "view adapters are not user edits"
    );
    let base = project["revision"].as_str().unwrap();
    let draft = core
        .update_source(
            &opened.workspace_id,
            base,
            None,
            std::collections::BTreeMap::from([(
                "geometry.py".into(),
                source.replace("box(2,3,4)", "box(3,3,4)"),
            )]),
            "user",
        )
        .await
        .unwrap();
    let job = core
        .build(&opened.workspace_id, base, &draft.id, "user")
        .await
        .unwrap();
    let job = done(&core, &opened.workspace_id, job).await;
    assert_eq!(job.state, JobState::Succeeded);
    assert_eq!(
        core.get_project(&opened.workspace_id).await.unwrap()["unsaved_changes"],
        true
    );
    core.save_project_folder(
        &opened.workspace_id,
        job.resulting_revision.as_deref().unwrap(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        core.get_project(&opened.workspace_id).await.unwrap()["unsaved_changes"],
        false
    );
}
