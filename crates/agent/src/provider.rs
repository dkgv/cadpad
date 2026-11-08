use crate::credentials::Secret;
use async_trait::async_trait;
use cadpad_core::{Error, Result, tools::ToolSpec};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::watch;

pub type EmitText = Arc<dyn Fn(String) + Send + Sync>;
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Native {
        value: Value,
    },
    ToolResult {
        tool_use_id: String,
        result: Value,
        is_error: bool,
    },
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}
impl Message {
    pub fn text(role: Role, text: String) -> Self {
        Self {
            role,
            content: vec![Block::Text { text }],
        }
    }
}
#[derive(Clone)]
pub struct ProviderRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
}
#[derive(Clone, Debug)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
}
#[derive(Clone, Debug)]
pub struct ProviderReply {
    pub content: Vec<Block>,
    pub stop: StopReason,
}
#[async_trait]
pub trait Provider: Send + Sync {
    async fn respond(
        &self,
        request: ProviderRequest,
        cancel: watch::Receiver<bool>,
        emit: EmitText,
    ) -> Result<ProviderReply>;
}

pub struct Anthropic {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    model: String,
    key: Secret,
}
impl Anthropic {
    pub fn new(model: String, key: Secret) -> Result<Self> {
        Self::with_endpoint("https://api.anthropic.com/v1/messages", model, key)
    }
    /// Only the official API and loopback test servers are accepted. This endpoint
    /// is not configurable by project Python, tools, or desktop settings.
    pub fn with_endpoint(endpoint: &str, model: String, key: Secret) -> Result<Self> {
        let endpoint = reqwest::Url::parse(endpoint)
            .map_err(|_| Error::domain("INVALID_PARAMETER", "Invalid provider endpoint"))?;
        let official = endpoint.scheme() == "https"
            && endpoint.host_str() == Some("api.anthropic.com")
            && endpoint.port_or_known_default() == Some(443);
        let local = endpoint.scheme() == "http"
            && matches!(endpoint.host_str(), Some("127.0.0.1" | "[::1]" | "::1"));
        if (!official && !local) || !endpoint.username().is_empty() || endpoint.password().is_some()
        {
            return Err(Error::domain(
                "INVALID_PARAMETER",
                "Provider endpoint must be the official HTTPS API or a loopback test server",
            ));
        }
        if model.is_empty() || model.len() > 128 || model.chars().any(char::is_control) {
            return Err(Error::domain("INVALID_PARAMETER", "Enter a model ID"));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|_| {
                Error::domain(
                    "LLM_UNAVAILABLE",
                    "Could not initialize the provider connection",
                )
            })?;
        Ok(Self {
            client,
            endpoint,
            model,
            key,
        })
    }
    pub fn body(&self, request: &ProviderRequest) -> Value {
        let messages=request.messages.iter().map(|message|json!({"role":match message.role{Role::User=>"user",Role::Assistant=>"assistant"},"content":message.content.iter().map(wire_block).collect::<Vec<_>>()})).collect::<Vec<_>>();
        json!({"model":self.model,"max_tokens":request.max_tokens,"system":request.system,"messages":messages,"tools":request.tools.iter().map(|tool|json!({"name":tool.name,"description":tool.description,"input_schema":tool.input_schema})).collect::<Vec<_>>(),"stream":true})
    }
}
fn wire_block(block: &Block) -> Value {
    match block {
        Block::Text { text } => json!({"type":"text","text":text}),
        Block::Native { value } => value.clone(),
        Block::ToolUse { id, name, input } => {
            json!({"type":"tool_use","id":id,"name":name,"input":input})
        }
        Block::ToolResult {
            tool_use_id,
            result,
            is_error,
        } => {
            let mut result = result.clone();
            let image = result
                .as_object_mut()
                .and_then(|object| object.remove("image"));
            let mut content =
                vec![json!({"type":"text","text":serde_json::to_string(&result).unwrap()})];
            if let Some(image) = image {
                content.push(json!({"type":"image","source":{"type":"base64","media_type":image["mime_type"],"data":image["data"]}}));
            }
            json!({"type":"tool_result","tool_use_id":tool_use_id,"content":content,"is_error":is_error})
        }
    }
}
#[async_trait]
impl Provider for Anthropic {
    async fn respond(
        &self,
        request: ProviderRequest,
        mut cancel: watch::Receiver<bool>,
        emit: EmitText,
    ) -> Result<ProviderReply> {
        if *cancel.borrow() {
            return Err(Error::domain("CANCELLED", "Agent stopped"));
        }
        let mut key = reqwest::header::HeaderValue::from_str(self.key.expose()).map_err(|_| {
            Error::domain(
                "INVALID_PARAMETER",
                "API key contains invalid header characters",
            )
        })?;
        key.set_sensitive(true);
        let response = tokio::select! {
            response=self.client.post(self.endpoint.clone()).header("x-api-key",key).header("anthropic-version","2023-06-01").json(&self.body(&request)).send()=>response.map_err(|_|Error::domain("LLM_UNAVAILABLE","Provider connection failed or timed out"))?,
            _=cancel.changed()=>return Err(Error::domain("CANCELLED","Agent stopped")),
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            return Err(Error::domain(
                if matches!(status, 401 | 403) {
                    "LLM_AUTHENTICATION_FAILED"
                } else if status == 429 {
                    "LLM_RATE_LIMITED"
                } else {
                    "LLM_UNAVAILABLE"
                },
                format!(
                    "Provider returned HTTP {status}. Check the API key, model access and account limits."
                ),
            ));
        }
        if !response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"))
        {
            return Err(Error::domain(
                "LLM_PROTOCOL_ERROR",
                "Provider did not return an event stream",
            ));
        }
        let mut stream = response.bytes_stream();
        let mut decoder = EventDecoder::default();
        let mut message = MessageAccumulator::default();
        let mut total = 0;
        loop {
            let next = tokio::select! {next=stream.next()=>next,_=cancel.changed()=>return Err(Error::domain("CANCELLED","Agent stopped"))};
            let Some(chunk) = next else {
                break;
            };
            let chunk = chunk
                .map_err(|_| Error::domain("LLM_UNAVAILABLE", "Provider stream was interrupted"))?;
            total += chunk.len();
            if total > 2 * 1024 * 1024 {
                return Err(Error::domain(
                    "AGENT_LIMIT",
                    "Provider response exceeded 2 MiB",
                ));
            }
            for event in decoder.push(&chunk)? {
                message.event(event, &emit)?;
            }
            if message.stopped {
                return message.finish();
            }
        }
        Err(Error::domain(
            "LLM_PROTOCOL_ERROR",
            "Provider stream ended without message_stop; no partial tool calls were executed",
        ))
    }
}
/// SSE framing is independent of TCP chunks, CRLF, and UTF-8 code-point splits.
#[derive(Default)]
pub struct EventDecoder {
    buffer: Vec<u8>,
}
impl EventDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>> {
        self.buffer.extend_from_slice(bytes);
        let mut events = vec![];
        loop {
            let boundary = [
                self.buffer
                    .windows(2)
                    .position(|bytes| bytes == b"\n\n")
                    .map(|index| (index, 2)),
                self.buffer
                    .windows(4)
                    .position(|bytes| bytes == b"\r\n\r\n")
                    .map(|index| (index, 4)),
            ]
            .into_iter()
            .flatten()
            .min_by_key(|(index, _)| *index);
            let Some((index, separator)) = boundary else {
                break;
            };
            if index > 512 * 1024 {
                return Err(Error::domain(
                    "AGENT_LIMIT",
                    "Provider event exceeded 512 KiB",
                ));
            }
            let frame = std::str::from_utf8(&self.buffer[..index]).map_err(|_| {
                Error::domain(
                    "LLM_PROTOCOL_ERROR",
                    "Provider stream contained invalid UTF-8",
                )
            })?;
            let data = frame
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("data:")
                        .map(|value| value.strip_prefix(' ').unwrap_or(value))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !data.is_empty() {
                events.push(serde_json::from_str(&data).map_err(|_| {
                    Error::domain(
                        "LLM_PROTOCOL_ERROR",
                        "Provider event contained invalid JSON",
                    )
                })?);
            }
            self.buffer.drain(..index + separator);
        }
        if self.buffer.len() > 512 * 1024 {
            return Err(Error::domain(
                "AGENT_LIMIT",
                "Provider event exceeded 512 KiB",
            ));
        }
        Ok(events)
    }
}
#[derive(Default)]
struct MessageAccumulator {
    blocks: BTreeMap<u64, (Block, String, bool)>,
    stop: Option<StopReason>,
    stopped: bool,
    started: bool,
}
impl MessageAccumulator {
    fn event(&mut self, event: Value, emit: &EmitText) -> Result<()> {
        if self.stopped {
            return Err(protocol());
        }
        let kind = event["type"].as_str().unwrap_or("");
        if kind != "message_start" && kind != "ping" && kind != "error" && !self.started {
            return Err(protocol());
        }
        match kind {
            "message_start" => {
                if self.started {
                    return Err(protocol());
                }
                self.started = true;
            }
            "ping" => {}
            "content_block_start" => {
                let index = event["index"].as_u64().ok_or_else(protocol)?;
                if index != self.blocks.len() as u64
                    || self.blocks.contains_key(&index)
                    || self.blocks.len() >= 32
                {
                    return Err(protocol());
                }
                let block = &event["content_block"];
                let content = match block["type"].as_str() {
                    Some("text") => Block::Text {
                        text: block["text"].as_str().unwrap_or("").into(),
                    },
                    Some("tool_use") => Block::ToolUse {
                        id: block["id"].as_str().ok_or_else(protocol)?.into(),
                        name: block["name"].as_str().ok_or_else(protocol)?.into(),
                        input: block["input"].clone(),
                    },
                    Some("thinking" | "redacted_thinking") => Block::Native {
                        value: block.clone(),
                    },
                    _ => {
                        return Err(Error::domain(
                            "LLM_PROTOCOL_ERROR",
                            "Provider returned an unsupported content block",
                        ));
                    }
                };
                if let Block::Text { text } = &content
                    && !text.is_empty()
                {
                    emit(text.clone());
                }
                self.blocks.insert(index, (content, String::new(), false));
            }
            "content_block_delta" => {
                let index = event["index"].as_u64().ok_or_else(protocol)?;
                let (block, json, closed) = self.blocks.get_mut(&index).ok_or_else(protocol)?;
                if *closed {
                    return Err(protocol());
                }
                match (block, event["delta"]["type"].as_str()) {
                    (Block::Text { text }, Some("text_delta")) => {
                        let delta = event["delta"]["text"].as_str().ok_or_else(protocol)?;
                        if text.len() + delta.len() > 65536 {
                            return Err(Error::domain(
                                "AGENT_LIMIT",
                                "Assistant text exceeded 64 KiB",
                            ));
                        }
                        text.push_str(delta);
                        emit(delta.into());
                    }
                    (Block::ToolUse { .. }, Some("input_json_delta")) => {
                        json.push_str(
                            event["delta"]["partial_json"]
                                .as_str()
                                .ok_or_else(protocol)?,
                        );
                        if json.len() > 1024 * 1024 {
                            return Err(Error::domain("AGENT_LIMIT", "Tool input exceeded 1 MiB"));
                        }
                    }
                    (
                        Block::Native { value },
                        Some(kind @ ("thinking_delta" | "signature_delta")),
                    ) => {
                        let field = if kind == "thinking_delta" {
                            "thinking"
                        } else {
                            "signature"
                        };
                        let delta = event["delta"][field].as_str().ok_or_else(protocol)?;
                        let mut text = value[field].as_str().unwrap_or("").to_string();
                        if text.len() + delta.len() > 512 * 1024 {
                            return Err(Error::domain(
                                "AGENT_LIMIT",
                                "Provider native reasoning block exceeded its size limit",
                            ));
                        }
                        text.push_str(delta);
                        value[field] = json!(text);
                    }
                    _ => return Err(protocol()),
                }
            }
            "content_block_stop" => {
                let index = event["index"].as_u64().ok_or_else(protocol)?;
                let (block, json, closed) = self.blocks.get_mut(&index).ok_or_else(protocol)?;
                if *closed {
                    return Err(protocol());
                }
                if let Block::ToolUse { input, .. } = block {
                    if !json.is_empty() {
                        *input = serde_json::from_str(json).map_err(|_| protocol())?;
                    }
                    if !input.is_object() {
                        return Err(protocol());
                    }
                }
                *closed = true;
            }
            "message_delta" => {
                if let Some(stop) = event["delta"]["stop_reason"].as_str() {
                    self.stop = Some(match stop {
                        "end_turn" | "stop_sequence" => StopReason::EndTurn,
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" => StopReason::MaxTokens,
                        "refusal" => StopReason::Refusal,
                        _ => return Err(protocol()),
                    });
                }
            }
            "message_stop" => self.stopped = true,
            "error" => {
                let kind = event["error"]["type"].as_str().unwrap_or("provider_error");
                let kind = if kind.len() < 80
                    && kind
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    kind
                } else {
                    "provider_error"
                };
                return Err(Error::domain(
                    "LLM_UNAVAILABLE",
                    format!("Provider stream failed ({kind}); no partial tool calls were executed"),
                ));
            }
            _ => {} // Forward-compatible ping/metadata events carry no executable tool data.
        }
        Ok(())
    }
    fn finish(self) -> Result<ProviderReply> {
        if !self.stopped || self.blocks.values().any(|(_, _, closed)| !*closed) {
            return Err(protocol());
        }
        let stop = self.stop.ok_or_else(protocol)?;
        let content = self
            .blocks
            .into_values()
            .map(|(block, _, _)| block)
            .collect::<Vec<_>>();
        let tools = content
            .iter()
            .filter(|block| matches!(block, Block::ToolUse { .. }))
            .count();
        if matches!(stop, StopReason::ToolUse) != (tools > 0) {
            return Err(protocol());
        }
        Ok(ProviderReply { content, stop })
    }
}
fn protocol() -> Error {
    Error::domain(
        "LLM_PROTOCOL_ERROR",
        "Provider sent an incomplete or inconsistent message",
    )
}
