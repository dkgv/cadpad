//! Content-addressed storage for snapshot sources.
//!
//! Every revision, draft and job snapshot used to embed the complete project
//! source tree (and each revision its published model), so a workspace journal
//! grew by several copies of the project on every build and each save rewrote
//! all of them. Journal bodies now reference content by digest: each file body,
//! each `path -> digest` manifest, each draft hash map and each revision model
//! is stored once per workspace under `sources/<sha256>`.
//!
//! Snapshots are append-only within a workspace, so blobs never become
//! unreferenced while their workspace exists and need no collection; deleting
//! the workspace directory removes them.
use crate::{Error, Result, portable};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

/// Journal key that replaces `inputs.files` in a compact body.
const FILES: &str = "files_manifest";
/// Journal key that replaces a draft's `source_hashes` in a compact body.
const HASHES: &str = "source_hashes_ref";
/// Journal key that replaces a revision's `model` in a compact body.
const MODEL: &str = "model_ref";
/// Journal key that replaces `inputs.assets` in a compact body.
const ASSETS: &str = "assets_ref";
/// Decoded blobs kept in memory per store; rebuilds and history scans reuse
/// the same few file versions many times.
const CACHE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct Cache {
    blobs: HashMap<String, Arc<[u8]>>,
    bytes: usize,
}

pub(crate) struct SourceStore {
    dir: PathBuf,
    known: Mutex<HashSet<String>>,
    cache: Mutex<Cache>,
}

impl SourceStore {
    /// The store for the workspace whose journal lives in `workspace_dir`.
    pub(crate) fn for_workspace(workspace_dir: &Path) -> Arc<Self> {
        static STORES: OnceLock<Mutex<HashMap<PathBuf, Weak<SourceStore>>>> = OnceLock::new();
        let dir = workspace_dir.join("sources");
        let mut stores = STORES
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        stores.retain(|_, store| store.strong_count() > 0);
        if let Some(store) = stores.get(&dir).and_then(Weak::upgrade) {
            return store;
        }
        let store = Arc::new(Self {
            dir: dir.clone(),
            known: Mutex::default(),
            cache: Mutex::default(),
        });
        stores.insert(dir, Arc::downgrade(&store));
        store
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        self.dir == other.dir
    }

    fn put(&self, digest: &str, bytes: &[u8]) -> Result<()> {
        if self.known.lock().unwrap().contains(digest) {
            return Ok(());
        }
        let path = self.dir.join(digest);
        if !path.is_file() {
            std::fs::create_dir_all(&self.dir)?;
            let temporary = self.dir.join(format!(".{}.tmp", crate::id()));
            let mut file = File::create(&temporary)?;
            file.write_all(bytes)?;
            // The journal that references this blob is synced after it, so the
            // blob must be durable first.
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &path)?;
        }
        self.known.lock().unwrap().insert(digest.to_owned());
        Ok(())
    }

    fn get(&self, digest: &str) -> Result<Arc<[u8]>> {
        if let Some(bytes) = self.cache.lock().unwrap().blobs.get(digest) {
            return Ok(bytes.clone());
        }
        if !portable::valid_hash(digest) {
            return Err(Error::domain("INVALID_REQUEST", "Invalid source digest"));
        }
        let bytes: Arc<[u8]> = std::fs::read(self.dir.join(digest))
            .map_err(|e| Error::domain("IO_ERROR", format!("Missing source blob {digest}: {e}")))?
            .into();
        if portable::bytes_hash(&bytes) != digest {
            return Err(Error::domain(
                "IO_ERROR",
                format!("Source blob {digest} is corrupt"),
            ));
        }
        self.known.lock().unwrap().insert(digest.to_owned());
        let mut cache = self.cache.lock().unwrap();
        if cache.bytes + bytes.len() > CACHE_BYTES {
            *cache = Cache::default();
        }
        cache.bytes += bytes.len();
        cache.blobs.insert(digest.to_owned(), bytes.clone());
        Ok(bytes)
    }

    fn put_map(&self, map: &BTreeMap<String, String>) -> Result<String> {
        let bytes = serde_json::to_vec(map)?;
        let digest = portable::bytes_hash(&bytes);
        self.put(&digest, &bytes)?;
        Ok(digest)
    }

    pub(crate) fn get_map(&self, digest: &str) -> Result<BTreeMap<String, String>> {
        Ok(serde_json::from_slice(&self.get(digest)?)?)
    }

    /// Stores a `path -> digest` map and returns its digest. A digest already
    /// computed for the same map skips reserializing it.
    pub(crate) fn store_hashes(
        &self,
        hashes: &BTreeMap<String, String>,
        digest: Option<&str>,
    ) -> Result<String> {
        match digest {
            Some(digest) if self.known.lock().unwrap().contains(digest) => Ok(digest.to_owned()),
            _ => self.put_map(hashes),
        }
    }

    /// Moves `inputs.files` and `source_hashes` of a serialized snapshot into
    /// the store, leaving digests in their place.
    pub(crate) fn compact(&self, value: &mut Value) -> Result<()> {
        if let Some(inputs) = value.get_mut("inputs").and_then(Value::as_object_mut)
            && let Some(Value::Object(files)) = inputs.remove("files")
        {
            let mut manifest = BTreeMap::new();
            for (path, source) in files {
                let Value::String(source) = source else {
                    return Err(Error::domain("INVALID_REQUEST", "Invalid source file"));
                };
                let digest = portable::bytes_hash(source.as_bytes());
                self.put(&digest, source.as_bytes())?;
                manifest.insert(path, digest);
            }
            inputs.insert(FILES.into(), Value::String(self.put_map(&manifest)?));
        }
        if let Some(inputs) = value.get_mut("inputs").and_then(Value::as_object_mut)
            && let Some(assets) = inputs.remove("assets")
        {
            let assets: BTreeMap<String, String> = serde_json::from_value(assets)?;
            inputs.insert(ASSETS.into(), Value::String(self.put_map(&assets)?));
        }
        if let Some(object) = value.as_object_mut()
            && let Some(hashes) = object.remove("source_hashes")
        {
            let hashes: BTreeMap<String, String> = serde_json::from_value(hashes)?;
            object.insert(HASHES.into(), Value::String(self.put_map(&hashes)?));
        }
        if let Some(object) = value.as_object_mut()
            && let Some(model) = object.remove("model")
        {
            let bytes = serde_json::to_vec(&model)?;
            let digest = portable::bytes_hash(&bytes);
            self.put(&digest, &bytes)?;
            object.insert(MODEL.into(), Value::String(digest));
        }
        Ok(())
    }

    /// Whether a serialized body keeps none of the externalized fields inline,
    /// so it can be copied verbatim into a journal of the same workspace.
    pub(crate) fn is_compact(value: &Value) -> bool {
        let inputs = value.get("inputs");
        !(inputs.is_some_and(|i| i.get("files").is_some() || i.get("assets").is_some())
            || value.get("source_hashes").is_some()
            || value.get("model").is_some())
    }

    /// Restores a compacted snapshot body. Returns whether it referenced the store.
    pub(crate) fn expand(store: Option<&Self>, value: &mut Value) -> Result<bool> {
        fn digest(value: Value) -> Result<String> {
            match value {
                Value::String(digest) => Ok(digest),
                _ => Err(Error::domain("INVALID_REQUEST", "Invalid source reference")),
            }
        }
        let missing = || Error::domain("IO_ERROR", "Compact snapshot has no source store");
        let mut compact = false;
        if let Some(inputs) = value.get_mut("inputs").and_then(Value::as_object_mut)
            && let Some(reference) = inputs.remove(FILES)
        {
            let store = store.ok_or_else(missing)?;
            let mut files = Map::new();
            for (path, digest) in store.get_map(&digest(reference)?)? {
                let source = String::from_utf8(store.get(&digest)?.to_vec())
                    .map_err(|_| Error::domain("IO_ERROR", "Source blob is not UTF-8"))?;
                files.insert(path, Value::String(source));
            }
            inputs.insert("files".into(), Value::Object(files));
            compact = true;
        }
        if let Some(inputs) = value.get_mut("inputs").and_then(Value::as_object_mut)
            && let Some(reference) = inputs.remove(ASSETS)
        {
            let store = store.ok_or_else(missing)?;
            let assets = store.get_map(&digest(reference)?)?;
            inputs.insert("assets".into(), serde_json::to_value(assets)?);
            compact = true;
        }
        if let Some(object) = value.as_object_mut()
            && let Some(reference) = object.remove(HASHES)
        {
            let store = store.ok_or_else(missing)?;
            let hashes = store.get_map(&digest(reference)?)?;
            object.insert("source_hashes".into(), serde_json::to_value(hashes)?);
            compact = true;
        }
        if let Some(object) = value.as_object_mut()
            && let Some(reference) = object.remove(MODEL)
        {
            let store = store.ok_or_else(missing)?;
            let model: Value = serde_json::from_slice(&store.get(&digest(reference)?)?)?;
            object.insert("model".into(), model);
            compact = true;
        }
        Ok(compact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn compact_bodies_round_trip_and_share_blobs() {
        let temp = tempfile::tempdir().unwrap();
        let store = SourceStore::for_workspace(temp.path());
        let original = json!({
            "id": "a",
            "model": {"parts": [{"name": "part"}]},
            "source_hashes": {"model.py": "abc"},
            "inputs": {"files": {"model.py": "# one", "lib/a.py": "# shared"}, "assets": {}, "entry_point": "model.py"},
        });
        let mut compact = original.clone();
        store.compact(&mut compact).unwrap();
        assert!(compact["inputs"].get("files").is_none());
        assert!(compact.get("source_hashes").is_none());
        assert!(compact.get("model").is_none());
        assert!(SourceStore::is_compact(&compact));
        assert!(!SourceStore::is_compact(&original));
        let mut second =
            json!({"id": "b", "inputs": {"files": {"model.py": "# two", "lib/a.py": "# shared"}}});
        store.compact(&mut second).unwrap();
        // two file bodies, one shared file, two manifests, one hash map, one
        // model, one empty asset map
        assert_eq!(
            std::fs::read_dir(temp.path().join("sources"))
                .unwrap()
                .count(),
            8
        );
        let mut expanded = compact.clone();
        assert!(SourceStore::expand(Some(&store), &mut expanded).unwrap());
        assert_eq!(expanded, original);
        let mut legacy = original.clone();
        assert!(!SourceStore::expand(None, &mut legacy).unwrap());
        assert_eq!(legacy, original);
        assert!(SourceStore::expand(None, &mut compact.clone()).is_err());
    }
}
