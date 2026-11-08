//! CLI-only check of the same resource layout used by the release desktop.
use cadpad_core::{Core, JobState, runtime::PackagedRuntime};
use cadpad_mcp::AppServer;
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ReadResourceRequestParams},
};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};

async fn call(peer: &rmcp::service::Peer<rmcp::RoleClient>, name: &str, args: Value) -> Value {
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        peer.call_tool(
            CallToolRequestParams::new(name.to_owned())
                .with_arguments(args.as_object().unwrap().clone()),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.is_error, Some(false), "{result:?}");
    result.structured_content.unwrap()
}
async fn completed(peer: &rmcp::service::Peer<rmcp::RoleClient>, ws: &str, job: &Value) -> Value {
    let job = terminal(peer, ws, job).await;
    assert_eq!(job["state"], "succeeded", "{job}");
    job
}
async fn terminal(peer: &rmcp::service::Peer<rmcp::RoleClient>, ws: &str, job: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let job = call(
                peer,
                "get_job",
                json!({"workspace_id":ws,"job_id":job["id"]}),
            )
            .await;
            if !matches!(job["state"].as_str(), Some("queued" | "running")) {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap()
}
async fn candidate(
    peer: &rmcp::service::Peer<rmcp::RoleClient>,
    ws: &str,
    revision: &str,
    source: &str,
) -> Value {
    let draft = call(
        peer,
        "update_source",
        json!({"workspace_id":ws,"expected_base_revision":revision,"changes":{"model.py":source}}),
    )
    .await;
    call(
        peer,
        "build",
        json!({"workspace_id":ws,"expected_base_revision":revision,"draft_id":draft["id"]}),
    )
    .await
}
async fn preserved(peer: &rmcp::service::Peer<rmcp::RoleClient>, ws: &str, revision: &str) {
    let project = call(peer, "get_project", json!({"workspace_id":ws})).await;
    assert_eq!(project["revision"], revision);
    assert_eq!(project["parameters"]["rail_length"]["value"], 240.0);
    let source = call(
        peer,
        "read_source",
        json!({"workspace_id":ws,"version":revision,"path":"model.py"}),
    )
    .await;
    assert_eq!(
        source["source"],
        include_str!("../../../fixtures/rail_carriage.py")
    );
    let measured = call(peer, "inspect", json!({"workspace_id":ws,"revision":revision,"entities":[{"revision":revision,"instance_id":"rail","entity_id":"part:0"}]})).await;
    assert_eq!(measured["entities"][0]["bounds"]["size"][0], 240.0);
}

// Poll a marker written by the generated source inside its allowed cache. A
// running job alone does not establish that native execution reached the loop.
async fn entered(artifacts: &std::path::Path) -> Value {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Ok(directories) = std::fs::read_dir(artifacts) {
                for directory in directories {
                    let path = directory.unwrap().path().join("packaged-entered.json");
                    if let Ok(bytes) = std::fs::read(path)
                        && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                    {
                        return value;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap()
}
async fn processes_gone(pids: &Value) {
    tokio::time::timeout(Duration::from_secs(10), async {
        for key in ["worker", "guardian"] {
            let pid = pids[key].as_u64().unwrap().to_string();
            loop {
                let status = tokio::process::Command::new("/bin/kill")
                    .args(["-0", &pid])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .unwrap();
                if !status.success() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    })
    .await
    .unwrap();
}
async fn connect(
    runtime: &PackagedRuntime,
    connection: &std::path::Path,
    temporary: &std::path::Path,
) -> Result<
    (
        tokio::process::Child,
        rmcp::service::RunningService<rmcp::RoleClient, ()>,
    ),
    Box<dyn std::error::Error>,
> {
    let mut child = tokio::process::Command::new(&runtime.bridge)
        .args([
            "--connection",
            connection.to_str().unwrap(),
            "--client-name",
            "Packaged runtime check",
        ])
        .env_clear()
        .env("PATH", "")
        .env("HOME", temporary)
        .env("TMPDIR", temporary)
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let client = tokio::time::timeout(Duration::from_secs(10), ().serve((output, input))).await??;
    Ok((child, client))
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        std::env::var("PATH").as_deref(),
        Ok(""),
        "Run this packaging probe with an empty PATH"
    );
    let resources = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("Resources directory is required")?,
    );
    let runtime = PackagedRuntime::open(&resources)?;
    let temporary = tempfile::tempdir()?;
    let core = Core::open(temporary.path().join("app"), runtime.worker_config()).await?;
    let ws = core.create_workspace("packaging-check").await?;
    let server = AppServer::start(core.clone(), temporary.path().join("mcp")).await?;
    let (mut child, client) = connect(&runtime, server.connection_file(), temporary.path()).await?;
    let tools = client.peer().list_tools(None).await?;
    assert!(tools.tools.iter().any(|tool| tool.name == "build"));
    let project = call(client.peer(), "get_project", json!({"workspace_id":ws})).await;
    let base = project["revision"].as_str().unwrap();
    let draft=call(client.peer(),"update_source",json!({"workspace_id":ws,"expected_base_revision":base,"changes":{"model.py":include_str!("../../../fixtures/rail_carriage.py")}})).await;
    let job = call(
        client.peer(),
        "build",
        json!({"workspace_id":ws,"expected_base_revision":base,"draft_id":draft["id"]}),
    )
    .await;
    let job = completed(client.peer(), &ws, &job).await;
    let revision = job["resulting_revision"].as_str().unwrap();
    let measured=call(client.peer(),"inspect",json!({"workspace_id":ws,"revision":revision,"entities":[{"revision":revision,"instance_id":"rail","entity_id":"part:0"}]})).await;
    assert_eq!(measured["entities"][0]["bounds"]["size"][0], 200.0);
    let project = call(client.peer(), "get_project", json!({"workspace_id":ws})).await;
    assert_eq!(project["model"]["parts"].as_array().unwrap().len(), 2);
    assert!(
        project["model"]["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == "mounting_spacing" && a["value"] == 120.0)
    );
    let parameter=call(client.peer(),"set_parameter",json!({"workspace_id":ws,"expected_base_revision":revision,"parameter_id":"rail_length","value":240.0,"unit":"mm"})).await;
    let refined = completed(client.peer(), &ws, &parameter).await;
    let revision = refined["resulting_revision"].as_str().unwrap();
    let measured=call(client.peer(),"inspect",json!({"workspace_id":ws,"revision":revision,"entities":[{"revision":revision,"instance_id":"rail","entity_id":"part:0"}]})).await;
    assert_eq!(measured["entities"][0]["bounds"]["size"][0], 240.0);
    let exported = call(
        client.peer(),
        "export",
        json!({"workspace_id":ws,"revision":revision,"format":"STEP"}),
    )
    .await;
    let resource = client
        .peer()
        .read_resource(ReadResourceRequestParams::new(
            exported["resource_uri"].as_str().unwrap(),
        ))
        .await?;
    use base64::Engine;
    let bytes = match &resource.contents[0] {
        rmcp::model::ResourceContents::BlobResourceContents { blob, .. } => {
            base64::engine::general_purpose::STANDARD.decode(blob)?
        }
        _ => panic!("Expected small STEP resource"),
    };
    assert!(bytes.starts_with(b"ISO-10303-21;"));
    let mut failures = Vec::new();
    for (name, expected_code, source) in [
        (
            "execution_error",
            "BUILD_FAILED",
            "raise RuntimeError('intentional packaged candidate failure')",
        ),
        (
            "invalid_intended_solid",
            "INVALID_GEOMETRY",
            "import cadquery as cq\nshow(cq.Workplane().box(10,10,10).faces('>Z'), name='invalid', kind='solid')",
        ),
        (
            "native_exit",
            "WORKER_UNAVAILABLE",
            "import os\nos._exit(23)",
        ),
    ] {
        let job = candidate(client.peer(), &ws, revision, source).await;
        let failed = terminal(client.peer(), &ws, &job).await;
        assert_eq!(failed["state"], "failed", "{failed}");
        assert_eq!(failed["diagnostics"]["code"], expected_code, "{failed}");
        assert!(failed["resulting_revision"].is_null());
        preserved(client.peer(), &ws, revision).await;
        failures.push(json!({"case":name,"state":failed["state"],"diagnostics":failed["diagnostics"],"committed_revision_preserved":true,"exact_rail_length_mm":240}));
    }
    let stuck = candidate(client.peer(), &ws, revision, r#"
import json, os, sys
from pathlib import Path
cache = Path(sys.argv[sys.argv.index('--cache') + 1])
(cache / 'packaged-entered.json').write_text(json.dumps(dict(worker=os.getpid(), guardian=os.getppid())))
while True: pass
"#).await;
    let pids = entered(&temporary.path().join("app/artifacts").join(&ws)).await;
    call(
        client.peer(),
        "cancel_job",
        json!({"workspace_id":ws,"job_id":stuck["id"]}),
    )
    .await;
    let cancelled = terminal(client.peer(), &ws, &stuck).await;
    assert_eq!(cancelled["state"], "cancelled", "{cancelled}");
    processes_gone(&pids).await;
    preserved(client.peer(), &ws, revision).await;
    failures.push(json!({"case":"cancel_entered_native_loop","state":"cancelled","native_execution_entered":true,"worker_and_guardian_gone":true,"committed_revision_preserved":true,"exact_rail_length_mm":240}));
    let recovery = candidate(
        client.peer(),
        &ws,
        revision,
        include_str!("../../../fixtures/rail_carriage.py"),
    )
    .await;
    let recovered = completed(client.peer(), &ws, &recovery).await;
    let recovered_revision = recovered["resulting_revision"].as_str().unwrap();
    assert_ne!(recovered_revision, revision);
    preserved(client.peer(), &ws, recovered_revision).await;
    let mut queued = Vec::new();
    let mut active_pids = Vec::new();
    let source = "import json,os,sys,time\nfrom pathlib import Path\ncache=Path(sys.argv[sys.argv.index('--cache')+1])\n(cache/'packaged-entered.json').write_text(json.dumps(dict(worker=os.getpid(),guardian=os.getppid())))\ntime.sleep(60)";
    for index in 0..8 {
        let workspace = core.create_workspace("packaged-capacity-check").await?;
        let base = core.get_project(&workspace).await?["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let job = candidate(client.peer(), &workspace, &base, source).await;
        if index < 2 {
            active_pids
                .push(entered(&temporary.path().join("app/artifacts").join(&workspace)).await);
        } else {
            assert_eq!(
                core.get_job(&workspace, job["id"].as_str().unwrap())
                    .await?
                    .state,
                JobState::Queued
            );
            assert!(
                !temporary
                    .path()
                    .join("app/runs")
                    .join(&workspace)
                    .join(job["id"].as_str().unwrap())
                    .exists()
            );
        }
        queued.push((workspace, base, job));
    }
    let rejected_ws = core.create_workspace("packaged-capacity-check").await?;
    let rejected_base = core.get_project(&rejected_ws).await?["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let rejected_draft = call(client.peer(),"update_source",json!({"workspace_id":rejected_ws,"expected_base_revision":rejected_base,"changes":{"model.py":source}})).await;
    let persisted = temporary
        .path()
        .join("app/workspaces")
        .join(&rejected_ws)
        .join("workspace.json");
    let before = std::fs::read(&persisted)?;
    let overload = client.peer().call_tool(CallToolRequestParams::new("build").with_arguments(json!({"workspace_id":rejected_ws,"expected_base_revision":rejected_base,"draft_id":rejected_draft["id"]}).as_object().unwrap().clone())).await?;
    assert_eq!(overload.is_error, Some(true));
    let overload = overload.structured_content.unwrap();
    assert_eq!(overload["code"], "RESOURCE_LIMIT");
    assert_eq!(
        overload["details"],
        json!({"resource":"outstanding_jobs","scope":"actor","limit":8,"observed":8,"retryable":true})
    );
    let schema = serde_json::to_value(
        tools
            .tools
            .iter()
            .find(|tool| tool.name == "build")
            .unwrap()
            .output_schema
            .as_ref()
            .unwrap(),
    )?;
    assert!(jsonschema::validator_for(&schema)?.is_valid(&overload));
    assert_eq!(std::fs::read(&persisted)?, before);
    preserved(client.peer(), &ws, recovered_revision).await;
    child.kill().await?;
    tokio::time::timeout(Duration::from_secs(10), child.wait()).await??;
    let _ = client.cancel().await;
    tokio::time::timeout(Duration::from_secs(15), async {
        for (workspace, base, job) in &queued {
            loop {
                let job = core
                    .get_job(workspace, job["id"].as_str().unwrap())
                    .await
                    .unwrap();
                if job.state == JobState::Cancelled {
                    break;
                }
                assert!(
                    matches!(job.state, JobState::Queued | JobState::Running),
                    "{}",
                    job.diagnostics
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(
                core.get_project(workspace).await.unwrap()["revision"],
                base.as_str()
            );
        }
    })
    .await?;
    for pids in &active_pids {
        processes_gone(pids).await;
    }
    let (mut child, client) = connect(&runtime, server.connection_file(), temporary.path()).await?;
    preserved(client.peer(), &ws, recovered_revision).await;
    let after_disconnect = candidate(
        client.peer(),
        &ws,
        recovered_revision,
        include_str!("../../../fixtures/rail_carriage.py"),
    )
    .await;
    let after_disconnect = completed(client.peer(), &ws, &after_disconnect).await;
    assert_ne!(after_disconnect["actor"], recovered["actor"]);
    preserved(
        client.peer(),
        &ws,
        after_disconnect["resulting_revision"].as_str().unwrap(),
    )
    .await;
    let job_admission = json!({"passed":true,"entered_workers":2,"queued_without_materialization":6,"accepted_before_rejection":8,"rejection":overload,"rejected_workspace_unchanged":true,"committed_inspection_while_full":true,"abrupt_bridge_exit_cancels_all_accepted_jobs":true,"entered_workers_and_guardians_gone":true,"reconnected_actor_build_succeeds":true});
    let _ = client.cancel().await;
    tokio::time::timeout(Duration::from_secs(10), child.wait()).await??;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"status":"passed","resources":resources.canonicalize()?,"worker":runtime.worker,"guardian":runtime.guardian,"bridge":runtime.bridge,"contained_bundled_worker":true,"real_stdio_bridge":true,"parts":2,"initial_rail_length_mm":200,"refined_rail_length_mm":240,"mounting_spacing_mm":120,"step_export_bytes":bytes.len(),"failure_recovery":failures,"successful_rebuild_after_failures":true,"job_admission":job_admission,"empty_path":true,"ui_launched":false})
        )?
    );
    server.shutdown().await;
    Ok(())
}
