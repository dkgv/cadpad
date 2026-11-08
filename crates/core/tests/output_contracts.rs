use cadpad_core::{Core, Error, Job, JobKind, JobState, error_value, tools, worker::WorkerConfig};
use serde_json::json;

fn validator(name: &str) -> jsonschema::Validator {
    let schema = tools::definition(name)
        .unwrap()
        .output_schema
        .as_ref()
        .unwrap();
    jsonschema::validator_for(schema).unwrap()
}
#[test]
fn advertised_output_schemas_are_valid_and_accept_both_execution_error_forms() {
    let covered: Vec<_> = tools::definitions_ref()
        .iter()
        .filter(|tool| tool.output_schema.is_some())
        .collect();
    assert_eq!(covered.len(), 23);
    assert_eq!(
        covered
            .iter()
            .filter(|tool| tool.mcp_output_schema.is_some())
            .count(),
        2
    );
    for tool in covered {
        for schema in tool
            .output_schema
            .iter()
            .chain(tool.mcp_output_schema.iter())
        {
            assert_eq!(schema["type"], "object");
            assert!(jsonschema::meta::is_valid(schema), "{}", tool.name);
            let validator = jsonschema::validator_for(schema).unwrap();
            for error in [
                Error::domain("POSE_CONFLICT", "Refresh the pose"),
                Error::Io(std::io::Error::other("fixture IO failure")),
            ] {
                assert!(
                    validator.is_valid(&error_value(&error)),
                    "{}: {}",
                    tool.name,
                    error_value(&error)
                );
            }
            assert!(!validator.is_valid(&json!({"code":42,"message":"wrong code type"})));
            assert!(!validator.is_valid(&json!({"code":"MISSING_MESSAGE"})));
            assert!(!validator.is_valid(&json!({})));
        }
    }
}
#[tokio::test]
async fn motion_test_catalog_reply_matches_the_advertised_agent_contract() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(
        temp.path().join("app"),
        WorkerConfig::bundled("unused".into()),
    )
    .await
    .unwrap();
    let ws = core.create_workspace("test").await.unwrap();
    let rev = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let reply = core.dispatch(&tools::ToolContext { actor: "test".into() }, "control_motion_tests", json!({"workspace_id":ws,"revision":rev,"expected_catalog":null,"command":{"action":"list"}})).await.unwrap();
    assert!(
        validator("control_motion_tests").is_valid(&reply),
        "{reply}"
    );
    let mut invalid = reply;
    invalid["tests"] = json!("not a test list");
    assert!(!validator("control_motion_tests").is_valid(&invalid));
}
#[test]
fn job_schemas_match_serialized_fields_and_reject_invalid_states_and_types() {
    let job = Job {
        kind: JobKind::Build,
        id: "job".into(),
        draft_id: "draft".into(),
        snapshot_id: Some("snapshot".into()),
        actor: "user".into(),
        state: JobState::Queued,
        resulting_revision: None,
        diagnostics: json!({"logs":[]}),
        created_at: 0,
    };
    let mut value = serde_json::to_value(job).unwrap();
    for name in [
        "build",
        "set_annotation",
        "delete_annotation",
        "repair_display",
        "get_job",
        "cancel_job",
        "set_parameter",
        "define_mechanism",
        "undo",
    ] {
        let validator = validator(name);
        for state in ["queued", "running", "succeeded", "failed", "cancelled"] {
            value["state"] = json!(state);
            assert!(validator.is_valid(&value), "{name}: {value}");
        }
        let mut broken = value.clone();
        broken["state"] = json!("complete");
        assert!(!validator.is_valid(&broken));
        let mut broken = value.clone();
        broken.as_object_mut().unwrap().remove("created_at");
        assert!(
            !validator.is_valid(&broken),
            "Serialize contract must require always-emitted default fields"
        );
        let mut broken = value.clone();
        broken["id"] = json!(42);
        assert!(!validator.is_valid(&broken));
        let mut broken = value.clone();
        broken["snapshot_id"] = json!(42);
        assert!(!validator.is_valid(&broken));
    }
}
#[tokio::test]
async fn real_empty_workspace_motion_output_validates_units_vectors_and_flattened_fields() {
    let directory = tempfile::tempdir().unwrap();
    let core = Core::open(
        directory.path().into(),
        WorkerConfig::bundled("unused".into()),
    )
    .await
    .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let revision = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let value = core
        .dispatch(
            &tools::ToolContext {
                actor: "user".into(),
            },
            "get_motion",
            json!({"workspace_id":ws,"revision":revision}),
        )
        .await
        .unwrap();
    let valid = validator("get_motion");
    assert!(valid.is_valid(&value), "{value}");
    let mut broken = value.clone();
    broken["cad_units"] = json!("m");
    assert!(!valid.is_valid(&broken));
    let mut broken = value.clone();
    broken["mode"] = json!("unknown");
    assert!(!valid.is_valid(&broken));
    let mut broken = value.clone();
    broken["pose_version"] = json!(-1);
    assert!(!valid.is_valid(&broken));
    let mut broken = value.clone();
    broken["revision"] = json!(42);
    assert!(!valid.is_valid(&broken));
    let mut broken = value.clone();
    broken["poses"] = json!({"part":{"position":[0,0],"quaternion":[0,0,0,1]}});
    assert!(!valid.is_valid(&broken));
    let mut broken = value.clone();
    broken.as_object_mut().unwrap().remove("joints");
    assert!(!valid.is_valid(&broken));
    assert!(validator("control_simulation").is_valid(&value));
    assert!(validator("set_joint_coordinate").is_valid(
        &json!({"snapshot":value,"requested":0.,"applied":0.,"clamped":false,"collision":null})
    ));
    assert!(
        validator("control_playback")
            .is_valid(&json!({"snapshot":value,"recordings":[],"frame_times_s":null}))
    );
}
#[test]
fn old_app_tool_definitions_remain_readable_without_output_schema() {
    let value = json!({"name":"build","description":"older app","input_schema":{"type":"object"}});
    let tool: tools::ToolSpec = serde_json::from_value(value).unwrap();
    assert!(tool.output_schema.is_none());
    assert!(tool.mcp_output_schema.is_none());
    assert!(
        serde_json::to_value(tool)
            .unwrap()
            .get("output_schema")
            .is_none()
    );
}

#[test]
fn export_schemas_distinguish_native_artifact_metadata_from_mcp_resources() {
    let tool = tools::definition("export").unwrap();
    let native = validator("export");
    let mcp = jsonschema::validator_for(tool.mcp_output_schema.as_ref().unwrap()).unwrap();
    for format in ["STEP", "STL"] {
        let value = json!({"handle":"artifact.step","bytes":10,"sha256":"hash","format":format,"units":"mm","revision":"revision","pose_version":0});
        assert!(native.is_valid(&value));
        assert!(!mcp.is_valid(&value), "MCP resource URI must be present");
        let mut resource = value.clone();
        resource["resource_uri"] = json!("cadpad://workspace/revision/artifact.step");
        assert!(mcp.is_valid(&resource));
        let mut broken = resource.clone();
        broken["resource_uri"] = json!(42);
        assert!(!mcp.is_valid(&broken));
        let mut broken = value.clone();
        broken["format"] = json!("OBJ");
        assert!(!native.is_valid(&broken));
        let mut broken = value.clone();
        broken["bytes"] = json!(-1);
        assert!(!native.is_valid(&broken));
        let mut broken = value.clone();
        broken["units"] = json!("m");
        assert!(!native.is_valid(&broken));
        let mut broken = value.clone();
        broken.as_object_mut().unwrap().remove("sha256");
        assert!(!native.is_valid(&broken));
    }
}

#[test]
fn provider_projection_preserves_inputs_without_counting_unused_result_metadata() {
    let projected = tools::input_definitions();
    for (full, input) in tools::definitions_ref().iter().zip(&projected) {
        assert_eq!(full.name, input.name);
        assert_eq!(full.description, input.description);
        assert_eq!(full.input_schema, input.input_schema);
        assert!(input.output_schema.is_none());
        assert!(input.mcp_output_schema.is_none());
    }
    assert!(
        serde_json::to_vec(&projected).unwrap().len()
            < serde_json::to_vec(tools::definitions_ref()).unwrap().len()
    );
    assert!(projected.iter().all(|tool| {
        serde_json::to_value(tool)
            .unwrap()
            .get("output_schema")
            .is_none()
    }));
}

#[tokio::test]
async fn source_contracts_cover_revision_draft_cumulative_edits_and_persistence() {
    let directory = tempfile::tempdir().unwrap();
    let core = Core::open(
        directory.path().into(),
        WorkerConfig::bundled("unused".into()),
    )
    .await
    .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let project = core.get_project(&ws).await.unwrap();
    let revision = project["revision"].as_str().unwrap();
    let path = project["entry_point"].as_str().unwrap();
    let actor = tools::ToolContext {
        actor: "user".into(),
    };
    let read = validator("read_source");
    let update = validator("update_source");
    let original = core
        .dispatch(
            &actor,
            "read_source",
            json!({"workspace_id":ws,"version":revision,"path":path}),
        )
        .await
        .unwrap();
    assert!(read.is_valid(&original));
    let source = "# café ⚙\n";
    let draft = core
        .dispatch(
            &actor,
            "update_source",
            json!({
                "workspace_id":ws,"expected_base_revision":revision,"changes":{path:source}
            }),
        )
        .await
        .unwrap();
    assert!(update.is_valid(&draft), "{draft}");
    assert_eq!(draft["actor"], "user");
    assert_eq!(draft["base_revision"], revision);
    assert_eq!(draft["source_hashes"][path], original["hash"]);
    assert_eq!(draft["changed_files"][0]["bytes"], source.len());
    let edited = core
        .dispatch(
            &actor,
            "read_source",
            json!({
                "workspace_id":ws,"version":draft["id"],"path":path
            }),
        )
        .await
        .unwrap();
    assert!(read.is_valid(&edited));
    assert_eq!(edited["source"], source);
    assert_eq!(edited["hash"], draft["changed_files"][0]["hash"]);
    assert_ne!(edited["hash"], original["hash"]);
    let cumulative = core
        .dispatch(
            &actor,
            "update_source",
            json!({
                "workspace_id":ws,"expected_base_revision":revision,"draft_id":draft["id"],
                "changes":{"helper.py":"value = 42\n"}
            }),
        )
        .await
        .unwrap();
    assert!(update.is_valid(&cumulative));
    assert_eq!(cumulative["changed_files"].as_array().unwrap().len(), 2);
    assert_eq!(cumulative["source_hashes"], draft["source_hashes"]);
    assert!(cumulative["source_hashes"].get("helper.py").is_none());
    let reverted = core
        .dispatch(
            &actor,
            "update_source",
            json!({
                "workspace_id":ws,"expected_base_revision":revision,"draft_id":draft["id"],
                "changes":{path:original["source"]}
            }),
        )
        .await
        .unwrap();
    assert!(update.is_valid(&reverted));
    assert_eq!(reverted["changed_files"].as_array().unwrap().len(), 1);
    assert_eq!(reverted["changed_files"][0]["path"], "helper.py");
    let foreign = tools::ToolContext {
        actor: "other".into(),
    };
    let error = core.dispatch(&foreign, "update_source", json!({
        "workspace_id":ws,"expected_base_revision":revision,"draft_id":draft["id"],"changes":{}
    })).await.unwrap_err();
    assert_eq!(error.code(), "REVISION_CONFLICT");
    assert!(update.is_valid(&error_value(&error)));
    let mut broken = cumulative.clone();
    broken["changed_files"][0]["bytes"] = json!(-1);
    assert!(!update.is_valid(&broken));
    let mut broken = cumulative.clone();
    broken["source_hashes"][path] = json!(42);
    assert!(!update.is_valid(&broken));
    let mut broken = original.clone();
    broken.as_object_mut().unwrap().remove("source");
    assert!(!read.is_valid(&broken));
    drop(core);
    let reopened = Core::open(
        directory.path().into(),
        WorkerConfig::bundled("unused".into()),
    )
    .await
    .unwrap();
    let persisted = reopened
        .dispatch(
            &actor,
            "read_source",
            json!({
                "workspace_id":ws,"version":draft["id"],"path":"helper.py"
            }),
        )
        .await
        .unwrap();
    assert!(read.is_valid(&persisted));
    assert_eq!(persisted["source"], "value = 42\n");
    assert_eq!(
        reopened.read_source(&ws, revision, path).await.unwrap(),
        original
    );
}

#[tokio::test]
async fn dimension_acknowledgments_validate_shape_identity_and_requested_settings() {
    use cadpad_core::views::ViewReply;
    let directory = tempfile::tempdir().unwrap();
    let core = Core::open(
        directory.path().into(),
        WorkerConfig::bundled("unused".into()),
    )
    .await
    .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let revision = core.get_project(&ws).await.unwrap()["revision"].clone();
    let (token, mut requests) = core.attach_view();
    let valid = validator("set_dimensions");
    for (scope, visible) in [("selected", true), ("pinned", false), ("all", true)] {
        for variant in [
            "valid",
            "missing",
            "type",
            "workspace",
            "revision",
            "visible",
            "scope",
            "unknown_scope",
        ] {
            let task_core = core.clone();
            let args = json!({"workspace_id":ws,"visible":visible,"scope":scope});
            let task = tokio::spawn(async move {
                task_core
                    .dispatch(
                        &tools::ToolContext {
                            actor: "user".into(),
                        },
                        "set_dimensions",
                        args,
                    )
                    .await
            });
            let request = tokio::time::timeout(std::time::Duration::from_secs(2), requests.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(request.command, "dimensions");
            assert_eq!(request.revision, revision);
            let mut reply =
                json!({"workspace_id":ws,"revision":revision,"visible":visible,"scope":scope});
            match variant {
                "missing" => {
                    reply.as_object_mut().unwrap().remove("revision");
                }
                "type" => reply["visible"] = json!(1),
                "workspace" => reply["workspace_id"] = json!("other"),
                "revision" => reply["revision"] = json!("obsolete"),
                "visible" => reply["visible"] = json!(!visible),
                "scope" => reply["scope"] = json!(if scope == "all" { "selected" } else { "all" }),
                "unknown_scope" => reply["scope"] = json!("unknown"),
                _ => {}
            }
            core.complete_view(
                &token,
                &request.id,
                ViewReply {
                    data: Some(reply.clone()),
                    error: None,
                },
            )
            .unwrap();
            match task.await.unwrap() {
                Ok(value) => {
                    assert_eq!(variant, "valid");
                    assert_eq!(value, reply);
                    assert!(valid.is_valid(&value));
                }
                Err(error) => {
                    assert_ne!(variant, "valid");
                    assert_eq!(
                        error.code(),
                        if ["workspace", "revision"].contains(&variant) {
                            "STALE_ENTITY"
                        } else {
                            "INVALID_REQUEST"
                        }
                    );
                    assert!(valid.is_valid(&error_value(&error)));
                }
            }
        }
    }
}
