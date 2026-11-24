//! Live wiring: snapshot from a real project, compile an E3-like requirement
//! against it, dispatch plan chunks to the worker and check the raw replies
//! are stable. Needs the project virtual environment (`.venv`), so it is
//! ignored by default: `cargo test -p cadpad-core --test requirements_dispatch -- --ignored`.
use cadpad_core::{
    Core, portable,
    requirements::{
        ProjectSettings, Requirement, compile,
        dispatch::{ModelView, execute_plan_chunks},
        ir::ModelRef,
    },
    worker::WorkerConfig,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};

const CABINET: &str = r#"import cadquery as cq

DEPTH = 18


def cabinet(context):
    angle = context.parameter("door_angle", 0.0, unit="deg", minimum=0.0, maximum=130.0)
    frame = cq.Workplane().box(40, DEPTH, 60).faces(">Y").workplane().rect(34, 54).cutBlind(-DEPTH + 2)
    door = cq.Workplane().box(36, 2, 56).translate((0, DEPTH / 2 + 1.2, 0))
    # Hinge on the +x edge of the frame front.
    door = door.rotate((18, DEPTH / 2, 0), (18, DEPTH / 2, 1), -angle)
    context.show(frame, name="frame", features={"opening": frame.faces(">Y[-2]")})
    context.show(door, name="door")
"#;

fn config(contained: bool) -> WorkerConfig {
    let config = WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    );
    if contained {
        config
    } else {
        config.uncontained_for_trusted_tests()
    }
}

fn model() -> ModelRef {
    ModelRef {
        entry: "cabinet.py".into(),
        builder: "cabinet".into(),
    }
}

fn requirement() -> Requirement {
    let part = |name: &str| json!({"part": {"model": {"entry": "cabinet.py", "builder": "cabinet"}, "part": {"name": name}}});
    serde_json::from_value(json!({
        "ir_version": 1, "id": "e3", "name": "Door opens to 20 degrees",
        "text": "@[door](cabinet.py#cabinet/door) opens to 20° without hitting @[frame](cabinet.py#cabinet/frame)",
        "claims": [{"id": "c1", "rule": "moves_freely", "span": [0, 96], "args": {
            "moving": part("door"), "against": part("frame"),
            "over": {"states": {"kind": "param", "param": {"name": "door_angle"},
                "values": {"shape": "range",
                    "from": {"value": 0.0, "unit": "deg", "source": "model"},
                    "to": {"value": 20.0, "unit": "deg"},
                    "step": {"value": 5.0, "unit": "deg", "source": "inferred"}}}}}}],
        "allow": [], "answers": [{"phrase": "opens", "answer": "to the limit without hitting; no stop required"}]
    }))
    .unwrap()
}

fn chunk() -> Value {
    let states: Vec<Value> = [0.0, 5.0, 10.0, 15.0, 20.0]
        .iter()
        .map(|a| {
            json!({"id": format!("a{a}"), "params": {"door_angle": a}, "measurements": [
                {"id": "hit", "primitive": "interference", "a": ["door"], "b": ["frame"]},
                {"id": "gap", "primitive": "min_clearance", "a": ["door"], "b": ["frame"]}]})
        })
        .collect();
    json!({"chunk_id": "c1", "states": states,
           "limits": {"workers": 2, "chunk_time_limit_s": 600.0}, "cache": {"results": false}})
}

async fn run(contained: bool) -> (Value, Value, ModelView) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("cabinet.py"), CABINET).unwrap();
    portable::initialize_project_browser(&root).unwrap();
    let core = Core::open(temp.path().join("app"), config(contained))
        .await
        .unwrap();
    let project = core.open_project(root, "user").await.unwrap();
    let live = core
        .snapshot_for_workspace(&project.workspace_id, "user", None, &BTreeMap::new())
        .await
        .unwrap();
    let built = live.snapshot.model(&model()).unwrap();
    assert!(built.build_error.is_none(), "{:?}", built.build_error);
    let built = built.built.as_ref().unwrap();
    // Everything the compiler needs comes from the live build.
    assert_eq!(
        built
            .part("door")
            .unwrap()
            .source
            .as_ref()
            .unwrap()
            .function
            .as_deref(),
        Some("cabinet")
    );
    assert!(built.part("door").unwrap().geometry_hash.len() == 64);
    assert!(built.constants.iter().any(|c| c.name == "DEPTH"));
    let opening = built.features.iter().find(|f| f.name == "opening").unwrap();
    assert_eq!((opening.part.as_str(), opening.count), ("frame", 1));
    assert!(built.param("door_angle").is_some());

    let compiled = compile(&requirement(), &live.snapshot, &ProjectSettings::default());
    assert!(
        compiled.diagnostics.is_empty() && compiled.plan.is_some(),
        "{:?}",
        compiled.diagnostics
    );

    let view = live.views[&model()].clone();
    let first = execute_plan_chunks(&core, &view.workspace, vec![chunk()])
        .await
        .unwrap();
    let second = execute_plan_chunks(&core, &view.workspace, vec![chunk()])
        .await
        .unwrap();
    assert_eq!((first.len(), second.len()), (1, 1));
    (first[0].clone(), second[0].clone(), view)
}

#[tokio::test]
#[ignore = "needs the project virtualenv and runs real worker builds"]
async fn live_snapshot_compiles_and_dispatch_is_deterministic() {
    let mut hashes = Vec::new();
    for contained in [false, true] {
        let (a, b, _) = run(contained).await;
        assert_eq!(a["status"], "complete", "{a}");
        assert_eq!(a["results"], b["results"], "two runs differ");
        assert_eq!(a["results_sha256"], b["results_sha256"]);
        // Closed and slightly open doors touch or clear; the physics is
        // not asserted here, only that every measurement has a value.
        let states = a["results"]["states"].as_array().unwrap();
        assert_eq!(states.len(), 5);
        assert!(
            states
                .iter()
                .flat_map(|s| s["measurements"].as_array().unwrap())
                .all(|m| m["status"] == "ok"),
            "{a}"
        );
        hashes.push(a["results_sha256"].clone());
    }
    // Contained (in-process, pool capped to zero) and uncontained (forked
    // pool) workers agree byte for byte.
    assert_eq!(hashes[0], hashes[1]);
}
