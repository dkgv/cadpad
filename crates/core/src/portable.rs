//! Portable source projects. Root files are editable; revisions are immutable
//! snapshots. Credentials, UI preferences, workers and derived meshes stay out.
use crate::{Error, Inputs, Result, Revision, id, validate_path};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const SOURCE_FILE_LIMIT: usize = 4096;
pub const PROJECT_BROWSER_SOURCE: &str = "# Cadpad project browser. Select a part in Explorer.\n";
pub const GENERATED_ENTRY_HEADER: &str = "# Generated Cadpad entry point.";
/// Folder projects keep generated entry points under `.cadpad/entries/` so
/// they do not clutter the user's source tree. Revisions still key them by
/// their plain file name, so builds, fingerprints and history are unchanged.
const ENTRIES_DIRECTORY: &str = "entries";

/// Entry points Cadpad wrote itself: build adapters and the project browser.
pub fn is_generated_entry(source: &str) -> bool {
    source.starts_with(GENERATED_ENTRY_HEADER) || source == PROJECT_BROWSER_SOURCE
}
pub const SOURCE_LIMIT: usize = 1024 * 1024;
pub const ASSET_LIMIT: usize = 64 * 1024 * 1024;
const TOTAL_LIMIT: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub units: String,
    pub entry_point: String,
    pub dependencies: BTreeMap<String, String>,
    pub source_files: Vec<String>,
    pub assets: Vec<String>,
    pub saved_revision: String,
    #[serde(default)]
    pub retained_sources: BTreeMap<String, String>,
    #[serde(default)]
    pub retained_assets: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recordings: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_tests: Option<String>,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Binding {
    pub directory: PathBuf,
    pub fingerprint: String,
    pub saved_revision: String,
    #[serde(default)]
    pub recordings: Option<String>,
    #[serde(default)]
    pub recording_fingerprint: Option<String>,
    #[serde(default)]
    pub motion_tests: Option<String>,
    #[serde(default)]
    pub motion_test_fingerprint: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Loaded {
    pub inputs: Inputs,
    pub blobs: BTreeMap<String, Vec<u8>>,
    pub fingerprint: String,
    pub manifest: Manifest,
}
#[derive(Serialize, Deserialize)]
struct Snapshot {
    schema_version: u32,
    id: String,
    parent: Option<String>,
    actor: String,
    input_fingerprint: String,
    #[serde(default)]
    restored_from: Option<String>,
}
pub fn bytes_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// STL paths may live under `assets/`, which ordinary source paths reject.
pub fn mesh_path(path: &str) -> Result<()> {
    let portable = !path.is_empty()
        && path.len() <= 240
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path.to_ascii_lowercase().ends_with(".stl")
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && !segment.starts_with('.')
                && !segment.eq_ignore_ascii_case("revisions")
                && !segment.eq_ignore_ascii_case("node_modules")
                && !segment.eq_ignore_ascii_case("__pycache__")
                && !segment.eq_ignore_ascii_case("venv")
        });
    if portable {
        Ok(())
    } else {
        Err(Error::domain(
            "INVALID_REQUEST",
            "Choose an STL file inside this project",
        ))
    }
}
pub fn relative(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 240
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
        || path.split('/').any(|s| {
            s.is_empty()
                || s.starts_with('.')
                || s.ends_with(['.', ' '])
                || s.eq_ignore_ascii_case("revisions")
                || s.eq_ignore_ascii_case("assets")
                || [
                    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6",
                    "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7",
                    "LPT8", "LPT9",
                ]
                .contains(&s.split('.').next().unwrap_or("").to_uppercase().as_str())
        })
    {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Project paths must be portable relative paths without hidden, reserved or parent components",
        ));
    }
    Ok(())
}
pub fn fingerprint(inputs: &Inputs) -> Result<String> {
    Ok(bytes_hash(&serde_json::to_vec(inputs)?))
}
pub(crate) fn checked_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(Error::domain("INVALID_REQUEST", "Invalid project path"));
        }
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Project symlinks are not followed",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}
fn open_regular_project_file(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Expected a regular project file",
        ));
    }
    Ok(file)
}
pub fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_regular_project_file(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    check_file_size(path, bytes.len(), limit)?;
    Ok(bytes)
}
fn check_file_size(path: &Path, bytes: usize, limit: usize) -> Result<()> {
    if bytes > limit {
        return Err(Error::domain(
            "INVALID_REQUEST",
            format!("{} exceeds the {} byte limit", path.display(), limit),
        ));
    }
    Ok(())
}
fn hash_bounded(path: &Path, limit: usize) -> Result<(String, usize)> {
    let mut reader = open_regular_project_file(path)?.take(limit as u64 + 1);
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut size = 0;
    loop {
        let len = reader.read(&mut buffer)?;
        if len == 0 {
            break;
        }
        size += len;
        hasher.update(&buffer[..len]);
    }
    check_file_size(path, size, limit)?;
    Ok((format!("{:x}", hasher.finalize()), size))
}
fn json_file<T: for<'de> Deserialize<'de>>(root: &Path, path: &str) -> Result<T> {
    Ok(serde_json::from_slice(&read_bounded(
        &checked_path(root, path)?,
        SOURCE_LIMIT,
    )?)?)
}
fn scan_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<String>,
    sources_only: bool,
    excluded: Option<&Path>,
) -> Result<()> {
    if directory.strip_prefix(root).unwrap().components().count() > 16 {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Project directories exceed 16 nested levels",
        ));
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.')
            || matches!(
                name.as_ref(),
                "assets" | "revisions" | "node_modules" | "__pycache__" | "venv"
            )
        {
            continue;
        }
        if excluded.is_some_and(|path| entry.path() == path) {
            continue;
        }
        let kind = entry.file_type()?;
        // Unrelated links must not prevent loading regular project files.
        // Explicit reads and writes still reject links through checked_path.
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            scan_files(root, &entry.path(), files, sources_only, excluded)?;
        } else if kind.is_file() && (!sources_only || name.ends_with(".py")) {
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if sources_only {
                validate_path(&relative)?;
            } else {
                self::relative(&relative)?;
            }
            files.push(relative);
            let limit = if sources_only { SOURCE_FILE_LIMIT } else { 256 };
            if files.len() > limit {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    if sources_only {
                        "Project exceeds 4096 Python source files"
                    } else {
                        "Project exceeds 256 managed assets"
                    },
                ));
            }
        }
    }
    Ok(())
}
/// STL files already in the project folder, including `assets/`. Symlinks are skipped.
pub fn list_mesh_files(directory: &Path) -> Result<Vec<String>> {
    let root = directory.canonicalize()?;
    let mut files = Vec::new();
    walk_model_files(
        &root,
        &root,
        &mut files,
        |path| mesh_path(path).is_ok(),
        256,
    )?;
    files.sort();
    Ok(files)
}
pub(crate) fn model_file_format(path: &str) -> Option<&'static str> {
    relative(path.strip_prefix("assets/").unwrap_or(path)).ok()?;
    match Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "step" | "stp" => Some("step"),
        "stl" => Some("stl"),
        _ => None,
    }
}
pub(crate) fn list_model_files(directory: &Path) -> Result<Vec<String>> {
    let root = directory.canonicalize()?;
    let mut files = Vec::new();
    walk_model_files(
        &root,
        &root,
        &mut files,
        |path| model_file_format(path).is_some(),
        usize::MAX,
    )?;
    files.sort();
    Ok(files)
}
fn walk_model_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<String>,
    accepts: fn(&str) -> bool,
    limit: usize,
) -> Result<()> {
    if files.len() >= limit
        || directory
            .strip_prefix(root)
            .unwrap_or(directory)
            .components()
            .count()
            > 16
    {
        return Ok(());
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        if files.len() >= limit {
            break;
        }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.')
            || matches!(
                name.as_ref(),
                "revisions" | "node_modules" | "__pycache__" | "venv"
            )
        {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk_model_files(root, &entry.path(), files, accepts, limit)?;
        } else if kind.is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path().as_path())
                .to_string_lossy()
                .replace('\\', "/");
            if accepts(&relative) {
                files.push(relative);
            }
        }
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq)]
pub struct StlSummary {
    pub format: &'static str,
    pub triangles: u64,
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub size: [f64; 3],
}
fn stl_summary(format: &'static str, triangles: u64, min: [f64; 3], max: [f64; 3]) -> StlSummary {
    StlSummary {
        format,
        triangles,
        min,
        max,
        size: [max[0] - min[0], max[1] - min[1], max[2] - min[2]],
    }
}
/// Bounds and triangle count only. The mesh is not stored or imported.
pub fn summarize_stl(bytes: &[u8]) -> Result<StlSummary> {
    visit_stl(bytes, |_| Ok(()))
}

pub(crate) fn visit_stl(
    bytes: &[u8],
    mut vertex: impl FnMut([f64; 3]) -> Result<()>,
) -> Result<StlSummary> {
    if bytes.len() >= 84 {
        let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as u64;
        let binary_len = 84u64.saturating_add(count.saturating_mul(50));
        if count > 0 && binary_len == bytes.len() as u64 {
            return summarize_binary_stl(bytes, count, vertex);
        }
    }
    let text = String::from_utf8_lossy(bytes);
    if text.to_ascii_lowercase().contains("facet") {
        return summarize_ascii_stl(&text, &mut vertex);
    }
    Err(Error::domain(
        "INVALID_GEOMETRY",
        "STL is neither a valid binary nor ASCII mesh",
    ))
}
fn summarize_binary_stl(
    bytes: &[u8],
    count: u64,
    mut emit: impl FnMut([f64; 3]) -> Result<()>,
) -> Result<StlSummary> {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for index in 0..count {
        let start = 84 + index as usize * 50;
        let record = &bytes[start..start + 50];
        for vertex in 0..3 {
            let offset = 12 + vertex * 12;
            let point = [
                f32::from_le_bytes(record[offset..offset + 4].try_into().unwrap()) as f64,
                f32::from_le_bytes(record[offset + 4..offset + 8].try_into().unwrap()) as f64,
                f32::from_le_bytes(record[offset + 8..offset + 12].try_into().unwrap()) as f64,
            ];
            if point.iter().any(|value| !value.is_finite()) {
                return Err(Error::domain(
                    "INVALID_GEOMETRY",
                    "STL coordinates are not finite",
                ));
            }
            emit(point)?;
            for axis in 0..3 {
                min[axis] = min[axis].min(point[axis]);
                max[axis] = max[axis].max(point[axis]);
            }
        }
    }
    Ok(stl_summary("binary", count, min, max))
}
fn summarize_ascii_stl(
    text: &str,
    mut emit: impl FnMut([f64; 3]) -> Result<()>,
) -> Result<StlSummary> {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    let mut vertices = 0u64;
    for line in text.lines() {
        let line = line.trim();
        if !line.to_ascii_lowercase().starts_with("vertex") {
            continue;
        }
        let mut values = line.split_whitespace().skip(1);
        let point = [
            values.next().and_then(|value| value.parse::<f64>().ok()),
            values.next().and_then(|value| value.parse::<f64>().ok()),
            values.next().and_then(|value| value.parse::<f64>().ok()),
        ];
        let Some(point) = point[0]
            .zip(point[1])
            .zip(point[2])
            .map(|((x, y), z)| [x, y, z])
        else {
            return Err(Error::domain(
                "INVALID_GEOMETRY",
                "ASCII STL vertex is malformed",
            ));
        };
        if point.iter().any(|value| !value.is_finite()) {
            return Err(Error::domain(
                "INVALID_GEOMETRY",
                "STL coordinates are not finite",
            ));
        }
        emit(point)?;
        vertices += 1;
        for axis in 0..3 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }
    if vertices == 0 || vertices % 3 != 0 {
        return Err(Error::domain(
            "INVALID_GEOMETRY",
            "ASCII STL has no usable triangles",
        ));
    }
    Ok(stl_summary("ascii", vertices / 3, min, max))
}
/// Configuration lives beside source, without imposing a project directory suffix.
pub fn metadata_directory(directory: &Path) -> Result<PathBuf> {
    let config = checked_path(directory, ".cadpad")?;
    if config.exists() {
        Ok(config)
    } else {
        Ok(directory.to_path_buf())
    }
}
pub fn python_entry_points(directory: &Path) -> Result<Vec<String>> {
    let root = directory.canonicalize()?;
    let mut paths = Vec::new();
    scan_files(&root, &root, &mut paths, true, None)?;
    paths.sort();
    Ok(paths)
}
/// A generated entry name that collides with no project source file:
/// `cadpad_entry.py`, `cadpad_entry_1.py`, … or `cadpad_view_0.py`, ….
pub(crate) fn unused_entry_name(root: &Path, prefix: &str) -> Result<String> {
    let taken = python_entry_points(root)?;
    let entries = checked_path(root, ".cadpad")?.join(ENTRIES_DIRECTORY);
    let bare = prefix == "cadpad_entry";
    (0..SOURCE_FILE_LIMIT)
        .map(|index| {
            if bare && index == 0 {
                format!("{prefix}.py")
            } else {
                format!("{prefix}_{index}.py")
            }
        })
        .find(|name| !taken.contains(name) && !entries.join(name).exists())
        .ok_or_else(|| Error::domain("INVALID_REQUEST", "Project has too many source files"))
}

/// Add only Cadpad configuration; existing source and unrelated files stay in place.
pub fn initialize_project_browser(directory: &Path) -> Result<()> {
    let root = directory.canonicalize()?;
    let name = unused_entry_name(&root, "cadpad_view")?;
    initialize_folder_with(&root, &name, Some(PROJECT_BROWSER_SOURCE))
}

pub fn initialize_folder(directory: &Path, entry: &str) -> Result<()> {
    initialize_folder_with(&directory.canonicalize()?, entry, None)
}

/// Creates `.cadpad`. A `generated` entry source is stored under
/// `.cadpad/entries/` instead of the project root.
pub(crate) fn initialize_folder_with(
    root: &Path,
    entry: &str,
    generated: Option<&str>,
) -> Result<()> {
    let root = root.canonicalize()?;
    validate_path(entry)?;
    let mut paths = python_entry_points(&root)?;
    match generated {
        Some(source) if is_generated_entry(source) && !paths.contains(&entry.to_string()) => {
            paths.push(entry.into());
            paths.sort();
        }
        Some(_) => {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Generated entry points need an unused name and generated content",
            ));
        }
        None if !paths.contains(&entry.to_string()) => {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Choose a Python entry file inside the project folder",
            ));
        }
        None => {}
    }
    let config = checked_path(&root, ".cadpad")?;
    if config.exists() {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "This folder already contains .cadpad configuration",
        ));
    }
    let staging = root.join(format!(".cadpad-{}.tmp", id()));
    let result = (|| -> Result<()> {
        fs::create_dir(&staging)?;
        if let Some(source) = generated {
            atomic_write(
                &staging,
                &format!("{ENTRIES_DIRECTORY}/{entry}"),
                source.as_bytes(),
            )?;
        }
        let defaults = Inputs::default();
        for (name, value) in [
            ("parameters.json", &defaults.parameters),
            ("annotations.json", &defaults.annotations),
            ("mechanism.json", &defaults.mechanism),
        ] {
            atomic_write(&staging, name, &serde_json::to_vec_pretty(value)?)?;
        }
        let manifest = Manifest {
            schema_version: 3,
            units: "mm".into(),
            entry_point: entry.into(),
            dependencies: BTreeMap::new(),
            source_files: paths,
            assets: vec![],
            saved_revision: String::new(),
            retained_sources: BTreeMap::new(),
            retained_assets: BTreeMap::new(),
            recordings: None,
            motion_tests: None,
        };
        atomic_write(
            &staging,
            "project.json",
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        fs::rename(&staging, &config)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}
pub fn read_manifest(directory: &Path) -> Result<Manifest> {
    let root = directory.canonicalize()?;
    let metadata = metadata_directory(&root)?;
    let manifest: Manifest = json_file(&metadata, "project.json")?;
    if !matches!(manifest.schema_version, 1 | 2 | 3) || manifest.units != "mm" {
        return Err(Error::domain(
            "UNSUPPORTED_OPERATION",
            "Project requires schema 1, 2 or 3 and millimetre CAD units",
        ));
    }
    crate::motion_tests::load(&metadata, manifest.motion_tests.as_deref())?;
    Ok(manifest)
}
pub fn read_project(directory: &Path) -> Result<Loaded> {
    read_project_with_assets(directory, true)
}

/// Compare disk inputs without retaining all managed asset bodies at once.
pub(crate) fn read_project_metadata(directory: &Path) -> Result<Loaded> {
    read_project_with_assets(directory, false)
}

fn read_project_with_assets(directory: &Path, retain_blobs: bool) -> Result<Loaded> {
    let root = directory.canonicalize()?;
    let metadata = metadata_directory(&root)?;
    let manifest: Manifest = json_file(&metadata, "project.json")?;
    crate::motion_tests::load(&metadata, manifest.motion_tests.as_deref())?;
    if !matches!(manifest.schema_version, 1 | 2 | 3) || manifest.units != "mm" {
        return Err(Error::domain(
            "UNSUPPORTED_OPERATION",
            "Project requires schema 1, 2 or 3 and millimetre CAD units",
        ));
    }
    validate_path(&manifest.entry_point)?;
    if !manifest.saved_revision.is_empty()
        && uuid::Uuid::parse_str(&manifest.saved_revision).is_err()
    {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Invalid saved revision ID",
        ));
    }
    if manifest.source_files.len() > SOURCE_FILE_LIMIT || manifest.assets.len() > 256 {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Project manifest has too many files",
        ));
    }
    let mut paths = Vec::new();
    scan_files(&root, &root, &mut paths, true, None)?;
    for path in &manifest.source_files {
        validate_path(path)?;
    }
    // Newly created Python modules are candidates too. Deleted files are reported
    // by fingerprint comparison rather than being silently restored from a cache.
    let mut inputs = Inputs {
        entry_point: manifest.entry_point.clone(),
        dependencies: manifest.dependencies.clone(),
        parameters: json_file(&metadata, "parameters.json")?,
        annotations: json_file(&metadata, "annotations.json")?,
        mechanism: json_file(&metadata, "mechanism.json")?,
        ..Inputs::default()
    };
    inputs.files.clear();
    let mut total = 0;
    for path in paths {
        let bytes = read_bounded(&checked_path(&root, &path)?, SOURCE_LIMIT)?;
        if !manifest.source_files.contains(&path)
            && manifest.retained_sources.get(&path) == Some(&bytes_hash(&bytes))
        {
            continue;
        }
        total += bytes.len();
        let source = String::from_utf8(bytes)
            .map_err(|_| Error::domain("INVALID_REQUEST", "Python source must be UTF-8"))?;
        inputs.files.insert(path, source);
    }
    // Root copies of generated entries predate `.cadpad/entries/`; the
    // relocated copy wins and the next save removes the root one.
    if metadata != root {
        for (path, source) in read_generated_entries(&metadata)? {
            total += source.len();
            inputs.files.insert(path, source);
        }
    }
    let mut blobs = BTreeMap::new();
    let mut asset_paths = Vec::new();
    let assets_root = checked_path(&root, "assets")?;
    if assets_root.exists() {
        scan_files(&assets_root, &assets_root, &mut asset_paths, false, None)?;
    }
    for asset in &manifest.assets {
        relative(asset)?;
    }
    for (path, hash) in manifest
        .retained_sources
        .iter()
        .chain(manifest.retained_assets.iter())
    {
        relative(path)?;
        if !valid_hash(hash) {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Invalid retained file digest",
            ));
        }
    }
    for asset in &asset_paths {
        relative(asset)?;
        if inputs.assets.contains_key(asset) {
            return Err(Error::domain("INVALID_REQUEST", "Duplicate managed asset"));
        }
        let path = checked_path(&root, &format!("assets/{asset}"))?;
        let (hash, size, bytes) = if retain_blobs {
            let bytes = read_bounded(&path, ASSET_LIMIT)?;
            (bytes_hash(&bytes), bytes.len(), Some(bytes))
        } else {
            let (hash, size) = hash_bounded(&path, ASSET_LIMIT)?;
            (hash, size, None)
        };
        if !manifest.assets.contains(asset) && manifest.retained_assets.get(asset) == Some(&hash) {
            continue;
        }
        total += size;
        if total > TOTAL_LIMIT {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Project exceeds 256 MiB input limit",
            ));
        }
        if let Some(bytes) = bytes {
            blobs.insert(hash.clone(), bytes);
        }
        inputs.assets.insert(asset.clone(), hash);
    }
    validate_inputs(&inputs)?;
    Ok(Loaded {
        fingerprint: fingerprint(&inputs)?,
        inputs,
        blobs,
        manifest,
    })
}
fn read_generated_entries(metadata: &Path) -> Result<BTreeMap<String, String>> {
    let directory = checked_path(metadata, ENTRIES_DIRECTORY)?;
    let mut entries = BTreeMap::new();
    if !directory.is_dir() {
        return Ok(entries);
    }
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_file() || !name.ends_with(".py") || name.starts_with('.') {
            continue;
        }
        validate_path(&name)?;
        let bytes = read_bounded(&checked_path(&directory, &name)?, SOURCE_LIMIT)?;
        let source = String::from_utf8(bytes)
            .map_err(|_| Error::domain("INVALID_REQUEST", "Python source must be UTF-8"))?;
        // Anything else in this directory was not written by Cadpad.
        if is_generated_entry(&source) {
            entries.insert(name, source);
        }
    }
    Ok(entries)
}

/// Writes generated root-level entries under `.cadpad/entries/`, removes
/// relocated or stale ones, and deletes root copies only while they still hold
/// generated content.
fn write_generated_entries(root: &Path, metadata: &Path, inputs: &Inputs) -> Result<()> {
    let generated = inputs
        .files
        .iter()
        .filter(|(path, source)| !path.contains('/') && is_generated_entry(source))
        .collect::<BTreeMap<_, _>>();
    for (path, source) in &generated {
        atomic_write(
            metadata,
            &format!("{ENTRIES_DIRECTORY}/{path}"),
            source.as_bytes(),
        )?;
        let legacy = checked_path(root, path)?;
        if legacy.is_file()
            && String::from_utf8(read_bounded(&legacy, SOURCE_LIMIT)?)
                .is_ok_and(|text| is_generated_entry(&text))
        {
            fs::remove_file(legacy)?;
        }
    }
    for (name, _) in read_generated_entries(metadata)? {
        if !generated.contains_key(&name) {
            fs::remove_file(checked_path(
                metadata,
                &format!("{ENTRIES_DIRECTORY}/{name}"),
            )?)?;
        }
    }
    Ok(())
}

pub fn validate_inputs(inputs: &Inputs) -> Result<()> {
    crate::annotations::decode(&inputs.annotations)?;
    crate::kinematics::decode(&inputs.mechanism)?;
    validate_path(&inputs.entry_point)?;
    if !inputs.files.contains_key(&inputs.entry_point) || inputs.files.len() > SOURCE_FILE_LIMIT {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Project needs a present entry point and at most 4096 source files",
        ));
    }
    let mut total = 0;
    for (path, source) in &inputs.files {
        validate_path(path)?;
        if source.len() > SOURCE_LIMIT {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Source file exceeds 1 MiB",
            ));
        }
        total += source.len();
    }
    if total > 8 * SOURCE_LIMIT {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Source files exceed 8 MiB",
        ));
    }
    if !inputs.parameters.is_object()
        || !inputs.annotations.is_array()
        || !inputs.mechanism.is_object()
    {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Parameters and mechanism must be objects; annotations must be an array",
        ));
    }
    for data in [&inputs.parameters, &inputs.annotations, &inputs.mechanism] {
        if serde_json::to_vec(data)?.len() > SOURCE_LIMIT {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Project metadata exceeds 1 MiB",
            ));
        }
    }
    if inputs.assets.len() > 256 {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Project exceeds 256 managed assets",
        ));
    }
    for paths in [
        inputs.files.keys().collect::<Vec<_>>(),
        inputs.assets.keys().collect::<Vec<_>>(),
    ] {
        let mut normalized = std::collections::HashSet::new();
        for path in paths {
            if !normalized.insert(path.to_lowercase()) {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Project paths collide on a case-insensitive filesystem",
                ));
            }
        }
    }
    for (asset, hash) in &inputs.assets {
        relative(asset)?;
        if !valid_hash(hash) {
            return Err(Error::domain("INVALID_REQUEST", "Invalid asset digest"));
        }
    }
    Ok(())
}
pub(crate) fn atomic_write(root: &Path, path: &str, bytes: &[u8]) -> Result<()> {
    atomic_write_inner(root, path, bytes, true)
}

/// Regenerable display data only. Readers validate checksums and can repair
/// lost buffers after a crash, so these writes need no forced disk flush.
pub(crate) fn atomic_write_display_buffer(root: &Path, path: &str, bytes: &[u8]) -> Result<()> {
    atomic_write_inner(root, path, bytes, false)
}

fn atomic_write_inner(root: &Path, path: &str, bytes: &[u8], durable: bool) -> Result<()> {
    let target = checked_path(root, path)?;
    fs::create_dir_all(target.parent().unwrap())?;
    let temporary = target
        .parent()
        .unwrap()
        .join(format!(".cadpad-{}.tmp", id()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        if durable {
            file.sync_all()?;
        }
        drop(file);
        // Validate again before replacing a user-editable path.
        checked_path(root, path)?;
        fs::rename(&temporary, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn write_inputs(
    root: &Path,
    inputs: &Inputs,
    revision: &str,
    blobs: &Path,
    recordings: Option<String>,
    motion_tests: Option<String>,
    folder: bool,
) -> Result<()> {
    validate_inputs(inputs)?;
    fs::create_dir_all(root)?;
    let mut retained_sources = BTreeMap::new();
    let mut existing = Vec::new();
    scan_files(root, root, &mut existing, true, None)?;
    for path in existing {
        if !inputs.files.contains_key(&path) {
            retained_sources.insert(
                path.clone(),
                bytes_hash(&read_bounded(&checked_path(root, &path)?, SOURCE_LIMIT)?),
            );
        }
    }
    let mut retained_assets = BTreeMap::new();
    let assets_root = checked_path(root, "assets")?;
    if assets_root.exists() {
        let mut existing = Vec::new();
        scan_files(&assets_root, &assets_root, &mut existing, false, None)?;
        for path in existing {
            if !inputs.assets.contains_key(&path) {
                retained_assets.insert(
                    path.clone(),
                    bytes_hash(&read_bounded(
                        &checked_path(&assets_root, &path)?,
                        ASSET_LIMIT,
                    )?),
                );
            }
        }
    }
    let metadata = if folder {
        checked_path(root, ".cadpad")?
    } else {
        root.to_path_buf()
    };
    fs::create_dir_all(&metadata)?;
    for (path, source) in &inputs.files {
        if folder && !path.contains('/') && is_generated_entry(source) {
            continue;
        }
        atomic_write(root, path, source.as_bytes())?;
    }
    if folder {
        write_generated_entries(root, &metadata, inputs)?;
    }
    for (name, hash) in &inputs.assets {
        let bytes = read_bounded(&blobs.join(hash), ASSET_LIMIT)?;
        if bytes_hash(&bytes) != *hash {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Managed asset digest mismatch",
            ));
        }
        atomic_write(root, &format!("assets/{name}"), &bytes)?;
    }
    for (name, value) in [
        ("parameters.json", &inputs.parameters),
        ("annotations.json", &inputs.annotations),
        ("mechanism.json", &inputs.mechanism),
    ] {
        atomic_write(&metadata, name, &serde_json::to_vec_pretty(value)?)?;
    }
    let manifest = Manifest {
        schema_version: if folder {
            3
        } else if recordings.is_some() || motion_tests.is_some() {
            2
        } else {
            1
        },
        units: "mm".into(),
        entry_point: inputs.entry_point.clone(),
        dependencies: inputs.dependencies.clone(),
        source_files: inputs.files.keys().cloned().collect(),
        assets: inputs.assets.keys().cloned().collect(),
        saved_revision: if folder {
            String::new()
        } else {
            revision.into()
        },
        retained_sources,
        retained_assets,
        recordings,
        motion_tests,
    };
    atomic_write(
        &metadata,
        "project.json",
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}
/// Saves only into a new directory or a previously bound directory whose inputs
/// still match the last observation. External edits must be reloaded first.
pub(crate) fn save_project(
    directory: &Path,
    expected_disk: Option<&str>,
    revisions: &crate::Snapshots<Revision>,
    current: &str,
    blobs: &Path,
    stored: &crate::StoredWorkspace,
    prefer_folder: bool,
) -> Result<Binding> {
    let exists = directory.exists();
    let folder = if exists {
        metadata_directory(directory)? != directory
    } else {
        prefer_folder
    };
    if exists {
        if fs::symlink_metadata(directory)?.file_type().is_symlink() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Project destination cannot be a symlink",
            ));
        }
        match expected_disk {
            Some(expected)
                if {
                    let loaded = read_project(directory)?;
                    loaded.fingerprint == expected
                        && loaded.manifest.motion_tests
                            == stored.project.as_ref().and_then(|b| b.motion_tests.clone())
                        && loaded.manifest.recordings
                            == stored.project.as_ref().and_then(|b| b.recordings.clone())
                } => {}
            _ => {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Destination exists or contains external edits; open/reload it or choose a new project folder",
                ));
            }
        }
    } else if expected_disk.is_some() {
        return Err(Error::domain(
            "REVISION_CONFLICT",
            "Saved project folder disappeared",
        ));
    }
    let current_revision = revisions.read(current)?;
    // Every snapshot is assembled in a sibling staging directory then published
    // with one rename. Existing revision directories are never modified.
    fs::create_dir_all(directory)?;
    let root = directory.canonicalize()?;
    if !folder {
        let history = checked_path(&root, "revisions")?;
        fs::create_dir_all(&history)?;
        for id in revisions.keys() {
            let revision = revisions.read(id)?;
            if uuid::Uuid::parse_str(&revision.id).is_err() {
                return Err(Error::domain("INVALID_REQUEST", "Invalid revision ID"));
            }
            let final_path = checked_path(&root, &format!("revisions/{}", revision.id))?;
            let fingerprint = fingerprint(&revision.inputs)?;
            if final_path.exists() {
                let snapshot: Snapshot = json_file(&final_path, "transaction.json")?;
                if snapshot.schema_version != 1
                    || snapshot.parent != revision.parent
                    || snapshot.restored_from != revision.restored_from
                    || snapshot.actor != revision.actor
                    || snapshot.id != revision.id
                    || snapshot.input_fingerprint != fingerprint
                    || read_project(&final_path)?.fingerprint != fingerprint
                {
                    return Err(Error::domain(
                        "REVISION_CONFLICT",
                        "Immutable revision snapshot was modified",
                    ));
                }
                continue;
            }
            let staging = history.join(format!(".{}.tmp", crate::id()));
            let result = (|| -> Result<()> {
                write_inputs(
                    &staging,
                    &revision.inputs,
                    &revision.id,
                    blobs,
                    None,
                    None,
                    false,
                )?;
                atomic_write(
                    &staging,
                    "transaction.json",
                    &serde_json::to_vec_pretty(&Snapshot {
                        schema_version: 1,
                        id: revision.id.clone(),
                        parent: revision.parent.clone(),
                        actor: revision.actor.clone(),
                        input_fingerprint: fingerprint,
                        restored_from: revision.restored_from.clone(),
                    })?,
                )?;
                fs::rename(&staging, &final_path)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_dir_all(&staging);
            }
            result?;
        }
    }
    // Root files are convenience source files. The manifest is written last;
    // immutable revisions above retain a recoverable successful source snapshot.
    if let Some(expected) = expected_disk
        && {
            let loaded = read_project(&root)?;
            loaded.fingerprint != expected
                || loaded.manifest.motion_tests
                    != stored.project.as_ref().and_then(|b| b.motion_tests.clone())
                || loaded.manifest.recordings
                    != stored.project.as_ref().and_then(|b| b.recordings.clone())
        }
    {
        return Err(Error::domain(
            "REVISION_CONFLICT",
            "Project files changed while saving snapshots; root files were preserved",
        ));
    }
    let metadata = if folder {
        checked_path(&root, ".cadpad")?
    } else {
        root.clone()
    };
    fs::create_dir_all(&metadata)?;
    let recordings = crate::portable_recordings::save(
        &metadata,
        blobs,
        &stored.recordings,
        &stored.recording_imports,
        &stored.revisions,
    )?;
    let motion_tests = crate::motion_tests::catalog_fingerprint(&stored.motion_tests)?;
    if let Some(digest) = &motion_tests {
        atomic_write(
            &metadata,
            &format!("motion-tests/{digest}.json"),
            &crate::motion_tests::encode(&stored.motion_tests)?,
        )?;
    }
    write_inputs(
        &root,
        &current_revision.inputs,
        current,
        blobs,
        recordings.clone(),
        motion_tests.clone(),
        folder,
    )?;
    Ok(Binding {
        directory: root,
        fingerprint: fingerprint(&current_revision.inputs)?,
        saved_revision: current.into(),
        recordings,
        recording_fingerprint: crate::playback::metadata_fingerprint(&stored.recordings)?,
        motion_test_fingerprint: motion_tests.clone(),
        motion_tests,
    })
}

pub fn store_blobs(directory: &Path, blobs: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    fs::create_dir_all(directory)?;
    for (hash, bytes) in blobs {
        if !valid_hash(hash) || bytes_hash(bytes) != *hash {
            return Err(Error::domain("INVALID_REQUEST", "Asset digest mismatch"));
        }
        store_verified_blob(directory, hash, bytes)?;
    }
    Ok(())
}

pub(crate) fn store_verified_blob(directory: &Path, hash: &str, bytes: &[u8]) -> Result<()> {
    let path = directory.join(hash);
    if path.exists() {
        if read_bounded(&path, ASSET_LIMIT)? != bytes {
            return Err(Error::domain("INVALID_REQUEST", "Corrupt managed asset"));
        }
    } else {
        atomic_write(directory, hash, bytes)?;
    }
    Ok(())
}

/// Portable history contains source only. Display data is reconstructed after
/// opening, never mistaken for geometry belonging to another running workspace.
pub(crate) fn read_history(
    directory: &Path,
    blob_directory: &Path,
) -> Result<crate::Snapshots<Revision>> {
    if metadata_directory(directory)? != directory {
        return Ok(crate::Snapshots::default());
    }
    let root = directory.canonicalize()?;
    let history = checked_path(&root, "revisions")?;
    if !history.exists() {
        return Ok(crate::Snapshots::default());
    }
    let mut revisions = crate::Snapshots::default();
    for entry in fs::read_dir(history)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if uuid::Uuid::parse_str(&name).is_err() || !entry.file_type()?.is_dir() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Unexpected immutable revision entry",
            ));
        }
        let path = checked_path(&root, &format!("revisions/{name}"))?;
        let snapshot: Snapshot = json_file(&path, "transaction.json")?;
        let loaded = read_project(&path)?;
        if snapshot.schema_version != 1
            || snapshot.id != name
            || snapshot.input_fingerprint != loaded.fingerprint
            || loaded.manifest.saved_revision != name
        {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Immutable revision snapshot was modified",
            ));
        }
        store_blobs(blob_directory, &loaded.blobs)?;
        revisions.park(
            snapshot.id.clone(),
            Revision {
                id: snapshot.id,
                parent: snapshot.parent,
                inputs: loaded.inputs,
                model: serde_json::json!({"parts":[],"rebuild_required":true}),
                actor: snapshot.actor,
                restored_from: snapshot.restored_from,
            },
        )?;
        if revisions.len() > 10000 {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Project history exceeds 10,000 revisions",
            ));
        }
    }
    for (id, summary) in revisions.headers() {
        if let Some(target) = &summary.restored_from {
            let restored = revisions.read(target)?;
            let revision = revisions.read(id)?;
            let mut expected = restored.inputs.clone();
            if expected.dependencies.is_empty() {
                expected.dependencies = revision.inputs.dependencies.clone();
            }
            if target == id || fingerprint(&expected)? != summary.input_fingerprint {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Undo restoration does not match its source snapshot",
                ));
            }
        }
    }
    let parents: std::collections::HashMap<_, _> = revisions
        .headers()
        .map(|(id, h)| (id.as_str(), h.parent.as_deref()))
        .collect();
    let mut complete = std::collections::HashSet::new();
    for (id, _) in revisions.headers() {
        let mut seen = std::collections::HashSet::new();
        let mut next = Some(id.as_str());
        while let Some(id) = next {
            if complete.contains(id) {
                break;
            }
            if !seen.insert(id) {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Project history contains a cycle",
                ));
            }
            next = *parents.get(id).ok_or_else(|| {
                Error::domain("INVALID_REQUEST", "Project history has a missing parent")
            })?;
        }
        complete.extend(seen);
    }
    Ok(revisions)
}

pub fn read_python_project(entry: &Path) -> Result<(Inputs, BTreeMap<String, Vec<u8>>)> {
    read_python_project_excluding(entry, None)
}
pub(crate) fn read_python_project_excluding(
    entry: &Path,
    excluded: Option<&Path>,
) -> Result<(Inputs, BTreeMap<String, Vec<u8>>)> {
    let entry = entry.canonicalize()?;
    let root = entry.parent().unwrap();
    let name = entry
        .file_name()
        .unwrap()
        .to_str()
        .ok_or_else(|| Error::domain("INVALID_REQUEST", "Entry name must be UTF-8"))?;
    validate_path(name)?;
    let mut inputs = Inputs {
        entry_point: name.into(),
        ..Inputs::default()
    };
    inputs.files.clear();
    let mut paths = Vec::new();
    scan_files(root, root, &mut paths, true, excluded)?;
    for path in paths {
        let bytes = read_bounded(&checked_path(root, &path)?, SOURCE_LIMIT)?;
        inputs.files.insert(
            path,
            String::from_utf8(bytes)
                .map_err(|_| Error::domain("INVALID_REQUEST", "Python source must be UTF-8"))?,
        );
    }
    for (name, target) in [
        ("parameters.json", &mut inputs.parameters),
        ("annotations.json", &mut inputs.annotations),
        ("mechanism.json", &mut inputs.mechanism),
    ] {
        if checked_path(root, name)?.exists() {
            *target = json_file(root, name)?;
        }
    }
    let mut blobs = BTreeMap::new();
    let assets = checked_path(root, "assets")?;
    if assets.exists() {
        let mut paths = Vec::new();
        scan_files(&assets, &assets, &mut paths, false, excluded)?;
        let mut total = 0;
        for path in paths {
            let bytes = read_bounded(&checked_path(&assets, &path)?, ASSET_LIMIT)?;
            total += bytes.len();
            if total > TOTAL_LIMIT {
                return Err(Error::domain("INVALID_REQUEST", "Assets exceed 256 MiB"));
            }
            let digest = bytes_hash(&bytes);
            inputs.assets.insert(path, digest.clone());
            blobs.insert(digest, bytes);
        }
    }
    validate_inputs(&inputs)?;
    Ok((inputs, blobs))
}

#[cfg(test)]
mod mesh_tests {
    use super::{list_mesh_files, mesh_path, summarize_stl};
    use std::fs;
    use std::io::Write;

    fn put_f32(bytes: &mut [u8], offset: usize, value: f32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn stl_summary_reads_binary_and_ascii_without_keeping_the_mesh() {
        let mut binary = vec![0u8; 84 + 50];
        binary[80..84].copy_from_slice(&1u32.to_le_bytes());
        put_f32(&mut binary, 96, 0.0);
        put_f32(&mut binary, 100, 0.0);
        put_f32(&mut binary, 104, 0.0);
        put_f32(&mut binary, 108, 10.0);
        put_f32(&mut binary, 124, 20.0);
        put_f32(&mut binary, 128, 5.0);
        let summary = summarize_stl(&binary).unwrap();
        assert_eq!(summary.format, "binary");
        assert_eq!(summary.triangles, 1);
        assert_eq!(summary.min, [0.0, 0.0, 0.0]);
        assert_eq!(summary.max, [10.0, 20.0, 5.0]);
        assert_eq!(summary.size, [10.0, 20.0, 5.0]);

        let ascii = b"solid part\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 4 0 0\nvertex 0 3 1\nendloop\nendfacet\nendsolid\n";
        let summary = summarize_stl(ascii).unwrap();
        assert_eq!(summary.format, "ascii");
        assert_eq!(summary.triangles, 1);
        assert_eq!(summary.size, [4.0, 3.0, 1.0]);
        assert!(summarize_stl(b"not a mesh").is_err());
        assert!(mesh_path("../part.stl").is_err());
        assert!(mesh_path("model.py").is_err());
        assert!(mesh_path("assets/part.stl").is_ok());
    }

    #[test]
    fn project_metadata_hashes_assets_without_retaining_bodies() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("model.py"), b"# source\n").unwrap();
        super::initialize_folder(root.path(), "model.py").unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::write(root.path().join("assets/mesh.stl"), vec![7; 128 * 1024 + 1]).unwrap();

        let full = super::read_project(root.path()).unwrap();
        let metadata = super::read_project_metadata(root.path()).unwrap();
        assert_eq!(metadata.fingerprint, full.fingerprint);
        assert_eq!(metadata.inputs.assets, full.inputs.assets);
        assert_eq!(full.blobs.len(), 1);
        assert!(metadata.blobs.is_empty());
    }

    #[test]
    fn project_mesh_list_includes_assets_and_skips_sources_and_revisions() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("refs")).unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::create_dir_all(root.path().join("revisions")).unwrap();
        fs::write(root.path().join("refs/part.stl"), b"solid\n").unwrap();
        fs::write(root.path().join("assets/mesh.stl"), b"solid\n").unwrap();
        fs::write(root.path().join("model.py"), b"print(1)\n").unwrap();
        fs::write(root.path().join("revisions/old.stl"), b"solid\n").unwrap();
        let mut hidden = fs::File::create(root.path().join(".secret.stl")).unwrap();
        hidden.write_all(b"solid\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            root.path().join("refs/part.stl"),
            root.path().join("linked.stl"),
        )
        .unwrap();

        let files = list_mesh_files(root.path()).unwrap();
        assert_eq!(
            files,
            vec!["assets/mesh.stl".to_string(), "refs/part.stl".to_string()]
        );
    }
}
