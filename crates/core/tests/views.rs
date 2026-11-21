use cadpad_core::{Core, tools::ToolContext, views::ViewReply, worker::WorkerConfig};
use serde_json::json;

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jA98AAAAASUVORK5CYII=";

#[tokio::test]
async fn multi_view_baselines_reuse_cameras_and_enforce_scope_and_retention() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path().into(), WorkerConfig::bundled("unused".into()))
        .await
        .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let other = core.create_workspace("user").await.unwrap();
    let rev = core.get_project(&ws).await.unwrap()["revision"].clone();
    let (token, mut requests) = core.attach_view();
    let view_core = core.clone();
    let renderer = tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            assert_eq!(request.command, "capture");
            let a = &request.arguments;
            let camera = if a["camera"].is_object() {
                assert!(a["camera_preset"].is_null());
                assert_eq!(a["width"], 1);
                assert_eq!(a["height"], 1);
                a["camera"].clone()
            } else {
                // Distinct cameras make accidental re-framing in compare_views observable.
                let x = match a["camera_preset"].as_str().unwrap() {
                    "front" => 2,
                    "right" => 3,
                    "top" => 4,
                    _ => 1,
                };
                json!({"position":[x,10,10],"target":[0,0,0],"quaternion":[0,0,0,1],"zoom":x,"orthographic":true})
            };
            view_core.complete_view(&token, &request.id, ViewReply {
                data: Some(json!({"image":PNG,"camera":camera,"revision":request.revision,"pose_version":a["pose_version"],"visible_part_ids":[]})), error: None,
            }).unwrap();
        }
    });
    let actor = ToolContext {
        actor: "user".into(),
    };
    let camera = json!({"position":[8.0,10.0,10.0],"target":[0.0,0.0,0.0],"quaternion":[0.0,0.0,0.0,1.0],"zoom":2.0,"orthographic":true});
    let standalone = core
        .dispatch(
            &actor,
            "capture_views",
            json!({"workspace_id":ws,"revision":rev,"single":true,"camera":camera,
                "width":1,"height":1,"dimensions":true}),
        )
        .await
        .unwrap();
    assert_eq!(standalone["camera"], camera);
    assert_eq!(standalone["image"]["data"], PNG);
    assert!(standalone.get("capture_id").is_none());
    let baseline = core
        .dispatch(
            &actor,
            "capture_views",
            json!({"workspace_id":ws,"revision":rev}),
        )
        .await
        .unwrap();
    assert_eq!(baseline["views"].as_array().unwrap().len(), 4);
    assert_eq!(baseline["images"].as_array().unwrap().len(), 4);
    assert!(baseline["compared_to"].is_null());
    let id = baseline["capture_id"].clone();
    let comparison = core
        .dispatch(
            &actor,
            "compare_views",
            json!({"workspace_id":ws,"revision":rev,"capture_id":id}),
        )
        .await
        .unwrap();
    assert_eq!(comparison["compared_to"], id);
    assert_eq!(comparison["images"].as_array().unwrap().len(), 8);
    assert_eq!(comparison["images"][0]["label"], "iso before");
    assert_eq!(comparison["images"][1]["label"], "iso after");
    for view in comparison["views"].as_array().unwrap() {
        assert_eq!(view["before"]["camera"], view["after"]["camera"]);
        assert_eq!(view["before"]["revision"], rev);
    }
    for (name, reply) in [("capture_views", &baseline), ("compare_views", &comparison)] {
        let spec = cadpad_core::tools::definition(name).unwrap();
        assert!(
            jsonschema::validator_for(spec.output_schema.as_ref().unwrap())
                .unwrap()
                .is_valid(reply)
        );
        let mut metadata = reply.clone();
        metadata.as_object_mut().unwrap().remove("images");
        assert!(
            jsonschema::validator_for(spec.mcp_output_schema.as_ref().unwrap())
                .unwrap()
                .is_valid(&metadata)
        );
    }
    for (who, workspace) in [("foreign", &ws), ("user", &other)] {
        let err = core
            .dispatch(
                &ToolContext { actor: who.into() },
                "compare_views",
                json!({"workspace_id":workspace,"revision":rev,"capture_id":id}),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), "INVALID_REQUEST");
    }
    for views in [
        json!([]),
        json!(["iso", "iso"]),
        json!(["iso", "front", "right", "top", "left"]),
    ] {
        assert_eq!(
            core.dispatch(
                &actor,
                "capture_views",
                json!({"workspace_id":ws,"revision":rev,"views":views})
            )
            .await
            .unwrap_err()
            .code(),
            "INVALID_PARAMETER"
        );
    }
    for (field, value, code) in [
        ("revision", json!("old"), "STALE_ENTITY"),
        ("pose_version", json!(1), "POSE_CONFLICT"),
        ("width", json!(0), "INVALID_PARAMETER"),
    ] {
        let mut args = json!({"workspace_id":ws,"revision":rev});
        args[field] = value;
        assert_eq!(
            core.dispatch(&actor, "capture_views", args)
                .await
                .unwrap_err()
                .code(),
            code
        );
    }
    // Eight newer sets evict the original without unbounded image retention.
    for _ in 0..8 {
        core.dispatch(
            &actor,
            "capture_views",
            json!({"workspace_id":ws,"revision":rev,"views":["iso"]}),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        core.dispatch(
            &actor,
            "compare_views",
            json!({"workspace_id":ws,"revision":rev,"capture_id":id})
        )
        .await
        .unwrap_err()
        .code(),
        "INVALID_REQUEST"
    );
    renderer.abort();
}
