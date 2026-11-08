#![cfg(unix)]
use cadpad_core::{Core, worker::WorkerConfig};
use cadpad_mcp::{AppServer, Client, Connection};
use rmcp::{ServiceExt, model::CallToolRequestParams};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

fn config() -> WorkerConfig {
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
    .uncontained_for_trusted_tests()
}
async fn terminal(client: &Client, ws: &str, job: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = client
                .call("get_job", json!({"workspace_id":ws,"job_id":job["id"]}))
                .await
                .unwrap();
            if !["queued", "running"].contains(&job["state"].as_str().unwrap()) {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn authenticated_connections_share_real_projects_and_revoke_pending_work() {
    let root = tempfile::tempdir().unwrap();
    let core = Core::open(root.path().join("app"), config()).await.unwrap();
    assert_eq!(
        Core::open(root.path().join("app"), config())
            .await
            .err()
            .unwrap()
            .code(),
        "APP_UNAVAILABLE"
    );
    let ws = core.create_workspace("local-user").await.unwrap();
    let other = core.create_workspace("local-user").await.unwrap();
    let server = AppServer::start(core.clone(), root.path().join("connection"))
        .await
        .unwrap();
    assert_eq!(
        std::fs::metadata(server.connection_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let connection: Connection =
        serde_json::from_slice(&std::fs::read(server.connection_file()).unwrap()).unwrap();
    assert_eq!(
        std::fs::metadata(connection.endpoint)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let client = Client::connect(server.connection_file(), "Claude Code test")
        .await
        .unwrap();
    assert!(client.actor.starts_with("external:Claude Code test:"));
    // Model the fields consumed by an older bridge that cannot read the new
    // transport projection. It must not promise an incompatible native shape.
    #[derive(serde::Deserialize)]
    struct LegacyTool {
        name: String,
        #[serde(default)]
        output_schema: Option<Value>,
    }
    let definitions = client.definitions().await.unwrap();
    let legacy: Vec<LegacyTool> = serde_json::from_value(definitions.clone()).unwrap();
    for name in ["capture_view", "export"] {
        assert!(
            legacy
                .iter()
                .find(|tool| tool.name == name)
                .unwrap()
                .output_schema
                .is_none()
        );
        let advertised = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        assert_eq!(
            &advertised["mcp_output_schema"],
            cadpad_core::tools::definition(name)
                .unwrap()
                .mcp_output_schema
                .as_ref()
                .unwrap()
        );
        assert!(
            cadpad_core::tools::definition(name)
                .unwrap()
                .output_schema
                .is_some(),
            "shared native contract must remain available"
        );
    }
    assert!(
        legacy
            .iter()
            .find(|tool| tool.name == "build")
            .unwrap()
            .output_schema
            .is_some()
    );
    let project = client
        .call("get_project", json!({"workspace_id":ws}))
        .await
        .unwrap();
    let base = project["revision"].as_str().unwrap();
    let draft=client.call("update_source",json!({"workspace_id":ws,"expected_base_revision":base,"changes":{"model.py":"raise ValueError('repair me')"}})).await.unwrap();
    let job = client
        .call(
            "build",
            json!({"workspace_id":ws,"expected_base_revision":base,"draft_id":draft["id"]}),
        )
        .await
        .unwrap();
    let failure = terminal(&client, &ws, &job).await;
    assert_eq!(failure["state"], "failed");
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], base);
    let source = "import cadquery as cq\nw=parameter('width',50,minimum=1,maximum=100)\nshow(cq.Workplane().box(w,20,6),name='plate')";
    client.call("update_source",json!({"workspace_id":ws,"expected_base_revision":base,"draft_id":draft["id"],"changes":{"model.py":source}})).await.unwrap();
    let job = client
        .call(
            "build",
            json!({"workspace_id":ws,"expected_base_revision":base,"draft_id":draft["id"]}),
        )
        .await
        .unwrap();
    core.focus_workspace(&other).await.unwrap();
    let success = terminal(&client, &ws, &job).await;
    assert_eq!(success["state"], "succeeded", "{success}");
    let revision = success["resulting_revision"].as_str().unwrap();
    assert_eq!(
        core.get_project(&other).await.unwrap()["model"]["parts"],
        json!([])
    );
    let measured=client.call("inspect",json!({"workspace_id":ws,"revision":revision,"entities":[{"revision":revision,"instance_id":"plate","entity_id":"part:0"}]})).await.unwrap();
    assert_eq!(measured["entities"][0]["volume"], 6000.0);
    assert_eq!(
        client
            .call(
                "update_source",
                json!({"workspace_id":ws,"expected_base_revision":base,"changes":{}})
            )
            .await
            .unwrap_err()
            .code(),
        "REVISION_CONFLICT"
    );
    let slow=client.call("update_source",json!({"workspace_id":ws,"expected_base_revision":revision,"changes":{"model.py":"import time\ntime.sleep(120)"}})).await.unwrap();
    let job = client
        .call(
            "build",
            json!({"workspace_id":ws,"expected_base_revision":revision,"draft_id":slow["id"]}),
        )
        .await
        .unwrap();
    assert!(server.disconnect(&client.session_id).await.unwrap() >= 1);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state = core
                .get_job(&ws, job["id"].as_str().unwrap())
                .await
                .unwrap();
            if state.state == cadpad_core::JobState::Cancelled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
    assert!(
        client
            .call("get_project", json!({"workspace_id":ws}))
            .await
            .is_err()
    );
    server.shutdown().await;
    assert!(!server.connection_file().exists());
}

#[tokio::test]
async fn invalid_tokens_and_public_credentials_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let core = Core::open(root.path().join("app"), config()).await.unwrap();
    let server = AppServer::start(core, root.path().join("connection"))
        .await
        .unwrap();
    let mut connection: Connection =
        serde_json::from_slice(&std::fs::read(server.connection_file()).unwrap()).unwrap();
    connection.token = "invalid".into();
    let bad = root.path().join("bad.json");
    std::fs::write(&bad, serde_json::to_vec(&connection).unwrap()).unwrap();
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        Client::connect(&bad, "test").await.err().unwrap().code(),
        "INVALID_AUTH"
    );
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        Client::connect(&bad, "test").await.err().unwrap().code(),
        "INVALID_AUTH"
    );
    assert!(server.sessions().await.is_empty());
    server.shutdown().await;
}

#[tokio::test]
async fn real_stdio_mcp_negotiates_schemas_and_returns_actionable_tool_errors() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("absent-app/connection.json");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_cadpad-mcp"))
        .arg("--connection")
        .arg(&missing)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let mcp = ().serve((output, input)).await.unwrap();
    let tools = mcp.peer().list_tools(None).await.unwrap();
    assert!(tools.tools.iter().any(|tool| tool.name == "update_source"));
    assert!(
        tools.tools.iter().all(|tool| tool.output_schema.is_none()),
        "Offline discovery cannot promise an unavailable app's output version"
    );
    let result = mcp
        .peer()
        .call_tool(CallToolRequestParams::new("get_project").with_arguments(serde_json::Map::new()))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.unwrap()["code"],
        "APP_UNAVAILABLE"
    );
    assert!(
        !root.path().join("absent-app").exists(),
        "Bridge must not create a headless workspace"
    );
    // The same real stdio client can connect once the desktop core becomes
    // available, without ever constructing state in the bridge process.
    let core = Core::open(root.path().join("app"), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let server = AppServer::start(core.clone(), missing.parent().unwrap().to_path_buf())
        .await
        .unwrap();
    let project = sdk(mcp.peer(), "get_project", json!({"workspace_id":ws})).await;
    let live_tools = mcp.peer().list_tools(None).await.unwrap();
    let project_schema = serde_json::to_value(
        live_tools
            .tools
            .iter()
            .find(|tool| tool.name == "get_project")
            .unwrap()
            .output_schema
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        &project_schema,
        cadpad_core::tools::definition("get_project")
            .unwrap()
            .output_schema
            .as_ref()
            .unwrap()
    );
    let project_validator = jsonschema::validator_for(&project_schema).unwrap();
    assert!(project_validator.is_valid(&project));
    let listing = sdk(mcp.peer(), "get_project", json!({})).await;
    assert!(project_validator.is_valid(&listing));
    assert!(
        listing["workspace_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(ws))
    );
    let job_schema = serde_json::to_value(
        live_tools
            .tools
            .iter()
            .find(|tool| tool.name == "build")
            .unwrap()
            .output_schema
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        job_schema,
        cadpad_core::tools::definition("build")
            .unwrap()
            .output_schema
            .as_ref()
            .unwrap()
            .clone()
    );
    let job_validator = jsonschema::validator_for(&job_schema).unwrap();
    let invalid_job = mcp
        .peer()
        .call_tool(
            CallToolRequestParams::new("get_job").with_arguments(
                json!({"workspace_id":ws,"job_id":"missing"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(invalid_job.is_error, Some(true));
    assert!(job_validator.is_valid(invalid_job.structured_content.as_ref().unwrap()));
    let base = project["revision"].as_str().unwrap();
    let draft=sdk(mcp.peer(),"update_source",json!({"workspace_id":ws,"expected_base_revision":base,"changes":{"model.py":"import cadquery as cq\nwidth=parameter('width',50,minimum=1,maximum=100)\nshow(cq.Workplane().box(width,20,6),name='plate')"}})).await;
    let source = sdk(
        mcp.peer(),
        "read_source",
        json!({
            "workspace_id":ws,"version":draft["id"],"path":"model.py"
        }),
    )
    .await;
    for (name, result) in [("update_source", &draft), ("read_source", &source)] {
        let schema = serde_json::to_value(
            live_tools
                .tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap()
                .output_schema
                .as_ref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            &schema,
            cadpad_core::tools::definition(name)
                .unwrap()
                .output_schema
                .as_ref()
                .unwrap()
        );
        assert!(
            jsonschema::validator_for(&schema).unwrap().is_valid(result),
            "{name}: {result}"
        );
    }
    assert_eq!(source["hash"], draft["changed_files"][0]["hash"]);
    let job = sdk(
        mcp.peer(),
        "build",
        json!({"workspace_id":ws,"expected_base_revision":base,"draft_id":draft["id"]}),
    )
    .await;
    assert!(job_validator.is_valid(&job));
    let completed = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = sdk(
                mcp.peer(),
                "get_job",
                json!({"workspace_id":ws,"job_id":job["id"]}),
            )
            .await;
            assert!(job_validator.is_valid(&job));
            if job["state"] == "succeeded" {
                break job;
            }
            assert_ne!(job["state"], "failed", "{job}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let revision = completed["resulting_revision"].as_str().unwrap();
    let measured=sdk(mcp.peer(),"inspect",json!({"workspace_id":ws,"revision":revision,"entities":[{"revision":revision,"instance_id":"plate","entity_id":"part:0"}]})).await;
    assert_eq!(measured["entities"][0]["volume"], 6000.0);
    let selection = sdk(mcp.peer(), "get_selection", json!({"workspace_id":ws})).await;
    for (name, result) in [("inspect", &measured), ("get_selection", &selection)] {
        let schema = serde_json::to_value(
            live_tools
                .tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap()
                .output_schema
                .as_ref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            &schema,
            cadpad_core::tools::definition(name)
                .unwrap()
                .output_schema
                .as_ref()
                .unwrap()
        );
        assert!(
            jsonschema::validator_for(&schema).unwrap().is_valid(result),
            "{name}: {result}"
        );
    }
    let (token, mut requests) = core.attach_view();
    let view_core = core.clone();
    let revision_copy = revision.to_string();
    let renderer = tokio::spawn(async move {
        let request = requests.recv().await.unwrap();
        view_core.complete_view(&token,&request.id,cadpad_core::views::ViewReply{data:Some(json!({"image":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jA98AAAAASUVORK5CYII=","camera":{"position":[100,100,100],"target":[0,0,0],"quaternion":[0,0,0,1],"zoom":1,"orthographic":true},"revision":revision_copy,"pose_version":0,"visible_part_ids":["plate"]})),error:None}).unwrap();
    });
    let capture = mcp
        .peer()
        .call_tool(
            CallToolRequestParams::new("capture_view").with_arguments(
                json!({"workspace_id":ws,"revision":revision})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    renderer.await.unwrap();
    assert_eq!(capture.is_error, Some(false));
    let capture_schema = serde_json::to_value(
        live_tools
            .tools
            .iter()
            .find(|tool| tool.name == "capture_view")
            .unwrap()
            .output_schema
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    let capture_contract = cadpad_core::tools::definition("capture_view").unwrap();
    assert_eq!(
        &capture_schema,
        capture_contract.mcp_output_schema.as_ref().unwrap()
    );
    assert_ne!(
        &capture_schema,
        capture_contract.output_schema.as_ref().unwrap()
    );
    let metadata = capture.structured_content.as_ref().unwrap();
    assert!(
        metadata.get("image").is_none(),
        "PNG must be an MCP content block, not duplicated structured data"
    );
    assert!(
        jsonschema::validator_for(&capture_schema)
            .unwrap()
            .is_valid(metadata)
    );
    assert!(
        capture
            .content
            .iter()
            .any(|block| matches!(block, rmcp::model::ContentBlock::Image(_)))
    );
    assert_eq!(
        capture.structured_content.unwrap()["numeric_context"][0]["volume"],
        6000.0
    );
    let parameter=sdk(mcp.peer(),"set_parameter",json!({"workspace_id":ws,"expected_base_revision":revision,"parameter_id":"width","value":75,"unit":"mm"})).await;
    let refined = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = sdk(
                mcp.peer(),
                "get_job",
                json!({"workspace_id":ws,"job_id":parameter["id"]}),
            )
            .await;
            if job["state"] == "succeeded" {
                break job;
            }
            assert_ne!(job["state"], "failed", "{job}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let revision = refined["resulting_revision"].as_str().unwrap();
    let refined_project = sdk(mcp.peer(), "get_project", json!({"workspace_id":ws})).await;
    assert!(project_validator.is_valid(&refined_project));
    assert!(refined_project["model"]["parts"][0].get("mesh").is_none());
    assert_eq!(refined_project["parameters"]["width"]["value"], 75.);
    let measured=sdk(mcp.peer(),"inspect",json!({"workspace_id":ws,"revision":revision,"entities":[{"revision":revision,"instance_id":"plate","entity_id":"part:0"}]})).await;
    assert_eq!(measured["entities"][0]["bounds"]["size"][0], 75.0);
    let exported = sdk(
        mcp.peer(),
        "export",
        json!({"workspace_id":ws,"revision":revision,"format":"STEP"}),
    )
    .await;
    let export_schema = serde_json::to_value(
        live_tools
            .tools
            .iter()
            .find(|tool| tool.name == "export")
            .unwrap()
            .output_schema
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    let export_contract = cadpad_core::tools::definition("export").unwrap();
    assert_eq!(
        &export_schema,
        export_contract.mcp_output_schema.as_ref().unwrap()
    );
    assert_ne!(
        &export_schema,
        export_contract.output_schema.as_ref().unwrap()
    );
    let export_validator = jsonschema::validator_for(&export_schema).unwrap();
    assert!(export_validator.is_valid(&exported));
    let stl = sdk(
        mcp.peer(),
        "export",
        json!({"workspace_id":ws,"revision":revision,"format":"STL"}),
    )
    .await;
    assert!(export_validator.is_valid(&stl));
    assert_eq!(stl["format"], "STL");
    assert!(stl["resource_uri"].as_str().unwrap().ends_with(".stl"));
    let resource = mcp
        .peer()
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            exported["resource_uri"].as_str().unwrap(),
        ))
        .await
        .unwrap();
    use base64::Engine;
    match &resource.contents[0] {
        rmcp::model::ResourceContents::BlobResourceContents { blob, .. } => assert!(
            base64::engine::general_purpose::STANDARD
                .decode(blob)
                .unwrap()
                .starts_with(b"ISO-10303-21;")
        ),
        _ => panic!("Expected STEP binary resource"),
    }
    // Exercise >8 MiB over the actual stdio bridge. Trailing STEP comments are
    // a deterministic transport fixture, not a large-scene geometry benchmark.
    let export_path = root
        .path()
        .join("app/artifacts")
        .join(&ws)
        .join(revision)
        .join(exported["handle"].as_str().unwrap());
    let mut large = std::fs::read(&export_path).unwrap();
    while large.len() <= 9 * 1024 * 1024 {
        large.extend_from_slice(b"\n/* Cadpad bounded export transport fixture */\n");
    }
    std::fs::write(&export_path, &large).unwrap();
    let uri = exported["resource_uri"].as_str().unwrap();
    let manifest_reply = mcp
        .peer()
        .read_resource(rmcp::model::ReadResourceRequestParams::new(uri))
        .await
        .unwrap();
    let manifest: Value = match &manifest_reply.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => {
            serde_json::from_str(text).unwrap()
        }
        _ => panic!("Large exports must return a bounded download manifest"),
    };
    assert_eq!(manifest["bytes"], large.len());
    let chunk_size = manifest["chunk_size"].as_u64().unwrap() as usize;
    let mut reconstructed = Vec::new();
    for offset in (0..large.len()).step_by(chunk_size) {
        let length = chunk_size.min(large.len() - offset);
        let chunk_uri = manifest["chunk_uri_template"]
            .as_str()
            .unwrap()
            .replace("{offset}", &offset.to_string())
            .replace("{length}", &length.to_string());
        let reply = mcp
            .peer()
            .read_resource(rmcp::model::ReadResourceRequestParams::new(chunk_uri))
            .await
            .unwrap();
        match &reply.contents[0] {
            rmcp::model::ResourceContents::BlobResourceContents { blob, .. } => reconstructed
                .extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(blob)
                        .unwrap(),
                ),
            _ => panic!("A byte range must be a binary MCP resource"),
        }
    }
    assert_eq!(reconstructed, large);
    assert_eq!(
        manifest["sha256"],
        cadpad_core::portable::bytes_hash(&reconstructed)
    );
    let invalid_uri = format!(
        "{uri}?offset=0&length=1048577&content_id={}",
        manifest["content_id"].as_str().unwrap()
    );
    assert!(
        mcp.peer()
            .read_resource(rmcp::model::ReadResourceRequestParams::new(invalid_uri))
            .await
            .is_err()
    );
    std::fs::write(&export_path, b"changed after manifest").unwrap();
    let changed_uri = format!(
        "{uri}?offset=0&length=1&content_id={}",
        manifest["content_id"].as_str().unwrap()
    );
    assert!(
        mcp.peer()
            .read_resource(rmcp::model::ReadResourceRequestParams::new(changed_uri))
            .await
            .is_err()
    );
    let slow=sdk(mcp.peer(),"update_source",json!({"workspace_id":ws,"expected_base_revision":revision,"changes":{"model.py":"import time\ntime.sleep(120)"}})).await;
    let pending = sdk(
        mcp.peer(),
        "build",
        json!({"workspace_id":ws,"expected_base_revision":revision,"draft_id":slow["id"]}),
    )
    .await;
    child.kill().await.unwrap();
    let _ = child.wait().await;
    let _ = mcp.cancel().await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let job = core
                .get_job(&ws, pending["id"].as_str().unwrap())
                .await
                .unwrap();
            if job.state == cadpad_core::JobState::Cancelled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
    server.shutdown().await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn stdio_job_capacity_errors_are_structured_retryable_and_do_not_disable_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = config();
    worker.containment = cadpad_core::worker::Containment::Required;
    let core = Core::open(root.path().join("app"), worker).await.unwrap();
    let server = AppServer::start(core.clone(), root.path().join("connection"))
        .await
        .unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_cadpad-mcp"))
        .args(["--connection", server.connection_file().to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let mcp = ().serve((output, input)).await.unwrap();
    let mut pending = Vec::new();
    let source = "import sys,time\nfrom pathlib import Path\ncache=Path(sys.argv[sys.argv.index('--cache')+1])\n(cache/'entered').write_text('entered')\ntime.sleep(60)";
    let mut rejected = None;
    for index in 0..9 {
        let ws = core.create_workspace("user").await.unwrap();
        let revision = core.get_project(&ws).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let draft = sdk(mcp.peer(),"update_source",json!({"workspace_id":ws,"expected_base_revision":revision,"changes":{"model.py":source}})).await;
        if index == 8 {
            rejected = Some((ws, revision, draft));
            break;
        }
        let job = sdk(
            mcp.peer(),
            "build",
            json!({"workspace_id":ws,"expected_base_revision":revision,"draft_id":draft["id"]}),
        )
        .await;
        if index < 2 {
            // The first two sources really entered, keeping construction full.
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    if let Ok(directories) =
                        std::fs::read_dir(root.path().join("app/artifacts").join(&ws))
                        && directories
                            .filter_map(Result::ok)
                            .any(|entry| entry.path().join("entered").exists())
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        } else {
            assert_eq!(
                core.get_job(&ws, job["id"].as_str().unwrap())
                    .await
                    .unwrap()
                    .state,
                cadpad_core::JobState::Queued
            );
            assert!(
                !root
                    .path()
                    .join("app/runs")
                    .join(&ws)
                    .join(job["id"].as_str().unwrap())
                    .exists()
            );
        }
        pending.push((ws, revision, job));
    }
    let (ws, revision, draft) = rejected.unwrap();
    let persisted = root
        .path()
        .join("app/workspaces")
        .join(&ws)
        .join("workspace.json");
    let before = std::fs::read(&persisted).unwrap();
    let overload = mcp
        .peer()
        .call_tool(
            CallToolRequestParams::new("build").with_arguments(
                json!({"workspace_id":ws,"expected_base_revision":revision,"draft_id":draft["id"]})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(overload.is_error, Some(true));
    let error = overload.structured_content.unwrap();
    assert_eq!(error["code"], "RESOURCE_LIMIT");
    assert_eq!(
        error["details"],
        json!({"resource":"outstanding_jobs","scope":"actor","limit":8,"observed":8,"retryable":true})
    );
    assert!(error["message"].as_str().unwrap().contains("cancel"));
    assert!(
        jsonschema::validator_for(
            cadpad_core::tools::definition("build")
                .unwrap()
                .output_schema
                .as_ref()
                .unwrap()
        )
        .unwrap()
        .is_valid(&error)
    );
    assert_eq!(std::fs::read(&persisted).unwrap(), before);
    assert_eq!(
        sdk(mcp.peer(), "get_project", json!({"workspace_id":ws})).await["revision"],
        revision
    );

    sdk(
        mcp.peer(),
        "cancel_job",
        json!({"workspace_id":pending[0].0,"job_id":pending[0].2["id"]}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if core
                .get_job(&pending[0].0, pending[0].2["id"].as_str().unwrap())
                .await
                .unwrap()
                .state
                == cadpad_core::JobState::Cancelled
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let replacement = sdk(
        mcp.peer(),
        "build",
        json!({"workspace_id":ws,"expected_base_revision":revision,"draft_id":draft["id"]}),
    )
    .await;
    pending.push((ws, revision, replacement));
    child.kill().await.unwrap();
    let _ = child.wait().await;
    let _ = mcp.cancel().await;
    tokio::time::timeout(Duration::from_secs(15), async {
        for (ws, revision, job) in pending {
            loop {
                if core
                    .get_job(&ws, job["id"].as_str().unwrap())
                    .await
                    .unwrap()
                    .state
                    == cadpad_core::JobState::Cancelled
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
        }
    })
    .await
    .unwrap();
    server.shutdown().await;
}

async fn sdk(peer: &rmcp::service::Peer<rmcp::RoleClient>, name: &str, args: Value) -> Value {
    let result = peer
        .call_tool(
            CallToolRequestParams::new(name.to_string())
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    result.structured_content.unwrap()
}
