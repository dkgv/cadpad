use cadpad_core::{Core, JobState, tools, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

const SOURCE: &str = r#"import cadquery as cq
plate = cq.Workplane().box(80,40,6).faces('>Z').workplane().hole(8)
show(plate, name='plate', position=(10,20,30), density=7800, features={'top':plate.faces('>Z')})
show(cq.Workplane().sphere(5), name='sphere', position=(100,0,0))
show(cq.Solid.makeTorus(8,2), name='torus', position=(140,0,0))
show(cq.Workplane().circle(5), name='wire', position=(200,0,0), kind='wire')
show(cq.Face.makePlane(10,10), name='surface', position=(240,0,0), kind='surface')
show(cq.Workplane().box(10,10,10), name='overlap', position=(10,20,30))
"#;

fn validator(name: &str) -> jsonschema::Validator {
    jsonschema::validator_for(
        tools::definition(name)
            .unwrap()
            .output_schema
            .as_ref()
            .unwrap(),
    )
    .unwrap()
}
fn assert_same_fields(left: &Value, right: &Value, path: &str) {
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(
                a.keys().collect::<Vec<_>>(),
                b.keys().collect::<Vec<_>>(),
                "{path}"
            );
            for (key, value) in a {
                assert_same_fields(value, &b[key], &format!("{path}/{key}"));
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{path}");
            for (index, (a, b)) in a.iter().zip(b).enumerate() {
                assert_same_fields(a, b, &format!("{path}/{index}"));
            }
        }
        // JSON numbers such as a published integer coordinate 10 and typed
        // f64 coordinate 10.0 carry the same exact value. Do not use a tolerance.
        (Value::Number(a), Value::Number(b)) if a.is_f64() || b.is_f64() => {
            assert_eq!(a.as_f64(), b.as_f64(), "{path}")
        }
        _ => assert_eq!(left, right, "{path}"),
    }
}
async fn build(core: &Core, ws: &str, base: &str, source: &str) -> String {
    let draft = core
        .update_source(
            ws,
            base,
            None,
            BTreeMap::from([("model.py".into(), source.into())]),
            "user",
        )
        .await
        .unwrap();
    let job = core.build(ws, base, &draft.id, "user").await.unwrap();
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let job = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                assert_eq!(job.state, JobState::Succeeded, "{}", job.diagnostics);
                return job.resulting_revision.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn real_geometry_results_preserve_every_field_and_validate_distinct_shapes() {
    let directory = tempfile::tempdir().unwrap();
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    // Use the default contained native worker, with an isolated storage directory.
    let core = Core::open(
        directory.path().into(),
        WorkerConfig::development(&repository),
    )
    .await
    .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let revision = build(&core, &ws, &base, SOURCE).await;
    let project = core.get_project(&ws).await.unwrap();
    let actor = tools::ToolContext {
        actor: "user".into(),
    };
    let mut refs = vec![];
    for part in project["model"]["parts"].as_array().unwrap() {
        for (kind, count) in [
            ("part", 1),
            ("face", part["face_count"].as_u64().unwrap()),
            ("edge", part["edge_count"].as_u64().unwrap()),
        ] {
            for index in 0..count {
                refs.push(json!({"revision":revision,"instance_id":part["instance_id"],"entity_id":format!("{kind}:{index}")}));
            }
        }
    }
    let args = json!({"workspace_id":ws,"revision":revision,"entities":refs});
    let raw = core
        .query(&ws, &revision, "inspect", args.clone())
        .await
        .unwrap();
    let inspected = core.dispatch(&actor, "inspect", args).await.unwrap();
    assert_same_fields(&inspected, &raw, "inspection");
    let inspect = validator("inspect");
    assert!(
        inspect.is_valid(&inspected),
        "{:?}",
        inspect.iter_errors(&inspected).collect::<Vec<_>>()
    );
    let entities = inspected["entities"].as_array().unwrap();
    let plate = entities
        .iter()
        .position(|e| e["kind"] == "part" && e["instance_id"] == "plate")
        .unwrap();
    let cylinder = entities
        .iter()
        .position(|e| e["surface_type"] == "CYLINDER")
        .unwrap();
    let plane = entities
        .iter()
        .position(|e| e["surface_type"] == "PLANE")
        .unwrap();
    let circle = entities
        .iter()
        .position(|e| e["curve_type"] == "CIRCLE")
        .unwrap();
    assert!(
        (entities[plate]["mass_kg"].as_f64().unwrap()
            - (80. * 40. * 6. - std::f64::consts::PI * 16. * 6.) * 1e-9 * 7800.)
            .abs()
            < 1e-9
    );
    assert_eq!(entities[cylinder]["radius"], 4.);
    assert!(
        entities
            .iter()
            .any(|e| e["surface_type"] == "SPHERE" && e["radius"] == 5.)
    );
    assert!(
        entities
            .iter()
            .any(|e| e["surface_type"] == "TORUS" && e.get("radius").is_none())
    );
    assert!(entities.iter().any(|e| {
        e["semantic_labels"]
            .as_array()
            .is_some_and(|labels| labels.contains(&json!("top")))
    }));
    for (axis, offset) in [10., 20., 30.].iter().enumerate() {
        assert!(
            (entities[plate]["world_center"][axis].as_f64().unwrap()
                - entities[plate]["center"][axis].as_f64().unwrap()
                - offset)
                .abs()
                < 1e-9
        );
    }
    let invalid = |value: &Value| {
        assert!(
            !inspect.is_valid(value),
            "malformed inspection accepted: {value}"
        )
    };
    for field in ["units", "accuracy"] {
        let mut broken = inspected.clone();
        broken[field] = json!("invalid");
        invalid(&broken);
    }
    let mut broken = inspected.clone();
    broken["revision"] = json!(42);
    invalid(&broken);
    let mut broken = inspected.clone();
    broken["pose_version"] = json!(-1);
    invalid(&broken);
    let mut broken = inspected.clone();
    broken["entities"][plate]["kind"] = json!("vertex");
    invalid(&broken);
    let mut broken = inspected.clone();
    broken["entities"][plate]
        .as_object_mut()
        .unwrap()
        .remove("volume");
    invalid(&broken);
    let mut broken = inspected.clone();
    broken["entities"][plane]["normal"] = json!([0, 1]);
    invalid(&broken);
    let mut broken = inspected.clone();
    broken["entities"][circle]["anchor"]["kind"] = json!("local_point");
    invalid(&broken);
    let mut broken = inspected.clone();
    broken["entities"][cylinder]["radius"] = json!("4");
    invalid(&broken);
    let mut broken = inspected.clone();
    broken["entities"][plate]["bounds"]["size"] = json!([80, 40]);
    invalid(&broken);
    let part_refs: Vec<_> = refs
        .iter()
        .filter(|e| e["entity_id"] == "part:0")
        .cloned()
        .collect();
    core.set_selection(
        &ws,
        &revision,
        serde_json::from_value(json!(part_refs)).unwrap(),
    )
    .await
    .unwrap();
    let selection = core
        .dispatch(&actor, "get_selection", json!({"workspace_id":ws}))
        .await
        .unwrap();
    let select = validator("get_selection");
    assert!(select.is_valid(&selection));
    assert_eq!(selection["expired"], false);
    assert_eq!(selection["parts"].as_array().unwrap().len(), 6);
    assert!(
        selection["parts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["instance_id"] == "wire" && p["volume"].is_null())
    );
    let mut broken = selection.clone();
    broken["parts"][0]["features"] = json!({"top":[1]});
    assert!(!select.is_valid(&broken));
    let distance = core.dispatch(&actor, "inspect", json!({"workspace_id":ws,"revision":revision,"entities":[part_refs[0],part_refs[1]],"query":"distance"})).await.unwrap();
    assert!(inspect.is_valid(&distance));
    assert!(distance["distance"].is_number());
    let mut broken = distance.clone();
    broken["distance"] = json!(null);
    invalid(&broken);
    let interference = core.dispatch(&actor, "check_interference", json!({"workspace_id":ws,"revision":revision,"pose_version":0,"pairs":[["plate","sphere"],["plate","overlap"]]})).await.unwrap();
    let interference_schema = validator("check_interference");
    assert!(interference_schema.is_valid(&interference));
    assert!(interference["proxy"].is_null());
    let exact_raw = core
        .query(
            &ws,
            &revision,
            "inspect",
            json!({"query":"interference","pairs":[["plate","sphere"],["plate","overlap"]]}),
        )
        .await
        .unwrap();
    assert_same_fields(&interference["exact"], &exact_raw, "interference");
    assert_eq!(interference["exact"]["pairs"][0]["interfering"], false);
    assert_eq!(interference["exact"]["pairs"][1]["interfering"], true);
    assert!(
        interference["exact"]["pairs"][1]["intersection_volume_mm3"]
            .as_f64()
            .unwrap()
            > 0.
    );
    let mut broken = interference.clone();
    broken["exact"]["pairs"][0]["instances"] = json!(["plate"]);
    assert!(!interference_schema.is_valid(&broken));
    let mut broken = interference.clone();
    broken["exact"]["units"] = json!("m");
    assert!(!interference_schema.is_valid(&broken));
    let next = build(
        &core,
        &ws,
        &revision,
        &format!("{SOURCE}\n# next revision\n"),
    )
    .await;
    let expired = core
        .dispatch(&actor, "get_selection", json!({"workspace_id":ws}))
        .await
        .unwrap();
    assert!(select.is_valid(&expired));
    assert_eq!(expired["revision"], next);
    assert_eq!(expired["expired"], true);
    assert_eq!(expired["entities"], json!([]));
    assert_eq!(expired["parts"], json!([]));
}
