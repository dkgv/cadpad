use cadpad_agent::cloud::CloudService;
use cadpad_agent::service::AgentService;
use cadpad_core::{Core, Error, error_value};
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
    agent
        .models(backend.unwrap_or_default())
        .await
        .map_err(error)
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
    folders: Option<Vec<String>>,
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
                folders: folders.unwrap_or_default(),
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

#[tauri::command]
pub async fn cloud_settings(cloud: State<'_, CloudService>) -> Result<Value, Value> {
    cloud.settings().await.map_err(error)
}

#[tauri::command]
pub async fn configure_cloud(
    cloud: State<'_, CloudService>,
    base_url: String,
    api_key: Option<String>,
    auth_frontend_url: Option<String>,
) -> Result<Value, Value> {
    cloud
        .configure(base_url, api_key, auth_frontend_url)
        .await
        .map_err(error)
}

#[tauri::command]
pub async fn forget_cloud_key(cloud: State<'_, CloudService>) -> Result<(), Value> {
    cloud.forget_key().await.map_err(error)
}

#[tauri::command]
pub async fn cloud_sign_in(cloud: State<'_, CloudService>) -> Result<Value, Value> {
    cloud.sign_in_with_browser().await.map_err(error)
}

/// Run image→CAD against backend and return CadQuery source (does not mutate the project).
#[tauri::command]
pub async fn cloud_generate(
    cloud: State<'_, CloudService>,
    image_base64: String,
    mime_type: Option<String>,
    prompt: Option<String>,
    filename: Option<String>,
) -> Result<Value, Value> {
    let image = decode_base64(&image_base64).map_err(error)?;
    cloud
        .generate_from_image(
            image,
            mime_type.unwrap_or_else(|| "image/png".into()),
            prompt,
            filename,
        )
        .await
        .map_err(error)
}

/// Generate CadQuery from an image and stage it into the open workspace as `model.py`, then build.
#[tauri::command]
pub async fn cloud_generate_into_workspace(
    cloud: State<'_, CloudService>,
    core: State<'_, Core>,
    workspace: String,
    image_base64: String,
    mime_type: Option<String>,
    prompt: Option<String>,
    filename: Option<String>,
) -> Result<Value, Value> {
    let image = decode_base64(&image_base64).map_err(error)?;
    let generated = cloud
        .generate_from_image(
            image,
            mime_type.unwrap_or_else(|| "image/png".into()),
            prompt,
            filename,
        )
        .await
        .map_err(error)?;
    let staged = stage_generated(&core, &workspace, &generated).await?;
    Ok(json!({
        "generation": generated,
        "draft_id": staged.0,
        "job": staged.1,
        "entry_point": staged.2,
        "source": staged.3,
    }))
}

/// Stage a cloud generation variant (an `alternates[]` entry from a previous
/// generation response) into the workspace as a new revision, then build.
#[tauri::command]
pub async fn cloud_stage_variant(
    core: State<'_, Core>,
    workspace: String,
    variant: Value,
) -> Result<Value, Value> {
    let staged = stage_generated(&core, &workspace, &variant).await?;
    Ok(json!({
        "variant": variant,
        "draft_id": staged.0,
        "job": staged.1,
        "entry_point": staged.2,
        "source": staged.3,
    }))
}

/// Stage a wrapped generation result (winner or alternate) into the workspace:
/// assets via stage_asset, entry point via update_source, then build.
/// Returns (draft_id, build_job, entry_point, source).
async fn stage_generated(
    core: &Core,
    workspace: &str,
    generated: &Value,
) -> Result<(String, cadpad_core::Job, String, String), Value> {
    let source = generated
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| error(Error::domain("NETWORK_ERROR", "Missing generated source")))?
        .to_string();
    let entry_point = generated
        .get("entry_point")
        .and_then(Value::as_str)
        .unwrap_or("model.py")
        .to_string();
    let project = core.get_project(workspace).await.map_err(error)?;
    let base = project
        .get("revision")
        .and_then(Value::as_str)
        .ok_or_else(|| error(Error::domain("INVALID_STATE", "Project revision missing")))?
        .to_string();
    let mut draft_id: Option<String> = None;
    if let Some(assets) = generated.get("assets").and_then(Value::as_array) {
        for asset in assets {
            let path = asset
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| error(Error::domain("NETWORK_ERROR", "Asset missing path")))?;
            let content_b64 = asset
                .get("content_base64")
                .and_then(Value::as_str)
                .ok_or_else(|| error(Error::domain("NETWORK_ERROR", "Asset missing content")))?;
            let bytes = decode_base64(content_b64).map_err(error)?;
            let draft = core
                .stage_asset(
                    workspace,
                    &base,
                    draft_id.as_deref(),
                    path,
                    bytes,
                    "cloud-generate",
                )
                .await
                .map_err(error)?;
            draft_id = Some(draft.id);
        }
    }
    let draft = core
        .update_source(
            workspace,
            &base,
            draft_id.as_deref(),
            BTreeMap::from([(entry_point.clone(), source.clone())]),
            "cloud-generate",
        )
        .await
        .map_err(error)?;
    let job = core
        .build(workspace, &base, &draft.id, "cloud-generate")
        .await
        .map_err(error)?;
    Ok((draft.id, job, entry_point, source))
}

fn decode_base64(value: &str) -> Result<Vec<u8>, Error> {
    use base64::Engine;
    let trimmed = value
        .trim()
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(',').map(|(_, data)| data))
        .unwrap_or(value.trim());
    base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .map_err(|_| Error::domain("INVALID_PARAMETER", "Image must be valid base64"))
}
