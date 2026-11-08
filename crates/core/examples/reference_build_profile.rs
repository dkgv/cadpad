//! Owned, contained diagnostic workers only. Profiling is not enabled in the app.
use cadpad_core::{Core, JobState, worker::WorkerConfig};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const FIXTURE: &str = include_str!("../../../fixtures/performance_reference/model.py");
const PINS: &str = include_str!("../../../fixtures/performance_reference/annotations.json");
const PROFILE: &str = r#"
import time as _cad_time
_cad_setup = (_cad_time.perf_counter(), _cad_time.process_time())
import json as _cad_json
import cadpad_worker.model as _cad_model
import cadpad_worker.annotations as _cad_annotations
import cadpad_worker.collision as _cad_collision
_cad_scopes = {}
_cad_stack = []
def _cad_record(name, wall, cpu, self_wall=None, self_cpu=None):
    row = _cad_scopes.setdefault(name, dict(calls=0, wall_s=0.0, cpu_s=0.0, self_wall_s=0.0, self_cpu_s=0.0))
    row['calls'] += 1
    row['wall_s'] += wall
    row['cpu_s'] += cpu
    row['self_wall_s'] += wall if self_wall is None else max(0.0, self_wall)
    row['self_cpu_s'] += cpu if self_cpu is None else max(0.0, self_cpu)
def _cad_timed(function, name):
    def measured(*args, **kwargs):
        before = (_cad_time.perf_counter(), _cad_time.process_time())
        children = [0.0, 0.0]
        _cad_stack.append(children)
        try:
            return function(*args, **kwargs)
        finally:
            elapsed = (_cad_time.perf_counter()-before[0], _cad_time.process_time()-before[1])
            _cad_stack.pop()
            if _cad_stack:
                _cad_stack[-1][0] += elapsed[0]
                _cad_stack[-1][1] += elapsed[1]
            _cad_record(name, *elapsed, elapsed[0]-children[0], elapsed[1]-children[1])
    measured.__wrapped__ = function
    return measured
for _cad_name in ['geometry_fingerprint', 'bounds']:
    setattr(_cad_model, _cad_name, _cad_timed(getattr(_cad_model, _cad_name), _cad_name))
for _cad_name in ['describe', 'mesh', 'artifact']:
    setattr(_cad_model.Model, _cad_name, _cad_timed(getattr(_cad_model.Model, _cad_name), _cad_name))
for _cad_name in ['resolve', 'automatic_circles']:
    setattr(_cad_annotations, _cad_name, _cad_timed(getattr(_cad_annotations, _cad_name), _cad_name))
_cad_collision.validate_proxies = _cad_timed(_cad_collision.validate_proxies, 'validate_proxies')
_cad_remesh = _cad_timed(_cad_model.Model.remesh, 'remesh_total')
def _cad_profiled_remesh(self, *args, **kwargs):
    try:
        return _cad_remesh(self, *args, **kwargs)
    finally:
        (self.cache / 'reference-build.profile.json').write_text(_cad_json.dumps(dict(scopes=_cad_scopes)))
_cad_model.Model.remesh = _cad_profiled_remesh
show = _cad_timed(show, 'publish_show')
dimension = _cad_timed(dimension, 'publish_dimension')
_cad_record('profile_setup', _cad_time.perf_counter()-_cad_setup[0], _cad_time.process_time()-_cad_setup[1])
_cad_source_start = (_cad_time.perf_counter(), _cad_time.process_time())
"#;
fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
#[tokio::main]
async fn main() -> Result<()> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let mut args = std::env::args_os().skip(1);
    let mut resources = None;
    let mut destination = None;
    while let Some(arg) = args.next() {
        if arg == "--resources" {
            resources = Some(PathBuf::from(args.next().ok_or("Missing resources")?));
        } else if arg == "--output" {
            destination = Some(PathBuf::from(
                args.next().ok_or("Missing output directory")?,
            ));
        } else {
            return Err("Unknown profile argument".into());
        }
    }
    let destination = destination.ok_or("Pass a new --output directory")?;
    std::fs::create_dir(&destination)?;
    let destination = destination.canonicalize()?;
    let packaged = resources.is_some();
    let config = if let Some(resources) = resources {
        cadpad_core::runtime::PackagedRuntime::open(&resources)?.worker_config()
    } else {
        WorkerConfig::development(&repo)
    };
    assert_eq!(
        config.containment,
        cadpad_core::worker::Containment::Required
    );
    let executable = std::env::current_exe()?;
    let executable_hash = hash(std::fs::read(&executable)?);
    let worker = config.executable.canonicalize()?;
    let worker_hash = hash(std::fs::read(&worker)?);
    let guardian = config.guardian_executable.canonicalize()?;
    let guardian_hash = hash(std::fs::read(&guardian)?);
    let mut inputs = BTreeMap::new();
    for entry in std::fs::read_dir(repo.join("python/cadpad_worker"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|value| value == "py") {
            inputs.insert(path.clone(), hash(std::fs::read(path)?));
        }
    }
    for path in [repo.join("Cargo.lock"), repo.join("uv.lock")] {
        inputs.insert(path.clone(), hash(std::fs::read(path)?));
    }
    let mut cases = Vec::new();
    for side in [10, 20] {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("app");
        let source = PROFILE.to_owned()
            + &FIXTURE.replace(
                "parameter(\"grid_side\", 10,",
                &format!("parameter(\"grid_side\", {side},"),
            )
            + "\n_cad_record('source_body', _cad_time.perf_counter()-_cad_source_start[0], _cad_time.process_time()-_cad_source_start[1])\nprint('__profile_source_executed__')\n";
        let source_root = temporary.path().join("source");
        std::fs::create_dir(&source_root)?;
        std::fs::write(source_root.join("model.py"), &source)?;
        std::fs::write(source_root.join("annotations.json"), PINS)?;
        let core = Core::open(root.clone(), config.clone()).await?;
        let opened = core
            .open_python(&source_root.join("model.py"), "reference-profile")
            .await?;
        let ws = opened.workspace_id;
        let mut draft = opened.draft;
        for phase in ["fresh_managed_cache", "populated_display_cache"] {
            eprintln!("Profiling {} instances, {phase}", side * side);
            let start = Instant::now();
            let job = core
                .build(&ws, &draft.base_revision, &draft.id, "reference-profile")
                .await?;
            let completed = tokio::time::timeout(Duration::from_secs(300), async {
                loop {
                    let result = core.get_job(&ws, &job.id).await?;
                    if !matches!(result.state, JobState::Queued | JobState::Running) {
                        break Ok::<_, cadpad_core::Error>(result);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await??;
            let observed_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(
                completed.state,
                JobState::Succeeded,
                "{:?}",
                completed.diagnostics
            );
            assert!(
                completed.diagnostics["logs"]
                    .as_str()
                    .unwrap()
                    .contains("__profile_source_executed__")
            );
            let revision = completed.resulting_revision.unwrap();
            let project = core.get_project(&ws).await?;
            assert_eq!(
                project["model"]["parts"].as_array().unwrap().len(),
                side * side
            );
            let pins = project["model"]["annotations"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|pin| pin["saved_id"].is_string())
                .collect::<Vec<_>>();
            assert_eq!(pins.len(), 30);
            assert!(
                pins.iter()
                    .all(|pin| pin["status"] == "resolved" && pin["value"] == 200.)
            );
            let stats = std::fs::read(
                root.join("artifacts")
                    .join(&ws)
                    .join(&revision)
                    .join("reference-build.profile.json"),
            )?;
            assert!(!stats.is_empty() && stats.len() < 4 * 1024 * 1024);
            let profile: serde_json::Value = serde_json::from_slice(&stats)?;
            assert_eq!(profile["scopes"]["source_body"]["calls"], 1);
            assert_eq!(profile["scopes"]["publish_show"]["calls"], side * side);
            assert!(
                profile["scopes"]["geometry_fingerprint"]["calls"]
                    .as_u64()
                    .unwrap()
                    >= 2 * (side * side) as u64
            );

            let name = format!("{}-{phase}.json", side * side);
            std::fs::write(destination.join(&name), &stats)?;
            cases.push(json!({"instances":side*side,"phase":phase,"source_sha256":hash(&source),"profile":name,"profile_sha256":hash(&stats),"profile_bytes":stats.len(),"source_executed":true,"resolved_pins":30,"dependencies":project["model"]["dependencies"],"observed_profiled_job_ms":observed_ms}));
            if phase == "fresh_managed_cache" {
                draft = core
                    .update_source(
                        &ws,
                        &revision,
                        None,
                        BTreeMap::from([("model.py".into(), source.clone())]),
                        "reference-profile",
                    )
                    .await?;
            }
        }
    }
    assert_eq!(executable_hash, hash(std::fs::read(&executable)?));
    assert_eq!(worker_hash, hash(std::fs::read(&worker)?));
    assert_eq!(guardian_hash, hash(std::fs::read(&guardian)?));
    for (path, digest) in &inputs {
        assert_eq!(
            &hash(std::fs::read(path)?),
            digest,
            "Input changed during profiling"
        );
    }
    let report = json!({"status":"passed","mode":if packaged{"packaged"}else{"development"},"captured_inputs":inputs,"required_containment":true,"empty_path":std::env::var("PATH").ok().is_some_and(|value|value.is_empty()),"benchmark_executable":executable,"benchmark_executable_sha256":executable_hash,"benchmark_source_sha256":hash(include_str!("reference_build_profile.rs")),"worker_executable":worker,"worker_executable_sha256":worker_hash,"guardian_executable":guardian,"guardian_executable_sha256":guardian_hash,"fixture_sha256":hash(FIXTURE),"pins_sha256":hash(PINS),"output":destination,"cases":cases,"ui_launched":false,
 "scope":"Instrumented diagnostic source in owned contained workers. Explicit process_time/perf_counter scopes around source publication, geometry helpers, describe, mesh, artifact, annotations, collision and remesh. Parent totals overlap child totals; self times subtract only measured child scopes. Source body overlaps publish scopes. Excludes worker startup before source, unwrapped build code, reply serialization and native Core/IPC/GPU. Diagnostic overhead means these are not release-latency measurements. Wrappers only affect owned fixture workers, retain __wrapped__ producers, and preserve source results."});
    std::fs::write(
        destination.join("manifest.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
