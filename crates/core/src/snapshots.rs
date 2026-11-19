//! Immutable snapshot bodies remain in journal byte ranges. Only the current
//! revision is cached; historical reads own their payload for one operation.
use crate::{Draft, Error, Inputs, Result, Revision, SourceFiles, StoredWorkspace, portable};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    marker::PhantomData,
    path::Path,
    sync::{Arc, RwLock},
};

#[derive(Clone, Serialize, Deserialize, Default)]
pub(crate) struct Summary {
    pub id: String,
    pub parent: Option<String>,
    pub actor: String,
    pub restored_from: Option<String>,
    pub input_fingerprint: String,
    pub content_fingerprint: String,
    pub entry_point: String,
    pub generated: bool,
    pub source_hashes: SourceFiles,
    pub assets: BTreeMap<String, String>,
    pub parameters: Value,
    pub part_names: Vec<String>,
    pub part_count: usize,
    pub has_content: bool,
    pub mesh_handles: HashSet<String>,
    pub protected: bool,
}
fn content_fingerprint(inputs: &Inputs) -> String {
    let mut inputs = inputs.clone();
    let generated = inputs.files.get(&inputs.entry_point).is_some_and(|s| {
        s.starts_with("# Generated Cadpad entry point.") || s == portable::PROJECT_BROWSER_SOURCE
    });
    if generated {
        let entry = inputs.entry_point.clone();
        inputs.files.remove(&entry);
    }
    inputs.entry_point.clear();
    inputs.dependencies.clear();
    portable::fingerprint(&inputs).expect("serializable inputs")
}
pub(crate) trait Snapshot:
    Clone + Serialize + DeserializeOwned + Send + Sync + 'static
{
    fn snapshot_id(&self) -> &str;
    fn summary(&self) -> Summary;
}
fn input_summary(id: &str, actor: &str, inputs: &Inputs) -> Summary {
    Summary {
        id: id.into(),
        actor: actor.into(),
        input_fingerprint: portable::fingerprint(inputs).expect("serializable inputs"),
        content_fingerprint: content_fingerprint(inputs),
        entry_point: inputs.entry_point.clone(),
        generated: inputs.files.get(&inputs.entry_point).is_some_and(|s| {
            s.starts_with("# Generated Cadpad entry point.")
                || s == portable::PROJECT_BROWSER_SOURCE
        }),
        source_hashes: inputs
            .files
            .iter()
            .map(|(name, source)| (name.clone(), crate::hash(source)))
            .collect::<BTreeMap<_, _>>()
            .into(),
        assets: inputs.assets.clone(),
        parameters: inputs.parameters.clone(),
        has_content: *inputs != Inputs::default(),
        ..Summary::default()
    }
}
impl Snapshot for Revision {
    fn snapshot_id(&self) -> &str {
        &self.id
    }
    fn summary(&self) -> Summary {
        self.summarize_revision(input_summary(&self.id, &self.actor, &self.inputs))
    }
}
impl Revision {
    fn summarize_revision(&self, mut summary: Summary) -> Summary {
        summary.id.clone_from(&self.id);
        summary.actor.clone_from(&self.actor);
        summary.parent = self.parent.clone();
        summary.restored_from = self.restored_from.clone();
        summary.part_count = 0;
        summary.part_names.clear();
        summary.mesh_handles.clear();
        if let Some(parts) = self.model["parts"].as_array() {
            summary.part_count = parts.len();
            summary.part_names = parts
                .iter()
                .filter_map(|p| p["name"].as_str().map(str::to_owned))
                .collect();
            for part in parts {
                if let Some(buffers) = part["mesh"]["buffers"].as_object() {
                    summary.mesh_handles.extend(
                        buffers
                            .values()
                            .filter_map(|v| v["handle"].as_str().map(str::to_owned)),
                    );
                }
            }
        }
        summary.has_content = self.inputs != Inputs::default() || summary.part_count > 0;
        summary.protected = (!crate::stl_model::is_mesh(&self.model)
            && crate::recovery::validate_publication(&self.model).is_err())
            || crate::recovery::validate_display_publication(&self.model).is_err();
        summary
    }
}
impl Summary {
    pub fn same_content(&self, other: &Self) -> bool {
        self.content_fingerprint == other.content_fingerprint
            && (self.entry_point == other.entry_point || self.generated || other.generated)
    }
}
impl Snapshot for Draft {
    fn snapshot_id(&self) -> &str {
        &self.id
    }
    fn summary(&self) -> Summary {
        input_summary(&self.id, &self.actor, &self.inputs)
    }
}

#[derive(Clone)]
struct Range {
    file: Arc<File>,
    start: u64,
    bytes: u64,
}
struct RangeReader {
    range: Range,
    offset: u64,
}
impl Read for RangeReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = buffer.len().min((self.range.bytes - self.offset) as usize);
        if count == 0 {
            return Ok(0);
        }
        #[cfg(unix)]
        use std::os::unix::fs::FileExt;
        #[cfg(windows)]
        use std::os::windows::fs::FileExt;
        #[cfg(unix)]
        let count = self
            .range
            .file
            .read_at(&mut buffer[..count], self.range.start + self.offset)?;
        #[cfg(windows)]
        let count = self
            .range
            .file
            .seek_read(&mut buffer[..count], self.range.start + self.offset)?;
        self.offset += count as u64;
        Ok(count)
    }
}
impl Range {
    fn reader(&self) -> RangeReader {
        RangeReader {
            range: self.clone(),
            offset: 0,
        }
    }
}
#[derive(Clone)]
struct Entry<T> {
    summary: Summary,
    location: Arc<RwLock<Option<Range>>>,
    value: Option<Arc<T>>,
}
#[derive(Clone)]
pub(crate) struct Snapshots<T> {
    entries: BTreeMap<String, Entry<T>>,
    parking: Option<(Arc<std::sync::Mutex<File>>, Arc<File>)>,
}
impl<T> Default for Snapshots<T> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            parking: None,
        }
    }
}
impl<T: Snapshot> From<BTreeMap<String, T>> for Snapshots<T> {
    fn from(values: BTreeMap<String, T>) -> Self {
        let mut result = Self::default();
        for (id, value) in values {
            result.insert(id, value);
        }
        result
    }
}
impl<T: Snapshot> Snapshots<T> {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn contains_key(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }
    pub fn headers(&self) -> impl Iterator<Item = (&String, &Summary)> {
        self.entries.iter().map(|(id, entry)| (id, &entry.summary))
    }
    pub fn header(&self, id: &str) -> Option<&Summary> {
        self.entries.get(id).map(|e| &e.summary)
    }
    pub fn insert(&mut self, id: String, value: T) {
        let summary = value.summary();
        self.insert_summary(id, value, summary);
    }
    fn insert_summary(&mut self, id: String, value: T, summary: Summary) {
        self.entries.insert(
            id,
            Entry {
                summary,
                value: Some(Arc::new(value)),
                location: Arc::new(RwLock::new(None)),
            },
        );
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn append(&mut self, other: Self) {
        self.entries.extend(other.entries);
    }
    pub fn resident(&self, id: &str) -> Option<&T> {
        self.entries.get(id)?.value.as_deref()
    }
    pub fn resident_mut(&mut self, id: &str) -> Option<SnapshotEdit<'_, T>> {
        let entry = self.entries.get_mut(id)?;
        entry.value.as_ref()?;
        entry.location = Arc::new(RwLock::new(None));
        Some(SnapshotEdit { entry })
    }
    pub fn read(&self, id: &str) -> Result<Arc<T>> {
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown snapshot"))?;
        if let Some(value) = &entry.value {
            return Ok(value.clone());
        }
        let range = entry
            .location
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::domain("IO_ERROR", "Snapshot has no durable body"))?;
        let value: T = serde_json::from_reader(BufReader::new(range.reader()))?;
        if value.snapshot_id() != id {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Snapshot identity mismatch",
            ));
        }
        Ok(Arc::new(value))
    }
    pub async fn load(&self, id: &str) -> Result<Arc<T>> {
        let mut one = Self::default();
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown snapshot"))?;
        if let Some(value) = &entry.value {
            return Ok(value.clone());
        }
        one.entries.insert(id.into(), entry.clone());
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || one.read(&id))
            .await
            .map_err(|e| Error::domain("IO_ERROR", e.to_string()))?
    }
    pub fn release_except(&mut self, id: Option<&str>) {
        for (key, entry) in &mut self.entries {
            if Some(key.as_str()) != id && entry.location.read().unwrap().is_some() {
                entry.value = None;
            }
        }
        self.parking = None;
    }
    pub async fn retain_current(&mut self, id: Option<&str>) -> Result<()> {
        self.parking = None;
        if let Some(id) = id {
            let value = self.load(id).await?;
            self.entries.get_mut(id).unwrap().value = Some(value);
        }
        for (key, entry) in &mut self.entries {
            if Some(key.as_str()) != id && entry.location.read().unwrap().is_some() {
                entry.value = None;
            }
        }
        Ok(())
    }
    pub fn park(&mut self, id: String, value: T) -> Result<()> {
        if self.parking.is_none() {
            let file = tempfile::tempfile()?;
            let reader = Arc::new(file.try_clone()?);
            self.parking = Some((Arc::new(std::sync::Mutex::new(file)), reader));
        }
        let (writer, file) = self.parking.as_ref().unwrap();
        let mut writer = writer.lock().unwrap();
        use std::io::{Seek, SeekFrom};
        let start = writer.seek(SeekFrom::End(0))?;
        serde_json::to_writer(&mut *writer, &value)?;
        writer.flush()?;
        let bytes = writer.stream_position()? - start;
        self.entries.insert(
            id,
            Entry {
                summary: value.summary(),
                value: None,
                location: Arc::new(RwLock::new(Some(Range {
                    file: file.clone(),
                    start,
                    bytes,
                }))),
            },
        );
        Ok(())
    }
}
impl Snapshots<Draft> {
    /// The queued copy differs from the draft only in its snapshot ID.
    pub(crate) fn insert_job_copy(&mut self, snapshot: Draft, mut summary: Summary) {
        summary.id.clone_from(&snapshot.id);
        self.insert_summary(snapshot.id.clone(), snapshot, summary);
    }
}
impl Snapshots<Revision> {
    /// Reuse only when the caller has established that the inputs are unchanged.
    pub(crate) fn insert_with_unchanged_inputs(
        &mut self,
        revision: Revision,
        input_summary: Summary,
    ) {
        let summary = revision.summarize_revision(input_summary);
        self.insert_summary(revision.id.clone(), revision, summary);
    }
}
pub(crate) struct SnapshotEdit<'a, T: Snapshot> {
    entry: &'a mut Entry<T>,
}
impl<T: Snapshot> std::ops::Deref for SnapshotEdit<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.entry.value.as_deref().unwrap()
    }
}
impl<T: Snapshot> std::ops::DerefMut for SnapshotEdit<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(self.entry.value.as_mut().unwrap())
    }
}
impl<T: Snapshot> Drop for SnapshotEdit<'_, T> {
    fn drop(&mut self) {
        self.entry.summary = self.entry.value.as_deref().unwrap().summary();
    }
}

// Normal serde remains compatible with the original journal format. Production
// persistence below streams unchanged byte ranges without decoding them.
impl<T: Snapshot> Serialize for Snapshots<T> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for id in self.keys() {
            let value = self.read(id).map_err(serde::ser::Error::custom)?;
            map.serialize_entry(id, value.as_ref())?;
        }
        map.end()
    }
}
impl<'de, T: Snapshot> Deserialize<'de> for Snapshots<T> {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Ok(BTreeMap::<String, T>::deserialize(deserializer)?.into())
    }
}

#[derive(Serialize, Deserialize)]
struct IndexedEntry {
    summary: Summary,
    start: u64,
    bytes: u64,
}
#[derive(Serialize, Deserialize)]
struct Index {
    version: u32,
    stamp: crate::workspace_memory::Stamp,
    metadata: StoredWorkspace,
    revisions: BTreeMap<String, IndexedEntry>,
    drafts: BTreeMap<String, IndexedEntry>,
    job_snapshots: BTreeMap<String, IndexedEntry>,
}
fn restore<T: Snapshot>(entries: BTreeMap<String, IndexedEntry>, file: &Arc<File>) -> Snapshots<T> {
    Snapshots {
        parking: None,
        entries: entries
            .into_iter()
            .map(|(id, e)| {
                (
                    id,
                    Entry {
                        summary: e.summary,
                        value: None,
                        location: Arc::new(RwLock::new(Some(Range {
                            file: file.clone(),
                            start: e.start,
                            bytes: e.bytes,
                        }))),
                    },
                )
            })
            .collect(),
    }
}
fn indexed<T: Snapshot>(
    entries: &Snapshots<T>,
    file: &Arc<File>,
) -> Result<BTreeMap<String, IndexedEntry>> {
    entries
        .headers()
        .map(|(id, _)| {
            let entry = &entries.entries[id];
            let range = entry.location.read().unwrap();
            let range = range
                .as_ref()
                .ok_or_else(|| Error::domain("IO_ERROR", "Unwritten snapshot"))?;
            if !Arc::ptr_eq(&range.file, file) {
                return Err(Error::domain(
                    "IO_ERROR",
                    "Snapshot index references another journal",
                ));
            }
            Ok((
                id.clone(),
                IndexedEntry {
                    summary: entry.summary.clone(),
                    start: range.start,
                    bytes: range.bytes,
                },
            ))
        })
        .collect()
}
fn metadata(stored: &StoredWorkspace) -> StoredWorkspace {
    let mut result = stored.clone();
    result.revisions.clear();
    result.drafts.clear();
    result.job_snapshots.clear();
    result
}
fn write_index(directory: &Path, stored: &StoredWorkspace, file: &Arc<File>) -> Result<()> {
    let index = Index {
        version: 1,
        stamp: crate::workspace_memory::Stamp::of(file)?,
        metadata: metadata(stored),
        revisions: indexed(&stored.revisions, file)?,
        drafts: indexed(&stored.drafts, file)?,
        job_snapshots: indexed(&stored.job_snapshots, file)?,
    };
    let temporary = directory.join(format!("{}.tmp", crate::id()));
    let mut writer = io::BufWriter::new(File::create(&temporary)?);
    serde_json::to_writer(&mut writer, &index)?;
    writer.flush()?;
    drop(writer);
    std::fs::rename(temporary, directory.join("workspace.index.json"))?;
    Ok(())
}

// A bounded lexical scanner records top-level value spans. Snapshot bodies are
// validated and summarized individually, never collected into one Value tree.
struct Scanner {
    reader: BufReader<File>,
    offset: u64,
}
impl Scanner {
    fn peek(&mut self) -> Result<Option<u8>> {
        Ok(self.reader.fill_buf()?.first().copied())
    }
    fn byte(&mut self) -> Result<u8> {
        let c = self
            .peek()?
            .ok_or_else(|| Error::domain("INVALID_REQUEST", "Truncated workspace journal"))?;
        self.reader.consume(1);
        self.offset += 1;
        Ok(c)
    }
    fn ws(&mut self) -> Result<()> {
        while self.peek()?.is_some_and(|c| c.is_ascii_whitespace()) {
            self.byte()?;
        }
        Ok(())
    }
    fn expect(&mut self, c: u8) -> Result<()> {
        self.ws()?;
        if self.byte()? != c {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Invalid workspace journal",
            ));
        }
        Ok(())
    }
    fn key(&mut self) -> Result<String> {
        self.ws()?;
        let mut bytes = vec![self.byte()?];
        if bytes[0] != b'"' {
            return Err(Error::domain("INVALID_REQUEST", "Expected journal key"));
        }
        let mut escape = false;
        loop {
            let c = self.byte()?;
            bytes.push(c);
            if c == b'"' && !escape {
                break;
            }
            if c == b'\\' && !escape {
                escape = true;
            } else {
                escape = false;
            }
            if bytes.len() > 4096 {
                return Err(Error::domain("INVALID_REQUEST", "Journal key is too long"));
            }
        }
        self.expect(b':')?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn span(&mut self) -> Result<(u64, u64)> {
        self.ws()?;
        let start = self.offset;
        let primitive = !matches!(self.peek()?, Some(b'{' | b'[' | b'"'));
        let mut depth = 0usize;
        let mut string = false;
        let mut escape = false;
        loop {
            let Some(c) = self.peek()? else {
                break;
            };
            if !string && depth == 0 && (c == b',' || c == b'}' || c.is_ascii_whitespace()) {
                break;
            }
            self.byte()?;
            if string {
                if c == b'"' && !escape {
                    string = false;
                }
                if c == b'\\' && !escape {
                    escape = true;
                } else {
                    escape = false;
                }
            } else {
                match c {
                    b'"' => string = true,
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        if depth == 0 {
                            return Err(Error::domain("INVALID_REQUEST", "Unbalanced journal"));
                        }
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            if depth == 0 && !string && !primitive {
                break;
            }
        }
        if self.offset == start || depth != 0 || string {
            return Err(Error::domain("INVALID_REQUEST", "Invalid journal value"));
        }
        Ok((start, self.offset - start))
    }
    fn separator(&mut self) -> Result<bool> {
        self.ws()?;
        match self.byte()? {
            b',' => Ok(true),
            b'}' => Ok(false),
            _ => Err(Error::domain(
                "INVALID_REQUEST",
                "Invalid journal separator",
            )),
        }
    }
    fn map<T: Snapshot>(&mut self, file: &Arc<File>) -> Result<Snapshots<T>> {
        self.expect(b'{')?;
        self.ws()?;
        let mut entries = BTreeMap::new();
        if self.peek()? == Some(b'}') {
            self.byte()?;
            return Ok(Snapshots {
                entries,
                parking: None,
            });
        }
        loop {
            let id = self.key()?;
            let (start, bytes) = self.span()?;
            let range = Range {
                file: file.clone(),
                start,
                bytes,
            };
            let value: T = serde_json::from_reader(BufReader::new(range.reader()))?;
            let summary = value.summary();
            if summary.id != id || entries.contains_key(&id) {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Invalid snapshot identity",
                ));
            }
            entries.insert(
                id,
                Entry {
                    summary,
                    value: None,
                    location: Arc::new(RwLock::new(Some(range))),
                },
            );
            if !self.separator()? {
                break;
            }
        }
        Ok(Snapshots {
            entries,
            parking: None,
        })
    }
}

pub(crate) fn open(path: &Path) -> Result<StoredWorkspace> {
    let file = Arc::new(File::open(path)?);
    let stamp = crate::workspace_memory::Stamp::of(&file)?;
    if let Ok(index_file) = File::open(path.with_file_name("workspace.index.json")) {
        if let Ok(index) = serde_json::from_reader::<_, Index>(BufReader::new(index_file)) {
            if index.version == 1 && index.stamp == stamp {
                let mut stored = index.metadata;
                stored.revisions = restore(index.revisions, &file);
                stored.drafts = restore(index.drafts, &file);
                stored.job_snapshots = restore(index.job_snapshots, &file);
                return Ok(stored);
            }
        }
    }
    let mut scan = Scanner {
        reader: BufReader::new(file.try_clone()?),
        offset: 0,
    };
    scan.expect(b'{')?;
    let mut seen = HashSet::new();
    let mut fields = serde_json::Map::new();
    let mut revisions = Snapshots::default();
    let mut drafts = Snapshots::default();
    let mut job_snapshots = Snapshots::default();
    loop {
        let key = scan.key()?;
        if !seen.insert(key.clone()) {
            return Err(Error::domain("INVALID_REQUEST", "Duplicate journal field"));
        }
        match key.as_str() {
            "revisions" => revisions = scan.map(&file)?,
            "drafts" => drafts = scan.map(&file)?,
            "job_snapshots" => job_snapshots = scan.map(&file)?,
            _ => {
                let (start, bytes) = scan.span()?;
                let value = serde_json::from_reader(BufReader::new(
                    Range {
                        file: file.clone(),
                        start,
                        bytes,
                    }
                    .reader(),
                ))?;
                fields.insert(key, value);
            }
        }
        if !scan.separator()? {
            break;
        }
    }
    scan.ws()?;
    if scan.peek()?.is_some() {
        return Err(Error::domain("INVALID_REQUEST", "Trailing journal data"));
    }
    fields.insert("revisions".into(), serde_json::json!({}));
    fields.insert("drafts".into(), serde_json::json!({}));
    fields.insert("job_snapshots".into(), serde_json::json!({}));
    let mut stored: StoredWorkspace = serde_json::from_value(Value::Object(fields))?;
    stored.revisions = revisions;
    stored.drafts = drafts;
    stored.job_snapshots = job_snapshots;
    if crate::workspace_memory::Stamp::read(path)? != stamp {
        return Err(Error::domain(
            "IO_ERROR",
            "Workspace changed while indexing",
        ));
    }
    let _ = write_index(path.parent().unwrap(), &stored, &file);
    Ok(stored)
}

struct Counter<W> {
    inner: W,
    offset: u64,
}
impl<W: Write> Write for Counter<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(b)?;
        self.offset += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
type Locations<T> = Vec<(
    Arc<RwLock<Option<Range>>>,
    u64,
    u64,
    Summary,
    PhantomData<T>,
)>;
fn write_map<T: Snapshot, W: Write>(
    map: &Snapshots<T>,
    writer: &mut Counter<W>,
) -> Result<Locations<T>> {
    writer.write_all(b"{")?;
    let mut locations = Vec::new();
    for (n, (id, entry)) in map.entries.iter().enumerate() {
        if n > 0 {
            writer.write_all(b",")?;
        }
        serde_json::to_writer(&mut *writer, id)?;
        writer.write_all(b":")?;
        let start = writer.offset;
        // Snapshot edits clear their range before any resident value changes.
        let range = entry.location.read().unwrap().clone();
        if let Some(range) = range {
            let n = io::copy(&mut range.reader(), writer)?;
            if n != range.bytes {
                return Err(Error::domain("IO_ERROR", "Truncated snapshot bytes"));
            }
        } else {
            let value = entry
                .value
                .as_ref()
                .ok_or_else(|| Error::domain("IO_ERROR", "Missing snapshot bytes"))?;
            serde_json::to_writer(&mut *writer, value.as_ref())?;
        }
        locations.push((
            entry.location.clone(),
            start,
            writer.offset - start,
            entry.summary.clone(),
            PhantomData,
        ));
    }
    writer.write_all(b"}")?;
    Ok(locations)
}
pub(crate) struct Publication {
    file: Arc<File>,
    revisions: Locations<Revision>,
    drafts: Locations<Draft>,
    job_snapshots: Locations<Draft>,
}
pub(crate) fn write(path: &Path, stored: &StoredWorkspace) -> Result<Publication> {
    let file = File::create(path)?;
    let mut writer = Counter {
        inner: io::BufWriter::with_capacity(128 * 1024, file),
        offset: 0,
    };
    let Value::Object(mut fields) = serde_json::to_value(metadata(stored))? else {
        unreachable!()
    };
    fields.remove("revisions");
    fields.remove("drafts");
    fields.remove("job_snapshots");
    writer.write_all(b"{")?;
    for (key, value) in fields {
        serde_json::to_writer(&mut writer, &key)?;
        writer.write_all(b":")?;
        serde_json::to_writer(&mut writer, &value)?;
        writer.write_all(b",")?;
    }
    writer.write_all(b"\"revisions\":")?;
    let revisions = write_map(&stored.revisions, &mut writer)?;
    writer.write_all(b",\"drafts\":")?;
    let drafts = write_map(&stored.drafts, &mut writer)?;
    writer.write_all(b",\"job_snapshots\":")?;
    let job_snapshots = write_map(&stored.job_snapshots, &mut writer)?;
    writer.write_all(b"}")?;
    writer.flush()?;
    writer.inner.get_ref().sync_all()?;
    Ok(Publication {
        file: Arc::new(File::open(path)?),
        revisions,
        drafts,
        job_snapshots,
    })
}
impl Publication {
    pub(crate) fn commit(
        self,
        directory: &Path,
        stored: &StoredWorkspace,
    ) -> Result<crate::workspace_memory::Stamp> {
        fn bind<T: Snapshot>(locations: Locations<T>, map: &mut Snapshots<T>, file: &Arc<File>) {
            for (slot, start, bytes, summary, _) in locations {
                *slot.write().unwrap() = Some(Range {
                    file: file.clone(),
                    start,
                    bytes,
                });
                if let Some(entry) = map.entries.get_mut(&summary.id) {
                    entry.summary = summary;
                }
            }
        }
        let mut indexed_stored = stored.clone();
        bind(self.revisions, &mut indexed_stored.revisions, &self.file);
        bind(self.drafts, &mut indexed_stored.drafts, &self.file);
        bind(
            self.job_snapshots,
            &mut indexed_stored.job_snapshots,
            &self.file,
        );
        let stamp = crate::workspace_memory::Stamp::of(&self.file)?;
        let _ = write_index(directory, &indexed_stored, &self.file);
        Ok(stamp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::WorkerConfig;
    #[tokio::test]
    async fn history_is_metadata_only_and_historical_reads_do_not_expand_the_current_cache() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let config = WorkerConfig::bundled("unused".into());
        let core = crate::Core::open(root.clone(), config.clone())
            .await
            .unwrap();
        let id = core.create_workspace("test").await.unwrap();
        let mut ids = Vec::new();
        {
            let mut all = core.workspace(&id).await.unwrap();
            let ws = all.get_mut(&id).unwrap();
            let mut revision = ws.stored.current_revision().clone();
            for i in 0..8 {
                revision.parent = Some(ws.stored.current.clone());
                revision.id = crate::id();
                revision.inputs.files.insert(
                    "model.py".into(),
                    format!("# snapshot {i}\n{}", "# body\n".repeat(25_000)),
                );
                ws.stored.current = revision.id.clone();
                ids.push(revision.id.clone());
                ws.stored
                    .revisions
                    .insert(revision.id.clone(), revision.clone());
            }
            core.persist(&ws.stored).await.unwrap();
        }
        core.worker_cache.trim().await;
        {
            let all = core.workspaces.lock().await;
            assert_eq!(
                all[&id]
                    .stored
                    .revisions
                    .entries
                    .values()
                    .filter(|e| e.value.is_some())
                    .count(),
                1,
                "commits must release the previous current payload"
            );
        }
        drop(core);
        let core = crate::Core::open(root.clone(), config).await.unwrap();
        let rows = core.model_history(&id).await.unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 9);
        {
            let all = core.workspaces.lock().await;
            let map = &all[&id].stored.revisions;
            assert!(
                map.entries.values().all(|e| e.value.is_none()),
                "history list must not decode bodies"
            );
        }
        let source = core.read_source(&id, &ids[0], "model.py").await.unwrap();
        assert!(
            source["source"]
                .as_str()
                .unwrap()
                .starts_with("# snapshot 0")
        );
        {
            let all = core.workspaces.lock().await;
            assert!(
                all[&id]
                    .stored
                    .revisions
                    .entries
                    .values()
                    .all(|e| e.value.is_none())
            );
        }
        let all = core.workspace(&id).await.unwrap();
        let map = &all[&id].stored.revisions;
        assert_eq!(
            map.entries.values().filter(|e| e.value.is_some()).count(),
            1
        );
        assert!(map.resident(ids.last().unwrap()).is_some());
    }
    #[tokio::test]
    async fn saving_streams_unchanged_bodies_and_old_readers_survive_atomic_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let core = crate::Core::open(root.clone(), WorkerConfig::bundled("unused".into()))
            .await
            .unwrap();
        let id = core.create_workspace("test").await.unwrap();
        let path = root.join("workspaces").join(&id).join("workspace.json");
        let before = open(&path).unwrap();
        let rev = before.current.clone();
        let draft = core
            .update_source(
                &id,
                &rev,
                None,
                BTreeMap::from([("model.py".into(), "# edited".into())]),
                "test",
            )
            .await
            .unwrap();
        let after = open(&path).unwrap();
        assert!(after.drafts.contains_key(&draft.id));
        assert_eq!(
            before.revisions.read(&rev).unwrap().inputs.files["model.py"],
            Inputs::default().files["model.py"]
        );
        assert!(after.revisions.entries.values().all(|e| e.value.is_none()));
        assert!(after.drafts.entries.values().all(|e| e.value.is_none()));
        // A bad derived index must fall back to the authoritative journal.
        std::fs::write(path.with_file_name("workspace.index.json"), b"broken index").unwrap();
        let rebuilt = open(&path).unwrap();
        assert_eq!(
            rebuilt.drafts.read(&draft.id).unwrap().inputs.files["model.py"],
            "# edited"
        );
        assert!(
            rebuilt
                .revisions
                .entries
                .values()
                .all(|e| e.value.is_none())
        );
    }
}
