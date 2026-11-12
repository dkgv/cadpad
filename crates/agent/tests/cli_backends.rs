use cadpad_agent::cli::{Backend, StreamEvent, StreamParser, tool_name};
use serde_json::json;

#[test]
fn backend_labels_round_trip() {
    for backend in [
        Backend::AnthropicApi,
        Backend::ClaudeCli,
        Backend::CodexCli,
        Backend::CursorCli,
    ] {
        assert_eq!(Backend::parse(backend.as_str()).unwrap(), backend);
    }
    assert!(Backend::parse("nope").is_err());
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
        detail: None
    }));
    assert!(events.contains(&StreamEvent::ToolFinished {
        id: Some("tu_1".into()),
        name: "build".into(),
        error: true
    }));
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
fn tool_names_drop_cadpad_prefixes() {
    assert_eq!(tool_name("mcp__cadpad__build"), "build");
    assert_eq!(tool_name("cadpad-get_project"), "get_project");
    assert_eq!(tool_name("inspect"), "inspect");
}

#[tokio::test]
async fn detects_local_cli_binaries_without_panicking() {
    let detections = cadpad_agent::cli::detect_all().await;
    assert_eq!(detections.len(), 3);
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
    use cadpad_agent::cli::{parse_anthropic_models, parse_claude_model_help, parse_codex_models, parse_cursor_models, selected_model};
    let cursor = parse_cursor_models("Available models\n\nauto - Auto (default)\ngpt-5.6-sol-high - GPT-5.6 Sol High\n--bad - Ignored\n");
    assert_eq!(cursor.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), ["auto", "gpt-5.6-sol-high"]);
    let codex = parse_codex_models(&json!({"models":[
        {"slug":"gpt-6-sol","display_name":"GPT-6 Sol","visibility":"list","priority":4},
        {"slug":"gpt-6.1-sol","display_name":"GPT-6.1-Sol","visibility":"list","priority":1},
        {"slug":"hidden-model","display_name":"Hidden","visibility":"hide","priority":0},
        {"slug":"-flag","display_name":"Unsafe","visibility":"list","priority":2}
    ]}));
    assert_eq!(codex.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), ["gpt-6.1-sol", "gpt-6-sol"]);
    let claude = parse_claude_model_help("  --model <model>  alias (e.g. 'fable', 'opus', or 'sonnet')\n  -n, --name <name>  'ignored'\n");
    assert_eq!(claude.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), ["fable", "opus", "sonnet"]);
    let anthropic = parse_anthropic_models(&json!({"data":[{"id":"claude-sonnet","display_name":"Claude Sonnet"},{"id":"claude sonnet","display_name":"Spaced"}]}));
    assert_eq!(anthropic.len(), 1);
    assert_eq!(selected_model(" composer-2.5 "), "composer-2.5");
    assert_eq!(selected_model("Cursor CLI"), "");
    assert_eq!(selected_model("--model"), "");
}
