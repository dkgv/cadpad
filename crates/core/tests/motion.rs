use cadpad_core::{
    Core, Job, JobState, kinematics,
    motion::{DefineMechanism, SetCoordinate},
    tools::ToolContext,
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
    .uncontained_for_trusted_tests()
}
async fn terminal(core: &Core, ws: &str, job: Job) -> Job {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let current = core.get_job(ws, &job.id).await.unwrap();
            if !matches!(current.state, JobState::Queued | JobState::Running) {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
async fn succeeded(core: &Core, ws: &str, job: Job) -> String {
    let job = terminal(core, ws, job).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.diagnostics);
    job.resulting_revision.unwrap()
}
const BOXES: &str = "import cadquery as cq\nshow(cq.Workplane().box(10,10,10),name='rail')\nshow(cq.Workplane().box(10,10,10),name='carriage',position=(20,0,0))";
fn mechanism() -> Value {
    json!({"schema_version":1,"joints":[{"id":"slide","parent":"rail","child":"carriage","kind":{"type":"prismatic","axis":[1,0,0]},"initial":0.02,"limits":[0.02,0.08]}]})
}
async fn model(core: &Core) -> (String, String) {
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), BOXES.into())]),
            "user",
        )
        .await
        .unwrap();
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            kinematics::decode(&mechanism()).unwrap(),
            "user",
        )
        .await
        .unwrap();
    let revision = succeeded(
        core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    (ws, revision)
}
fn entities(revision: &str) -> Value {
    json!([{"revision":revision,"instance_id":"rail","entity_id":"part:0"},{"revision":revision,"instance_id":"carriage","entity_id":"part:0"}])
}
#[tokio::test]
async fn authoritative_pose_cas_clamping_exact_queries_exports_and_design_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, revision) = model(&core).await;
    let original = core.get_project(&ws).await.unwrap();
    let snapshot = core.get_motion(&ws, &revision).await.unwrap();
    assert_eq!(snapshot.pose_version, 0);
    assert_eq!(
        snapshot.evaluation.poses["carriage"].position,
        [20., 0., 0.]
    );
    let mut channel = core.subscribe_motion();
    let args = SetCoordinate {
        workspace_id: ws.clone(),
        revision: revision.clone(),
        expected_pose_version: 0,
        joint_id: "slide".into(),
        coordinate: 1.,
        unit: "m".into(),
        check_collision: true,
    };
    let moved = core.set_coordinate("external:test", args).await.unwrap();
    assert!(moved.clamped);
    assert_eq!(moved.applied, 0.08);
    assert_eq!(moved.snapshot.pose_version, 1);
    assert_eq!(channel.recv().await.unwrap().pose_version, 1);
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
    assert_eq!(
        core.get_project(&ws).await.unwrap()["activity"],
        original["activity"]
    );
    let context = ToolContext {
        actor: "builtin:test".into(),
    };
    let stale = json!({"workspace_id":ws,"revision":revision,"expected_pose_version":0,"joint_id":"slide","coordinate":0.03,"unit":"m"});
    assert_eq!(
        core.dispatch(&context, "set_joint_coordinate", stale)
            .await
            .unwrap_err()
            .code(),
        "POSE_CONFLICT"
    );
    assert_eq!(
        core.query(
            &ws,
            &revision,
            "inspect",
            json!({"entities":entities(&revision),"query":"distance","pose_version":0})
        )
        .await
        .unwrap_err()
        .code(),
        "POSE_CONFLICT"
    );
    assert_eq!(
        core.query(
            &ws,
            &revision,
            "inspect",
            json!({"entities":entities(&revision),"query":"distance"})
        )
        .await
        .unwrap_err()
        .code(),
        "POSE_CONFLICT"
    );
    assert_eq!(
        core.query(
            &ws,
            &revision,
            "inspect",
            json!({"entities":entities(&revision),"query":"distance","pose_version":1,"poses":{}})
        )
        .await
        .unwrap_err()
        .code(),
        "INVALID_REQUEST"
    );
    let distance = core
        .query(
            &ws,
            &revision,
            "inspect",
            json!({"entities":entities(&revision),"query":"distance","pose_version":1}),
        )
        .await
        .unwrap();
    assert!((distance["distance"].as_f64().unwrap() - 70.).abs() < 1e-8);
    assert_eq!(distance["pose_version"], 1);
    let exported = core
        .dispatch(
            &context,
            "export",
            json!({"workspace_id":ws,"revision":revision,"format":"STEP","pose_version":1}),
        )
        .await
        .unwrap();
    let data = core
        .artifact(&ws, &revision, exported["handle"].as_str().unwrap())
        .await
        .unwrap();
    assert!(data.starts_with(b"ISO-10303-21;"));
    assert_eq!(exported["pose_version"], 1);
    let asset = core
        .stage_asset(&ws, &revision, None, "posed.step", data, "user")
        .await
        .unwrap();
    let replacement = "show(import_step('posed.step'),name='roundtrip')";
    let draft = core
        .update_source(
            &ws,
            &revision,
            Some(&asset.id),
            BTreeMap::from([("model.py".into(), replacement.into())]),
            "user",
        )
        .await
        .unwrap();
    let draft = core
        .stage_mechanism(&ws, &revision, Some(&draft.id), Default::default(), "user")
        .await
        .unwrap();
    let roundtrip = succeeded(
        &core,
        &ws,
        core.build(&ws, &revision, &draft.id, "user").await.unwrap(),
    )
    .await;
    let model = core.get_project(&ws).await.unwrap();
    assert!(
        (model["model"]["parts"][0]["bounds"]["size"][0]
            .as_f64()
            .unwrap()
            - 90.)
            .abs()
            < 1e-8
    );
    let restored = succeeded(
        &core,
        &ws,
        core.undo(&ws, &roundtrip, "user").await.unwrap(),
    )
    .await;
    assert_eq!(
        core.get_motion(&ws, &restored).await.unwrap().pose_version,
        0
    );
    assert!(
        (core
            .get_motion(&ws, &restored)
            .await
            .unwrap()
            .evaluation
            .poses["carriage"]
            .position[0]
            - 20.)
            .abs()
            < 1e-10
    );
    let directory = temp.path().join("project");
    core.save_project(&ws, &restored, Some(directory.clone()))
        .await
        .unwrap();
    let saved: Value =
        serde_json::from_slice(&std::fs::read(directory.join("mechanism.json")).unwrap()).unwrap();
    assert_eq!(saved["joints"][0]["initial"], 0.02);
    let other = Core::open(temp.path().join("other"), config())
        .await
        .unwrap();
    let opened = other.open_project(directory, "user").await.unwrap();
    let revision = succeeded(
        &other,
        &opened.workspace_id,
        other
            .build(
                &opened.workspace_id,
                &opened.base_revision,
                &opened.draft.id,
                "user",
            )
            .await
            .unwrap(),
    )
    .await;
    let state = other
        .get_motion(&opened.workspace_id, &revision)
        .await
        .unwrap();
    assert_eq!(state.pose_version, 0);
    assert_eq!(state.evaluation.poses["carriage"].position, [20., 0., 0.]);
}
#[tokio::test]
async fn invalid_mechanism_candidate_and_stale_or_revoked_drivers_preserve_state() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let (ws, base) = model(&core).await;
    let mut invalid = mechanism();
    invalid["joints"][0]["child"] = json!("missing");
    let job = core
        .define_mechanism(
            "user",
            DefineMechanism {
                workspace_id: ws.clone(),
                expected_base_revision: base.clone(),
                draft_id: None,
                definition: kinematics::decode(&invalid).unwrap(),
            },
        )
        .await
        .unwrap();
    let job = terminal(&core, &ws, job).await;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.diagnostics["code"], "INVALID_MECHANISM");
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], base);
    core.revoke_actor("driver").await;
    let args = SetCoordinate {
        workspace_id: ws.clone(),
        revision: base.clone(),
        expected_pose_version: 0,
        joint_id: "slide".into(),
        coordinate: 0.04,
        unit: "m".into(),
        check_collision: true,
    };
    assert_eq!(
        core.set_coordinate("driver", args)
            .await
            .unwrap_err()
            .code(),
        "CANCELLED"
    );
    assert_eq!(core.get_motion(&ws, &base).await.unwrap().pose_version, 0);
    let args = SetCoordinate {
        workspace_id: ws.clone(),
        revision: "stale".into(),
        expected_pose_version: 0,
        joint_id: "slide".into(),
        coordinate: 0.04,
        unit: "m".into(),
        check_collision: true,
    };
    assert_eq!(
        core.set_coordinate("user", args).await.unwrap_err().code(),
        "STALE_ENTITY"
    );
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), BOXES.replace("carriage", "renamed"))]),
            "user",
        )
        .await
        .unwrap();
    let mut updated = mechanism();
    updated["joints"][0]["child"] = json!("renamed");
    let job = core
        .define_mechanism(
            "user",
            DefineMechanism {
                workspace_id: ws.clone(),
                expected_base_revision: base,
                draft_id: Some(draft.id),
                definition: kinematics::decode(&updated).unwrap(),
            },
        )
        .await
        .unwrap();
    let revision = succeeded(&core, &ws, job).await;
    assert_eq!(
        core.get_motion(&ws, &revision)
            .await
            .unwrap()
            .evaluation
            .joints[0]
            .child,
        "renamed"
    );
}

#[tokio::test]
async fn moving_during_a_native_query_or_capture_never_publishes_a_stale_measurement() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let (ws, base) = model(&core).await;
    let marker = temp.path().join("entered-inspection");
    let source = format!(
        "from cadpad_worker.model import Model\nfrom pathlib import Path\nimport time\noriginal_inspect=Model.inspect\ndef inspect(self,args):\n    Path({:?}).write_text('entered')\n    time.sleep(0.4)\n    return original_inspect(self,args)\nModel.inspect=inspect\n{}",
        marker.to_str().unwrap(),
        BOXES
    );
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), source)]),
            "user",
        )
        .await
        .unwrap();
    let revision = succeeded(
        &core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    let query_core = core.clone();
    let query_ws = ws.clone();
    let query_rev = revision.clone();
    let task = tokio::spawn(async move {
        query_core
            .query(
                &query_ws,
                &query_rev,
                "inspect",
                json!({"entities":entities(&query_rev),"query":"distance","pose_version":0}),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    core.set_coordinate(
        "user",
        SetCoordinate {
            workspace_id: ws.clone(),
            revision: revision.clone(),
            expected_pose_version: 0,
            joint_id: "slide".into(),
            coordinate: 0.04,
            unit: "m".into(),
            check_collision: true,
    },
    )
    .await
    .unwrap();
    assert_eq!(task.await.unwrap().unwrap_err().code(), "POSE_CONFLICT");
    let (token, mut requests) = core.attach_view();
    let query_core = core.clone();
    let query_ws = ws.clone();
    let query_rev = revision.clone();
    let task = tokio::spawn(async move {
        query_core
            .capture_view(
                "agent",
                serde_json::from_value(
                    json!({"workspace_id":query_ws,"revision":query_rev,"pose_version":1}),
                )
                .unwrap(),
            )
            .await
    });
    let request = requests.recv().await.unwrap();
    assert_eq!(request.arguments["motion"]["pose_version"], 1);
    assert_eq!(
        request.arguments["motion"]["poses"]["carriage"]["position"][0],
        40.
    );
    core.set_coordinate(
        "user",
        SetCoordinate {
            workspace_id: ws.clone(),
            revision: revision.clone(),
            expected_pose_version: 1,
            joint_id: "slide".into(),
            coordinate: 0.06,
            unit: "m".into(),
            check_collision: true,
    },
    )
    .await
    .unwrap();
    let image = json!({"image":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jA98AAAAASUVORK5CYII=","camera":{"position":[100,100,100],"target":[0,0,0],"quaternion":[0,0,0,1],"zoom":1,"orthographic":true},"revision":revision,"pose_version":1,"visible_part_ids":["rail","carriage"]});
    core.complete_view(
        &token,
        &request.id,
        cadpad_core::views::ViewReply {
            data: Some(image.clone()),
            error: None,
        },
    )
    .unwrap();
    assert_eq!(task.await.unwrap().unwrap_err().code(), "STALE_ENTITY");
    let query_core = core.clone();
    let query_ws = ws.clone();
    let query_rev = revision.clone();
    let task = tokio::spawn(async move {
        query_core
            .capture_view(
                "agent",
                serde_json::from_value(
                    json!({"workspace_id":query_ws,"revision":query_rev,"pose_version":2}),
                )
                .unwrap(),
            )
            .await
    });
    let request = requests.recv().await.unwrap();
    let mut image = image;
    image["pose_version"] = json!(2);
    core.complete_view(
        &token,
        &request.id,
        cadpad_core::views::ViewReply {
            data: Some(image),
            error: None,
        },
    )
    .unwrap();
    let captured = task.await.unwrap().unwrap();
    assert_eq!(captured["pose_version"], 2);
    assert_eq!(captured["poses"]["carriage"]["position"][0], 60.);
}

#[tokio::test]
async fn hinge_pose_keeps_local_dimensions_and_exports_rotated_step_and_stl() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let source = "import cadquery as cq\nshow(cq.Workplane().box(10,10,10),name='support')\nshow(cq.Workplane().box(20,4,4).translate((10,0,0)),name='arm',position=(0,0,10))";
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), source.into())]),
            "user",
        )
        .await
        .unwrap();
    let definition = json!({"joints":[{"id":"hinge","parent":"support","child":"arm","parent_frame":{"position_m":[0,0,0.01]},"kind":{"type":"revolute","axis":[0,0,1]},"initial":0,"limits":[-3.2,3.2]}]});
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            kinematics::decode(&definition).unwrap(),
            "user",
        )
        .await
        .unwrap();
    let revision = succeeded(
        &core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    core.set_coordinate(
        "user",
        SetCoordinate {
            workspace_id: ws.clone(),
            revision: revision.clone(),
            expected_pose_version: 0,
            joint_id: "hinge".into(),
            coordinate: std::f64::consts::FRAC_PI_2,
            unit: "rad".into(),
            check_collision: true,
    },
    )
    .await
    .unwrap();
    let inspected=core.query(&ws,&revision,"inspect",json!({"entities":[{"revision":revision,"instance_id":"arm","entity_id":"part:0"}],"pose_version":1})).await.unwrap();
    let entity = &inspected["entities"][0];
    for (actual, expected) in entity["bounds"]["size"]
        .as_array()
        .unwrap()
        .iter()
        .zip([20., 4., 4.])
    {
        assert!((actual.as_f64().unwrap() - expected).abs() < 1e-8);
    }
    for (actual, expected) in entity["world_center"]
        .as_array()
        .unwrap()
        .iter()
        .zip([0., 10., 10.])
    {
        assert!((actual.as_f64().unwrap() - expected).abs() < 1e-8);
    }
    for (actual, expected) in entity["world_bounds"]["size"]
        .as_array()
        .unwrap()
        .iter()
        .zip([4., 20., 4.])
    {
        assert!((actual.as_f64().unwrap() - expected).abs() < 1e-8);
    }
    let exported = core
        .query(
            &ws,
            &revision,
            "export",
            json!({"format":"STL","pose_version":1}),
        )
        .await
        .unwrap();
    let data = core
        .artifact(&ws, &revision, exported["handle"].as_str().unwrap())
        .await
        .unwrap();
    let mut vertices = vec![];
    if data.len() >= 84
        && data.len() == 84 + u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize * 50
    {
        for triangle in data[84..].as_chunks::<50>().0 {
            for point in triangle[12..48].as_chunks::<12>().0 {
                vertices.push([
                    f32::from_le_bytes(point[0..4].try_into().unwrap()) as f64,
                    f32::from_le_bytes(point[4..8].try_into().unwrap()) as f64,
                    f32::from_le_bytes(point[8..12].try_into().unwrap()) as f64,
                ]);
            }
        }
    } else {
        let text = std::str::from_utf8(&data).unwrap();
        let mut words = text.split_whitespace();
        while let Some(word) = words.next() {
            if word == "vertex" {
                vertices.push([
                    words.next().unwrap().parse().unwrap(),
                    words.next().unwrap().parse().unwrap(),
                    words.next().unwrap().parse().unwrap(),
                ]);
            }
        }
    }
    assert!(vertices.len() >= 36);
    for (axis, (expected_min, expected_max)) in
        [(-5., 5.), (-5., 20.), (-5., 12.)].into_iter().enumerate()
    {
        let min = vertices
            .iter()
            .map(|p| p[axis])
            .fold(f64::INFINITY, f64::min);
        let max = vertices
            .iter()
            .map(|p| p[axis])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (min - expected_min).abs() < 1e-5 && (max - expected_max).abs() < 1e-5,
            "STL axis {axis}: {min}..{max}"
        );
    }
    let step = core
        .query(
            &ws,
            &revision,
            "export",
            json!({"format":"STEP","pose_version":1}),
        )
        .await
        .unwrap();
    assert_eq!(step["pose_version"], 1);
    let data = core
        .artifact(&ws, &revision, step["handle"].as_str().unwrap())
        .await
        .unwrap();
    let draft = core
        .stage_asset(&ws, &revision, None, "hinge.step", data, "user")
        .await
        .unwrap();
    let draft = core
        .update_source(
            &ws,
            &revision,
            Some(&draft.id),
            BTreeMap::from([(
                "model.py".into(),
                "show(import_step('hinge.step'),name='roundtrip')".into(),
            )]),
            "user",
        )
        .await
        .unwrap();
    let draft = core
        .stage_mechanism(&ws, &revision, Some(&draft.id), Default::default(), "user")
        .await
        .unwrap();
    succeeded(
        &core,
        &ws,
        core.build(&ws, &revision, &draft.id, "user").await.unwrap(),
    )
    .await;
    let model = core.get_project(&ws).await.unwrap();
    for (actual, expected) in model["model"]["parts"][0]["bounds"]["size"]
        .as_array()
        .unwrap()
        .iter()
        .zip([10., 25., 17.])
    {
        assert!((actual.as_f64().unwrap() - expected).abs() < 1e-8);
    }
}

#[tokio::test]
async fn slider_and_hinge_traverse_declared_metre_and_radian_limits_without_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("app"), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let source = "import cadquery as cq\nshow(cq.Workplane().box(10,10,10),name='rail')\nshow(cq.Workplane().box(10,10,10),name='carriage')\nshow(cq.Workplane().box(20,4,4).translate((10,0,0)),name='arm')";
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), source.into())]),
            "user",
        )
        .await
        .unwrap();
    // A 150 mm slider limit is 0.15 m in the motion engine; hinge limits are radians.
    let definition = json!({"joints":[
        {"id":"slide","parent":"rail","child":"carriage","kind":{"type":"prismatic","axis":[1,0,0]},"initial":0,"limits":[0,0.15]},
        {"id":"hinge","parent":"carriage","child":"arm","parent_frame":{"position_m":[0,0,0]},"kind":{"type":"revolute","axis":[0,0,1]},"initial":0,"limits":[-1.2,1.2]}
    ]});
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            kinematics::decode(&definition).unwrap(),
            "user",
        )
        .await
        .unwrap();
    let revision = succeeded(
        &core,
        &ws,
        core.build(&ws, &base, &draft.id, "user").await.unwrap(),
    )
    .await;
    let before = core.get_project(&ws).await.unwrap();
    let mut version = 0u64;
    for target in [0.075, 0.15, 0.3, 0.] {
        let moved = core
            .set_coordinate(
                "user",
                SetCoordinate {
                    workspace_id: ws.clone(),
                    revision: revision.clone(),
                    expected_pose_version: version,
                    joint_id: "slide".into(),
                    coordinate: target,
                    unit: "m".into(),
                    check_collision: true,
    },
            )
            .await
            .unwrap();
        assert_eq!(moved.applied, target.clamp(0., 0.15));
        assert_eq!(moved.clamped, target > 0.15);
        assert!(
            (moved.snapshot.evaluation.poses["carriage"].position[0] - moved.applied * 1000.).abs()
                < 1e-9
        );
        version = moved.snapshot.pose_version;
    }
    for target in [0.6, 1.2, 2., -1.2] {
        let moved = core
            .set_coordinate(
                "user",
                SetCoordinate {
                    workspace_id: ws.clone(),
                    revision: revision.clone(),
                    expected_pose_version: version,
                    joint_id: "hinge".into(),
                    coordinate: target,
                    unit: "rad".into(),
                    check_collision: true,
    },
            )
            .await
            .unwrap();
        assert_eq!(moved.applied, target.clamp(-1.2, 1.2));
        assert_eq!(moved.clamped, !( (-1.2..=1.2).contains(&target) ));
        version = moved.snapshot.pose_version;
    }
    let after = core.get_project(&ws).await.unwrap();
    assert_eq!(after["revision"], before["revision"]);
    assert_eq!(after["activity"], before["activity"]);
    assert_eq!(after["model"]["parts"], before["model"]["parts"]);
    assert_eq!(
        core.dispatch(
            &ToolContext {
                actor: "user".into()
            },
            "set_joint_coordinate",
            json!({"workspace_id":ws,"revision":revision,"expected_pose_version":version,"joint_id":"slide","coordinate":0.05,"unit":"mm"})
        )
        .await
        .unwrap_err()
        .code(),
        "INVALID_MECHANISM"
    );
}
