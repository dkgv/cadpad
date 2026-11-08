//! Local-only origin tracking for a standalone Python project. Three-way field
//! merges preserve independent in-app edits; conflicting files require a choice.
use crate::{Error, Inputs, Result, portable};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub entry: PathBuf,
    pub fingerprint: String,
    pub hashes: BTreeMap<String, String>,
    pub applied: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    KeepCadpad,
    UseExternal,
}
fn digest(value: &impl Serialize) -> Result<String> {
    Ok(portable::bytes_hash(&serde_json::to_vec(value)?))
}
fn hashes(inputs: &Inputs) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for (name, source) in &inputs.files {
        result.insert(name.clone(), digest(source)?);
    }
    for (name, hash) in &inputs.assets {
        result.insert(format!("assets/{name}"), digest(hash)?);
    }
    for (name, value) in [
        ("parameters.json", &inputs.parameters),
        ("annotations.json", &inputs.annotations),
        ("mechanism.json", &inputs.mechanism),
    ] {
        result.insert(name.into(), digest(value)?);
    }
    result.insert("$entry_point".into(), digest(&inputs.entry_point)?);
    result.insert("$dependencies".into(), digest(&inputs.dependencies)?);
    Ok(result)
}
impl Origin {
    pub fn capture(entry: PathBuf, inputs: &Inputs, applied: bool) -> Result<Self> {
        Ok(Self {
            entry,
            fingerprint: portable::fingerprint(inputs)?,
            hashes: hashes(inputs)?,
            applied,
        })
    }
    pub fn read(&self) -> Result<(Inputs, BTreeMap<String, Vec<u8>>)> {
        self.read_excluding(None)
    }
    fn read_excluding(
        &self,
        excluded: Option<&Path>,
    ) -> Result<(Inputs, BTreeMap<String, Vec<u8>>)> {
        let parent = self.entry.parent().ok_or_else(|| {
            Error::domain("INVALID_REQUEST", "Python origin has no parent directory")
        })?;
        if parent.canonicalize()? != parent
            || std::fs::symlink_metadata(&self.entry)?
                .file_type()
                .is_symlink()
        {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Python origin moved or became a symlink; reopen its intended location",
            ));
        }
        if parent.join("project.json").exists() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "A project.json appeared beside this Python origin; reopen the project to review its manifest",
            ));
        }
        portable::read_python_project_excluding(&self.entry, excluded)
    }
    pub fn changes(&self, external: &Inputs) -> Result<BTreeMap<String, String>> {
        let other = hashes(external)?;
        Ok(self
            .hashes
            .keys()
            .chain(other.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|key| self.hashes.get(*key) != other.get(*key))
            .map(|key| {
                (
                    key.clone(),
                    if !self.hashes.contains_key(key) {
                        "added"
                    } else if !other.contains_key(key) {
                        "deleted"
                    } else {
                        "modified"
                    }
                    .into(),
                )
            })
            .collect())
    }
    pub fn conflicts(&self, current: &Inputs, external: &Inputs) -> Result<Vec<String>> {
        if !self.applied {
            return Ok(vec![]);
        }
        let ours = hashes(current)?;
        let theirs = hashes(external)?;
        Ok(self
            .hashes
            .keys()
            .chain(ours.keys())
            .chain(theirs.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|key| {
                ours.get(*key) != self.hashes.get(*key)
                    && theirs.get(*key) != self.hashes.get(*key)
                    && ours.get(*key) != theirs.get(*key)
            })
            .cloned()
            .collect())
    }
    pub fn merge(
        &self,
        current: &Inputs,
        external: Inputs,
        resolutions: &BTreeMap<String, Resolution>,
    ) -> Result<Inputs> {
        let conflicts = self.conflicts(current, &external)?;
        if resolutions.keys().any(|key| !conflicts.contains(key)) {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Conflict choices no longer match these files; review the latest changes",
            ));
        }
        let unresolved = conflicts
            .iter()
            .filter(|key| !resolutions.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        if !unresolved.is_empty() {
            return Err(Error::Domain {
                code: "EXTERNAL_EDIT_CONFLICT".into(),
                message:
                    "These files changed both in Cadpad and on disk; choose which version to keep"
                        .into(),
                details: serde_json::json!({"conflicts":unresolved}),
            });
        }
        if !self.applied {
            return Ok(external);
        }
        let theirs = hashes(&external)?;
        let use_external = |key: &str| -> bool {
            if let Some(choice) = resolutions.get(key) {
                matches!(choice, Resolution::UseExternal)
            } else {
                theirs.get(key) != self.hashes.get(key)
            }
        };
        let mut result = current.clone();
        for key in current
            .files
            .keys()
            .chain(external.files.keys())
            .chain(
                self.hashes
                    .keys()
                    .filter(|key| key.ends_with(".py") && !key.starts_with("assets/")),
            )
            .collect::<BTreeSet<_>>()
        {
            if use_external(key) {
                if let Some(value) = external.files.get(key) {
                    result.files.insert(key.clone(), value.clone());
                } else {
                    result.files.remove(key);
                }
            }
        }
        let mut assets = current
            .assets
            .keys()
            .chain(external.assets.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        assets.extend(
            self.hashes
                .keys()
                .filter_map(|key| key.strip_prefix("assets/").map(str::to_string)),
        );
        for key in assets {
            if use_external(&format!("assets/{key}")) {
                if let Some(value) = external.assets.get(&key) {
                    result.assets.insert(key, value.clone());
                } else {
                    result.assets.remove(&key);
                }
            }
        }
        if use_external("parameters.json") {
            result.parameters = external.parameters;
        }
        if use_external("annotations.json") {
            result.annotations = external.annotations;
        }
        if use_external("mechanism.json") {
            result.mechanism = external.mechanism;
        }
        if use_external("$entry_point") {
            result.entry_point = external.entry_point;
        }
        if use_external("$dependencies") {
            result.dependencies = external.dependencies;
        }
        portable::validate_inputs(&result)?;
        Ok(result)
    }
    pub fn entry_name(&self) -> Option<&str> {
        self.entry.file_name().and_then(|name| name.to_str())
    }
}

pub(crate) fn unchanged(origin: &Origin, excluded: Option<&Path>) -> Result<()> {
    let (inputs, _) = origin.read_excluding(excluded).map_err(|error| {
        Error::domain(
            "EXTERNAL_CHANGES",
            format!("Cannot verify the original Python files before Save: {error}"),
        )
    })?;
    if portable::fingerprint(&inputs)? != origin.fingerprint {
        return Err(Error::domain(
            "EXTERNAL_CHANGES",
            "Original Python files changed; review and reload them before Save",
        ));
    }
    Ok(())
}

impl crate::Core {
    async fn python_origin_context(
        &self,
        workspace: &str,
    ) -> Result<Option<(Origin, String, Inputs)>> {
        let workspaces = self.workspaces.lock().await;
        let ws = workspaces
            .get(workspace)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        if ws.stored.project.is_some() {
            return Ok(None);
        }
        Ok(ws.stored.python_origin.as_ref().map(|origin| {
            (
                origin.clone(),
                ws.stored.current.clone(),
                ws.stored.revisions[&ws.stored.current].inputs.clone(),
            )
        }))
    }
    pub(crate) async fn scan_python_origin(
        &self,
        workspace: &str,
    ) -> Result<Option<serde_json::Value>> {
        let Some((origin, revision, inputs)) = self.python_origin_context(workspace).await? else {
            return Ok(None);
        };
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move||->Result<_>{
            let (external,_)=origin.read()?;
            let fingerprint=portable::fingerprint(&external)?;
            let changes=origin.changes(&external)?;let conflicts=origin.conflicts(&inputs,&external)?;
            Ok(Some(serde_json::json!({"workspace_id":workspace,"revision":revision,"source_kind":"python","changed":fingerprint!=origin.fingerprint,"fingerprint":fingerprint,"changes":changes,"conflicts":conflicts})))
        }).await.map_err(|_|Error::domain("IO_ERROR","Python origin scan failed"))?
    }
    pub(crate) async fn stage_python_origin(
        &self,
        workspace: &str,
        base: &str,
        observed: &str,
        actor: &str,
        resolutions: &BTreeMap<String, Resolution>,
    ) -> Result<Option<crate::Draft>> {
        let Some((origin, revision, inputs)) = self.python_origin_context(workspace).await? else {
            return Ok(None);
        };
        if revision != base {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Reload base is no longer current",
            ));
        }
        let previous_origin = origin.clone();
        let observed = observed.to_owned();
        let resolutions = resolutions.clone();
        let blobs_directory = self.root.join("blobs");
        let (merged, origin) = tokio::task::spawn_blocking(move || -> Result<_> {
            let (external, blobs) = origin.read()?;
            if portable::fingerprint(&external)? != observed {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Original Python files changed again; review their latest contents",
                ));
            }
            let accepted = Origin::capture(origin.entry.clone(), &external, true)?;
            let merged = origin.merge(&inputs, external, &resolutions)?;
            portable::store_blobs(&blobs_directory, &blobs)?;
            Ok((merged, accepted))
        })
        .await
        .map_err(|_| Error::domain("IO_ERROR", "Python origin reload failed"))??;
        self.validate_asset_budget(&merged).await?;
        let mut workspaces = self.workspaces.lock().await;
        self.ensure_actor_active(actor)?;
        let ws = workspaces
            .get_mut(workspace)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        if ws.stored.current != base
            || ws.stored.python_origin.as_ref() != Some(&previous_origin)
            || ws.stored.project.is_some()
        {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Project or origin changed while reading the external files",
            ));
        }
        let mut draft = Self::input_draft(merged, base, actor);
        draft.python_origin = Some(origin);
        draft.source_hashes = ws.stored.revisions[base]
            .inputs
            .files
            .iter()
            .map(|(name, source)| (name.clone(), crate::hash(source)))
            .collect();
        let mut next = ws.stored.clone();
        next.drafts.insert(draft.id.clone(), draft.clone());
        self.persist(&next).await?;
        ws.stored = next;
        Ok(Some(draft))
    }
}

impl crate::Core {
    /// Native Save checks the tracked origin before opening a destination dialog;
    /// save_project checks again at its own I/O boundaries.
    pub async fn check_python_origin_before_save(&self, workspace: &str) -> Result<()> {
        let Some((origin, _, _)) = self.python_origin_context(workspace).await? else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || unchanged(&origin, None))
            .await
            .map_err(|_| Error::domain("IO_ERROR", "Python origin check failed"))?
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    #[test]
    fn asset_deletions_and_python_named_assets_do_not_become_source_modules() {
        let mut base = Inputs::default();
        base.assets.insert("module.py".into(), "a".repeat(64));
        base.assets.insert("retained.bin".into(), "b".repeat(64));
        let origin = Origin::capture("/local/main.py".into(), &base, true).unwrap();
        let mut current = base.clone();
        current.assets.insert("local.bin".into(), "c".repeat(64));
        current.annotations = serde_json::json!([{"id":"local","label":"Width","definition":{"kind":"overall","instance_id":"part","axis":"x"}}]);
        let mut external = base;
        external.assets.remove("module.py");
        external
            .assets
            .insert("retained.bin".into(), "d".repeat(64));
        let merged = origin.merge(&current, external, &BTreeMap::new()).unwrap();
        assert!(!merged.assets.contains_key("module.py"));
        assert_eq!(merged.assets["local.bin"], "c".repeat(64));
        assert_eq!(merged.assets["retained.bin"], "d".repeat(64));
        assert!(!merged.files.contains_key("assets/module.py"));
        assert_eq!(merged.annotations, current.annotations);
    }
}
