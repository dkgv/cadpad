//! Committed-model fixtures for tests of metadata, persistence and motion control.
//!
//! A real worker prepares each distinct input once per nextest run. Consumers
//! get independent copies of the persisted app data, never shared Core state or
//! a shared interpreter. Tests of construction, warm workers or native recovery
//! must keep their own explicit build setup instead.
use cadpad_core::{Core, JobState, kinematics, worker::WorkerConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

#[derive(Serialize, Deserialize)]
struct Published {
    workspace: String,
    revision: String,
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        // Sandbox files and sockets are tied to the original interpreter.
        if entry.file_name().to_string_lossy().starts_with(".sandbox") {
            continue;
        }
        let kind = entry.file_type().unwrap();
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            assert!(kind.is_file(), "Fixture contains a non-regular file");
            // Never hard-link: corruption/cleanup tests must not affect a seed
            // or any other test's copy.
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn copy_app(source: &Path, destination: &Path) {
    assert!(
        !destination.join("workspaces").exists(),
        "Fixture needs a fresh app root"
    );
    // Leave process locks, native admission leases, run directories and IPC out
    // of the snapshot. Core::open will reconstruct its own runtime state.
    for name in ["workspaces", "artifacts", "blobs"] {
        if source.join(name).exists() {
            copy_tree(&source.join(name), &destination.join(name));
        }
    }
}

async fn build(
    root: PathBuf,
    config: WorkerConfig,
    source: &str,
    mechanism: &Value,
) -> (Core, Published) {
    let core = Core::open(root, config).await.unwrap();
    let workspace = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&workspace).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut draft = core
        .update_source(
            &workspace,
            &base,
            None,
            BTreeMap::from([("model.py".into(), source.into())]),
            "user",
        )
        .await
        .unwrap();
    if !mechanism.is_null() {
        draft = core
            .stage_mechanism(
                &workspace,
                &base,
                Some(&draft.id),
                kinematics::decode(mechanism).unwrap(),
                "user",
            )
            .await
            .unwrap();
    }
    let job = core
        .build(&workspace, &base, &draft.id, "user")
        .await
        .unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let current = core.get_job(&workspace, &job.id).await.unwrap();
            if !matches!(current.state, JobState::Queued | JobState::Running) {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        finished.state,
        JobState::Succeeded,
        "{}",
        finished.diagnostics
    );
    (
        core,
        Published {
            workspace,
            revision: finished.resulting_revision.unwrap(),
        },
    )
}

#[allow(dead_code)] // Some harnesses only test open_shared directly.
pub async fn open(
    root: PathBuf,
    config: WorkerConfig,
    source: &str,
    mechanism: Value,
) -> (Core, String, String) {
    // Packaged QA deliberately exercises the complete shipped worker path.
    // The fresh switch also provides an A/B comparison with identical assertions.
    if std::env::var_os("CADPAD_TEST_PACKAGED_RESOURCES").is_some()
        || std::env::var_os("CADPAD_TEST_FRESH_FIXTURES").is_some()
    {
        let (core, published) = build(root, config, source, &mechanism).await;
        return (core, published.workspace, published.revision);
    }
    open_shared(root, config, source, mechanism).await
}

// Exposed within each test harness so the fixture-isolation regression can
// exercise sharing even when scenario benchmarks request fresh setup.
pub async fn open_shared(
    root: PathBuf,
    config: WorkerConfig,
    source: &str,
    mechanism: Value,
) -> (Core, String, String) {
    static LOCAL_RUN: OnceLock<String> = OnceLock::new();
    let run = std::env::var("NEXTEST_RUN_ID").unwrap_or_else(|_| {
        LOCAL_RUN
            .get_or_init(|| uuid::Uuid::new_v4().to_string())
            .clone()
    });
    let run = uuid::Uuid::parse_str(&run).expect("Invalid test run ID");
    let key = cadpad_core::portable::bytes_hash(&serde_json::to_vec(&json!({
        "source":source, "mechanism":mechanism,
        "executable":config.executable, "args":config.args, "module_path":config.module_path,
        "guardian":config.guardian_executable, "containment":format!("{:?}",config.containment),
        "limits":[config.limits.operation_timeout_ms,config.limits.cpu_time_ms,config.limits.memory_bytes]
    })).unwrap());
    let directory = std::env::temp_dir()
        .join("cadpad-published-test-fixtures")
        .join(run.to_string());
    fs::create_dir_all(&directory).unwrap();
    let lock_path = directory.join(format!("{key}.lock"));
    let _lock = tokio::task::spawn_blocking(move || {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .unwrap();
        file.lock().unwrap();
        file
    })
    .await
    .unwrap();
    let seed = directory.join(&key);
    let manifest = seed.join("published.json");
    let trace = std::env::var_os("CADPAD_TEST_FIXTURE_TRACE").is_some();
    if manifest.exists() {
        let published: Published = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
        copy_app(&seed.join("app"), &root);
        let core = Core::open(root, config).await.unwrap();
        core.focus_workspace(&published.workspace).await.unwrap();
        if trace {
            eprintln!("PUBLISHED_FIXTURE reused {key}");
        }
        (core, published.workspace, published.revision)
    } else {
        if seed.exists() {
            fs::remove_dir_all(&seed).unwrap();
        }
        fs::create_dir_all(&seed).unwrap();
        let (core, published) = build(root.clone(), config, source, &mechanism).await;
        // The terminal job was durably persisted before get_job returned it.
        // The live worker pins its artifacts while this immutable seed is copied.
        copy_app(&root, &seed.join("app"));
        let temporary = seed.join("published.tmp");
        fs::write(&temporary, serde_json::to_vec(&published).unwrap()).unwrap();
        fs::rename(temporary, manifest).unwrap();
        if trace {
            eprintln!("PUBLISHED_FIXTURE built {key}");
        }
        (core, published.workspace, published.revision)
    }
}
