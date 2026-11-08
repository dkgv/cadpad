use cadpad_core::{
    Core,
    tools::{Entity, ToolContext},
    views::{Capture, ViewReply},
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
            BTreeMap::from([(
                "model.py".into(),
                "import cadquery as cq\nshow(cq.Workplane().box(50,20,6),name='plate')".into(),
            )]),
            "user",
        )
        .await
        .unwrap();
    let job = core.build(&ws, &base, &draft.id, "user").await.unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = core.get_job(&ws, &job.id).await.unwrap();
            if let Some(revision) = job.resulting_revision {
                return (ws, revision);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
fn capture(ws: &str, revision: &str) -> Capture {
    serde_json::from_value(json!({"workspace_id":ws,"revision":revision})).unwrap()
}
pub fn image(revision: &str) -> Value {
    json!({"image":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jA98AAAAASUVORK5CYII=","camera":{"position":[100,100,100],"target":[0,0,0],"quaternion":[0,0,0,1],"zoom":1,"orthographic":true},"revision":revision,"pose_version":0,"visible_part_ids":["plate"]})
}
#[tokio::test]
async fn selection_capture_and_dimension_requests_keep_explicit_revision_scope() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let (ws, revision) = model(&core).await;
    core.set_selection(
        &ws,
        &revision,
        vec![Entity {
            revision: revision.clone(),
            instance_id: "plate".into(),
            entity_id: "face:0".into(),
        }],
    )
    .await
    .unwrap();
    let selected = core.get_selection(&ws).await.unwrap();
    assert_eq!(selected["entities"][0]["entity_id"], "face:0");
    assert_eq!(selected["parts"][0]["volume"], 6000.0);
    assert_eq!(
        core.capture_view("agent", capture(&ws, &revision))
            .await
            .unwrap_err()
            .code(),
        "APP_UNAVAILABLE"
    );
    let (token, mut requests) = core.attach_view();
    let task_core = core.clone();
    let ws_copy = ws.clone();
    let rev = revision.clone();
    let task = tokio::spawn(async move {
        task_core
            .capture_view("agent", capture(&ws_copy, &rev))
            .await
    });
    let request = requests.recv().await.unwrap();
    assert_eq!(request.workspace_id, ws);
    assert_eq!(request.revision, revision);
    core.complete_view(
        &token,
        &request.id,
        ViewReply {
            data: Some(image(&revision)),
            error: None,
        },
    )
    .unwrap();
    let snapshot = task.await.unwrap().unwrap();
    assert_eq!(snapshot["numeric_context"][0]["volume"], 6000.0);
    assert_eq!(snapshot["width"], 1);
    assert_eq!(snapshot["image"]["mime_type"], "image/png");
    let definition = cadpad_core::tools::definition("capture_view").unwrap();
    let native_schema =
        jsonschema::validator_for(definition.output_schema.as_ref().unwrap()).unwrap();
    let mcp_schema =
        jsonschema::validator_for(definition.mcp_output_schema.as_ref().unwrap()).unwrap();
    assert!(native_schema.is_valid(&snapshot));
    let mut metadata = snapshot.clone();
    metadata.as_object_mut().unwrap().remove("image");
    assert!(
        !native_schema.is_valid(&metadata),
        "native capture must include its image"
    );
    assert!(mcp_schema.is_valid(&metadata));
    let mut broken = snapshot.clone();
    broken["image"]["mime_type"] = json!("image/jpeg");
    assert!(!native_schema.is_valid(&broken));
    let mut broken = snapshot.clone();
    broken["camera"]["quaternion"] = json!([0, 0, 1]);
    assert!(!native_schema.is_valid(&broken));
    let mut broken = snapshot.clone();
    broken["poses"]["plate"]["position"] = json!([0, 0]);
    assert!(!native_schema.is_valid(&broken));
    let mut broken = metadata.clone();
    broken["width"] = json!(0);
    assert!(!mcp_schema.is_valid(&broken));
    let mut broken = metadata.clone();
    broken["height"] = json!(1201);
    assert!(!mcp_schema.is_valid(&broken));
    let mut broken = metadata.clone();
    broken["numeric_context"][0]["coordinate_space"] = json!("world");
    assert!(!mcp_schema.is_valid(&broken));
    let mut broken = metadata.clone();
    broken["numeric_context"] = json!(vec![metadata["numeric_context"][0].clone(); 129]);
    assert!(!mcp_schema.is_valid(&broken));
    assert_eq!(
        core.complete_view(
            "obsolete-window",
            &request.id,
            ViewReply {
                data: Some(json!({})),
                error: None
            }
        )
        .unwrap_err()
        .code(),
        "STALE_ENTITY"
    );
    let task_core = core.clone();
    let ws_copy = ws.clone();
    let task = tokio::spawn(async move {
        task_core
            .dispatch(
                &ToolContext {
                    actor: "agent".into(),
                },
                "set_dimensions",
                json!({"workspace_id":ws_copy,"visible":true,"scope":"all"}),
            )
            .await
    });
    let request = requests.recv().await.unwrap();
    assert_eq!(request.command, "dimensions");
    core.complete_view(
        &token,
        &request.id,
        ViewReply {
            data: Some(json!({"workspace_id":request.workspace_id,"revision":request.revision,"visible":true,"scope":"all"})),
            error: None,
        },
    )
    .unwrap();
    assert_eq!(task.await.unwrap().unwrap()["visible"], true);
    let mut oversized = capture(&ws, &revision);
    oversized.width = Some(10000);
    assert_eq!(
        core.capture_view("agent", oversized)
            .await
            .unwrap_err()
            .code(),
        "INVALID_PARAMETER"
    );
    assert!(
        core.set_selection(
            &ws,
            &revision,
            vec![Entity {
                revision: revision.clone(),
                instance_id: "plate".into(),
                entity_id: "face:999".into()
            }]
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn stale_images_window_replacement_and_actor_cancellation_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().into(), config()).await.unwrap();
    let (ws, revision) = model(&core).await;
    let (token, mut requests) = core.attach_view();
    let task_core = core.clone();
    let ws_copy = ws.clone();
    let rev = revision.clone();
    let task = tokio::spawn(async move {
        task_core
            .capture_view("agent", capture(&ws_copy, &rev))
            .await
    });
    let request = requests.recv().await.unwrap();
    core.complete_view(
        &token,
        &request.id,
        ViewReply {
            data: Some(image("obsolete")),
            error: None,
        },
    )
    .unwrap();
    assert_eq!(task.await.unwrap().unwrap_err().code(), "STALE_ENTITY");
    let task_core = core.clone();
    let ws_copy = ws.clone();
    let rev = revision.clone();
    let task = tokio::spawn(async move {
        task_core
            .capture_view("agent", capture(&ws_copy, &rev))
            .await
    });
    let _ = requests.recv().await.unwrap();
    let (_, mut current) = core.attach_view();
    assert_eq!(task.await.unwrap().unwrap_err().code(), "APP_UNAVAILABLE");
    let task_core = core.clone();
    let ws_copy = ws.clone();
    let task = tokio::spawn(async move {
        task_core
            .capture_view("agent", capture(&ws_copy, &revision))
            .await
    });
    let _ = current.recv().await.unwrap();
    core.revoke_actor("agent").await;
    assert_eq!(task.await.unwrap().unwrap_err().code(), "CANCELLED");
    assert_eq!(
        core.dispatch(
            &ToolContext {
                actor: "agent".into()
            },
            "get_project",
            json!({"workspace_id":ws})
        )
        .await
        .unwrap_err()
        .code(),
        "CANCELLED"
    );
}
