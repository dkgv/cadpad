use cadpad_agent::{
    credentials::SystemCredentials,
    runner::{Limits, Step},
    service::{AgentService, DefaultProviderFactory, Turn, TurnState},
};
use cadpad_core::{Core, JobState, worker::WorkerConfig};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    time::Duration,
};

#[derive(Deserialize)]
struct Case {
    id: String,
    fixture: String,
    golden: String,
    prose: String,
}

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn worker_config() -> WorkerConfig {
    WorkerConfig::development(&project_root())
}

fn selected_backend() -> (&'static str, &'static str) {
    match std::env::var("CADPAD_REQUIREMENTS_EVAL_BACKEND").as_deref() {
        Ok("claude_cli") => ("claude_cli", ""),
        Ok("codex_cli") => ("codex_cli", ""),
        _ => panic!("Set CADPAD_REQUIREMENTS_EVAL_BACKEND explicitly to claude_cli or codex_cli"),
    }
}

fn output_path() -> PathBuf {
    let path = std::env::var_os("CADPAD_REQUIREMENTS_EVAL_RESULTS")
        .map(PathBuf::from)
        .expect("Set CADPAD_REQUIREMENTS_EVAL_RESULTS to a new local JSONL output path");
    assert!(
        !path.exists(),
        "Refusing to overwrite an existing eval results file"
    );
    path
}

fn fixture_identity(name: &str) -> (&'static str, &'static str, &'static str) {
    match name {
        "turntable" => ("turntable", "machine.py", "machine"),
        "clamp" => ("clamp", "machine.py", "machine"),
        "cabinet" => ("cabinet", "cabinet.py", "cabinet"),
        "bracket" => ("bracket", "bracket.py", "bracket"),
        _ => panic!("Unknown P1 fixture"),
    }
}

async fn wait_for_build(
    core: &Core,
    workspace: &str,
    job_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(240), async {
        loop {
            let job = core.get_job(workspace, job_id).await?;
            match job.state {
                JobState::Succeeded => return Ok(()),
                JobState::Failed | JobState::Cancelled => {
                    return Err(std::io::Error::other(format!(
                        "Fixture build failed: {}",
                        job.diagnostics
                    ))
                    .into());
                }
                JobState::Queued | JobState::Running => {
                    tokio::time::sleep(Duration::from_millis(100)).await
                }
            }
        }
    })
    .await
    .map_err(|_| std::io::Error::other("Fixture build timed out"))?
}

async fn wait_for_turn(
    service: &AgentService,
    turn: &Turn,
) -> Result<Turn, Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(900), async {
        loop {
            let current = service.turn(&turn.id).unwrap();
            if !matches!(current.state, TurnState::Running | TurnState::Cancelling) {
                return current;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| std::io::Error::other("Agent turn timed out"))
    .map_err(Into::into)
}

fn questions(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| line.contains('?'))
        .map(str::to_owned)
        .collect()
}

async fn run_case(
    core: &Core,
    agent: &AgentService,
    case: &Case,
    run_root: &std::path::Path,
) -> Result<Value, Box<dyn std::error::Error>> {
    let (directory, file, builder) = fixture_identity(&case.fixture);
    let fixture_root = project_root().join("fixtures/requirements").join(directory);
    let project_root = run_root.join("projects").join(&case.id);
    fs::create_dir_all(&project_root)?;
    fs::copy(fixture_root.join(file), project_root.join(file))?;
    cadpad_core::portable::initialize_project_browser(&project_root)?;

    // Open the fixture as a normal Cadpad project and open its declared model.
    let project = core.open_project(project_root, "requirements-eval").await?;
    let (workspace, job) = core
        .open_project_model(
            &project.workspace_id,
            &project.base_revision,
            file,
            builder,
            "requirements-eval",
        )
        .await?;
    wait_for_build(core, &workspace, &job.id).await?;

    let prompt = format!(
        "Create a requirement draft for the current model using this exact user prose:\n{}\n\nUse control_requirements: list first, then compile and save the draft if its meaning is clear and compilation has no errors. Ask a concise clarification question and stop if meaning is unresolved; do not guess. Do not edit source or approve anything. State the canonical print or the clarification needed.",
        case.prose
    );
    let started = agent.start(workspace.clone(), prompt).await?;
    let done = wait_for_turn(agent, &started).await?;

    // Read what the agent actually persisted. A successful saved print has
    // hashes, a canonical print, and no diagnostics; failed turns stay null.
    let listed = core
        .dispatch(
            &cadpad_core::tools::ToolContext {
                actor: "requirements-eval-review".into(),
            },
            "control_requirements",
            json!({"workspace_id":workspace,"command":{"action":"list"}}),
        )
        .await?;
    let saved = listed["requirements"]
        .as_array()
        .and_then(|items| items.first());
    let saved_compile_success = saved.map(|entry| {
        entry["draft_hashes"].is_object()
            && entry["print"].is_object()
            && entry["diagnostics"].as_array().is_some_and(|diagnostics| {
                diagnostics
                    .iter()
                    .all(|diagnostic| diagnostic["severity"] != "error")
            })
    });
    let tool_calls: Vec<String> = done
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Tool { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    let blocked_by_tool_approval = {
        let text = done.text.to_lowercase();
        text.contains("require approval")
            || text.contains("cannot grant it")
            || text.contains("disallows it")
    };
    let question_list = questions(&done.text);
    let clarification_only =
        saved.is_none() && !blocked_by_tool_approval && !question_list.is_empty();
    let compile_success = saved_compile_success.or_else(|| {
        (saved.is_none()
            && !blocked_by_tool_approval
            && !clarification_only
            && done.state == TurnState::Succeeded)
            .then_some(false)
    });
    let run_status = if blocked_by_tool_approval {
        "tool_approval_blocked"
    } else if clarification_only {
        "clarification"
    } else if saved_compile_success == Some(true) {
        "draft_compiled"
    } else if saved_compile_success == Some(false) {
        "compile_failed"
    } else if done.state == TurnState::Succeeded {
        "no_valid_draft"
    } else {
        "turn_failed"
    };
    Ok(json!({
        "id":case.id,
        "fixture":case.fixture,
        "golden":case.golden,
        "compile_success":compile_success,
        "questions":question_list,
        "manual_review":"unreviewed",
        "review_note":"",
        "canonical_print":saved.and_then(|entry| entry["rendered"].as_str()),
        "diagnostics":saved.map(|entry| &entry["diagnostics"]),
        "response":done.text,
        "turn_state":done.state,
        "run_status":run_status,
        "tool_calls":tool_calls,
    }))
}

#[tokio::test]
#[ignore = "runs 20 live CLI-backed agent turns; explicitly select an authenticated CLI backend"]
async fn run_requirements_agent_eval() -> Result<(), Box<dyn std::error::Error>> {
    let (backend, model) = selected_backend();
    let output_path = output_path();
    let mut output_options = OpenOptions::new();
    output_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        output_options.mode(0o600);
    }
    let mut output = output_options.open(output_path)?;
    let cases_path = project_root().join("scripts/requirements-eval/cases.jsonl");
    let mut cases: Vec<Case> = fs::read_to_string(cases_path)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(
        cases.len(),
        20,
        "P1.91 corpus must contain exactly 20 cases"
    );
    let ids: std::collections::HashSet<_> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), 20, "P1.91 case IDs must be unique");
    if let Some(limit) = std::env::var_os("CADPAD_REQUIREMENTS_EVAL_CASE_LIMIT") {
        let limit: usize = limit.to_string_lossy().parse()?;
        assert!((1..=20).contains(&limit), "Case limit must be from 1 to 20");
        cases.truncate(limit);
    }

    let root = project_root();
    let mcp = std::env::var_os("CADPAD_MCP_EXECUTABLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/debug/cadpad-mcp"));
    let mcp = mcp
        .canonicalize()
        .expect("Build cadpad-mcp or set CADPAD_MCP_EXECUTABLE");
    let temp = tempfile::tempdir()?;
    let core = Core::open(temp.path().join("core"), worker_config()).await?;
    let mcp_server = cadpad_mcp::AppServer::start(core.clone(), temp.path().join("mcp")).await?;
    let agent = AgentService::open_with_bridge(
        core.clone(),
        temp.path().join("agent"),
        std::sync::Arc::new(SystemCredentials),
        std::sync::Arc::new(DefaultProviderFactory),
        Limits::default(),
        Some(cadpad_agent::cli::Bridge {
            mcp_executable: mcp,
            connection_file: mcp_server.connection_file().to_path_buf(),
        }),
    )
    .await?;
    agent
        .configure(backend.into(), model.into(), None)
        .await
        .expect("Selected CLI backend must be installed and authenticated");

    let run_root = temp.path().to_path_buf();
    for case in cases {
        let mut record = match run_case(&core, &agent, &case, &run_root).await {
            Ok(record) => record,
            Err(error) => json!({
                "id":case.id,
                "fixture":case.fixture,
                "golden":case.golden,
                "compile_success":null,
                "questions":null,
                "manual_review":"unreviewed",
                "review_note":"",
                "driver_error":error.to_string(),
            }),
        };
        if let Some(record) = record.as_object_mut() {
            record.insert("backend".into(), Value::String(backend.into()));
        }
        serde_json::to_writer(&mut output, &record)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    output.sync_all()?;
    Ok(())
}
