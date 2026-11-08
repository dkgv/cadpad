//! Completed motion data travels separately from executable project inputs.
//! Rebinding is restricted to the first matching explicit build after Open.
use crate::{Error, Inputs, Result, Revision, kinematics::Mechanism, playback, portable};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORDINGS: usize = 16;
fn invalid(message: &str) -> Error {
    Error::domain("INVALID_RECORDING", message)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Import {
    pub model_fingerprint: String,
    pub pending_revision: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    metadata: playback::Metadata,
    model_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_inputs: Option<Inputs>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Archive {
    schema_version: u32,
    recordings: Vec<Entry>,
}
#[derive(Default)]
pub(crate) struct Loaded {
    pub recordings: BTreeMap<String, playback::Metadata>,
    pub imports: BTreeMap<String, Import>,
    pub blobs: BTreeMap<String, Vec<u8>>,
    pub revisions: BTreeMap<String, Revision>,
}
pub(crate) struct Rebinding {
    pub id: String,
    pub old_digest: String,
    pub metadata: playback::Metadata,
}
pub(crate) fn model_fingerprint(model: &Value) -> Result<String> {
    if model["rebuild_required"] == true {
        return Err(invalid("Recording geometry provenance is unavailable"));
    }
    crate::recovery::validate_publication(model)?;
    let mut contract = crate::recovery::contract(model);
    // A new explicit build has a new envelope revision. All geometry,
    // instance/topology/publication metadata and dependencies must still match.
    contract.as_object_mut().unwrap().remove("revision");
    Ok(portable::bytes_hash(&serde_json::to_vec(&contract)?))
}
fn validate_entry(entry: &Entry, revisions: &BTreeMap<String, Revision>) -> Result<()> {
    let revision = revisions
        .get(&entry.metadata.revision)
        .ok_or_else(|| invalid("Recording references missing source history"))?;
    if !portable::valid_hash(&entry.model_fingerprint)
        || entry.metadata.bytes > MAX_BYTES
        || !portable::valid_hash(&entry.metadata.digest)
        || portable::fingerprint(&revision.inputs)? != entry.metadata.input_fingerprint
    {
        return Err(invalid("Recording source or geometry provenance mismatch"));
    }
    Ok(())
}

pub(crate) fn save(
    root: &Path,
    blobs: &Path,
    recordings: &BTreeMap<String, playback::Metadata>,
    imports: &BTreeMap<String, Import>,
    revisions: &BTreeMap<String, Revision>,
) -> Result<Option<String>> {
    if recordings.is_empty() {
        return Ok(None);
    }
    if recordings.len() > MAX_RECORDINGS {
        return Err(invalid("Portable project exceeds 16 recordings"));
    }
    let mut entries = Vec::new();
    let mut total = 0usize;
    // Validate all data before publishing any archive files or root manifest.
    let mut payloads = BTreeMap::new();
    for (id, metadata) in recordings {
        if id != &metadata.id {
            return Err(invalid("Recording identity mismatch"));
        }
        total = total
            .checked_add(metadata.bytes)
            .filter(|n| *n <= TOTAL_BYTES)
            .ok_or_else(|| invalid("Portable recordings exceed 64 MiB"))?;
        let model = revisions
            .get(&metadata.revision)
            .ok_or_else(|| invalid("Recording revision is missing"))?;
        let model_fingerprint = if model.model["rebuild_required"] == true {
            imports
                .get(id)
                .ok_or_else(|| invalid("Recording geometry provenance is missing"))?
                .model_fingerprint
                .clone()
        } else {
            model_fingerprint(&model.model)?
        };
        let entry = Entry {
            metadata: metadata.clone(),
            model_fingerprint,
            source_inputs: (root.file_name().is_some_and(|name| name == ".cadpad"))
                .then(|| model.inputs.clone()),
        };
        validate_entry(&entry, revisions)?;
        let bytes = portable::read_bounded(&blobs.join(&metadata.digest), MAX_BYTES)?;
        playback::validate_recording_bytes(&bytes, metadata)?;
        payloads.insert(metadata.digest.clone(), bytes);
        entries.push(entry);
    }
    for (digest, bytes) in payloads {
        portable::atomic_write(
            root,
            &format!(".cadpad-recordings/blobs/{digest}.json"),
            &bytes,
        )?;
    }
    let bytes = serde_json::to_vec(&Archive {
        schema_version: 1,
        recordings: entries,
    })?;
    let digest = portable::bytes_hash(&bytes);
    portable::atomic_write(
        root,
        &format!(".cadpad-recordings/index/{digest}.json"),
        &bytes,
    )?;
    Ok(Some(digest))
}

pub(crate) fn load(
    root: &Path,
    manifest: &portable::Manifest,
    revisions: &BTreeMap<String, Revision>,
) -> Result<Loaded> {
    let Some(digest) = &manifest.recordings else {
        return Ok(Loaded::default());
    };
    if !portable::valid_hash(digest) {
        return Err(invalid("Invalid recording archive digest"));
    }
    let path = portable::checked_path(root, &format!(".cadpad-recordings/index/{digest}.json"))?;
    let bytes = portable::read_bounded(&path, TOTAL_BYTES)?;
    if portable::bytes_hash(&bytes) != *digest {
        return Err(invalid("Recording archive digest mismatch"));
    }
    let archive: Archive = serde_json::from_slice(&bytes)?;
    if archive.schema_version != 1 || archive.recordings.len() > MAX_RECORDINGS {
        return Err(invalid("Unsupported or oversized recording archive"));
    }
    let mut loaded = Loaded::default();
    let mut sources = revisions.clone();
    let mut total = 0usize;
    for entry in archive.recordings {
        if manifest.schema_version == 3 {
            let inputs = entry
                .source_inputs
                .as_ref()
                .ok_or_else(|| invalid("Recording source provenance is missing"))?;
            portable::validate_inputs(inputs)?;
            if uuid::Uuid::parse_str(&entry.metadata.revision).is_err() {
                return Err(invalid("Invalid recording revision"));
            }
            if let Some(prior) = sources.get(&entry.metadata.revision) {
                if portable::fingerprint(&prior.inputs)? != portable::fingerprint(inputs)? {
                    return Err(invalid("Conflicting recording source provenance"));
                }
            } else {
                let revision = Revision {
                    id: entry.metadata.revision.clone(),
                    parent: None,
                    inputs: inputs.clone(),
                    model: serde_json::json!({"parts":[],"rebuild_required":true}),
                    actor: "recording-import".into(),
                    restored_from: None,
                };
                loaded
                    .revisions
                    .insert(revision.id.clone(), revision.clone());
                sources.insert(revision.id.clone(), revision);
            }
        }
        validate_entry(&entry, &sources)?;
        let metadata = entry.metadata;
        total = total
            .checked_add(metadata.bytes)
            .filter(|n| *n <= TOTAL_BYTES)
            .ok_or_else(|| invalid("Portable recordings exceed 64 MiB"))?;
        if loaded.recordings.contains_key(&metadata.id) {
            return Err(invalid("Duplicate portable recording ID"));
        }
        let path = portable::checked_path(
            root,
            &format!(".cadpad-recordings/blobs/{}.json", metadata.digest),
        )?;
        let bytes = portable::read_bounded(&path, MAX_BYTES)?;
        playback::validate_recording_bytes(&bytes, &metadata)?;
        loaded.blobs.insert(metadata.digest.clone(), bytes);
        loaded.imports.insert(
            metadata.id.clone(),
            Import {
                model_fingerprint: entry.model_fingerprint,
                pending_revision: (metadata.revision == manifest.saved_revision
                    || manifest.schema_version == 3)
                    .then(|| metadata.revision.clone()),
            },
        );
        loaded.recordings.insert(metadata.id.clone(), metadata);
    }
    Ok(loaded)
}

pub(crate) struct Rebuild<'a> {
    pub base: &'a str,
    pub revision: &'a str,
    pub inputs: &'a Inputs,
    pub model: &'a Value,
    pub mechanism: &'a Mechanism,
}
pub(crate) fn rebind(
    blobs: &Path,
    rebuild: Rebuild<'_>,
    recordings: &BTreeMap<String, playback::Metadata>,
    imports: &BTreeMap<String, Import>,
) -> Result<Vec<Rebinding>> {
    let Rebuild {
        base,
        revision,
        inputs,
        model,
        mechanism,
    } = rebuild;
    if !imports
        .values()
        .any(|import| import.pending_revision.as_deref() == Some(base))
    {
        return Ok(Vec::new());
    }
    let input_fingerprint = portable::fingerprint(inputs)?;
    let model_fingerprint = model_fingerprint(model)?;
    let mut results = Vec::new();
    let mut payloads = BTreeMap::new();
    for (id, import) in imports {
        let Some(metadata) = recordings.get(id) else {
            continue;
        };
        if import.pending_revision.as_deref() != Some(base)
            || metadata.revision != base
            || metadata.input_fingerprint != input_fingerprint
            || import.model_fingerprint != model_fingerprint
        {
            continue;
        }
        let bytes = portable::read_bounded(&blobs.join(&metadata.digest), MAX_BYTES)?;
        let (rebound, bytes) = playback::rebase_recording(&bytes, metadata, revision, mechanism)?;
        payloads.insert(rebound.digest.clone(), bytes);
        results.push(Rebinding {
            id: id.clone(),
            old_digest: metadata.digest.clone(),
            metadata: rebound,
        });
    }
    portable::store_blobs(blobs, &payloads)?;
    Ok(results)
}
