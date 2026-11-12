//! Private, bounded BRep offers for explicitly deterministic shape builders, plus
//! skip-source authoritative rebuild when whole-project inputs match a prior
//! revision. Offers are indexed by input digest so an intervening different
//! revision does not erase a still-resident match. Neither path is portable
//! project authority. Annotation-only edits reuse the base publication when
//! geometry-affecting inputs are unchanged and anchors resolve without BRep.
use crate::{Core, Error, Result, annotations, display_reuse, portable, recovery};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio::sync::watch;
const FORMAT: &str = "cadpad-native-shape-v1";
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 512 * 1024;
const MAX_OFFERS: usize = 8;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    handle: String,
    bytes: u64,
    sha256: String,
    geometry_sha256: String,
    shape_type: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    format: String,
    input_digest: String,
    policy: String,
    entries: BTreeMap<String, Entry>,
}
fn manifest(value: &Value) -> Option<Manifest> {
    if serde_json::to_vec(value).ok()?.len() > MAX_MANIFEST_BYTES {
        return None;
    }
    let value: Manifest = serde_json::from_value(value.clone()).ok()?;
    if value.format != FORMAT
        || !portable::valid_hash(&value.input_digest)
        || !portable::valid_hash(&value.policy)
        || value.entries.len() > 128
    {
        return None;
    }
    let mut total = 0u64;
    for (name, entry) in &value.entries {
        if name.is_empty()
            || name.chars().count() > 128
            || entry.bytes == 0
            || entry.bytes > MAX_ENTRY_BYTES
            || !portable::valid_hash(&entry.sha256)
            || entry.handle != format!("{}.brep", entry.sha256)
            || !portable::valid_hash(&entry.geometry_sha256)
            || !matches!(
                entry.shape_type.as_str(),
                "Vertex" | "Edge" | "Wire" | "Face" | "Shell" | "Solid" | "Compound"
            )
        {
            return None;
        }
        total = total.checked_add(entry.bytes)?;
        if total > MAX_BYTES {
            return None;
        }
    }
    Some(value)
}
fn offer(value: &Value) -> Option<(String, Manifest)> {
    let revision = value["revision"].as_str()?.to_owned();
    if uuid::Uuid::parse_str(&revision).is_err() {
        return None;
    }
    Some((revision, manifest(&value["manifest"])?))
}
fn offers(record: &Value) -> Vec<(String, Manifest)> {
    if record.is_null() {
        return Vec::new();
    }
    if let Some(list) = record["offers"].as_array() {
        return list.iter().filter_map(offer).collect();
    }
    offer(record).into_iter().collect()
}
fn encode(offers: Vec<(String, Manifest)>) -> Value {
    json!({
        "schema_version": 2,
        "offers": offers.into_iter().map(|(revision, manifest)| json!({
            "revision": revision,
            "manifest": manifest
        })).collect::<Vec<_>>()
    })
}
pub(crate) fn record(
    previous: &Value,
    revision: &str,
    expected_digest: &str,
    value: &Value,
) -> Value {
    let Some(manifest) = manifest(value).filter(|m| m.input_digest == expected_digest) else {
        return Value::Null;
    };
    let mut next = vec![(revision.to_owned(), manifest)];
    for (prior_revision, prior) in offers(previous) {
        if prior.input_digest == expected_digest || prior_revision == revision {
            continue;
        }
        next.push((prior_revision, prior));
        if next.len() == MAX_OFFERS {
            break;
        }
    }
    encode(next)
}
pub(crate) fn handles(record: &Value, revision: &str) -> Vec<String> {
    offers(record)
        .into_iter()
        .filter(|(offer_revision, _)| offer_revision == revision)
        .flat_map(|(_, manifest)| manifest.entries.into_values().map(|entry| entry.handle))
        .collect()
}
pub(crate) fn retained_revisions(record: &Value) -> Vec<String> {
    offers(record)
        .into_iter()
        .map(|(revision, _)| revision)
        .collect()
}
/// Result of committing a new revision by copying a prior publication whose
/// inputs fingerprint matches, without spawning a worker or executing source.
pub(crate) struct AuthoritativeReuse {
    pub result: Value,
    pub motion: crate::motion::State,
    pub rebound: Vec<crate::portable_recordings::Rebinding>,
}

fn mesh_buffers(model: &Value) -> Result<Vec<(String, u64, String)>> {
    let mut buffers = BTreeMap::new();
    for part in model["parts"].as_array().into_iter().flatten() {
        let Some(mesh) = part.get("mesh") else {
            continue;
        };
        let Some(entries) = mesh["buffers"].as_object() else {
            continue;
        };
        for buffer in entries.values() {
            let handle = buffer["handle"]
                .as_str()
                .ok_or_else(|| Error::domain("INVALID_REQUEST", "Mesh buffer handle missing"))?;
            let hash = buffer["sha256"]
                .as_str()
                .ok_or_else(|| Error::domain("INVALID_REQUEST", "Mesh buffer digest missing"))?;
            let bytes = buffer["bytes"]
                .as_u64()
                .ok_or_else(|| Error::domain("INVALID_REQUEST", "Mesh buffer size missing"))?;
            if !portable::valid_hash(hash) || handle != format!("{hash}.bin") {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Authoritative mesh buffer identity is invalid",
                ));
            }
            buffers.insert(handle.to_owned(), (bytes, hash.to_owned()));
        }
    }
    Ok(buffers
        .into_iter()
        .map(|(handle, (bytes, hash))| (handle, bytes, hash))
        .collect())
}

impl Core {
    /// Annotation-only draft: geometry inputs match the base revision, so copy
    /// that publication's display and reproject saved annotations in-process.
    /// Returns `Ok(None)` when anchors need live BRep or geometry changed.
    pub(crate) async fn try_annotation_only_rebuild(
        &self,
        workspace: &str,
        base: &str,
        revision: &str,
        draft: &crate::Draft,
        cancel: watch::Receiver<bool>,
    ) -> Result<Option<AuthoritativeReuse>> {
        if *cancel.borrow() {
            return Err(Error::domain(
                "CANCELLED",
                "Cancelled before annotation-only reuse",
            ));
        }
        let (source_revision, mut model) = {
            let all = self.workspaces.lock().await;
            let ws = all
                .get(workspace)
                .ok_or_else(|| Error::domain("NOT_FOUND", "Workspace not found"))?;
            if ws.stored.current != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Revision changed before annotation-only reuse",
                ));
            }
            let Some(prior) = ws.stored.revisions.get(base) else {
                return Ok(None);
            };
            if !annotations::same_geometry_inputs(&prior.inputs, &draft.inputs)
                || prior.inputs.annotations == draft.inputs.annotations
            {
                return Ok(None);
            }
            if recovery::validate_publication(&prior.model).is_err()
                || recovery::validate_display_publication(&prior.model).is_err()
                || !prior.model["parts"]
                    .as_array()
                    .is_some_and(|parts| !parts.is_empty())
            {
                return Ok(None);
            }
            let buffers = mesh_buffers(&prior.model)?;
            if buffers.is_empty()
                || !buffers.iter().all(|(handle, _, _)| {
                    self.root
                        .join("artifacts")
                        .join(workspace)
                        .join(base)
                        .join(handle)
                        .is_file()
                })
            {
                return Ok(None);
            }
            (base.to_owned(), prior.model.clone())
        };
        let records = annotations::decode(&draft.inputs.annotations)?;
        let Some(annotations) = annotations::reproject(&records, &model)? else {
            return Ok(None);
        };
        model["annotations"] = annotations;
        if let Some(parts) = model["parts"].as_array_mut() {
            for part in parts {
                if let Some(object) = part.as_object_mut() {
                    object.insert("revision".into(), json!(revision));
                }
            }
        }
        model["revision"] = json!(revision);
        if let Some(mut worker) = self.worker_cache.retire_oldest().await {
            let _ = worker.stop().await;
        }
        let publication = self.display_publication.clone().read_owned().await;
        let root = self.root.clone();
        let workspace_name = workspace.to_owned();
        let revision_name = revision.to_owned();
        let source_name = source_revision.clone();
        let model_for_copy = model.clone();
        let copy_cancel = cancel.clone();
        let copied = tokio::task::spawn_blocking(move || {
            let _publication = publication;
            let scope = display_reuse::Scope {
                root: &root,
                workspace: &workspace_name,
                base: &source_name,
                revision: &revision_name,
            };
            let target = root
                .join("artifacts")
                .join(&workspace_name)
                .join(&revision_name);
            std::fs::create_dir_all(&target)?;
            let _lease = crate::cache::Lease::shared(&target)?;
            let buffers = mesh_buffers(&model_for_copy)?;
            if buffers.is_empty() {
                return Ok(false);
            }
            for (handle, bytes, hash) in buffers {
                if *copy_cancel.borrow() {
                    return Err(Error::domain(
                        "CANCELLED",
                        "Cancelled while copying annotation-only display",
                    ));
                }
                if display_reuse::copy_buffer(scope, &target, &handle, bytes, &hash).is_err() {
                    return Ok(false);
                }
            }
            Ok(true)
        })
        .await
        .map_err(|_| Error::domain("WORKER_UNAVAILABLE", "Annotation-only reuse task failed"))??;
        if !copied {
            return Ok(None);
        }
        if *cancel.borrow() {
            return Err(Error::domain(
                "CANCELLED",
                "Cancelled after annotation-only copy",
            ));
        }
        {
            let all = self.workspaces.lock().await;
            if all[workspace].stored.current != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Revision changed during annotation-only reuse",
                ));
            }
        }
        let inputs = draft.inputs.clone();
        let motion_model = model.clone();
        let motion =
            tokio::task::spawn_blocking(move || crate::motion::State::new(&inputs, &motion_model))
                .await
                .map_err(|_| Error::domain("INVALID_MECHANISM", "Motion validation failed"))??;
        let recording_publication = self.blob_publication.read().await;
        let (recordings, imports) = {
            let workspaces = self.workspaces.lock().await;
            let ws = &workspaces[workspace];
            (
                ws.stored.recordings.clone(),
                ws.stored.recording_imports.clone(),
            )
        };
        let mut inputs = draft.inputs.clone();
        inputs.parameters = model["parameters"].clone();
        inputs.dependencies =
            serde_json::from_value(model["dependencies"].clone()).unwrap_or_default();
        let blobs = self.root.join("blobs");
        let base_name = base.to_owned();
        let target = revision.to_owned();
        let rebound_model = model.clone();
        let mechanism = motion.mechanism.clone();
        let rebound = {
            let _publication = recording_publication;
            tokio::task::spawn_blocking(move || {
                crate::portable_recordings::rebind(
                    &blobs,
                    crate::portable_recordings::Rebuild {
                        base: &base_name,
                        revision: &target,
                        inputs: &inputs,
                        model: &rebound_model,
                        mechanism: &mechanism,
                    },
                    &recordings,
                    &imports,
                )
            })
            .await
            .map_err(|_| {
                Error::domain(
                    "INVALID_RECORDING",
                    "Recording import validation task failed",
                )
            })??
        };
        Ok(Some(AuthoritativeReuse {
            result: json!({
                "data": model,
                "logs": "__source_skipped__\n",
                "logs_truncated": false,
                "geometry_cache": Value::Null,
                "authoritative_reuse": {
                    "source_revision": source_revision,
                    "annotation_only": true,
                },
            }),
            motion,
            rebound,
        }))
    }

    /// When draft inputs match a prior revision's fingerprint and that revision
    /// still has a valid publication, copy its display buffers and commit without
    /// executing source. Returns `Ok(None)` when no safe match exists.
    pub(crate) async fn try_authoritative_rebuild(
        &self,
        workspace: &str,
        base: &str,
        revision: &str,
        draft: &crate::Draft,
        cancel: watch::Receiver<bool>,
    ) -> Result<Option<AuthoritativeReuse>> {
        if *cancel.borrow() {
            return Err(Error::domain(
                "CANCELLED",
                "Cancelled before authoritative reuse",
            ));
        }
        let digest = portable::fingerprint(&draft.inputs)?;
        let matched = {
            let all = self.workspaces.lock().await;
            let ws = all
                .get(workspace)
                .ok_or_else(|| Error::domain("NOT_FOUND", "Workspace not found"))?;
            if ws.stored.current != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Revision changed before authoritative reuse",
                ));
            }
            let mut candidates = Vec::new();
            for (id, prior) in &ws.stored.revisions {
                let prior_digest = match portable::fingerprint(&prior.inputs) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                if prior_digest != digest {
                    continue;
                }
                if recovery::validate_publication(&prior.model).is_err()
                    || recovery::validate_display_publication(&prior.model).is_err()
                    || !prior.model["parts"]
                        .as_array()
                        .is_some_and(|parts| !parts.is_empty())
                {
                    continue;
                }
                candidates.push((id.clone(), prior.model.clone()));
            }
            // Prefer a publication whose display buffers are still on disk.
            // UUID map order is not chronological; skip revisions that soft
            // collection has already stripped of mesh bytes.
            candidates.into_iter().find(|(id, model)| {
                mesh_buffers(model).ok().is_some_and(|buffers| {
                    !buffers.is_empty()
                        && buffers.iter().all(|(handle, _, _)| {
                            self.root
                                .join("artifacts")
                                .join(workspace)
                                .join(id)
                                .join(handle)
                                .is_file()
                        })
                })
            })
        };
        let Some((source_revision, mut model)) = matched else {
            return Ok(None);
        };
        if source_revision == revision {
            return Ok(None);
        }
        // Publication embeds the producing revision on each part; retarget to
        // the candidate so later recovery compares equal contracts.
        if let Some(parts) = model["parts"].as_array_mut() {
            for part in parts {
                if let Some(object) = part.as_object_mut() {
                    object.insert("revision".into(), json!(revision));
                }
            }
        }
        model["revision"] = json!(revision);
        // Release any idle registry that may still hold shared leases on the
        // source revision's display buffers before copying them.
        if let Some(mut worker) = self.worker_cache.retire_oldest().await {
            let _ = worker.stop().await;
        }
        let publication = self.display_publication.clone().read_owned().await;
        let root = self.root.clone();
        let workspace_name = workspace.to_owned();
        let revision_name = revision.to_owned();
        let source_name = source_revision.clone();
        let model_for_copy = model.clone();
        let copy_cancel = cancel.clone();
        let copied = tokio::task::spawn_blocking(move || {
            let _publication = publication;
            let scope = display_reuse::Scope {
                root: &root,
                workspace: &workspace_name,
                base: &source_name,
                revision: &revision_name,
            };
            let target = root
                .join("artifacts")
                .join(&workspace_name)
                .join(&revision_name);
            std::fs::create_dir_all(&target)?;
            let _lease = crate::cache::Lease::shared(&target)?;
            let buffers = mesh_buffers(&model_for_copy)?;
            if buffers.is_empty() {
                return Ok(false);
            }
            for (handle, bytes, hash) in buffers {
                if *copy_cancel.borrow() {
                    return Err(Error::domain(
                        "CANCELLED",
                        "Cancelled while copying authoritative display",
                    ));
                }
                if display_reuse::copy_buffer(scope, &target, &handle, bytes, &hash).is_err() {
                    return Ok(false);
                }
            }
            Ok(true)
        })
        .await
        .map_err(|_| Error::domain("WORKER_UNAVAILABLE", "Authoritative reuse task failed"))??;
        if !copied {
            return Ok(None);
        }
        if *cancel.borrow() {
            return Err(Error::domain(
                "CANCELLED",
                "Cancelled after authoritative copy",
            ));
        }
        let offered = self
            .reusable_geometry(workspace, base, revision, &digest, cancel.clone())
            .await
            .unwrap_or(Value::Null);
        {
            let all = self.workspaces.lock().await;
            if all[workspace].stored.current != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Revision changed during authoritative reuse",
                ));
            }
        }
        let inputs = draft.inputs.clone();
        let motion_model = model.clone();
        let motion =
            tokio::task::spawn_blocking(move || crate::motion::State::new(&inputs, &motion_model))
                .await
                .map_err(|_| Error::domain("INVALID_MECHANISM", "Motion validation failed"))??;
        let recording_publication = self.blob_publication.read().await;
        let (recordings, imports) = {
            let workspaces = self.workspaces.lock().await;
            let ws = &workspaces[workspace];
            (
                ws.stored.recordings.clone(),
                ws.stored.recording_imports.clone(),
            )
        };
        let mut inputs = draft.inputs.clone();
        inputs.parameters = model["parameters"].clone();
        inputs.dependencies =
            serde_json::from_value(model["dependencies"].clone()).unwrap_or_default();
        let blobs = self.root.join("blobs");
        let base_name = base.to_owned();
        let target = revision.to_owned();
        let rebound_model = model.clone();
        let mechanism = motion.mechanism.clone();
        let rebound = {
            let _publication = recording_publication;
            tokio::task::spawn_blocking(move || {
                crate::portable_recordings::rebind(
                    &blobs,
                    crate::portable_recordings::Rebuild {
                        base: &base_name,
                        revision: &target,
                        inputs: &inputs,
                        model: &rebound_model,
                        mechanism: &mechanism,
                    },
                    &recordings,
                    &imports,
                )
            })
            .await
            .map_err(|_| {
                Error::domain(
                    "INVALID_RECORDING",
                    "Recording import validation task failed",
                )
            })??
        };
        Ok(Some(AuthoritativeReuse {
            result: json!({
                "data": model,
                "logs": "__source_skipped__\n",
                "logs_truncated": false,
                "geometry_cache": offered,
                "authoritative_reuse": {"source_revision": source_revision},
            }),
            motion,
            rebound,
        }))
    }

    pub(crate) async fn reusable_geometry(
        &self,
        workspace: &str,
        base: &str,
        revision: &str,
        digest: &str,
        cancel: watch::Receiver<bool>,
    ) -> Result<Value> {
        if *cancel.borrow() {
            return Err(Error::domain("CANCELLED", "Geometry reuse cancelled"));
        }
        let offered = {
            let all = self.workspaces.lock().await;
            let ws = &all[workspace];
            if ws.stored.current != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Revision changed before geometry reuse",
                ));
            }
            offers(&ws.stored.geometry_cache)
                .into_iter()
                .find(|(_, manifest)| manifest.input_digest == digest)
        };
        let Some((source_revision, mut offered)) = offered else {
            return Ok(Value::Null);
        };
        let root = self.root.clone();
        let workspace = workspace.to_owned();
        let revision = revision.to_owned();
        tokio::task::spawn_blocking(move || {
            let target = root.join("artifacts").join(&workspace).join(&revision);
            let scope = display_reuse::Scope {
                root: &root,
                workspace: &workspace,
                base: &source_revision,
                revision: &revision,
            };
            let mut valid = BTreeMap::new();
            for (name, entry) in offered.entries {
                if *cancel.borrow() {
                    return Err(Error::domain("CANCELLED", "Geometry reuse cancelled"));
                }
                if display_reuse::copy_buffer(
                    scope,
                    &target,
                    &entry.handle,
                    entry.bytes,
                    &entry.sha256,
                )
                .is_ok()
                {
                    valid.insert(name, entry);
                }
            }
            offered.entries = valid;
            Ok(json!(offered))
        })
        .await
        .map_err(|_| Error::domain("WORKER_UNAVAILABLE", "Geometry reuse task failed"))?
    }
}
