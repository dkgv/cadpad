//! Headless Claude / Codex / Cursor CLIs as subscription-backed agent hosts.
//! Cadpad supplies tools through the existing MCP bridge; the CLI owns the loop.
use cadpad_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::watch,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    AnthropicApi,
    ClaudeCli,
    CodexCli,
    CursorCli,
}
impl Default for Backend {
    fn default() -> Self {
        Self::AnthropicApi
    }
}
impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicApi => "anthropic_api",
            Self::ClaudeCli => "claude_cli",
            Self::CodexCli => "codex_cli",
            Self::CursorCli => "cursor_cli",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::AnthropicApi => "Anthropic API",
            Self::ClaudeCli => "Claude Code CLI",
            Self::CodexCli => "Codex CLI",
            Self::CursorCli => "Cursor CLI",
        }
    }
    pub fn is_cli(self) -> bool {
        !matches!(self, Self::AnthropicApi)
    }
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "anthropic" | "anthropic_api" => Ok(Self::AnthropicApi),
            "claude_cli" | "claude" => Ok(Self::ClaudeCli),
            "codex_cli" | "codex" => Ok(Self::CodexCli),
            "cursor_cli" | "cursor" => Ok(Self::CursorCli),
            _ => Err(Error::domain(
                "INVALID_PARAMETER",
                "Choose Anthropic API, Claude CLI, Codex CLI, or Cursor CLI",
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Bridge {
    pub mcp_executable: PathBuf,
    pub connection_file: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Detection {
    pub backend: Backend,
    pub installed: bool,
    pub authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub detail: String,
}

fn candidates(backend: Backend) -> &'static [&'static str] {
    match backend {
        Backend::ClaudeCli => &["claude"],
        Backend::CodexCli => &["codex"],
        Backend::CursorCli => &["cursor-agent", "agent"],
        Backend::AnthropicApi => &[],
    }
}

async fn which(name: &str) -> Option<PathBuf> {
    let output = Command::new("/usr/bin/env")
        .arg("which")
        .arg(name)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty() && Path::new(&path).is_file()).then(|| PathBuf::from(path))
}

async fn run_status(binary: &Path, args: &[&str]) -> Option<(bool, String)> {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        Command::new(binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some((output.status.success(), text))
}

/// "Logged in as …" style status output; "not logged in" is a negative.
fn reports_logged_in(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("logged in") && !lower.contains("not logged in")
}

pub async fn detect(backend: Backend) -> Detection {
    let mut detection = Detection {
        backend,
        installed: false,
        authenticated: false,
        path: None,
        detail: String::new(),
    };
    if backend == Backend::AnthropicApi {
        detection.installed = true;
        detection.authenticated = true;
        detection.detail = "Uses an Anthropic API key from the keychain".into();
        return detection;
    }
    let mut binary = None;
    for name in candidates(backend) {
        if let Some(path) = which(name).await {
            binary = Some(path);
            break;
        }
    }
    let Some(binary) = binary else {
        detection.detail = format!(
            "Install the {} to use your subscription from Cadpad",
            backend.label()
        );
        return detection;
    };
    detection.installed = true;
    detection.path = Some(binary.display().to_string());
    match backend {
        Backend::ClaudeCli => {
            let status = run_status(&binary, &["auth", "status"]).await;
            let value = status
                .as_ref()
                .and_then(|(_, text)| serde_json::from_str::<Value>(text.trim()).ok())
                .unwrap_or(Value::Null);
            if value["loggedIn"].as_bool().unwrap_or(false) {
                detection.authenticated = true;
                let method = value["subscriptionType"]
                    .as_str()
                    .or_else(|| value["authMethod"].as_str())
                    .unwrap_or("unknown");
                detection.detail = format!("Signed in with {method}");
            } else {
                detection.detail = "Run `claude auth login` to use your Claude subscription".into();
            }
        }
        Backend::CodexCli => {
            let status = run_status(&binary, &["login", "status"]).await;
            if status.is_some_and(|(ok, text)| ok && reports_logged_in(&text)) {
                detection.authenticated = true;
                detection.detail = "Signed in".into();
            } else {
                detection.detail = "Run `codex login` to use your ChatGPT subscription".into();
            }
        }
        Backend::CursorCli => {
            let status = run_status(&binary, &["status"]).await;
            if status.is_some_and(|(ok, text)| ok && reports_logged_in(&text)) {
                detection.authenticated = true;
                detection.detail = "Signed in".into();
            } else {
                detection.detail = "Run `agent login` to use your Cursor subscription".into();
            }
        }
        Backend::AnthropicApi => {}
    }
    detection
}

pub async fn detect_all() -> Vec<Detection> {
    let (claude, codex, cursor) = tokio::join!(
        detect(Backend::ClaudeCli),
        detect(Backend::CodexCli),
        detect(Backend::CursorCli)
    );
    vec![claude, codex, cursor]
}

pub struct LaunchRequest<'a> {
    pub backend: Backend,
    pub bridge: &'a Bridge,
    pub workspace_id: &'a str,
    pub project_directory: Option<&'a std::path::Path>,
    pub prompt: &'a str,
    pub client_name: &'a str,
}

pub struct Launch {
    pub child: Child,
    pub work_dir: PathBuf,
}

/// Codex config values are TOML basic strings; JSON string escaping is compatible.
fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

pub async fn spawn(request: LaunchRequest<'_>) -> Result<Launch> {
    let status = detect(request.backend).await;
    if !status.installed || !status.authenticated {
        return Err(Error::domain("LLM_NOT_CONFIGURED", status.detail));
    }
    let binary =
        PathBuf::from(status.path.ok_or_else(|| {
            Error::domain("LLM_NOT_CONFIGURED", "CLI executable path is missing")
        })?);
    if !request.bridge.mcp_executable.is_file() {
        return Err(Error::domain(
            "APP_UNAVAILABLE",
            "Cadpad MCP bridge is missing; restart through the desktop launcher",
        ));
    }
    let work_dir = std::env::temp_dir().join(format!("cadpad-cli-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&work_dir).await?;
    let mcp = json!({
        "mcpServers": {
            "cadpad": {
                "command": request.bridge.mcp_executable,
                "args": [
                    "--connection",
                    request.bridge.connection_file,
                    "--client-name",
                    request.client_name
                ]
            }
        }
    });
    let catalog = cadpad_core::tools::definitions_ref().iter()
        .map(|tool| format!("- {}: {}", tool.name, tool.description))
        .collect::<Vec<_>>().join("\n");
    let system = format!(
        "You are editing a Cadpad CAD document through the cadpad MCP server. \
         Call get_project with workspace_id={} first, then use explicit workspace_id and revision \
         on every Cadpad tool. The only modeling target is the supplied workspace. \
         Submit design source through update_source and build so Cadpad can validate and display it. \
         Keep millimetre units. Do not invent exact dimensions from pixels.

         Available Cadpad MCP commands (discover the input schemas you need through the connected server in one lookup when possible; reuse discovered schemas for the rest of the turn, and do not rediscover before each call):
{catalog}

         Modeling workflow: get_project and read_source first. Use update_source to stage changes, build the returned draft,          and poll get_job until terminal. A failed build leaves the previous model displayed; repair the draft rather than claiming success.          After success, get_project again and use inspect for exact geometry and capture_view to check the visible result.          Only claim a change is reflected in the canvas after verifying the committed revision. Use managed asset tools for imports;          an STL reference mesh is not an editable parametric solid. Keep all project calls scoped to this workspace.          Use the connected cadpad MCP commands directly rather than asking the user to run them.",
        request.workspace_id
    );
    let system = format!("{system}\nProject folder: {}. This is the sole source of truth. Paths mentioned in old messages are historical context, not additional workspaces. Do not read, edit, synchronize or export to other project folders. Use Cadpad MCP for all model edits and managed exports. Never maintain a second copy of the model.", request.project_directory.map(|path| path.display().to_string()).unwrap_or_else(|| "unsaved Cadpad workspace".into()));
    let child = match request.backend {
        Backend::ClaudeCli => {
            let mcp_path = work_dir.join("mcp.json");
            tokio::fs::write(&mcp_path, serde_json::to_vec_pretty(&mcp)?).await?;
            Command::new(&binary)
                .current_dir(&work_dir)
                .env_remove("ANTHROPIC_API_KEY")
                .env_remove("ANTHROPIC_AUTH_TOKEN")
                .args(["-p", request.prompt, "--mcp-config"])
                .arg(&mcp_path)
                .args([
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--include-partial-messages",
                    "--permission-mode",
                    "acceptEdits",
                    "--tools",
                    "",
                    "--strict-mcp-config",
                    "--append-system-prompt",
                    &system,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
        }
        Backend::CodexCli => {
            // Keep the user's normal CLI configuration, skills and authentication.
            // Override only this run's workspace permissions and Cadpad connection.
            let command_config = format!(
                "mcp_servers.cadpad.command={}",
                toml_string(&request.bridge.mcp_executable.display().to_string())
            );
            let args_config = format!(
                "mcp_servers.cadpad.args=[{}, {}, {}, {}]",
                toml_string("--connection"),
                toml_string(&request.bridge.connection_file.display().to_string()),
                toml_string("--client-name"),
                toml_string(request.client_name)
            );
            tokio::fs::write(work_dir.join("AGENTS.md"), system.as_bytes()).await?;
            Command::new(&binary)
                .current_dir(&work_dir)
                .env_remove("OPENAI_API_KEY")
                .env_remove("CODEX_API_KEY")
                .args([
                    "exec",
                    "--json",
                    "--sandbox",
                    "workspace-write",
                    "--skip-git-repo-check",
                    "-c",
                    "approval_policy=\"never\"",
                    "-c",
                    &command_config,
                    "-c",
                    &args_config,
                    request.prompt,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
        }
        Backend::CursorCli => {
            let cursor_dir = work_dir.join(".cursor");
            tokio::fs::create_dir_all(&cursor_dir).await?;
            tokio::fs::write(
                cursor_dir.join("mcp.json"),
                serde_json::to_vec_pretty(&mcp)?,
            )
            .await?;
            tokio::fs::write(work_dir.join("AGENTS.md"), system.as_bytes()).await?;
            Command::new(&binary)
                .current_dir(&work_dir)
                .env_remove("CURSOR_API_KEY")
                .args([
                    "-p",
                    request.prompt,
                    "--output-format",
                    "stream-json",
                    "--stream-partial-output",
                    "--approve-mcps",
                    "--sandbox",
                    "enabled",
                    "--trust",
                    "--workspace",
                ])
                .arg(&work_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
        }
        Backend::AnthropicApi => {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Anthropic API turns do not spawn a CLI",
            ));
        }
    }
    .map_err(|_| {
        Error::domain(
            "LLM_UNAVAILABLE",
            format!("Could not start {}", request.backend.label()),
        )
    })?;
    Ok(Launch { child, work_dir })
}

#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    /// Visible answer text.
    Text(String),
    /// The current answer segment ended; the next text starts a new paragraph.
    TextBreak,
    /// Reasoning text, shown as a live "thinking" hint.
    Thinking(String),
    ThinkingDone,
    ToolStarted {
        id: Option<String>,
        name: String,
        /// The agent's own one-line reason for the call, when the CLI reports one.
        detail: Option<String>,
    },
    ToolFinished {
        id: Option<String>,
        name: String,
        error: bool,
    },
    Done,
}

/// Normalizes `mcp__cadpad__build`, `cadpad-build` and similar to `build`.
pub fn tool_name(raw: &str) -> String {
    let raw = raw.trim();
    for prefix in ["mcp__cadpad__", "cadpad-", "cadpad__", "cadpad."] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    raw.to_string()
}

/// Stateful JSONL parser for one CLI run. Each backend repeats information in
/// its stream (partial deltas and a final full message); this keeps exactly
/// one copy and turns the rest into segment boundaries.
#[derive(Default)]
pub struct StreamParser {
    backend: Option<Backend>,
    /// Claude tool_use id -> normalized tool name, to label tool results.
    claude_tools: std::collections::HashMap<String, String>,
    /// Claude content block index -> block type for the open message.
    claude_blocks: std::collections::HashMap<u64, String>,
    claude_streamed: bool,
    /// Cursor text streamed since the last segment boundary.
    cursor_segment: String,
}

impl StreamParser {
    pub fn new(backend: Backend) -> Self {
        Self {
            backend: Some(backend),
            ..Self::default()
        }
    }

    pub fn parse(&mut self, line: &str) -> Vec<StreamEvent> {
        let line = line.trim();
        if line.is_empty() || line == "[DONE]" {
            return vec![];
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return vec![];
        };
        match self.backend {
            Some(Backend::ClaudeCli) => self.claude(&value),
            Some(Backend::CursorCli) => self.cursor(&value),
            Some(Backend::CodexCli) => codex(&value),
            _ => vec![],
        }
    }

    fn claude(&mut self, value: &Value) -> Vec<StreamEvent> {
        match value["type"].as_str().unwrap_or("") {
            "stream_event" => {
                let event = &value["event"];
                let index = event["index"].as_u64().unwrap_or(0);
                match event["type"].as_str().unwrap_or("") {
                    "content_block_start" => {
                        let kind = event["content_block"]["type"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        self.claude_blocks.insert(index, kind);
                        vec![]
                    }
                    "content_block_delta" => {
                        let delta = &event["delta"];
                        match delta["type"].as_str().unwrap_or("") {
                            "text_delta" => {
                                self.claude_streamed = true;
                                delta["text"]
                                    .as_str()
                                    .map(|text| vec![StreamEvent::Text(text.into())])
                                    .unwrap_or_default()
                            }
                            "thinking_delta" => delta["thinking"]
                                .as_str()
                                .map(|text| vec![StreamEvent::Thinking(text.into())])
                                .unwrap_or_default(),
                            _ => vec![],
                        }
                    }
                    "content_block_stop" => match self.claude_blocks.remove(&index).as_deref() {
                        Some("text") => vec![StreamEvent::TextBreak],
                        Some("thinking") => vec![StreamEvent::ThinkingDone],
                        _ => vec![],
                    },
                    _ => vec![],
                }
            }
            "assistant" => {
                let mut events = vec![];
                for block in value["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str().unwrap_or("") {
                        // Partial streaming already delivered this text.
                        "text" if !self.claude_streamed => {
                            if let Some(text) = block["text"].as_str() {
                                events.push(StreamEvent::Text(text.into()));
                                events.push(StreamEvent::TextBreak);
                            }
                        }
                        "tool_use" => {
                            let id = block["id"].as_str().map(str::to_string);
                            let name = tool_name(block["name"].as_str().unwrap_or("tool"));
                            if let Some(id) = &id {
                                self.claude_tools.insert(id.clone(), name.clone());
                            }
                            events.push(StreamEvent::ToolStarted {
                                id,
                                name,
                                detail: block["input"]["description"].as_str().map(str::to_string),
                            });
                        }
                        _ => {}
                    }
                }
                events
            }
            "user" => value["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|block| block["type"] == "tool_result")
                .map(|block| {
                    let id = block["tool_use_id"].as_str().map(str::to_string);
                    let name = id
                        .as_ref()
                        .and_then(|id| self.claude_tools.get(id).cloned())
                        .unwrap_or_else(|| "tool".into());
                    StreamEvent::ToolFinished {
                        id,
                        name,
                        error: block["is_error"].as_bool().unwrap_or(false),
                    }
                })
                .collect(),
            "result" => vec![StreamEvent::Done],
            _ => vec![],
        }
    }
}

fn cursor_tool(value: &Value) -> (Option<String>, String, Option<String>, bool) {
    let id = value["call_id"]
        .as_str()
        .or_else(|| value["tool_call"]["toolCallId"].as_str())
        .map(str::to_string);
    let Some((key, call)) = value["tool_call"]
        .as_object()
        .and_then(|object| object.iter().find(|(key, _)| key.ends_with("ToolCall")))
    else {
        return (id, "tool".into(), None, false);
    };
    let args = &call["args"];
    let name = match key.as_str() {
        "mcpToolCall" => tool_name(
            args["toolName"]
                .as_str()
                .or_else(|| args["name"].as_str())
                .unwrap_or("tool"),
        ),
        "getMcpToolsToolCall" => "tool_schema".into(),
        other => format!("cursor:{}", other.trim_end_matches("ToolCall")),
    };
    let detail = call["description"].as_str().map(str::to_string).or_else(|| {
        if key != "getMcpToolsToolCall" { return None; }
        let names = args.get("toolNames").or_else(||args.get("tools")).or_else(||args.get("toolName"));
        names.and_then(|value| match value {
            Value::String(name) => Some(name.clone()),
            Value::Array(names) => Some(names.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")),
            _ => None,
        }).filter(|names|!names.is_empty())
    });
    let result = &call["result"];
    let error = result.get("error").is_some()
        || result.get("failure").is_some()
        || result["success"]["isError"].as_bool().unwrap_or(false);
    (id, name, detail, error)
}

impl StreamParser {
    fn cursor(&mut self, value: &Value) -> Vec<StreamEvent> {
        let kind = value["type"].as_str().unwrap_or("");
        if kind != "assistant" {
            if kind == "tool_call" || kind == "thinking" {
                self.cursor_segment.clear();
            }
            return cursor(value);
        }
        let text = value["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| block["text"].as_str())
            .collect::<String>();
        if text.is_empty() {
            return vec![];
        }
        // With --stream-partial-output each segment streams as deltas and is
        // then repeated once in full (with or without timestamp_ms). The
        // repeat marks the segment boundary and is not shown again.
        if !self.cursor_segment.is_empty() && text == self.cursor_segment {
            self.cursor_segment.clear();
            return vec![StreamEvent::TextBreak];
        }
        if value.get("timestamp_ms").is_none() {
            // A whole message without preceding deltas (no partial output).
            self.cursor_segment.clear();
            return vec![StreamEvent::Text(text), StreamEvent::TextBreak];
        }
        self.cursor_segment.push_str(&text);
        vec![StreamEvent::Text(text)]
    }
}

fn cursor(value: &Value) -> Vec<StreamEvent> {
    match (
        value["type"].as_str().unwrap_or(""),
        value["subtype"].as_str().unwrap_or(""),
    ) {
        ("thinking", "delta") => value["text"]
            .as_str()
            .map(|text| vec![StreamEvent::Thinking(text.into())])
            .unwrap_or_default(),
        ("thinking", "completed") => vec![StreamEvent::ThinkingDone],
        ("tool_call", "started") => {
            let (id, name, detail, _) = cursor_tool(value);
            vec![StreamEvent::ToolStarted { id, name, detail }]
        }
        ("tool_call", "completed") => {
            let (id, name, _, error) = cursor_tool(value);
            vec![StreamEvent::ToolFinished { id, name, error }]
        }
        ("result", _) => vec![StreamEvent::Done],
        _ => vec![],
    }
}

fn codex(value: &Value) -> Vec<StreamEvent> {
    let kind = value["type"].as_str().unwrap_or("");
    // Current `codex exec --json`: item.started / item.updated / item.completed.
    if let Some(phase) = kind.strip_prefix("item.") {
        let item = &value["item"];
        let id = item["id"].as_str().map(str::to_string);
        return match (item["type"].as_str().unwrap_or(""), phase) {
            ("agent_message", "completed") => item["text"]
                .as_str()
                .map(|text| vec![StreamEvent::Text(text.into()), StreamEvent::TextBreak])
                .unwrap_or_default(),
            ("reasoning", "completed") => item["text"]
                .as_str()
                .map(|text| {
                    vec![
                        StreamEvent::Thinking(text.into()),
                        StreamEvent::ThinkingDone,
                    ]
                })
                .unwrap_or_default(),
            ("mcp_tool_call", "started") => vec![StreamEvent::ToolStarted {
                id,
                name: tool_name(item["tool"].as_str().unwrap_or("tool")),
                detail: None,
            }],
            ("mcp_tool_call", "completed") => vec![StreamEvent::ToolFinished {
                id,
                name: tool_name(item["tool"].as_str().unwrap_or("tool")),
                error: item["status"] == "failed"
                    || item.get("error").is_some_and(|e| !e.is_null()),
            }],
            ("command_execution", "started") => vec![StreamEvent::ToolStarted {
                id,
                name: "cli:command".into(),
                detail: item["command"].as_str().map(str::to_string),
            }],
            ("command_execution", "completed") => vec![StreamEvent::ToolFinished {
                id,
                name: "cli:command".into(),
                error: item["status"] == "failed",
            }],
            _ => vec![],
        };
    }
    match kind {
        "agent_message" | "message" => value["text"]
            .as_str()
            .or_else(|| value["message"].as_str())
            .map(|text| vec![StreamEvent::Text(text.into()), StreamEvent::TextBreak])
            .unwrap_or_default(),
        "agent_message_delta" | "message.delta" => value["delta"]
            .as_str()
            .or_else(|| value["text"].as_str())
            .map(|text| vec![StreamEvent::Text(text.into())])
            .unwrap_or_default(),
        "tool_call" | "function_call" => vec![StreamEvent::ToolStarted {
            id: value["call_id"].as_str().map(str::to_string),
            name: tool_name(value["name"].as_str().unwrap_or("tool")),
            detail: None,
        }],
        "tool_result" | "function_call_output" => vec![StreamEvent::ToolFinished {
            id: value["call_id"].as_str().map(str::to_string),
            name: tool_name(value["name"].as_str().unwrap_or("tool")),
            error: value["is_error"].as_bool().unwrap_or(false),
        }],
        "turn.completed" | "result" => vec![StreamEvent::Done],
        _ => vec![],
    }
}

/// One-line convenience for callers that only need the first event of a line.
pub fn parse_line(backend: Backend, line: &str) -> Option<StreamEvent> {
    StreamParser::new(backend).parse(line).into_iter().next()
}

pub async fn drive(
    mut launch: Launch,
    backend: Backend,
    mut cancel: watch::Receiver<bool>,
    mut on_event: impl FnMut(StreamEvent),
) -> Result<()> {
    // A full stderr pipe can block the CLI before it closes stdout or exits.
    let stderr_task = launch.child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        })
    });
    let result = async {
        let stdout = launch.child.stdout.take().ok_or_else(|| Error::domain("LLM_UNAVAILABLE", "CLI stdout unavailable"))?;
        let mut lines = BufReader::new(stdout).lines();
        let mut parser = StreamParser::new(backend);
        let mut done = false;
        loop {
            if *cancel.borrow() { return Err(Error::domain("CANCELLED", "Agent stopped")); }
            let next = tokio::select! {
                line = lines.next_line() => line,
                _ = cancel.changed() => return Err(Error::domain("CANCELLED", "Agent stopped")),
                _ = tokio::time::sleep(Duration::from_secs(120)) => return Err(Error::domain("LLM_UNAVAILABLE", "CLI produced no output for too long")),
            };
            match next {
                Ok(Some(line)) => {
                    for event in parser.parse(&line) {
                        if matches!(event, StreamEvent::Done) { done = true; }
                        on_event(event);
                    }
                    // A terminal protocol result completes the answer. Some CLI
                    // versions keep their MCP connections and stdout open afterward.
                    if done { break; }
                }
                Ok(None) => break,
                Err(_) => return Err(Error::domain("LLM_UNAVAILABLE", "CLI stream was interrupted")),
            }
        }
        if *cancel.borrow() { return Err(Error::domain("CANCELLED", "Agent stopped")); }
        let status = tokio::select! {
            status = launch.child.wait() => status.map_err(|_| Error::domain("LLM_UNAVAILABLE", "CLI exited unexpectedly"))?,
            _ = cancel.changed() => return Err(Error::domain("CANCELLED", "Agent stopped")),
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                if done { return Ok(()); }
                return Err(Error::domain("LLM_UNAVAILABLE", "CLI closed its output but did not exit"));
            }
        };
        if !status.success() && !done {
            return Err(Error::domain("LLM_UNAVAILABLE", format!("{} exited with status {}", backend.label(), status.code().unwrap_or(-1))));
        }
        Ok(())
    }.await;
    // Clean up both terminal-result processes and all failure/cancellation paths.
    if launch.child.try_wait().ok().flatten().is_none() {
        let _ = launch.child.kill().await;
    }
    if let Some(task) = stderr_task {
        task.abort();
        let _ = task.await;
    }
    let _ = tokio::fs::remove_dir_all(&launch.work_dir).await;
    result
}
