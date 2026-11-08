#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod agents;
mod builtin;
#[cfg(test)]
mod frontend_assets;
mod menus;
use builtin::{
    agent_chat, agent_conversation, agent_settings, agent_start, agent_stop, agent_turn,
    configure_agent, connect_agent, forget_agent_key,
};
mod files;
use agents::{
    clean_cache, complete_view, connect_activity, connect_motion, connect_view,
    disconnect_external, interference, joint_coordinate, mcp_settings, motion, motion_tests,
    playback, rebuild, repair_display, set_selection, simulation,
};
use cadpad_core::{Core, error_value, worker::WorkerConfig};
use files::{
    check_external, export, forget_workspace, forget_workspaces, import_model, import_step,
    import_stl, new_scratchpad, open_project, reload_project, save_project, switch_project,
    workspaces,
};
use serde_json::{Value, json};
use tauri::{Manager, State};

fn error(error: cadpad_core::Error) -> Value {
    error_value(&error)
}
#[tauri::command]
async fn initialize(core: State<'_, Core>) -> Result<Value, Value> {
    let mut workspaces = core.list_workspaces().await;
    workspaces.sort();
    let focused = core.focused_workspace().await;
    let workspace = match focused.as_ref().or_else(|| workspaces.first()) {
        Some(id) => id.clone(),
        None => core.create_workspace("local-user").await.map_err(error)?,
    };
    core.focus_workspace(&workspace).await.map_err(error)?;
    core.get_project(&workspace).await.map_err(error)
}
#[tauri::command]
async fn project(core: State<'_, Core>, workspace: String) -> Result<Value, Value> {
    core.get_project(&workspace).await.map_err(error)
}
#[tauri::command]
async fn read_source(
    core: State<'_, Core>,
    workspace: String,
    version: String,
    path: String,
) -> Result<Value, Value> {
    core.read_source(&workspace, &version, &path)
        .await
        .map_err(error)
}
#[tauri::command]
async fn example(
    core: State<'_, Core>,
    workspace: String,
    revision: String,
    channel: Option<bool>,
    curved: Option<bool>,
    linkage: Option<bool>,
) -> Result<Value, Value> {
    let changes = std::collections::BTreeMap::from([(
        "model.py".into(),
        if linkage.unwrap_or(false) {
            include_str!("../../../../fixtures/four_bar.py")
        } else if curved.unwrap_or(false) {
            include_str!("../../../../fixtures/curved_carriage.py")
        } else if channel.unwrap_or(false) {
            include_str!("../../../../fixtures/channel_carriage.py")
        } else {
            include_str!("../../../../fixtures/rail_carriage.py")
        }
        .into(),
    )]);
    let draft = core
        .update_source(&workspace, &revision, None, changes, "local-user")
        .await
        .map_err(error)?;
    let definition = serde_json::from_str(if linkage.unwrap_or(false) {
        include_str!("../../../../fixtures/four_bar.mechanism.json")
    } else if curved.unwrap_or(false) {
        include_str!("../../../../fixtures/curved_carriage.mechanism.json")
    } else if channel.unwrap_or(false) {
        include_str!("../../../../fixtures/channel_carriage.mechanism.json")
    } else {
        include_str!("../../../../fixtures/rail_carriage.mechanism.json")
    })
    .map_err(|failure| json!({"code":"INVALID_MECHANISM","message":failure.to_string()}))?;
    let draft = core
        .stage_mechanism(
            &workspace,
            &revision,
            Some(&draft.id),
            definition,
            "local-user",
        )
        .await
        .map_err(error)?;
    let job = core
        .build(&workspace, &revision, &draft.id, "local-user")
        .await
        .map_err(error)?;
    Ok(json!(job))
}
#[tauri::command]
async fn job(core: State<'_, Core>, workspace: String, job_id: String) -> Result<Value, Value> {
    core.get_job(&workspace, &job_id)
        .await
        .map(|j| json!(j))
        .map_err(error)
}
#[tauri::command]
async fn stop(core: State<'_, Core>, workspace: String, job_id: String) -> Result<Value, Value> {
    core.cancel_job(&workspace, &job_id)
        .await
        .map(|j| json!(j))
        .map_err(error)
}
#[tauri::command]
async fn undo(core: State<'_, Core>, workspace: String, revision: String) -> Result<Value, Value> {
    core.undo(&workspace, &revision, "local-user")
        .await
        .map(|j| json!(j))
        .map_err(error)
}
#[tauri::command]
async fn parameter(
    core: State<'_, Core>,
    workspace: String,
    revision: String,
    parameter: String,
    value: f64,
    unit: String,
) -> Result<Value, Value> {
    core.set_parameter(
        &workspace,
        &revision,
        &parameter,
        value,
        &unit,
        "local-user",
    )
    .await
    .map(|j| json!(j))
    .map_err(error)
}
#[tauri::command]
async fn annotation(
    core: State<'_, Core>,
    workspace: String,
    revision: String,
    annotation_id: String,
    record: Option<cadpad_core::annotations::Annotation>,
) -> Result<Value, Value> {
    core.edit_annotation(&workspace, &revision, &annotation_id, record, "local-user")
        .await
        .map(|job| json!(job))
        .map_err(error)
}
#[tauri::command]
async fn inspect(
    core: State<'_, Core>,
    workspace: String,
    revision: String,
    args: Value,
) -> Result<Value, Value> {
    core.query(&workspace, &revision, "inspect", args)
        .await
        .map_err(error)
}
#[tauri::command]
async fn artifact(
    core: State<'_, Core>,
    workspace: String,
    revision: String,
    handle: String,
) -> Result<tauri::ipc::Response, Value> {
    core.artifact(&workspace, &revision, &handle)
        .await
        .map(tauri::ipc::Response::new)
        .map_err(error)
}
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .on_menu_event(menus::dispatch)
        .setup(|app| {
            menus::install(app)?;
            let config = if cfg!(debug_assertions) {
                WorkerConfig::development(
                    &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../..")
                        .canonicalize()?,
                )
            } else {
                cadpad_core::runtime::PackagedRuntime::open(&app.path().resource_dir()?)?
                    .worker_config()
            };
            let core =
                tauri::async_runtime::block_on(Core::open(app.path().app_data_dir()?, config))?;
            let server = tauri::async_runtime::block_on(cadpad_mcp::AppServer::start(
                core.clone(),
                app.path().app_data_dir()?.join("mcp"),
            ))?;
            let mcp_executable = if cfg!(debug_assertions) {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../target/debug/cadpad-mcp")
            } else {
                cadpad_core::runtime::PackagedRuntime::open(&app.path().resource_dir()?)?.bridge
            };
            let agent = tauri::async_runtime::block_on(
                cadpad_agent::service::AgentService::open_with_bridge(
                    core.clone(),
                    app.path().app_data_dir()?.join("agent"),
                    std::sync::Arc::new(cadpad_agent::credentials::SystemCredentials),
                    std::sync::Arc::new(cadpad_agent::service::DefaultProviderFactory),
                    cadpad_agent::runner::Limits::default(),
                    Some(cadpad_agent::cli::Bridge {
                        mcp_executable,
                        connection_file: server.connection_file().to_path_buf(),
                    }),
                ),
            )?;
            app.manage(agent);
            app.manage(server);
            app.manage(core);
            app.manage(files::PendingProjectFolder::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            initialize,
            project,
            read_source,
            example,
            job,
            stop,
            menus::connect_native_menu,
            menus::update_native_menu,
            menus::popup_native_menu,
            undo,
            parameter,
            annotation,
            inspect,
            export,
            artifact,
            new_scratchpad,
            open_project,
            files::open_project_entry,
            files::configure_project_entry,
            files::project_entry_plan,
            save_project,
            check_external,
            reload_project,
            import_model,
            import_step,
            import_stl,
            workspaces,
            forget_workspace,
            forget_workspaces,
            switch_project,
            connect_view,
            complete_view,
            set_selection,
            mcp_settings,
            disconnect_external,
            connect_activity,
            connect_motion,
            motion,
            joint_coordinate,
            simulation,
            playback,
            motion_tests,
            clean_cache,
            repair_display,
            rebuild,
            interference,
            agent_settings,
            configure_agent,
            forget_agent_key,
            agent_start,
            agent_stop,
            agent_turn,
            agent_conversation,
            agent_chat,
            connect_agent
        ])
        .run(tauri::generate_context!())
        .expect("Cadpad failed to launch");
}
