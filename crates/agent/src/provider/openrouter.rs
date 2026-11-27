use super::{
    Block, EmitText, EventDecoder, Message, Provider, ProviderReply, ProviderRequest, Role,
    StopReason, TokenUsage,
};
use crate::credentials::Secret;
use async_trait::async_trait;
use cadpad_core::{Error, Result};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::watch;

const MAX_TOOL_CALLS: usize = 32;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_REASONING_BYTES: usize = 512 * 1024;
const MAX_TOOL_INPUT_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

pub struct OpenRouter {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    model: String,
    key: Secret,
}

impl OpenRouter {
    pub fn new(model: String, key: Secret) -> Result<Self> {
        Self::with_endpoint("https://openrouter.ai/api/v1/chat/completions", model, key)
    }

    /// Only the official API and loopback test servers are accepted. This endpoint
    /// is not configurable by project Python, tools, or desktop settings.
    pub fn with_endpoint(endpoint: &str, model: String, key: Secret) -> Result<Self> {
        let endpoint = reqwest::Url::parse(endpoint)
            .map_err(|_| Error::domain("INVALID_PARAMETER", "Invalid provider endpoint"))?;
        let official = endpoint.scheme() == "https"
            && endpoint.host_str() == Some("openrouter.ai")
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
        if model.is_empty() || model.len() > 200 || model.chars().any(char::is_control) {
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
        let mut messages = Vec::with_capacity(request.messages.len() + 1);
        messages.push(json!({"role": "system", "content": request.system}));
        for message in &request.messages {
            messages.extend(wire_message(message));
        }
        let mut body = json!({
            "model": self.model,
            "max_tokens": request.max_tokens,
            "messages": messages,
            "stream": true,
            // Modeling needs tool calling; do not route to an endpoint that drops it.
            "provider": {"require_parameters": true},
        });
        if !request.tools.is_empty() {
            body["tools"] = json!(
                request
                    .tools
                    .iter()
                    .map(|tool| json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema,
                        }
                    }))
                    .collect::<Vec<_>>()
            );
        }
        body
    }
}

fn wire_message(message: &Message) -> Vec<Value> {
    match message.role {
        Role::Assistant => vec![assistant_message(message)],
        Role::User => user_messages(message),
    }
}

fn assistant_message(message: &Message) -> Value {
    let mut text = Vec::new();
    let mut tool_calls = Vec::new();
    let mut reasoning = Vec::new();
    for block in &message.content {
        match block {
            Block::Text { text: value } => text.push(value.as_str()),
            Block::ToolUse { id, name, input } => {
                let arguments = serde_json::to_string(input).unwrap_or_else(|_| "{}".into());
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments}
                }));
            }
            Block::Native { value } => {
                if value["type"] == "openrouter_reasoning_details"
                    && let Some(details) = value["details"].as_array()
                {
                    reasoning.extend(details.iter().cloned());
                }
            }
            Block::ToolResult { .. } => {}
        }
    }
    let content = if text.is_empty() && !tool_calls.is_empty() {
        Value::Null
    } else {
        Value::String(text.join("\n\n"))
    };
    let mut message = json!({"role": "assistant", "content": content});
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }
    if !reasoning.is_empty() {
        message["reasoning_details"] = json!(reasoning);
    }
    message
}

fn user_messages(message: &Message) -> Vec<Value> {
    let mut tool_results = Vec::new();
    let mut text = Vec::new();
    let mut images = Vec::new();
    for block in &message.content {
        match block {
            Block::Text { text: value } => text.push(value.as_str()),
            Block::Native { value } => {
                if let Some(image) = native_image(value) {
                    images.push((None, image));
                }
            }
            Block::ToolResult {
                tool_use_id,
                result,
                is_error,
            } => {
                let (content, embedded) = tool_result_wire(result, *is_error);
                tool_results.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": content,
                }));
                images.extend(embedded.into_iter().map(|(label, url)| (Some(label), url)));
            }
            Block::ToolUse { .. } => {}
        }
    }
    let mut messages = tool_results;
    if !text.is_empty() || !images.is_empty() {
        messages.push(json!({
            "role": "user",
            "content": content_parts(text.join("\n\n"), &images),
        }));
    }
    messages
}

fn content_parts(text: String, images: &[(Option<String>, String)]) -> Value {
    if images.is_empty() {
        return Value::String(text);
    }
    let mut parts = Vec::with_capacity(images.len() * 2 + 1);
    if !text.is_empty() {
        parts.push(json!({"type": "text", "text": text}));
    }
    for (label, url) in images {
        if let Some(label) = label.as_deref().filter(|label| !label.is_empty()) {
            parts.push(json!({"type": "text", "text": label}));
        }
        parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
    }
    Value::Array(parts)
}

fn native_image(value: &Value) -> Option<String> {
    if value["type"] != "image" {
        return None;
    }
    let source = &value["source"];
    if source["type"] != "base64" {
        return None;
    }
    let mime = source["media_type"].as_str()?;
    let data = source["data"].as_str()?;
    if !mime.starts_with("image/") || data.is_empty() {
        return None;
    }
    Some(format!("data:{mime};base64,{data}"))
}

fn tool_image(value: &Value) -> Option<(String, String)> {
    let mime = value["mime_type"].as_str()?;
    let data = value["data"].as_str()?;
    if !mime.starts_with("image/") || data.is_empty() {
        return None;
    }
    let label = value["label"].as_str().unwrap_or("").to_string();
    Some((label, format!("data:{mime};base64,{data}")))
}

fn tool_result_wire(result: &Value, is_error: bool) -> (String, Vec<(String, String)>) {
    let mut result = result.clone();
    let mut images = Vec::new();
    if let Some(object) = result.as_object_mut() {
        if let Some(image) = object.get("image").cloned()
            && let Some((label, url)) = tool_image(&image)
        {
            object.remove("image");
            images.push((label, url));
        }
        if let Some(Value::Array(values)) = object.get("images").cloned() {
            let parsed = values.iter().map(tool_image).collect::<Option<Vec<_>>>();
            if let Some(parsed) = parsed {
                object.remove("images");
                images.extend(parsed);
            }
        }
    }
    let content = serde_json::to_string(&result).unwrap_or_else(|_| "null".into());
    if is_error {
        (format!("Tool error: {content}"), images)
    } else {
        (content, images)
    }
}

#[derive(Default)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct MessageAccumulator {
    text: String,
    refusal: String,
    reasoning: Vec<Value>,
    reasoning_bytes: usize,
    tool_calls: BTreeMap<u64, ToolCall>,
    finish: Option<StopReason>,
    finished: bool,
    usage: TokenUsage,
}

impl MessageAccumulator {
    fn event(&mut self, event: Value, emit: &EmitText) -> Result<()> {
        if let Some(usage) = event.get("usage").and_then(TokenUsage::from_json) {
            if usage.input_tokens > 0 {
                self.usage.input_tokens = usage.input_tokens;
            }
            if usage.output_tokens > 0 {
                self.usage.output_tokens = usage.output_tokens;
            }
        }
        if event.get("error").is_some() {
            let kind = event["error"]["type"]
                .as_str()
                .or_else(|| event["error"]["code"].as_str())
                .unwrap_or("provider_error");
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
        let Some(choices) = event.get("choices").and_then(Value::as_array) else {
            return Ok(());
        };
        // Empty choices carry usage or keep-alive metadata only.
        if choices.is_empty() {
            return Ok(());
        }
        if choices.len() != 1 || self.finished {
            return Err(protocol());
        }
        let choice = &choices[0];
        let delta = &choice["delta"];
        if let Some(text) = delta["content"].as_str()
            && !text.is_empty()
        {
            if self.text.len() + text.len() > MAX_TEXT_BYTES {
                return Err(Error::domain(
                    "AGENT_LIMIT",
                    "Assistant text exceeded 64 KiB",
                ));
            }
            self.text.push_str(text);
            emit(text.into());
        }
        if let Some(refusal) = delta["refusal"].as_str()
            && !refusal.is_empty()
        {
            if self.refusal.len() + refusal.len() > MAX_TEXT_BYTES {
                return Err(Error::domain(
                    "AGENT_LIMIT",
                    "Assistant refusal exceeded 64 KiB",
                ));
            }
            self.refusal.push_str(refusal);
        }
        if let Some(reasoning) = delta["reasoning"].as_str()
            && !reasoning.is_empty()
        {
            if self.reasoning_bytes + reasoning.len() > MAX_REASONING_BYTES {
                return Err(Error::domain(
                    "AGENT_LIMIT",
                    "Provider native reasoning block exceeded its size limit",
                ));
            }
            self.reasoning_bytes += reasoning.len();
        }
        if let Some(details) = delta["reasoning_details"].as_array() {
            for detail in details {
                let size = serde_json::to_vec(detail)
                    .map(|bytes| bytes.len())
                    .unwrap_or(MAX_REASONING_BYTES + 1);
                if self.reasoning_bytes + size > MAX_REASONING_BYTES {
                    return Err(Error::domain(
                        "AGENT_LIMIT",
                        "Provider native reasoning block exceeded its size limit",
                    ));
                }
                self.reasoning_bytes += size;
                self.reasoning.push(detail.clone());
            }
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            if calls.len() > MAX_TOOL_CALLS {
                return Err(protocol());
            }
            for call in calls {
                let index = call["index"].as_u64().ok_or_else(protocol)?;
                if index >= MAX_TOOL_CALLS as u64 {
                    return Err(protocol());
                }
                let slot = self.tool_calls.entry(index).or_default();
                if let Some(id) = call["id"].as_str()
                    && !id.is_empty()
                {
                    if id.len() > 128 || id.chars().any(char::is_control) {
                        return Err(protocol());
                    }
                    if !slot.id.is_empty() && slot.id != id {
                        return Err(protocol());
                    }
                    slot.id = id.into();
                }
                if let Some(kind) = call["type"].as_str()
                    && kind != "function"
                {
                    return Err(protocol());
                }
                if let Some(name) = call["function"]["name"].as_str()
                    && !name.is_empty()
                {
                    if name.len() > 128 || name.chars().any(char::is_control) {
                        return Err(protocol());
                    }
                    if slot.name.is_empty() {
                        slot.name = name.into();
                    } else if slot.name != name {
                        slot.name.push_str(name);
                        if slot.name.len() > 128 {
                            return Err(protocol());
                        }
                    }
                }
                if let Some(arguments) = call["function"]["arguments"].as_str() {
                    slot.arguments.push_str(arguments);
                    if slot.arguments.len() > MAX_TOOL_INPUT_BYTES {
                        return Err(Error::domain("AGENT_LIMIT", "Tool input exceeded 1 MiB"));
                    }
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason")
            && !reason.is_null()
        {
            if self.finish.is_some() {
                return Err(protocol());
            }
            let mut stop = match reason.as_str() {
                Some("stop") => StopReason::EndTurn,
                Some("tool_calls" | "function_call") => StopReason::ToolUse,
                Some("length") => StopReason::MaxTokens,
                Some("content_filter") => StopReason::Refusal,
                _ => return Err(protocol()),
            };
            // Some upstreams report a plain stop after emitting tool calls.
            if matches!(stop, StopReason::EndTurn) && !self.tool_calls.is_empty() {
                stop = StopReason::ToolUse;
            }
            if matches!(stop, StopReason::EndTurn) && !self.refusal.is_empty() {
                stop = StopReason::Refusal;
            }
            self.finish = Some(stop);
            self.finished = true;
        }
        Ok(())
    }

    fn finish(self) -> Result<ProviderReply> {
        let stop = self.finish.ok_or_else(protocol)?;
        let mut content = Vec::new();
        if !self.reasoning.is_empty() {
            content.push(Block::Native {
                value: json!({
                    "type": "openrouter_reasoning_details",
                    "details": self.reasoning,
                }),
            });
        }
        if !self.text.is_empty() {
            content.push(Block::Text { text: self.text });
        } else if !self.refusal.is_empty() {
            content.push(Block::Text { text: self.refusal });
        }
        for (expected, (index, call)) in self.tool_calls.into_iter().enumerate() {
            if index != expected as u64 || call.id.is_empty() || call.name.is_empty() {
                return Err(protocol());
            }
            let input = if call.arguments.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&call.arguments).map_err(|_| protocol())?
            };
            if !input.is_object() {
                return Err(protocol());
            }
            content.push(Block::ToolUse {
                id: call.id,
                name: call.name,
                input,
            });
        }
        let tools = content
            .iter()
            .filter(|block| matches!(block, Block::ToolUse { .. }))
            .count();
        if matches!(stop, StopReason::ToolUse) != (tools > 0) {
            return Err(protocol());
        }
        let usage = (!self.usage.is_zero()).then_some(self.usage);
        Ok(ProviderReply {
            content,
            stop,
            usage,
        })
    }
}

#[async_trait]
impl Provider for OpenRouter {
    async fn respond(
        &self,
        request: ProviderRequest,
        mut cancel: watch::Receiver<bool>,
        emit: EmitText,
    ) -> Result<ProviderReply> {
        if *cancel.borrow() {
            return Err(Error::domain("CANCELLED", "Agent stopped"));
        }
        let mut key =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.key.expose()))
                .map_err(|_| {
                    Error::domain(
                        "INVALID_PARAMETER",
                        "API key contains invalid header characters",
                    )
                })?;
        key.set_sensitive(true);
        let response = tokio::select! {
            response=self.client.post(self.endpoint.clone()).header(reqwest::header::AUTHORIZATION,key).json(&self.body(&request)).send()=>response.map_err(|_|Error::domain("LLM_UNAVAILABLE","Provider connection failed or timed out"))?,
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
            if total > MAX_RESPONSE_BYTES {
                return Err(Error::domain(
                    "AGENT_LIMIT",
                    "Provider response exceeded 2 MiB",
                ));
            }
            for event in decoder.push(&chunk)? {
                message.event(event, &emit)?;
                if message.finished {
                    return message.finish();
                }
            }
        }
        Err(Error::domain(
            "LLM_PROTOCOL_ERROR",
            "Provider stream ended without a finish reason; no partial tool calls were executed",
        ))
    }
}

fn protocol() -> Error {
    Error::domain(
        "LLM_PROTOCOL_ERROR",
        "Provider sent an incomplete or inconsistent message",
    )
}
