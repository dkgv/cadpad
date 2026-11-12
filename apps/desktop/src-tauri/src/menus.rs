use serde::Deserialize;
use std::{collections::HashMap, sync::Mutex};
use tauri::{
    AppHandle, Manager, State, Wry,
    ipc::Channel,
    menu::{CheckMenuItem, ContextMenu, Menu, MenuItem, PredefinedMenuItem, Submenu},
};

struct OpenDocuments {
    separator: Option<PredefinedMenuItem<Wry>>,
    items: Vec<CheckMenuItem<Wry>>,
    signature: String,
}
pub struct NativeMenus {
    channel: Mutex<Option<Channel<String>>>,
    actions: HashMap<&'static str, MenuItem<Wry>>,
    dimensions: CheckMenuItem<Wry>,
    advanced: CheckMenuItem<Wry>,
    inspectors: Vec<(&'static str, CheckMenuItem<Wry>)>,
    selection: Vec<(&'static str, CheckMenuItem<Wry>)>,
    scopes: Vec<(&'static str, CheckMenuItem<Wry>)>,
    window_menu: Submenu<Wry>,
    documents: Mutex<OpenDocuments>,
}
pub fn install(app: &mut tauri::App) -> tauri::Result<()> {
    let mut actions = HashMap::new();
    let mut action =
        |id: &'static str, label: &str, shortcut: Option<&str>| -> tauri::Result<MenuItem<Wry>> {
            let item = MenuItem::with_id(app, format!("cadpad.{id}"), label, true, shortcut)?;
            actions.insert(id, item.clone());
            Ok(item)
        };
    let settings = action("settings", "Settings…", Some("CmdOrCtrl+,"))?;
    let application = Submenu::with_items(
        app,
        "Cadpad",
        true,
        &[
            &PredefinedMenuItem::about(
                app,
                None,
                Some(tauri::menu::AboutMetadata {
                    name: Some("Cadpad".into()),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                    ..Default::default()
                }),
            )?,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;
    let new_model = action("new-model", "New Model", Some("CmdOrCtrl+T"))?;
    let new_project = action("new-project", "New Project", Some("CmdOrCtrl+N"))?;
    let open = action("open", "Open…", Some("CmdOrCtrl+O"))?;
    let save = action("save", "Save", Some("CmdOrCtrl+S"))?;
    let save_as = action("save-as", "Save As…", Some("CmdOrCtrl+Shift+S"))?;
    let import = action("import", "Import Model…", Some("CmdOrCtrl+Shift+I"))?;
    let export_step = action("export-step", "STEP…", None)?;
    let export_stl = action("export-stl", "STL…", None)?;
    let export = Submenu::with_items(app, "Export", true, &[&export_step, &export_stl])?;
    let rebuild = action("rebuild", "Rebuild Model", Some("CmdOrCtrl+R"))?;
    let close_tab = action("close-tab", "Close Tab", Some("CmdOrCtrl+W"))?;
    let file = Submenu::with_items(
        app,
        "File",
        true,
        &[
            &new_model,
            &new_project,
            &open,
            &PredefinedMenuItem::separator(app)?,
            &save,
            &save_as,
            &PredefinedMenuItem::separator(app)?,
            &import,
            &export,
            &PredefinedMenuItem::separator(app)?,
            &close_tab,
        ],
    )?;
    let undo = action("undo", "Undo Model Change", Some("CmdOrCtrl+Z"))?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &undo,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let dimensions = CheckMenuItem::with_id(
        app,
        "cadpad.dimensions",
        "Show Dimensions",
        true,
        false,
        Some("CmdOrCtrl+Shift+M"),
    )?;
    let advanced = CheckMenuItem::with_id(
        app,
        "cadpad.advanced",
        "Advanced View Controls",
        true,
        false,
        None::<&str>,
    )?;
    let fit = action("fit", "Fit Model", Some("CmdOrCtrl+0"))?;
    let iso = action("iso", "Isometric", Some("CmdOrCtrl+1"))?;
    let front = action("front", "Front", Some("CmdOrCtrl+2"))?;
    let top = action("top", "Top", Some("CmdOrCtrl+3"))?;
    let right = action("right", "Right", Some("CmdOrCtrl+4"))?;
    let camera = Submenu::with_items(app, "Camera", true, &[&iso, &front, &top, &right])?;
    let mut selection = Vec::new();
    for (id, label) in [
        ("face", "Pick Faces"),
        ("edge", "Pick Edges"),
        ("part", "Pick Objects"),
    ] {
        selection.push((
            id,
            CheckMenuItem::with_id(
                app,
                format!("cadpad.select-{id}"),
                label,
                true,
                id == "face",
                None::<&str>,
            )?,
        ));
    }
    let selection_menu = Submenu::with_items(
        app,
        "Select",
        true,
        &selection
            .iter()
            .map(|(_, item)| item as &dyn tauri::menu::IsMenuItem<Wry>)
            .collect::<Vec<_>>(),
    )?;
    let mut scopes = Vec::new();
    for (id, label) in [
        ("selected", "Selected Object"),
        ("pinned", "Pinned Dimensions"),
        ("all", "All Visible Objects"),
    ] {
        scopes.push((
            id,
            CheckMenuItem::with_id(
                app,
                format!("cadpad.scope-{id}"),
                label,
                true,
                id == "selected",
                None::<&str>,
            )?,
        ));
    }
    let scope_menu = Submenu::with_items(
        app,
        "Dimension Scope",
        true,
        &scopes
            .iter()
            .map(|(_, item)| item as &dyn tauri::menu::IsMenuItem<Wry>)
            .collect::<Vec<_>>(),
    )?;
    let mut inspectors = Vec::new();
    for (id, label, shortcut) in [
        ("model", "Explorer", "CmdOrCtrl+Alt+1"),
        ("motion", "Motion Panel", "CmdOrCtrl+Alt+2"),
        ("assistant", "Assistant", "CmdOrCtrl+Alt+3"),
    ] {
        inspectors.push((
            id,
            CheckMenuItem::with_id(
                app,
                format!("cadpad.panel-{id}"),
                label,
                true,
                id == "model",
                Some(shortcut),
            )?,
        ));
    }
    let inspector = Submenu::with_items(
        app,
        "Panels",
        true,
        &inspectors
            .iter()
            .map(|(_, item)| item as &dyn tauri::menu::IsMenuItem<Wry>)
            .collect::<Vec<_>>(),
    )?;
    let split_right = action("split-right", "Split Editor Right", None)?;
    let split_down = action("split-down", "Split Editor Down", None)?;
    let view = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &fit,
            &camera,
            &dimensions,
            &scope_menu,
            &advanced,
            &PredefinedMenuItem::separator(app)?,
            &inspector,
            &PredefinedMenuItem::separator(app)?,
            &split_right,
            &split_down,
        ],
    )?;
    let isolate = action(
        "isolate",
        "Isolate Selected Part",
        Some("CmdOrCtrl+Shift+H"),
    )?;
    let show_all = action("show-all", "Show All Parts", Some("CmdOrCtrl+Alt+H"))?;
    let model = Submenu::with_items(
        app,
        "Model",
        true,
        &[
            &selection_menu,
            &PredefinedMenuItem::separator(app)?,
            &isolate,
            &show_all,
            &PredefinedMenuItem::separator(app)?,
            &rebuild,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            &PredefinedMenuItem::fullscreen(app, None)?,
        ],
    )?;
    app.set_menu(Menu::with_items(
        app,
        &[&application, &file, &edit, &view, &model, &window],
    )?)?;
    app.manage(NativeMenus {
        channel: Mutex::new(None),
        actions,
        dimensions,
        advanced,
        inspectors,
        selection,
        scopes,
        window_menu: window,
        documents: Mutex::new(OpenDocuments {
            separator: None,
            items: Vec::new(),
            signature: String::new(),
        }),
    });
    Ok(())
}
pub fn dispatch(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let Some(action) = event.id().as_ref().strip_prefix("cadpad.") else {
        return;
    };
    if let Some(state) = app.try_state::<NativeMenus>() {
        let channel = state.channel.lock().unwrap();
        if let Some(channel) = channel.as_ref() {
            let _ = channel.send(action.to_string());
        }
    }
}
#[tauri::command]
pub fn connect_native_menu(state: State<'_, NativeMenus>, channel: Channel<String>) {
    *state.channel.lock().unwrap() = Some(channel);
}
#[derive(Default, Deserialize)]
pub struct MenuStatus {
    pub has_project: bool,
    pub has_parts: bool,
    pub working: bool,
    pub busy: bool,
    pub can_undo: bool,
    pub editing: bool,
    pub dimensions: bool,
    pub advanced: bool,
    pub document_name: String,
    #[serde(default)]
    pub inspector: String,
    #[serde(default)]
    pub model_browser_open: Option<bool>,
    #[serde(default)]
    pub motion_panel_open: Option<bool>,
    #[serde(default)]
    pub has_selection: bool,
    #[serde(default)]
    pub has_hidden_parts: bool,
    #[serde(default)]
    pub selection_mode: String,
    #[serde(default)]
    pub dimension_scope: String,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceMenuEntry>,
}
fn enabled(id: &str, status: &MenuStatus) -> bool {
    match id {
        "close-tab" | "split-right" | "split-down" => status.has_project,
        "save" | "save-as" => status.has_project && !status.busy,
        "import" | "rebuild" => status.has_project && !status.working,
        "export-step" | "export-stl" | "fit" | "iso" | "front" | "top" | "right" => {
            status.has_parts
        }
        "undo" => status.editing || status.can_undo && !status.working,
        "isolate" => status.has_parts && status.has_selection,
        "show-all" => status.has_parts && status.has_hidden_parts,
        "new-model" | "new-project" | "open" => !status.busy,
        _ => true,
    }
}

#[tauri::command]
pub fn update_native_menu(
    app: AppHandle,
    state: State<'_, NativeMenus>,
    status: MenuStatus,
) -> Result<(), String> {
    for (id, item) in &state.actions {
        let enabled = enabled(id, &status);
        item.set_enabled(enabled).map_err(|e| e.to_string())?;
    }
    state.actions["undo"]
        .set_text(if status.editing {
            "Undo"
        } else {
            "Undo Model Change"
        })
        .map_err(|e| e.to_string())?;
    state
        .dimensions
        .set_checked(status.dimensions)
        .map_err(|e| e.to_string())?;
    state
        .advanced
        .set_checked(status.advanced)
        .map_err(|e| e.to_string())?;
    for (id, item) in &state.inspectors {
        item.set_checked(match *id {
            "model" => status
                .model_browser_open
                .unwrap_or(status.inspector == "model"),
            "motion" => status
                .motion_panel_open
                .unwrap_or(status.inspector == "motion"),
            "assistant" => true,
            _ => false,
        })
        .map_err(|e| e.to_string())?;
    }
    for (id, item) in &state.selection {
        item.set_checked(*id == status.selection_mode)
            .map_err(|e| e.to_string())?;
    }
    for (id, item) in &state.scopes {
        item.set_checked(*id == status.dimension_scope)
            .map_err(|e| e.to_string())?;
    }
    if let Some(window) = app.get_webview_window("main") {
        window
            .set_title(&status.document_name)
            .map_err(|e| e.to_string())?;
    }
    sync_documents(&app, &state, &status)
}
fn sync_documents(app: &AppHandle, menus: &NativeMenus, status: &MenuStatus) -> Result<(), String> {
    let signature = status
        .workspaces
        .iter()
        .map(|workspace| {
            format!(
                "{}|{}|{}|{}|{}",
                workspace.id, workspace.name, workspace.selected, workspace.running, status.busy
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut documents = menus.documents.lock().unwrap();
    if documents.signature == signature {
        return Ok(());
    }
    for item in documents.items.drain(..) {
        menus.window_menu.remove(&item).map_err(|e| e.to_string())?;
    }
    if let Some(separator) = documents.separator.take() {
        menus
            .window_menu
            .remove(&separator)
            .map_err(|e| e.to_string())?;
    }
    documents.signature = signature;
    if status.workspaces.is_empty() {
        return Ok(());
    }
    let separator = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    menus
        .window_menu
        .append(&separator)
        .map_err(|e| e.to_string())?;
    documents.separator = Some(separator);
    for workspace in &status.workspaces {
        let label = if workspace.running {
            format!("{} — Working…", workspace.name)
        } else {
            workspace.name.clone()
        };
        let item = CheckMenuItem::with_id(
            app,
            format!("cadpad.workspace:{}", workspace.id),
            label,
            !status.busy,
            workspace.selected,
            None::<&str>,
        )
        .map_err(|e| e.to_string())?;
        menus.window_menu.append(&item).map_err(|e| e.to_string())?;
        documents.items.push(item);
    }
    Ok(())
}
#[tauri::command]
pub fn popup_native_menu(
    window: tauri::WebviewWindow,
    state: State<'_, NativeMenus>,
    kind: &str,
    x: f64,
    y: f64,
) -> Result<(), String> {
    let menu = match kind {
        "selection" | "scope" => {
            let items = if kind == "selection" {
                &state.selection
            } else {
                &state.scopes
            };
            Menu::with_items(
                &window,
                &items
                    .iter()
                    .map(|(_, item)| item as &dyn tauri::menu::IsMenuItem<Wry>)
                    .collect::<Vec<_>>(),
            )
        }
        "export" => Menu::with_items(
            &window,
            &[&state.actions["export-step"], &state.actions["export-stl"]],
        ),
        _ => return Err("Unknown native menu".into()),
    }
    .map_err(|e| e.to_string())?;
    menu.popup_at(
        window.as_ref().window().clone(),
        tauri::LogicalPosition::new(x, y),
    )
    .map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct WorkspaceMenuEntry {
    id: String,
    name: String,
    selected: bool,
    running: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visibility_commands_follow_selection_and_hidden_parts() {
        let mut status = MenuStatus::default();
        assert!(!enabled("isolate", &status));
        assert!(!enabled("show-all", &status));
        status.has_parts = true;
        status.has_selection = true;
        assert!(enabled("isolate", &status));
        assert!(!enabled("show-all", &status));
        status.has_hidden_parts = true;
        status.working = true;
        assert!(enabled("isolate", &status));
        assert!(enabled("show-all", &status));
    }
    #[test]
    fn document_navigation_and_exports_stay_available_during_background_work() {
        let mut status = MenuStatus {
            has_project: true,
            has_parts: true,
            working: true,
            ..Default::default()
        };
        for id in [
            "new-model",
            "new-project",
            "open",
            "save",
            "save-as",
            "export-step",
            "export-stl",
        ] {
            assert!(enabled(id, &status), "{id}");
        }
        for id in ["import", "rebuild", "undo"] {
            assert!(!enabled(id, &status), "{id}");
        }
        status.busy = true;
        for id in ["new-model", "new-project", "open", "save", "save-as"] {
            assert!(!enabled(id, &status), "{id}");
        }
    }
    #[test]
    fn text_undo_is_independent_of_model_undo_and_empty_models_cannot_export() {
        let mut status = MenuStatus {
            editing: true,
            working: true,
            ..Default::default()
        };
        assert!(enabled("undo", &status));
        status.editing = false;
        assert!(!enabled("undo", &status));
        status.working = false;
        status.can_undo = true;
        assert!(enabled("undo", &status));
        for id in ["save", "export-step", "export-stl", "fit"] {
            assert!(!enabled(id, &status), "{id}");
        }
    }
}
