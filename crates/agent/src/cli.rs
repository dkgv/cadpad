//! Headless Claude / Codex / Cursor / OpenCode / Antigravity CLIs as agent hosts.
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
    #[serde(rename = "openrouter_api")]
    OpenRouterApi,
    ClaudeCli,
    CodexCli,
    CursorCli,
    #[serde(rename = "opencode_cli")]
    OpenCodeCli,
    #[serde(rename = "antigravity_cli")]
    AntigravityCli,
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
            Self::OpenRouterApi => "openrouter_api",
            Self::ClaudeCli => "claude_cli",
            Self::CodexCli => "codex_cli",
            Self::CursorCli => "cursor_cli",
            Self::OpenCodeCli => "opencode_cli",
            Self::AntigravityCli => "antigravity_cli",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::AnthropicApi => "Anthropic API",
            Self::OpenRouterApi => "OpenRouter API",
            Self::ClaudeCli => "Claude Code CLI",
            Self::CodexCli => "Codex CLI",
            Self::CursorCli => "Cursor CLI",
            Self::OpenCodeCli => "OpenCode CLI",
            Self::AntigravityCli => "Antigravity CLI",
        }
    }
    pub fn is_cli(self) -> bool {
        matches!(
            self,
            Self::ClaudeCli
                | Self::CodexCli
                | Self::CursorCli
                | Self::OpenCodeCli
                | Self::AntigravityCli
        )
    }
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "anthropic" | "anthropic_api" => Ok(Self::AnthropicApi),
            "openrouter" | "openrouter_api" => Ok(Self::OpenRouterApi),
            "claude_cli" | "claude" => Ok(Self::ClaudeCli),
            "codex_cli" | "codex" => Ok(Self::CodexCli),
            "cursor_cli" | "cursor" => Ok(Self::CursorCli),
            "opencode_cli" | "opencode" => Ok(Self::OpenCodeCli),
            "antigravity_cli" | "antigravity" | "agy" => Ok(Self::AntigravityCli),
            _ => Err(Error::domain(
                "INVALID_PARAMETER",
                "Choose Anthropic API, OpenRouter API, Claude CLI, Codex CLI, Cursor CLI, OpenCode CLI, or Antigravity CLI",
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
        Backend::OpenCodeCli => &["opencode"],
        Backend::AntigravityCli => &["agy", "antigravity"],
        Backend::AnthropicApi | Backend::OpenRouterApi => &[],
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
    run_status_at(binary, args, None).await
}

async fn run_status_at(
    binary: &Path,
    args: &[&str],
    directory: Option<&Path>,
) -> Option<(bool, String)> {
    let mut command = Command::new(binary);
    command.args(args);
    if let Some(directory) = directory {
        command.current_dir(directory).env("PWD", directory);
    }
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        command
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
    if !backend.is_cli() {
        detection.installed = true;
        detection.authenticated = true;
        detection.detail = format!("Uses an {} key from the keychain", backend.label());
        return detection;
    }
    let Some(binary) = binary(backend).await else {
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
        Backend::OpenCodeCli => {
            let version = run_status(&binary, &["--version"]).await;
            if !version.is_some_and(|(ok, text)| ok && opencode_v2(&text)) {
                detection.detail = "Install OpenCode 2 or newer to use it from Cadpad".into();
            } else if !opencode_models(&binary).await.is_empty() {
                // OpenCode can use saved accounts, environment credentials, local
                // providers, or free models; auth-list alone is not a readiness check.
                detection.authenticated = true;
                detection.detail = "Models available through your OpenCode configuration".into();
            } else {
                detection.detail =
                    "Configure a provider with `opencode auth login`, then check `opencode models`"
                        .into();
            }
        }
        Backend::AntigravityCli => {
            let status = run_status(&binary, &["models"]).await;
            if status.is_some_and(|(ok, text)| ok && !parse_antigravity_models(&text).is_empty()) {
                detection.authenticated = true;
                detection.detail = "Models available through your Antigravity account".into();
            } else {
                detection.detail = "Run `agy` to sign in to Antigravity".into();
            }
        }
        Backend::AnthropicApi | Backend::OpenRouterApi => {}
    }
    detection
}

pub async fn detect_all() -> Vec<Detection> {
    let (claude, codex, cursor, opencode, antigravity) = tokio::join!(
        detect(Backend::ClaudeCli),
        detect(Backend::CodexCli),
        detect(Backend::CursorCli),
        detect(Backend::OpenCodeCli),
        detect(Backend::AntigravityCli)
    );
    vec![claude, codex, cursor, opencode, antigravity]
}

pub struct LaunchRequest<'a> {
    pub backend: Backend,
    pub bridge: &'a Bridge,
    pub workspace_id: &'a str,
    pub project_directory: Option<&'a std::path::Path>,
    pub prompt: &'a str,
    pub client_name: &'a str,
    /// Empty means the CLI's own default. A saved backend label is treated the same way.
    pub model: &'a str,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelChoice {
    pub id: String,
    pub label: String,
}

/// Model ids are passed as a single CLI argument, so they cannot look like flags.
pub fn valid_model_id(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 200
        && !model.starts_with('-')
        && model.chars().all(|char| {
            char.is_ascii_alphanumeric()
                || matches!(
                    char,
                    '.' | '_'
                        | '-'
                        | ':'
                        | '@'
                        | '/'
                        | '+'
                        | '='
                        | '['
                        | ']'
                        | ','
                        | '#'
                        | '~'
                        | '%'
                )
        })
}

/// The id to send to a provider, or empty when the user wants that provider's default.
pub fn selected_model(model: &str) -> &str {
    let model = model.trim();
    if valid_model_id(model) { model } else { "" }
}

pub fn legacy_default_model(model: &str) -> bool {
    let model = model.trim();
    model.is_empty()
        || [
            Backend::AnthropicApi,
            Backend::OpenRouterApi,
            Backend::ClaudeCli,
            Backend::CodexCli,
            Backend::CursorCli,
            Backend::OpenCodeCli,
            Backend::AntigravityCli,
        ]
        .into_iter()
        .any(|backend| backend.label() == model)
}

pub fn parse_cursor_models(text: &str) -> Vec<ModelChoice> {
    let mut models: Vec<ModelChoice> = Vec::new();
    for line in text.lines() {
        let Some((id, label)) = line.trim().split_once(" - ") else {
            continue;
        };
        let id = id.trim();
        let label = label.trim();
        if !valid_model_id(id) || label.is_empty() || models.iter().any(|model| model.id == id) {
            continue;
        }
        models.push(ModelChoice {
            id: id.to_string(),
            label: label.to_string(),
        });
    }
    models
}

/// `agy models` emits one `id<TAB>label` row per model after a status line.
pub fn parse_antigravity_models(text: &str) -> Vec<ModelChoice> {
    let mut models: Vec<ModelChoice> = Vec::new();
    for line in text.lines() {
        let Some((id, label)) = line
            .trim()
            .split_once('\t')
            .or_else(|| line.trim().split_once("  "))
        else {
            continue;
        };
        let id = id.trim();
        let label = label.trim();
        if !valid_model_id(id) || label.is_empty() || models.iter().any(|model| model.id == id) {
            continue;
        }
        models.push(ModelChoice {
            id: id.to_string(),
            label: label.to_string(),
        });
    }
    models
}

fn opencode_v2(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        word.trim_start_matches('v')
            .split_once('.')
            .is_some_and(|(major, _)| major.parse::<u32>().is_ok_and(|major| major >= 2))
    })
}

/// `opencode models` emits one provider/model[#variant] id per line.
pub fn parse_opencode_models(text: &str) -> Vec<ModelChoice> {
    let mut models: Vec<ModelChoice> = Vec::new();
    for id in text.lines().map(str::trim) {
        if !valid_model_id(id)
            || !id
                .split_once('/')
                .is_some_and(|(provider, model)| !provider.is_empty() && !model.is_empty())
            || models.iter().any(|model| model.id == id)
        {
            continue;
        }
        models.push(ModelChoice {
            id: id.into(),
            label: id.into(),
        });
    }
    models
}

async fn opencode_models(binary: &Path) -> Vec<ModelChoice> {
    // Match the isolated launch directory while retaining the user's global
    // OpenCode configuration and authentication.
    run_status_at(
        binary,
        &["models", "--standalone"],
        Some(&std::env::temp_dir()),
    )
    .await
    .filter(|(ok, _)| *ok)
    .map(|(_, text)| parse_opencode_models(&text))
    .unwrap_or_default()
}

pub fn parse_codex_models(value: &Value) -> Vec<ModelChoice> {
    let Some(entries) = value.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for entry in entries {
        if entry.get("visibility").and_then(Value::as_str) == Some("hide") {
            continue;
        }
        let Some(id) = entry
            .get("slug")
            .and_then(Value::as_str)
            .filter(|id| valid_model_id(id))
        else {
            continue;
        };
        let label = entry
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|label| !label.is_empty())
            .unwrap_or(id);
        let priority = entry
            .get("priority")
            .and_then(Value::as_i64)
            .unwrap_or(1_000);
        rows.push((
            priority,
            ModelChoice {
                id: id.to_string(),
                label: label.to_string(),
            },
        ));
    }
    rows.sort_by_key(|(priority, _)| *priority);
    rows.into_iter().map(|(_, model)| model).collect()
}

pub fn parse_anthropic_models(value: &Value) -> Vec<ModelChoice> {
    let Some(entries) = value.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut models: Vec<ModelChoice> = Vec::new();
    for entry in entries {
        let Some(id) = entry
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_model_id(id))
        else {
            continue;
        };
        if models.iter().any(|model| model.id == id) {
            continue;
        }
        let label = entry
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|label| !label.is_empty())
            .unwrap_or(id);
        models.push(ModelChoice {
            id: id.to_string(),
            label: label.to_string(),
        });
    }
    models
}

/// `GET /api/v1/models` lists OpenAI-compatible model ids. Cadpad needs tool
/// calling, so entries that advertise parameters without tools are omitted.
pub fn parse_openrouter_models(value: &Value) -> Vec<ModelChoice> {
    let Some(entries) = value.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut models: Vec<ModelChoice> = Vec::new();
    for entry in entries {
        let Some(id) = entry
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_model_id(id))
        else {
            continue;
        };
        if let Some(parameters) = entry.get("supported_parameters").and_then(Value::as_array)
            && !parameters
                .iter()
                .any(|parameter| parameter.as_str() == Some("tools"))
        {
            continue;
        }
        if models.iter().any(|model| model.id == id) {
            continue;
        }
        let label = entry
            .get("name")
            .and_then(Value::as_str)
            .filter(|label| !label.is_empty())
            .unwrap_or(id);
        models.push(ModelChoice {
            id: id.to_string(),
            label: label.chars().take(200).collect(),
        });
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models
}

pub fn parse_claude_model_help(help: &str) -> Vec<ModelChoice> {
    let mut block = String::new();
    let mut remaining = 0u8;
    for line in help.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("--model") {
            remaining = 8;
        }
        if remaining == 0 {
            continue;
        }
        // The alias examples sit in the wrapped --model description, before the next flag.
        if !trimmed.starts_with("--model") && trimmed.starts_with('-') {
            break;
        }
        block.push_str(line);
        block.push(' ');
        remaining -= 1;
    }
    let mut models: Vec<ModelChoice> = Vec::new();
    let mut rest = block.as_str();
    while let Some(start) = rest.find('\'') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('\'') else { break };
        let id = &rest[..end];
        rest = &rest[end + 1..];
        if valid_model_id(id) && !models.iter().any(|model| model.id == id) {
            let mut label = id.to_string();
            if let Some(first) = label.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            models.push(ModelChoice {
                id: id.to_string(),
                label: format!("{label} (latest)"),
            });
        }
    }
    models
}

fn claude_fallback() -> Vec<ModelChoice> {
    ["fable", "opus", "sonnet"]
        .into_iter()
        .map(|id| {
            let mut label = id.to_string();
            if let Some(first) = label.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            ModelChoice {
                id: id.to_string(),
                label: format!("{label} (latest)"),
            }
        })
        .collect()
}

fn json_document(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

async fn binary(backend: Backend) -> Option<PathBuf> {
    for name in candidates(backend) {
        if let Some(path) = which(name).await {
            return Some(path);
        }
    }
    None
}

/// Models the installed CLI can run. Claude falls back to the aliases its help documents.
pub async fn list_models(backend: Backend) -> Vec<ModelChoice> {
    if !backend.is_cli() {
        return Vec::new();
    }
    let Some(binary) = binary(backend).await else {
        return if backend == Backend::ClaudeCli {
            claude_fallback()
        } else {
            Vec::new()
        };
    };
    match backend {
        Backend::ClaudeCli => {
            let help = run_status(&binary, &["--help"]).await.map(|(_, text)| text);
            let parsed = help
                .as_deref()
                .map(parse_claude_model_help)
                .unwrap_or_default();
            if parsed.is_empty() {
                claude_fallback()
            } else {
                parsed
            }
        }
        Backend::CodexCli => {
            let Some((ok, text)) = run_status(&binary, &["debug", "models"]).await else {
                return Vec::new();
            };
            if !ok {
                return Vec::new();
            }
            json_document(&text)
                .and_then(|text| serde_json::from_str(text).ok())
                .map(|value| parse_codex_models(&value))
                .unwrap_or_default()
        }
        Backend::CursorCli => run_status(&binary, &["--list-models"])
            .await
            .map(|(_, text)| parse_cursor_models(&text))
            .unwrap_or_default(),
        Backend::OpenCodeCli => opencode_models(&binary).await,
        Backend::AntigravityCli => run_status(&binary, &["models"])
            .await
            .map(|(_, text)| parse_antigravity_models(&text))
            .unwrap_or_default(),
        Backend::AnthropicApi | Backend::OpenRouterApi => Vec::new(),
    }
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
    let catalog = cadpad_core::tools::definitions_ref()
        .iter()
        .map(|tool| format!("- {}: {}", tool.name, tool.description))
        .collect::<Vec<_>>()
        .join("\n");
    let system = format!(
        "You are editing a Cadpad CAD document through the cadpad MCP server. \
         Call get_project with workspace_id={} first, then use explicit workspace_id and revision \
         on every Cadpad tool. The only modeling target is the supplied workspace. \
         Submit design source through update_source and build so Cadpad can validate and display it. \
         Keep millimetre units. Do not invent exact dimensions from pixels.

         Available Cadpad MCP commands (discover the input schemas you need through the connected server in one lookup when possible; reuse discovered schemas for the rest of the turn, and do not rediscover before each call):
{catalog}

         Modeling workflow: get_project first. Use the source path from the project or selection to locate text with search_source, then read around hits with read_source and start_line. Use read_source when you need the full file. Use trace_cad_edge for native edge adjacency and observed operation sites. For an STL edge use trace_mesh_edges, and use match_mesh_edge if the user identifies a corresponding CAD tab. Read-only lookup and edge tracing in that explicitly identified tab within this project are allowed. Use trace_part_source for a component publication site or linked asset path. Use update_source to stage changes, build the returned draft,          and poll get_job until terminal. A failed build leaves the previous model displayed; repair the draft rather than claiming success.          After success, get_project again and use inspect for geometry properties (mesh accuracy for STL) and capture_views with single=true to check the visible result. For multiple angles omit single; for before/after comparisons capture_views before editing, retain capture_id, then compare_views after building with current revision and pose_version.          If the project folder changed outside Cadpad, call redraw_canvas to re-present the viewport and re-check the disk, then poll get_project before capturing. Only claim a change is reflected in the canvas after verifying the committed revision. Use managed asset tools for imports;          an STL reference mesh is not an editable parametric solid. For rigid tweaks on STL assets use transform_mesh; use check_printability for slicer triage (watertightness, build volume, feature size, overhangs) and re-run it after edits. Keep all other project calls scoped to this workspace.          Use the connected cadpad MCP commands directly rather than asking the user to run them.",
        request.workspace_id
    );
    let system = format!(
        "{system}\nProject folder: {}. This is the sole source of truth. Paths mentioned in old messages are historical context, not additional workspaces. Do not read, edit, synchronize or export to other project folders. Use Cadpad MCP for all model edits and managed exports. Never maintain a second copy of the model. Never use browser, JS, or computer-use/window-automation tools to access, inspect, or drive the Cadpad app or window; use only the connected cadpad MCP tools and workspace files.",
        request
            .project_directory
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "unsaved Cadpad workspace".into())
    );
    let model = selected_model(request.model);
    let model_args: &[&str] = if model.is_empty() {
        &[]
    } else {
        &["--model", model]
    };
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
                    // Headless Claude cannot prompt to approve Cadpad MCP calls.
                    "--allowedTools",
                    "mcp__cadpad__*",
                    "--strict-mcp-config",
                    "--append-system-prompt",
                    &system,
                ])
                .args(model_args)
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
                    // approval_policy=never suppresses the prompt, so every MCP
                    // tool call fails with "requires approval" unless the
                    // Cadpad server is set to auto-approve its tools.
                    "-c",
                    "mcp_servers.cadpad.default_tools_approval_mode=\"approve\"",
                    "-c",
                    &command_config,
                    "-c",
                    &args_config,
                    request.prompt,
                ])
                .args(model_args)
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
                .args(model_args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
        }
        Backend::OpenCodeCli => {
            let config = opencode_config(request.bridge, request.client_name, &system);
            Command::new(&binary)
                .current_dir(&work_dir)
                .env("PWD", &work_dir)
                .env("OPENCODE_CONFIG_CONTENT", serde_json::to_string(&config)?)
                .args([
                    "run",
                    "--standalone",
                    "--format",
                    "json",
                    "--thinking",
                    "--agent",
                    "cadpad",
                ])
                .args(model_args)
                .arg("--")
                .arg(request.prompt)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
        }
        Backend::AntigravityCli => {
            let agents_dir = work_dir.join(".agents");
            tokio::fs::create_dir_all(&agents_dir).await?;
            tokio::fs::write(
                agents_dir.join("mcp_config.json"),
                serde_json::to_vec_pretty(&mcp)?,
            )
            .await?;
            tokio::fs::write(work_dir.join("AGENTS.md"), system.as_bytes()).await?;
            let settings = json!({"trustedWorkspaces": [&work_dir]});
            Command::new(&binary)
                .current_dir(&work_dir)
                .args([
                    "-p",
                    request.prompt,
                    "--output-format",
                    "stream-json",
                    "--dangerously-skip-permissions",
                    "--settings",
                ])
                .arg(serde_json::to_string(&settings)?)
                .args(model_args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
        }
        Backend::AnthropicApi | Backend::OpenRouterApi => {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "API turns do not spawn a CLI",
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

/// OpenCode v2 configuration is scoped to this process. The dedicated agent
/// exposes Cadpad tools directly and prevents shell/file edits bypassing MCP.
fn opencode_config(bridge: &Bridge, client_name: &str, system: &str) -> Value {
    json!({
        "mcp": {"servers": {"cadpad": {
            "type": "local",
            "command": [bridge.mcp_executable, "--connection", bridge.connection_file, "--client-name", client_name],
            "codemode": false
        }}},
        "agents": {"cadpad": {
            "description": "Edit the active Cadpad document through MCP",
            "mode": "primary",
            "system": system,
            "permissions": [
                {"action": "*", "resource": "*", "effect": "deny"},
                {"action": "cadpad_*", "resource": "*", "effect": "allow"}
            ]
        }}
    })
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
        /// A more specific label than the tool's generic default.
        label: Option<String>,
        /// The agent's own one-line reason for the call, when the CLI reports one.
        detail: Option<String>,
    },
    ToolFinished {
        id: Option<String>,
        name: String,
        error: bool,
    },
    Usage(crate::provider::TokenUsage),
    Done,
}

fn usage_then_done(value: &Value) -> Vec<StreamEvent> {
    let mut events = Vec::with_capacity(2);
    if let Some(usage) = value
        .get("usage")
        .or_else(|| value["result"].get("usage"))
        .and_then(crate::provider::TokenUsage::from_json)
    {
        events.push(StreamEvent::Usage(usage));
    }
    events.push(StreamEvent::Done);
    events
}

/// Normalizes `mcp__cadpad__build`, `cadpad-build` and similar to `build`.
pub fn tool_name(raw: &str) -> String {
    let raw = raw.trim();
    for prefix in ["mcp__cadpad__", "cadpad-", "cadpad__", "cadpad.", "cadpad_"] {
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
    /// Antigravity tool step index -> call details, so completion keeps its name.
    antigravity_tools: std::collections::HashMap<u64, (String, Option<String>, Option<String>)>,
    /// Antigravity tool steps whose start was already reported.
    antigravity_started: std::collections::HashSet<u64>,
    /// Antigravity agent_response steps that streamed visible text.
    antigravity_text: std::collections::HashSet<u64>,
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
            Some(Backend::OpenCodeCli) => opencode(&value),
            Some(Backend::AntigravityCli) => self.antigravity(&value),
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
                            let described = crate::activity::describe(&name, Some(&block["input"]));
                            events.push(StreamEvent::ToolStarted {
                                id,
                                name,
                                label: described.label,
                                detail: block["input"]["description"]
                                    .as_str()
                                    .map(str::to_string)
                                    .or(described.detail),
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
            "result" => usage_then_done(value),
            _ => vec![],
        }
    }
}

fn cursor_tool(value: &Value) -> (Option<String>, String, Option<String>, Option<String>, bool) {
    let id = value["call_id"]
        .as_str()
        .or_else(|| value["tool_call"]["toolCallId"].as_str())
        .map(str::to_string);
    let Some((key, call)) = value["tool_call"]
        .as_object()
        .and_then(|object| object.iter().find(|(key, _)| key.ends_with("ToolCall")))
    else {
        return (id, "tool".into(), None, None, false);
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
    let detail = call["description"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            if key != "getMcpToolsToolCall" {
                return None;
            }
            let names = args
                .get("toolNames")
                .or_else(|| args.get("tools"))
                .or_else(|| args.get("toolName"));
            names
                .and_then(|value| match value {
                    Value::String(name) => Some(name.clone()),
                    Value::Array(names) => Some(
                        names
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                    _ => None,
                })
                .filter(|names| !names.is_empty())
        });
    let described = crate::activity::describe(&name, Some(args));
    let result = &call["result"];
    let error = result.get("error").is_some()
        || result.get("failure").is_some()
        || result["success"]["isError"].as_bool().unwrap_or(false);
    (
        id,
        name,
        detail.or(described.detail),
        described.label,
        error,
    )
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
            let (id, name, detail, label, _) = cursor_tool(value);
            vec![StreamEvent::ToolStarted {
                id,
                name,
                label,
                detail,
            }]
        }
        ("tool_call", "completed") => {
            let (id, name, _, _, error) = cursor_tool(value);
            vec![StreamEvent::ToolFinished { id, name, error }]
        }
        ("result", _) => usage_then_done(value),
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
            ("mcp_tool_call", "started") => {
                let name = tool_name(item["tool"].as_str().unwrap_or("tool"));
                let described = crate::activity::describe(&name, item.get("arguments"));
                vec![StreamEvent::ToolStarted {
                    id,
                    name,
                    label: described.label,
                    detail: described.detail,
                }]
            }
            ("mcp_tool_call", "completed") => vec![StreamEvent::ToolFinished {
                id,
                name: tool_name(item["tool"].as_str().unwrap_or("tool")),
                error: item["status"] == "failed"
                    || item.get("error").is_some_and(|e| !e.is_null()),
            }],
            ("command_execution", "started") => vec![StreamEvent::ToolStarted {
                id,
                name: "cli:command".into(),
                label: None,
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
            label: None,
            detail: None,
        }],
        "tool_result" | "function_call_output" => vec![StreamEvent::ToolFinished {
            id: value["call_id"].as_str().map(str::to_string),
            name: tool_name(value["name"].as_str().unwrap_or("tool")),
            error: value["is_error"].as_bool().unwrap_or(false),
        }],
        "turn.completed" | "result" => usage_then_done(value),
        _ => vec![],
    }
}

fn opencode(value: &Value) -> Vec<StreamEvent> {
    let part = &value["part"];
    match value["type"].as_str().unwrap_or("") {
        "text" => part["text"]
            .as_str()
            .map(|text| vec![StreamEvent::Text(text.into()), StreamEvent::TextBreak])
            .unwrap_or_default(),
        "reasoning" => part["text"]
            .as_str()
            .map(|text| {
                vec![
                    StreamEvent::Thinking(text.into()),
                    StreamEvent::ThinkingDone,
                ]
            })
            .unwrap_or_default(),
        "tool_use" => {
            let id = part["callID"]
                .as_str()
                .or_else(|| part["id"].as_str())
                .map(str::to_string);
            let name = tool_name(part["tool"].as_str().unwrap_or("tool"));
            let status = part["state"]["status"].as_str().unwrap_or("");
            if !matches!(status, "completed" | "error") {
                return vec![];
            }
            // OpenCode reports tools only after completion. Emit the pair so
            // the chat still records the call and its outcome.
            let described = crate::activity::describe(
                &name,
                part.get("state").and_then(|state| state.get("input")),
            );
            vec![
                StreamEvent::ToolStarted {
                    id: id.clone(),
                    name: name.clone(),
                    label: described.label,
                    detail: described.detail,
                },
                StreamEvent::ToolFinished {
                    id,
                    name,
                    error: status == "error",
                },
            ]
        }
        // A step is one model invocation; tool-calls finishes are not terminal.
        // Wait for EOF and the exit code even after a final stop, so later
        // errors or background work cannot be mistaken for success.
        _ => vec![],
    }
}

/// Antigravity exposes MCP tools through one generic `call_mcp_tool` step.
/// Translate it back to the Cadpad tool name so the chat can describe it.
fn antigravity_tool(raw: &str, parameters: &Value) -> (String, Option<String>, Option<String>) {
    if raw == "call_mcp_tool" {
        let tool = parameters["ToolName"]
            .as_str()
            .or_else(|| parameters["tool_name"].as_str())
            .unwrap_or("tool");
        let name = tool_name(tool);
        let arguments = parameters
            .get("Arguments")
            .or_else(|| parameters.get("arguments"))
            .filter(|value| !value.is_null());
        let described = crate::activity::describe(&name, arguments);
        return (name, described.label, described.detail);
    }
    if raw == "read_file"
        && let Some(path) = parameters["Path"].as_str()
        && path.contains("/mcp/")
        && path.ends_with(".json")
    {
        let tool = Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string);
        return ("tool_schema".into(), None, tool);
    }
    let name = tool_name(raw);
    let described = crate::activity::describe(&name, Some(parameters));
    (name, described.label, described.detail)
}

impl StreamParser {
    fn antigravity(&mut self, value: &Value) -> Vec<StreamEvent> {
        match value["event"].as_str().unwrap_or("") {
            "step_update" => self.antigravity_step(&value["step_update"]),
            "result" => usage_then_done(value),
            _ => vec![],
        }
    }

    fn antigravity_step(&mut self, step: &Value) -> Vec<StreamEvent> {
        let index = step["step_index"].as_u64().unwrap_or(0);
        match step["step_type"].as_str().unwrap_or("") {
            "agent_response" => {
                let mut events = Vec::new();
                if let Some(text) = step["text_delta"].as_str()
                    && !text.is_empty()
                {
                    self.antigravity_text.insert(index);
                    events.push(StreamEvent::Text(text.into()));
                }
                if step["state"] == "DONE" && self.antigravity_text.remove(&index) {
                    events.push(StreamEvent::TextBreak);
                }
                events
            }
            "tool" => {
                let id = Some(index.to_string());
                let info = &step["tool_info"];
                let raw = info["name"]
                    .as_str()
                    .or_else(|| step["tool_name"].as_str())
                    .unwrap_or("tool");
                let described = antigravity_tool(raw, &info["parameters"]);
                match step["state"].as_str().unwrap_or("") {
                    "ACTIVE" => {
                        self.antigravity_started.insert(index);
                        self.antigravity_tools.insert(index, described.clone());
                        vec![StreamEvent::ToolStarted {
                            id,
                            name: described.0,
                            label: described.1,
                            detail: described.2,
                        }]
                    }
                    "DONE" | "ERROR" => {
                        let described = self.antigravity_tools.remove(&index).unwrap_or(described);
                        let started = self.antigravity_started.remove(&index);
                        let name = described.0;
                        let error = step["state"] == "ERROR" || info.get("error").is_some();
                        let finished = StreamEvent::ToolFinished {
                            id: id.clone(),
                            name: name.clone(),
                            error,
                        };
                        if started {
                            vec![finished]
                        } else {
                            vec![
                                StreamEvent::ToolStarted {
                                    id,
                                    name,
                                    label: described.1,
                                    detail: described.2,
                                },
                                finished,
                            ]
                        }
                    }
                    _ => vec![],
                }
            }
            _ => vec![],
        }
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
                    if backend == Backend::OpenCodeCli
                        && let Ok(value) = serde_json::from_str::<Value>(&line)
                        && value["type"] == "error"
                    {
                        let message = value["error"]["message"].as_str()
                            .or_else(|| value["error"]["data"]["message"].as_str())
                            .unwrap_or("OpenCode could not complete the turn");
                        return Err(Error::domain("LLM_UNAVAILABLE", message));
                    }
                    if backend == Backend::AntigravityCli
                        && let Ok(value) = serde_json::from_str::<Value>(&line)
                        && value["event"] == "result"
                        && let Some(status) = value["result"]["status"].as_str()
                        && status != "SUCCESS"
                    {
                        let message = value["result"]["error"].as_str()
                            .unwrap_or("Antigravity could not complete the turn");
                        return Err(Error::domain("LLM_UNAVAILABLE", message));
                    }
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
