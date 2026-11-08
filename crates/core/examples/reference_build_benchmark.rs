//! Headless native-core timings. No webview, rendering, GPU or provider claims.
use cadpad_core::{Core, Draft, JobState, worker::WorkerConfig};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const ACTOR: &str = "reference-build-benchmark";
const SOURCE: &str = include_str!("../../../fixtures/performance_reference/model.py");
const PINS: &str = include_str!("../../../fixtures/performance_reference/annotations.json");
fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.
}
fn summary(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return json!({"samples":0});
    }
    let quantile = |p: f64| values[((values.len() as f64 * p).ceil() as usize).saturating_sub(1)];
    json!({"samples":values.len(),"min_ms":values[0],"median_ms":quantile(0.5),"p95_ms":quantile(0.95),"max_ms":values[values.len()-1]})
}
async fn measure(
    core: &Core,
    ws: &str,
    draft: Draft,
    side: usize,
    phase: &str,
    probes: bool,
) -> Result<Value> {
    let start = Instant::now();
    let job = core
        .build(ws, &draft.base_revision, &draft.id, ACTOR)
        .await?;
    let accepted = ms(start);
    let mut contexts = Vec::new();
    let job = tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            let job = core.get_job(ws, &job.id).await?;
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                return Ok::<_, cadpad_core::Error>(job);
            }
            if probes {
                let read = Instant::now();
                let context = core.get_project(ws).await?;
                contexts.push(ms(read));
                assert_eq!(context["workspace_id"], ws);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let terminal = ms(start);
    if job.state != JobState::Succeeded {
        return Err(format!("Build failed: {:?}", job.diagnostics).into());
    }
    assert!(
        job.diagnostics["logs"]
            .as_str()
            .unwrap()
            .contains("__reference_source_executed__")
    );
    let revision = job.resulting_revision.unwrap();
    let context_start = Instant::now();
    let project = core.get_project(ws).await?;
    let committed_context_ms = ms(context_start);
    assert_eq!(project["revision"], revision);
    assert!(project["display_error"].is_null());
    let model = &project["model"];
    let parts = model["parts"].as_array().unwrap();
    assert_eq!(parts.len(), side * side);
    let pins = model["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|pin| pin.get("saved_id").is_some())
        .collect::<Vec<_>>();
    assert_eq!(pins.len(), 30);
    assert!(
        pins.iter()
            .all(|pin| pin["status"] == "resolved" && pin["value"] == 200.)
    );
    let mut buffers = BTreeMap::<String, Value>::new();
    let mut geometry = std::collections::BTreeSet::new();
    let mut triangles = 0;
    for part in parts {
        geometry.insert(part["geometry_fingerprint"]["sha256"].as_str().unwrap());
        triangles += part["mesh"]["buffers"]["triangles"]["bytes"]
            .as_u64()
            .unwrap()
            / 12;
        for descriptor in part["mesh"]["buffers"].as_object().unwrap().values() {
            let handle = descriptor["handle"].as_str().unwrap().to_owned();
            if let Some(previous) = buffers.insert(handle, descriptor.clone()) {
                assert_eq!(previous, *descriptor);
            }
        }
    }
    assert_eq!(geometry.len(), 1);
    assert_eq!(triangles, 10_108 * (side * side) as u64);
    assert_eq!(buffers.len(), 9);
    let mut reads = Vec::new();
    let mut checksums = Vec::new();
    let mut bytes = 0;
    for (handle, descriptor) in &buffers {
        let read = Instant::now();
        let data = core.artifact(ws, &revision, handle).await?;
        reads.push(ms(read));
        let check = Instant::now();
        assert_eq!(hash(&data), descriptor["sha256"].as_str().unwrap());
        checksums.push(ms(check));
        assert_eq!(data.len() as u64, descriptor["bytes"].as_u64().unwrap());
        bytes += data.len();
    }
    assert_eq!(bytes, 245_144);
    Ok(
        json!({"phase":phase,"context_probes_enabled":probes,"instances":parts.len(),"scene_triangles":triangles,"resolved_pins":pins.len(),"unique_geometry":geometry.len(),"unique_buffers":buffers.len(),"buffer_bytes":bytes,
        "job_accept_ms":accepted,"job_terminal_observed_ms":terminal,"committed_context_ms":committed_context_ms,
        "context_reads_after_observing_nonterminal_job":summary(contexts),"scoped_artifact_read":summary(reads),"client_sha256_check":summary(checksums),
        "model_json_bytes":serde_json::to_vec(model)?.len(),"dependencies":model["dependencies"],"source_executed":true,"buffer_manifest_sha256":hash(serde_json::to_vec(&buffers)?)}),
    )
}
async fn reopen(root: PathBuf, config: WorkerConfig) -> Result<Core> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Core::open(root.clone(), config.clone()).await {
            Ok(core) => return Ok(core),
            Err(error) if error.code() == "APP_ALREADY_RUNNING" && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await
            }
            Err(error) => return Err(error.into()),
        }
    }
}
fn host_value(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
#[tokio::main]
async fn main() -> Result<()> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let mut args = std::env::args_os().skip(1);
    let mut resources = None;
    let mut trials = 3;
    while let Some(arg) = args.next() {
        if arg == "--resources" {
            resources = Some(PathBuf::from(args.next().ok_or("Missing resources")?));
        } else if arg == "--trials" {
            trials = args
                .next()
                .ok_or("Missing trials")?
                .to_str()
                .ok_or("Invalid trials")?
                .parse::<usize>()?;
        } else {
            return Err("Unknown reference-build benchmark argument".into());
        }
    }
    if !(1..=10).contains(&trials) {
        return Err("Trials must be in 1..10".into());
    }
    let config = if let Some(resources) = &resources {
        cadpad_core::runtime::PackagedRuntime::open(resources)?.worker_config()
    } else {
        WorkerConfig::development(&repo)
    };
    assert_eq!(
        config.containment,
        cadpad_core::worker::Containment::Required
    );
    let executable = std::env::current_exe()?;
    let worker = std::fs::canonicalize(&config.executable)?;
    let guardian = std::fs::canonicalize(&config.guardian_executable)?;
    let mut inputs = BTreeMap::new();
    for path in [
        &executable,
        &worker,
        &guardian,
        &repo.join("Cargo.lock"),
        &repo.join("uv.lock"),
    ] {
        inputs.insert(path.clone(), hash(std::fs::read(path)?));
    }
    for entry in std::fs::read_dir(repo.join("python/cadpad_worker"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "py") {
            inputs.insert(path.clone(), hash(std::fs::read(path)?));
        }
    }
    let mut cases = Vec::new();
    for side in [10, 20] {
        for probes in [false, true] {
            for trial in 0..trials {
                eprintln!(
                    "Reference: {} instances, trial {}, context probes {}",
                    side * side,
                    trial + 1,
                    probes
                );
                let temporary = tempfile::tempdir()?;
                let root = temporary.path().join("app");
                let source = SOURCE.replace(
                    "parameter(\"grid_side\", 10,",
                    &format!("parameter(\"grid_side\", {side},"),
                ) + "\nprint('__reference_source_executed__')\n";
                let source_root = temporary.path().join("source");
                std::fs::create_dir(&source_root)?;
                std::fs::write(source_root.join("model.py"), &source)?;
                std::fs::write(source_root.join("annotations.json"), PINS)?;
                let opened = Instant::now();
                let core = Core::open(root.clone(), config.clone()).await?;
                let core_open_ms = ms(opened);
                let stage = Instant::now();
                let opened = core
                    .open_python(&source_root.join("model.py"), ACTOR)
                    .await?;
                let stage_ms = ms(stage);
                let ws = opened.workspace_id;
                let mut fresh = measure(
                    &core,
                    &ws,
                    opened.draft,
                    side,
                    "fresh_managed_cache",
                    probes,
                )
                .await?;
                fresh["core_open_ms"] = json!(core_open_ms);
                fresh["stage_ms"] = json!(stage_ms);
                let base = core.get_project(&ws).await?["revision"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let stage = Instant::now();
                let draft = core
                    .update_source(
                        &ws,
                        &base,
                        None,
                        BTreeMap::from([("model.py".into(), source.clone())]),
                        ACTOR,
                    )
                    .await?;
                let stage_ms = ms(stage);
                let mut populated =
                    measure(&core, &ws, draft, side, "populated_display_cache", probes).await?;
                populated["stage_ms"] = json!(stage_ms);
                drop(core);
                let start = Instant::now();
                let core = reopen(root, config.clone()).await?;
                let core_reopen_ms = ms(start);
                let base = core.get_project(&ws).await?["revision"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let stage = Instant::now();
                let draft = core
                    .update_source(
                        &ws,
                        &base,
                        None,
                        BTreeMap::from([("model.py".into(), source.clone())]),
                        ACTOR,
                    )
                    .await?;
                let stage_ms = ms(stage);
                let mut restarted = measure(
                    &core,
                    &ws,
                    draft,
                    side,
                    "reopened_core_populated_display_cache",
                    probes,
                )
                .await?;
                restarted["core_reopen_ms"] = json!(core_reopen_ms);
                restarted["stage_ms"] = json!(stage_ms);
                assert_eq!(
                    fresh["buffer_manifest_sha256"],
                    populated["buffer_manifest_sha256"]
                );
                assert_eq!(
                    fresh["buffer_manifest_sha256"],
                    restarted["buffer_manifest_sha256"]
                );
                cases.push(json!({"grid_side":side,"trial":trial+1,"context_probes_enabled":probes,"executed_source_sha256":hash(source),"measurements":[fresh,populated,restarted]}));
            }
        }
    }
    for (path, digest) in &inputs {
        assert_eq!(
            &hash(std::fs::read(path)?),
            digest,
            "Input changed during benchmark: {}",
            path.display()
        );
    }
    let report = json!({"status":"passed","measured_at_unix_s":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),"os":std::env::consts::OS,"architecture":std::env::consts::ARCH,
        "hardware_model":host_value("/usr/sbin/sysctl",&["-n","hw.model"]),"cpu":host_value("/usr/sbin/sysctl",&["-n","machdep.cpu.brand_string"]),"memory_bytes":host_value("/usr/sbin/sysctl",&["-n","hw.memsize"]),"os_version":host_value("/usr/bin/sw_vers",&["-productVersion"]),"os_build":host_value("/usr/bin/sw_vers",&["-buildVersion"]),
        "mode":if resources.is_some(){"packaged"}else{"development"},"required_containment":true,"empty_path":std::env::var("PATH").ok().is_some_and(|path|path.is_empty()),"trials_per_scene_and_probe_mode":trials,
        "benchmark_executable":executable,"benchmark_executable_sha256":hash(std::fs::read(&executable)?),"worker_executable":worker,"worker_executable_sha256":hash(std::fs::read(&worker)?),"benchmark_source_sha256":hash(include_str!("reference_build_benchmark.rs")),"fixture_source_sha256":hash(SOURCE),"pins_sha256":hash(PINS),"cargo_lock_sha256":hash(std::fs::read(repo.join("Cargo.lock"))?),"uv_lock_sha256":hash(std::fs::read(repo.join("uv.lock"))?),"captured_inputs":inputs,"guardian_executable":guardian,"limits":{"operation_timeout_ms":config.limits.operation_timeout_ms,"cpu_time_ms":config.limits.cpu_time_ms,"memory_bytes":config.limits.memory_bytes},
        "scope":"Native Core jobs, context and scoped artifact reads; every build starts a new worker and executes source. Fresh/warm describe managed display-cache availability, not OS page-cache state. All builds use 10 ms job polling; context probes are enabled in separate trials and add observer overhead. Fresh first builds have an empty committed scene, while rebuilds retain the reference scene. Core reopen time includes waiting for prior ownership to release. No webview/IPC/GPU/provider/visible labels or input-to-visible timing.","ui_launched":false,"cases":cases});
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
