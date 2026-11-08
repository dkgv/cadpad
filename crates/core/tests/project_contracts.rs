use cadpad_core::{
    Core, Job, JobState,
    tools::{self, ToolContext},
    worker::WorkerConfig,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

fn config() -> WorkerConfig {
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
}
fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(
        tools::definition("get_project")
            .unwrap()
            .output_schema
            .as_ref()
            .unwrap(),
    )
    .unwrap()
}
fn actor() -> ToolContext {
    ToolContext {
        actor: "user".into(),
    }
}
async fn terminal(core: &Core, ws: &str, job: Job) -> Job {
    tokio::time::timeout(Duration::from_secs(90), async {
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
async fn built(core: &Core, ws: &str, base: &str, draft: &str) -> String {
    let job = terminal(core, ws, core.build(ws, base, draft, "user").await.unwrap()).await;
    assert_eq!(job.state, JobState::Succeeded, "{}", job.diagnostics);
    job.resulting_revision.unwrap()
}
async fn context(core: &Core, ws: &str) -> Value {
    let mut expected = core.get_project(ws).await.unwrap();
    for part in expected["model"]["parts"].as_array_mut().unwrap() {
        part.as_object_mut().unwrap().remove("mesh");
    }
    let activity = expected["activity"].as_array_mut().unwrap();
    if activity.len() > 20 {
        activity.drain(..activity.len() - 20);
    }
    for job in activity {
        job["diagnostics"] =
            json!({"code":job["diagnostics"]["code"],"message":job["diagnostics"]["message"]});
    }
    let value = core
        .dispatch(&actor(), "get_project", json!({"workspace_id":ws}))
        .await
        .unwrap();
    assert_eq!(
        value, expected,
        "typed context must preserve every field except deliberate mesh/log projection"
    );
    let schema = validator();
    assert!(
        schema.is_valid(&value),
        "{:?}",
        schema.iter_errors(&value).collect::<Vec<_>>()
    );
    value
}

#[tokio::test]
async fn listing_and_empty_scratchpad_are_typed_without_inventing_portable_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let schema = validator();
    let empty = core
        .dispatch(&actor(), "get_project", json!({}))
        .await
        .unwrap();
    assert!(schema.is_valid(&empty));
    assert_eq!(empty["workspace_ids"], json!([]));
    let ws = core.create_workspace("user").await.unwrap();
    let listing = core
        .dispatch(&actor(), "get_project", json!({}))
        .await
        .unwrap();
    assert!(schema.is_valid(&listing));
    assert_eq!(listing["workspace_ids"], json!([ws]));
    assert_eq!(listing["tools"].as_array().unwrap().len(), 23);
    let project = context(&core, &ws).await;
    assert_eq!(project["model"], json!({"parts":[]}));
    assert_eq!(project["mechanism"], json!({}));
    assert_eq!(project["annotations"], json!([]));
    assert_eq!(project["activity"], json!([]));
    let mut broken = listing;
    broken["workspace_ids"] = json!([42]);
    assert!(!schema.is_valid(&broken));
    let mut broken = project.clone();
    broken["protocol_version"] = json!("1");
    assert!(!schema.is_valid(&broken));
    let mut broken = project.clone();
    broken["motion"]["pose_version"] = json!(-1);
    assert!(!schema.is_valid(&broken));
    let mut broken = project;
    broken["model"] = json!({"unrestricted":true});
    assert!(!schema.is_valid(&broken));
}

const SOURCE: &str = r#"import cadquery as cq
width=parameter('width',80,minimum=40,maximum=120,description='Plate width')
parameter('swing',90,unit='deg')
plate=cq.Workplane().box(width,40,6).faces('>Z').workplane().hole(8)
show(plate,name='plate',density=7800,features={'bore':plate.faces('%CYLINDER')})
show(cq.Workplane().box(10,10,10),name='carriage',position=(100,0,0))
dimension('mounts','plate',(-30,0,3),(30,0,3))
angle('swing','plate',(10,0,3),(0,0,3),(0,10,3),parameter='swing')
"#;

#[tokio::test]
async fn malformed_publication_metadata_never_replaces_readable_committed_geometry() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let valid = "import cadquery as cq\nshow(cq.Workplane().box(10,20,30),name='box')\n";
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), valid.into())]),
            "user",
        )
        .await
        .unwrap();
    let revision = built(&core, &ws, &base, &draft.id).await;
    for invalid in [
        format!("{valid}\nparameter('width',10,description=42)"),
        format!("{valid}\ndimension('bad','box',(0,0,0),(1,0,0),label=42)"),
    ] {
        let draft = core
            .update_source(
                &ws,
                &revision,
                None,
                BTreeMap::from([("model.py".into(), invalid)]),
                "user",
            )
            .await
            .unwrap();
        let failed = terminal(
            &core,
            &ws,
            core.build(&ws, &revision, &draft.id, "user").await.unwrap(),
        )
        .await;
        assert_eq!(failed.state, JobState::Failed);
        assert_eq!(failed.diagnostics["code"], "INVALID_GEOMETRY");
        assert!(
            failed.diagnostics["message"]
                .as_str()
                .unwrap()
                .contains("Publication metadata")
        );
        let current = context(&core, &ws).await;
        assert_eq!(current["revision"], revision);
        assert_eq!(current["model"]["parts"][0]["volume"], 6000.);
    }
    // Valid geometry with malformed portable record metadata must also fail
    // before a commit; the worker itself ignores this extra root record field.
    let directory = temp.path().join("source");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("model.py"), valid).unwrap();
    for (name, value) in [
        (
            "project.json",
            json!({"schema_version":1,"units":"mm","entry_point":"model.py","dependencies":{},"source_files":["model.py"],"assets":[],"saved_revision":""}),
        ),
        ("parameters.json", json!({})),
        ("mechanism.json", json!({})),
        (
            "annotations.json",
            json!([{"id":"size","label":"Width","definition":{"kind":"overall","instance_id":"box","axis":"x"},"unsupported":true}]),
        ),
    ] {
        std::fs::write(directory.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
    }
    let error = core.open_project(directory, "user").await.unwrap_err();
    assert_eq!(error.code(), "INVALID_REQUEST");
    let preserved = context(&core, &ws).await;
    assert_eq!(preserved["revision"], revision);
    assert_eq!(preserved["model"]["parts"][0]["volume"], 6000.);
}

#[tokio::test]
async fn native_portable_project_context_keeps_source_presence_dimensions_recordings_and_bounded_history()
 {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("original"), config())
        .await
        .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
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
    let revision = built(&core, &ws, &base, &draft.id).await;
    let portable = temp.path().join("portable");
    core.save_project(&ws, &revision, Some(portable.clone()))
        .await
        .unwrap();
    let mechanism = json!({"joints":[{"id":"slide","parent":"plate","child":"carriage","kind":{"type":"prismatic","axis":[1,0,0]},"initial":0.1,"limits":[0.1,0.2]}],"collision":{"bodies":{"plate":[{"shape":{"type":"bounds"}}],"carriage":[{"shape":{"type":"bounds"}}]}}});
    let annotations = json!([
        {"id":"size","label":"Width","definition":{"kind":"overall","instance_id":"plate","axis":"x"}},
        {"id":"spacing","label":"Mount spacing","definition":{"kind":"published","publication_id":"mounts"}},
        {"id":"turn","label":"Swing","definition":{"kind":"published","publication_id":"swing"},"format":{"unit":"deg","precision":2},"parameter":null},
        {"id":"bad","label":"Missing feature","definition":{"kind":"radius","instance_id":"plate","feature":"missing"}}
    ]);
    for (name, value) in [
        ("mechanism.json", &mechanism),
        ("annotations.json", &annotations),
    ] {
        std::fs::write(portable.join(name), serde_json::to_vec(value).unwrap()).unwrap();
    }
    // Treat this as an existing source project without a saved checkpoint.
    // Checkpointed projects intentionally open immutable history until Reload.
    let manifest_path = portable.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["saved_revision"] = json!("");
    std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    // Opening existing portable source must not require a canonical serialization
    // with defaults filled in by the app.
    let root = temp.path().join("opened");
    let opened = Core::open(root.clone(), config()).await.unwrap();
    let staged = opened.open_project(portable.clone(), "user").await.unwrap();
    let ws = staged.workspace_id;
    let revision = built(&opened, &ws, &staged.base_revision, &staged.draft.id).await;
    let project = context(&opened, &ws).await;
    assert_eq!(project["mechanism"], mechanism);
    assert_eq!(project["annotations"], annotations);
    assert!(project["annotations"][0].get("format").is_none());
    assert!(project["annotations"][0].get("parameter").is_none());
    assert!(project["annotations"][2]["parameter"].is_null());
    assert_eq!(
        project["model"]["collision_validation"]["method"],
        "brep_boolean_coverage"
    );
    assert_eq!(project["parameters"]["width"]["value"], 80);
    let dimensions = project["model"]["annotations"].as_array().unwrap();
    assert!(
        dimensions
            .iter()
            .any(|d| d["automatic"] == true && d["kind"] == "diameter")
    );
    assert!(
        dimensions
            .iter()
            .any(|d| d["saved_id"] == "turn" && d["kind"] == "angle" && d["unit"] == "rad")
    );
    let broken = dimensions.iter().find(|d| d["saved_id"] == "bad").unwrap();
    assert_eq!(broken["status"], "broken");
    assert!(broken.get("value").is_none());
    assert!(broken.get("start").is_none());
    let start = opened.dispatch(&actor(),"control_playback",json!({"workspace_id":ws,"revision":revision,"expected_pose_version":0,"command":{"action":"start_recording","name":"Slide"}})).await.unwrap();
    let moved = opened.dispatch(&actor(),"set_joint_coordinate",json!({"workspace_id":ws,"revision":revision,"expected_pose_version":start["snapshot"]["pose_version"],"joint_id":"slide","coordinate":0.12,"unit":"m"})).await.unwrap();
    opened.dispatch(&actor(),"control_playback",json!({"workspace_id":ws,"revision":revision,"expected_pose_version":moved["snapshot"]["pose_version"],"command":{"action":"finish_recording"}})).await.unwrap();
    opened.save_project(&ws, &revision, None).await.unwrap();
    let recorded = context(&opened, &ws).await;
    assert_eq!(recorded["recordings"][0]["frame_count"], 2);
    assert_eq!(recorded["dirty"], false);
    assert_eq!(recorded["saved_revision"], revision);
    let invalid = opened
        .update_source(
            &ws,
            &revision,
            None,
            BTreeMap::from([(
                "model.py".into(),
                "raise ValueError('fixture build failure')".into(),
            )]),
            "user",
        )
        .await
        .unwrap();
    let failed = terminal(
        &opened,
        &ws,
        opened
            .build(&ws, &revision, &invalid.id, "user")
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(failed.state, JobState::Failed);
    let failed_context = context(&opened, &ws).await;
    assert!(
        failed_context["activity"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| job["diagnostics"]["code"] == "BUILD_FAILED")
    );
    let schema = validator();
    for mutation in ["axis", "unit", "status", "anchor", "parameter", "recording"] {
        let mut malformed = recorded.clone();
        match mutation {
            "axis" => malformed["mechanism"]["joints"][0]["kind"]["axis"] = json!([1, 0]),
            "unit" => malformed["model"]["units"] = json!("m"),
            "status" => malformed["model"]["annotations"][0]["status"] = json!("unknown"),
            "anchor" => malformed["annotations"][0]["definition"]["axis"] = json!("w"),
            "parameter" => malformed["parameters"]["width"]["value"] = json!("80"),
            "recording" => malformed["recordings"][0]["frame_count"] = json!("two"),
            _ => unreachable!(),
        }
        assert!(!schema.is_valid(&malformed), "{mutation}");
    }
    drop(opened);
    // A synthetic persisted history exercises the actual bounded read path
    // without starting 25 redundant Python builds.
    let path = root.join("workspaces").join(&ws).join("workspace.json");
    let mut stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let template = serde_json::to_value(failed).unwrap();
    for index in 0..25 {
        let mut job = template.clone();
        job["id"] = json!(format!("history-{index:02}"));
        job["created_at"] = json!(u64::MAX - 25 + index);
        stored["jobs"][format!("history-{index:02}")] = job;
    }
    let artifact = stored["revisions"][&revision]["model"]["parts"][0]["mesh"]["buffers"]["vertices"]["handle"].as_str().unwrap().to_owned();
    stored["revisions"][&revision]["model"]["parts"][0]
        .as_object_mut()
        .unwrap()
        .remove("geometry_fingerprint");
    std::fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
    std::fs::remove_file(
        root.join("artifacts")
            .join(&ws)
            .join(&revision)
            .join(artifact),
    )
    .unwrap();
    let reopened = Core::open(root, config()).await.unwrap();
    let recovered = context(&reopened, &ws).await;
    assert_eq!(recovered["activity"].as_array().unwrap().len(), 20);
    assert!(recovered["total_jobs"].as_u64().unwrap() > 20);
    assert!(!recovered["display_error"].is_null());
    assert_eq!(recovered["display_error"]["code"], "DISPLAY_CACHE_MISSING");
    assert!(
        recovered["model"]["parts"][0]
            .get("geometry_fingerprint")
            .is_none()
    );
    for job in recovered["activity"].as_array().unwrap() {
        assert!(job["diagnostics"].get("logs").is_none());
    }
}
