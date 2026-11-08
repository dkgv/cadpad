//! Native file authorization belongs to the desktop transport, not CAD tools.
use cadpad_core::{Core, error_value, portable};
use serde_json::{Value, json};
use std::path::Path;
use std::{collections::BTreeMap, path::PathBuf};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

#[derive(Default)]
pub struct PendingProjectFolder(std::sync::Mutex<Option<PendingEntry>>);
struct PendingEntry {
    id: String,
    directory: PathBuf,
    manifest_hash: Option<String>,
    plans: BTreeMap<String, Value>,
}

fn dialog_error(message: impl ToString) -> Value {
    json!({"code":"IO_ERROR","message":message.to_string()})
}
async fn file_path(
    app: &AppHandle,
    title: &str,
    extensions: &[&str],
    save: bool,
    name: &str,
) -> Result<Option<PathBuf>, Value> {
    let (send, receive) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file().set_title(title);
    if !extensions.is_empty() {
        dialog = dialog.add_filter("CAD project", extensions);
    }
    if save {
        dialog.set_file_name(name).save_file(move |path| {
            let _ = send.send(path);
        });
    } else {
        dialog.pick_file(move |path| {
            let _ = send.send(path);
        });
    }
    receive
        .await
        .map_err(dialog_error)?
        .map(|path| path.into_path().map_err(dialog_error))
        .transpose()
}
#[tauri::command]
pub async fn new_scratchpad(core: State<'_, Core>) -> Result<Value, Value> {
    let workspace = core
        .create_workspace("local-user")
        .await
        .map_err(error_value_owned)?;
    core.get_project(&workspace)
        .await
        .map_err(error_value_owned)
}
fn error_value_owned(error: cadpad_core::Error) -> Value {
    error_value(&error)
}
#[tauri::command]
pub async fn open_project(
    app: AppHandle,
    core: State<'_, Core>,
    pending: State<'_, PendingProjectFolder>,
) -> Result<Value, Value> {
    *pending.0.lock().unwrap() = None;
    let (send, receive) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Open project folder")
        .pick_folder(move |path| {
            let _ = send.send(path);
        });
    let Some(path) = receive.await.map_err(dialog_error)? else {
        return Ok(json!({"cancelled":true}));
    };
    let directory = path
        .into_path()
        .map_err(dialog_error)?
        .canonicalize()
        .map_err(dialog_error)?;
    let config = portable::metadata_directory(&directory).map_err(error_value_owned)?;
    let legacy = config == directory && portable::read_manifest(&directory).is_ok();
    if config == directory && !legacy {
        return prepare_entry_selection(&pending, directory).await;
    }
    open_directory(&core, directory).await
}
#[tauri::command]
pub async fn open_project_entry(
    core: State<'_, Core>,
    pending: State<'_, PendingProjectFolder>,
    request_id: String,
    entry: Option<String>,
    adapter: Option<String>,
) -> Result<Value, Value> {
    let (directory, expected_manifest, plan) = {
        let selection = pending.0.lock().unwrap();
        let selection = selection.as_ref().filter(|selection| selection.id == request_id)
            .ok_or_else(||json!({"code":"INVALID_REQUEST","message":"This folder selection has expired. Open the folder again."}))?;
        (
            selection.directory.clone(),
            selection.manifest_hash.clone(),
            entry
                .as_ref()
                .and_then(|name| selection.plans.get(name))
                .cloned(),
        )
    };
    let Some(entry) = entry else {
        *pending.0.lock().unwrap() = None;
        return Ok(json!({"cancelled":true}));
    };
    let generated = if let Some(builder) = adapter {
        let plan =
            plan.ok_or_else(|| dialog_error("Wait for the entry preview before creating it"))?;
        let source = plan["options"]
            .as_array()
            .and_then(|options| {
                options
                    .iter()
                    .find(|option| option["id"] == builder && option["available"] == true)
            })
            .and_then(|option| option["source"].as_str())
            .ok_or_else(|| dialog_error("Choose an available geometry builder"))?
            .to_string();
        Some((
            plan["source_hash"]
                .as_str()
                .ok_or_else(|| dialog_error("Entry preview is missing its source hash"))?
                .to_string(),
            source,
        ))
    } else {
        None
    };
    let root = directory.clone();
    tauri::async_runtime::spawn_blocking(move || {
        cadpad_core::project_entry::configure_entry(
            &root,
            &entry,
            expected_manifest.as_deref(),
            generated
                .as_ref()
                .map(|(hash, code)| (hash.as_str(), code.as_str())),
        )
    })
    .await
    .map_err(dialog_error)?
    .map_err(error_value_owned)?;
    *pending.0.lock().unwrap() = None;
    open_directory(&core, directory).await
}
async fn prepare_entry_selection(
    pending: &PendingProjectFolder,
    directory: PathBuf,
) -> Result<Value, Value> {
    let root = directory.clone();
    let (entries, manifest_hash, current_entry) =
        tauri::async_runtime::spawn_blocking(move || -> cadpad_core::Result<_> {
            Ok((
                portable::python_entry_points(&root)?,
                cadpad_core::project_entry::manifest_hash(&root)?,
                portable::read_manifest(&root)
                    .ok()
                    .map(|manifest| manifest.entry_point),
            ))
        })
        .await
        .map_err(dialog_error)?
        .map_err(error_value_owned)?;
    if entries.is_empty() {
        return Err(
            json!({"code":"INVALID_REQUEST","message":"This folder contains no Python models. Use Import model for STEP/STL."}),
        );
    }
    let id = uuid::Uuid::new_v4().to_string();
    let folder = directory
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    *pending.0.lock().unwrap() = Some(PendingEntry {
        id: id.clone(),
        directory,
        manifest_hash,
        plans: BTreeMap::new(),
    });
    Ok(
        json!({"choose_entry":{"request_id":id,"folder":folder,"entries":entries,"current_entry":current_entry}}),
    )
}
#[tauri::command]
pub async fn configure_project_entry(
    core: State<'_, Core>,
    pending: State<'_, PendingProjectFolder>,
    workspace: String,
) -> Result<Value, Value> {
    let directory = core
        .project_directory(&workspace)
        .await
        .map_err(error_value_owned)?;
    prepare_entry_selection(&pending, directory).await
}
#[tauri::command]
pub async fn project_entry_plan(
    core: State<'_, Core>,
    pending: State<'_, PendingProjectFolder>,
    request_id: String,
    entry: String,
) -> Result<Value, Value> {
    let directory = {
        let selection = pending.0.lock().unwrap();
        selection
            .as_ref()
            .filter(|selection| selection.id == request_id)
            .map(|selection| selection.directory.clone())
            .ok_or_else(|| dialog_error("Entry setup has closed. Open it again."))?
    };
    let plan = core
        .analyze_project_entry(directory, entry.clone())
        .await
        .map_err(error_value_owned)?;
    let mut selection = pending.0.lock().unwrap();
    let selection = selection
        .as_mut()
        .filter(|selection| selection.id == request_id)
        .ok_or_else(|| dialog_error("Entry setup has closed. Open it again."))?;
    if selection.plans.len() >= 16 {
        selection.plans.pop_first();
    }
    selection.plans.insert(entry, plan.clone());
    Ok(plan)
}
async fn open_directory(core: &Core, directory: PathBuf) -> Result<Value, Value> {
    let opened = core
        .open_project(directory, "local-user")
        .await
        .map_err(error_value_owned)?;
    core.focus_workspace(&opened.workspace_id)
        .await
        .map_err(error_value_owned)?;
    let job = core
        .build(
            &opened.workspace_id,
            &opened.base_revision,
            &opened.draft.id,
            "local-user",
        )
        .await
        .map_err(error_value_owned)?;
    Ok(
        json!({"project":core.get_project(&opened.workspace_id).await.map_err(error_value_owned)?,"job":job}),
    )
}
#[tauri::command]
pub async fn save_project(
    app: AppHandle,
    core: State<'_, Core>,
    workspace: String,
    revision: String,
    save_as: bool,
) -> Result<Value, Value> {
    let project = core
        .get_project(&workspace)
        .await
        .map_err(error_value_owned)?;
    core.check_python_origin_before_save(&workspace)
        .await
        .map_err(error_value_owned)?;
    let destination = if save_as || project["saved_revision"].is_null() {
        let Some(path) =
            file_path(&app, "Save project as a new folder", &[], true, "Untitled").await?
        else {
            return Ok(json!({"cancelled":true}));
        };
        Some(path)
    } else {
        None
    };
    core.save_project_folder(&workspace, &revision, destination)
        .await
        .map_err(error_value_owned)?;
    core.get_project(&workspace)
        .await
        .map_err(error_value_owned)
}
#[tauri::command]
pub async fn check_external(core: State<'_, Core>, workspace: String) -> Result<Value, Value> {
    core.scan_external_changes(&workspace)
        .await
        .map_err(error_value_owned)
}
#[tauri::command]
pub async fn reload_project(
    core: State<'_, Core>,
    workspace: String,
    revision: String,
    fingerprint: String,
    resolutions: Option<BTreeMap<String, cadpad_core::origins::Resolution>>,
) -> Result<Value, Value> {
    let draft = core
        .stage_external_changes_resolved(
            &workspace,
            &revision,
            &fingerprint,
            "local-user",
            resolutions.unwrap_or_default(),
        )
        .await
        .map_err(error_value_owned)?;
    core.build(&workspace, &revision, &draft.id, "local-user")
        .await
        .map(|job| json!(job))
        .map_err(error_value_owned)
}
async fn stage_imported_asset(
    app: &AppHandle,
    core: &Core,
    workspace: &str,
    revision: &str,
    title: &str,
    extensions: &[&str],
) -> Result<Option<(cadpad_core::Draft, String)>, Value> {
    let Some(path) = file_path(app, title, extensions, false, "").await? else {
        return Ok(None);
    };
    let name = path
        .file_name()
        .ok_or_else(|| dialog_error("Missing asset name"))?
        .to_string_lossy()
        .to_string();
    portable::relative(&name).map_err(error_value_owned)?;
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        portable::read_bounded(&path, portable::ASSET_LIMIT)
    })
    .await
    .map_err(dialog_error)?
    .map_err(error_value_owned)?;
    let digest = portable::bytes_hash(&bytes);
    let name = format!("imports/{}/{}", &digest[..12], name);
    let draft = core
        .stage_asset(workspace, revision, None, &name, bytes, "local-user")
        .await
        .map_err(error_value_owned)?;
    Ok(Some((draft, name)))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ImportKind {
    Step,
    Stl,
}
fn import_kind(name: &str) -> Result<ImportKind, Value> {
    match Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "step" | "stp" => Ok(ImportKind::Step),
        "stl" => Ok(ImportKind::Stl),
        _ => Err(dialog_error("Choose a STEP or STL file")),
    }
}
async fn build_import(
    core: &Core,
    workspace: &str,
    revision: &str,
    draft: cadpad_core::Draft,
    name: String,
    kind: ImportKind,
) -> Result<Value, Value> {
    let source = core
        .read_source(workspace, &draft.id, &draft.inputs.entry_point)
        .await
        .map_err(error_value_owned)?;
    let quoted = serde_json::to_string(&name).map_err(dialog_error)?;
    let unique = &draft.id[..8];
    let base = source["source"].as_str().unwrap();
    let source = match kind {
        ImportKind::Step => {
            format!("{base}\nshow(import_step({quoted}), name=\"imported_{unique}\")\n")
        }
        ImportKind::Stl => {
            let part = format!("part_{unique}");
            let reference = format!("stl_ref_{unique}");
            // Keep STL as a visual reference with report-mode reconstruction checks.
            format!(
                "{base}\nshow(import_stl({quoted}), name=\"{reference}\", kind=\"surface\")\n\
# Reconstruct a parametric solid named \"{part}\" that matches the STL, then set on_fail=\"error\".\n\
fit_part(\"{part}\", reference_stl={quoted}, strictness=\"strict\", on_fail=\"report\",\n\
         tolerances=dict(bbox_mm=0.5, volume_rel=0.02, section_mm=0.5, hausdorff_mm=0.5))\n"
            )
        }
    };
    core.update_source(
        workspace,
        revision,
        Some(&draft.id),
        BTreeMap::from([(draft.inputs.entry_point.clone(), source)]),
        "local-user",
    )
    .await
    .map_err(error_value_owned)?;
    core.build(workspace, revision, &draft.id, "local-user")
        .await
        .map(|job| json!(job))
        .map_err(error_value_owned)
}
#[tauri::command]
pub async fn import_model(
    app: AppHandle,
    core: State<'_, Core>,
    workspace: String,
    revision: String,
) -> Result<Value, Value> {
    let Some((draft, name)) = stage_imported_asset(
        &app,
        &*core,
        &workspace,
        &revision,
        "Import a model",
        &["step", "stp", "stl", "STEP", "STP", "STL"],
    )
    .await?
    else {
        return Ok(json!({"cancelled": true}));
    };
    let kind = import_kind(&name)?;
    build_import(&core, &workspace, &revision, draft, name, kind).await
}
#[tauri::command]
pub async fn import_step(
    app: AppHandle,
    core: State<'_, Core>,
    workspace: String,
    revision: String,
) -> Result<Value, Value> {
    let Some((draft, name)) = stage_imported_asset(
        &app,
        &*core,
        &workspace,
        &revision,
        "Import a STEP part",
        &["step", "stp", "STEP", "STP"],
    )
    .await?
    else {
        return Ok(json!({"cancelled": true}));
    };
    build_import(&core, &workspace, &revision, draft, name, ImportKind::Step).await
}
#[tauri::command]
pub async fn import_stl(
    app: AppHandle,
    core: State<'_, Core>,
    workspace: String,
    revision: String,
) -> Result<Value, Value> {
    let Some((draft, name)) = stage_imported_asset(
        &app,
        &*core,
        &workspace,
        &revision,
        "Import an STL reference for reconstruction",
        &["stl", "STL"],
    )
    .await?
    else {
        return Ok(json!({"cancelled": true}));
    };
    build_import(&core, &workspace, &revision, draft, name, ImportKind::Stl).await
}
#[tauri::command]
pub async fn export(
    app: AppHandle,
    core: State<'_, Core>,
    workspace: String,
    revision: String,
    format: String,
    handle: Option<String>,
    pose_version: Option<u64>,
) -> Result<Value, Value> {
    let exported = if let Some(handle) = handle {
        if !matches!(format.as_str(), "STEP" | "STL")
            || !handle.ends_with(&format!(".{}", format.to_lowercase()))
        {
            return Err(
                json!({"code":"INVALID_REQUEST","message":"Invalid export format or artifact handle"}),
            );
        }
        json!({"handle":handle,"revision":revision,"format":format})
    } else {
        let pose_version = match pose_version {
            Some(version) => version,
            None => {
                core.get_motion(&workspace, &revision)
                    .await
                    .map_err(error_value_owned)?
                    .pose_version
            }
        };
        core.query(
            &workspace,
            &revision,
            "export",
            json!({"format":format,"pose_version":pose_version}),
        )
        .await
        .map_err(error_value_owned)?
    };
    let frozen = core
        .freeze_export(&workspace, &revision, exported["handle"].as_str().unwrap())
        .await
        .map_err(error_value_owned)?;
    let extension = format.to_lowercase();
    let Some(path) = file_path(
        &app,
        "Export committed assembly",
        &[&extension],
        true,
        &format!("cadpad.{extension}"),
    )
    .await?
    else {
        return Ok(json!({"cancelled":true}));
    };
    // Retain the selected export across the dialog without loading it into RAM.
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    tauri::async_runtime::spawn_blocking(move || frozen.save_to(&path))
        .await
        .map_err(dialog_error)?
        .map_err(error_value_owned)?;
    Ok(json!({"name":name,"revision":revision,"format":format,"units":"mm"}))
}

#[tauri::command]
pub async fn workspaces(core: State<'_, Core>) -> Result<Value, Value> {
    Ok(core.workspace_summaries().await)
}
#[tauri::command]
pub async fn forget_workspace(core: State<'_, Core>, workspace: String) -> Result<(), Value> {
    core.forget_workspace(&workspace)
        .await
        .map_err(error_value_owned)
}
#[tauri::command]
pub async fn forget_workspaces(core: State<'_, Core>) -> Result<usize, Value> {
    core.forget_workspaces().await.map_err(error_value_owned)
}
#[tauri::command]
pub async fn switch_project(core: State<'_, Core>, workspace: String) -> Result<Value, Value> {
    core.focus_workspace(&workspace)
        .await
        .map_err(error_value_owned)?;
    core.get_project(&workspace)
        .await
        .map_err(error_value_owned)
}

#[cfg(test)]
mod import_tests {
    use super::*;
    #[test]
    fn combined_import_routes_supported_extensions() {
        for name in ["part.step", "part.stp", "part.STEP", "part.StP"] {
            assert_eq!(import_kind(name).unwrap(), ImportKind::Step);
        }
        for name in ["mesh.stl", "mesh.STL", "mesh.StL"] {
            assert_eq!(import_kind(name).unwrap(), ImportKind::Stl);
        }
        assert!(import_kind("model.obj").is_err());
        assert!(import_kind("model").is_err());
    }
}
