use cadpad_agent::cli::{Backend, StreamEvent, StreamParser, tool_name};
use serde_json::json;

#[test]
fn backend_labels_round_trip() {
    for backend in [
        Backend::AnthropicApi,
        Backend::OpenRouterApi,
        Backend::ClaudeCli,
        Backend::CodexCli,
        Backend::CursorCli,
        Backend::OpenCodeCli,
        Backend::AntigravityCli,
    ] {
        assert_eq!(Backend::parse(backend.as_str()).unwrap(), backend);
    }
    assert!(Backend::parse("nope").is_err());
    assert_eq!(
        Backend::parse("openrouter").unwrap(),
        Backend::OpenRouterApi
    );
    assert_eq!(Backend::parse("agy").unwrap(), Backend::AntigravityCli);
    assert!(!Backend::OpenRouterApi.is_cli());
    assert_eq!(
        serde_json::to_value(Backend::OpenRouterApi).unwrap(),
        "openrouter_api"
    );
    assert_eq!(
        serde_json::to_value(Backend::AntigravityCli).unwrap(),
        "antigravity_cli"
    );
}

fn run(backend: Backend, lines: &[serde_json::Value]) -> Vec<StreamEvent> {
    let mut parser = StreamParser::new(backend);
    lines
        .iter()
        .flat_map(|line| parser.parse(&line.to_string()))
        .collect()
}

fn text(events: &[StreamEvent]) -> String {
    let mut out = String::new();
    for event in events {
        match event {
            StreamEvent::Text(delta) => out.push_str(delta),
            StreamEvent::TextBreak => out.push('|'),
            _ => {}
        }
    }
    out
}

/// Shapes recorded from `agent -p … --output-format stream-json --stream-partial-output`.
#[test]
fn cursor_keeps_partial_deltas_once_and_reports_thinking_and_mcp_tools() {
    let tool = |subtype: &str, result: Option<serde_json::Value>| {
        let mut call = json!({"args":{"name":"cadpad-get_project","args":{},"toolName":"get_project","providerIdentifier":"cadpad"},
            "description":"List all cadpad workspaces"});
        if let Some(result) = result {
            call["result"] = result;
        }
        json!({"type":"tool_call","subtype":subtype,"call_id":"call-1","tool_call":{"mcpToolCall":call},"timestamp_ms":2})
    };
    let delta = |text: &str| json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":text}]},"timestamp_ms":1});
    let full = |text: &str| json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":text}]}});
    let events = run(
        Backend::CursorCli,
        &[
            json!({"type":"thinking","subtype":"delta","text":"Finding","timestamp_ms":1}),
            json!({"type":"thinking","subtype":"delta","text":" the cube","timestamp_ms":1}),
            json!({"type":"thinking","subtype":"completed","timestamp_ms":1}),
            delta("I'll pull"),
            delta(" the project."),
            // Recorded: a repeat before tool calls keeps timestamp_ms and adds model_call_id.
            json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I'll pull the project."}]},"model_call_id":"m","timestamp_ms":3}),
            tool("started", None),
            tool("completed", Some(json!({"success":{"content":[]}}))),
            delta("Removed the cube."),
            full("Removed the cube."),
            json!({"type":"result","subtype":"success","result":"I'll pull the project.Removed the cube."}),
        ],
    );
    assert_eq!(text(&events), "I'll pull the project.|Removed the cube.|");
    assert!(events.contains(&StreamEvent::Thinking("Finding".into())));
    assert!(events.contains(&StreamEvent::ThinkingDone));
    assert!(events.contains(&StreamEvent::ToolStarted {
        id: Some("call-1".into()),
        name: "get_project".into(),
        label: None,
        detail: Some("List all cadpad workspaces".into()),
    }));
    assert!(events.contains(&StreamEvent::ToolFinished {
        id: Some("call-1".into()),
        name: "get_project".into(),
        error: false,
    }));
    assert_eq!(events.last(), Some(&StreamEvent::Done));
}

#[test]
fn claude_partial_messages_are_not_repeated_and_tool_results_keep_names() {
    let block = |kind: &str, index: u64| json!({"type":"stream_event","event":{"type":"content_block_start","index":index,"content_block":{"type":kind}}});
    let stop = |index: u64| json!({"type":"stream_event","event":{"type":"content_block_stop","index":index}});
    let events = run(
        Backend::ClaudeCli,
        &[
            block("thinking", 0),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Plan"}}}),
            stop(0),
            block("text", 1),
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello"}}}),
            stop(1),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"Hello"},
                {"type":"tool_use","id":"tu_1","name":"mcp__cadpad__build","input":{}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_1","is_error":true}]}}),
            json!({"type":"result"}),
        ],
    );
    assert_eq!(text(&events), "Hello|");
    assert!(events.contains(&StreamEvent::Thinking("Plan".into())));
    assert!(events.contains(&StreamEvent::ToolStarted {
        id: Some("tu_1".into()),
        name: "build".into(),
        label: None,
        detail: None
    }));
    assert!(events.contains(&StreamEvent::ToolFinished {
        id: Some("tu_1".into()),
        name: "build".into(),
        error: true
    }));
}

#[test]
fn antigravity_models_keep_ids_and_labels_and_drop_status_lines() {
    use cadpad_agent::cli::parse_antigravity_models;
    let models = parse_antigravity_models(
        "Fetching available models...\ngemini-4-hydrogen-medium\tGemini 4 Hydrogen (Medium)\nclaude-opus-5-5-low\tClaude Opus 5.5 (Low)\ngemini-4-hydrogen-medium\tDuplicate\n--bad\tIgnored\nbad model\tIgnored\n",
    );
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gemini-4-hydrogen-medium", "claude-opus-5-5-low"]
    );
    assert_eq!(models[0].label, "Gemini 4 Hydrogen (Medium)");
}

#[test]
fn antigravity_streams_text_and_resolves_generic_mcp_tool_calls() {
    let parameters = json!({
        "ServerName": "cadpad",
        "ToolName": "control_requirements",
        "Arguments": {
            "command": {
                "action": "save",
                "ir": {"name": "Frame attachment", "claims": [{}, {}]}
            }
        }
    });
    let events = run(
        Backend::AntigravityCli,
        &[
            json!({"event":"init","init":{"cwd":"/tmp"}}),
            json!({"event":"step_update","step_update":{"step_index":0,"state":"DONE","step_type":"user_input"}}),
            // The agent first reads the MCP tool schema from its cache.
            json!({"event":"step_update","step_update":{"step_index":1,"state":"DONE","step_type":"tool","tool_name":"read_file","tool_info":{"name":"read_file","parameters":{"Path":"/Users/test/.gemini/antigravity-cli/mcp/cadpad/control_requirements.json"}}}}),
            json!({"event":"step_update","step_update":{"step_index":2,"state":"ACTIVE","step_type":"tool","tool_name":"call_mcp_tool","tool_info":{"name":"call_mcp_tool","parameters":parameters}}}),
            // Completion does not need to repeat the call parameters.
            json!({"event":"step_update","step_update":{"step_index":2,"state":"DONE","step_type":"tool","tool_name":"call_mcp_tool","tool_info":{"name":"call_mcp_tool","output":"saved"}}}),
            json!({"event":"step_update","step_update":{"step_index":3,"state":"ACTIVE","step_type":"agent_response","text_delta":"Saved "}}),
            json!({"event":"step_update","step_update":{"step_index":3,"state":"DONE","step_type":"agent_response","text_delta":"it.\n"}}),
            json!({"event":"result","result":{"status":"SUCCESS","response":"Saved it.\n"}}),
        ],
    );
    assert_eq!(text(&events), "Saved it.\n|");
    assert!(events.contains(&StreamEvent::ToolStarted {
        id: Some("1".into()),
        name: "tool_schema".into(),
        label: None,
        detail: Some("control_requirements".into()),
    }));
    assert!(events.contains(&StreamEvent::ToolStarted {
        id: Some("2".into()),
        name: "control_requirements".into(),
        label: Some("Saving a requirement draft".into()),
        detail: Some("Frame attachment · 2 claims".into()),
    }));
    assert!(events.contains(&StreamEvent::ToolFinished {
        id: Some("2".into()),
        name: "control_requirements".into(),
        error: false,
    }));
    assert_eq!(events.last(), Some(&StreamEvent::Done));
}

#[cfg(unix)]
#[tokio::test]
async fn antigravity_error_results_fail_even_with_a_successful_exit() {
    let (_directory, launch) = stalled_cli(
        r#"printf '%s\n' '{"event":"result","result":{"status":"ERROR","error":"Model unavailable"}}'"#,
    )
    .await;
    let path = launch.work_dir.clone();
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    let error = cadpad_agent::cli::drive(launch, Backend::AntigravityCli, receiver, |_| {})
        .await
        .unwrap_err();
    assert_eq!(error.code(), "LLM_UNAVAILABLE");
    assert!(error.to_string().contains("Model unavailable"));
    assert!(!path.exists());
}

#[test]
fn codex_items_map_to_text_reasoning_and_tools() {
    let events = run(
        Backend::CodexCli,
        &[
            json!({"type":"item.completed","item":{"id":"r","type":"reasoning","text":"Checking"}}),
            json!({"type":"item.started","item":{"id":"m","type":"mcp_tool_call","server":"cadpad","tool":"inspect","status":"in_progress"}}),
            json!({"type":"item.completed","item":{"id":"m","type":"mcp_tool_call","server":"cadpad","tool":"inspect","status":"failed"}}),
            json!({"type":"item.completed","item":{"id":"a","type":"agent_message","text":"Done."}}),
            json!({"type":"turn.completed"}),
        ],
    );
    assert_eq!(text(&events), "Done.|");
    assert!(events.contains(&StreamEvent::Thinking("Checking".into())));
    assert!(events.contains(&StreamEvent::ToolFinished {
        id: Some("m".into()),
        name: "inspect".into(),
        error: true
    }));
}

#[test]
fn codex_requirement_calls_show_the_action_and_draft_name() {
    let events = run(
        Backend::CodexCli,
        &[json!({
            "type": "item.started",
            "item": {
                "id": "m",
                "type": "mcp_tool_call",
                "server": "cadpad",
                "tool": "control_requirements",
                "arguments": {
                    "command": {
                        "action": "save",
                        "ir": {"name": "Frame attachment", "claims": [{}, {}]}
                    }
                },
                "status": "in_progress"
            }
        })],
    );
    assert!(events.contains(&StreamEvent::ToolStarted {
        id: Some("m".into()),
        name: "control_requirements".into(),
        label: Some("Saving a requirement draft".into()),
        detail: Some("Frame attachment · 2 claims".into()),
    }));
}

#[test]
fn tool_names_drop_cadpad_prefixes() {
    assert_eq!(tool_name("mcp__cadpad__build"), "build");
    assert_eq!(tool_name("cadpad-get_project"), "get_project");
    assert_eq!(tool_name("inspect"), "inspect");
    assert_eq!(tool_name("cadpad_get_project"), "get_project");
}

#[test]
fn opencode_catalog_keeps_provider_ids_nested_models_and_variants() {
    use cadpad_agent::cli::{parse_opencode_models, selected_model};
    let models = parse_opencode_models(
        "Available models\nopenai/gpt-test\nopenrouter/~openai/gpt-test#high\nopenai/gpt-test\n--flag/model\nprovider/\n/model\nprovider/bad model\n",
    );
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["openai/gpt-test", "openrouter/~openai/gpt-test#high"]
    );
    assert_eq!(
        selected_model("openai/gpt-test#high"),
        "openai/gpt-test#high"
    );
    assert_eq!(Backend::parse("opencode").unwrap(), Backend::OpenCodeCli);
    assert_eq!(
        serde_json::to_value(Backend::OpenCodeCli).unwrap(),
        "opencode_cli"
    );
}

#[test]
fn opencode_steps_do_not_end_the_turn_and_completed_tools_are_recorded() {
    let events = run(
        Backend::OpenCodeCli,
        &[
            json!({"type":"reasoning","part":{"text":"Checking geometry"}}),
            json!({"type":"tool_use","part":{"id":"call1","tool":"cadpad_get_project","state":{"status":"completed"}}}),
            json!({"type":"step_finish","part":{"reason":"tool-calls"}}),
            json!({"type":"tool_use","part":{"callID":"call2","tool":"cadpad_build","state":{"status":"error"}}}),
            json!({"type":"text","part":{"text":"Build needs repair."}}),
            json!({"type":"step_finish","part":{"reason":"stop"}}),
        ],
    );
    assert_eq!(text(&events), "Build needs repair.|");
    assert!(events.contains(&StreamEvent::Thinking("Checking geometry".into())));
    assert!(events.contains(&StreamEvent::ToolStarted {
        id: Some("call1".into()),
        name: "get_project".into(),
        label: None,
        detail: None
    }));
    assert!(events.contains(&StreamEvent::ToolFinished {
        id: Some("call2".into()),
        name: "build".into(),
        error: true
    }));
    assert!(!events.contains(&StreamEvent::Done));
}

#[cfg(unix)]
#[tokio::test]
async fn opencode_protocol_errors_fail_even_with_a_successful_exit() {
    let (_directory, launch) = stalled_cli(r#"printf '%s\n' '{"type":"error","error":{"type":"unknown","message":"Model unavailable"}}'"#).await;
    let path = launch.work_dir.clone();
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    let error = cadpad_agent::cli::drive(launch, Backend::OpenCodeCli, receiver, |_| {})
        .await
        .unwrap_err();
    assert_eq!(error.code(), "LLM_UNAVAILABLE");
    assert!(error.to_string().contains("Model unavailable"));
    assert!(!path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn opencode_can_be_cancelled_between_model_steps() {
    use std::time::Duration;
    let (_directory, launch) = stalled_cli(r#"printf '%s\n' '{"type":"step_finish","part":{"reason":"tool-calls"}}'; exec /bin/sleep 30"#).await;
    let path = launch.work_dir.clone();
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.send_replace(true);
    });
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        cadpad_agent::cli::drive(launch, Backend::OpenCodeCli, receiver, |_| {}),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code(), "CANCELLED");
    assert!(!path.exists());
}

// Isolate PATH in a subprocess so tests never launch a real model or change
// environment variables shared by concurrent Rust tests.
#[cfg(unix)]
#[test]
fn opencode_launch_connects_mcp_and_passes_default_or_explicit_models() {
    use std::os::unix::fs::PermissionsExt;
    const FLAG: &str = "CADPAD_OPENCODE_LAUNCH_TEST";
    if std::env::var_os(FLAG).is_none() {
        let directory = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/usr/bin/which", directory.path().join("which")).unwrap();
        let binary = directory.path().join("opencode");
        std::fs::write(&binary, r#"#!/bin/sh
case "$1" in
  --version) echo 'opencode v2.2.6'; exit 0;;
  models) echo 'test/model'; exit 0;;
esac
printf '%s' "$OPENCODE_CONFIG_CONTENT" > "$CADPAD_OPENCODE_CONFIG_LOG"
printf '%s\000' "$@" > "$CADPAD_OPENCODE_ARGS_LOG"
printf '%s\n' '{"type":"text","part":{"text":"Project checked."}}' '{"type":"step_finish","part":{"reason":"stop"}}'
"#).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "opencode_launch_connects_mcp_and_passes_default_or_explicit_models",
            ])
            .env(FLAG, "1")
            .env("PATH", directory.path())
            .env(
                "CADPAD_OPENCODE_CONFIG_LOG",
                directory.path().join("config.json"),
            )
            .env("CADPAD_OPENCODE_ARGS_LOG", directory.path().join("args"))
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        use cadpad_agent::cli::{Bridge, LaunchRequest, drive, spawn};
        let bridge = Bridge {
            mcp_executable: "/usr/bin/true".into(),
            connection_file: "/tmp/connection with spaces.json".into(),
        };
        for model in ["", "test/model#high"] {
            let launch = spawn(LaunchRequest {
                backend: Backend::OpenCodeCli,
                bridge: &bridge,
                workspace_id: "workspace-test",
                project_directory: None,
                prompt: "--a prompt\nwith a newline",
                client_name: "Cadpad test",
                model,
            })
            .await
            .unwrap();
            let path = launch.work_dir.clone();
            let (_cancel, receiver) = tokio::sync::watch::channel(false);
            let mut events = vec![];
            drive(launch, Backend::OpenCodeCli, receiver, |event| {
                events.push(event)
            })
            .await
            .unwrap();
            assert_eq!(text(&events), "Project checked.|");
            assert!(!path.exists());
            let config: serde_json::Value = serde_json::from_slice(
                &std::fs::read(std::env::var_os("CADPAD_OPENCODE_CONFIG_LOG").unwrap()).unwrap(),
            )
            .unwrap();
            assert_eq!(
                config["mcp"]["servers"]["cadpad"]["command"],
                json!([
                    "/usr/bin/true",
                    "--connection",
                    "/tmp/connection with spaces.json",
                    "--client-name",
                    "Cadpad test"
                ])
            );
            assert_eq!(config["mcp"]["servers"]["cadpad"]["codemode"], false);
            assert!(
                config["agents"]["cadpad"]["system"]
                    .as_str()
                    .unwrap()
                    .contains("workspace-test")
            );
            assert_eq!(
                config["agents"]["cadpad"]["permissions"],
                json!([
                    {"action":"*","resource":"*","effect":"deny"},
                    {"action":"cadpad_*","resource":"*","effect":"allow"}
                ])
            );
            let args =
                std::fs::read(std::env::var_os("CADPAD_OPENCODE_ARGS_LOG").unwrap()).unwrap();
            let args: Vec<_> = args
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| std::str::from_utf8(arg).unwrap())
                .collect();
            assert_eq!(
                &args[..8],
                [
                    "run",
                    "--standalone",
                    "--format",
                    "json",
                    "--thinking",
                    "--agent",
                    "cadpad",
                    if model.is_empty() { "--" } else { "--model" }
                ]
            );
            assert_eq!(args.last(), Some(&"--a prompt\nwith a newline"));
            assert_eq!(args.contains(&"--model"), !model.is_empty());
            if !model.is_empty() {
                assert!(args.contains(&model));
            }
        }
    });
}

// Isolate PATH in a subprocess so tests never launch a real model or change
// environment variables shared by concurrent Rust tests.
#[cfg(unix)]
#[test]
fn antigravity_launch_uses_a_private_workspace_mcp_config() {
    use std::os::unix::fs::PermissionsExt;
    const FLAG: &str = "CADPAD_ANTIGRAVITY_LAUNCH_TEST";
    if std::env::var_os(FLAG).is_none() {
        let directory = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/usr/bin/which", directory.path().join("which")).unwrap();
        let binary = directory.path().join("agy");
        std::fs::write(
            &binary,
            r#"#!/bin/sh
case "$1" in
  models) printf 'test-model\tTest Model\n'; exit 0;;
esac
printf '%s\000' "$@" > "$CADPAD_ANTIGRAVITY_ARGS_LOG"
/bin/cp .agents/mcp_config.json "$CADPAD_ANTIGRAVITY_MCP_LOG"
/bin/cp AGENTS.md "$CADPAD_ANTIGRAVITY_SYSTEM_LOG"
printf '%s\n' \
  '{"event":"step_update","step_update":{"step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"Project "}}' \
  '{"event":"step_update","step_update":{"step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"checked.\n"}}' \
  '{"event":"result","result":{"status":"SUCCESS","response":"Project checked.\n"}}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "antigravity_launch_uses_a_private_workspace_mcp_config",
            ])
            .env(FLAG, "1")
            .env("PATH", directory.path())
            .env("CADPAD_ANTIGRAVITY_ARGS_LOG", directory.path().join("args"))
            .env(
                "CADPAD_ANTIGRAVITY_MCP_LOG",
                directory.path().join("mcp.json"),
            )
            .env(
                "CADPAD_ANTIGRAVITY_SYSTEM_LOG",
                directory.path().join("AGENTS.md"),
            )
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        use cadpad_agent::cli::{Bridge, LaunchRequest, drive, spawn};
        let bridge = Bridge {
            mcp_executable: "/usr/bin/true".into(),
            connection_file: "/tmp/connection with spaces.json".into(),
        };
        for model in ["", "test-model"] {
            let launch = spawn(LaunchRequest {
                backend: Backend::AntigravityCli,
                bridge: &bridge,
                workspace_id: "workspace-test",
                project_directory: Some(std::path::Path::new("/tmp/cadpad project")),
                prompt: "--a prompt\nwith a newline",
                client_name: "Cadpad test",
                model,
            })
            .await
            .unwrap();
            let path = launch.work_dir.clone();
            let (_cancel, receiver) = tokio::sync::watch::channel(false);
            let mut events = vec![];
            drive(launch, Backend::AntigravityCli, receiver, |event| {
                events.push(event)
            })
            .await
            .unwrap();
            assert_eq!(text(&events), "Project checked.\n|");
            assert_eq!(events.last(), Some(&StreamEvent::Done));
            assert!(!path.exists());
            let config: serde_json::Value = serde_json::from_slice(
                &std::fs::read(std::env::var_os("CADPAD_ANTIGRAVITY_MCP_LOG").unwrap()).unwrap(),
            )
            .unwrap();
            assert_eq!(
                config["mcpServers"]["cadpad"]["command"],
                json!("/usr/bin/true")
            );
            assert_eq!(
                config["mcpServers"]["cadpad"]["args"],
                json!([
                    "--connection",
                    "/tmp/connection with spaces.json",
                    "--client-name",
                    "Cadpad test"
                ])
            );
            let system =
                std::fs::read_to_string(std::env::var_os("CADPAD_ANTIGRAVITY_SYSTEM_LOG").unwrap())
                    .unwrap();
            assert!(system.contains("workspace-test"));
            assert!(system.contains("/tmp/cadpad project"));
            assert!(system.contains("computer-use/window-automation"));
            let args =
                std::fs::read(std::env::var_os("CADPAD_ANTIGRAVITY_ARGS_LOG").unwrap()).unwrap();
            let args: Vec<_> = args
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| std::str::from_utf8(arg).unwrap())
                .collect();
            assert_eq!(
                &args[..6],
                [
                    "-p",
                    "--a prompt\nwith a newline",
                    "--output-format",
                    "stream-json",
                    "--dangerously-skip-permissions",
                    "--settings"
                ]
            );
            let settings: serde_json::Value = serde_json::from_str(args[6]).unwrap();
            assert_eq!(
                settings["trustedWorkspaces"],
                json!([path.display().to_string()])
            );
            if model.is_empty() {
                assert_eq!(args.len(), 7);
            } else {
                assert_eq!(&args[7..], ["--model", model]);
            }
        }
    });
}

#[tokio::test]
async fn detects_local_cli_binaries_without_panicking() {
    let detections = cadpad_agent::cli::detect_all().await;
    assert_eq!(detections.len(), 5);
    for item in detections {
        assert!(item.backend.is_cli());
        assert!(!item.detail.is_empty());
    }
}

#[cfg(unix)]
async fn stalled_cli(script: &str) -> (tempfile::TempDir, cadpad_agent::cli::Launch) {
    use std::process::Stdio;
    let directory = tempfile::tempdir().unwrap();
    let work_dir = directory.path().join("cli");
    std::fs::create_dir(&work_dir).unwrap();
    let child = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    (directory, cadpad_agent::cli::Launch { child, work_dir })
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_cli_result_completes_even_when_process_and_stdout_stay_open() {
    use std::time::Duration;
    let (_directory, launch) =
        stalled_cli(r#"printf '%s\n' '{"type":"result","subtype":"success"}'; exec /bin/sleep 30"#)
            .await;
    let path = launch.work_dir.clone();
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    let mut events = vec![];
    tokio::time::timeout(
        Duration::from_secs(4),
        cadpad_agent::cli::drive(launch, Backend::CursorCli, receiver, |event| {
            events.push(event)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(events.last(), Some(&StreamEvent::Done));
    assert!(!path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn closed_stdout_with_lingering_process_obeys_cancellation_and_cleans_up() {
    use std::time::Duration;
    let (_directory, launch) = stalled_cli("exec 1>&-; exec /bin/sleep 30").await;
    let path = launch.work_dir.clone();
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.send_replace(true);
    });
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        cadpad_agent::cli::drive(launch, Backend::CursorCli, receiver, |_| {}),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code(), "CANCELLED");
    assert!(!path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn closed_stdout_without_a_terminal_result_has_a_bounded_exit_wait() {
    use std::time::Duration;
    let (_directory, launch) = stalled_cli("exec 1>&-; exec /bin/sleep 30").await;
    let path = launch.work_dir.clone();
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    let error = tokio::time::timeout(
        Duration::from_secs(4),
        cadpad_agent::cli::drive(launch, Backend::CursorCli, receiver, |_| {}),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code(), "LLM_UNAVAILABLE");
    assert!(!path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn cli_stderr_is_drained_so_verbose_output_cannot_block_completion() {
    use std::time::Duration;
    let (_directory,launch)=stalled_cli(r#"/bin/dd if=/dev/zero bs=65536 count=16 >&2 2>/dev/null; printf '%s\n' '{"type":"result","subtype":"success"}'"#).await;
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    let mut done = false;
    tokio::time::timeout(
        Duration::from_secs(3),
        cadpad_agent::cli::drive(launch, Backend::CursorCli, receiver, |event| {
            if event == StreamEvent::Done {
                done = true;
            }
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(done);
}

#[test]
fn model_catalogs_keep_selectable_ids_and_drop_hidden_or_unsafe_values() {
    use cadpad_agent::cli::{
        parse_anthropic_models, parse_claude_model_help, parse_codex_models, parse_cursor_models,
        parse_openrouter_models, selected_model,
    };
    let cursor = parse_cursor_models(
        "Available models\n\nauto - Auto (default)\ngpt-5.6-sol-high - GPT-5.6 Sol High\n--bad - Ignored\n",
    );
    assert_eq!(
        cursor
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["auto", "gpt-5.6-sol-high"]
    );
    let codex = parse_codex_models(&json!({"models":[
        {"slug":"gpt-6-sol","display_name":"GPT-6 Sol","visibility":"list","priority":4},
        {"slug":"gpt-6.1-sol","display_name":"GPT-6.1-Sol","visibility":"list","priority":1},
        {"slug":"hidden-model","display_name":"Hidden","visibility":"hide","priority":0},
        {"slug":"-flag","display_name":"Unsafe","visibility":"list","priority":2}
    ]}));
    assert_eq!(
        codex
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gpt-6.1-sol", "gpt-6-sol"]
    );
    let claude = parse_claude_model_help(
        "  --model <model>  alias (e.g. 'fable', 'opus', or 'sonnet')\n  -n, --name <name>  'ignored'\n",
    );
    assert_eq!(
        claude
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["fable", "opus", "sonnet"]
    );
    let anthropic = parse_anthropic_models(
        &json!({"data":[{"id":"claude-sonnet","display_name":"Claude Sonnet"},{"id":"claude sonnet","display_name":"Spaced"}]}),
    );
    assert_eq!(anthropic.len(), 1);
    let openrouter = parse_openrouter_models(&json!({"data":[
        {"id":"anthropic/claude-sonnet","name":"Anthropic: Claude Sonnet","supported_parameters":["tools","temperature"]},
        {"id":"openai/gpt-no-tools","name":"No tools","supported_parameters":["temperature"]},
        {"id":"google/gemini","name":"Google: Gemini","supported_parameters":["tools"]},
        {"id":"google/gemini","name":"Duplicate","supported_parameters":["tools"]},
        {"id":"bad model","name":"Unsafe","supported_parameters":["tools"]}
    ]}));
    assert_eq!(
        openrouter
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["anthropic/claude-sonnet", "google/gemini"]
    );
    assert_eq!(openrouter[0].label, "Anthropic: Claude Sonnet");
    assert_eq!(selected_model(" composer-2.5 "), "composer-2.5");
    assert_eq!(selected_model("Cursor CLI"), "");
    assert_eq!(selected_model("--model"), "");
}
