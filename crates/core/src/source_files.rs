//! Source snapshots share immutable storage across history and recovered workspaces.
//! Weak interning never keeps a closed workspace's sources alive.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    collections::{BTreeMap, HashMap},
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, OnceLock, Weak},
};

type Files = BTreeMap<String, String>;
type Pool = HashMap<u64, Vec<Weak<Files>>>;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceFiles(Arc<Files>);
impl From<Files> for SourceFiles {
    fn from(files: Files) -> Self {
        static POOL: OnceLock<Mutex<Pool>> = OnceLock::new();
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        files.hash(&mut hash);
        let mut pool = POOL
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        pool.retain(|_, entries| {
            entries.retain(|entry| entry.strong_count() > 0);
            !entries.is_empty()
        });
        let entries = pool.entry(hash.finish()).or_default();
        for entry in entries.iter().filter_map(Weak::upgrade) {
            if *entry == files {
                return Self(entry);
            }
        }
        let files = Arc::new(files);
        entries.push(Arc::downgrade(&files));
        Self(files)
    }
}
impl Deref for SourceFiles {
    type Target = Files;
    fn deref(&self) -> &Files {
        &self.0
    }
}
impl DerefMut for SourceFiles {
    fn deref_mut(&mut self) -> &mut Files {
        Arc::make_mut(&mut self.0)
    }
}
impl<'a> IntoIterator for &'a SourceFiles {
    type Item = (&'a String, &'a String);
    type IntoIter = std::collections::btree_map::Iter<'a, String, String>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl Serialize for SourceFiles {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.as_ref().serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for SourceFiles {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Files::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovered_snapshots_share_storage_and_edits_preserve_history() {
        let json = r##"{"model.py":"# shared historical source"}"##;
        let original: SourceFiles = serde_json::from_str(json).unwrap();
        let recovered: SourceFiles = serde_json::from_str(json).unwrap();
        assert!(Arc::ptr_eq(&original.0, &recovered.0));
        let mut edited = original.clone();
        edited.insert("model.py".into(), "# edited".into());
        assert_eq!(original["model.py"], "# shared historical source");
        assert_eq!(serde_json::to_string(&recovered).unwrap(), json);
        let weak = Arc::downgrade(&original.0);
        drop(original);
        drop(recovered);
        assert!(weak.upgrade().is_none());
    }
}
