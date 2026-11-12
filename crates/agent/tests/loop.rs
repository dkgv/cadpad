use async_trait::async_trait;
use cadpad_agent::{
    credentials::{Credentials, Secret},
    provider::*,
    runner::Limits,
    service::{AgentService, ProviderFactory, Settings, Turn, TurnState},
};
use cadpad_core::{Core, Result, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;
#[derive(Default)]
struct MemoryCredentials(Mutex<Option<String>>);
impl Credentials for MemoryCredentials {
    fn get(&self) -> Result<Option<Secret>> {
        self.0.lock().unwrap().clone().map(Secret::new).transpose()
    }
    fn set(&self, key: Secret) -> Result<()> {
        *self.0.lock().unwrap() = Some(key.expose().into());
        Ok(())
    }
    fn delete(&self) -> Result<()> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}
struct Factory(Arc<dyn Provider>);
impl ProviderFactory for Factory {
    fn create(&self, _: &Settings, _: Secret) -> Result<Arc<dyn Provider>> {
        Ok(self.0.clone())
    }
}
fn config() -> WorkerConfig {
    if let Some(resources) = std::env::var_os("CADPAD_TEST_PACKAGED_RESOURCES") {
        assert_eq!(
            std::env::var("PATH").unwrap_or_default(),
            "",
            "Packaged agent QA requires empty PATH"
        );
        return cadpad_core::runtime::PackagedRuntime::open(&PathBuf::from(resources))
            .unwrap()
            .worker_config();
    }
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
}
struct Script {
    state: Mutex<(u32, String, String, String, String)>,
}
impl Script {
    fn new() -> Self {
        Self {
            state: Mutex::new((
                0,
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            )),
        }
    }
}
fn last_result(request: &ProviderRequest) -> Value {
    match request.messages.last().unwrap().content.last().unwrap() {
        Block::ToolResult { result, .. } => result.clone(),
        _ => Value::Null,
    }
}
#[async_trait]
impl Provider for Script {
    async fn respond(
        &self,
        request: ProviderRequest,
        _: watch::Receiver<bool>,
        emit: EmitText,
    ) -> Result<ProviderReply> {
        let mut state = self.state.lock().unwrap();
        let (step, ws, base, draft, revision) = &mut *state;
        let result = last_result(&request);
        if *step == 0 {
            let Block::Text { text } = &request.messages[0].content[0] else {
                panic!()
            };
            let context = text
                .strip_prefix("Current authoritative workspace context:\n")
                .unwrap()
                .split("\nSelection:")
                .next()
                .unwrap();
            let context: Value = serde_json::from_str(context).unwrap();
            *ws = context["workspace_id"].as_str().unwrap().into();
            *base = context["revision"].as_str().unwrap().into();
        }
        let call = match *step {
            0 => (
                "update_source",
                json!({"workspace_id":ws,"expected_base_revision":base,"changes":{"model.py":"raise ValueError('intentional repair exercise')"}}),
            ),
            1 => {
                *draft = result["id"].as_str().unwrap().into();
                (
                    "build",
                    json!({"workspace_id":ws,"expected_base_revision":base,"draft_id":draft}),
                )
            }
            2 => {
                assert_eq!(result["observed_job"]["state"], "failed");
                (
                    "update_source",
                    json!({"workspace_id":ws,"expected_base_revision":base,"draft_id":draft,"changes":{"model.py":"import cadquery as cq\nw=parameter('width',50,minimum=1,maximum=100)\nshow(cq.Workplane().box(w,20,6),name='plate')"}}),
                )
            }
            3 => (
                "build",
                json!({"workspace_id":ws,"expected_base_revision":base,"draft_id":draft}),
            ),
            4 => {
                *revision = result["observed_job"]["resulting_revision"]
                    .as_str()
                    .unwrap()
                    .into();
                assert_eq!(result["verification"]["entities"][0]["volume"], 6000.0);
                (
                    "capture_view",
                    json!({"workspace_id":ws,"revision":revision,"pose_version":0,"dimensions":true}),
                )
            }
            5 => {
                assert_eq!(result["image"]["mime_type"], "image/png");
                (
                    "set_parameter",
                    json!({"workspace_id":ws,"expected_base_revision":revision,"parameter_id":"width","value":75,"unit":"mm"}),
                )
            }
            6 => {
                *revision = result["observed_job"]["resulting_revision"]
                    .as_str()
                    .unwrap()
                    .into();
                assert_eq!(
                    result["verification"]["entities"][0]["bounds"]["size"][0],
                    75.0
                );
                (
                    "export",
                    json!({"workspace_id":ws,"revision":revision,"format":"STEP"}),
                )
            }
            _ => {
                assert_eq!(result["format"], "STEP");
                let text = "Created the plate, repaired the failed build, verified its width is 75 mm, and prepared STEP export.";
                emit(text.into());
                return Ok(ProviderReply {
                    content: vec![Block::Text { text: text.into() }],
                    stop: StopReason::EndTurn,
                });
            }
        };
        *step += 1;
        Ok(ProviderReply {
            content: vec![Block::ToolUse {
                id: format!("call-{step}"),
                name: call.0.into(),
                input: call.1,
            }],
            stop: StopReason::ToolUse,
        })
    }
}
async fn terminal(service: &AgentService, turn: &Turn) -> Turn {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let current = service.turn(&turn.id).unwrap();
            if !matches!(current.state, TurnState::Running | TurnState::Cancelling) {
                return current;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn builtin_loop_repairs_inspects_captures_refines_exports_and_keeps_credentials_private() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("core"), config())
        .await
        .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let other = core.create_workspace("user").await.unwrap();
    let (endpoint, http_server) = policy_server(Arc::new(Script::new())).await;
    let http_provider = Arc::new(
        Anthropic::with_endpoint(
            &endpoint,
            "test-model".into(),
            Secret::new("private-api-key".into()).unwrap(),
        )
        .unwrap(),
    );
    let service = AgentService::open(
        core.clone(),
        temp.path().join("agent"),
        Arc::new(MemoryCredentials::default()),
        Arc::new(Factory(http_provider)),
        Limits::default(),
    )
    .await
    .unwrap();
    service
        .configure(
            "anthropic_api".into(),
            "test-model".into(),
            Some("private-api-key".into()),
        )
        .await
        .unwrap();
    let missing = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        service
            .start(missing.clone(), "Unknown workspace".into())
            .await
            .unwrap_err()
            .code(),
        "APP_UNAVAILABLE"
    );
    assert_eq!(
        service.conversation(&missing, None).unwrap()["messages"],
        json!([])
    );
    let mut events = service.subscribe();
    let (token, mut views) = core.attach_view();
    let view_core = core.clone();
    let renderer = tokio::spawn(async move {
        let request = views.recv().await.unwrap();
        view_core.complete_view(&token,&request.id,cadpad_core::views::ViewReply{data:Some(json!({"image":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jA98AAAAASUVORK5CYII=","camera":{"position":[100,100,100],"quaternion":[0,0,0,1],"target":[0,0,0],"zoom":1,"orthographic":true},"revision":request.revision,"pose_version":0,"visible_part_ids":["plate"]})),error:None}).unwrap();
    });
    let turn = service
        .start(
            ws.clone(),
            "Create a plate and refine its width to 75 mm, then export STEP".into(),
        )
        .await
        .unwrap();
    core.focus_workspace(&other).await.unwrap();
    let turn = terminal(&service, &turn).await;
    assert_eq!(turn.state, TurnState::Succeeded, "{:?}", turn.error);
    assert_eq!(turn.tool_calls, 7);
    assert_eq!(turn.artifacts.len(), 1);
    renderer.await.unwrap();
    http_server.await.unwrap();
    let exported = &turn.artifacts[0];
    let bytes = core
        .artifact(
            &ws,
            exported["revision"].as_str().unwrap(),
            exported["handle"].as_str().unwrap(),
        )
        .await
        .unwrap();
    assert!(bytes.starts_with(b"ISO-10303-21;"));
    assert_eq!(
        core.get_project(&other).await.unwrap()["model"]["parts"],
        json!([])
    );
    assert_eq!(
        core.get_project(&ws).await.unwrap()["parameters"]["width"]["value"],
        75.0
    );
    let mut streamed = false;
    while let Ok(event) = events.try_recv() {
        // The first chunk of a segment opens a text step; later chunks are deltas.
        if event.kind == "text_delta"
            || event.kind == "step"
                && event.turn.as_ref().is_some_and(|turn| {
                    turn.steps
                        .iter()
                        .any(|step| matches!(step, cadpad_agent::runner::Step::Text { .. }))
                })
        {
            streamed = true;
        }
        assert!(
            !serde_json::to_string(&event)
                .unwrap()
                .contains("private-api-key")
        );
    }
    assert!(streamed);
    for path in [
        temp.path().join("agent/settings.json"),
        temp.path().join(format!("agent/conversations/{ws}.json")),
    ] {
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.contains("private-api-key"));
    }
    let saved = service.conversation(&ws, None).unwrap();
    assert_eq!(saved["messages"].as_array().unwrap().len(), 2);
    assert!(
        saved["messages"][1]["text"]
            .as_str()
            .unwrap()
            .contains("75 mm")
    );
}
struct SlowNative {
    state: Mutex<(u32, String, String, String)>,
}
#[async_trait]
impl Provider for SlowNative {
    async fn respond(
        &self,
        request: ProviderRequest,
        _: watch::Receiver<bool>,
        _: EmitText,
    ) -> Result<ProviderReply> {
        let mut state = self.state.lock().unwrap();
        let (step, ws, base, draft) = &mut *state;
        let (name, input) = if *step == 0 {
            let Block::Text { text } = &request.messages[0].content[0] else {
                panic!()
            };
            let context: Value = serde_json::from_str(
                text.strip_prefix("Current authoritative workspace context:\n")
                    .unwrap()
                    .split("\nSelection:")
                    .next()
                    .unwrap(),
            )
            .unwrap();
            *ws = context["workspace_id"].as_str().unwrap().into();
            *base = context["revision"].as_str().unwrap().into();
            (
                "update_source",
                json!({"workspace_id":ws,"expected_base_revision":base,"changes":{"model.py":"from pathlib import Path\nimport sys,time\ncache=Path(sys.argv[sys.argv.index('--cache')+1])\n(cache/'agent-entered').write_text('running')\ntime.sleep(120)"}}),
            )
        } else {
            *draft = last_result(&request)["id"].as_str().unwrap().into();
            (
                "build",
                json!({"workspace_id":ws,"expected_base_revision":base,"draft_id":draft}),
            )
        };
        *step += 1;
        Ok(ProviderReply {
            content: vec![Block::ToolUse {
                id: format!("slow-{step}"),
                name: name.into(),
                input,
            }],
            stop: StopReason::ToolUse,
        })
    }
}
#[tokio::test]
async fn stop_waits_for_native_termination_and_preserves_the_committed_model() {
    let temp = tempfile::tempdir().unwrap();
    let core_root = temp.path().join("core");
    let core = Core::open(core_root.clone(), config()).await.unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([(
                "model.py".into(),
                "import cadquery as cq\nshow(cq.Workplane().box(10,10,10),name='box')".into(),
            )]),
            "user",
        )
        .await
        .unwrap();
    let job = core.build(&ws, &base, &draft.id, "user").await.unwrap();
    let base = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let job = core.get_job(&ws, &job.id).await.unwrap();
            if let Some(revision) = job.resulting_revision {
                break revision;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let service = AgentService::open(
        core.clone(),
        temp.path().join("agent"),
        Arc::new(MemoryCredentials::default()),
        Arc::new(Factory(Arc::new(SlowNative {
            state: Mutex::new((0, String::new(), String::new(), String::new())),
        }))),
        Limits::default(),
    )
    .await
    .unwrap();
    service
        .configure(
            "anthropic_api".into(),
            "test-model".into(),
            Some("private-api-key".into()),
        )
        .await
        .unwrap();
    let turn = service
        .start(ws.clone(), "Exercise cancellation".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let jobs = core.actor_jobs(&turn.actor).await;
            assert!(
                !jobs
                    .iter()
                    .any(|(_, job)| job.state == cadpad_core::JobState::Failed),
                "Native cancellation setup failed: {jobs:?}"
            );
            let entered = std::fs::read_dir(core_root.join("artifacts").join(&ws))
                .unwrap()
                .flatten()
                .any(|entry| entry.path().join("agent-entered").is_file());
            if entered
                && jobs
                    .iter()
                    .any(|(_, job)| job.state == cadpad_core::JobState::Running)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        service.stop(&turn.id).await.unwrap().state,
        TurnState::Cancelling
    );
    let stopped = terminal(&service, &turn).await;
    assert_eq!(stopped.state, TurnState::Cancelled);
    assert!(
        core.actor_jobs(&turn.actor)
            .await
            .iter()
            .all(|(_, job)| !matches!(
                job.state,
                cadpad_core::JobState::Running | cadpad_core::JobState::Queued
            ))
    );
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], base);
    let inspected = core
        .query(
            &ws,
            &base,
            "inspect",
            json!({"entities":[{"revision":base,"instance_id":"box","entity_id":"part:0"}]}),
        )
        .await
        .unwrap();
    assert!((inspected["entities"][0]["volume"].as_f64().unwrap() - 1000.0).abs() < 1e-9);
}

// Exercise the production HTTP/SSE adapter and the real core together. This
// deterministic loopback service replaces only the remote model, not CAD tools.
async fn policy_server(policy: Arc<dyn Provider>) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut length = 0;
            let mut line = String::new();
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
            assert!(
                headers
                    .to_lowercase()
                    .contains("x-api-key: private-api-key")
            );
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            let messages=body["messages"].as_array().unwrap().iter().map(|message|Message{role:if message["role"]=="assistant"{Role::Assistant}else{Role::User},content:message["content"].as_array().unwrap().iter().map(|block|match block["type"].as_str().unwrap(){
            "text"=>Block::Text{text:block["text"].as_str().unwrap().into()},
            "tool_use"=>Block::ToolUse{id:block["id"].as_str().unwrap().into(),name:block["name"].as_str().unwrap().into(),input:block["input"].clone()},
            "tool_result"=>{let mut result:Value=serde_json::from_str(block["content"][0]["text"].as_str().unwrap()).unwrap();if let Some(image)=block["content"].as_array().unwrap().iter().find(|block|block["type"]=="image"){result["image"]=json!({"mime_type":image["source"]["media_type"],"data":image["source"]["data"]});}Block::ToolResult{tool_use_id:block["tool_use_id"].as_str().unwrap().into(),result,is_error:block["is_error"].as_bool().unwrap()}},
            _=>panic!("Unexpected test block"),
        }).collect()}).collect();
            let (_send, cancel) = watch::channel(false);
            let reply = policy
                .respond(
                    ProviderRequest {
                        system: body["system"].as_str().unwrap().into(),
                        messages,
                        tools: cadpad_core::tools::definitions(),
                        max_tokens: body["max_tokens"].as_u64().unwrap() as u32,
                    },
                    cancel,
                    Arc::new(|_| {}),
                )
                .await
                .unwrap();
            let mut events = vec![json!({"type":"message_start","message":{"content":[]}})];
            for (index, block) in reply.content.iter().enumerate() {
                let block = match block {
                    Block::Text { text } => json!({"type":"text","text":text}),
                    Block::ToolUse { id, name, input } => {
                        json!({"type":"tool_use","id":id,"name":name,"input":input})
                    }
                    _ => panic!("Unexpected test block"),
                };
                events.push(
                    json!({"type":"content_block_start","index":index,"content_block":block}),
                );
                events.push(json!({"type":"content_block_stop","index":index}));
            }
            events.push(json!({"type":"message_delta","delta":{"stop_reason":if matches!(reply.stop,StopReason::ToolUse){"tool_use"}else{"end_turn"}}}));
            events.push(json!({"type":"message_stop"}));
            let output = events
                .into_iter()
                .map(|event| format!("data: {event}\n\n"))
                .collect::<String>();
            let stream = reader.get_mut();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",output.len()).as_bytes()).await.unwrap();
            stream.write_all(output.as_bytes()).await.unwrap();
            if matches!(reply.stop, StopReason::EndTurn) {
                break;
            }
        }
    });
    (format!("http://{address}/v1/messages"), task)
}

struct RepeatedFailure {
    step: Mutex<u32>,
    draft: Mutex<String>,
}
#[async_trait]
impl Provider for RepeatedFailure {
    async fn respond(
        &self,
        request: ProviderRequest,
        _: watch::Receiver<bool>,
        _: EmitText,
    ) -> Result<ProviderReply> {
        let mut step = self.step.lock().unwrap();
        let Block::Text { text } = &request.messages[0].content[0] else {
            panic!()
        };
        let context: Value = serde_json::from_str(
            text.strip_prefix("Current authoritative workspace context:\n")
                .unwrap()
                .split("\nSelection:")
                .next()
                .unwrap(),
        )
        .unwrap();
        let input = if *step == 0 {
            json!({"workspace_id":context["workspace_id"],"expected_base_revision":context["revision"],"changes":{"model.py":"raise ValueError('retry bound')"}})
        } else {
            if *step == 1 {
                *self.draft.lock().unwrap() = last_result(&request)["id"].as_str().unwrap().into();
            }
            json!({"workspace_id":context["workspace_id"],"expected_base_revision":context["revision"],"draft_id":*self.draft.lock().unwrap()})
        };
        let name = if *step == 0 { "update_source" } else { "build" };
        *step += 1;
        Ok(ProviderReply {
            content: vec![Block::ToolUse {
                id: format!("retry-{step}"),
                name: name.into(),
                input,
            }],
            stop: StopReason::ToolUse,
        })
    }
}
#[tokio::test]
async fn repeated_build_failures_stop_at_the_budget_and_history_recovers_without_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("core"), config())
        .await
        .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let original = core.get_project(&ws).await.unwrap()["revision"].clone();
    let credentials = Arc::new(MemoryCredentials::default());
    let factory = Arc::new(Factory(Arc::new(RepeatedFailure {
        step: Mutex::new(0),
        draft: Mutex::new(String::new()),
    })));
    let limits = Limits {
        build_failures: 2,
        ..Limits::default()
    };
    let service = AgentService::open(
        core.clone(),
        temp.path().join("agent"),
        credentials.clone(),
        factory.clone(),
        limits.clone(),
    )
    .await
    .unwrap();
    service
        .configure(
            "anthropic_api".into(),
            "test-model".into(),
            Some("private-api-key".into()),
        )
        .await
        .unwrap();
    let mut events = service.subscribe();
    let turn = service
        .start(ws.clone(), "Exercise bounded repair".into())
        .await
        .unwrap();
    let finished = terminal(&service, &turn).await;
    assert_eq!(finished.state, TurnState::Failed);
    assert_eq!(finished.error.unwrap()["code"], "AGENT_LIMIT");
    assert_eq!(core.actor_jobs(&turn.actor).await.len(), 2);
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], original);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if events.recv().await.unwrap().kind == "turn_finished" {
                break;
            }
        }
    })
    .await
    .unwrap();
    let reopened = AgentService::open(
        core,
        temp.path().join("agent"),
        credentials.clone(),
        factory,
        limits,
    )
    .await
    .unwrap();
    let conversation = reopened.conversation(&ws, None).unwrap();
    assert_eq!(conversation["messages"].as_array().unwrap().len(), 2);
    assert!(
        conversation["messages"][1]["text"]
            .as_str()
            .unwrap()
            .contains("repeated failed builds")
    );
    reopened.forget_key().await.unwrap();
    assert!(credentials.get().unwrap().is_none());
}
struct WrongWorkspace {
    target: String,
    step: Mutex<u32>,
}
#[async_trait]
impl Provider for WrongWorkspace {
    async fn respond(
        &self,
        request: ProviderRequest,
        _: watch::Receiver<bool>,
        emit: EmitText,
    ) -> Result<ProviderReply> {
        let mut step = self.step.lock().unwrap();
        *step += 1;
        if *step == 1 {
            Ok(ProviderReply {
                content: vec![Block::ToolUse {
                    id: "wrong-workspace".into(),
                    name: "get_project".into(),
                    input: json!({"workspace_id":self.target}),
                }],
                stop: StopReason::ToolUse,
            })
        } else {
            assert_eq!(last_result(&request)["code"], "WORKSPACE_MISMATCH");
            let text = "The selected workspace remains unchanged.";
            emit(text.into());
            Ok(ProviderReply {
                content: vec![Block::Text { text: text.into() }],
                stop: StopReason::EndTurn,
            })
        }
    }
}
#[tokio::test]
async fn workspace_binding_and_tool_budgets_are_enforced_before_execution() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("core"), config())
        .await
        .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let other = core.create_workspace("user").await.unwrap();
    for (index, budget) in [64, 0].into_iter().enumerate() {
        let credentials = Arc::new(MemoryCredentials::default());
        let limits = Limits {
            tool_calls: budget,
            ..Limits::default()
        };
        let service = AgentService::open(
            core.clone(),
            temp.path().join(format!("agent-{index}")),
            credentials,
            Arc::new(Factory(Arc::new(WrongWorkspace {
                target: other.clone(),
                step: Mutex::new(0),
            }))),
            limits,
        )
        .await
        .unwrap();
        service
            .configure(
                "anthropic_api".into(),
                "test-model".into(),
                Some("private-api-key".into()),
            )
            .await
            .unwrap();
        let turn = service
            .start(ws.clone(), "Stay in this workspace".into())
            .await
            .unwrap();
        let finished = terminal(&service, &turn).await;
        if budget == 0 {
            assert_eq!(finished.state, TurnState::Failed);
            assert_eq!(finished.error.unwrap()["code"], "AGENT_LIMIT");
        } else {
            assert_eq!(finished.state, TurnState::Succeeded);
        }
    }
    assert_eq!(
        core.get_project(&other).await.unwrap()["model"]["parts"],
        json!([])
    );
    assert_eq!(
        core.get_project(&ws).await.unwrap()["model"]["parts"],
        json!([])
    );
}

struct CaptureWait;
#[async_trait]
impl Provider for CaptureWait {
    async fn respond(
        &self,
        request: ProviderRequest,
        _: watch::Receiver<bool>,
        _: EmitText,
    ) -> Result<ProviderReply> {
        let Block::Text { text } = &request.messages[0].content[0] else {
            panic!()
        };
        let context: Value = serde_json::from_str(
            text.strip_prefix("Current authoritative workspace context:\n")
                .unwrap()
                .split("\nSelection:")
                .next()
                .unwrap(),
        )
        .unwrap();
        Ok(ProviderReply {
            content: vec![Block::ToolUse {
                id: "waiting-capture".into(),
                name: "capture_view".into(),
                input: json!({"workspace_id":context["workspace_id"],"revision":context["revision"]}),
            }],
            stop: StopReason::ToolUse,
        })
    }
}
#[tokio::test]
async fn turn_deadline_revokes_an_inflight_tool_instead_of_waiting_for_its_own_timeout() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("core"), config())
        .await
        .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let (token, mut views) = core.attach_view();
    let limits = Limits {
        turn_timeout: Duration::from_millis(250),
        ..Limits::default()
    };
    let service = AgentService::open(
        core.clone(),
        temp.path().join("agent"),
        Arc::new(MemoryCredentials::default()),
        Arc::new(Factory(Arc::new(CaptureWait))),
        limits,
    )
    .await
    .unwrap();
    service
        .configure(
            "anthropic_api".into(),
            "test-model".into(),
            Some("private-api-key".into()),
        )
        .await
        .unwrap();
    let turn = service.start(ws, "Capture a view".into()).await.unwrap();
    let request = views.recv().await.unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(2), terminal(&service, &turn))
        .await
        .unwrap();
    assert_eq!(finished.state, TurnState::Failed);
    assert_eq!(finished.error.unwrap()["code"], "AGENT_LIMIT");
    assert_eq!(
        core.complete_view(
            &token,
            &request.id,
            cadpad_core::views::ViewReply {
                data: Some(json!({})),
                error: None
            }
        )
        .unwrap_err()
        .code(),
        "STALE_ENTITY"
    );
}
/// Records the final user message and ends the turn.
struct RecordPrompt(Mutex<Option<Message>>);
#[async_trait]
impl Provider for RecordPrompt {
    async fn respond(
        &self,
        request: ProviderRequest,
        _: watch::Receiver<bool>,
        _: EmitText,
    ) -> Result<ProviderReply> {
        *self.0.lock().unwrap() = request.messages.last().cloned();
        Ok(ProviderReply {
            content: vec![Block::Text {
                text: "Noted.".into(),
            }],
            stop: StopReason::EndTurn,
        })
    }
}
#[tokio::test]
async fn viewport_references_reach_the_model_with_component_source_exact_properties_and_view() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("core"), config())
        .await
        .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([
                (
                    "model.py".into(),
                    "from parts import plate\n\ndef build(context):\n    plate.build(context)\n".into(),
                ),
                (
                    "parts/plate.py".into(),
                    "import cadquery as cq\n\ndef build(context):\n    body = cq.Workplane().box(40, 20, 6)\n    context.show(body, name='plate', features={'top': body.faces('>Z')})\n".into(),
                ),
            ]),
            "user",
        )
        .await
        .unwrap();
    let job = core.build(&ws, &base, &draft.id, "user").await.unwrap();
    let revision = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = core.get_job(&ws, &job.id).await.unwrap();
            if let Some(revision) = job.resulting_revision {
                break revision;
            }
            assert_ne!(
                job.state,
                cadpad_core::JobState::Failed,
                "{:?}",
                job.diagnostics
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let project = core.get_project(&ws).await.unwrap();
    let part = &project["model"]["parts"][0];
    assert_eq!(part["source"], json!({"file":"parts/plate.py","line":5}));
    let top = part["features"]["top"][0].as_str().unwrap().to_string();
    let provider = Arc::new(RecordPrompt(Mutex::new(None)));
    let service = AgentService::open(
        core.clone(),
        temp.path().join("agent"),
        Arc::new(MemoryCredentials::default()),
        Arc::new(Factory(provider.clone())),
        Limits::default(),
    )
    .await
    .unwrap();
    service
        .configure(
            "anthropic_api".into(),
            "test-model".into(),
            Some("private-api-key".into()),
        )
        .await
        .unwrap();
    let entity = |id: &str| cadpad_core::tools::Entity {
        revision: revision.clone(),
        instance_id: "plate".into(),
        entity_id: id.into(),
    };
    let stale = cadpad_core::tools::Entity {
        revision: base.clone(),
        ..entity("part:0")
    };
    let attachments = |references| cadpad_agent::references::Attachments {
        references,
        image: Some("iVBORw0KGgo=".into()),
    };
    assert_eq!(
        service
            .start_with(ws.clone(), "Thicker".into(), attachments(vec![stale]))
            .await
            .unwrap_err()
            .code(),
        "STALE_ENTITY",
        "A selection from an earlier revision is never relocated"
    );
    let turn = service
        .start_with(
            ws.clone(),
            "Make A 2 mm thicker than B".into(),
            attachments(vec![entity(&top), entity("part:0")]),
        )
        .await
        .unwrap();
    assert_eq!(terminal(&service, &turn).await.state, TurnState::Succeeded);
    let message = provider.0.lock().unwrap().clone().unwrap();
    let [
        Block::Text { text: context },
        Block::Native { value: image },
        Block::Text { text: prompt },
    ] = message.content.as_slice()
    else {
        panic!("unexpected prompt blocks: {:?}", message.content)
    };
    assert_eq!(prompt, "Make A 2 mm thicker than B");
    assert_eq!(image["source"]["data"], "iVBORw0KGgo=");
    assert!(context.contains("\"letter\": \"A\"") && context.contains("\"letter\": \"B\""));
    assert!(context.contains("\"label\": \"top\""));
    assert!(context.contains("<source path=\"parts/plate.py\""));
    assert!(context.contains("cq.Workplane().box(40, 20, 6)"));
    assert!(
        !context.contains("\"unavailable\""),
        "exact inspection should succeed: {context}"
    );
    let history = service.conversation(&ws, None).unwrap();
    assert_eq!(
        history["messages"][0]["references"]
            .as_array()
            .unwrap()
            .iter()
            .map(|chip| (
                chip["letter"].as_str().unwrap(),
                chip["file"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        [("A", "parts/plate.py"), ("B", "parts/plate.py")]
    );
    // The shared chat belongs to the project, while references belong to a model tab.
    let (model_workspace, job) = core
        .open_project_model(&ws, &revision, "parts/plate.py", "build", "user")
        .await
        .unwrap();
    let model_revision = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let job = core.get_job(&model_workspace, &job.id).await.unwrap();
            if let Some(revision) = job.resulting_revision {
                break revision;
            }
            assert_ne!(job.state, cadpad_core::JobState::Failed, "{:?}", job.diagnostics);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    let model_entity = cadpad_core::tools::Entity {
        revision: model_revision.clone(),
        ..entity("part:0")
    };
    let turn = service.start_in_chat_for_model(
        ws.clone(), "Adjust this model".into(), attachments(vec![model_entity]), None,
        Some(model_workspace.clone()),
    ).await.unwrap();
    assert_eq!(turn.workspace_id, ws, "the conversation remains on the project");
    let completed = terminal(&service, &turn).await;
    assert_eq!(completed.state, TurnState::Succeeded, "{:?}", completed.error);
    let message = provider.0.lock().unwrap().clone().unwrap();
    let context = match &message.content[0] {
        Block::Text { text } => text,
        other => panic!("expected reference context, got {other:?}"),
    };
    assert!(context.contains("parts/plate.py"));
    assert!(!context.contains("\"unavailable\""), "model references resolve in their own workspace: {context}");
    assert_eq!(service.conversation(&ws, None).unwrap()["messages"][2]["references"][0]["part"], "plate");
    let unrelated = core.create_workspace("user").await.unwrap();
    assert_eq!(service.start_in_chat_for_model(
        ws.clone(), "Wrong model".into(), Default::default(), None, Some(unrelated),
    ).await.unwrap_err().code(), "INVALID_REQUEST");
}

struct ChatHistoryProvider(Mutex<Vec<String>>, Option<Arc<tokio::sync::Semaphore>>);
#[async_trait]
impl Provider for ChatHistoryProvider {
    async fn respond(
        &self,
        request: ProviderRequest,
        _: watch::Receiver<bool>,
        _: EmitText,
    ) -> Result<ProviderReply> {
        let text = request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let first = {
            let mut requests = self.0.lock().unwrap();
            requests.push(text);
            requests.len() == 1
        };
        if first && let Some(gate) = &self.1 {
            gate.acquire().await.unwrap().forget();
        }
        Ok(ProviderReply {
            content: vec![Block::Text {
                text: "Saved reply".into(),
            }],
            stop: StopReason::EndTurn,
        })
    }
}
async fn finished_event(
    events: &mut tokio::sync::broadcast::Receiver<cadpad_agent::service::AgentEvent>,
    id: &str,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.kind == "turn_finished" && event.turn_id == id {
                break;
            }
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn saved_chats_isolate_context_resume_and_survive_restart() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("core"), config())
        .await
        .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let original = core.get_project(&ws).await.unwrap()["revision"].clone();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = Arc::new(ChatHistoryProvider(Mutex::new(vec![]), Some(gate.clone())));
    let credentials = Arc::new(MemoryCredentials::default());
    let factory = Arc::new(Factory(provider.clone()));
    let root = temp.path().join("agent");
    let service = AgentService::open(
        core.clone(),
        root.clone(),
        credentials.clone(),
        factory.clone(),
        Limits::default(),
    )
    .await
    .unwrap();
    service
        .configure(
            "anthropic_api".into(),
            "test-model".into(),
            Some("private-api-key".into()),
        )
        .await
        .unwrap();
    assert_eq!(service.conversation(&ws, None).unwrap()["chats"], json!([]));
    let mut events = service.subscribe();
    let first = service
        .start(ws.clone(), "First mechanism discussion".into())
        .await
        .unwrap();
    assert_eq!(
        service
            .change_chat(&ws, "remove", Some(&first.chat_id))
            .await
            .unwrap_err()
            .code(),
        "AGENT_BUSY"
    );
    let fresh = service.change_chat(&ws, "new", None).await.unwrap();
    gate.add_permits(1);
    finished_event(&mut events, &first.id).await;
    let first_id = first.chat_id.clone();
    assert_eq!(fresh["messages"], json!([]));
    assert!(fresh["turn"].is_null());
    assert_eq!(fresh["chats"].as_array().unwrap().len(), 1);
    assert_eq!(
        service.change_chat(&ws, "new", None).await.unwrap()["active_chat_id"],
        fresh["active_chat_id"]
    );
    let second_id = fresh["active_chat_id"].as_str().unwrap().to_string();
    assert_ne!(first_id, second_id);
    assert_eq!(
        service
            .start_in_chat(
                ws.clone(),
                "Stale send".into(),
                Default::default(),
                Some(uuid::Uuid::new_v4().to_string())
            )
            .await
            .unwrap_err()
            .code(),
        "STALE_CHAT"
    );
    let second = service
        .start(ws.clone(), "Second independent discussion".into())
        .await
        .unwrap();
    finished_event(&mut events, &second.id).await;
    let resumed = service
        .change_chat(&ws, "select", Some(&first_id))
        .await
        .unwrap();
    assert_eq!(resumed["messages"][0]["text"], "First mechanism discussion");
    assert_eq!(resumed["turn"]["id"], first.id);
    let continuation = service
        .start(ws.clone(), "Continue this discussion".into())
        .await
        .unwrap();
    finished_event(&mut events, &continuation.id).await;
    let requests = provider.0.lock().unwrap();
    assert!(!requests[1].contains("First mechanism discussion"));
    assert!(requests[2].contains("First mechanism discussion"));
    assert!(!requests[2].contains("Second independent discussion"));
    drop(requests);
    let reopened = AgentService::open(core.clone(), root, credentials, factory, Limits::default())
        .await
        .unwrap();
    let current = reopened.conversation(&ws, None).unwrap();
    assert_eq!(current["active_chat_id"], first_id);
    assert_eq!(current["messages"].as_array().unwrap().len(), 4);
    assert_eq!(current["turn"]["id"], continuation.id);
    let other = core.create_workspace("user").await.unwrap();
    assert!(
        reopened
            .change_chat(&other, "select", Some(&first_id))
            .await
            .is_err()
    );
    assert_eq!(
        reopened
            .change_chat(&ws, "select", Some(&second_id))
            .await
            .unwrap()["messages"][0]["text"],
        "Second independent discussion"
    );
    let removed = reopened
        .change_chat(&ws, "remove", Some(&first_id))
        .await
        .unwrap();
    assert_eq!(removed["active_chat_id"], second_id);
    assert_eq!(removed["chats"].as_array().unwrap().len(), 1);
    assert!(
        reopened
            .change_chat(&ws, "select", Some(&first_id))
            .await
            .is_err()
    );
    let removed_current = reopened
        .change_chat(&ws, "remove", Some(&second_id))
        .await
        .unwrap();
    assert_eq!(removed_current["messages"], json!([]));
    assert_eq!(removed_current["chats"], json!([]));
    let reloaded = AgentService::open(
        core.clone(),
        temp.path().join("agent"),
        Arc::new(MemoryCredentials::default()),
        Arc::new(Factory(provider.clone())),
        Limits::default(),
    )
    .await
    .unwrap();
    assert_eq!(reloaded.conversation(&ws, None).unwrap()["chats"], json!([]));
    let restored = reloaded
        .change_chat(&ws, "restore", Some(&first_id))
        .await
        .unwrap();
    assert_eq!(restored["chats"].as_array().unwrap().len(), 1);
    assert_eq!(
        reloaded
            .change_chat(&ws, "select", Some(&first_id))
            .await
            .unwrap()["messages"],
        current["messages"]
    );
    assert_eq!(core.get_project(&ws).await.unwrap()["revision"], original);
}

#[tokio::test]
async fn legacy_chat_migrates_and_failed_chat_save_preserves_selection() {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(temp.path().join("core"), config())
        .await
        .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let root = temp.path().join("agent");
    std::fs::create_dir_all(root.join("conversations")).unwrap();
    std::fs::write(root.join("conversations").join(format!("{ws}.json")),serde_json::to_vec(&json!([{"role":"user","text":"Existing project conversation"},{"role":"assistant","text":"Existing answer"}])).unwrap()).unwrap();
    let service = AgentService::open(
        core,
        root.clone(),
        Arc::new(MemoryCredentials::default()),
        Arc::new(Factory(Arc::new(ChatHistoryProvider(Mutex::new(vec![]), None)))),
        Limits::default(),
    )
    .await
    .unwrap();
    let old = service.conversation(&ws, None).unwrap();
    assert_eq!(old["messages"].as_array().unwrap().len(), 2);
    let id = old["active_chat_id"].as_str().unwrap();
    std::fs::rename(root.join("conversations"), root.join("backup")).unwrap();
    std::fs::write(root.join("conversations"), b"blocked").unwrap();
    assert!(service.change_chat(&ws, "new", None).await.is_err());
    assert_eq!(service.conversation(&ws, None).unwrap()["active_chat_id"], id);
    std::fs::remove_file(root.join("conversations")).unwrap();
    std::fs::rename(root.join("backup"), root.join("conversations")).unwrap();
    let new = service.change_chat(&ws, "new", None).await.unwrap();
    assert_ne!(new["active_chat_id"], id);
    assert_eq!(
        service.change_chat(&ws, "select", Some(id)).await.unwrap()["messages"],
        old["messages"]
    );
    let stored: Value = serde_json::from_slice(
        &std::fs::read(root.join("conversations").join(format!("{ws}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(stored["chats"].as_array().unwrap().len(), 2);
}

// Run in a child process so a synchronous mutex deadlock is bounded, and the
// fake CLI's PATH cannot affect other tests or invoke a real subscription.
#[cfg(unix)]
#[test]
fn cli_clarification_finishes_persists_and_accepts_another_message() {
    const FLAG: &str = "CADPAD_CLI_COMPLETION_CHILD";
    if std::env::var_os(FLAG).is_none() {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/usr/bin/which", bin.path().join("which")).unwrap();
        let agent = bin.path().join("cursor-agent");
        std::fs::write(&agent, r#"#!/bin/sh
if [ "$1" = status ]; then echo 'Logged in'; exit 0; fi
printf '%s\n' '{"type":"tool_call","subtype":"started","call_id":"inspect","tool_call":{"mcpToolCall":{"args":{"toolName":"get_project"}}}}' '{"type":"tool_call","subtype":"completed","call_id":"inspect","tool_call":{"mcpToolCall":{"args":{"toolName":"get_project"},"result":{"success":{}}}}}' '{"type":"assistant","message":{"content":[{"type":"text","text":"Which part should receive the load?"}]}}' '{"type":"result","subtype":"success"}'
"#).unwrap();
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli_clarification_finishes_persists_and_accepts_another_message", "--nocapture"])
            .env(FLAG, "1").env("PATH", bin.path()).spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("CLI completion blocked instead of releasing the conversation");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::open(temp.path().join("core"), config()).await.unwrap();
        let ws = core.create_workspace("user").await.unwrap();
        let root = temp.path().join("agent");
        let service = AgentService::open_with_bridge(
            core, root.clone(), Arc::new(MemoryCredentials::default()),
            Arc::new(Factory(Arc::new(Script::new()))), Limits::default(),
            Some(cadpad_agent::cli::Bridge {
                mcp_executable: "/usr/bin/true".into(),
                connection_file: "/unused/connection.json".into(),
            }),
        ).await.unwrap();
        service.configure("cursor_cli".into(), "Cursor CLI".into(), None).await.unwrap();
        for prompt in ["Test the load", "Apply it to the coupler"] {
            let mut events = service.subscribe();
            let turn = service.start(ws.clone(), prompt.into()).await.unwrap();
            let done = terminal(&service, &turn).await;
            assert_eq!(done.state, TurnState::Succeeded, "{:?}", done.error);
            assert_eq!(done.text, "Which part should receive the load?");
            assert_eq!(done.tool_calls, 1);
            // Completion includes persistence and removal of the busy marker.
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let event = events.recv().await.unwrap();
                    if event.turn_id == turn.id && event.kind == "turn_finished" { break; }
                }
            }).await.unwrap();
        }
        let stored: Value = serde_json::from_slice(&std::fs::read(root.join("conversations").join(format!("{ws}.json"))).unwrap()).unwrap();
        assert_eq!(stored["chats"][0]["messages"].as_array().unwrap().len(), 4);
        assert_eq!(stored["chats"][0]["last_turn"]["state"], "succeeded");
    });
}
