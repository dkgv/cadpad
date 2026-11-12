//! Desktop-authorized entry adapters. Analysis parses source without running it.
use crate::{Core, Error, Result, portable, worker::Worker};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

/// Recover the source file + builder a generated Cadpad adapter wraps.
/// Used so Explorer can select the real entry even when the canvas is the project root.
pub fn generated_model_entry(source: &str) -> Option<(String, String)> {
    if !source.starts_with("# Generated Cadpad entry point.") {
        return None;
    }
    let entry = source.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix("path = Path(__file__).parent / \"")?;
        rest.strip_suffix('"').map(str::to_string)
    })?;
    // Prefer the primary builder call; group adapters synthesize a dict and stay anonymous.
    let builder = source.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix("result = getattr(module, \"")?;
        let name = rest.split('"').next()?.to_string();
        (!name.is_empty()).then_some(name)
    })?;
    crate::validate_path(&entry).ok()?;
    Some((entry, builder))
}

pub(crate) fn inputs_model_entry(inputs: &crate::Inputs) -> Option<(String, String)> {
    inputs
        .files
        .get(&inputs.entry_point)
        .and_then(|source| generated_model_entry(source))
}

// Ignore private view adapters and build-discovered dependencies when comparing user content.
// Comparing borrowed inputs avoids copying scene metadata or Python source strings.
pub(crate) fn same_project_content(current: &crate::Inputs, saved: &crate::Inputs) -> bool {
    let generated_entry = |inputs: &crate::Inputs| {
        inputs.files.get(&inputs.entry_point).is_some_and(|source| {
            source.starts_with("# Generated Cadpad entry point.")
                || source == portable::PROJECT_BROWSER_SOURCE
        })
    };
    let current_generated = generated_entry(current);
    let saved_generated = generated_entry(saved);
    current
        .files
        .iter()
        .filter(|(path, _)| !current_generated || *path != &current.entry_point)
        .eq(saved
            .files
            .iter()
            .filter(|(path, _)| !saved_generated || *path != &saved.entry_point))
        && (current.entry_point == saved.entry_point || current_generated || saved_generated)
        && current.assets == saved.assets
        && current.parameters == saved.parameters
        && current.annotations == saved.annotations
        && current.mechanism == saved.mechanism
}

impl Core {
    pub async fn project_directory(&self, workspace: &str) -> Result<PathBuf> {
        let all = self.workspaces.lock().await;
        let ws = all
            .get(workspace)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        ws.stored
            .project
            .as_ref()
            .map(|binding| binding.directory.clone())
            .or_else(|| {
                ws.stored
                    .python_origin
                    .as_ref()
                    .and_then(|origin| origin.entry.parent().map(Path::to_path_buf))
            })
            .ok_or_else(|| {
                Error::domain(
                    "INVALID_REQUEST",
                    "Save this model to a project folder first",
                )
            })
    }
    pub async fn analyze_project_entry(&self, directory: PathBuf, entry: String) -> Result<Value> {
        self.analyze_project_entry_builder(directory, entry, None)
            .await
    }
    pub async fn analyze_project_entry_builder(
        &self,
        directory: PathBuf,
        entry: String,
        builder: Option<String>,
    ) -> Result<Value> {
        let text = tokio::task::spawn_blocking(move || -> Result<_> {
            crate::validate_path(&entry)?;
            let root = directory.canonicalize()?;
            let bytes = portable::read_bounded(
                &portable::checked_path(&root, &entry)?,
                portable::SOURCE_LIMIT,
            )?;
            let digest = portable::bytes_hash(&bytes);
            let source = String::from_utf8(bytes)
                .map_err(|_| Error::domain("INVALID_REQUEST", "Python source must be UTF-8"))?;
            Ok((entry, source, digest))
        })
        .await
        .map_err(|_| Error::domain("IO_ERROR", "Could not read Python source"))??;
        let mut plan = self
            .entry_analysis_request(
                "analyze_entry",
                json!({"entry":text.0,"source":text.1,"builder":builder}),
            )
            .await?;
        plan["source_hash"] = json!(text.2);
        Ok(plan)
    }
    pub async fn index_project_sources(&self, workspace: &str, revision: &str) -> Result<Value> {
        let sources = {
            let all = self.workspaces.lock().await;
            let ws = all
                .get(workspace)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            ws.stored
                .revisions
                .get(revision)
                .ok_or_else(|| Error::domain("REVISION_CONFLICT", "Unknown revision"))?
                .inputs
                .files
                .iter()
                // Build adapters belong to the runtime, not the source catalog.
                // Check their content so similarly named user files stay visible.
                .filter(|(_, source)| {
                    !source.starts_with("# Generated Cadpad entry point.")
                        && source.as_str() != portable::PROJECT_BROWSER_SOURCE
                })
                .map(|(path, source)| (path.clone(), source.clone()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        self.entry_analysis_request("index_sources", json!({"sources": sources}))
            .await
    }
    /// STL files already stored in the project folder. Scratchpads have none.
    pub async fn project_meshes(&self, workspace: &str) -> Result<Value> {
        let directory = match self.project_directory(workspace).await {
            Ok(directory) => directory,
            Err(error) if error.code() == "INVALID_REQUEST" => return Ok(json!({"files": []})),
            Err(error) => return Err(error),
        };
        let files = tokio::task::spawn_blocking(move || portable::list_mesh_files(&directory))
            .await
            .map_err(|_| Error::domain("IO_ERROR", "Could not list project STL files"))??;
        Ok(json!({"files": files}))
    }
    /// Triangle count and bounds for one project STL. The file is not imported.
    pub async fn inspect_project_mesh(&self, workspace: &str, path: &str) -> Result<Value> {
        portable::mesh_path(path)?;
        let directory = self.project_directory(workspace).await?;
        let relative = path.to_owned();
        let (bytes, summary) = tokio::task::spawn_blocking(move || -> Result<_> {
            let root = directory.canonicalize()?;
            let file = portable::checked_path(&root, &relative)?;
            let canonical = file.canonicalize()?;
            if !canonical.starts_with(&root) {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Choose an STL file inside this project",
                ));
            }
            let bytes = portable::read_bounded(&canonical, portable::ASSET_LIMIT)?;
            let summary = portable::summarize_stl(&bytes)?;
            Ok((bytes.len(), summary))
        })
        .await
        .map_err(|_| Error::domain("IO_ERROR", "Could not inspect the STL file"))??;
        Ok(json!({
            "path": path,
            "bytes": bytes,
            "format": summary.format,
            "triangles": summary.triangles,
            "min": summary.min,
            "max": summary.max,
            "size": summary.size,
        }))
    }
    pub async fn model_parent(&self, workspace: &str) -> Option<String> {
        self.workspaces
            .lock()
            .await
            .get(workspace)
            .and_then(|ws| ws.stored.model_parent.clone())
    }
    pub async fn model_source_dirty(&self, workspace: &str) -> bool {
        let all = self.workspaces.lock().await;
        let Some(ws) = all.get(workspace) else {
            return false;
        };
        if ws.stored.model_parent.is_none() {
            return false;
        }
        let Some(binding) = ws.stored.project.as_ref() else {
            return false;
        };
        let Some(saved) = ws.stored.revisions.get(&binding.saved_revision) else {
            return false;
        };
        let current = &ws.stored.revisions[&ws.stored.current].inputs;
        let previous = &saved.inputs;
        let files = |inputs: &crate::Inputs| {
            inputs
                .files
                .iter()
                .filter(|(path, _)| *path != &current.entry_point)
                .map(|(path, value)| (path.clone(), value.clone()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        files(current) != files(previous)
            || current.assets != previous.assets
            || current.parameters != previous.parameters
            || current.annotations != previous.annotations
            || current.mechanism != previous.mechanism
    }
    pub async fn open_project_model(
        &self,
        workspace: &str,
        revision: &str,
        entry: &str,
        builder: &str,
        actor: &str,
    ) -> Result<(String, crate::Job)> {
        let workspace_id = crate::id();
        let base = crate::id();
        {
            let mut all = self.workspaces.lock().await;
            self.ensure_actor_active(actor)?;
            let parent = all
                .get(workspace)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if parent.stored.current != revision {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Project changed while opening a model",
                ));
            }
            let mut stored = parent.stored.clone();
            stored.model_parent = Some(
                stored
                    .model_parent
                    .clone()
                    .unwrap_or_else(|| workspace.into()),
            );
            stored.id = workspace_id.clone();
            stored.current = base.clone();
            let mut initial = stored.revisions[revision].clone();
            initial.id = base.clone();
            initial.parent = Some(revision.into());
            initial.model = json!({"parts":[]});
            stored.revisions.insert(base.clone(), initial);
            stored.drafts.clear();
            stored.jobs.clear();
            stored.job_snapshots.clear();
            stored.geometry_cache = serde_json::Value::Null;
            stored.recordings.clear();
            stored.recording_imports.clear();
            self.persist(&stored).await?;
            all.insert(
                workspace_id.clone(),
                crate::Workspace {
                    stored,
                    motion: None,
                    recovery: std::sync::Arc::new(tokio::sync::Mutex::new(())),
                    display_epoch: std::sync::Arc::new(crate::id()),
                    worker: None,
                    worker_last_used: 0,
                    cancellation: std::collections::HashMap::new(),
                },
            );
        }
        match self
            .work_on_project_builder(&workspace_id, &base, entry, builder, actor)
            .await
        {
            Ok(job) => Ok((workspace_id, job)),
            Err(error) => {
                let _ = self.forget_workspace(&workspace_id).await;
                Err(error)
            }
        }
    }
    pub async fn work_on_project_builder(
        &self,
        workspace: &str,
        revision: &str,
        entry: &str,
        builder: &str,
        actor: &str,
    ) -> Result<crate::Job> {
        let source = self.read_source(workspace, revision, entry).await?;
        let mut group_builders = Vec::new();
        if builder == "$group" {
            let index = self.index_project_sources(workspace, revision).await?;
            if let Some(file) = index["files"]
                .as_array()
                .and_then(|files| files.iter().find(|file| file["path"] == entry))
            {
                if let Some(builders) = file["builders"].as_array() {
                    for candidate in builders {
                        if candidate["available"] == true
                            && matches!(
                                candidate["kind"].as_str(),
                                Some("geometry_candidate" | "part" | "assembly")
                            )
                        {
                            if let Some(name) = candidate["id"]
                                .as_str()
                                .filter(|name| !name.contains(':') && !name.starts_with('$'))
                            {
                                group_builders.push(name.to_string());
                            }
                        }
                    }
                }
            }
        }
        let plan = self
            .entry_analysis_request(
                "analyze_entry",
                json!({"entry":entry,"source":source["source"],"builder":builder,"group_builders":group_builders}),
            )
            .await?;
        let adapter = plan["options"]
            .as_array()
            .and_then(|options| {
                options
                    .iter()
                    .find(|option| option["id"] == builder && option["available"] == true)
            })
            .and_then(|option| option["source"].as_str())
            .ok_or_else(|| {
                Error::domain("INVALID_REQUEST", "Choose an available geometry builder")
            })?
            .to_string();
        let draft = {
            let mut all = self.workspaces.lock().await;
            self.ensure_actor_active(actor)?;
            let ws = all
                .get_mut(workspace)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != revision {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Project changed while selecting a builder",
                ));
            }
            let inputs = ws.stored.revisions[revision].inputs.clone();
            let mut draft = Self::input_draft(inputs, revision, actor);
            let existing = &draft.inputs.entry_point;
            let name = if draft.inputs.files.get(existing).is_some_and(|source| {
                source.starts_with("# Generated Cadpad entry point.")
                    || source == portable::PROJECT_BROWSER_SOURCE
            }) {
                existing.clone()
            } else {
                (0..=portable::SOURCE_FILE_LIMIT)
                    .map(|index| format!("cadpad_view_{index}.py"))
                    .find(|name| !draft.inputs.files.contains_key(name))
                    .unwrap()
            };
            draft.inputs.files.insert(name.clone(), adapter);
            draft.inputs.entry_point = name;
            portable::validate_inputs(&draft.inputs)?;
            let mut next = ws.stored.clone();
            next.drafts.insert(draft.id.clone(), draft.clone());
            self.persist(&next).await?;
            ws.stored = next;
            draft
        };
        self.build(workspace, revision, &draft.id, actor).await
    }
    pub async fn index_project_folder(&self, directory: PathBuf) -> Result<Value> {
        let sources = tokio::task::spawn_blocking(move || -> Result<_> {
            let root = directory.canonicalize()?;
            let mut sources = std::collections::BTreeMap::new();
            let mut total = 0;
            for path in portable::python_entry_points(&root)? {
                let bytes = portable::read_bounded(
                    &portable::checked_path(&root, &path)?,
                    portable::SOURCE_LIMIT,
                )?;
                total += bytes.len();
                if total > 8 * portable::SOURCE_LIMIT {
                    return Err(Error::domain(
                        "INVALID_REQUEST",
                        "Source files exceed 8 MiB",
                    ));
                }
                sources.insert(
                    path,
                    String::from_utf8(bytes).map_err(|_| {
                        Error::domain("INVALID_REQUEST", "Python source must be UTF-8")
                    })?,
                );
            }
            Ok(sources)
        })
        .await
        .map_err(|_| Error::domain("IO_ERROR", "Could not index project source"))??;
        self.entry_analysis_request("index_sources", json!({"sources":sources}))
            .await
    }
    async fn entry_analysis_request(&self, method: &str, params: Value) -> Result<Value> {
        let (_send, cancel) = tokio::sync::watch::channel(false);
        let (admission, _construction) = self.native_construction(cancel.clone()).await?;
        let cache = self.root.join("entry-analysis").join(crate::id());
        let mut config = self.config.clone();
        config.limits.operation_timeout_ms = 15000;
        let result = async {
            let mut worker =
                Worker::spawn_admitted(&config, cache.clone(), None, admission).await?;
            let result = worker
                .request("entry-analysis", method, params, cancel)
                .await;
            let stopped = worker.stop().await;
            drop(worker);
            let plan = result?["data"].clone();
            stopped?;
            Ok(plan)
        }
        .await;
        let _ = tokio::fs::remove_dir_all(cache).await;
        result
    }
}

pub fn manifest_hash(directory: &Path) -> Result<Option<String>> {
    let metadata = portable::metadata_directory(directory)?;
    let path = portable::checked_path(&metadata, "project.json")?;
    if !path.exists() || portable::read_manifest(directory).is_err() {
        return Ok(None);
    }
    Ok(Some(portable::bytes_hash(&portable::read_bounded(
        &path,
        portable::SOURCE_LIMIT,
    )?)))
}
/// Publish a new adapter without overwriting existing source, then select it in
/// configuration. The manifest and analyzed file must still match the preview.
pub fn configure_entry(
    directory: &Path,
    entry: &str,
    expected_manifest: Option<&str>,
    adapter: Option<(&str, &str)>,
) -> Result<String> {
    let root = directory.canonicalize()?;
    if manifest_hash(&root)?.as_deref() != expected_manifest {
        return Err(Error::domain(
            "REVISION_CONFLICT",
            "Project settings changed. Reopen entry setup to review them.",
        ));
    }
    crate::validate_path(entry)?;
    let mut created: Option<PathBuf> = None;
    let selected = if let Some((hash, code)) = adapter {
        let original = portable::read_bounded(
            &portable::checked_path(&root, entry)?,
            portable::SOURCE_LIMIT,
        )?;
        if portable::bytes_hash(&original) != hash {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Python source changed. Select it again to refresh the adapter preview.",
            ));
        }
        let mut name = String::new();
        for suffix in 0..1000 {
            name = if suffix == 0 {
                "cadpad_entry.py".into()
            } else {
                format!("cadpad_entry_{suffix}.py")
            };
            let path = portable::checked_path(&root, &name)?;
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    if let Err(error) = file
                        .write_all(code.as_bytes())
                        .and_then(|_| file.sync_all())
                    {
                        drop(file);
                        let _ = std::fs::remove_file(&path);
                        return Err(error.into());
                    }
                    created = Some(path);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        if created.is_none() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Choose another name for the Cadpad entry file",
            ));
        }
        name
    } else {
        entry.into()
    };
    let result = (|| -> Result<()> {
        if expected_manifest.is_none() {
            return portable::initialize_folder(&root, &selected);
        }
        let mut manifest = portable::read_manifest(&root)?;
        if !portable::python_entry_points(&root)?.contains(&selected) {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Choose a Python file in this project folder",
            ));
        }
        manifest.entry_point = selected.clone();
        manifest.saved_revision.clear();
        if !manifest.source_files.contains(&selected) {
            manifest.source_files.push(selected.clone());
        }
        let metadata = portable::metadata_directory(&root)?;
        portable::atomic_write(
            &metadata,
            "project.json",
            &serde_json::to_vec_pretty(&manifest)?,
        )
    })();
    if result.is_err() {
        if let Some(path) = created {
            let _ = std::fs::remove_file(path);
        }
    }
    result?;
    Ok(selected)
}
