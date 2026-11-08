use cadpad_core::{Core, Job, JobState, portable, worker::WorkerConfig};
use serde_json::json;
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
async fn done(core: &Core, ws: &str, job: Job) -> Job {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let state = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(state.state, JobState::Queued | JobState::Running) {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap()
}
const SOURCE: &str = "import cadquery as cq\nw=parameter('width',10,minimum=1,maximum=100)\nshow(cq.Workplane().box(w,20,30),name='box')";
async fn initial(core: &Core) -> (String, String) {
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
            BTreeMap::from([("model.py".into(), SOURCE.into())]),
            "user",
        )
        .await
        .unwrap();
    let job = done(
        core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    (ws, job.resulting_revision.unwrap())
}

#[tokio::test]
async fn portable_save_open_assets_history_and_undo_use_source_authority() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app-a"), config())
        .await
        .unwrap();
    let (ws, first) = initial(&core).await;
    let exported = core
        .query(&ws, &first, "export", json!({"format":"STEP"}))
        .await
        .unwrap();
    let step = core
        .artifact(&ws, &first, exported["handle"].as_str().unwrap())
        .await
        .unwrap();
    let draft = core
        .stage_asset(&ws, &first, None, "parts/input.step", step, "user")
        .await
        .unwrap();
    let source =
        "shape=import_step('parts/input.step')\nshow(shape,name='imported',position=(40,0,0))";
    core.update_source(
        &ws,
        &first,
        Some(&draft.id),
        BTreeMap::from([("model.py".into(), source.into())]),
        "user",
    )
    .await
    .unwrap();
    let job = done(
        &core,
        &ws,
        core.build(&ws, &first, &draft.id, "user").await.unwrap(),
    )
    .await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    let second = job.resulting_revision.unwrap();
    let directory = temp.path().join("portable-project");
    core.save_project(&ws, &second, Some(directory.clone()))
        .await
        .unwrap();
    assert!(directory.join("project.json").is_file());
    assert!(directory.join("assets/parts/input.step").is_file());
    assert!(
        directory
            .join("revisions")
            .join(&first)
            .join("model.py")
            .is_file()
    );
    let portable = portable::read_project(&directory).unwrap();
    assert_eq!(portable.inputs.files["model.py"], source);
    assert!(
        !serde_json::to_string(&portable.inputs)
            .unwrap()
            .contains(temp.path().to_str().unwrap())
    );
    assert!(
        !core.get_project(&ws).await.unwrap()["dirty"]
            .as_bool()
            .unwrap()
    );
    core.save_project(&ws, &second, None).await.unwrap();
    // A second application data root proves that no original meshes or native
    // worker state are required to reconstruct the project.
    let other = Core::open(temp.path().join("app-b"), config())
        .await
        .unwrap();
    let opened = other.open_project(directory.clone(), "user").await.unwrap();
    assert!(
        other.get_project(&opened.workspace_id).await.unwrap()["activity"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let built = done(
        &other,
        &opened.workspace_id,
        other
            .build(
                &opened.workspace_id,
                &opened.base_revision,
                &opened.draft.id,
                "user",
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(built.state, JobState::Succeeded, "{:?}", built.diagnostics);
    let revision = built.resulting_revision.unwrap();
    let result=other.query(&opened.workspace_id,&revision,"inspect",json!({"entities":[{"revision":revision,"instance_id":"imported","entity_id":"part:0"}]})).await.unwrap();
    assert_eq!(result["entities"][0]["world_center"][0], 40.0);
    assert_eq!(result["entities"][0]["volume"], 6000.0);
    let undo = done(
        &other,
        &opened.workspace_id,
        other
            .undo(&opened.workspace_id, &revision, "user")
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(undo.state, JobState::Succeeded, "{:?}", undo.diagnostics);
    // Reopened history includes the older original model, not only this session's import.
    let undo = done(
        &other,
        &opened.workspace_id,
        other
            .undo(
                &opened.workspace_id,
                undo.resulting_revision.as_deref().unwrap(),
                "user",
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(undo.state, JobState::Succeeded, "{:?}", undo.diagnostics);
}

#[tokio::test]
async fn filesystem_edits_are_inert_until_reviewed_snapshot_builds() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, base) = initial(&core).await;
    let directory = temp.path().join("project");
    core.save_project(&ws, &base, Some(directory.clone()))
        .await
        .unwrap();
    let marker = temp.path().join("must-not-run");
    let poison = format!(
        "from pathlib import Path\nPath({:?}).write_text('executed')\nraise ValueError('external candidate')",
        marker.to_str().unwrap()
    );
    std::fs::write(directory.join("model.py"), &poison).unwrap();
    let changes = core.scan_external_changes(&ws).await.unwrap();
    assert_eq!(changes["changed"], true);
    assert_eq!(changes["changes"]["model.py"], "modified");
    assert!(!marker.exists());
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], base);
    assert_eq!(
        core.save_project(&ws, &base, None)
            .await
            .unwrap_err()
            .code(),
        "REVISION_CONFLICT"
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("model.py")).unwrap(),
        poison
    );
    let draft = core
        .stage_external_changes(&ws, &base, changes["fingerprint"].as_str().unwrap(), "user")
        .await
        .unwrap();
    assert!(!marker.exists());
    // Later changes on disk cannot alter the already reviewed draft snapshot.
    std::fs::write(directory.join("model.py"), SOURCE).unwrap();
    assert_eq!(
        core.stage_external_changes(&ws, &base, changes["fingerprint"].as_str().unwrap(), "user")
            .await
            .unwrap_err()
            .code(),
        "REVISION_CONFLICT"
    );
    let built = done(
        &core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    assert_eq!(built.state, JobState::Failed);
    assert!(marker.exists());
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], base);
    // A successful explicit reload can then be saved normally.
    let replacement = SOURCE.replace("box(w,20,30)", "box(w,25,30)");
    std::fs::write(directory.join("model.py"), &replacement).unwrap();
    let changes = core.scan_external_changes(&ws).await.unwrap();
    let draft = core
        .stage_external_changes(&ws, &base, changes["fingerprint"].as_str().unwrap(), "user")
        .await
        .unwrap();
    let built = done(
        &core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    assert_eq!(built.state, JobState::Succeeded, "{:?}", built.diagnostics);
    core.save_project(&ws, built.resulting_revision.as_deref().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(
        core.scan_external_changes(&ws).await.unwrap()["changed"],
        false
    );
}

#[tokio::test]
async fn portable_paths_reject_symlinks_escapes_and_modified_snapshots() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, base) = initial(&core).await;
    let directory = temp.path().join("project");
    core.save_project(&ws, &base, Some(directory.clone()))
        .await
        .unwrap();
    assert_eq!(
        core.save_project(&ws, &base, Some(temp.path().to_path_buf()))
            .await
            .unwrap_err()
            .code(),
        "REVISION_CONFLICT"
    );
    for path in [
        "/abs.step",
        "../a.step",
        "a//b.step",
        "a/./b.step",
        "C:/a.step",
        "CON.step",
        "a\\b.step",
    ] {
        assert!(
            core.stage_asset(&ws, &base, None, path, vec![], "user")
                .await
                .is_err(),
            "{path}"
        );
    }
    std::fs::write(
        directory.join("revisions").join(&base).join("model.py"),
        "# tampered",
    )
    .unwrap();
    assert_eq!(
        core.save_project(&ws, &base, None)
            .await
            .unwrap_err()
            .code(),
        "REVISION_CONFLICT"
    );
    assert_eq!(
        core.open_project(directory.clone(), "user")
            .await
            .unwrap_err()
            .code(),
        "REVISION_CONFLICT"
    );
    #[cfg(unix)]
    {
        std::fs::remove_file(directory.join("model.py")).unwrap();
        std::os::unix::fs::symlink(temp.path().join("outside.py"), directory.join("model.py"))
            .unwrap();
        assert_eq!(
            core.scan_external_changes(&ws).await.unwrap_err().code(),
            "INVALID_REQUEST"
        );
    }
}

#[tokio::test]
async fn undo_and_save_retain_inactive_files_without_resurrecting_them() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, base) = initial(&core).await;
    let directory = temp.path().join("project");
    core.save_project(&ws, &base, Some(directory.clone()))
        .await
        .unwrap();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("helper.py".into(), "VALUE = 42\n".into())]),
            "user",
        )
        .await
        .unwrap();
    let built = done(
        &core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    let revision = built.resulting_revision.unwrap();
    core.save_project(&ws, &revision, None).await.unwrap();
    let undo = done(&core, &ws, core.undo(&ws, &revision, "user").await.unwrap()).await;
    core.save_project(&ws, undo.resulting_revision.as_deref().unwrap(), None)
        .await
        .unwrap();
    assert!(
        directory.join("helper.py").exists(),
        "Save must preserve inactive source on disk"
    );
    let loaded = portable::read_project(&directory).unwrap();
    assert!(!loaded.inputs.files.contains_key("helper.py"));
    assert!(loaded.manifest.retained_sources.contains_key("helper.py"));
    assert_eq!(
        core.scan_external_changes(&ws).await.unwrap()["changed"],
        false
    );
    std::fs::write(directory.join("helper.py"), "VALUE = 43\n").unwrap();
    assert_eq!(
        core.scan_external_changes(&ws).await.unwrap()["changes"]["helper.py"],
        "added"
    );
}

#[tokio::test]
async fn plain_multifile_python_and_dependency_mismatches_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("python-project");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("helpers.py"), "WIDTH = 42\n").unwrap();
    std::fs::write(directory.join("model.py"),"import cadquery as cq\nfrom helpers import WIDTH\nshow(cq.Workplane().box(WIDTH,20,30),name='plain')").unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let opened = core
        .open_python(&directory.join("model.py"), "user")
        .await
        .unwrap();
    assert_eq!(opened.draft.inputs.files.len(), 2);
    let built = done(
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
    assert_eq!(built.state, JobState::Succeeded, "{:?}", built.diagnostics);
    let revision = built.resulting_revision.unwrap();
    let project = core.get_project(&opened.workspace_id).await.unwrap();
    assert_eq!(project["model"]["parts"][0]["bounds"]["size"][0], 42.0);
    let saved = temp.path().join("saved");
    core.save_project(&opened.workspace_id, &revision, Some(saved.clone()))
        .await
        .unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(saved.join("project.json")).unwrap()).unwrap();
    manifest["dependencies"]["cadquery"] = json!("0.0.invalid");
    std::fs::write(
        saved.join("project.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let changes = core
        .scan_external_changes(&opened.workspace_id)
        .await
        .unwrap();
    let draft = core
        .stage_external_changes(
            &opened.workspace_id,
            &revision,
            changes["fingerprint"].as_str().unwrap(),
            "user",
        )
        .await
        .unwrap();
    let built = done(
        &core,
        &opened.workspace_id,
        core.build(&opened.workspace_id, &revision, &draft.id, "user")
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(built.state, JobState::Failed);
    assert_eq!(built.diagnostics["code"], "UNSUPPORTED_OPERATION");
    assert_eq!(
        core.get_project(&opened.workspace_id).await.unwrap()["revision"],
        revision
    );
}

#[tokio::test]
async fn repeated_undo_moves_back_through_designs_and_survives_portable_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, mut revision) = initial(&core).await;
    for width in [20.0, 30.0] {
        let built = done(
            &core,
            &ws,
            core.set_parameter(&ws, &revision, "width", width, "mm", "user")
                .await
                .unwrap(),
        )
        .await;
        revision = built.resulting_revision.unwrap();
    }
    for expected in [20.0, 10.0] {
        let undo = done(&core, &ws, core.undo(&ws, &revision, "user").await.unwrap()).await;
        assert_eq!(undo.state, JobState::Succeeded, "{:?}", undo.diagnostics);
        revision = undo.resulting_revision.unwrap();
        assert_eq!(
            core.get_project(&ws).await.unwrap()["parameters"]["width"]["value"],
            expected
        );
    }
    let directory = temp.path().join("saved");
    core.save_project(&ws, &revision, Some(directory.clone()))
        .await
        .unwrap();
    let other = Core::open(temp.path().join("other-app"), config())
        .await
        .unwrap();
    let opened = other.open_project(directory.clone(), "user").await.unwrap();
    let rebuilt = done(
        &other,
        &opened.workspace_id,
        other
            .build(
                &opened.workspace_id,
                &opened.base_revision,
                &opened.draft.id,
                "user",
            )
            .await
            .unwrap(),
    )
    .await;
    let undo = done(
        &other,
        &opened.workspace_id,
        other
            .undo(
                &opened.workspace_id,
                rebuilt.resulting_revision.as_deref().unwrap(),
                "user",
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(undo.state, JobState::Succeeded, "{:?}", undo.diagnostics);
    let empty = undo.resulting_revision.unwrap();
    assert_eq!(
        other.get_project(&opened.workspace_id).await.unwrap()["model"]["parts"],
        json!([])
    );
    other
        .save_project(&opened.workspace_id, &empty, None)
        .await
        .unwrap();
    // Empty restoration gains the dependency inventory but remains reopenable.
    assert!(other.open_project(directory, "user").await.is_ok());
    assert_eq!(
        other
            .undo(&opened.workspace_id, &empty, "user")
            .await
            .unwrap_err()
            .code(),
        "UNSUPPORTED_OPERATION"
    );
}

#[tokio::test]
async fn reopening_reconstructs_saved_source_and_keeps_newer_external_edits_inert() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, base) = initial(&core).await;
    let directory = temp.path().join("project");
    core.save_project(&ws, &base, Some(directory.clone()))
        .await
        .unwrap();
    let marker = temp.path().join("external-must-stay-inert");
    std::fs::write(
        directory.join("model.py"),
        format!(
            "from pathlib import Path\nPath({:?}).write_text('bad')\nraise ValueError('external')",
            marker.to_str().unwrap()
        ),
    )
    .unwrap();
    let other = Core::open(temp.path().join("other"), config())
        .await
        .unwrap();
    let opened = other.open_project(directory, "user").await.unwrap();
    assert_eq!(opened.draft.inputs.files["model.py"], SOURCE);
    let rebuilt = done(
        &other,
        &opened.workspace_id,
        other
            .build(
                &opened.workspace_id,
                &opened.base_revision,
                &opened.draft.id,
                "user",
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        rebuilt.state,
        JobState::Succeeded,
        "{:?}",
        rebuilt.diagnostics
    );
    assert!(!marker.exists());
    assert_eq!(
        other
            .scan_external_changes(&opened.workspace_id)
            .await
            .unwrap()["changed"],
        true
    );
}

#[tokio::test]
async fn invalid_editable_metadata_does_not_block_saved_checkpoint_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, base) = initial(&core).await;
    let directory = temp.path().join("project");
    core.save_project(&ws, &base, Some(directory.clone()))
        .await
        .unwrap();
    std::fs::write(
        directory.join("parameters.json"),
        "{ half-written external edit",
    )
    .unwrap();
    let other = Core::open(temp.path().join("other"), config())
        .await
        .unwrap();
    let opened = other.open_project(directory, "user").await.unwrap();
    let rebuilt = done(
        &other,
        &opened.workspace_id,
        other
            .build(
                &opened.workspace_id,
                &opened.base_revision,
                &opened.draft.id,
                "user",
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        rebuilt.state,
        JobState::Succeeded,
        "{:?}",
        rebuilt.diagnostics
    );
    assert_eq!(
        other
            .scan_external_changes(&opened.workspace_id)
            .await
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );
    assert_eq!(
        other.get_project(&opened.workspace_id).await.unwrap()["model"]["parts"][0]["volume"],
        6000.0
    );
}

#[tokio::test]
async fn existing_cadquery_folder_opens_saves_and_reloads_in_place() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("existing-design");
    std::fs::create_dir_all(directory.join("components")).unwrap();
    let helper = "import cadquery as cq\ndef plate():\n    return cq.Workplane().box(12, 20, 3)\n";
    let source = "from components.plate import plate\nshow_object(plate(), name='plate')\n";
    std::fs::write(directory.join("components/plate.py"), helper).unwrap();
    std::fs::write(directory.join("main.py"), source).unwrap();
    std::fs::write(directory.join("README.md"), "My existing project").unwrap();
    // A pre-existing JSON file is not a Cadpad setting and remains untouched.
    std::fs::write(directory.join("parameters.json"), "{\"unrelated\":true}").unwrap();
    assert_eq!(
        portable::python_entry_points(&directory).unwrap(),
        vec!["components/plate.py", "main.py"]
    );
    portable::initialize_folder(&directory, "main.py").unwrap();
    assert!(directory.join(".cadpad/project.json").is_file());
    assert_eq!(
        std::fs::read_to_string(directory.join("main.py")).unwrap(),
        source
    );
    let core = Core::open(temp.path().join("app-folder"), config())
        .await
        .unwrap();
    let opened = core.open_project(directory.clone(), "user").await.unwrap();
    assert_eq!(opened.draft.inputs.entry_point, "main.py");
    assert!(
        opened
            .draft
            .inputs
            .files
            .contains_key("components/plate.py")
    );
    let built = done(
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
    assert_eq!(built.state, JobState::Succeeded, "{:?}", built.diagnostics);
    let revision = built.resulting_revision.unwrap();
    assert_eq!(
        core.get_project(&opened.workspace_id).await.unwrap()["model"]["parts"][0]["name"],
        "plate"
    );
    let replacement = "from components.plate import plate\nshow_object(plate(), name='updated')\n";
    let draft = core
        .update_source(
            &opened.workspace_id,
            &revision,
            None,
            BTreeMap::from([("main.py".into(), replacement.into())]),
            "user",
        )
        .await
        .unwrap();
    let built = done(
        &core,
        &opened.workspace_id,
        core.build(&opened.workspace_id, &revision, &draft.id, "user")
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(built.state, JobState::Succeeded, "{:?}", built.diagnostics);
    let revision = built.resulting_revision.unwrap();
    core.save_project(&opened.workspace_id, &revision, None)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(directory.join("main.py")).unwrap(),
        replacement
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("components/plate.py")).unwrap(),
        helper
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("README.md")).unwrap(),
        "My existing project"
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("parameters.json")).unwrap(),
        "{\"unrelated\":true}"
    );
    assert!(!directory.join("project.json").exists());
    assert!(!directory.join("revisions").exists());
    assert!(!directory.join(".cadpad/revisions").exists());
    assert!(
        !core
            .scan_external_changes(&opened.workspace_id)
            .await
            .unwrap()["changed"]
            .as_bool()
            .unwrap()
    );
    // Reopening reconstructs from the actual source files, without saved revisions.
    let reopened = core.open_project(directory.clone(), "user").await.unwrap();
    assert_eq!(reopened.draft.inputs.files["main.py"], replacement);
    std::fs::write(
        directory.join("components/plate.py"),
        helper.replace("12", "24"),
    )
    .unwrap();
    assert!(
        core.scan_external_changes(&opened.workspace_id)
            .await
            .unwrap()["changed"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        core.save_project(&opened.workspace_id, &revision, None)
            .await
            .unwrap_err()
            .code(),
        "REVISION_CONFLICT"
    );
}

#[test]
fn folder_setup_requires_an_existing_contained_entry_and_never_replaces_config() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("main.py"), "result = None").unwrap();
    assert!(portable::initialize_folder(temp.path(), "../other.py").is_err());
    assert!(portable::initialize_folder(temp.path(), "missing.py").is_err());
    assert!(!temp.path().join(".cadpad").exists());
    portable::initialize_folder(temp.path(), "main.py").unwrap();
    let manifest = std::fs::read(temp.path().join(".cadpad/project.json")).unwrap();
    assert!(portable::initialize_folder(temp.path(), "main.py").is_err());
    assert_eq!(
        std::fs::read(temp.path().join(".cadpad/project.json")).unwrap(),
        manifest
    );
}

#[tokio::test]
async fn existing_cadquery_result_variable_can_publish_without_cadpad_helpers() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("scripts")).unwrap();
    std::fs::write(
        temp.path().join("scripts/shape.py"),
        "import cadquery as cq\ndef make(): return cq.Workplane().box(2, 3, 4)\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("scripts/main.py"),
        "from shape import make\nif __name__ == '__main__':\n    result = make()\n",
    )
    .unwrap();
    portable::initialize_folder(temp.path(), "scripts/main.py").unwrap();
    let core = Core::open(temp.path().join(".app"), config())
        .await
        .unwrap();
    let opened = core.open_project(temp.path().into(), "user").await.unwrap();
    let built = done(
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
    assert_eq!(built.state, JobState::Succeeded, "{:?}", built.diagnostics);
}

#[tokio::test]
async fn desktop_save_creates_an_ordinary_folder_without_project_history() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, revision) = initial(&core).await;
    let folder = temp.path().join("my-design");
    core.save_project_folder(&ws, &revision, Some(folder.clone()))
        .await
        .unwrap();
    assert!(folder.join(".cadpad/project.json").is_file());
    assert!(folder.join(".cadpad/parameters.json").is_file());
    assert!(!folder.join("project.json").exists());
    assert!(!folder.join("revisions").exists());
    assert!(!folder.join(".cadpad/revisions").exists());
    core.save_project_folder(&ws, &revision, None)
        .await
        .unwrap();
    let reopened = core.open_project(folder, "user").await.unwrap();
    assert_eq!(reopened.draft.inputs.files["model.py"], SOURCE);
}

#[tokio::test]
async fn forgetting_a_workspace_clears_recent_state_without_deleting_project_folders() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, revision) = initial(&core).await;
    let folder = temp.path().join("kept-design");
    core.save_project_folder(&ws, &revision, Some(folder.clone()))
        .await
        .unwrap();
    core.focus_workspace(&ws).await.unwrap();
    assert!(core.list_workspaces().await.contains(&ws));
    assert!(temp.path().join("app/workspaces").join(&ws).is_dir());
    core.forget_workspace(&ws).await.unwrap();
    assert!(!core.list_workspaces().await.contains(&ws));
    assert!(!temp.path().join("app/workspaces").join(&ws).exists());
    assert!(folder.join(".cadpad/project.json").is_file());
    assert!(folder.join("model.py").is_file());
    assert!(core.focused_workspace().await.is_none());
    let other = core.create_workspace("user").await.unwrap();
    core.forget_workspaces().await.unwrap();
    assert!(core.list_workspaces().await.is_empty());
    assert!(!temp.path().join("app/workspaces").join(&other).exists());
}

#[tokio::test]
async fn opening_a_folder_keeps_disk_sources_as_the_current_revision() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("library");
    std::fs::create_dir_all(directory.join("parts")).unwrap();
    let entry = "from parts.bracket import bracket\nshow_object(bracket(), name='bracket')\n";
    let part = "import cadquery as cq\ndef bracket():\n    return cq.Workplane().box(10, 8, 4)\n";
    std::fs::write(directory.join("main.py"), entry).unwrap();
    std::fs::write(directory.join("parts/bracket.py"), part).unwrap();
    portable::initialize_folder(&directory, "main.py").unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let opened = core.open_project(directory, "user").await.unwrap();
    let project = core.get_project(&opened.workspace_id).await.unwrap();
    assert_eq!(project["revision"], opened.base_revision);
    assert_eq!(opened.draft.inputs.files["main.py"], entry);
    assert_eq!(opened.draft.inputs.files["parts/bracket.py"], part);
    // Import/Save read the current revision; it must not be an empty scratchpad model.py.
    let source = core
        .read_source(&opened.workspace_id, &opened.base_revision, "main.py")
        .await
        .unwrap();
    assert_eq!(source["source"], entry);
    assert!(
        core.read_source(&opened.workspace_id, &opened.base_revision, "model.py")
            .await
            .is_err()
    );
}
