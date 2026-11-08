use cadpad_core::{Core, Job, JobState, annotations, tools::ToolContext, worker::WorkerConfig};
use serde_json::{Value, json};
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
async fn done(core: &Core, ws: &str, job: Job) -> String {
    let job = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let current = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(current.state, JobState::Queued | JobState::Running) {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    job.resulting_revision.unwrap()
}
async fn source(core: &Core, ws: &str, base: &str, text: &str) -> String {
    let draft = core
        .update_source(
            ws,
            base,
            None,
            BTreeMap::from([("model.py".into(), text.into())]),
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
fn width() -> Value {
    json!({"id":"width","label":"Width","definition":{"kind":"overall","instance_id":"box","axis":"x"},"format":{"unit":"in","precision":4},"parameter":"width","label_offset":[5.0,-3.0]})
}
const BOX: &str = "import cadquery as cq\nw=parameter('width',25.4,minimum=1,maximum=100)\nshow(cq.Workplane().box(w,20,6),name='box')";

fn angle() -> Value {
    json!({"id":"corner","label":"Included angle","definition":{"kind":"angle",
        "start":{"kind":"local_point","instance_id":"left","point":[10,0,0]},
        "vertex":{"kind":"local_point","instance_id":"left","point":[0,0,0]},
        "end":{"kind":"local_point","instance_id":"right","point":[0,0,0]}},
        "format":{"unit":"deg","precision":3}})
}

#[test]
fn angle_contract_validates_every_anchor_and_separates_angular_display_units() {
    assert!(annotations::decode(&json!([angle()])).is_ok());
    for unit in ["mm", "cm", "m", "in"] {
        let mut invalid = angle();
        invalid["format"]["unit"] = json!(unit);
        assert!(annotations::decode(&json!([invalid])).is_err());
    }
    for unit in ["deg", "rad"] {
        let mut valid = angle();
        valid["format"]["unit"] = json!(unit);
        assert!(annotations::decode(&json!([valid])).is_ok());
        let mut invalid = width();
        invalid["format"]["unit"] = json!(unit);
        assert!(annotations::decode(&json!([invalid])).is_err());
    }
    for key in ["start", "vertex", "end"] {
        let mut invalid = angle();
        invalid["definition"][key]["instance_id"] = json!("");
        assert!(annotations::decode(&json!([invalid])).is_err());
    }
}

#[tokio::test]
async fn three_point_angles_publish_rebuild_reopen_recover_and_break_without_stale_values() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let empty = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let text = "import cadquery as cq\nparameter('included',90,unit='deg')\nshow(cq.Workplane().box(2,2,2),name='left')\nshow(cq.Workplane().box(2,2,2),name='right',position=(0,20,0))\nangle('published','left',(10,0,0),(0,0,0),(0,10,0),parameter='included')";
    let revision = source(&core, &ws, &empty, text).await;
    let context = ToolContext {
        actor: "external:angle-test".into(),
    };
    let job = core
        .dispatch(
            &context,
            "set_annotation",
            json!({"workspace_id":ws,"expected_base_revision":revision,"annotation":angle()}),
        )
        .await
        .unwrap();
    let revision = done(&core, &ws, serde_json::from_value(job).unwrap()).await;
    let changed_text = text.replace("position=(0,20,0)", "position=(20,20,0)");
    let revision = source(&core, &ws, &revision, &changed_text).await;
    let project = core.get_project(&ws).await.unwrap();
    let saved = project["model"]["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["saved_id"] == "corner")
        .unwrap();
    assert!((saved["value"].as_f64().unwrap() - std::f64::consts::FRAC_PI_4).abs() < 1e-12);
    assert_eq!(saved["unit"], "rad");
    assert_eq!(saved["vertex_part"], "left");
    assert_eq!(project["model"]["annotations"][0]["kind"], "angle");
    assert_eq!(project["model"]["annotations"][0]["parameter"], "included");
    let portable = temp.path().join("portable");
    core.save_project(&ws, &revision, Some(portable.clone()))
        .await
        .unwrap();
    let disk: Value =
        serde_json::from_slice(&std::fs::read(portable.join("annotations.json")).unwrap()).unwrap();
    assert_eq!(
        disk,
        serde_json::to_value(annotations::decode(&json!([angle()])).unwrap()).unwrap()
    );
    let other_path = temp.path().join("other");
    let other = Core::open(other_path.clone(), config()).await.unwrap();
    let opened = other.open_project(portable, "user").await.unwrap();
    let job = other
        .build(
            &opened.workspace_id,
            &opened.base_revision,
            &opened.draft.id,
            "user",
        )
        .await
        .unwrap();
    let reopened = done(&other, &opened.workspace_id, job).await;
    assert_eq!(
        other.get_project(&opened.workspace_id).await.unwrap()["model"]["annotations"],
        project["model"]["annotations"]
    );
    drop(other);
    let recovered = Core::open(other_path, config()).await.unwrap();
    recovered
        .query(
            &opened.workspace_id,
            &reopened,
            "inspect",
            json!({"entities":[{"revision":reopened,"instance_id":"left","entity_id":"part:0"}]}),
        )
        .await
        .unwrap();
    assert_eq!(
        recovered.get_project(&opened.workspace_id).await.unwrap()["model"]["annotations"],
        project["model"]["annotations"]
    );
    source(
        &core,
        &ws,
        &revision,
        &changed_text.replace("name='right'", "name='renamed'"),
    )
    .await;
    let project = core.get_project(&ws).await.unwrap();
    let broken = project["model"]["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["saved_id"] == "corner")
        .unwrap();
    assert_eq!(broken["status"], "broken");
    for key in [
        "value",
        "start",
        "vertex",
        "end",
        "part",
        "end_part",
        "vertex_part",
    ] {
        assert!(broken.get(key).is_none(), "retained {key}");
    }
}

#[test]
fn saved_annotation_contract_rejects_invalid_or_ambiguous_documents() {
    assert!(annotations::decode(&json!([width()])).is_ok());
    assert!(annotations::decode(&json!([width(), width()])).is_err());
    let mut invalid = width();
    invalid["format"]["precision"] = json!(7);
    assert!(annotations::decode(&json!([invalid])).is_err());
    let mut invalid = width();
    invalid["definition"] = json!({"kind":"distance","start":{"kind":"bounds","instance_id":"box","fraction":[0,2,0]},"end":{"kind":"local_point","instance_id":"box","point":[0,0,0]}});
    assert!(annotations::decode(&json!([invalid])).is_err());
    let mut invalid = width();
    invalid["definition"] = json!({"kind":"face","instance_id":"box","entity_id":"face:0"});
    assert!(annotations::decode(&json!([invalid])).is_err());
    let mut invalid = width();
    invalid["unexpected"] = json!(true);
    assert!(annotations::decode(&json!([invalid])).is_err());
}

#[tokio::test]
async fn annotation_only_pin_and_format_skip_cad_source() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let empty = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let base = source(&core, &ws, &empty, BOX).await;
    let context = ToolContext {
        actor: "external:annotation-fast".into(),
    };
    let job = core
        .dispatch(
            &context,
            "set_annotation",
            json!({"workspace_id":ws,"expected_base_revision":base,"annotation":width()}),
        )
        .await
        .unwrap();
    let job: Job = serde_json::from_value(job).unwrap();
    let pinned = done(&core, &ws, job.clone()).await;
    let pin_job = core.get_job(&ws, &job.id).await.unwrap();
    assert!(
        pin_job.diagnostics["logs"]
            .as_str()
            .unwrap_or("")
            .contains("__source_skipped__"),
        "pin should reuse geometry: {:?}",
        pin_job.diagnostics
    );
    let mut formatted = width();
    formatted["format"]["precision"] = json!(3);
    let format_job = core
        .dispatch(
            &context,
            "set_annotation",
            json!({"workspace_id":ws,"expected_base_revision":pinned,"annotation":formatted}),
        )
        .await
        .unwrap();
    let format_job: Job = serde_json::from_value(format_job).unwrap();
    let formatted_revision = done(&core, &ws, format_job.clone()).await;
    let format_done = core.get_job(&ws, &format_job.id).await.unwrap();
    assert!(
        format_done.diagnostics["logs"]
            .as_str()
            .unwrap_or("")
            .contains("__source_skipped__"),
        "format edit should reuse geometry: {:?}",
        format_done.diagnostics
    );
    let project = core.get_project(&ws).await.unwrap();
    assert_eq!(project["revision"], formatted_revision);
    assert_eq!(project["model"]["annotations"][0]["format"]["precision"], 3);
    assert_eq!(project["model"]["annotations"][0]["value"], 25.4);
}

#[tokio::test]
async fn saved_annotations_share_dispatch_rebuild_undo_portable_reopen_and_repair() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let empty = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let base = source(&core, &ws, &empty, BOX).await;
    let context = ToolContext {
        actor: "external:test".into(),
    };
    let job = core
        .dispatch(
            &context,
            "set_annotation",
            json!({"workspace_id":ws,"expected_base_revision":base,"annotation":width()}),
        )
        .await
        .unwrap();
    let pinned = done(&core, &ws, serde_json::from_value(job).unwrap()).await;
    assert_eq!(
        core.dispatch(
            &context,
            "set_annotation",
            json!({"workspace_id":ws,"expected_base_revision":base,"annotation":width()})
        )
        .await
        .unwrap_err()
        .code(),
        "REVISION_CONFLICT"
    );
    let project = core.get_project(&ws).await.unwrap();
    assert_eq!(project["annotations"][0], width());
    assert_eq!(project["model"]["annotations"][0]["value"], 25.4);
    assert_eq!(project["model"]["annotations"][0]["format"]["unit"], "in");
    let changed = done(
        &core,
        &ws,
        core.set_parameter(&ws, &pinned, "width", 50.8, "mm", "user")
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        core.get_project(&ws).await.unwrap()["model"]["annotations"][0]["value"],
        50.8
    );
    let directory = temp.path().join("portable");
    core.save_project(&ws, &changed, Some(directory.clone()))
        .await
        .unwrap();
    let disk: Value =
        serde_json::from_slice(&std::fs::read(directory.join("annotations.json")).unwrap())
            .unwrap();
    assert_eq!(disk, json!([width()]));
    assert!(!disk.to_string().contains("value"));
    let other = Core::open(temp.path().join("other"), config())
        .await
        .unwrap();
    let opened = other.open_project(directory, "user").await.unwrap();
    let job = other
        .build(
            &opened.workspace_id,
            &opened.base_revision,
            &opened.draft.id,
            "user",
        )
        .await
        .unwrap();
    done(&other, &opened.workspace_id, job).await;
    assert_eq!(
        other.get_project(&opened.workspace_id).await.unwrap()["model"]["annotations"][0]["value"],
        50.8
    );
    // Missing part commits valid geometry, retaining a repairable definition and
    // omitting all old numbers/anchors from the new derived model.
    let broken = source(
        &core,
        &ws,
        &changed,
        &BOX.replace("name='box'", "name='renamed'"),
    )
    .await;
    let item = &core.get_project(&ws).await.unwrap()["model"]["annotations"][0];
    assert_eq!(item["status"], "broken");
    assert!(item.get("value").is_none() && item.get("start").is_none());
    let mut repair = width();
    repair["definition"]["instance_id"] = json!("renamed");
    let repaired_job = core
        .dispatch(
            &context,
            "set_annotation",
            json!({"workspace_id":ws,"expected_base_revision":broken,"annotation":repair}),
        )
        .await
        .unwrap();
    let repaired = done(&core, &ws, serde_json::from_value(repaired_job).unwrap()).await;
    assert_eq!(
        core.get_project(&ws).await.unwrap()["model"]["annotations"][0]["value"],
        50.8
    );
    let removed_job = core
        .dispatch(
            &context,
            "delete_annotation",
            json!({"workspace_id":ws,"expected_base_revision":repaired,"annotation_id":"width"}),
        )
        .await
        .unwrap();
    let removed = done(&core, &ws, serde_json::from_value(removed_job).unwrap()).await;
    assert_eq!(
        core.get_project(&ws).await.unwrap()["annotations"],
        json!([])
    );
    done(&core, &ws, core.undo(&ws, &removed, "user").await.unwrap()).await;
    assert_eq!(
        core.get_project(&ws).await.unwrap()["model"]["annotations"][0]["status"],
        "resolved"
    );
}

#[tokio::test]
async fn selected_geometry_measurements_are_portable_and_invalidate_when_geometry_changes() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let empty = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let text = "import cadquery as cq\nshow(cq.Workplane().box(20,10,4).faces('>Z').workplane().hole(4),name='plate')";
    let revision = source(&core, &ws, &empty, text).await;
    let project = core.get_project(&ws).await.unwrap();
    let automatic = project["model"]["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["automatic"] == true)
        .unwrap();
    assert_eq!(automatic["value"], 4.0);
    let count = project["model"]["parts"][0]["face_count"].as_u64().unwrap();
    let entities:Vec<_> = (0..count).map(|index|json!({"revision":revision,"instance_id":"plate","entity_id":format!("face:{index}")})).collect();
    let inspected = core
        .query(&ws, &revision, "inspect", json!({"entities":entities}))
        .await
        .unwrap();
    let face = inspected["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["surface_type"] == "CYLINDER")
        .unwrap();
    let distance = json!({"id":"selected-distance","label":"From bore center","definition":{"kind":"distance","start":face["anchor"],"end":{"kind":"local_point","instance_id":"plate","point":[10,0,0]}}});
    let circle = json!({"id":"selected-circle","label":"Bore diameter","definition":face["circular_definition"]});
    let mut invalid = circle.clone();
    invalid["definition"]["entity"]["format"] = json!("unknown");
    assert!(annotations::decode(&json!([invalid])).is_err());
    let context = ToolContext {
        actor: "external:test".into(),
    };
    let job = core
        .dispatch(
            &context,
            "set_annotation",
            json!({"workspace_id":ws,"expected_base_revision":revision,"annotation":distance}),
        )
        .await
        .unwrap();
    let revision = done(&core, &ws, serde_json::from_value(job).unwrap()).await;
    let job = core
        .dispatch(
            &context,
            "set_annotation",
            json!({"workspace_id":ws,"expected_base_revision":revision,"annotation":circle}),
        )
        .await
        .unwrap();
    let revision = done(&core, &ws, serde_json::from_value(job).unwrap()).await;
    let model = core.get_project(&ws).await.unwrap()["model"].clone();
    assert_eq!(model["annotations"].as_array().unwrap().len(), 2);
    assert!((model["annotations"][0]["value"].as_f64().unwrap() - 10.).abs() < 1e-10);
    assert_eq!(model["annotations"][1]["value"], 4.0);
    let directory = temp.path().join("portable");
    core.save_project(&ws, &revision, Some(directory.clone()))
        .await
        .unwrap();
    let other = Core::open(temp.path().join("other"), config())
        .await
        .unwrap();
    let opened = other.open_project(directory, "user").await.unwrap();
    let job = other
        .build(
            &opened.workspace_id,
            &opened.base_revision,
            &opened.draft.id,
            "user",
        )
        .await
        .unwrap();
    let reopened = done(&other, &opened.workspace_id, job).await;
    let recovered = other.get_project(&opened.workspace_id).await.unwrap();
    assert_eq!(recovered["revision"], reopened);
    assert_eq!(recovered["model"]["annotations"], model["annotations"]);
    drop(other);
    let restart = Core::open(temp.path().join("other"), config())
        .await
        .unwrap();
    let selected = recovered["model"]["parts"][0]["instance_id"]
        .as_str()
        .unwrap();
    restart
        .query(
            &opened.workspace_id,
            &reopened,
            "inspect",
            json!({"entities":[{"revision":reopened,"instance_id":selected,"entity_id":"part:0"}]}),
        )
        .await
        .unwrap();
    let changed = source(&core, &ws, &revision, &text.replace("hole(4)", "hole(6)")).await;
    assert_ne!(changed, revision);
    let updated = core.get_project(&ws).await.unwrap();
    for item in updated["model"]["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| !item["saved_id"].is_null())
    {
        assert_eq!(item["status"], "broken");
        assert!(item.get("value").is_none());
        assert!(item.get("start").is_none());
    }
    assert_eq!(
        updated["model"]["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["automatic"] == true)
            .unwrap()["value"],
        6.0
    );
}
