//! Real contained workers and scoped binary transport; no application UI.
use cadpad_core::{Core, Job, JobState, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
const SOURCE: &str = include_str!("../../../fixtures/display_reuse.py");
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
async fn finished(core: &Core, ws: &str, job: Job) -> String {
    let job = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    job.resulting_revision.unwrap()
}
async fn build(core: &Core, ws: &str) -> String {
    let base = core.get_project(ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let draft = core
        .update_source(
            ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), SOURCE.into())]),
            "reuse-test",
        )
        .await
        .unwrap();
    finished(
        core,
        ws,
        core.build(ws, &base, &draft.id, "reuse-test")
            .await
            .unwrap(),
    )
    .await
}
fn cache(root: &Path, ws: &str, rev: &str) -> PathBuf {
    root.join("artifacts").join(ws).join(rev)
}
fn calls(root: &Path, ws: &str, rev: &str) -> usize {
    let directory = cache(root, ws, rev);
    assert_eq!(
        std::fs::read_to_string(directory.join("source-ran")).unwrap(),
        "source executed"
    );
    std::fs::read_to_string(directory.join("tessellator-calls")).map_or(0, |s| s.lines().count())
}
fn part<'a>(project: &'a Value, name: &str) -> &'a Value {
    project["model"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["instance_id"] == name)
        .unwrap()
}
async fn verify(core: &Core, root: &Path, ws: &str, project: &Value) {
    let revision = project["revision"].as_str().unwrap();
    for p in project["model"]["parts"].as_array().unwrap() {
        for info in p["mesh"]["buffers"].as_object().unwrap().values() {
            let handle = info["handle"].as_str().unwrap();
            let bytes = std::fs::read(cache(root, ws, revision).join(handle)).unwrap();
            assert_eq!(bytes.len() as u64, info["bytes"].as_u64().unwrap());
            assert_eq!(cadpad_core::portable::bytes_hash(&bytes), info["sha256"]);
            assert_eq!(core.artifact(ws, revision, handle).await.unwrap(), bytes);
        }
    }
}
#[tokio::test]
async fn cross_revision_reuse_skips_only_equivalent_tessellation_and_copies_do_not_alias() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let ws = core.create_workspace("reuse-test").await.unwrap();
    let first = build(&core, &ws).await;
    let original = core.get_project(&ws).await.unwrap();
    assert_eq!(calls(&root, &ws, &first), 2);
    let next = finished(
        &core,
        &ws,
        core.set_parameter(&ws, &first, "length", 20., "mm", "reuse-test")
            .await
            .unwrap(),
    )
    .await;
    let changed = core.get_project(&ws).await.unwrap();
    assert_eq!(calls(&root, &ws, &next), 1);
    assert_eq!(part(&changed, "box")["bounds"]["size"][0], 20.);
    assert_eq!(part(&changed, "round")["position"][0], 40.);
    assert_eq!(
        part(&changed, "round")["mesh"]["buffers"],
        part(&original, "round")["mesh"]["buffers"]
    );
    verify(&core, &root, &ws, &changed).await;
    let measured = core
        .query(
            &ws,
            &next,
            "inspect",
            json!({"entities":[{"revision":next,"instance_id":"round","entity_id":"face:0"}]}),
        )
        .await
        .unwrap();
    assert_eq!(measured["entities"][0]["radius"], 5.);
    assert!(
        core.query(
            &ws,
            &next,
            "inspect",
            json!({"entities":[{"revision":first,"instance_id":"round","entity_id":"face:0"}]})
        )
        .await
        .is_err()
    );
    let handle = part(&changed, "round")["mesh"]["buffers"]["vertices"]["handle"]
        .as_str()
        .unwrap();
    let old = cache(&root, &ws, &first).join(handle);
    let new = cache(&root, &ws, &next).join(handle);
    let before = std::fs::read(&old).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_ne!(
            std::fs::metadata(&old).unwrap().ino(),
            std::fs::metadata(&new).unwrap().ino()
        );
    }
    let mut corrupt = std::fs::read(&new).unwrap();
    corrupt[0] ^= 1;
    std::fs::write(&new, corrupt).unwrap();
    assert_eq!(std::fs::read(&old).unwrap(), before);
    let repaired = build(&core, &ws).await;
    let project = core.get_project(&ws).await.unwrap();
    assert_eq!(calls(&root, &ws, &repaired), 1);
    verify(&core, &root, &ws, &project).await;
}
#[tokio::test]
async fn restart_can_reuse_verified_display_without_skipping_source_and_missing_buffers_regenerate()
{
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let ws = core.create_workspace("reuse-test").await.unwrap();
    let first = build(&core, &ws).await;
    drop(core);
    let core = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match Core::open(root.clone(), config()).await {
                Ok(core) => break core,
                Err(error) if error.code() == "APP_ALREADY_RUNNING" => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap();
    let second = build(&core, &ws).await;
    assert_ne!(first, second);
    assert_eq!(calls(&root, &ws, &second), 0);
    let project = core.get_project(&ws).await.unwrap();
    verify(&core, &root, &ws, &project).await;
    let handle = part(&project, "round")["mesh"]["buffers"]["vertices"]["handle"]
        .as_str()
        .unwrap();
    std::fs::remove_file(cache(&root, &ws, &second).join(handle)).unwrap();
    let third = build(&core, &ws).await;
    let project = core.get_project(&ws).await.unwrap();
    assert_eq!(calls(&root, &ws, &third), 1);
    verify(&core, &root, &ws, &project).await;
}

#[tokio::test]
async fn exact_intrinsics_and_circular_discovery_keep_occurrence_semantics_and_rebuild_fresh() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("app");
    let core = Core::open(root.clone(), config()).await.unwrap();
    let ws = core.create_workspace("intrinsic-test").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = r#"
import cadquery as cq
width=parameter('width',40,unit='mm',minimum=30,maximum=100)
def plate(x):
    return cq.Workplane().box(width,20,6).faces('>Z').workplane().pushPoints([(x,0)]).hole(4)
a=plate(-width/4)
b=plate(-width/4)
show(a,name='first',features={'top':a.faces('>Z')},density=1000)
show(b,name='second',position=(100,0,0),features={'bottom':b.faces('<Z')},density=2700)
show(plate(width/4),name='changed')
"#;
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), source.into())]),
            "intrinsic-test",
        )
        .await
        .unwrap();
    let revision = finished(
        &core,
        &ws,
        core.build(&ws, &base, &draft.id, "intrinsic-test")
            .await
            .unwrap(),
    )
    .await;
    let project = core.get_project(&ws).await.unwrap();
    assert_eq!(
        part(&project, "first")["geometry_fingerprint"],
        part(&project, "second")["geometry_fingerprint"]
    );
    assert_ne!(
        part(&project, "first")["geometry_fingerprint"],
        part(&project, "changed")["geometry_fingerprint"]
    );
    assert_eq!(
        part(&project, "first")["bounds"],
        part(&project, "changed")["bounds"]
    );
    assert!(
        !part(&project, "first")["features"]["top"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !part(&project, "second")["features"]["bottom"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let exact=core.query(&ws,&revision,"inspect",json!({"entities":[{"revision":revision,"instance_id":"first","entity_id":"part:0"},{"revision":revision,"instance_id":"second","entity_id":"part:0"}]})).await.unwrap();
    for (index, (name, density)) in [("first", 1000.), ("second", 2700.)]
        .into_iter()
        .enumerate()
    {
        let metadata = part(&project, name);
        let volume = exact["entities"][index]["volume"].as_f64().unwrap();
        assert!((volume - (40. * 20. * 6. - std::f64::consts::PI * 4. * 6.)).abs() < 1e-6);
        assert!((metadata["volume"].as_f64().unwrap() - volume).abs() < 1e-9);
        assert!((metadata["mass_kg"].as_f64().unwrap() - volume * 1e-9 * density).abs() < 1e-12);
    }
    let minimum = exact["entities"][1]["world_bounds"]["min"][0]
        .as_f64()
        .unwrap();
    assert!((minimum - 80.).abs() < 1e-6);
    let automatic = project["model"]["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["automatic"] == true)
        .collect::<Vec<_>>();
    assert_eq!(automatic.len(), 3);
    for item in automatic {
        assert_eq!(item["value"], 4.);
        let x = (item["start"][0].as_f64().unwrap() + item["end"][0].as_f64().unwrap()) / 2.;
        assert!((x - if item["part"] == "changed" { 10. } else { -10. }).abs() < 1e-6);
        assert_eq!(item["part"], item["definition"]["instance_id"]);
    }
    let next = finished(
        &core,
        &ws,
        core.set_parameter(&ws, &revision, "width", 48., "mm", "intrinsic-test")
            .await
            .unwrap(),
    )
    .await;
    let updated = core.get_project(&ws).await.unwrap();
    let first = part(&updated, "first");
    assert_eq!(first["bounds"]["size"][0], 48.);
    assert_ne!(
        first["geometry_fingerprint"],
        part(&project, "first")["geometry_fingerprint"]
    );
    assert!(
        first["volume"].as_f64().unwrap() > part(&project, "first")["volume"].as_f64().unwrap()
    );
    let stale = core
        .query(
            &ws,
            &next,
            "inspect",
            json!({"entities":[{"revision":revision,"instance_id":"first","entity_id":"part:0"}]}),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code(), "STALE_ENTITY");
    verify(&core, &root, &ws, &updated).await;
}
