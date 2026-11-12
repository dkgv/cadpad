//! Save/Open and explicit filesystem reload, independent of window components.
use crate::{Core, Draft, Error, Inputs, Result, StoredWorkspace, Workspace, hash, id, portable};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenedProject {
    pub workspace_id: String,
    pub base_revision: String,
    pub draft: Draft,
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| Error::domain("IO_ERROR", error.to_string()))?
}
impl Core {
    async fn load_portable(&self, directory: PathBuf) -> Result<portable::Loaded> {
        let mut loaded = blocking(move || portable::read_project(&directory)).await?;
        let blobs = std::mem::take(&mut loaded.blobs);
        let directory = self.root.join("blobs");
        blocking(move || portable::store_blobs(&directory, &blobs)).await?;
        Ok(loaded)
    }
    pub(crate) fn input_draft(inputs: Inputs, base: &str, actor: &str) -> Draft {
        Draft {
            id: id(),
            base_revision: base.into(),
            source_hashes: inputs
                .files
                .iter()
                .map(|(path, source)| (path.clone(), hash(source)))
                .collect(),
            inputs,
            actor: actor.into(),
            external_fingerprint: None,
            python_origin: None,
            restore_revision: None,
        }
    }
    /// Opening stages source; execution is an explicit subsequent build command.
    pub async fn open_project(&self, directory: PathBuf, actor: &str) -> Result<OpenedProject> {
        let _publication = self.blob_publication.read().await;
        let directory = tokio::fs::canonicalize(directory).await?;
        let manifest_directory = directory.clone();
        let manifest = blocking(move || portable::read_manifest(&manifest_directory)).await?;
        let tests_root = portable::metadata_directory(&directory)?;
        let tests_digest = manifest.motion_tests.clone();
        let motion_tests =
            blocking(move || crate::motion_tests::load(&tests_root, tests_digest.as_deref()))
                .await?;
        let history_directory = directory.clone();
        let blob_directory = self.root.join("blobs");
        let history =
            blocking(move || portable::read_history(&history_directory, &blob_directory)).await?;
        let revisions: BTreeMap<_, _> = history
            .into_iter()
            .map(|revision| (revision.id.clone(), revision))
            .collect();
        let recordings_directory = portable::metadata_directory(&directory)?;
        let recordings_manifest = manifest.clone();
        let (history, imported) = blocking(move || {
            let imported = crate::portable_recordings::load(
                &recordings_directory,
                &recordings_manifest,
                &revisions,
            )?;
            Ok((revisions, imported))
        })
        .await?;
        let mut revisions = history;
        revisions.extend(imported.revisions.clone());
        let recording_fingerprint = crate::playback::metadata_fingerprint(&imported.recordings)?;
        let blob_directory = self.root.join("blobs");
        let recording_blobs = imported.blobs;
        blocking(move || portable::store_blobs(&blob_directory, &recording_blobs)).await?;
        let workspace_id = id();
        let (base_revision, inputs, disk_baseline, external_fingerprint) =
            if manifest.saved_revision.is_empty() {
                let loaded = self.load_portable(directory.clone()).await?;
                let base = revisions
                    .values()
                    .find(|revision| {
                        portable::fingerprint(&revision.inputs).ok().as_ref()
                            == Some(&loaded.fingerprint)
                    })
                    .map(|revision| revision.id.clone())
                    .unwrap_or_else(id);
                // Seed the current revision with the folder's real sources. Using
                // Inputs::default() here made Open look like an empty scratchpad
                // (and Import/Save act on model.py) while the draft alone held the
                // opened files — so STL work could land in the wrong tree.
                let opened_inputs = loaded.inputs.clone();
                revisions
                    .entry(base.clone())
                    .or_insert_with(|| crate::Revision {
                        id: base.clone(),
                        parent: None,
                        inputs: opened_inputs.clone(),
                        model: json!({"parts":[]}),
                        actor: actor.into(),
                        restored_from: None,
                    });
                /* baseline source is loaded from the folder, not from project revisions */
                (
                    base,
                    opened_inputs,
                    loaded.fingerprint.clone(),
                    Some(loaded.fingerprint),
                )
            } else {
                let baseline = revisions.get(&manifest.saved_revision).ok_or_else(|| {
                    Error::domain(
                        "INVALID_REQUEST",
                        "Saved project revision snapshot is missing",
                    )
                })?;
                let fingerprint = portable::fingerprint(&baseline.inputs)?;
                // Editable root files may be half-written or invalid. They never
                // prevent reconstruction of the immutable successful checkpoint.
                // The independent external-change scan reports their diagnostics.
                (
                    baseline.id.clone(),
                    baseline.inputs.clone(),
                    fingerprint,
                    None,
                )
            };
        let mut draft = Self::input_draft(inputs, &base_revision, actor);
        draft.source_hashes = revisions[&base_revision]
            .inputs
            .files
            .iter()
            .map(|(path, source)| (path.clone(), hash(source)))
            .collect();
        draft.external_fingerprint = external_fingerprint;
        let stored = StoredWorkspace {
            model_parent: None,
            geometry_cache: Value::Null,
            motion_tests: motion_tests.clone(),
            job_snapshots: BTreeMap::new(),
            recordings: imported.recordings,
            recording_imports: imported.imports,
            schema_version: 1,
            id: workspace_id.clone(),
            current: base_revision.clone(),
            revisions,
            drafts: BTreeMap::from([(draft.id.clone(), draft.clone())]),
            jobs: BTreeMap::new(),
            title: directory
                .file_name()
                .map(|name| name.to_string_lossy().to_string()),
            python_origin: None,
            project: Some(portable::Binding {
                directory,
                fingerprint: disk_baseline,
                saved_revision: base_revision.clone(),
                recordings: manifest.recordings,
                recording_fingerprint,
                motion_tests: manifest.motion_tests,
                motion_test_fingerprint: crate::motion_tests::catalog_fingerprint(&motion_tests)?,
            }),
        };
        self.persist(&stored).await?;
        self.workspaces.lock().await.insert(
            workspace_id.clone(),
            Workspace {
                display_epoch: std::sync::Arc::new(id()),
                recovery: std::sync::Arc::new(tokio::sync::Mutex::new(())),
                motion: None,
                stored,
                worker: None,
                worker_last_used: 0,
                cancellation: HashMap::new(),
            },
        );
        Ok(OpenedProject {
            workspace_id,
            base_revision,
            draft,
        })
    }
    /// The destination comes from the local file dialog/transport authorization,
    /// never from generated modeling code. Existing unbound folders aren't overwritten.
    pub async fn save_project(
        &self,
        workspace_id: &str,
        expected: &str,
        destination: Option<PathBuf>,
    ) -> Result<portable::Binding> {
        self.save_project_layout(workspace_id, expected, destination, false)
            .await
    }
    /// Desktop saves use ordinary folders with .cadpad configuration. Existing
    /// legacy bindings keep their format until explicitly saved to a new folder.
    pub async fn save_project_folder(
        &self,
        workspace_id: &str,
        expected: &str,
        destination: Option<PathBuf>,
    ) -> Result<portable::Binding> {
        self.save_project_layout(workspace_id, expected, destination, true)
            .await
    }
    async fn save_project_layout(
        &self,
        workspace_id: &str,
        expected: &str,
        destination: Option<PathBuf>,
        folder: bool,
    ) -> Result<portable::Binding> {
        let _publication = self.blob_publication.read().await;
        let (stored, directory, expected_disk) = {
            let workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get(workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != expected {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Save revision is no longer current",
                ));
            }
            let directory = destination
                .or_else(|| {
                    ws.stored
                        .project
                        .as_ref()
                        .map(|binding| binding.directory.clone())
                })
                .ok_or_else(|| {
                    Error::domain(
                        "INVALID_REQUEST",
                        "Choose a project folder for the first Save",
                    )
                })?;
            let absolute = if directory.is_absolute() {
                directory
            } else {
                std::env::current_dir()?.join(directory)
            };
            let resolved = if absolute.exists() {
                absolute.canonicalize()?
            } else {
                absolute
                    .parent()
                    .ok_or_else(|| Error::domain("INVALID_REQUEST", "Invalid project destination"))?
                    .canonicalize()?
                    .join(absolute.file_name().unwrap())
            };
            let expected_disk = ws
                .stored
                .project
                .as_ref()
                .filter(|binding| binding.directory == resolved)
                .map(|binding| binding.fingerprint.clone());
            (ws.stored.clone(), resolved, expected_disk)
        };
        if let Some(origin) = stored.python_origin.clone() {
            blocking(move || crate::origins::unchanged(&origin, None)).await?;
        }
        let revisions = stored.revisions.values().cloned().collect::<Vec<_>>();
        let current = stored.current.clone();
        let blobs = self.root.join("blobs");
        let saved_stored = stored.clone();
        let binding = blocking(move || {
            portable::save_project(
                &directory,
                expected_disk.as_deref(),
                &revisions,
                &current,
                &blobs,
                &saved_stored,
                folder,
            )
        })
        .await?;
        if let Some(origin) = stored.python_origin.clone() {
            let saved_directory = binding.directory.clone();
            let exclude = saved_directory.clone();
            blocking(move||crate::origins::unchanged(&origin,Some(&exclude))).await.map_err(|error|Error::domain("EXTERNAL_CHANGES",format!("Original files changed while saving. The reviewed copy exists at {}. Review the original files before retrying: {error}",saved_directory.display())))?;
        }
        let mut workspaces = self.workspaces.lock().await;
        let ws = workspaces.get_mut(workspace_id).unwrap();
        if ws.stored.python_origin != stored.python_origin {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Python origin changed while saving; the reviewed copy was written but not bound",
            ));
        }
        // New commits may have arrived during I/O. Binding records the revision
        // actually saved, so the current newer revision correctly remains dirty.
        let mut next = ws.stored.clone();
        next.project = Some(binding.clone());
        next.python_origin = None;
        self.persist(&next).await?;
        ws.stored = next;
        Ok(binding)
    }
    pub async fn scan_external_changes(&self, workspace_id: &str) -> Result<Value> {
        if let Some(result) = self.scan_python_origin(workspace_id).await? {
            return Ok(result);
        }
        let (binding, revision, inputs) = {
            let workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get(workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            let binding = ws.stored.project.clone().ok_or_else(|| {
                Error::domain("INVALID_REQUEST", "Workspace has no saved project folder")
            })?;
            (
                binding,
                ws.stored.current.clone(),
                ws.stored.revisions[&ws.stored.current].inputs.clone(),
            )
        };
        let directory = binding.directory.clone();
        let loaded = blocking(move || portable::read_project(&directory)).await?;
        let changed = loaded.fingerprint != binding.fingerprint;
        let mut changes = BTreeMap::new();
        for path in inputs.files.keys().chain(loaded.inputs.files.keys()) {
            if inputs.files.get(path) != loaded.inputs.files.get(path) {
                changes.insert(
                    path.clone(),
                    if !inputs.files.contains_key(path) {
                        "added"
                    } else if !loaded.inputs.files.contains_key(path) {
                        "deleted"
                    } else {
                        "modified"
                    },
                );
            }
        }
        for (path, a, b) in [
            (
                "parameters.json",
                &inputs.parameters,
                &loaded.inputs.parameters,
            ),
            (
                "annotations.json",
                &inputs.annotations,
                &loaded.inputs.annotations,
            ),
            (
                "mechanism.json",
                &inputs.mechanism,
                &loaded.inputs.mechanism,
            ),
        ] {
            if a != b {
                changes.insert(path.into(), "modified");
            }
        }
        if inputs.assets != loaded.inputs.assets {
            changes.insert("assets/".into(), "modified");
        }
        if inputs.entry_point != loaded.inputs.entry_point
            || inputs.dependencies != loaded.inputs.dependencies
        {
            changes.insert("project.json".into(), "modified");
        }
        Ok(
            json!({"workspace_id":workspace_id,"revision":revision,"changed":changed,"fingerprint":loaded.fingerprint,"changes":changes}),
        )
    }
    pub async fn stage_external_changes(
        &self,
        workspace_id: &str,
        base: &str,
        observed_fingerprint: &str,
        actor: &str,
    ) -> Result<Draft> {
        self.stage_external_changes_resolved(
            workspace_id,
            base,
            observed_fingerprint,
            actor,
            BTreeMap::new(),
        )
        .await
    }
    pub async fn stage_external_changes_resolved(
        &self,
        workspace_id: &str,
        base: &str,
        observed_fingerprint: &str,
        actor: &str,
        resolutions: BTreeMap<String, crate::origins::Resolution>,
    ) -> Result<Draft> {
        let _publication = self.blob_publication.read().await;
        self.ensure_actor_active(actor)?;
        if let Some(draft) = self
            .stage_python_origin(
                workspace_id,
                base,
                observed_fingerprint,
                actor,
                &resolutions,
            )
            .await?
        {
            return Ok(draft);
        }
        if !resolutions.is_empty() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Conflict choices apply only to standalone Python origins",
            ));
        }
        let directory = {
            let workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get(workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Reload base is no longer current",
                ));
            }
            ws.stored
                .project
                .as_ref()
                .ok_or_else(|| {
                    Error::domain("INVALID_REQUEST", "Workspace has no saved project folder")
                })?
                .directory
                .clone()
        };
        let loaded = self.load_portable(directory).await?;
        if loaded.fingerprint != observed_fingerprint {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Project files changed again; review the latest external changes",
            ));
        }
        let mut workspaces = self.workspaces.lock().await;
        self.ensure_actor_active(actor)?;
        let ws = workspaces.get_mut(workspace_id).unwrap();
        if ws.stored.current != base {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Reload base changed while reading files",
            ));
        }
        let mut draft = Self::input_draft(loaded.inputs, base, actor);
        draft.external_fingerprint = Some(loaded.fingerprint);
        draft.source_hashes = ws.stored.revisions[base]
            .inputs
            .files
            .iter()
            .map(|(path, source)| (path.clone(), hash(source)))
            .collect();
        let mut next = ws.stored.clone();
        next.drafts.insert(draft.id.clone(), draft.clone());
        // Remember the disk observation only when the draft successfully commits.
        self.persist(&next).await?;
        ws.stored = next;
        Ok(draft)
    }
    pub async fn stage_asset(
        &self,
        workspace_id: &str,
        base: &str,
        draft_id: Option<&str>,
        name: &str,
        bytes: Vec<u8>,
        actor: &str,
    ) -> Result<Draft> {
        let _publication = self.blob_publication.read().await;
        portable::relative(name)?;
        if bytes.len() > portable::ASSET_LIMIT {
            return Err(Error::domain("INVALID_REQUEST", "Asset exceeds 64 MiB"));
        }
        let digest = portable::bytes_hash(&bytes);
        let blobs = BTreeMap::from([(digest.clone(), bytes)]);
        let blob_directory = self.root.join("blobs");
        blocking(move || portable::store_blobs(&blob_directory, &blobs)).await?;
        let mut workspaces = self.workspaces.lock().await;
        let ws = workspaces
            .get_mut(workspace_id)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        if ws.stored.current != base {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Asset base is no longer current",
            ));
        }
        let mut draft = if let Some(draft_id) = draft_id {
            let draft = ws
                .stored
                .drafts
                .get(draft_id)
                .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown draft"))?;
            if draft.actor != actor || draft.base_revision != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Draft ownership or base mismatch",
                ));
            }
            draft.clone()
        } else {
            Self::input_draft(ws.stored.revisions[base].inputs.clone(), base, actor)
        };
        draft.restore_revision = None;
        draft.inputs.assets.insert(name.into(), digest);
        self.validate_asset_budget(&draft.inputs).await?;
        portable::validate_inputs(&draft.inputs)?;
        let mut next = ws.stored.clone();
        next.drafts.insert(draft.id.clone(), draft.clone());
        self.persist(&next).await?;
        ws.stored = next;
        Ok(draft)
    }
    /// Import a plain Python project without requiring an agent or project wizard.
    /// The user selected the entry file; dependencies remain the pinned worker set.
    pub async fn open_python(&self, entry: &Path, actor: &str) -> Result<OpenedProject> {
        let entry = entry.canonicalize()?;
        let directory = entry.parent().unwrap().to_path_buf();
        if directory.join("project.json").exists() {
            return self.open_project(directory, actor).await;
        }
        let _publication = self.blob_publication.read().await;
        let origin_entry = entry.clone();
        let (inputs, blobs) = blocking(move || portable::read_python_project(&entry)).await?;
        let origin = crate::origins::Origin::capture(origin_entry, &inputs, false)?;
        let blob_directory = self.root.join("blobs");
        blocking(move || portable::store_blobs(&blob_directory, &blobs)).await?;
        let workspace_id = self.create_workspace(actor).await?;
        let project = self.get_project(&workspace_id).await?;
        let base_revision = project["revision"].as_str().unwrap().to_string();
        let mut workspaces = self.workspaces.lock().await;
        let ws = workspaces.get_mut(&workspace_id).unwrap();
        let mut draft = Self::input_draft(inputs, &base_revision, actor);
        draft.python_origin = Some(origin.clone());
        let mut next = ws.stored.clone();
        next.title = origin.entry_name().map(str::to_string);
        next.python_origin = Some(origin);
        next.drafts.insert(draft.id.clone(), draft.clone());
        self.persist(&next).await?;
        ws.stored = next;
        Ok(OpenedProject {
            workspace_id,
            base_revision,
            draft,
        })
    }
}

impl Core {
    pub async fn model_history(&self, workspace: &str) -> Result<Value> {
        let all = self.workspaces.lock().await;
        let ws = all
            .get(workspace)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        let mut entries = Vec::new();
        let mut cursor = Some(ws.stored.current.as_str());
        while let Some(id) = cursor {
            let Some(revision) = ws.stored.revisions.get(id) else {
                break;
            };
            let job = ws
                .stored
                .jobs
                .values()
                .find(|job| job.resulting_revision.as_deref() == Some(id));
            let label = if revision.restored_from.is_some() {
                "Restored earlier model".to_string()
            } else if let Some(parent) = revision
                .parent
                .as_ref()
                .and_then(|id| ws.stored.revisions.get(id))
            {
                let added_assets = revision
                    .inputs
                    .assets
                    .keys()
                    .filter(|name| !parent.inputs.assets.contains_key(*name))
                    .collect::<Vec<_>>();
                let parameters = revision
                    .inputs
                    .parameters
                    .as_object()
                    .map(|parameters| {
                        parameters
                            .iter()
                            .filter(|(name, value)| {
                                parent.inputs.parameters.get(*name) != Some(*value)
                            })
                            .map(|(name, _)| name.replace('_', " "))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let changed_files = revision
                    .inputs
                    .files
                    .iter()
                    .filter(|(name, source)| parent.inputs.files.get(*name) != Some(*source))
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                let old_parts = parent.model["parts"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let parts = revision.model["parts"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let added = parts
                    .iter()
                    .filter(|part| !old_parts.iter().any(|old| old["name"] == part["name"]))
                    .filter_map(|part| part["name"].as_str())
                    .collect::<Vec<_>>();
                let removed = old_parts
                    .iter()
                    .filter(|part| !parts.iter().any(|new| new["name"] == part["name"]))
                    .filter_map(|part| part["name"].as_str())
                    .collect::<Vec<_>>();
                if !added_assets.is_empty() {
                    format!(
                        "Imported {}",
                        added_assets
                            .iter()
                            .map(|path| path.rsplit('/').next().unwrap_or(path))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else if !added.is_empty() {
                    format!("Added {}", added.join(", ").replace('_', " "))
                } else if !removed.is_empty() {
                    format!("Removed {}", removed.join(", ").replace('_', " "))
                } else if !parameters.is_empty() {
                    format!("Adjusted {}", parameters.join(", "))
                } else if !changed_files.is_empty() {
                    let names = parts
                        .iter()
                        .filter_map(|part| part["name"].as_str())
                        .collect::<Vec<_>>();
                    if names.len() == 1 {
                        format!("Updated {}", names[0].replace('_', " "))
                    } else {
                        format!("Edited {}", changed_files.join(", "))
                    }
                } else {
                    "Rebuilt model".into()
                }
            } else {
                "Initial model".into()
            };
            entries.push(json!({"label":label,"revision":id,"restored":revision.restored_from.is_some(),"initial":revision.parent.is_none(),"actor":revision.actor,"parts":revision.model["parts"].as_array().map_or(0,Vec::len),"created_at":job.map(|job|job.created_at)}));
            cursor = revision.parent.as_deref();
            if entries.len() >= 100 {
                break;
            }
        }
        Ok(json!(entries))
    }
    pub async fn workspace_summaries(&self) -> Value {
        let workspaces = self.workspaces.lock().await;
        let mut candidates = workspaces.values().collect::<Vec<_>>();
        candidates.sort_by(|a, b| b.stored.id.cmp(&a.stored.id));
        let mut seen = std::collections::HashSet::new();
        let mut summaries=candidates.into_iter().filter(|ws| {
            let revision = &ws.stored.revisions[&ws.stored.current];
            let has_parts = revision.model["parts"].as_array().is_some_and(|parts| !parts.is_empty());
            let has_inputs = serde_json::to_value(&revision.inputs).ok() != serde_json::to_value(crate::Inputs::default()).ok();
            if !has_parts && !has_inputs && ws.stored.jobs.is_empty() && ws.stored.recordings.is_empty() && ws.stored.motion_tests.is_empty() {
                return false;
            }

            // Model-view tabs share the project folder with the root workspace. Keep every
            // view so dock restore can remount them; only collapse duplicate roots.
            if ws.stored.model_parent.is_some() {
                return true;
            }
            let Some(binding) = ws.stored.project.as_ref() else { return true; };
            let directory = binding.directory.canonicalize().unwrap_or_else(|_| binding.directory.clone());
            seen.insert(directory)
        }).map(|ws|json!({
            "workspace_id":ws.stored.id,
            "name":ws.stored.project.as_ref().and_then(|binding|binding.directory.file_name()).map(|name|name.to_string_lossy().to_string()).or_else(||ws.stored.title.clone()).unwrap_or_else(||"Untitled".into()),
            "running":ws.stored.jobs.values().filter(|job|matches!(job.state,crate::JobState::Queued|crate::JobState::Running)).count()
        })).collect::<Vec<_>>();
        summaries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        json!(summaries)
    }
    pub async fn focused_workspace(&self) -> Option<String> {
        let bytes = tokio::fs::read(self.root.join("ui.json")).await.ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        let workspace = value["workspace_id"].as_str()?;
        self.workspaces
            .lock()
            .await
            .contains_key(workspace)
            .then(|| workspace.to_string())
    }
    pub async fn focus_workspace(&self, workspace: &str) -> Result<()> {
        if !self.workspaces.lock().await.contains_key(workspace) {
            return Err(Error::domain("APP_UNAVAILABLE", "Unknown workspace"));
        }
        let temporary = self.root.join(format!(".ui-{}.tmp", id()));
        tokio::fs::write(
            &temporary,
            serde_json::to_vec(&json!({"workspace_id":workspace}))?,
        )
        .await?;
        tokio::fs::rename(temporary, self.root.join("ui.json")).await?;
        Ok(())
    }
    /// Removes a workspace from Cadpad's recent list and deletes managed app data.
    /// Bound project folders on disk are left untouched.
    pub async fn forget_workspace(&self, workspace: &str) -> Result<()> {
        self.motion_test_runs.forget(workspace);
        let ui_points_here = tokio::fs::read(self.root.join("ui.json"))
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value["workspace_id"].as_str().map(|id| id == workspace))
            .unwrap_or(false);
        let (directory, remaining) = {
            let mut workspaces = self.workspaces.lock().await;
            let Some(mut ws) = workspaces.remove(workspace) else {
                return Err(Error::domain("APP_UNAVAILABLE", "Unknown workspace"));
            };
            for send in ws.cancellation.values() {
                let _ = send.send(true);
            }
            drop(ws.worker.take());
            let directory = self.root.join("workspaces").join(workspace);
            let remaining = workspaces.keys().cloned().collect::<Vec<_>>();
            (directory, remaining)
        };
        if tokio::fs::try_exists(&directory).await.unwrap_or(false) {
            tokio::fs::remove_dir_all(&directory).await?;
        }
        if ui_points_here {
            if let Some(next) = remaining.first() {
                self.focus_workspace(next).await?;
            } else {
                let _ = tokio::fs::remove_file(self.root.join("ui.json")).await;
            }
        }
        Ok(())
    }
    pub async fn forget_workspaces(&self) -> Result<usize> {
        let ids = self.list_workspaces().await;
        for id in &ids {
            self.forget_workspace(id).await?;
        }
        Ok(ids.len())
    }
}

impl Core {
    pub(crate) async fn validate_asset_budget(&self, inputs: &Inputs) -> Result<()> {
        let mut total = inputs
            .files
            .values()
            .map(|source| source.len() as u64)
            .sum::<u64>();
        for digest in inputs.assets.values() {
            if !portable::valid_hash(digest) {
                return Err(Error::domain("INVALID_REQUEST", "Invalid asset digest"));
            }
            let metadata = tokio::fs::metadata(self.root.join("blobs").join(digest)).await?;
            if !metadata.is_file() || metadata.len() > portable::ASSET_LIMIT as u64 {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Managed asset exceeds 64 MiB or is not a regular file",
                ));
            }
            total += metadata.len();
            if total > 256 * 1024 * 1024 {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Project exceeds 256 MiB input limit",
                ));
            }
        }
        Ok(())
    }
}
