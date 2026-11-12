//! Exact geometry reuse through contained workers, private journals and restart.
use cadpad_core::{Core, Job, JobState, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
const SOURCE: &str = include_str!("../../../fixtures/cached_shape.py");
fn config() -> WorkerConfig {
    if let Some(resources) = std::env::var_os("CADPAD_TEST_PACKAGED_RESOURCES") {
        assert_eq!(std::env::var("PATH").as_deref(), Ok(""));
        return cadpad_core::runtime::PackagedRuntime::open(Path::new(&resources))
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
async fn open(root: &Path) -> Core {
    let start = std::time::Instant::now();
    loop {
        match Core::open(root.to_path_buf(), config()).await {
            Ok(core) => return core,
            Err(e)
                if e.code() == "APP_ALREADY_RUNNING"
                    && start.elapsed() < Duration::from_secs(10) =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await
            }
            Err(e) => panic!("{e}"),
        }
    }
}
async fn finished(core: &Core, ws: &str, job: Job) -> Job {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let job = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn build(core: &Core, ws: &str, source: &str) -> Job {
    let project = core.get_project(ws).await.unwrap();
    let base = project["revision"].as_str().unwrap();
    let draft = core
        .update_source(
            ws,
            base,
            None,
            BTreeMap::from([("model.py".into(), source.into())]),
            "cache-test",
        )
        .await
        .unwrap();
    finished(
        core,
        ws,
        core.build(ws, base, &draft.id, "cache-test").await.unwrap(),
    )
    .await
}
fn verify_job(job: &Job, builder: bool) -> &str {
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    let logs = job.diagnostics["logs"].as_str().unwrap();
    assert!(
        logs.contains("__source_executed__"),
        "expected native source execution: {logs}"
    );
    assert!(
        !logs.contains("__source_skipped__"),
        "expected native source execution: {logs}"
    );
    assert_eq!(
        logs.contains("__native_builder_executed__"),
        builder,
        "{logs}"
    );
    job.resulting_revision.as_deref().unwrap()
}
fn verify_skipped(job: &Job) -> &str {
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    let logs = job.diagnostics["logs"].as_str().unwrap();
    assert!(
        logs.contains("__source_skipped__"),
        "expected skip-source authoritative reuse: {logs}"
    );
    assert!(
        !logs.contains("__source_executed__"),
        "authoritative reuse must not execute source: {logs}"
    );
    assert!(
        !logs.contains("__native_builder_executed__"),
        "authoritative reuse must not execute builders: {logs}"
    );
    job.resulting_revision.as_deref().unwrap()
}
fn private_cache(root: &Path, ws: &str) -> Value {
    let value: Value = serde_json::from_slice(
        &std::fs::read(root.join("workspaces").join(ws).join("workspace.json")).unwrap(),
    )
    .unwrap();
    value["geometry_cache"].clone()
}
fn normalized(mut model: Value) -> Value {
    model["revision"] = json!("normalized");
    if let Some(parts) = model["parts"].as_array_mut() {
        for part in parts {
            part["revision"] = json!("normalized");
        }
    }
    model
}
#[tokio::test]
async fn deterministic_shapes_reuse_after_normalization_restart_and_cache_collection() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = open(&root).await;
    let ws = core.create_workspace("cache-test").await.unwrap();
    let first = verify_job(&build(&core, &ws, SOURCE).await, true).to_owned();
    // After parameters are published, identical inputs skip source entirely.
    let before = verify_skipped(&build(&core, &ws, SOURCE).await).to_owned();
    let old = core.get_project(&ws).await.unwrap();
    let entry = private_cache(&root, &ws)["offers"][0]["manifest"]["entries"]["plate"].clone();
    let handle = entry["handle"].as_str().unwrap();
    let revision = verify_skipped(&build(&core, &ws, SOURCE).await).to_owned();
    let project = core.get_project(&ws).await.unwrap();
    assert_eq!(
        old["model"]["parts"][0]["geometry_fingerprint"],
        project["model"]["parts"][0]["geometry_fingerprint"]
    );
    assert_eq!(
        old["model"]["parts"][0]["bounds"],
        project["model"]["parts"][0]["bounds"]
    );
    assert!(project.get("geometry_cache").is_none());
    assert!(
        core.artifact(&ws, &revision, handle).await.is_err(),
        "Native geometry is not public display transport"
    );
    let offer_revision = private_cache(&root, &ws)["offers"][0]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        root.join("artifacts")
            .join(&ws)
            .join(&offer_revision)
            .join(handle)
            .is_file(),
        "private BRep offer must remain on its native revision"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Skip-source copies display meshes, not private BRep offers.
        let mesh = old["model"]["parts"][0]["mesh"]["buffers"]["vertices"]["handle"]
            .as_str()
            .unwrap();
        let path = |r: &str| root.join("artifacts").join(&ws).join(r).join(mesh);
        assert_ne!(
            std::fs::metadata(path(&before)).unwrap().ino(),
            std::fs::metadata(path(&revision)).unwrap().ino()
        );
        let _ = first;
    }
    let report = core.collect_cache().await.unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(
        root.join("artifacts")
            .join(&ws)
            .join(&offer_revision)
            .join(handle)
            .exists()
    );
    drop(core);
    let core = open(&root).await;
    let current = core.get_project(&ws).await.unwrap();
    assert_eq!(current["revision"], revision);
    assert_eq!(current["model"]["parts"][0]["bounds"]["size"][0], 40.);
    let _ = first;
    let changed = finished(
        &core,
        &ws,
        core.set_parameter(&ws, &revision, "width", 48., "mm", "cache-test")
            .await
            .unwrap(),
    )
    .await;
    let next = verify_job(&changed, true);
    assert_eq!(
        core.get_project(&ws).await.unwrap()["model"]["parts"][0]["bounds"]["size"][0],
        48.
    );
    assert!(
        core.query(&ws, &revision, "inspect", json!({"entities":[]}))
            .await
            .is_err()
    );
    assert_ne!(revision, next);
}
#[tokio::test]
async fn corrupt_native_bytes_regenerate_and_failed_candidates_preserve_the_committed_cache() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = open(&root).await;
    let ws = core.create_workspace("cache-test").await.unwrap();
    verify_job(&build(&core, &ws, SOURCE).await, true);
    let warm = verify_skipped(&build(&core, &ws, SOURCE).await).to_owned();
    let cache = private_cache(&root, &ws);
    let handle = cache["offers"][0]["manifest"]["entries"]["plate"]["handle"]
        .as_str()
        .unwrap();
    let previous = core.get_project(&ws).await.unwrap();
    // Corrupt the private BRep offer on the native-built revision that still
    // anchors geometry_cache, then force a native rebuild by changing inputs.
    std::fs::write(
        root.join("artifacts").join(&ws).join(&warm).join(handle),
        b"corrupt",
    )
    .unwrap();
    // Touch a no-op comment so inputs change and skip-source cannot apply; the
    // shape builder must regenerate after the corrupt offer is refused.
    let repaired = build(&core, &ws, &format!("{SOURCE}\n# force-native\n")).await;
    let revision = verify_job(&repaired, true).to_owned();
    assert_eq!(
        normalized(previous["model"].clone())["parts"][0]["bounds"],
        normalized(core.get_project(&ws).await.unwrap()["model"].clone())["parts"][0]["bounds"]
    );
    let committed = private_cache(&root, &ws);
    let failed = build(
        &core,
        &ws,
        &format!("{SOURCE}\nraise RuntimeError('candidate failed')\n"),
    )
    .await;
    assert_eq!(failed.state, JobState::Failed, "{:?}", failed.diagnostics);
    let logs = failed.diagnostics["details"]["logs"]
        .as_str()
        .unwrap_or_else(|| panic!("failed job has no logs: {}", failed.diagnostics));
    assert!(
        logs.contains("__native_builder_executed__"),
        "Changed source must not reuse the previous builder result"
    );
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
    assert_eq!(private_cache(&root, &ws), committed);
    let cached = build(&core, &ws, &format!("{SOURCE}\n# force-native\n")).await;
    verify_skipped(&cached);
}

#[tokio::test]
async fn matching_digest_reuses_across_an_intervening_different_revision() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = open(&root).await;
    let ws = core.create_workspace("cache-test").await.unwrap();
    let first = verify_job(&build(&core, &ws, SOURCE).await, true).to_owned();
    verify_skipped(&build(&core, &ws, SOURCE).await);
    let before_change = private_cache(&root, &ws)["offers"]
        .as_array()
        .unwrap()
        .len();
    assert!(before_change >= 1, "{}", private_cache(&root, &ws));
    let changed = build(
        &core,
        &ws,
        &format!("{SOURCE}\nshow(cq.Workplane().box(12,12,12),name='other')\n"),
    )
    .await;
    verify_job(&changed, true);
    let after_change = private_cache(&root, &ws);
    assert!(
        after_change["offers"].as_array().unwrap().len() > before_change,
        "expected retained prior digest offers after intervening revision: {after_change}"
    );
    let restored = build(&core, &ws, SOURCE).await;
    assert_eq!(
        restored.state,
        JobState::Succeeded,
        "{:?}",
        restored.diagnostics
    );
    assert_ne!(restored.resulting_revision.as_deref(), Some(first.as_str()));
    assert_eq!(
        normalized(core.get_project(&ws).await.unwrap()["model"].clone())["parts"][0]["bounds"]["size"]
            [0],
        40.
    );
    let logs = restored.diagnostics["logs"].as_str().unwrap_or("");
    assert!(
        logs.contains("__source_skipped__") || logs.contains("__source_executed__"),
        "{logs}"
    );
}
