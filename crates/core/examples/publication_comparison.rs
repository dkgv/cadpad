//! Compare frozen-worker publications without a window or user project storage.
use cadpad_core::{Core, JobState, worker::WorkerConfig};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
fn difference(a: &Value, b: &Value, at: &str) -> Option<String> {
    if a == b {
        return None;
    }
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for key in a.keys().chain(b.keys()) {
                match (a.get(key), b.get(key)) {
                    (Some(a), Some(b)) => {
                        if let Some(path) = difference(a, b, &format!("{at}.{key}")) {
                            return Some(path);
                        }
                    }
                    _ => return Some(format!("{at}.{key}")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Some(format!("{at}.length"));
            }
            for (index, (a, b)) in a.iter().zip(b).enumerate() {
                if let Some(path) = difference(a, b, &format!("{at}[{index}]")) {
                    return Some(path);
                }
            }
        }
        _ => {}
    }
    Some(at.into())
}
async fn publication(config: WorkerConfig, source: &str, pins: &str) -> Result<Value> {
    let temporary = tempfile::tempdir()?;
    let folder = temporary.path().join("source");
    std::fs::create_dir(&folder)?;
    std::fs::write(folder.join("model.py"), source)?;
    std::fs::write(folder.join("annotations.json"), pins)?;
    let core = Core::open(temporary.path().join("app"), config).await?;
    let opened = core
        .open_python(&folder.join("model.py"), "publication-comparison")
        .await?;
    let ws = opened.workspace_id;
    let job = core
        .build(
            &ws,
            &opened.base_revision,
            &opened.draft.id,
            "publication-comparison",
        )
        .await?;
    let completed = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let job = core.get_job(&ws, &job.id).await?;
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                break Ok::<_, cadpad_core::Error>(job);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    if completed.state != JobState::Succeeded {
        return Err(format!("Publication build failed: {:?}", completed.diagnostics).into());
    }
    let project = core.get_project(&ws).await?;
    let mut model = project["model"].clone();
    model["revision"] = json!("comparison-revision");
    // Verify the bytes represented by each publication before comparing descriptors.
    let revision = completed.resulting_revision.unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for part in model["parts"].as_array().unwrap() {
        for descriptor in part["mesh"]["buffers"].as_object().unwrap().values() {
            let handle = descriptor["handle"].as_str().unwrap();
            if seen.insert(handle) {
                let bytes = core.artifact(&ws, &revision, handle).await?;
                assert_eq!(hash(&bytes), descriptor["sha256"].as_str().unwrap());
                assert_eq!(bytes.len() as u64, descriptor["bytes"].as_u64().unwrap());
            }
        }
    }
    Ok(model)
}
fn packaged(resources: &Path) -> Result<WorkerConfig> {
    Ok(cadpad_core::runtime::PackagedRuntime::open(resources)?.worker_config())
}
#[tokio::main]
async fn main() -> Result<()> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let mut args = std::env::args_os().skip(1);
    let mut baseline = None;
    let mut candidate = None;
    while let Some(arg) = args.next() {
        if arg == "--baseline-resources" {
            baseline = Some(PathBuf::from(
                args.next().ok_or("Missing baseline resources")?,
            ));
        } else if arg == "--candidate-resources" {
            candidate = Some(PathBuf::from(
                args.next().ok_or("Missing candidate resources")?,
            ));
        } else {
            return Err("Unknown publication comparison argument".into());
        }
    }
    let baseline = packaged(&baseline.ok_or("Pass --baseline-resources")?)?;
    let candidate_mode = if candidate.is_some() {
        "packaged"
    } else {
        "development"
    };
    let candidate = if let Some(resources) = candidate {
        packaged(&resources)?
    } else {
        WorkerConfig::development(&repo)
    };
    assert_eq!(
        baseline.containment,
        cadpad_core::worker::Containment::Required
    );
    assert_eq!(
        candidate.containment,
        cadpad_core::worker::Containment::Required
    );
    let profiles=vec![
  ("reference-100",include_str!("../../../fixtures/performance_reference/model.py").to_owned(),include_str!("../../../fixtures/performance_reference/annotations.json")),
  ("reference-400",include_str!("../../../fixtures/performance_reference/model.py").replace("parameter(\"grid_side\", 10,","parameter(\"grid_side\", 20,"),include_str!("../../../fixtures/performance_reference/annotations.json")),
  ("occurrence-semantics",r#"
import cadquery as cq
shape=cq.Workplane().box(40,20,6).faces('>Z').workplane().pushPoints([(-10,0)]).hole(4)
show(shape,name='first',features={'top':shape.faces('>Z')},density=1000)
second=cq.Workplane().box(40,20,6).faces('>Z').workplane().pushPoints([(-10,0)]).hole(4)
show(second,name='second',position=(100,0,0),features={'bottom':second.faces('<Z')},density=2700)
show(cq.Workplane().box(40,20,6).faces('>Z').workplane().pushPoints([(10,0)]).hole(4),name='changed')
wire=cq.Workplane().rect(10,10)
show(wire,name='wire',kind='wire')
show(wire,name='second_wire',position=(50,0,0),kind='wire')
face=cq.Workplane().box(10,10,1).faces('>Z')
show(face,name='face',kind='surface')
show(face,name='second_face',position=(0,50,0),kind='surface')
class Custom(cq.Solid):
    def Volume(self): return self.measurement
base=cq.Workplane().box(10,10,10).val()
a=Custom(base.wrapped);a.measurement=100
b=Custom(base.wrapped);b.measurement=200
show(a,name='custom_first')
show(b,name='custom_second')
"#.to_owned(),"[]")];
    let mut cases = Vec::new();
    for (name, source, pins) in profiles {
        eprintln!("Comparing {name}");
        let old = publication(baseline.clone(), &source, pins).await?;
        let new = publication(candidate.clone(), &source, pins).await?;
        if let Some(path) = difference(&old, &new, "model") {
            return Err(format!("Publication differs for {name} at {path}").into());
        }
        cases.push(json!({"case":name,"source_sha256":hash(source),"pins_sha256":hash(pins),"instances":old["parts"].as_array().unwrap().len(),"model_sha256":hash(serde_json::to_vec(&old)?),"equal":true}));
    }
    let executable = std::env::current_exe()?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"status":"passed","required_containment":true,"baseline":"packaged","candidate":candidate_mode,"empty_path":std::env::var("PATH").ok().is_some_and(|path|path.is_empty()),"baseline_worker":baseline.executable,"baseline_worker_sha256":hash(std::fs::read(&baseline.executable)?),"candidate_worker":candidate.executable,"candidate_worker_sha256":hash(std::fs::read(&candidate.executable)?),"checker_sha256":hash(std::fs::read(executable)?),"checker_source_sha256":hash(include_str!("publication_comparison.rs")),"cases":cases,"scope":"Complete model JSON equality after normalizing only revision, including mesh policy/descriptors, topology IDs, exact metadata, per-instance features/density/poses and annotations. Every unique buffer checksum verified. No UI or rendering.","ui_launched":false})
        )?
    );
    Ok(())
}
