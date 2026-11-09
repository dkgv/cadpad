use crate::{
    cli::{self, Backend, Bridge, StreamEvent},
    credentials::{Credentials, Secret},
    provider::{Anthropic, Provider},
    references::{self, Attachments, ChatReference},
    runner::{self, ChatMessage, EmitProgress, Limits, Progress, Step, ToolState},
};
use cadpad_core::{Core, Error, JobState, Result, error_value};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast;

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub backend: Backend,
    pub model: String,
}
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    Running,
    Cancelling,
    Cancelled,
    Succeeded,
    Failed,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Turn {
    pub id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub chat_id: String,
    pub actor: String,
    pub state: TurnState,
    pub text: String,
    pub activity: String,
    pub error: Option<Value>,
    pub tool_calls: u32,
    pub rounds: u32,
    pub artifacts: Vec<Value>,
    pub started_at: u64,
    /// Live timeline of thinking, text segments and tool calls.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Whether the last text step may still grow (no segment break since).
    #[serde(skip)]
    text_open: bool,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct AgentEvent {
    pub workspace_id: String,
    pub turn_id: String,
    pub kind: String,
    pub delta: Option<String>,
    pub turn: Option<Turn>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Chat {
    id: String,
    title: String,
    created_at: u64,
    updated_at: u64,
    messages: Vec<ChatMessage>,
    last_turn: Option<Turn>,
    #[serde(default)]
    archived: bool,
}
impl Default for Chat {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: "New chat".into(),
            created_at: now(),
            updated_at: now(),
            messages: vec![],
            last_turn: None,
            archived: false,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct ChatBook {
    active_id: String,
    chats: Vec<Chat>,
}
impl Default for ChatBook {
    fn default() -> Self {
        let chat = Chat::default();
        Self {
            active_id: chat.id.clone(),
            chats: vec![chat],
        }
    }
}
impl ChatBook {
    fn current(&self) -> &Chat {
        self.chats
            .iter()
            .find(|chat| chat.id == self.active_id)
            .expect("selected chat exists")
    }
    fn current_mut(&mut self) -> &mut Chat {
        self.chats
            .iter_mut()
            .find(|chat| chat.id == self.active_id)
            .expect("selected chat exists")
    }
}
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredChats {
    Legacy(Vec<ChatMessage>),
    Book(ChatBook),
}
#[derive(Default)]
struct State {
    turns: HashMap<String, Turn>,
    active: HashMap<String, String>,
    conversations: HashMap<String, ChatBook>,
    changing_chats: HashSet<String>,
}
pub trait ProviderFactory: Send + Sync {
    fn create(&self, settings: &Settings, key: Secret) -> Result<Arc<dyn Provider>>;
}
pub struct DefaultProviderFactory;
impl ProviderFactory for DefaultProviderFactory {
    fn create(&self, settings: &Settings, key: Secret) -> Result<Arc<dyn Provider>> {
        Ok(Arc::new(Anthropic::new(settings.model.clone(), key)?))
    }
}
struct Inner {
    core: Core,
    root: PathBuf,
    settings: tokio::sync::Mutex<Settings>,
    credentials: Arc<dyn Credentials>,
    factory: Arc<dyn ProviderFactory>,
    bridge: Option<Bridge>,
    state: Mutex<State>,
    events: broadcast::Sender<AgentEvent>,
    limits: Limits,
    storage: tokio::sync::Mutex<()>,
}
#[derive(Clone)]
pub struct AgentService {
    inner: Arc<Inner>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn valid_workspace(workspace: &str) -> Result<()> {
    uuid::Uuid::parse_str(workspace)
        .map(|_| ())
        .map_err(|_| Error::domain("INVALID_REQUEST", "Invalid workspace ID"))
}
async fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(path.parent().unwrap())?;
        let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    })
    .await
    .map_err(|_| Error::domain("IO_ERROR", "Could not save agent state"))?
}
impl AgentService {
    pub async fn open(
        core: Core,
        root: PathBuf,
        credentials: Arc<dyn Credentials>,
        factory: Arc<dyn ProviderFactory>,
        limits: Limits,
    ) -> Result<Self> {
        Self::open_with_bridge(core, root, credentials, factory, limits, None).await
    }
    pub async fn open_with_bridge(
        core: Core,
        root: PathBuf,
        credentials: Arc<dyn Credentials>,
        factory: Arc<dyn ProviderFactory>,
        limits: Limits,
        bridge: Option<Bridge>,
    ) -> Result<Self> {
        tokio::fs::create_dir_all(root.join("conversations")).await?;
        let settings = tokio::fs::read(root.join("settings.json"))
            .await
            .ok()
            .and_then(|bytes| {
                if bytes.len() < 65536 {
                    serde_json::from_slice::<Settings>(&bytes).ok()
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let mut state = State::default();
        let mut entries = tokio::fs::read_dir(root.join("conversations")).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(workspace) = name.strip_suffix(".json") else {
                continue;
            };
            if valid_workspace(workspace).is_err() {
                continue;
            }
            let metadata = entry.metadata().await?;
            if !metadata.is_file() {
                continue;
            }
            if let Ok(bytes) = tokio::fs::read(entry.path()).await
                && let Ok(stored) = serde_json::from_slice::<StoredChats>(&bytes)
            {
                let book = match stored {
                    StoredChats::Book(book)
                        if !book.chats.is_empty()
                            && book.chats.iter().any(|chat| chat.id == book.active_id) =>
                    {
                        book
                    }
                    StoredChats::Book(_) => continue,
                    StoredChats::Legacy(messages) => {
                        let mut book = ChatBook::default();
                        let chat = book.current_mut();
                        chat.title = chat_title(&messages);
                        chat.messages = messages;
                        book
                    }
                };
                state.conversations.insert(workspace.into(), book);
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                core,
                root,
                settings: tokio::sync::Mutex::new(settings),
                credentials,
                factory,
                bridge,
                state: Mutex::new(state),
                events: broadcast::channel(256).0,
                limits,
                storage: tokio::sync::Mutex::new(()),
            }),
        })
    }
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.inner.events.subscribe()
    }
    pub async fn settings(&self) -> Result<Value> {
        let settings = self.inner.settings.lock().await.clone();
        let detections = cli::detect_all().await;
        let configured = if settings.backend.is_cli() {
            detections
                .iter()
                .find(|item| item.backend == settings.backend)
                .is_some_and(|item| item.installed && item.authenticated)
                && self.inner.bridge.is_some()
        } else if settings.model.is_empty() {
            false
        } else {
            let credentials = self.inner.credentials.clone();
            tokio::task::spawn_blocking(move || credentials.get().map(|key| key.is_some()))
                .await
                .map_err(|_| {
                    Error::domain(
                        "CREDENTIAL_UNAVAILABLE",
                        "Could not access the system keychain",
                    )
                })??
        };
        Ok(json!({
            "provider": settings.backend.as_str(),
            "backend": settings.backend.as_str(),
            "model": settings.model,
            "configured": configured,
            "credential_storage": if settings.backend.is_cli() {"local_cli"} else {"system_keychain"},
            "cli": detections,
            "bridge_available": self.inner.bridge.is_some(),
        }))
    }
    pub async fn configure(
        &self,
        backend: String,
        model: String,
        api_key: Option<String>,
    ) -> Result<Value> {
        let backend = Backend::parse(&backend)?;
        let model = model.trim().to_string();
        if backend == Backend::AnthropicApi {
            if model.is_empty() || model.len() > 128 || model.chars().any(char::is_control) {
                return Err(Error::domain("INVALID_PARAMETER", "Enter an API model ID"));
            }
        } else if model.len() > 128 || model.chars().any(char::is_control) {
            return Err(Error::domain("INVALID_PARAMETER", "Invalid model ID"));
        }
        if backend.is_cli() {
            if self.inner.bridge.is_none() {
                return Err(Error::domain(
                    "APP_UNAVAILABLE",
                    "CLI agents need the Cadpad MCP bridge",
                ));
            }
            let status = cli::detect(backend).await;
            if !status.installed || !status.authenticated {
                return Err(Error::domain("LLM_NOT_CONFIGURED", status.detail));
            }
        }
        let mut settings = self.inner.settings.lock().await;
        if backend == Backend::AnthropicApi {
            if let Some(key) = api_key {
                let secret = Secret::new(key)?;
                let credentials = self.inner.credentials.clone();
                tokio::task::spawn_blocking(move || credentials.set(secret))
                    .await
                    .map_err(|_| {
                        Error::domain("CREDENTIAL_UNAVAILABLE", "Could not save the API key")
                    })??;
            }
        }
        let next = Settings {
            backend,
            model: if backend.is_cli() && model.is_empty() {
                backend.label().into()
            } else {
                model
            },
        };
        write_json(&self.inner.root.join("settings.json"), &next).await?;
        *settings = next;
        drop(settings);
        self.settings().await
    }
    pub async fn forget_key(&self) -> Result<()> {
        let _settings = self.inner.settings.lock().await;
        let turns = self
            .inner
            .state
            .lock()
            .unwrap()
            .active
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for turn in turns {
            let _ = self.stop(&turn).await;
        }
        let credentials = self.inner.credentials.clone();
        tokio::task::spawn_blocking(move || credentials.delete())
            .await
            .map_err(|_| Error::domain("CREDENTIAL_UNAVAILABLE", "Could not remove the API key"))?
    }
    pub fn conversation(&self, workspace: &str, chat_id: Option<&str>) -> Result<Value> {
        valid_workspace(workspace)?;
        let mut state = self.inner.state.lock().unwrap();
        let live = state.active.values().filter_map(|id|state.turns.get(id)).filter(|turn|turn.workspace_id==workspace).cloned().collect::<Vec<_>>();
        let book = state.conversations.entry(workspace.into()).or_default();
        let chat = if let Some(id) = chat_id {
            book.chats
                .iter()
                .find(|chat| chat.id == id && !chat.archived)
                .ok_or_else(|| {
                    Error::domain(
                        "INVALID_REQUEST",
                        "This chat does not belong to the current project",
                    )
                })?
        } else {
            book.current()
        };
        let turn = live.into_iter()
            .find(|turn| turn.chat_id == chat.id)
            .or_else(|| chat.last_turn.clone());
        let mut chats = book.chats.iter().filter(|chat| !chat.archived && !chat.messages.is_empty()).map(|chat| json!({"id": chat.id, "title": chat.title, "created_at": chat.created_at, "updated_at": chat.updated_at, "message_count": chat.messages.len()})).collect::<Vec<_>>();
        chats
            .sort_by_key(|chat| std::cmp::Reverse(chat["updated_at"].as_u64().unwrap_or_default()));
        Ok(
            json!({"messages": chat.messages, "turn": turn, "active_chat_id": book.active_id, "chats": chats}),
        )
    }
    /// Switch history only while idle. Publish the selection after the file is saved.
    pub async fn change_chat(
        &self,
        workspace: &str,
        action: &str,
        chat_id: Option<&str>,
    ) -> Result<Value> {
        valid_workspace(workspace)?;
        if !self.inner.core.has_workspace(workspace).await {
            return Err(Error::domain("APP_UNAVAILABLE", "Unknown workspace"));
        }
        let _storage = self.inner.storage.lock().await;
        let next = {
            let mut state = self.inner.state.lock().unwrap();
            if state.changing_chats.contains(workspace) || (!matches!(action,"new"|"select") && state.active.values().filter_map(|id|state.turns.get(id)).any(|turn| turn.workspace_id==workspace && chat_id.is_none_or(|id|turn.chat_id==id))) {
                return Err(Error::domain(
                    "AGENT_BUSY",
                    "Wait for the assistant to finish or stop it before changing chats",
                ));
            }
            let mut book = state
                .conversations
                .get(workspace)
                .cloned()
                .unwrap_or_default();
            match action {
                "new" => {
                    // Reuse an empty chat instead of filling history with blank entries.
                    if !book.current().messages.is_empty() {
                        if let Some(empty) = book
                            .chats
                            .iter()
                            .find(|chat| !chat.archived && chat.messages.is_empty())
                        {
                            book.active_id = empty.id.clone();
                        } else {
                            let chat = Chat::default();
                            book.active_id = chat.id.clone();
                            book.chats.push(chat);
                        }
                    }
                }
                "select" => {
                    let id =
                        chat_id.ok_or_else(|| Error::domain("INVALID_REQUEST", "Choose a chat"))?;
                    if !book
                        .chats
                        .iter()
                        .any(|chat| chat.id == id && !chat.archived)
                    {
                        return Err(Error::domain(
                            "INVALID_REQUEST",
                            "This chat does not belong to the current project",
                        ));
                    }
                    book.active_id = id.into();
                }
                "remove" | "restore" => {
                    let id =
                        chat_id.ok_or_else(|| Error::domain("INVALID_REQUEST", "Choose a chat"))?;
                    let chat = book
                        .chats
                        .iter_mut()
                        .find(|chat| chat.id == id)
                        .ok_or_else(|| {
                            Error::domain(
                                "INVALID_REQUEST",
                                "This chat does not belong to the current project",
                            )
                        })?;
                    chat.archived = action == "remove";
                    if action == "remove" && book.active_id == id {
                        if let Some(empty) = book
                            .chats
                            .iter()
                            .find(|chat| !chat.archived && chat.messages.is_empty())
                        {
                            book.active_id = empty.id.clone();
                        } else {
                            let empty = Chat::default();
                            book.active_id = empty.id.clone();
                            book.chats.push(empty);
                        }
                    }
                }
                action if action.starts_with("rewind:") => {
                    let index = action[7..].parse::<usize>().map_err(|_| Error::domain("INVALID_REQUEST", "Invalid message"))?;
                    let id = chat_id.ok_or_else(|| Error::domain("INVALID_REQUEST", "Choose a chat"))?;
                    let chat = book.chats.iter_mut().find(|chat| chat.id == id && !chat.archived)
                        .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown chat"))?;
                    if !chat.messages.get(index).is_some_and(|message| message.role == "user") {
                        return Err(Error::domain("INVALID_REQUEST", "Choose a user message"));
                    }
                    chat.messages.truncate(index);
                    chat.last_turn = None;
                    chat.updated_at = now();
                    book.active_id = id.into();
                }
                _ => return Err(Error::domain("INVALID_REQUEST", "Unknown chat action")),
            }
            state.changing_chats.insert(workspace.into());
            book
        };
        let saved = write_json(
            &self
                .inner
                .root
                .join("conversations")
                .join(format!("{workspace}.json")),
            &next,
        )
        .await;
        {
            let mut state = self.inner.state.lock().unwrap();
            state.changing_chats.remove(workspace);
            if saved.is_ok() {
                state.conversations.insert(workspace.into(), next);
            }
        }
        saved?;
        self.conversation(workspace, None)
    }
    pub fn turn(&self, id: &str) -> Result<Turn> {
        self.inner
            .state
            .lock()
            .unwrap()
            .turns
            .get(id)
            .cloned()
            .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown agent turn"))
    }
    pub async fn start(&self, workspace: String, prompt: String) -> Result<Turn> {
        self.start_with(workspace, prompt, Attachments::default())
            .await
    }
    /// Starts a turn with viewport references. They are validated against the
    /// committed revision now, so a stale selection fails before any work starts.
    pub async fn start_with(
        &self,
        workspace: String,
        prompt: String,
        attachments: Attachments,
    ) -> Result<Turn> {
        self.start_in_chat(workspace, prompt, attachments, None)
            .await
    }
    pub async fn start_in_chat(
        &self,
        workspace: String,
        prompt: String,
        attachments: Attachments,
        expected_chat: Option<String>,
    ) -> Result<Turn> {
        valid_workspace(&workspace)?;
        references::validate_image(&attachments.image)?;
        if prompt.trim().is_empty() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Enter a request",
            ));
        }
        if !self.inner.core.has_workspace(&workspace).await {
            return Err(Error::domain("APP_UNAVAILABLE", "Unknown workspace"));
        }
        let checkpoint = self.inner.core.get_project(&workspace).await?["revision"].as_str().map(str::to_owned);
        let chips = if attachments.references.is_empty() {
            vec![]
        } else {
            let project = self.inner.core.get_project(&workspace).await?;
            references::summarize(&project, &attachments.references)?
        };
        let settings = self.inner.settings.lock().await.clone();
        if settings.backend.is_cli() {
            if self.inner.bridge.is_none() {
                return Err(Error::domain(
                    "APP_UNAVAILABLE",
                    "CLI agents need the Cadpad MCP bridge",
                ));
            }
        } else if settings.model.is_empty() {
            return Err(Error::domain(
                "LLM_NOT_CONFIGURED",
                "Choose a provider in the agent settings",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let actor = format!("builtin:{id}");
        let mut turn = Turn {
            id: id.clone(),
            workspace_id: workspace.clone(),
            chat_id: String::new(),
            actor,
            state: TurnState::Running,
            text: String::new(),
            activity: if settings.backend.is_cli() {
                format!("Starting {}", settings.backend.label())
            } else {
                "Connecting to the model".into()
            },
            error: None,
            tool_calls: 0,
            rounds: 0,
            artifacts: vec![],
            started_at: now(),
            steps: vec![],
            text_open: false,
        };
        let (history, previous_chat) = {
            let mut state = self.inner.state.lock().unwrap();
            let target_chat = expected_chat.as_deref().or_else(||state.conversations.get(&workspace).map(|book|book.active_id.as_str()));
            if state.changing_chats.contains(&workspace) || state.active.values().filter_map(|id|state.turns.get(id)).any(|turn| turn.workspace_id==workspace && Some(turn.chat_id.as_str())==target_chat) {
                return Err(Error::domain(
                    "AGENT_BUSY",
                    "Stop or wait for the current agent turn in this workspace",
                ));
            }
            if state.active.len() >= 4 {
                return Err(Error::domain(
                    "AGENT_BUSY",
                    "Four agent turns are already running; stop or wait for one to finish",
                ));
            }
            while state.turns.len() >= 128 {
                let retired = state
                    .turns
                    .values()
                    .filter(|turn| {
                        matches!(
                            turn.state,
                            TurnState::Succeeded | TurnState::Failed | TurnState::Cancelled
                        )
                    })
                    .min_by_key(|turn| turn.started_at)
                    .map(|turn| turn.id.clone());
                if let Some(id) = retired {
                    state.turns.remove(&id);
                } else {
                    break;
                }
            }
            let book = state
                .conversations
                .entry(workspace.clone())
                .or_default();
            if let Some(id) = expected_chat.as_ref() {
                if !book
                    .chats
                    .iter()
                    .any(|chat| chat.id == *id && !chat.archived)
                {
                    return Err(Error::domain(
                        "STALE_CHAT",
                        "The selected chat changed. Open it again before sending.",
                    ));
                }
                book.active_id = id.clone();
            }
            let chat = book.current_mut();
            turn.chat_id = chat.id.clone();
            let previous_chat = chat.clone();
            let history = chat.messages.clone();
            chat.messages.push(ChatMessage {
                checkpoint_revision: checkpoint,
                image: attachments.image.clone(),
                role: "user".into(),
                text: prompt.clone(),
                references: chips.clone(),
                steps: vec![],
            });
            trim_history(&mut chat.messages);
            if history.is_empty() {
                chat.title = chat_title(&chat.messages);
            }
            chat.updated_at = now();
            state.active.insert(id.clone(), id.clone());
            state.turns.insert(id.clone(), turn.clone());
            (history, previous_chat)
        };
        if let Err(error) = self.persist_conversation(&workspace).await {
            let mut state = self.inner.state.lock().unwrap();
            state.active.remove(&id);
            state.turns.remove(&id);
            *state
                .conversations
                .get_mut(&workspace)
                .unwrap()
                .current_mut() = previous_chat;
            return Err(error);
        }
        self.emit(&turn, "turn_started", None);
        let service = self.clone();
        let task_turn = turn.clone();
        let pending = PendingReferences {
            entities: attachments.references,
            chips,
            image: attachments.image,
        };
        tokio::spawn(async move {
            service
                .execute(task_turn, prompt, history, settings, pending)
                .await;
        });
        Ok(turn)
    }
    fn emit(&self, turn: &Turn, kind: &str, delta: Option<String>) {
        let _ = self.inner.events.send(AgentEvent {
            workspace_id: turn.workspace_id.clone(),
            turn_id: turn.id.clone(),
            kind: kind.into(),
            delta,
            turn: if kind == "text_delta" {
                None
            } else {
                Some(turn.clone())
            },
        });
    }
    fn progress(&self, turn_id: &str, progress: Progress) {
        let mut state = self.inner.state.lock().unwrap();
        let Some(turn) = state.turns.get_mut(turn_id) else {
            return;
        };
        match progress {
            Progress::Text(delta) => {
                let room = 65536usize.saturating_sub(turn.text.len());
                let mut end = delta.len().min(room);
                while !delta.is_char_boundary(end) {
                    end -= 1;
                }
                let delta = delta[..end].to_string();
                if delta.is_empty() {
                    return;
                }
                // Deltas extend the open text step; anything else starts a new
                // segment, separated from the previous one by a blank line.
                let extends =
                    turn.text_open && matches!(turn.steps.last(), Some(Step::Text { .. }));
                if extends {
                    if let Some(Step::Text { text }) = turn.steps.last_mut() {
                        text.push_str(&delta);
                    }
                    turn.text.push_str(&delta);
                    self.emit(turn, "text_delta", Some(delta));
                } else {
                    close_thinking(turn);
                    if !turn.text.is_empty() {
                        turn.text.push_str("\n\n");
                    }
                    turn.text.push_str(&delta);
                    push_step(turn, Step::Text { text: delta });
                    turn.text_open = true;
                    turn.activity = "Writing".into();
                    self.emit(turn, "step", None);
                }
            }
            Progress::TextBreak => {
                turn.text_open = false;
            }
            Progress::Thinking(delta) => {
                if let Some(Step::Thinking { text, done: false }) = turn.steps.last_mut() {
                    if text.len() < MAX_THINKING_BYTES {
                        text.push_str(&delta);
                        self.emit(turn, "thinking_delta", Some(delta));
                    }
                } else {
                    turn.text_open = false;
                    push_step(
                        turn,
                        Step::Thinking {
                            text: delta,
                            done: false,
                        },
                    );
                    turn.activity = "Thinking".into();
                    self.emit(turn, "step", None);
                }
            }
            Progress::ThinkingDone => {
                if close_thinking(turn) {
                    self.emit(turn, "step", None);
                }
            }
            Progress::ToolStarted { id, name, detail } => {
                turn.tool_calls += 1;
                turn.text_open = false;
                close_thinking(turn);
                let label = activity(&name);
                turn.activity = label.clone();
                push_step(
                    turn,
                    Step::Tool {
                        id,
                        name,
                        label,
                        detail: detail.filter(|detail| !detail.trim().is_empty()),
                        state: ToolState::Running,
                    },
                );
                self.emit(turn, "tool_started", None);
            }
            Progress::ToolFinished { id, name, error } => {
                let target = turn.steps.iter_mut().rev().find(|step| {
                    matches!(step, Step::Tool { id: step_id, name: step_name, state: ToolState::Running, .. }
                        if id.is_some() && step_id == &id || id.is_none() && step_name == &name)
                });
                if let Some(Step::Tool { state, .. }) = target {
                    *state = if error {
                        ToolState::Failed
                    } else {
                        ToolState::Done
                    };
                }
                turn.activity = if error {
                    format!("Reviewing {} feedback", activity(&name).to_lowercase())
                } else {
                    "Checking the result".into()
                };
                self.emit(turn, "tool_finished", None);
            }
            Progress::Artifact(artifact) => {
                turn.artifacts.push(artifact);
                self.emit(turn, "artifact", None);
            }
        }
    }
    async fn execute(
        &self,
        turn: Turn,
        prompt: String,
        history: Vec<ChatMessage>,
        settings: Settings,
        pending: PendingReferences,
    ) {
        let timed_out = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let expired = timed_out.clone();
        let deadline_core = self.inner.core.clone();
        let actor = turn.actor.clone();
        let timeout = self.inner.limits.turn_timeout;
        let timer = tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            expired.store(true, std::sync::atomic::Ordering::SeqCst);
            deadline_core.revoke_actor(&actor).await;
        });
        let result = async {
            self.inner.core.ensure_actor_active(&turn.actor)?;
            let attachment = self
                .resolve_references(&turn, pending, !settings.backend.is_cli())
                .await?;
            let prompt = if settings.backend.is_cli() && !attachment.text.is_empty() {
                format!("{}\n{}", attachment.text, prompt)
            } else {
                prompt
            };
            if settings.backend.is_cli() {
                let bridge = self.inner.bridge.clone().ok_or_else(|| {
                    Error::domain("APP_UNAVAILABLE", "CLI agents need the Cadpad MCP bridge")
                })?;
                let mut context = history.iter().rev().take(20).collect::<Vec<_>>();
                context.reverse();
                let transcript = context.into_iter().map(|message| {
                    format!("{}: {}", message.role, message.text)
                }).collect::<Vec<_>>().join("\n\n");
                let prompt = if transcript.is_empty() { prompt } else {
                    format!("Prior conversation (context only; follow the latest user message):\n{}\n\nLatest user message:\n{}", transcript, prompt)
                };
                let project_directory = self.inner.core.project_directory(&turn.workspace_id).await.ok();
                let launch = cli::spawn(cli::LaunchRequest {
                    backend: settings.backend,
                    bridge: &bridge,
                    workspace_id: &turn.workspace_id,
                    project_directory: project_directory.as_deref(),
                    prompt: &prompt,
                    client_name: &format!("Cadpad · {}", settings.backend.label()),
                })
                .await?;
                let service = self.clone();
                let id = turn.id.clone();
                let cancel = self.inner.core.actor_signal(&turn.actor);
                cli::drive(launch, settings.backend, cancel, move |event| {
                    let progress = match event {
                        StreamEvent::Text(text) => Progress::Text(text),
                        StreamEvent::TextBreak => Progress::TextBreak,
                        StreamEvent::Thinking(text) => Progress::Thinking(text),
                        StreamEvent::ThinkingDone => Progress::ThinkingDone,
                        StreamEvent::ToolStarted { id, name, detail } => {
                            Progress::ToolStarted { id, name, detail }
                        }
                        StreamEvent::ToolFinished { id, name, error } => {
                            Progress::ToolFinished { id, name, error }
                        }
                        StreamEvent::Done => return,
                    };
                    service.progress(&id, progress);
                })
                .await?;
                // Snapshot once. Mutex guards created in struct fields live
                // through the whole statement; locking again for tool_calls
                // would deadlock after the CLI has already finished.
                let completed = self.turn(&turn.id)?;
                Ok(runner::RunOutcome {
                    text: completed.text,
                    rounds: 1,
                    tool_calls: completed.tool_calls,
                    artifacts: vec![],
                })
            } else {
                let credentials = self.inner.credentials.clone();
                let read_key = tokio::task::spawn_blocking(move || credentials.get());
                let mut cancelled = self.inner.core.actor_signal(&turn.actor);
                let key = tokio::select! {
                    key = read_key => key.map_err(|_| Error::domain("CREDENTIAL_UNAVAILABLE", "Could not access the system keychain"))??,
                    _ = cancelled.changed() => return Err(Error::domain("CANCELLED", "Agent stopped while reading its credential")),
                }
                .ok_or_else(|| {
                    Error::domain(
                        "LLM_NOT_CONFIGURED",
                        "Add your Anthropic API key in the agent settings",
                    )
                })?;
                self.inner.core.ensure_actor_active(&turn.actor)?;
                let provider = self.inner.factory.create(&settings, key)?;
                let service = self.clone();
                let id = turn.id.clone();
                let emit: EmitProgress = Arc::new(move |progress| service.progress(&id, progress));
                runner::run(
                    self.inner.core.clone(),
                    runner::RunInput {
                        workspace: turn.workspace_id.clone(),
                        actor: turn.actor.clone(),
                        prompt,
                        history,
                        attachment,
                    },
                    provider,
                    self.inner.limits.clone(),
                    emit,
                )
                .await
            }
        }
        .await;
        timer.abort();
        let result = if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
            Err(Error::domain(
                "AGENT_LIMIT",
                "Agent turn reached its time limit; pending work was cancelled",
            ))
        } else {
            result
        };
        self.inner.core.revoke_actor(&turn.actor).await;
        // No terminal cancellation state before all owned native jobs stop.
        loop {
            let jobs = self.inner.core.actor_jobs(&turn.actor).await;
            if jobs
                .iter()
                .all(|(_, job)| !matches!(job.state, JobState::Queued | JobState::Running))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let finished = {
            let mut state = self.inner.state.lock().unwrap();
            let stored = state.turns.get_mut(&turn.id).unwrap();
            let cancelling = stored.state == TurnState::Cancelling;
            match result {
                Ok(outcome) if !cancelling => {
                    stored.state = TurnState::Succeeded;
                    stored.text = outcome.text;
                    stored.rounds = outcome.rounds;
                    stored.tool_calls = outcome.tool_calls;
                    stored.artifacts = outcome.artifacts;
                    stored.activity = "Finished".into();
                }
                Ok(_) => {
                    stored.state = TurnState::Cancelled;
                    stored.activity = "Stopped".into();
                }
                Err(error) => {
                    stored.state = if cancelling || error.code() == "CANCELLED" {
                        TurnState::Cancelled
                    } else {
                        TurnState::Failed
                    };
                    stored.error = Some(error_value(&error));
                    stored.activity = if stored.state == TurnState::Cancelled {
                        "Stopped"
                    } else {
                        "Needs attention"
                    }
                    .into();
                }
            }
            // Nothing is still running once the turn is terminal.
            close_thinking(stored);
            for step in &mut stored.steps {
                if let Step::Tool { state, .. } = step
                    && *state == ToolState::Running
                {
                    *state = if stored.state == TurnState::Succeeded {
                        ToolState::Done
                    } else {
                        ToolState::Failed
                    };
                }
            }
            let finished = stored.clone();
            let chat = state
                .conversations
                .get_mut(&turn.workspace_id)
                .unwrap()
                .chats
                .iter_mut()
                .find(|chat| chat.id == turn.chat_id)
                .expect("turn chat exists");
            chat.updated_at = now();
            chat.last_turn = Some(finished.clone());
            let messages = &mut chat.messages;
            let text = if finished.text.is_empty() {
                finished
                    .error
                    .as_ref()
                    .and_then(|error| error["message"].as_str())
                    .unwrap_or("Stopped.")
                    .to_string()
            } else {
                finished.text.clone()
            };
            messages.push(ChatMessage {
                checkpoint_revision: None,
                image: None,
                role: "assistant".into(),
                text,
                references: vec![],
                steps: history_steps(&finished.steps),
            });
            trim_history(messages);
            finished
        };
        if let Err(error) = self.persist_conversation(&turn.workspace_id).await
            && let Some(stored) = self.inner.state.lock().unwrap().turns.get_mut(&turn.id)
        {
            stored.error = Some(error_value(&error));
        }
        self.inner
            .state
            .lock()
            .unwrap()
            .active
            .remove(&turn.id);
        let finished = self.turn(&finished.id).unwrap_or(finished);
        self.emit(&finished, "turn_finished", None);
    }
    async fn persist_conversation(&self, workspace: &str) -> Result<()> {
        let _guard = self.inner.storage.lock().await;
        let book = self
            .inner
            .state
            .lock()
            .unwrap()
            .conversations
            .get(workspace)
            .cloned()
            .unwrap_or_default();
        write_json(
            &self
                .inner
                .root
                .join("conversations")
                .join(format!("{workspace}.json")),
            &book,
        )
        .await
    }
    pub async fn stop(&self, id: &str) -> Result<Turn> {
        let turn = {
            let mut state = self.inner.state.lock().unwrap();
            let turn = state
                .turns
                .get_mut(id)
                .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown agent turn"))?;
            if !matches!(turn.state, TurnState::Running | TurnState::Cancelling) {
                return Ok(turn.clone());
            }
            if matches!(turn.state, TurnState::Running | TurnState::Cancelling) {
                turn.state = TurnState::Cancelling;
                turn.activity = "Stopping…".into();
            }
            turn.clone()
        };
        self.emit(&turn, "turn_cancelling", None);
        self.inner.core.revoke_actor(&turn.actor).await;
        Ok(turn)
    }
}
struct PendingReferences {
    entities: Vec<cadpad_core::tools::Entity>,
    chips: Vec<ChatReference>,
    image: Option<String>,
}
impl AgentService {
    async fn resolve_references(
        &self,
        turn: &Turn,
        pending: PendingReferences,
        image_supported: bool,
    ) -> Result<references::Resolved> {
        let Some(first) = pending.entities.first() else {
            // User-dropped reference images still reach the model when nothing is selected.
            let image = if image_supported { pending.image } else { None };
            return Ok(references::Resolved {
                text: if image.is_some() {
                    "The user attached a reference image to the message below.\n".into()
                } else {
                    String::new()
                },
                image,
            });
        };
        self.progress(
            &turn.id,
            Progress::ToolStarted {
                id: Some("references".into()),
                name: "references".into(),
                detail: None,
            },
        );
        let project = self.inner.core.get_project(&turn.workspace_id).await?;
        let pose_version = project["motion"]["pose_version"].as_u64().unwrap_or(0);
        let tools = runner::CoreTools {
            core: self.inner.core.clone(),
            workspace: turn.workspace_id.clone(),
            actor: turn.actor.clone(),
        };
        let resolved = references::resolve(
            &tools,
            references::Request {
                workspace: &turn.workspace_id,
                revision: &first.revision,
                pose_version,
                entities: &pending.entities,
                chips: &pending.chips,
                image: pending.image,
                image_supported,
            },
        )
        .await;
        self.progress(
            &turn.id,
            Progress::ToolFinished {
                id: Some("references".into()),
                name: "references".into(),
                error: resolved.is_err(),
            },
        );
        resolved
    }
}
fn trim_history(messages: &mut Vec<ChatMessage>) {
    while messages.len() > 100
        || messages
            .iter()
            .map(|message| message.text.len())
            .sum::<usize>()
            > 256 * 1024
    {
        messages.remove(0);
    }
}
const MAX_THINKING_BYTES: usize = 16 * 1024;
const MAX_STEPS: usize = 300;

fn push_step(turn: &mut Turn, step: Step) {
    if turn.steps.len() < MAX_STEPS {
        turn.steps.push(step);
    }
}

/// Marks an open thinking step finished; returns whether one was open.
fn close_thinking(turn: &mut Turn) -> bool {
    match turn.steps.last_mut() {
        Some(Step::Thinking { done, .. }) if !*done => {
            *done = true;
            true
        }
        _ => false,
    }
}

/// Steps saved with a finished message. Thinking is shortened; text and tools
/// are kept so the conversation shows what happened in order.
fn history_steps(steps: &[Step]) -> Vec<Step> {
    steps
        .iter()
        .map(|step| match step {
            Step::Thinking { text, done } if text.len() > 2048 => {
                let mut start = text.len() - 2048;
                while !text.is_char_boundary(start) {
                    start += 1;
                }
                Step::Thinking {
                    text: format!("…{}", &text[start..]),
                    done: *done,
                }
            }
            other => other.clone(),
        })
        .collect()
}

/// Plain-language hint for a tool call, used while it runs and in history.
fn activity(tool: &str) -> String {
    let label = match tool {
        "get_project" => "Reading the project",
        "read_source" => "Reading the source",
        "update_source" => "Updating the design",
        "build" => "Building geometry",
        "get_job" => "Waiting for the build",
        "cancel_job" => "Cancelling a build",
        "inspect" => "Measuring geometry",
        "get_selection" => "Looking at your selection",
        "references" => "Reading your selection",
        "capture_view" => "Looking at the view",
        "check_interference" => "Checking clearances",
        "set_parameter" => "Changing a parameter",
        "export" => "Preparing an export",
        "undo" => "Restoring the model",
        "set_dimensions" => "Showing dimensions",
        "set_annotation" => "Saving a dimension",
        "delete_annotation" => "Removing a dimension",
        "define_mechanism" => "Defining the mechanism",
        "get_motion" => "Reading the mechanism",
        "set_joint_coordinate" => "Moving a joint",
        "control_motion_tests" => "Working on motion tests",
        "control_playback" => "Controlling playback",
        "tool_schema" => "Looking up Cadpad tools",
        "cli:command" => "Running a command",
        "cursor:read" => "Reading a file",
        "cursor:grep" | "cursor:glob" | "cursor:ls" => "Searching files",
        "cursor:edit" | "cursor:write" => "Editing a file",
        "cursor:shell" => "Running a command",
        other => {
            let name = other.rsplit(':').next().unwrap_or(other).replace('_', " ");
            return if name.is_empty() {
                "Working".into()
            } else {
                format!("Using {name}")
            };
        }
    };
    label.into()
}

fn chat_title(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .find(|message| message.role == "user")
        .map(|message| {
            message
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(72)
                .collect()
        })
        .filter(|title: &String| !title.is_empty())
        .unwrap_or_else(|| "New chat".into())
}
