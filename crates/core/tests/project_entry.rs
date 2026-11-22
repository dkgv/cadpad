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
    let built = done(&core, &plate, job).await;
    assert_eq!(built.state, JobState::Succeeded);
    let (reused, cached) = core
        .open_project_model_reusing(
            &project.workspace_id,
            &project.base_revision,
            "geometry.py",
            "plate",
            "user",
            Some(&plate),
        )
        .await
        .unwrap();
    assert_eq!(reused, plate);
    assert_eq!(cached.id, built.id);
    let state: Value = serde_json::from_slice(
        &std::fs::read(
            temp.path()
                .join("app/workspaces")
                .join(&plate)
                .join("workspace.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let revisions = state["revisions"].as_object().unwrap();
    assert_eq!(revisions.len(), 2);
    assert!(!revisions.contains_key(&project.base_revision));
    assert_eq!(state["model_source_revision"], project.base_revision);
    let current = &revisions[state["current"].as_str().unwrap()];
    assert!(revisions[current["parent"].as_str().unwrap()]["parent"].is_null());
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

#[tokio::test]
async fn opening_a_new_disk_model_uses_disk_sources_without_changing_project_history() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("original.py"), "# original source\n").unwrap();
    portable::initialize_project_browser(&root).unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let project = core.open_project(root.clone(), "user").await.unwrap();
    std::fs::create_dir(root.join("new")).unwrap();
    let source = "import cadquery as cq\ndef plate(): return cq.Workplane().box(2,3,4)\n";
    std::fs::write(root.join("new/geometry.py"), source).unwrap();
    std::fs::remove_file(root.join("original.py")).unwrap();
    let (view, job) = core
        .open_project_model(
            &project.workspace_id,
            &project.base_revision,
            "new/geometry.py",
            "plate",
            "user",
        )
        .await
        .unwrap();
    assert_eq!(done(&core, &view, job).await.state, JobState::Succeeded);
    let model = core.get_project(&view).await.unwrap();
    assert_eq!(
        model["model"]["parts"][0]["bounds"]["size"],
        serde_json::json!([2.0, 3.0, 4.0])
    );
    assert_eq!(
        core.read_source(
            &view,
            model["revision"].as_str().unwrap(),
            "new/geometry.py"
        )
        .await
        .unwrap()["source"],
        source
    );
    assert!(!core.model_source_dirty(&view).await);
    assert!(
        core.read_source(&project.workspace_id, &project.base_revision, "original.py")
            .await
            .is_ok()
    );
    assert!(
        core.read_source(
            &project.workspace_id,
            &project.base_revision,
            "new/geometry.py"
        )
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(root.join("new/geometry.py")).unwrap(),
        source
    );
}

#[tokio::test]
async fn nested_step_and_stl_exports_are_discovered_and_open_in_model_tabs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    let folder = "parts/frame/v4/final/assembled";
    std::fs::create_dir_all(root.join(folder)).unwrap();
    std::fs::write(
        root.join("geometry.py"),
        "import cadquery as cq\ndef plate(): return cq.Workplane().box(2, 3, 4)\n",
    )
    .unwrap();
    portable::initialize_project_browser(&root).unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let project = core.open_project(root.clone(), "user").await.unwrap();
    let (parent, base) = (&project.workspace_id, &project.base_revision);
    let before = core
        .project_source_catalog(parent, base, None)
        .await
        .unwrap();
    let (source, job) = core
        .open_project_model(parent, base, "geometry.py", "plate", "user")
        .await
        .unwrap();
    assert_eq!(done(&core, &source, job).await.state, JobState::Succeeded);
    let current = core.get_project(&source).await.unwrap();
    let revision = current["revision"].as_str().unwrap();
    let native_revision = revision;

    let context = cadpad_core::tools::ToolContext { actor: "user".into() };
    let native_part = &current["model"]["parts"][0];
    let source_trace = core
        .dispatch(
            &context,
            "trace_part_source",
            serde_json::json!({"workspace_id":source,"revision":revision,"instance_id":native_part["instance_id"]}),
        )
        .await
        .unwrap();
    assert_eq!(source_trace["publication"], native_part["source"]);
    assert!(source_trace["publication"]["line"].is_u64());
    assert!(source_trace["asset_path"].is_null());

    for (format, extension) in [("STEP", "step"), ("STL", "STL")] {
        let export = core
            .query(
                &source,
                revision,
                "export",
                serde_json::json!({"format":format}),
            )
            .await
            .unwrap();
        let path = root.join(folder).join(format!("plate.{extension}"));
        core.freeze_export(&source, revision, export["handle"].as_str().unwrap())
            .await
            .unwrap()
            .save_to(&path)
            .unwrap();
    }
    let index = core
        .project_source_catalog(parent, base, before["fingerprint"].as_str())
        .await
        .unwrap();
    for extension in ["step", "STL"] {
        let entry = format!("{folder}/plate.{extension}");
        let file = index["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == entry)
            .unwrap();
        let builder = file["builders"][0]["id"].as_str().unwrap();
        let (tab, job) = core
            .open_project_model(parent, base, &entry, builder, "user")
            .await
            .unwrap();
        let job = done(&core, &tab, job).await;
        assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
        let model = core.get_project(&tab).await.unwrap();
        assert_eq!(model["model_entry"]["entry"], entry);
        assert_eq!(model["model"]["parts"].as_array().unwrap().len(), 1);
        let trace = core
            .dispatch(
                &context,
                "trace_part_source",
                serde_json::json!({"workspace_id":tab,"revision":model["revision"],"instance_id":"model"}),
            )
            .await
            .unwrap();
        assert_eq!(trace["asset_path"], entry);
        assert!(trace["publication"].is_null());
        if extension == "step" {
            let cad_revision = &model["revision"];
            let imported = core.dispatch(&context, "trace_cad_edge", serde_json::json!({
                "workspace_id":tab,"revision":cad_revision,
                "entities":[{"revision":cad_revision,"instance_id":"model","entity_id":"edge:0"}]
            })).await.unwrap();
            assert_eq!(imported["edges"][0]["incident_face_count"], 2);
            assert!(imported["edges"][0]["operation"].is_null());
        }
        if extension == "STL" {
            let state: Value = serde_json::from_slice(
                &std::fs::read(
                    temp.path()
                        .join("app")
                        .join("workspaces")
                        .join(&tab)
                        .join("workspace.json"),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(state["revisions"].as_object().unwrap().len(), 2);
            assert_eq!(state["model_source_revision"], *base);
            let part = &model["model"]["parts"][0];
            assert_eq!(part["accuracy"], "mesh");
            assert_eq!(
                part["face_count"], 6,
                "box triangles join into six planar surfaces"
            );
            assert_eq!(part["edge_count"], 12, "coplanar diagonals are excluded");
            let revision = model["revision"].as_str().unwrap();
            let edge =
                serde_json::json!({"revision":revision,"instance_id":"model","entity_id":"edge:0"});
            let matched = core.dispatch(&context, "match_mesh_edge", serde_json::json!({
                "workspace_id":tab,"revision":revision,"entities":[edge],
                "target_workspace_id":source,"target_revision":native_revision,"target_instance_id":native_part["instance_id"]
            })).await.unwrap();
            assert_eq!(matched["matches"][0]["candidate_count"], 1);
            let candidate = &matched["matches"][0]["candidates"][0];
            assert!(candidate["max_sample_distance_mm"].as_f64().unwrap() < 1e-6);
            let native_trace = core.dispatch(&context, "trace_cad_edge", serde_json::json!({
                "workspace_id":source,"revision":native_revision,"entities":[candidate["reference"]]
            })).await.unwrap();
            assert_eq!(native_trace["edges"][0]["incident_face_count"], 2);
            assert_eq!(native_trace["edges"][0]["operation"], serde_json::json!({
                "file":"geometry.py","line":2,"operation":"box"
            }));
            let trace_args = serde_json::json!({"workspace_id":tab,"revision":revision,"entities":[edge]});
            let trace = core.dispatch(&context, "trace_mesh_edges", trace_args.clone()).await.unwrap();
            assert_eq!(trace["accuracy"], "mesh");
            let details = &trace["edges"][0];
            assert_eq!(details["reference"], edge);
            assert_eq!(details["classification"], "crease");
            assert_eq!(details["plane_angle_degrees"], 90.);
            assert_eq!(details["incident_triangle_count"], 2);
            assert_ne!(details["incident_triangles"][0]["face_id"], details["incident_triangles"][1]["face_id"]);
            let mut stale = trace_args;
            stale["entities"][0]["revision"] = serde_json::json!("old");
            assert_eq!(core.dispatch(&context, "trace_mesh_edges", stale).await.unwrap_err().code(), "STALE_ENTITY");
            let inspected_edge = core
                .query(
                    &tab,
                    revision,
                    "inspect",
                    serde_json::json!({"entities":[edge]}),
                )
                .await
                .unwrap();
            let _: cadpad_core::geometry::InspectionReply =
                serde_json::from_value(inspected_edge.clone()).unwrap();
            let inspected_edge = &inspected_edge["entities"][0];
            assert_eq!(inspected_edge["kind"], "edge");
            assert_eq!(inspected_edge["anchor"]["kind"], "local_point");
            assert!([2., 3., 4.].contains(&inspected_edge["length"].as_f64().unwrap()));
            let entities = serde_json::json!([
                {"revision":revision,"instance_id":"model","entity_id":"face:0"},
                {"revision":revision,"instance_id":"model","entity_id":"face:1"}
            ]);
            let result = core
                .query(
                    &tab,
                    revision,
                    "inspect",
                    serde_json::json!({"entities":entities}),
                )
                .await
                .unwrap();
            let _: cadpad_core::geometry::InspectionReply =
                serde_json::from_value(result.clone()).unwrap();
            assert_eq!(result["accuracy"], "mesh");
            assert_eq!(result["entities"][0]["anchor"]["kind"], "local_point");
            assert!(result["entities"][0]["area"].as_f64().unwrap() > 0.);
            let distance = core
                .query(
                    &tab,
                    revision,
                    "inspect",
                    serde_json::json!({
                        "query":"distance","entities":entities
                    }),
                )
                .await
                .unwrap();
            assert!(
                (distance["distance"].as_f64().unwrap() - 2.).abs() < 1e-6,
                "{distance}"
            );
            let mixed_distance = core
                .query(
                    &tab,
                    revision,
                    "inspect",
                    serde_json::json!({
                        "query":"distance","entities":[edge, entities[0]]
                    }),
                )
                .await
                .unwrap();
            assert!(mixed_distance["distance"].as_f64().unwrap().is_finite());
            for format in ["STL", "STEP"] {
                let exported = core
                    .query(
                        &tab,
                        revision,
                        "export",
                        serde_json::json!({"format":format}),
                    )
                    .await
                    .unwrap();
                assert_eq!(exported["format"], format);
            }
            let handle = part["mesh"]["buffers"]["vertices"]["handle"]
                .as_str()
                .unwrap();
            std::fs::remove_file(
                temp.path()
                    .join("app")
                    .join("artifacts")
                    .join(&tab)
                    .join(revision)
                    .join(handle),
            )
            .unwrap();
            let repair = core
                .repair_display(
                    "user",
                    cadpad_core::recovery::RepairDisplay {
                        workspace_id: tab.clone(),
                        revision: revision.into(),
                    },
                )
                .await
                .unwrap();
            let repair = done(&core, &tab, repair).await;
            assert_eq!(
                repair.state,
                JobState::Succeeded,
                "{:?}",
                repair.diagnostics
            );
            assert_eq!(core.get_project(&tab).await.unwrap()["revision"], revision);
        }
        let (reopened, cached_job) = core
            .open_project_model_reusing(parent, base, &entry, builder, "user", Some(&tab))
            .await
            .unwrap();
        assert_eq!(reopened, tab);
        assert_eq!(cached_job.id, job.id);
        if extension == "step" {
            let file = root.join(&entry);
            let mut bytes = std::fs::read(&file).unwrap();
            bytes.push(b'\n');
            std::fs::write(file, bytes).unwrap();
            let (updated, new_job) = core
                .open_project_model_reusing(parent, base, &entry, builder, "user", Some(&tab))
                .await
                .unwrap();
            assert_ne!(updated, tab);
            assert_eq!(
                done(&core, &updated, new_job).await.state,
                JobState::Succeeded
            );
        }
    }
    assert_eq!(core.get_project(parent).await.unwrap()["revision"], *base);
    std::fs::remove_file(root.join(folder).join("plate.STL")).unwrap();
    let updated = core
        .project_source_catalog(parent, base, index["fingerprint"].as_str())
        .await
        .unwrap();
    assert_eq!(updated["files"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn standalone_stl_uses_project_snapshot_and_checks_the_selected_file_on_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir_all(root.join("assets")).unwrap();
    let source = root.join("geometry.py");
    std::fs::write(&source, "# saved snapshot\n").unwrap();
    let mesh = |width| {
        format!(
            "solid part\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex {width} 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid\n"
        )
    };
    let entry = "assets/part.stl";
    std::fs::write(root.join(entry), mesh(1)).unwrap();
    portable::initialize_project_browser(&root).unwrap();
    std::fs::write(
        root.join("cadpad_view_0.py"),
        "# Generated Cadpad entry point.\n# Project model file: \"assets/part.stl\"\nshow(import_stl(\"part.stl\"), name=\"model\", kind=\"surface\")\n",
    ).unwrap();
    let core = Core::open(
        temp.path().join("app"),
        WorkerConfig::bundled("unused".into()),
    )
    .await
    .unwrap();
    let project = core.open_project(root.clone(), "user").await.unwrap();
    let (parent, base) = (&project.workspace_id, &project.base_revision);

    // A half-written sibling source file cannot prevent opening a mesh.
    std::fs::write(&source, [0xff]).unwrap();
    let (tab, job) = core
        .open_project_model(parent, base, entry, "$file", "user")
        .await
        .unwrap();
    assert_eq!(
        job.state,
        JobState::Succeeded,
        "the first standalone STL open returns its committed mesh"
    );
    assert_eq!(
        core.read_source(&tab, job.snapshot_id.as_deref().unwrap(), "geometry.py")
            .await
            .unwrap()["source"],
        "# saved snapshot\n"
    );
    let (cached, cached_job) = core
        .open_project_model_reusing(parent, base, entry, "$file", "user", Some(&tab))
        .await
        .unwrap();
    assert_eq!(cached, tab);
    assert_eq!(cached_job.id, job.id);

    std::fs::write(root.join(entry), mesh(3)).unwrap();
    let preview = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let received = preview.clone();
    let (updated, job) = core
        .open_project_model_streaming(
            parent,
            base,
            entry,
            "$file",
            "user",
            Some(&tab),
            Some(std::sync::Arc::new(move |chunk| {
                received.lock().unwrap().push(chunk)
            })),
        )
        .await
        .unwrap();
    let preview = preview.lock().unwrap();
    let expected: Vec<_> = [0.0f32, 0., 0., 3., 0., 0., 0., 1., 0.]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    assert_eq!(preview.as_slice(), &[expected]);
    drop(preview);
    assert_ne!(updated, tab);
    assert_eq!(done(&core, &updated, job).await.state, JobState::Succeeded);
    assert_eq!(
        core.get_project(&updated).await.unwrap()["model"]["parts"][0]["bounds"]["size"][0],
        3.
    );
    assert_eq!(
        core.get_project(&updated).await.unwrap()["model"]["parts"][0]["edge_count"],
        3
    );
    assert_eq!(core.get_project(parent).await.unwrap()["revision"], *base);
}

/// Run with CADPAD_BENCH_PROJECT and CADPAD_BENCH_ENTRY set to an existing project and STL.
#[tokio::test]
#[ignore = "explicit real-file benchmark"]
async fn benchmark_stl_open() {
    use serde_json::json;
    use std::{
        collections::BTreeMap,
        sync::{Arc, OnceLock},
        time::Instant,
    };
    use tokio::task::JoinSet;

    let root = PathBuf::from(std::env::var_os("CADPAD_BENCH_PROJECT").expect("project directory"));
    let entry = std::env::var("CADPAD_BENCH_ENTRY").expect("project-relative STL path");
    assert!(
        PathBuf::from(&entry)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("stl"))
    );
    let runs = std::env::var("CADPAD_BENCH_RUNS").map_or(5, |value| value.parse().unwrap());
    assert!(runs > 0);

    let mut samples = BTreeMap::<_, Vec<(f64, f64)>>::new();
    for run in 1..=runs {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::open(
            temp.path().join("app"),
            WorkerConfig::bundled("unused".into()),
        )
        .await
        .unwrap();
        let parent = core.open_project(root.clone(), "benchmark").await.unwrap();
        let mut cached = None;
        for mode in ["new_workspace", "reused_workspace"] {
            let started = Instant::now();
            let first_batch = Arc::new(OnceLock::new());
            let received = first_batch.clone();
            let (view, job) = core
                .open_project_model_streaming(
                    &parent.workspace_id,
                    &parent.base_revision,
                    &entry,
                    "$file",
                    "benchmark",
                    cached.as_deref(),
                    Some(Arc::new(move |_| {
                        received.get_or_init(|| started.elapsed().as_secs_f64() * 1000.0);
                    })),
                )
                .await
                .unwrap();
            let ready = done(&core, &view, job).await;
            assert_eq!(ready.state, JobState::Succeeded);
            if let Some(expected) = &cached {
                assert_eq!(&view, expected);
            }

            let project = core.get_project(&view).await.unwrap();
            let metadata_ms = started.elapsed().as_secs_f64() * 1000.0;
            let revision = project["revision"].as_str().unwrap();
            let mut handles = BTreeMap::new();
            for part in project["model"]["parts"].as_array().unwrap() {
                for info in part["mesh"]["buffers"].as_object().unwrap().values() {
                    handles.insert(
                        info["handle"].as_str().unwrap(),
                        info["bytes"].as_u64().unwrap(),
                    );
                }
            }
            assert!(!handles.is_empty());
            let mut reads = JoinSet::new();
            for (handle, bytes) in handles {
                let (core, view, revision, handle) = (
                    core.clone(),
                    view.clone(),
                    revision.to_owned(),
                    handle.to_owned(),
                );
                reads.spawn(async move {
                    let data = core.artifact(&view, &revision, &handle).await.unwrap();
                    assert_eq!(data.len() as u64, bytes);
                    bytes
                });
            }
            let mut bytes = 0;
            while let Some(result) = reads.join_next().await {
                bytes += result.unwrap();
            }
            let buffers_ms = started.elapsed().as_secs_f64() * 1000.0;
            samples
                .entry(mode)
                .or_default()
                .push((metadata_ms, buffers_ms));
            println!(
                "{}",
                json!({"entry":entry,"run":run,"mode":mode,"bufferBytes":bytes,
                       "openToFirstPreviewBatchMs":first_batch.get(),
                       "openToMetadataMs":metadata_ms,"openToBuffersMs":buffers_ms})
            );
            cached = Some(view);
        }
    }
    fn median(mut values: Vec<f64>) -> f64 {
        values.sort_by(f64::total_cmp);
        let middle = values.len() / 2;
        (values[middle] + values[(values.len() - 1) / 2]) / 2.0
    }
    for (mode, timings) in samples {
        println!(
            "{}",
            json!({"entry":entry,"runs":runs,"mode":mode,
                   "medianOpenToMetadataMs":median(timings.iter().map(|sample| sample.0).collect()),
                   "medianOpenToBuffersMs":median(timings.iter().map(|sample| sample.1).collect())})
        );
    }
}

#[tokio::test]
async fn project_requirements_are_shared_by_model_tabs_and_bound_to_their_model() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir(&root).unwrap();
    let source = "import cadquery as cq\ndef plate(): return cq.Workplane().box(2,3,4)\ndef pin(): return cq.Workplane().box(1,1,8)\n";
    std::fs::write(root.join("geometry.py"), source).unwrap();
    portable::initialize_project_browser(&root).unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let project = core.open_project(root.clone(), "user").await.unwrap();
    let mut views = Vec::new();
    for builder in ["plate", "pin"] {
        let (view, job) = core
            .open_project_model(&project.workspace_id, &project.base_revision, "geometry.py", builder, "user")
            .await
            .unwrap();
        assert_eq!(done(&core, &view, job).await.state, JobState::Succeeded);
        views.push(view);
    }
    let (plate, pin) = (&views[0], &views[1]);
    let plate_project = core.get_project(plate).await.unwrap();
    let plate_revision = plate_project["revision"].as_str().unwrap().to_owned();
    let pin_revision = core.get_project(pin).await.unwrap()["revision"].as_str().unwrap().to_owned();
    let body = plate_project["model"]["parts"][0]["instance_id"].as_str().unwrap().to_owned();
    let tests = |workspace: &str, revision: &str, catalog: Option<&str>, command: Value| {
        let request = serde_json::json!({"workspace_id":workspace,"revision":revision,"expected_catalog":catalog,"command":command});
        let core = core.clone();
        async move { core.control_motion_tests("user", serde_json::from_value(request).unwrap()).await }
    };
    let listed = tests(plate, &plate_revision, None, serde_json::json!({"action":"list"})).await.unwrap();
    assert_eq!(listed.project_workspace, project.workspace_id);
    // Saved from the plate tab without naming a model: the plate is recorded.
    let definition = serde_json::json!({
        "name":"Plate falls","intent":"The plate drops at least 1 mm",
        "initial_coordinates":{},
        "simulation":{"gravity_m_s2":[0,0,-9.81],"bodies":{body.clone():{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.1},"friction":0.5,"restitution":0}}},
        "actions":[{"ticks":10,"motors":[],"loads":{}}],
        "checks":[{"type":"displacement","body":body,"direction":[0,0,-1],"minimum_mm":1}]
    });
    let saved = tests(plate, &plate_revision, Some(&listed.catalog), serde_json::json!({"action":"upsert","id":"fall","definition":definition}))
        .await
        .unwrap();
    let model = saved.tests[0].definition.model.clone().unwrap();
    assert_eq!((model.entry.as_str(), model.builder.as_str()), ("geometry.py", "plate"));
    // One catalog for the project: the pin tab and the project itself list it too.
    let from_pin = tests(pin, &pin_revision, None, serde_json::json!({"action":"list"})).await.unwrap();
    assert_eq!(from_pin.catalog, saved.catalog);
    assert_eq!(from_pin.tests[0].id, "fall");
    let from_project = core
        .control_motion_tests("user", serde_json::from_value(serde_json::json!({"workspace_id":project.workspace_id,"expected_catalog":null,"command":{"action":"list"},"model_views":[plate]})).unwrap())
        .await
        .unwrap();
    assert_eq!(from_project.catalog, saved.catalog);
    assert!(from_project.tests[0].design_compared, "listed plate workspace is compared");
    // A plate requirement cannot run against the pin.
    let Err(wrong) = tests(pin, &pin_revision, Some(&saved.catalog), serde_json::json!({"action":"run","id":"fall"})).await else {
        panic!("a plate requirement must not run against the pin");
    };
    assert_eq!(wrong.code(), "INVALID_MOTION_TEST");
    // Model tabs no longer keep their own copy, and saving a tab writes the project's catalog.
    let stored: Value = serde_json::from_slice(&std::fs::read(temp.path().join("app/workspaces").join(plate).join("workspace.json")).unwrap()).unwrap();
    assert!(stored["motion_tests"].as_object().is_none_or(|tests| tests.is_empty()));
    assert_eq!(core.get_project(pin).await.unwrap()["unsaved_changes"], true, "project requirements changed");
    core.save_project_folder(plate, &plate_revision, None).await.unwrap();
    assert_eq!(core.get_project(pin).await.unwrap()["unsaved_changes"], false, "saved through the plate tab");
    let other = Core::open(temp.path().join("app-b"), config()).await.unwrap();
    let reopened = other.open_project(root.clone(), "user").await.unwrap();
    let imported = other
        .control_motion_tests("user", serde_json::from_value(serde_json::json!({"workspace_id":reopened.workspace_id,"expected_catalog":null,"command":{"action":"list"}})).unwrap())
        .await
        .unwrap();
    assert_eq!(imported.tests[0].definition.model.as_ref().unwrap().builder, "plate");
}
