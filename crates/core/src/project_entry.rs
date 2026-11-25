//! Desktop-authorized entry adapters. Analysis parses source without running it.
use crate::{Core, Error, Result, portable, worker::Worker};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

const MODEL_FILE_BUILDER: &str = "$file";
const MODEL_FILE_COMMENT: &str = "# Project model file: ";

fn model_file_asset(entry: &str) -> String {
    if let Some(asset) = entry.strip_prefix("assets/") {
        return asset.into();
    }
    let digest = crate::hash(entry);
    let extension = portable::model_file_format(entry).unwrap();
    format!("imports/project/{digest}.{extension}")
}

fn model_file_adapter(entry: &str, asset: &str, format: &str) -> Result<String> {
    let path = serde_json::to_string(entry)?;
    let name = serde_json::to_string(asset)?;
    let kind = if format == "stl" {
        ", kind=\"surface\""
    } else {
        ""
    };
    let show = if format == "step" {
        format!("show(import_step_named({name}, name=\"model\"), name=\"model\")")
    } else {
        format!("show(import_stl({name}), name=\"model\"{kind})")
    };
    Ok(format!(
        "# Generated Cadpad entry point.\n{MODEL_FILE_COMMENT}{path}\n{show}\n"
    ))
}

fn set_builder_entry(inputs: &mut crate::Inputs, adapter: String) {
    let existing = &inputs.entry_point;
    let name = if inputs.files.get(existing).is_some_and(|source| {
        source.starts_with("# Generated Cadpad entry point.")
            || source == portable::PROJECT_BROWSER_SOURCE
    }) {
        existing.clone()
    } else {
        (0..=portable::SOURCE_FILE_LIMIT)
            .map(|index| format!("cadpad_view_{index}.py"))
            .find(|name| !inputs.files.contains_key(name))
            .unwrap()
    };
    inputs.files.insert(name.clone(), adapter);
    inputs.entry_point = name;
}

pub(crate) fn model_entry_asset(inputs: &crate::Inputs) -> Option<(String, String, &'static str)> {
    let source = inputs.files.get(&inputs.entry_point)?;
    let (entry, builder) = generated_model_entry(source)?;
    let format = portable::model_file_format(&entry)?;
    if builder != MODEL_FILE_BUILDER {
        return None;
    }
    let asset = model_file_asset(&entry);
    if model_file_adapter(&entry, &asset, format).ok()?.as_str() != source {
        return None;
    }
    Some((entry, inputs.assets.get(&asset)?.clone(), format))
}

pub(crate) fn stl_model_asset(inputs: &crate::Inputs) -> Option<(String, String)> {
    let (entry, digest, format) = model_entry_asset(inputs)?;
    (format == "stl").then_some((entry, digest))
}

async fn read_project_model_file(directory: PathBuf, entry: &str) -> Result<Vec<u8>> {
    let relative = entry.to_owned();
    tokio::task::spawn_blocking(move || {
        let root = directory.canonicalize()?;
        let path = portable::checked_path(&root, &relative)?.canonicalize()?;
        if !path.starts_with(&root) {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Choose a file inside this project",
            ));
        }
        portable::read_bounded(&path, portable::ASSET_LIMIT)
    })
    .await
    .map_err(|_| Error::domain("IO_ERROR", "Could not read project model"))?
}

/// Recover the source file + builder a generated Cadpad adapter wraps.
/// Used so Explorer can select the real entry even when the canvas is the project root.
pub fn generated_model_entry(source: &str) -> Option<(String, String)> {
    if !source.starts_with("# Generated Cadpad entry point.") {
        return None;
    }
    if let Some(path) = source
        .lines()
        .nth(1)
        .and_then(|line| line.strip_prefix(MODEL_FILE_COMMENT))
    {
        let entry: String = serde_json::from_str(path).ok()?;
        portable::model_file_format(&entry)?;
        return Some((entry, MODEL_FILE_BUILDER.into()));
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

/// A generated entry can outlive its source after a project is reorganized on disk.
/// Keep the imported files browsable instead of executing a dangling adapter.
pub(crate) fn reset_missing_model_entry(inputs: &mut crate::Inputs) {
    if let Some((entry, builder)) = inputs_model_entry(inputs) {
        let present = if builder == MODEL_FILE_BUILDER {
            inputs.assets.contains_key(&model_file_asset(&entry))
        } else {
            inputs.files.contains_key(&entry)
        };
        if !present {
            inputs.files.insert(
                inputs.entry_point.clone(),
                portable::PROJECT_BROWSER_SOURCE.into(),
            );
        }
    }
}

impl Core {
    pub async fn project_directory(&self, workspace: &str) -> Result<PathBuf> {
        let all = self.workspaces.lock().await;
        let ws = all
            .get(workspace)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        ws.stored
            .project_directory()
            .map(Path::to_path_buf)
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
        let _memory = self.pin_workspace(workspace).await?;
        let sources = {
            let all = self.workspace(workspace).await?;
            let ws = all
                .get(workspace)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            ws.stored
                .revisions
                .load(revision)
                .await?
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
        let Ok(_memory) = self.pin_workspace(workspace).await else {
            return false;
        };
        let Ok(all) = self.workspace(workspace).await else {
            return false;
        };
        let Some(ws) = all.get(workspace) else {
            return false;
        };
        if ws.stored.model_parent.is_none() {
            return false;
        }
        let Some(binding) = ws.stored.project.as_ref() else {
            return false;
        };
        let Some(saved) = ws.stored.revisions.header(&binding.saved_revision) else {
            return false;
        };
        let current = ws.stored.revisions.header(&ws.stored.current).unwrap();
        !current.same_content(saved)
    }
    pub async fn open_project_model(
        &self,
        workspace: &str,
        revision: &str,
        entry: &str,
        builder: &str,
        actor: &str,
    ) -> Result<(String, crate::Job)> {
        self.open_project_model_reusing(workspace, revision, entry, builder, actor, None)
            .await
    }
    pub async fn open_project_model_reusing(
        &self,
        workspace: &str,
        revision: &str,
        entry: &str,
        builder: &str,
        actor: &str,
        cached_view: Option<&str>,
    ) -> Result<(String, crate::Job)> {
        self.open_project_model_streaming(
            workspace,
            revision,
            entry,
            builder,
            actor,
            cached_view,
            None,
        )
        .await
    }

    pub async fn open_project_model_streaming(
        &self,
        workspace: &str,
        revision: &str,
        entry: &str,
        builder: &str,
        actor: &str,
        cached_view: Option<&str>,
        preview: Option<crate::StlPreviewSink>,
    ) -> Result<(String, crate::Job)> {
        self.open_project_model_view(
            workspace,
            revision,
            entry,
            builder,
            actor,
            cached_view,
            preview,
            true,
        )
        .await
    }

    pub(crate) async fn open_requirement_model(
        &self,
        workspace: &str,
        revision: &str,
        entry: &str,
        builder: &str,
        actor: &str,
        cached_view: Option<&str>,
    ) -> Result<(String, crate::Job)> {
        self.open_project_model_view(
            workspace,
            revision,
            entry,
            builder,
            actor,
            cached_view,
            None,
            false,
        )
        .await
    }

    async fn open_project_model_view(
        &self,
        workspace: &str,
        revision: &str,
        entry: &str,
        builder: &str,
        actor: &str,
        cached_view: Option<&str>,
        preview: Option<crate::StlPreviewSink>,
        check_requirements: bool,
    ) -> Result<(String, crate::Job)> {
        let _memory = self.pin_workspace(workspace).await?;
        let standalone_stl =
            builder == MODEL_FILE_BUILDER && portable::model_file_format(entry) == Some("stl");
        // Opening a model executes the selected disk sources in a private view.
        // Keep the project's saved checkpoints independent of that live layout.
        let (directory, mesh_base) = {
            let all = self.workspace(workspace).await?;
            let parent = all
                .get(workspace)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if parent.stored.current != revision {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Project changed while opening a model",
                ));
            }
            let current = parent.stored.current_revision();
            let binding = parent.stored.project.as_ref();
            let mesh_base = if standalone_stl
                && current.inputs.mechanism == json!({})
                && current.inputs.parameters == json!({})
            {
                binding.map(|binding| (current.inputs.clone(), binding.fingerprint.clone()))
            } else {
                None
            };
            (binding.map(|binding| binding.directory.clone()), mesh_base)
        };
        let mut publication = if mesh_base.is_none() {
            Some(self.blob_publication.read().await)
        } else {
            None
        };
        let mut mesh_file = None;
        let disk = if let Some(directory) = directory {
            let disk = if mesh_base.is_some() {
                let bytes = read_project_model_file(directory.clone(), entry).await?;
                mesh_file = Some((portable::bytes_hash(&bytes), bytes));
                None
            } else {
                let root = directory.clone();
                Some(
                    tokio::task::spawn_blocking(move || portable::read_project(&root))
                        .await
                        .map_err(|error| Error::domain("IO_ERROR", error.to_string()))??,
                )
            };
            let fingerprint = mesh_base
                .as_ref()
                .map(|(_, fingerprint)| fingerprint)
                .or_else(|| disk.as_ref().map(|loaded| &loaded.fingerprint))
                .unwrap();
            if let Some(view) = cached_view.filter(|view| *view != workspace) {
                let out_of_tree_asset =
                    builder == MODEL_FILE_BUILDER && !entry.starts_with("assets/");
                let asset_hash = if let Some((hash, _)) = &mesh_file {
                    Some(hash.clone())
                } else if out_of_tree_asset && portable::model_file_format(entry).is_some() {
                    Some(portable::bytes_hash(
                        &read_project_model_file(directory.clone(), entry).await?,
                    ))
                } else {
                    None
                };
                if let Ok(all) = self.workspace(view).await {
                    let candidate = &all[view].stored;
                    let current = candidate.current_revision();
                    let matches_asset = (mesh_file.is_none() && !out_of_tree_asset)
                        || asset_hash.as_ref().is_some_and(|hash| {
                            current.inputs.assets.get(&model_file_asset(entry)) == Some(hash)
                        });
                    let original_branch_build = candidate.model_source_revision.as_deref()
                        == Some(revision)
                        && current.parent.as_deref().is_some_and(|base| {
                            candidate
                                .revisions
                                .header(base)
                                .is_some_and(|header| header.parent.is_none())
                        });
                    if all
                        .get(workspace)
                        .is_some_and(|parent| parent.stored.current == revision)
                        && candidate.model_parent.as_deref() == Some(workspace)
                        && (!standalone_stl || candidate.model_source_revision.is_some())
                        && current.model["parts"][0]["geometry_fingerprint"]["format"]
                            != crate::stl_model::LEGACY_FORMAT
                        && candidate.project.as_ref().is_some_and(|binding| {
                            binding.directory == directory
                                && &binding.fingerprint == fingerprint
                                && (original_branch_build
                                    || (current.parent.as_deref()
                                        == Some(binding.saved_revision.as_str())
                                        && candidate
                                            .revisions
                                            .header(&binding.saved_revision)
                                            .is_some_and(|base| {
                                                base.parent.as_deref() == Some(revision)
                                            })))
                        })
                        && inputs_model_entry(&current.inputs)
                            .as_ref()
                            .is_some_and(|(path, id)| path == entry && id == builder)
                        && matches_asset
                        && let Some(job) = candidate.jobs.values().find(|job| {
                            matches!(job.kind, crate::JobKind::Build)
                                && job.state == crate::JobState::Succeeded
                                && job.resulting_revision.as_deref() == Some(current.id.as_str())
                        })
                    {
                        self.ensure_actor_active(actor)?;
                        return Ok((view.into(), job.clone()));
                    }
                }
            }
            if let Some(mut loaded) = disk {
                self.publish_portable_assets(&mut loaded).await?;
                Some(loaded)
            } else {
                None
            }
        } else {
            None
        };
        let workspace_id = crate::id();
        let base = crate::id();
        let mut display_publication = None;
        let mut construction = None;
        let mesh = if let (Some((inputs, _)), Some((digest, bytes))) = (mesh_base, mesh_file) {
            {
                let all = self.workspaces.lock().await;
                self.ensure_actor_active(actor)?;
                crate::construction::check_job_capacity(&all, &workspace_id, actor)?;
            }
            self.ensure_disk_quota().await?;
            let (cancel, receive) = tokio::sync::watch::channel(false);
            construction = Some(self.construction_slot(receive.clone()).await?);
            publication = Some(self.blob_publication.read().await);
            display_publication = Some(self.display_publication.read().await);
            // Reuse the selected file's bytes to build the display buffers
            // after storing the source blob.
            let bytes = self.publish_asset_blob(digest.clone(), bytes).await?;
            let mut draft = Self::input_draft(inputs, &base, actor);
            let asset = model_file_asset(entry);
            let adapter = model_file_adapter(entry, &asset, "stl")?;
            draft.inputs.assets.insert(asset, digest.clone());
            set_builder_entry(&mut draft.inputs, adapter);
            self.validate_asset_budget(&draft.inputs).await?;
            portable::validate_inputs(&draft.inputs)?;
            let model_revision = crate::id();
            let model = self
                .publish_stl(
                    &workspace_id,
                    &model_revision,
                    draft.inputs.clone(),
                    entry.into(),
                    crate::stl_model::Source {
                        digest,
                        bytes: Some(bytes),
                        preview,
                    },
                    true,
                    receive,
                )
                .await?;
            drop(cancel);
            crate::project_result::validate_publication(&model)?;
            let motion = crate::motion::State::new(&draft.inputs, &model)?;
            Some((draft, model_revision, model, motion))
        } else {
            None
        };
        let completed = {
            let mut all = self.workspace(workspace).await?;
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
            // A model tab owns its checkpoints from this branch point onward.
            // The source revision identifies the parent project snapshot for reuse.
            let source = &parent.stored;
            let mut stored = crate::StoredWorkspace {
                ephemeral: !check_requirements,
                model_parent: Some(
                    source
                        .model_parent
                        .clone()
                        .unwrap_or_else(|| workspace.into()),
                ),
                model_source_revision: Some(revision.into()),
                schema_version: source.schema_version,
                id: workspace_id.clone(),
                current: base.clone(),
                revisions: Default::default(),
                drafts: Default::default(),
                jobs: Default::default(),
                job_snapshots: Default::default(),
                project: source.project.clone(),
                python_origin: source.python_origin.clone(),
                title: source.title.clone(),
                recordings: Default::default(),
                recording_imports: Default::default(),
                // Requirements stay on the project; model views read them there.
                motion_tests: Default::default(),
                geometry_cache: serde_json::Value::Null,
            };
            let (inputs, disk_fingerprint) = match disk {
                Some(disk) => (disk.inputs, Some(disk.fingerprint)),
                None => (source.current_revision().inputs.clone(), None),
            };
            let initial = crate::Revision {
                id: base.clone(),
                parent: None,
                inputs,
                model: json!({"parts":[]}),
                actor: source.current_revision().actor.clone(),
                restored_from: None,
            };
            if let Some(binding) = &mut stored.project {
                binding.saved_revision = base.clone();
                if let Some(fingerprint) = disk_fingerprint {
                    binding.fingerprint = fingerprint;
                }
            }
            if mesh.is_some() {
                let summary = parent.stored.revisions.header(revision).unwrap().clone();
                stored
                    .revisions
                    .insert_with_unchanged_inputs(initial, summary);
            } else {
                stored.revisions.insert(base.clone(), initial);
            }
            let (completed, motion) = if let Some((draft, model_revision, model, motion)) = mesh {
                crate::construction::check_job_capacity(&all, &workspace_id, actor)?;
                stored.drafts.insert(draft.id.clone(), draft.clone());
                // These inputs are already immutable in the committed revision.
                // read_source accepts that revision as the job snapshot.
                let mut job = Self::job_for_snapshot(
                    &draft,
                    actor,
                    crate::JobKind::Build,
                    model_revision.clone(),
                );
                let summary = stored.drafts.header(&draft.id).unwrap().clone();
                if let Some(binding) = &mut stored.project
                    && binding.fingerprint == summary.input_fingerprint
                {
                    binding.saved_revision = model_revision.clone();
                }
                if let Some(origin) = &mut stored.python_origin {
                    origin.applied = true;
                }
                stored.revisions.insert_with_unchanged_inputs(
                    crate::Revision {
                        id: model_revision.clone(),
                        parent: Some(base.clone()),
                        inputs: draft.inputs,
                        model,
                        actor: draft.actor,
                        restored_from: None,
                    },
                    summary,
                );
                stored.current = model_revision.clone();
                job.state = crate::JobState::Succeeded;
                job.resulting_revision = Some(model_revision);
                stored.jobs.insert(job.id.clone(), job.clone());
                (Some(job), Some(motion))
            } else {
                (None, None)
            };
            self.persist(&stored).await?;
            if completed.is_some() {
                self.display_use.touch(&workspace_id, &stored.current);
            }
            all.insert(
                workspace_id.clone(),
                crate::Workspace {
                    archived: None,
                    memory: std::sync::Arc::new(()),
                    saved_pose: None,
                    stored,
                    motion,
                    recovery: std::sync::Arc::new(tokio::sync::Mutex::new(())),
                    display_epoch: std::sync::Arc::new(crate::id()),
                    worker: None,
                    worker_last_used: 0,
                    cancellation: std::collections::HashMap::new(),
                },
            );
            completed
        };
        drop(display_publication);
        drop(publication);
        drop(construction);
        if let Some(job) = completed {
            self.job_activity(&workspace_id, &job);
            if check_requirements && let Some(revision) = &job.resulting_revision {
                self.schedule_requirement_auto_run(workspace, &workspace_id, revision);
            }
            return Ok((workspace_id, job));
        }
        match self
            .build_project_model(
                &workspace_id,
                &base,
                entry,
                builder,
                actor,
                check_requirements,
            )
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
        self.build_project_model(workspace, revision, entry, builder, actor, true)
            .await
    }

    async fn build_project_model(
        &self,
        workspace: &str,
        revision: &str,
        entry: &str,
        builder: &str,
        actor: &str,
        check_requirements: bool,
    ) -> Result<crate::Job> {
        let _memory = self.pin_workspace(workspace).await?;
        let publication = if builder == MODEL_FILE_BUILDER {
            Some(self.blob_publication.read().await)
        } else {
            None
        };
        let (adapter, staged) = if builder == MODEL_FILE_BUILDER {
            let format = portable::model_file_format(entry)
                .ok_or_else(|| Error::domain("INVALID_REQUEST", "Choose a STEP or STL file"))?;
            let directory = self.project_directory(workspace).await?;
            let bytes = read_project_model_file(directory, entry).await?;
            let asset = model_file_asset(entry);
            let digest = self.store_asset_blob(bytes).await?;
            let adapter = model_file_adapter(entry, &asset, format)?;
            (adapter, Some((asset, digest)))
        } else {
            (
                self.project_builder_adapter(workspace, revision, entry, builder)
                    .await?,
                None,
            )
        };
        let draft = {
            let mut all = self.workspace(workspace).await?;
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
            let inputs = ws.stored.current_revision().inputs.clone();
            let mut draft = Self::input_draft(inputs, revision, actor);
            if let Some((asset, digest)) = staged {
                draft.inputs.assets.insert(asset, digest);
                self.validate_asset_budget(&draft.inputs).await?;
            }
            set_builder_entry(&mut draft.inputs, adapter);
            portable::validate_inputs(&draft.inputs)?;
            let mut next = ws.stored.clone();
            next.drafts.insert(draft.id.clone(), draft.clone());
            self.persist(&next).await?;
            ws.stored = next;
            draft
        };
        drop(publication);
        self.queue_job(
            workspace,
            revision,
            &draft.id,
            actor,
            crate::JobKind::Build,
            check_requirements,
        )
        .await
    }
    async fn project_builder_adapter(
        &self,
        workspace: &str,
        revision: &str,
        entry: &str,
        builder: &str,
    ) -> Result<String> {
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
        Ok(adapter)
    }
    /// The Explorer catalog describes disk, never the saved revision's layout.
    /// Fingerprints only avoid repeating Python analysis; every call reads disk.
    pub async fn project_source_catalog(
        &self,
        workspace: &str,
        revision: &str,
        fingerprint: Option<&str>,
    ) -> Result<Value> {
        let directory = {
            let all = self.workspaces.lock().await;
            let ws = all
                .get(workspace)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            // The archive keeps the current revision and folder binding.
            // A disk-backed catalog needs neither revision bodies nor native state.
            if ws.stored.current == revision {
                ws.stored
                    .project
                    .as_ref()
                    .map(|binding| binding.directory.clone())
            } else {
                drop(all);
                let all = self.workspace_metadata(workspace).await?;
                let ws = &all[workspace];
                if !ws.stored.revisions.contains_key(revision) {
                    return Err(Error::domain("REVISION_CONFLICT", "Unknown revision"));
                }
                ws.stored
                    .project
                    .as_ref()
                    .map(|binding| binding.directory.clone())
            }
        };
        if let Some(directory) = directory {
            let (sources, models) =
                tokio::try_join!(Self::catalog_sources(directory.clone()), async move {
                    tokio::task::spawn_blocking(move || portable::list_model_files(&directory))
                        .await
                        .map_err(|_| Error::domain("IO_ERROR", "Could not list project models"))?
                })?;
            let observed = crate::hash(&serde_json::to_string(&(&sources, &models))?);
            if fingerprint == Some(observed.as_str()) {
                return Ok(json!({"unchanged":true,"fingerprint":observed}));
            }
            let mut index = self
                .entry_analysis_request("index_sources", json!({"sources":sources}))
                .await?;
            // Preview the exact disk text that was indexed, including new files.
            if let Some(files) = index["files"].as_array_mut() {
                for file in files {
                    if let Some(source) = file["path"].as_str().and_then(|path| sources.get(path)) {
                        file["source"] = json!(source);
                    }
                }
            }
            if let Some(files) = index["files"].as_array_mut() {
                files.extend(models.into_iter().map(|path| {
                    let name = path.rsplit('/').next().unwrap();
                    json!({
                        "path": path, "description": "CAD file", "diagnostic": null,
                        "builders": [{
                            "id": MODEL_FILE_BUILDER, "kind": "model_file", "label": name,
                            "available": true, "reason": "", "line": 0
                        }]
                    })
                }));
            }
            index["fingerprint"] = json!(observed);
            return Ok(index);
        }
        self.index_project_sources(workspace, revision).await
    }
    pub async fn index_project_folder(&self, directory: PathBuf) -> Result<Value> {
        let sources = Self::catalog_sources(directory).await?;
        self.entry_analysis_request("index_sources", json!({"sources":sources}))
            .await
    }
    async fn catalog_sources(
        directory: PathBuf,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        tokio::task::spawn_blocking(move || -> Result<_> {
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
            sources.retain(|_, source| {
                !source.starts_with("# Generated Cadpad entry point.")
                    && source.as_str() != portable::PROJECT_BROWSER_SOURCE
            });
            Ok(sources)
        })
        .await
        .map_err(|_| Error::domain("IO_ERROR", "Could not index project source"))?
    }
    async fn entry_analysis_request(&self, method: &str, params: Value) -> Result<Value> {
        let (_send, cancel) = tokio::sync::watch::channel(false);
        self.entry_analysis_with_cancel(method, params, cancel)
            .await
    }

    pub(crate) async fn entry_analysis_with_cancel(
        &self,
        method: &str,
        params: Value,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<Value> {
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

#[cfg(test)]
mod missing_entry_tests {
    use super::*;

    #[test]
    fn generated_step_entry_uses_named_import_while_stl_stays_surface_reference() {
        let step = model_file_adapter("assets/device.step", "device.step", "step").unwrap();
        assert!(
            step.contains(
                "show(import_step_named(\"device.step\", name=\"model\"), name=\"model\")"
            )
        );
        let stl = model_file_adapter("assets/device.stl", "device.stl", "stl").unwrap();
        assert!(stl.contains("show(import_stl(\"device.stl\"), name=\"model\", kind=\"surface\")"));
    }

    #[test]
    fn deleted_generated_target_returns_to_browser_without_restoring_deleted_files() {
        let mut inputs = crate::Inputs::default();
        inputs.entry_point = "cadpad_view_0.py".into();
        let adapter = "# Generated Cadpad entry point.\ndef build(context):\n    path = Path(__file__).parent / \"old/model.py\"\n    result = getattr(module, \"chassis\")()\n";
        inputs
            .files
            .insert(inputs.entry_point.clone(), adapter.into());
        inputs
            .files
            .insert("new/model.py".into(), "# moved source".into());
        reset_missing_model_entry(&mut inputs);
        assert_eq!(
            inputs.files[&inputs.entry_point],
            portable::PROJECT_BROWSER_SOURCE
        );
        assert!(inputs.files.contains_key("new/model.py"));
        assert!(!inputs.files.contains_key("old/model.py"));
    }

    #[test]
    fn existing_generated_target_and_user_entry_are_preserved() {
        let mut inputs = crate::Inputs::default();
        inputs.entry_point = "cadpad_view_0.py".into();
        let adapter = "# Generated Cadpad entry point.\ndef build(context):\n    path = Path(__file__).parent / \"model.py\"\n    result = getattr(module, \"chassis\")()\n";
        inputs
            .files
            .insert(inputs.entry_point.clone(), adapter.into());
        inputs.files.insert("model.py".into(), "# source".into());
        reset_missing_model_entry(&mut inputs);
        assert_eq!(inputs.files[&inputs.entry_point], adapter);
        inputs
            .files
            .insert(inputs.entry_point.clone(), "# user code".into());
        reset_missing_model_entry(&mut inputs);
        assert_eq!(inputs.files[&inputs.entry_point], "# user code");
    }
}
