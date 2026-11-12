use cadpad_agent::service::AgentService;
use cadpad_core::{Error, error_value};
use serde_json::{Value, json};
use tauri::{State, ipc::Channel};
fn error(error: Error) -> Value {
    error_value(&error)
}
#[tauri::command]
pub async fn agent_settings(agent: State<'_, AgentService>) -> Result<Value, Value> {
    agent.settings().await.map_err(error)
}
#[tauri::command]
pub async fn agent_models(
    agent: State<'_, AgentService>,
    backend: Option<String>,
) -> Result<Value, Value> {
    agent.models(backend.unwrap_or_default()).await.map_err(error)
}
#[tauri::command]
pub async fn configure_agent(
    agent: State<'_, AgentService>,
    backend: Option<String>,
    model: String,
    api_key: Option<String>,
) -> Result<Value, Value> {
    agent
        .configure(
            backend.unwrap_or_else(|| "anthropic_api".into()),
            model,
            api_key,
        )
        .await
        .map_err(error)
}
#[tauri::command]
pub async fn forget_agent_key(agent: State<'_, AgentService>) -> Result<(), Value> {
    agent.forget_key().await.map_err(error)
}
#[tauri::command]
pub async fn agent_start(
    agent: State<'_, AgentService>,
    workspace: String,
    prompt: String,
    references: Option<Vec<cadpad_core::tools::Entity>>,
    image: Option<String>,
    chat_id: Option<String>,
    model_workspace: Option<String>,
) -> Result<Value, Value> {
    agent
        .start_in_chat_for_model(
            workspace,
            prompt,
            cadpad_agent::references::Attachments {
                references: references.unwrap_or_default(),
                image,
            },
            chat_id,
            model_workspace,
        )
        .await
        .map(|turn| json!(turn))
        .map_err(error)
}
#[tauri::command]
pub async fn agent_stop(agent: State<'_, AgentService>, turn_id: String) -> Result<Value, Value> {
    agent
        .stop(&turn_id)
        .await
        .map(|turn| json!(turn))
        .map_err(error)
}
#[tauri::command]
pub async fn agent_turn(agent: State<'_, AgentService>, turn_id: String) -> Result<Value, Value> {
    agent.turn(&turn_id).map(|turn| json!(turn)).map_err(error)
}
#[tauri::command]
pub async fn agent_conversation(
    agent: State<'_, AgentService>,
    workspace: String,
    chat_id: Option<String>,
) -> Result<Value, Value> {
    agent
        .conversation(&workspace, chat_id.as_deref())
        .map_err(error)
}
#[tauri::command]
pub async fn agent_chat(
    agent: State<'_, AgentService>,
    workspace: String,
    action: String,
    chat_id: Option<String>,
) -> Result<Value, Value> {
    agent
        .change_chat(&workspace, &action, chat_id.as_deref())
        .await
        .map_err(error)
}
#[tauri::command]
pub async fn connect_agent(
    agent: State<'_, AgentService>,
    subscriptions: State<'_, crate::subscriptions::Subscriptions>,
    channel: Channel<Value>,
) -> Result<(), Value> {
    let mut events = agent.subscribe();
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
