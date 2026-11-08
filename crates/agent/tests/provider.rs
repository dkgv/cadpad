use cadpad_agent::{
    credentials::Secret,
    provider::{
        Anthropic, Block, EventDecoder, Message, Provider, ProviderRequest, Role, StopReason,
    },
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
fn frame(event: Value) -> String {
    format!(
        "event: {}\ndata: {event}\n\n",
        event["type"].as_str().unwrap_or("message")
    )
}
async fn server(body: Vec<u8>, status: &str) -> (String, tokio::task::JoinHandle<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let status = status.to_string();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let mut length = 0;
        let mut headers = String::new();
        loop {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
            if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        assert!(length < 2 * 1024 * 1024);
        assert!(headers.to_lowercase().contains("x-api-key: test-secret"));
        assert!(headers.contains("2023-06-01"));
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).await.unwrap();
        let stream = reader.get_mut();
        stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
        for chunk in body.chunks(3) {
            if stream.write_all(chunk).await.is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
        serde_json::from_slice(&payload).unwrap()
    });
    (format!("http://{address}/v1/messages"), task)
}
fn request() -> ProviderRequest {
    ProviderRequest {
        system: "cadpad test".into(),
        messages: vec![Message::text(Role::User, "Create a plate".into())],
        tools: cadpad_core::tools::definitions(),
        max_tokens: 8192,
    }
}
#[tokio::test]
async fn fragmented_streams_preserve_tools_images_native_signatures_and_visible_text() {
    let events = vec![
        json!({"type":"message_start","message":{"content":[]}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"private reasoning"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"A plate — 50 mm"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"tool-1","name":"get_project","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"workspace_id\":"}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"workspace\"}"}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
        json!({"type":"message_stop"}),
    ];
    let body = events
        .into_iter()
        .map(frame)
        .collect::<String>()
        .into_bytes();
    let (url, server) = server(body, "200 OK").await;
    let provider = Anthropic::with_endpoint(
        &url,
        "test-model".into(),
        Secret::new("test-secret".into()).unwrap(),
    )
    .unwrap();
    let text = Arc::new(Mutex::new(String::new()));
    let output = text.clone();
    let (_send, cancel) = tokio::sync::watch::channel(false);
    let reply = provider
        .respond(
            request(),
            cancel,
            Arc::new(move |delta| output.lock().unwrap().push_str(&delta)),
        )
        .await
        .unwrap();
    assert_eq!(*text.lock().unwrap(), "A plate — 50 mm");
    assert!(matches!(reply.stop, StopReason::ToolUse));
    assert!(
        matches!(&reply.content[2],Block::ToolUse{input,..} if input["workspace_id"]=="workspace")
    );
    let mut next = request();
    next.messages.push(Message {
        role: Role::Assistant,
        content: reply.content,
    });
    next.messages.push(Message {
        role: Role::User,
        content: vec![Block::ToolResult {
            tool_use_id: "tool-1".into(),
            result: json!({"revision":"r1","image":{"mime_type":"image/png","data":"png-data"}}),
            is_error: false,
        }],
    });
    let body = provider.body(&next);
    assert_eq!(body["messages"][1]["content"][0]["signature"], "signed");
    assert_eq!(
        body["messages"][2]["content"][0]["content"][1]["source"]["data"],
        "png-data"
    );
    let received = server.await.unwrap();
    assert_eq!(
        received["tools"][0]["input_schema"],
        cadpad_core::tools::definitions()[0].input_schema
    );
    assert_eq!(received["model"], "test-model");
    assert!(received.get("api_key").is_none());
}
#[tokio::test]
async fn incomplete_tool_json_and_upstream_errors_never_return_executable_calls_or_keys() {
    for (body, status, code) in [
        (
            vec![
                json!({"type":"message_start"}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool","name":"update_source","input":{}}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"changes\":"}}),
            ],
            "200 OK",
            "LLM_PROTOCOL_ERROR",
        ),
        (
            vec![
                json!({"type":"error","error":{"type":"overloaded_error","message":"test-secret"}}),
            ],
            "200 OK",
            "LLM_UNAVAILABLE",
        ),
        (
            vec![json!({"message":"test-secret"})],
            "401 Unauthorized",
            "LLM_AUTHENTICATION_FAILED",
        ),
    ] {
        let body = body.into_iter().map(frame).collect::<String>().into_bytes();
        let (url, server) = server(body, status).await;
        let provider = Anthropic::with_endpoint(
            &url,
            "test-model".into(),
            Secret::new("test-secret".into()).unwrap(),
        )
        .unwrap();
        let (_send, cancel) = tokio::sync::watch::channel(false);
        let error = provider
            .respond(request(), cancel, Arc::new(|_| {}))
            .await
            .unwrap_err();
        assert_eq!(error.code(), code);
        assert!(!error.to_string().contains("test-secret"));
        server.await.unwrap();
    }
    assert!(
        Anthropic::with_endpoint(
            "https://untrusted.example/v1/messages",
            "test".into(),
            Secret::new("test-secret".into()).unwrap()
        )
        .is_err()
    );
}
#[test]
fn sse_accepts_mixed_line_endings_comments_and_split_utf8_but_bounds_a_single_event() {
    let bytes =
        ": ping\r\nevent: x\r\ndata: {\"value\":\"ø\"}\r\n\r\ndata: {\"value\":2}\n\n".as_bytes();
    let mut decoder = EventDecoder::default();
    let mut events = vec![];
    for byte in bytes {
        events.extend(decoder.push(&[*byte]).unwrap());
    }
    assert_eq!(events, vec![json!({"value":"ø"}), json!({"value":2})]);
    assert!(
        EventDecoder::default()
            .push(&vec![b'x'; 512 * 1024 + 1])
            .is_err()
    );
}

#[tokio::test]
async fn cancellation_interrupts_a_live_http_stream_without_waiting_for_provider_timeout() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let server_release = release.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"message_start\"}\n\n").await.unwrap();
        let _ = entered.send(());
        server_release.notified().await;
    });
    let provider = Anthropic::with_endpoint(
        &format!("http://{address}/v1/messages"),
        "test-model".into(),
        Secret::new("test-secret".into()).unwrap(),
    )
    .unwrap();
    let (cancel, signal) = tokio::sync::watch::channel(false);
    let request =
        tokio::spawn(async move { provider.respond(request(), signal, Arc::new(|_| {})).await });
    started.await.unwrap();
    cancel.send(true).unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), request)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), "CANCELLED");
    release.notify_one();
    server.await.unwrap();
}
