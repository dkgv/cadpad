//! Desktop-owned external sessions and viewport channels.
use cadpad_core::{Core, error_value, tools::Entity, views::ViewReply};
use cadpad_mcp::AppServer;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager, State, ipc::Channel};

fn error(error: cadpad_core::Error) -> Value {
    error_value(&error)
}
#[tauri::command]
pub async fn connect_view(
    core: State<'_, Core>,
    subscriptions: State<'_, crate::subscriptions::Subscriptions>,
    channel: Channel<Value>,
) -> Result<String, Value> {
    let (token, mut requests) = core.attach_view();
    subscriptions.spawn(channel.id(), async move {
        while let Some(request) = requests.recv().await {
            if channel.send(json!(request)).is_err() {
                break;
            }
        }
    });
    Ok(token)
}
#[tauri::command]
pub async fn complete_view(
    core: State<'_, Core>,
    token: String,
    request_id: String,
    reply: ViewReply,
) -> Result<(), Value> {
    core.complete_view(&token, &request_id, reply)
        .map_err(error)
}
#[tauri::command]
pub async fn set_selection(
    core: State<'_, Core>,
    workspace: String,
    revision: String,
    entities: Vec<Entity>,
) -> Result<(), Value> {
    core.set_selection(&workspace, &revision, entities)
        .await
        .map_err(error)
}
#[tauri::command]
pub async fn mcp_settings(app: AppHandle, server: State<'_, AppServer>) -> Result<Value, Value> {
    let executable = if cfg!(debug_assertions) {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target/debug/cadpad-mcp")
    } else {
        cadpad_core::runtime::PackagedRuntime::open(
            &app.path()
                .resource_dir()
                .map_err(|error| json!({"code":"APP_UNAVAILABLE","message":error.to_string()}))?,
        )
        .map_err(error)?
        .bridge
    };
    let executable = executable.canonicalize().unwrap_or(executable);
    Ok(
        json!({"config":{"mcpServers":{"cadpad":{"command":executable,"args":["--connection",server.connection_file(),"--client-name","External agent"]}}},"sessions":server.sessions().await,"bridge_installed":executable.is_file()}),
    )
}
#[tauri::command]
pub async fn disconnect_external(
    server: State<'_, AppServer>,
    session_id: String,
) -> Result<Value, Value> {
    server
        .disconnect(&session_id)
        .await
        .map(|count| json!({"cancellation_requested_for_jobs":count}))
        .map_err(error)
}

#[tauri::command]
pub async fn connect_activity(
    core: State<'_, Core>,
    subscriptions: State<'_, crate::subscriptions::Subscriptions>,
    channel: Channel<Value>,
) -> Result<(), Value> {
    let mut events = core.subscribe_activity();
    subscriptions.spawn(channel.id(), async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if channel.send(json!(event)).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if channel.send(json!({"kind":"refresh"})).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn connect_motion(
    core: State<'_, Core>,
    subscriptions: State<'_, crate::subscriptions::Subscriptions>,
    channel: Channel<Value>,
) -> Result<(), Value> {
    let mut snapshots = core.subscribe_motion();
    subscriptions.spawn(channel.id(), async move {
        loop {
            match snapshots.recv().await {
                Ok(snapshot) => {
                    if channel.send(json!(snapshot)).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if channel.send(json!({"kind":"refresh"})).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn motion(
    core: State<'_, Core>,
    workspace: String,
    revision: String,
) -> Result<Value, Value> {
    core.get_motion(&workspace, &revision)
        .await
        .map(|snapshot| json!(snapshot))
        .map_err(error)
}
#[tauri::command]
pub async fn joint_coordinate(
    core: State<'_, Core>,
    request: cadpad_core::motion::SetCoordinate,
) -> Result<Value, Value> {
    core.set_coordinate("local-user", request)
        .await
        .map(|result| json!(result))
        .map_err(error)
}

#[tauri::command]
pub async fn interference(
    core: State<'_, Core>,
    request: cadpad_core::motion::CheckInterference,
) -> Result<Value, Value> {
    core.check_interference("local-user", request)
        .await
        .map_err(error)
}

#[tauri::command]
pub async fn simulation(
    core: State<'_, Core>,
    request: cadpad_core::dynamics::Control,
) -> Result<Value, Value> {
    core.control_simulation("local-user", request)
        .await
        .map(|snapshot| json!(snapshot))
        .map_err(error)
}

#[tauri::command]
pub async fn playback(
    core: State<'_, Core>,
    request: cadpad_core::playback::Control,
) -> Result<Value, Value> {
    core.control_playback("local-user", request)
        .await
        .map(|reply| json!(reply))
        .map_err(error)
}

#[tauri::command]
pub async fn motion_tests(
    core: State<'_, Core>,
    request: cadpad_core::motion_tests::Control,
) -> Result<Value, Value> {
    core.control_motion_tests("local-user", request)
        .await
        .map(|reply| json!(reply))
        .map_err(error)
}

#[tauri::command]
pub async fn clean_cache(core: State<'_, Core>) -> Result<Value, Value> {
    core.collect_cache()
        .await
        .map(|report| json!(report))
        .map_err(error)
}

#[tauri::command]
pub async fn repair_display(
    core: State<'_, Core>,
    workspace: String,
    revision: String,
) -> Result<Value, Value> {
    core.repair_display(
        "local-user",
        cadpad_core::recovery::RepairDisplay {
            workspace_id: workspace,
            revision,
        },
    )
    .await
    .map(|job| json!(job))
    .map_err(error)
}
#[tauri::command]
pub async fn rebuild(
    core: State<'_, Core>,
    workspace: String,
    revision: String,
) -> Result<Value, Value> {
    let draft = core
        .update_source(
            &workspace,
            &revision,
            None,
            std::collections::BTreeMap::new(),
            "local-user",
        )
        .await
        .map_err(error)?;
    core.build(&workspace, &revision, &draft.id, "local-user")
        .await
        .map(|job| json!(job))
        .map_err(error)
}
