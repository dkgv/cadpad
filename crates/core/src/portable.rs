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
pub fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
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
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::domain(
            "INVALID_REQUEST",
            format!("{} exceeds the {} byte limit", path.display(), limit),
        ));
    }
    Ok(bytes)
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
        if kind.is_symlink() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Project symlinks are not followed",
            ));
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
    walk_mesh_files(&root, &root, &mut files)?;
    files.sort();
    Ok(files)
}
fn walk_mesh_files(root: &Path, directory: &Path, files: &mut Vec<String>) -> Result<()> {
    if files.len() >= 256
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
        if files.len() >= 256 {
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
            walk_mesh_files(root, &entry.path(), files)?;
        } else if kind.is_file() && name.to_ascii_lowercase().ends_with(".stl") {
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path().as_path())
                .to_string_lossy()
                .replace('\\', "/");
            if mesh_path(&relative).is_ok() {
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
    if bytes.len() >= 84 {
        let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as u64;
        let binary_len = 84u64.saturating_add(count.saturating_mul(50));
        if count > 0 && binary_len == bytes.len() as u64 {
            return summarize_binary_stl(bytes, count);
        }
    }
    let text = String::from_utf8_lossy(bytes);
    if text.to_ascii_lowercase().contains("facet") {
        return summarize_ascii_stl(&text);
    }
    Err(Error::domain(
        "INVALID_GEOMETRY",
        "STL is neither a valid binary nor ASCII mesh",
    ))
}
fn summarize_binary_stl(bytes: &[u8], count: u64) -> Result<StlSummary> {
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
            for axis in 0..3 {
                min[axis] = min[axis].min(point[axis]);
                max[axis] = max[axis].max(point[axis]);
            }
        }
    }
    Ok(stl_summary("binary", count, min, max))
}
fn summarize_ascii_stl(text: &str) -> Result<StlSummary> {
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
/// Add only Cadpad configuration; existing source and unrelated files stay in place.
pub fn initialize_project_browser(directory: &Path) -> Result<()> {
    let root = directory.canonicalize()?;
    for index in 0..SOURCE_FILE_LIMIT {
        let name = format!("cadpad_view_{index}.py");
        let path = checked_path(&root, &name)?;
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };
        let result = (|| {
            file.write_all(PROJECT_BROWSER_SOURCE.as_bytes())?;
            file.sync_all()?;
            initialize_folder(&root, &name)
        })();
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        return result;
    }
    Err(Error::domain(
        "INVALID_REQUEST",
        "Project has too many source files",
    ))
}

pub fn initialize_folder(directory: &Path, entry: &str) -> Result<()> {
    let root = directory.canonicalize()?;
    validate_path(entry)?;
    let paths = python_entry_points(&root)?;
    if !paths.contains(&entry.to_string()) {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Choose a Python entry file inside the project folder",
        ));
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
    if !inputs.files.contains_key(&manifest.entry_point) {
        use std::io::Write;
        let path = checked_path(&root, &manifest.entry_point)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let source = "# Describe a part to get started.\n";
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(source.as_bytes())?;
        inputs
            .files
            .insert(manifest.entry_point.clone(), source.into());
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
        let bytes = read_bounded(
            &checked_path(&root, &format!("assets/{asset}"))?,
            ASSET_LIMIT,
        )?;
        if !manifest.assets.contains(asset)
            && manifest.retained_assets.get(asset) == Some(&bytes_hash(&bytes))
        {
            continue;
        }
        total += bytes.len();
        if total > TOTAL_LIMIT {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Project exceeds 256 MiB input limit",
            ));
        }
        let hash = bytes_hash(&bytes);
        blobs.insert(hash.clone(), bytes);
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
        file.sync_all()?;
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
    for (path, source) in &inputs.files {
        atomic_write(root, path, source.as_bytes())?;
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
    let metadata = if folder {
        checked_path(root, ".cadpad")?
    } else {
        root.to_path_buf()
    };
    fs::create_dir_all(&metadata)?;
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
    revisions: &[Revision],
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
    let current_revision = revisions
        .iter()
        .find(|r| r.id == current)
        .ok_or_else(|| Error::domain("REVISION_CONFLICT", "Unknown saved revision"))?;
    // Every snapshot is assembled in a sibling staging directory then published
    // with one rename. Existing revision directories are never modified.
    fs::create_dir_all(directory)?;
    let root = directory.canonicalize()?;
    if !folder {
        let history = checked_path(&root, "revisions")?;
        fs::create_dir_all(&history)?;
        for revision in revisions {
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
            let staging = history.join(format!(".{}.tmp", id()));
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
        let path = directory.join(hash);
        if path.exists() {
            if bytes_hash(&read_bounded(&path, ASSET_LIMIT)?) != *hash {
                return Err(Error::domain("INVALID_REQUEST", "Corrupt managed asset"));
            }
        } else {
            atomic_write(directory, hash, bytes)?;
        }
    }
    Ok(())
}

/// Portable history contains source only. Display data is reconstructed after
/// opening, never mistaken for geometry belonging to another running workspace.
pub fn read_history(directory: &Path, blob_directory: &Path) -> Result<Vec<Revision>> {
    if metadata_directory(directory)? != directory {
        return Ok(vec![]);
    }
    let root = directory.canonicalize()?;
    let history = checked_path(&root, "revisions")?;
    if !history.exists() {
        return Ok(vec![]);
    }
    let mut revisions = Vec::new();
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
        revisions.push(Revision {
            id: snapshot.id,
            parent: snapshot.parent,
            inputs: loaded.inputs,
            model: serde_json::json!({"parts":[],"rebuild_required":true}),
            actor: snapshot.actor,
            restored_from: snapshot.restored_from,
        });
        if revisions.len() > 10000 {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Project history exceeds 10,000 revisions",
            ));
        }
    }
    for revision in &revisions {
        if let Some(target) = &revision.restored_from {
            let restored = revisions
                .iter()
                .find(|candidate| candidate.id == *target)
                .ok_or_else(|| {
                    Error::domain(
                        "INVALID_REQUEST",
                        "Undo history has a missing restoration target",
                    )
                })?;
            let mut expected = restored.inputs.clone();
            // Previously unbuilt empty snapshots acquire the pinned dependency
            // inventory when their source is reconstructed for the first time.
            if expected.dependencies.is_empty() {
                expected.dependencies = revision.inputs.dependencies.clone();
            }
            if target == &revision.id || fingerprint(&expected)? != fingerprint(&revision.inputs)? {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Undo restoration does not match its source snapshot",
                ));
            }
        }
    }
    let parents: std::collections::HashMap<_, _> = revisions
        .iter()
        .map(|revision| (revision.id.as_str(), revision.parent.as_deref()))
        .collect();
    let mut complete = std::collections::HashSet::new();
    for revision in &revisions {
        let mut seen = std::collections::HashSet::new();
        let mut next = Some(revision.id.as_str());
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
