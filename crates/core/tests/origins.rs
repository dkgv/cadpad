use cadpad_core::{Core, Job, JobState, origins::Resolution, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
fn config() -> WorkerConfig {
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
    .uncontained_for_trusted_tests()
}
fn source(width: u32) -> String {
    format!("import cadquery as cq\nshow(cq.Workplane().box({width},20,6),name='part')\n")
}
async fn terminal(core: &Core, ws: &str, job: Job) -> Job {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
async fn done(core: &Core, ws: &str, job: Job) -> String {
    let job = terminal(core, ws, job).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    job.resulting_revision.unwrap()
}
async fn open(core: &Core, entry: &Path) -> (String, String) {
    let opened = core.open_python(entry, "user").await.unwrap();
    let revision = done(
        core,
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
    (opened.workspace_id, revision)
}
async fn local(core: &Core, ws: &str, base: &str, path: &str, source: String) -> String {
    let draft = core
        .update_source(
            ws,
            base,
            None,
            BTreeMap::from([(path.into(), source)]),
            "user",
        )
        .await
        .unwrap();
    done(
        core,
        ws,
        core.build(ws, base, &draft.id, "user").await.unwrap(),
    )
    .await
}
async fn reload(
    core: &Core,
    ws: &str,
    base: &str,
    observed: &Value,
    resolutions: BTreeMap<String, Resolution>,
) -> String {
    let draft = core
        .stage_external_changes_resolved(
            ws,
            base,
            observed["fingerprint"].as_str().unwrap(),
            "user",
            resolutions,
        )
        .await
        .unwrap();
    done(
        core,
        ws,
        core.build(ws, base, &draft.id, "user").await.unwrap(),
    )
    .await
}
#[tokio::test]
async fn standalone_changes_merge_independent_edits_freeze_inputs_and_save_a_nested_portable_fork()
{
    let temp = tempfile::tempdir().unwrap();
    let origin = temp.path().join("python");
    std::fs::create_dir(&origin).unwrap();
    let entry = origin.join("main.py");
    let original = "import cadquery as cq\nfrom helper import WIDTH\nthickness=parameter('thickness',6,minimum=1,maximum=20)\nshow(cq.Workplane().box(WIDTH,20,thickness),name='part')\n";
    std::fs::write(&entry, original).unwrap();
    std::fs::write(origin.join("helper.py"), "WIDTH=10\n").unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, base) = open(&core, &entry).await;
    assert_eq!(
        core.get_project(&ws).await.unwrap()["external_tracking"],
        true
    );
    assert_eq!(
        core.scan_external_changes(&ws).await.unwrap()["changed"],
        false
    );
    let rev = local(
        &core,
        &ws,
        &base,
        "main.py",
        format!("{original}# local note\n"),
    )
    .await;
    let rev = done(
        &core,
        &ws,
        core.set_parameter(&ws, &rev, "thickness", 7., "mm", "user")
            .await
            .unwrap(),
    )
    .await;
    std::fs::write(origin.join("helper.py"), "WIDTH=30\n").unwrap();
    let observed = core.scan_external_changes(&ws).await.unwrap();
    assert_eq!(observed["changes"]["helper.py"], "modified");
    assert!(observed["conflicts"].as_array().unwrap().is_empty());
    let destination = origin.join("saved.cadpad");
    assert_eq!(
        core.save_project(&ws, &rev, Some(destination.clone()))
            .await
            .unwrap_err()
            .code(),
        "EXTERNAL_CHANGES"
    );
    assert!(!destination.exists());
    let draft = core
        .stage_external_changes(&ws, &rev, observed["fingerprint"].as_str().unwrap(), "user")
        .await
        .unwrap();
    assert!(draft.inputs.files["main.py"].contains("# local note"));
    assert_eq!(draft.inputs.parameters["thickness"]["value"], 7.);
    std::fs::write(origin.join("helper.py"), "WIDTH=40\n").unwrap();
    let rev = done(
        &core,
        &ws,
        core.build(&ws, &rev, &draft.id, "user").await.unwrap(),
    )
    .await;
    assert!(
        (core.get_project(&ws).await.unwrap()["model"]["parts"][0]["volume"]
            .as_f64()
            .unwrap()
            - 4200.)
            .abs()
            < 1e-6
    );
    let latest = core.scan_external_changes(&ws).await.unwrap();
    assert_eq!(latest["changed"], true);
    let rev = reload(&core, &ws, &rev, &latest, BTreeMap::new()).await;
    assert_eq!(
        core.scan_external_changes(&ws).await.unwrap()["changed"],
        false
    );
    core.save_project(&ws, &rev, Some(destination.clone()))
        .await
        .unwrap();
    assert!(destination.join("project.json").exists());
    assert!(
        std::fs::read_to_string(destination.join("main.py"))
            .unwrap()
            .contains("# local note")
    );
    assert!(
        !std::fs::read_to_string(destination.join("project.json"))
            .unwrap()
            .contains("python_origin")
    );
    std::fs::write(origin.join("helper.py"), "WIDTH=99\n").unwrap();
    assert_eq!(
        core.scan_external_changes(&ws).await.unwrap()["changed"],
        false
    );
}
#[tokio::test]
async fn same_file_edits_require_explicit_choices_and_successfully_reviewed_versions_allow_save() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("python");
    std::fs::create_dir(&directory).unwrap();
    let entry = directory.join("main.py");
    std::fs::write(&entry, source(10)).unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, base) = open(&core, &entry).await;
    let rev = local(&core, &ws, &base, "main.py", source(12)).await;
    std::fs::write(&entry, source(14)).unwrap();
    let observed = core.scan_external_changes(&ws).await.unwrap();
    assert_eq!(observed["conflicts"], json!(["main.py"]));
    assert_eq!(
        core.stage_external_changes(&ws, &rev, observed["fingerprint"].as_str().unwrap(), "user")
            .await
            .unwrap_err()
            .code(),
        "EXTERNAL_EDIT_CONFLICT"
    );
    let rev = reload(
        &core,
        &ws,
        &rev,
        &observed,
        BTreeMap::from([("main.py".into(), Resolution::KeepCadpad)]),
    )
    .await;
    assert_eq!(
        core.read_source(&ws, &rev, "main.py").await.unwrap()["source"],
        source(12)
    );
    assert_eq!(
        core.scan_external_changes(&ws).await.unwrap()["changed"],
        false
    );
    std::fs::write(&entry, source(16)).unwrap();
    let observed = core.scan_external_changes(&ws).await.unwrap();
    assert_eq!(observed["conflicts"], json!(["main.py"]));
    let rev = reload(
        &core,
        &ws,
        &rev,
        &observed,
        BTreeMap::from([("main.py".into(), Resolution::UseExternal)]),
    )
    .await;
    assert_eq!(
        core.read_source(&ws, &rev, "main.py").await.unwrap()["source"],
        source(16)
    );
    std::fs::write(&entry, source(18)).unwrap();
    let observed = core.scan_external_changes(&ws).await.unwrap();
    std::fs::write(&entry, source(20)).unwrap();
    assert_eq!(
        core.stage_external_changes(&ws, &rev, observed["fingerprint"].as_str().unwrap(), "user")
            .await
            .unwrap_err()
            .code(),
        "REVISION_CONFLICT"
    );
}
#[tokio::test]
async fn failed_reload_and_restart_retain_the_last_good_model_and_unreviewed_origin() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("python");
    std::fs::create_dir(&directory).unwrap();
    let entry = directory.join("main.py");
    std::fs::write(&entry, source(10)).unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let (ws, rev) = open(&core, &entry).await;
    std::fs::write(&entry, "raise RuntimeError('broken external edit')").unwrap();
    let observed = core.scan_external_changes(&ws).await.unwrap();
    let draft = core
        .stage_external_changes(&ws, &rev, observed["fingerprint"].as_str().unwrap(), "user")
        .await
        .unwrap();
    let failed = terminal(
        &core,
        &ws,
        core.build(&ws, &rev, &draft.id, "user").await.unwrap(),
    )
    .await;
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], rev);
    assert_eq!(
        core.scan_external_changes(&ws).await.unwrap()["changed"],
        true
    );
    drop(core);
    let core = Core::open(root, config()).await.unwrap();
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], rev);
    assert_eq!(
        core.scan_external_changes(&ws).await.unwrap()["changed"],
        true
    );
    assert_eq!(
        core.check_python_origin_before_save(&ws)
            .await
            .unwrap_err()
            .code(),
        "EXTERNAL_CHANGES"
    );
    std::fs::remove_file(&entry).unwrap();
    assert!(core.scan_external_changes(&ws).await.is_err());
    let destination = temp.path().join("save");
    assert_eq!(
        core.save_project(&ws, &rev, Some(destination.clone()))
            .await
            .unwrap_err()
            .code(),
        "EXTERNAL_CHANGES"
    );
    assert!(!destination.exists());
}
#[cfg(unix)]
#[tokio::test]
async fn origin_entry_symlinks_do_not_retarget_the_source_and_revoked_actors_cannot_reload() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("python");
    std::fs::create_dir(&directory).unwrap();
    let entry = directory.join("main.py");
    std::fs::write(&entry, source(10)).unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, rev) = open(&core, &entry).await;
    let external = temp.path().join("outside.py");
    std::fs::write(&external, source(30)).unwrap();
    std::fs::remove_file(&entry).unwrap();
    std::os::unix::fs::symlink(&external, &entry).unwrap();
    assert_eq!(
        core.scan_external_changes(&ws).await.unwrap_err().code(),
        "INVALID_REQUEST"
    );
    assert_eq!(
        core.check_python_origin_before_save(&ws)
            .await
            .unwrap_err()
            .code(),
        "EXTERNAL_CHANGES"
    );
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], rev);
    std::fs::remove_file(&entry).unwrap();
    std::fs::write(&entry, source(20)).unwrap();
    let observed = core.scan_external_changes(&ws).await.unwrap();
    core.revoke_actor("user").await;
    assert!(
        core.stage_external_changes(&ws, &rev, observed["fingerprint"].as_str().unwrap(), "user")
            .await
            .is_err()
    );
}
