//! Leased derived-cache collection. Source revisions, drafts and build snapshots
//! remain authoritative and are never pruned by this collector.
use crate::{Core, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    path::{Path, PathBuf},
};

/// Collection-time target for referenced current display/native buffers, not an
/// aggregate disk quota. Active readers may keep the cache above this target.
pub const DEFAULT_DISPLAY_TARGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Hard admission limit for managed derived app data (`artifacts`, `blobs`,
/// `runs`). Source workspaces, journals and portable projects are outside this
/// quota. Collection runs before rejecting a job that would start over budget.
pub const DEFAULT_DISK_QUOTA_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// Hard admission limit for Core-owned native interpreter families. Each admitted
/// family reserves its per-worker memory budget until the capacity lease drops.
/// The default matches six families at the default 4 GiB per-worker budget so the
/// file-lease ceiling binds first; tighter quotas are set with `open_with_quotas`.
/// Idle retirement can free reservations; busy families keep them. This is not a
/// kernel allocation cap or a webview/GPU bound.
pub const DEFAULT_RAM_QUOTA_BYTES: u64 = 24 * 1024 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiskUsage {
    pub quota_bytes: u64,
    pub managed_bytes: u64,
    pub artifacts_bytes: u64,
    pub blobs_bytes: u64,
    pub runs_bytes: u64,
    pub within_quota: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RamUsage {
    pub quota_bytes: u64,
    pub reserved_bytes: u64,
    pub family_bytes: u64,
    pub within_quota: bool,
}
fn directory_bytes(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Managed cache path cannot be a symlink",
        ));
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Managed cache cannot contain symlinks",
                ));
            }
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}
pub fn managed_disk_usage(root: &Path, quota_bytes: u64) -> Result<DiskUsage> {
    let artifacts_bytes = directory_bytes(&root.join("artifacts"))?;
    let blobs_bytes = directory_bytes(&root.join("blobs"))?;
    let runs_bytes = directory_bytes(&root.join("runs"))?;
    let managed_bytes = artifacts_bytes
        .saturating_add(blobs_bytes)
        .saturating_add(runs_bytes);
    Ok(DiskUsage {
        quota_bytes,
        managed_bytes,
        artifacts_bytes,
        blobs_bytes,
        runs_bytes,
        within_quota: managed_bytes < quota_bytes,
    })
}
type DisplayRanks = HashMap<(String, String), u64>;
#[derive(Default)]
struct UsageClock {
    tick: u64,
    ranks: DisplayRanks,
}
#[derive(Default)]
pub(crate) struct DisplayUse(std::sync::Mutex<UsageClock>);
impl DisplayUse {
    pub(crate) fn touch(&self, workspace: &str, revision: &str) {
        let mut state = self.0.lock().unwrap();
        state.tick = state.tick.saturating_add(1);
        let tick = state.tick;
        state
            .ranks
            .insert((workspace.into(), revision.into()), tick);
    }
    fn ranks(&self, roots: &MeshRoots) -> DisplayRanks {
        let keys = roots
            .iter()
            .map(|(ws, rev, _)| (ws.clone(), rev.clone()))
            .collect::<HashSet<_>>();
        let mut state = self.0.lock().unwrap();
        state.ranks.retain(|key, _| keys.contains(key));
        state.ranks.clone()
    }
}

pub(crate) struct Lease {
    pub file: File,
}
impl Lease {
    pub fn shared(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory)?;
        if fs::symlink_metadata(directory)?.file_type().is_symlink() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Cache directory cannot be a symlink",
            ));
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options.open(directory.join(".lease"))?;
        Self::from_file(file)
    }
    pub fn from_file(file: File) -> Result<Self> {
        if !file.metadata()?.is_file() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Cache lease must be a regular file",
            ));
        }
        file.try_lock_shared()
            .map_err(|_| Error::domain("CACHE_BUSY", "Cache directory is being collected"))?;
        Ok(Self { file })
    }
}
pub(crate) struct Run {
    pub source: PathBuf,
    pub lease: Lease,
}
impl Run {
    pub fn new(directory: PathBuf) -> Result<Self> {
        let lease = Lease::shared(&directory)?;
        let source = directory.join("source");
        fs::create_dir_all(&source)?;
        Ok(Self { source, lease })
    }
}
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub display_target_bytes: u64,
    pub display_bytes_before: u64,
    pub display_bytes_after: u64,
    pub display_budget_exceeded: bool,
    pub evicted_display_revisions: u64,
    pub retained_unrecoverable_displays: u64,
    pub removed_runs: u64,
    pub removed_artifact_revisions: u64,
    pub removed_mesh_buffers: u64,
    pub retained_mesh_buffers: u64,
    pub removed_blobs: u64,
    pub removed_temporary_files: u64,
    pub retained_leased: u64,
    pub retained_unmanaged: u64,
    pub bytes_removed: u64,
    pub errors: Vec<String>,
    pub already_running: bool,
}
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}
fn directories(root: &Path) -> Result<Vec<(String, String, PathBuf)>> {
    if !root.exists() {
        return Ok(vec![]);
    }
    if fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Cache root cannot be a symlink",
        ));
    }
    let mut result = Vec::new();
    for workspace in fs::read_dir(root)? {
        let workspace = workspace?;
        let name = workspace.file_name().to_string_lossy().to_string();
        if !uuid(&name) || !workspace.file_type()?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(workspace.path())? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().to_string();
            if (uuid(&id) || id.strip_prefix(".gc-").is_some_and(uuid))
                && entry.file_type()?.is_dir()
            {
                result.push((name.clone(), id, entry.path()));
            }
        }
    }
    Ok(result)
}
fn exclusive(directory: &Path, report: &mut Report) -> Result<Option<File>> {
    let path = directory.join(".lease");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if directory
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.strip_prefix(".gc-").is_some_and(uuid))
            {
                let file = File::create(directory.join(".lease"))?;
                file.try_lock()
                    .map_err(|_| Error::domain("CACHE_BUSY", "Cache tombstone is in use"))?;
                return Ok(Some(file));
            }
            report.retained_unmanaged += 1;
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        report.retained_unmanaged += 1;
        return Ok(None);
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    if file.try_lock().is_err() {
        report.retained_leased += 1;
        return Ok(None);
    }
    Ok(Some(file))
}
fn size(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        total += size(&entry?.path())?;
    }
    Ok(total)
}
fn remove(path: &Path, report: &mut Report) -> Result<()> {
    let bytes = size(path)?;
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    report.bytes_removed += bytes;
    Ok(())
}
fn temporary(name: &str) -> bool {
    name.strip_suffix(".tmp")
        .is_some_and(|s| uuid(s.strip_prefix('.').unwrap_or(s)))
}
type Candidate = (&'static str, String, String, PathBuf, File);
type MeshRoots = Vec<(String, String, HashSet<String>)>;

fn buffer_name(name: &str) -> bool {
    if let Some(stem) = name.strip_suffix(".brep") {
        return crate::portable::valid_hash(stem);
    }
    name.strip_suffix(".bin").is_some_and(|stem| {
        // Content-addressed buffers and the earlier opaque UUID format.
        [32, 64].contains(&stem.len())
            && stem
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        a.len() == b.len()
            && a.created().ok() == b.created().ok()
            && a.modified().ok() == b.modified().ok()
    }
}
struct MeshDirectory {
    #[cfg(not(unix))]
    path: PathBuf,
    #[cfg(unix)]
    file: File,
}
#[cfg(unix)]
struct MeshEntries {
    directory: *mut libc::DIR,
    done: bool,
}
#[cfg(unix)]
fn directory_errno() -> *mut libc::c_int {
    // SAFETY: these libc accessors return the calling thread's errno cell.
    #[cfg(target_vendor = "apple")]
    {
        unsafe { libc::__error() }
    }
    #[cfg(target_os = "linux")]
    {
        unsafe { libc::__errno_location() }
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    {
        std::ptr::null_mut()
    }
}
#[cfg(unix)]
impl Iterator for MeshEntries {
    type Item = Result<std::ffi::OsString>;
    fn next(&mut self) -> Option<Self::Item> {
        use std::os::unix::ffi::OsStrExt;
        if self.done {
            return None;
        }
        let errno = directory_errno();
        if errno.is_null() {
            self.done = true;
            return Some(Err(Error::domain(
                "UNSUPPORTED_OPERATION",
                "Pinned cache enumeration is not implemented on this platform",
            )));
        }
        loop {
            // SAFETY: this stream is owned by this non-shared iterator, and
            // errno belongs to the same calling thread. Copy the name before
            // the next readdir call can invalidate the returned entry.
            let entry = unsafe {
                *errno = 0;
                libc::readdir(self.directory)
            };
            if entry.is_null() {
                self.done = true;
                let code = unsafe { *errno };
                return if code == 0 {
                    None
                } else {
                    Some(Err(std::io::Error::from_raw_os_error(code).into()))
                };
            }
            let bytes = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            return Some(Ok(std::ffi::OsStr::from_bytes(bytes).to_os_string()));
        }
    }
}
#[cfg(unix)]
impl Drop for MeshEntries {
    fn drop(&mut self) {
        // SAFETY: fdopendir transferred the duplicated descriptor to this
        // stream, which is closed exactly once here, including on errors.
        unsafe {
            libc::closedir(self.directory);
        }
    }
}
impl MeshDirectory {
    fn open(root: &Path, workspace: &str, revision: &str) -> Result<Self> {
        Ok(Self {
            #[cfg(not(unix))]
            path: root.join("artifacts").join(workspace).join(revision),
            #[cfg(unix)]
            file: crate::artifacts::open_scoped_kind(
                root,
                &["artifacts", workspace, revision],
                true,
            )?,
        })
    }
    #[cfg(unix)]
    fn entries(&self) -> Result<MeshEntries> {
        use std::os::fd::AsRawFd;
        // SAFETY: duplicate the live directory descriptor without leaking it
        // into subprocesses. On success fdopendir owns it; failure closes it.
        let fd = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let directory = unsafe { libc::fdopendir(fd) };
        if directory.is_null() {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(error.into());
        }
        Ok(MeshEntries {
            directory,
            done: false,
        })
    }
    #[cfg(not(unix))]
    fn entries(&self) -> Result<impl Iterator<Item = Result<std::ffi::OsString>>> {
        Ok(fs::read_dir(&self.path)?.map(|entry| Ok(entry?.file_name())))
    }
    fn open_file(&self, name: &str) -> Result<File> {
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = std::ffi::CString::new(name)
                .map_err(|_| Error::domain("INVALID_REQUEST", "Invalid cache file name"))?;
            // SAFETY: this is a pinned, scoped directory descriptor. The name
            // and fd remain live; ownership transfers exactly once on success.
            let fd = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDWR | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(unsafe { File::from_raw_fd(fd) })
        }
        #[cfg(not(unix))]
        {
            let path = self.path.join(name);
            if fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Cache symlinks are not followed",
                ));
            }
            Ok(fs::OpenOptions::new().read(true).write(true).open(path)?)
        }
    }
    fn remove_file(&self, name: &str) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let name = std::ffi::CString::new(name)
                .map_err(|_| Error::domain("INVALID_REQUEST", "Invalid cache file name"))?;
            // SAFETY: unlinkat acts only within the pinned cache directory; it
            // cannot follow a replacement parent path or a final symlink.
            if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Ok(fs::remove_file(self.path.join(name))?)
        }
    }
}
fn managed_mesh_directory(
    root: &Path,
    workspace: &str,
    revision: &str,
    report: &mut Report,
) -> Result<Option<(MeshDirectory, Lease)>> {
    if !uuid(workspace) || !uuid(revision) {
        report.retained_unmanaged += 1;
        return Ok(None);
    }
    let directory = root.join("artifacts").join(workspace).join(revision);
    let mut safe = true;
    for path in [
        root.join("artifacts"),
        root.join("artifacts").join(workspace),
        directory.clone(),
    ] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => (),
            Ok(_) => {
                report.retained_unmanaged += 1;
                safe = false;
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                safe = false;
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    if !safe {
        return Ok(None);
    }
    // Never turn an unleased legacy directory into a managed cache merely
    // because its name resembles a revision. A shared directory lease keeps
    // whole-directory collection out without excluding ordinary readers.
    if !fs::symlink_metadata(directory.join(".lease"))
        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
    {
        report.retained_unmanaged += 1;
        return Ok(None);
    }
    let directory = MeshDirectory::open(root, workspace, revision)?;
    let lease = match Lease::from_file(directory.open_file(".lease")?) {
        Ok(lease) => lease,
        Err(error) if error.code() == "CACHE_BUSY" => {
            report.retained_leased += 1;
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    Ok(Some((directory, lease)))
}
fn prune_meshes(root: &Path, meshes: &MeshRoots, report: &mut Report) -> Result<()> {
    for (workspace, revision, referenced) in meshes {
        let Some((directory, _lease)) = managed_mesh_directory(root, workspace, revision, report)?
        else {
            continue;
        };
        for entry in directory.entries()? {
            let name = entry?.to_string_lossy().to_string();
            // Export handles, publication temporaries and unknown files have
            // separate lifetimes. This pass only owns derived mesh buffers.
            if !buffer_name(&name) {
                continue;
            }
            if referenced.contains(&name) {
                report.retained_mesh_buffers += 1;
                continue;
            }
            let file = match directory.open_file(&name) {
                Ok(file) => file,
                Err(Error::Io(error))
                    if error.raw_os_error() == Some(libc::ELOOP)
                        || error.kind() == std::io::ErrorKind::NotFound =>
                {
                    report.retained_unmanaged += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                report.retained_unmanaged += 1;
                continue;
            }
            if file.try_lock().is_err() {
                report.retained_leased += 1;
                continue;
            }
            let path_metadata = directory.open_file(&name)?.metadata()?;
            if !metadata.is_file()
                || !path_metadata.is_file()
                || !same_file(&metadata, &path_metadata)
            {
                report.retained_unmanaged += 1;
                continue;
            }
            directory.remove_file(&name)?;
            report.removed_mesh_buffers += 1;
            report.bytes_removed += metadata.len();
        }
    }
    Ok(())
}
/// Count only known referenced buffers, then evict complete publications in
/// access order. Reserve all files before unlinking any so an entered reader
/// protects its publication. Scope remains pinned to directory descriptors.
fn budget_meshes(
    root: &Path,
    roots: &MeshRoots,
    ranks: &DisplayRanks,
    protected: &HashSet<(String, String)>,
    target: u64,
    report: &mut Report,
    mut invalidated: impl FnMut(&str, &str),
) -> Result<()> {
    let mut groups = Vec::new();
    for (workspace, revision, referenced) in roots {
        let Some((directory, _lease)) = managed_mesh_directory(root, workspace, revision, report)?
        else {
            continue;
        };
        let mut names = Vec::new();
        for name in referenced.iter().filter(|name| buffer_name(name)) {
            match directory.open_file(name) {
                Ok(file) => {
                    let metadata = file.metadata()?;
                    if metadata.is_file() {
                        names.push((name.clone(), metadata.len()));
                    } else {
                        report.retained_unmanaged += 1;
                    }
                }
                Err(Error::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(libc::ELOOP) => {}
                Err(error) => return Err(error),
            }
        }
        names.sort();
        let bytes = names.iter().map(|(_, bytes)| bytes).sum::<u64>();
        report.display_bytes_before = report.display_bytes_before.saturating_add(bytes);
        groups.push((
            ranks
                .get(&(workspace.clone(), revision.clone()))
                .copied()
                .unwrap_or(0),
            workspace.clone(),
            revision.clone(),
            names,
        ));
    }
    report.display_bytes_after = report.display_bytes_before;
    groups.sort_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
    for (_, workspace, revision, names) in groups {
        if report.display_bytes_after <= target {
            break;
        }
        if protected.contains(&(workspace.clone(), revision.clone())) {
            report.retained_unrecoverable_displays += 1;
            continue;
        }
        // Once eviction starts, errors become report entries rather than
        // discarding invalidations for publications already removed.
        let reservation = (|| -> Result<_> {
            let Some((directory, lease)) =
                managed_mesh_directory(root, &workspace, &revision, report)?
            else {
                return Ok(None);
            };
            let mut reserved = Vec::new();
            for (name, bytes) in names {
                let file = directory.open_file(&name)?;
                let metadata = file.metadata()?;
                if !metadata.is_file() || metadata.len() != bytes {
                    return Ok(None);
                }
                if file.try_lock().is_err() {
                    report.retained_leased += 1;
                    return Ok(None);
                }
                let current = directory.open_file(&name)?.metadata()?;
                if !same_file(&metadata, &current) {
                    return Ok(None);
                }
                reserved.push((name, bytes, file));
            }
            Ok(Some((directory, lease, reserved)))
        })();
        let (directory, _lease, reserved) = match reservation {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(error) => {
                if report.errors.len() < 32 {
                    report.errors.push(error.to_string());
                }
                continue;
            }
        };
        let mut removed = false;
        for (name, bytes, _file) in &reserved {
            match directory.remove_file(name) {
                Ok(()) => {
                    removed = true;
                    report.display_bytes_after = report.display_bytes_after.saturating_sub(*bytes);
                    report.bytes_removed += bytes;
                    report.removed_mesh_buffers += 1;
                    report.retained_mesh_buffers = report.retained_mesh_buffers.saturating_sub(1);
                }
                Err(error) => {
                    if report.errors.len() < 32 {
                        report.errors.push(error.to_string());
                    }
                    break;
                }
            }
        }
        if removed {
            report.evicted_display_revisions += 1;
            invalidated(&workspace, &revision);
        }
    }
    report.display_budget_exceeded = report.display_bytes_after > target;
    Ok(())
}

fn candidates(
    root: &Path,
    current: &HashSet<(String, String)>,
    report: &mut Report,
) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    for (kind, directory) in [
        ("run", root.join("runs")),
        ("artifact", root.join("artifacts")),
    ] {
        for (ws, id, path) in directories(&directory)? {
            // A reader after restart may be the only shared lease holder. Never
            // reserve the current revision exclusively even during the scan.
            if kind == "artifact" && current.contains(&(ws.clone(), id.clone())) {
                continue;
            }
            if let Some(lease) = exclusive(&path, report)? {
                candidates.push((kind, ws, id, path, lease));
            }
        }
    }
    Ok(candidates)
}
impl Core {
    pub async fn collect_cache(&self) -> Result<Report> {
        self.collect_cache_with_display_target(DEFAULT_DISPLAY_TARGET_BYTES)
            .await
    }
    /// A soft collection target. Busy readers, unmanaged data and concurrent
    /// publications are not a hard storage quota; inspect the returned report.
    pub async fn collect_cache_with_display_target(
        &self,
        display_target_bytes: u64,
    ) -> Result<Report> {
        let Ok(collector) = self.cache_collection.clone().try_lock_owned() else {
            return Ok(Report {
                already_running: true,
                display_target_bytes,
                ..Report::default()
            });
        };
        // Blob publishers hold a read guard until their authoritative metadata
        // commits. Collection takes the write guard before inspecting roots.
        let publication = self.blob_publication.clone().write_owned().await;
        // A remesh holds a read guard until its new descriptors are durable.
        // Ordinary display and exact-geometry reads do not take this guard.
        let display = self.display_publication.clone().write_owned().await;
        let (referenced, current, meshes, ranks, protected) = {
            let workspaces = self.workspaces.lock().await;
            let mut hashes = HashSet::new();
            for ws in workspaces.values() {
                for inputs in ws
                    .stored
                    .revisions
                    .values()
                    .map(|r| &r.inputs)
                    .chain(ws.stored.drafts.values().map(|d| &d.inputs))
                    .chain(ws.stored.job_snapshots.values().map(|d| &d.inputs))
                {
                    hashes.extend(inputs.assets.values().cloned());
                }
                hashes.extend(ws.stored.recordings.values().map(|r| r.digest.clone()));
            }
            let mut current = workspaces
                .iter()
                .map(|(id, ws)| (id.clone(), ws.stored.current.clone()))
                .collect::<HashSet<_>>();
            for (id, ws) in workspaces.iter() {
                for revision in crate::geometry_cache::retained_revisions(&ws.stored.geometry_cache)
                {
                    current.insert((id.clone(), revision));
                }
            }
            let meshes = workspaces
                .iter()
                .flat_map(|(id, ws)| {
                    let mut roots = Vec::new();
                    let revision = ws.stored.current.clone();
                    let mut handles = HashSet::new();
                    handles.extend(crate::geometry_cache::handles(
                        &ws.stored.geometry_cache,
                        &revision,
                    ));
                    if let Some(parts) = ws.stored.revisions[&revision].model["parts"].as_array() {
                        for part in parts {
                            if let Some(buffers) = part["mesh"]["buffers"].as_object() {
                                handles.extend(buffers.values().filter_map(|value| {
                                    value["handle"].as_str().map(str::to_owned)
                                }));
                            }
                        }
                    }
                    roots.push((id.clone(), revision, handles));
                    for offer_revision in
                        crate::geometry_cache::retained_revisions(&ws.stored.geometry_cache)
                    {
                        if offer_revision == ws.stored.current {
                            continue;
                        }
                        let handles = crate::geometry_cache::handles(
                            &ws.stored.geometry_cache,
                            &offer_revision,
                        )
                        .into_iter()
                        .collect::<HashSet<_>>();
                        if !handles.is_empty() {
                            roots.push((id.clone(), offer_revision, handles));
                        }
                    }
                    roots
                })
                .collect::<MeshRoots>();
            let protected = workspaces
                .iter()
                .filter_map(|(id, ws)| {
                    let model = &ws.stored.revisions[&ws.stored.current].model;
                    if crate::recovery::validate_publication(model).is_err()
                        || crate::recovery::validate_display_publication(model).is_err()
                    {
                        Some((id.clone(), ws.stored.current.clone()))
                    } else {
                        None
                    }
                })
                .collect::<HashSet<_>>();
            let ranks = self.display_use.ranks(&meshes);
            (hashes, current, meshes, ranks, protected)
        };
        let root = self.root.clone();
        let workspaces = self.workspaces.clone();
        let activity = self.activity.clone();
        let process_lock = self._process_lock.clone();
        let (mut report, candidates, temporaries, _scan_guards) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let guards = (collector, publication, display, process_lock);
                let mut report = Report {
                    display_target_bytes,
                    ..Report::default()
                };
                let candidates = candidates(&root, &current, &mut report)?;
                prune_meshes(&root, &meshes, &mut report)?;
                let blobs = root.join("blobs");
                if blobs.exists() {
                    if fs::symlink_metadata(&blobs)?.file_type().is_symlink() {
                        return Err(Error::domain(
                            "INVALID_REQUEST",
                            "Blob cache cannot be a symlink",
                        ));
                    }
                    for entry in fs::read_dir(&blobs)? {
                        let entry = entry?;
                        let name = entry.file_name().to_string_lossy().to_string();
                        if !entry.file_type()?.is_file() {
                            continue;
                        }
                        if crate::portable::valid_hash(&name) && !referenced.contains(&name) {
                            remove(&entry.path(), &mut report)?;
                            report.removed_blobs += 1;
                        } else if temporary(&name) {
                            remove(&entry.path(), &mut report)?;
                            report.removed_temporary_files += 1;
                        }
                    }
                }
                // Old atomic workspace writes are recoverable only after rename;
                // canonical workspace.json plus immutable job snapshots are authority.
                let mut temporaries = Vec::new();
                for entry in fs::read_dir(root.join("workspaces"))? {
                    let entry = entry?;
                    if !entry.file_type()?.is_dir() || !uuid(&entry.file_name().to_string_lossy()) {
                        continue;
                    }
                    for file in fs::read_dir(entry.path())? {
                        let file = file?;
                        if file.file_type()?.is_file()
                            && temporary(&file.file_name().to_string_lossy())
                        {
                            temporaries.push(file.path());
                        }
                    }
                }
                budget_meshes(
                    &root,
                    &meshes,
                    &ranks,
                    &protected,
                    display_target_bytes,
                    &mut report,
                    |workspace, revision| {
                        // This runs in the owned blocking task, even if its caller
                        // stops waiting. Never leave removed data with a stale epoch.
                        let mut workspaces = workspaces.blocking_lock();
                        if let Some(ws) = workspaces.get_mut(workspace)
                            && ws.stored.current == revision
                        {
                            ws.display_epoch = std::sync::Arc::new(crate::id());
                            let _ = activity.send(crate::events::Activity {
                                workspace_id: workspace.into(),
                                kind: "refresh".into(),
                                job: None,
                                actor: None,
                                tool: None,
                            });
                        }
                    },
                )?;
                Ok((report, candidates, temporaries, guards))
            })
            .await
            .map_err(|_| Error::domain("IO_ERROR", "Cache scan failed"))??;
        for temporary in temporaries {
            // All workspace journal writes hold this lock. If a scanned file
            // belonged to a concurrent write, its rename has completed by now.
            let _workspaces = self.workspaces.lock().await;
            if let Ok(metadata) = tokio::fs::symlink_metadata(&temporary).await {
                match tokio::fs::remove_file(&temporary).await {
                    Ok(()) => {
                        report.removed_temporary_files += 1;
                        report.bytes_removed += metadata.len();
                    }
                    Err(error) => {
                        if report.errors.len() < 32 {
                            report.errors.push(error.to_string());
                        }
                    }
                }
            }
        }
        // Recheck current revision at the atomic rename boundary. Deletion and
        // recursive traversal happen outside the workspace lock.
        for (kind, workspace, id, path, lease) in candidates {
            let trash = {
                let workspaces = self.workspaces.lock().await;
                if kind == "artifact"
                    && workspaces
                        .get(&workspace)
                        .is_some_and(|ws| ws.stored.current == id)
                {
                    continue;
                }
                let trash = path.with_file_name(format!(".gc-{}", crate::id()));
                match tokio::fs::rename(&path, &trash).await {
                    Ok(()) => Some(trash),
                    Err(error) => {
                        if report.errors.len() < 32 {
                            report.errors.push(error.to_string());
                        }
                        None
                    }
                }
            };
            if let Some(trash) = trash {
                let process_lock = self._process_lock.clone();
                let result = tokio::task::spawn_blocking(move || -> Result<u64> {
                    let _process_lock = process_lock;
                    let bytes = size(&trash)?;
                    fs::remove_dir_all(trash)?;
                    drop(lease);
                    Ok(bytes)
                })
                .await
                .map_err(|_| Error::domain("IO_ERROR", "Cache removal failed"))?;
                match result {
                    Ok(bytes) => {
                        report.bytes_removed += bytes;
                        if kind == "run" {
                            report.removed_runs += 1;
                        } else {
                            report.removed_artifact_revisions += 1;
                        }
                    }
                    Err(error) => {
                        if report.errors.len() < 32 {
                            report.errors.push(error.to_string());
                        }
                    }
                }
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_budget_uses_access_order_and_reserves_whole_publications_for_readers() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = crate::id();
        let first = crate::id();
        let second = crate::id();
        let names = [
            format!("{}.bin", "a".repeat(64)),
            format!("{}.bin", "b".repeat(64)),
        ];
        let mut roots = Vec::new();
        for revision in [&first, &second] {
            let directory = temp
                .path()
                .join("artifacts")
                .join(&workspace)
                .join(revision);
            drop(Lease::shared(&directory).unwrap());
            for name in &names {
                fs::write(directory.join(name), b"12345").unwrap();
            }
            fs::write(directory.join("unchanged.step"), b"export").unwrap();
            roots.push((
                workspace.clone(),
                revision.clone(),
                names.iter().cloned().collect(),
            ));
        }
        let usage = DisplayUse::default();
        usage.touch(&workspace, &first);
        usage.touch(&workspace, &second);
        usage.touch(&workspace, &first);
        let mut removed = Vec::new();
        let mut report = Report::default();
        budget_meshes(
            temp.path(),
            &roots,
            &usage.ranks(&roots),
            &HashSet::new(),
            10,
            &mut report,
            |ws, rev| removed.push((ws.to_owned(), rev.to_owned())),
        )
        .unwrap();
        assert_eq!(removed, vec![(workspace.clone(), second.clone())]);
        assert_eq!(report.display_bytes_before, 20);
        assert_eq!(report.display_bytes_after, 10);
        assert!(!report.display_budget_exceeded);
        let directory = temp.path().join("artifacts").join(&workspace).join(&first);
        let reader = File::open(directory.join(&names[1])).unwrap();
        reader.try_lock_shared().unwrap();
        let mut report = Report::default();
        budget_meshes(
            temp.path(),
            &roots,
            &usage.ranks(&roots),
            &HashSet::new(),
            0,
            &mut report,
            |_, _| panic!("A reader pins its whole publication"),
        )
        .unwrap();
        assert!(report.display_budget_exceeded);
        assert_eq!(report.display_bytes_after, 10);
        assert_eq!(report.evicted_display_revisions, 0);
        assert!(names.iter().all(|name| directory.join(name).exists()));
        drop(reader);
        let mut report = Report::default();
        budget_meshes(
            temp.path(),
            &roots,
            &usage.ranks(&roots),
            &HashSet::new(),
            0,
            &mut report,
            |_, _| {},
        )
        .unwrap();
        assert_eq!(report.display_bytes_after, 0);
        assert!(!report.display_budget_exceeded);
        assert_eq!(
            fs::read(directory.join("unchanged.step")).unwrap(),
            b"export"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_collection_keeps_publication_and_process_ownership_until_invalidation_finishes()
     {
        use std::{future::Future, task::Poll, time::Duration};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let core = Core::open(
            root.clone(),
            crate::worker::WorkerConfig::bundled("unused-worker".into()),
        )
        .await
        .unwrap();
        let ws = core.create_workspace("test").await.unwrap();
        let rev = core.get_project(&ws).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let directory = root.join("artifacts").join(&ws).join(&rev);
        drop(Lease::shared(&directory).unwrap());
        let name = format!("{}.bin", "a".repeat(64));
        fs::write(directory.join(&name), b"derived data").unwrap();
        {
            let mut workspaces = core.workspaces.lock().await;
            workspaces
                .get_mut(&ws)
                .unwrap()
                .stored
                .revisions
                .get_mut(&rev)
                .unwrap()
                .model["parts"] = serde_json::json!([{"geometry_fingerprint":{"format":"occt-brep-v3-no-display-v1","sha256":"a".repeat(64)},"mesh":{"format":crate::artifacts::MESH_FORMAT,"buffers":{"vertices":{"handle":name}}}}]);
        }
        let workspaces = core.workspaces.clone();
        let display = core.display_publication.clone();
        let blobs = core.blob_publication.clone();
        let collector = core.cache_collection.clone();
        let mut events = core.subscribe_activity();
        // Pause root capture at its usage-clock lock, then enqueue the workspace
        // guard ahead of invalidation. Tokio's FIFO mutex grants it first.
        let usage = core.display_use.clone();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let clock_thread = std::thread::spawn(move || {
            let _held = usage.0.lock().unwrap();
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready.await.unwrap();
        let task = {
            let core = core.clone();
            tokio::spawn(async move { core.collect_cache_with_display_target(0).await })
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while workspaces.try_lock().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut guard = Box::pin(workspaces.clone().lock_owned());
        std::future::poll_fn(|cx| {
            assert!(guard.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        release.send(()).unwrap();
        let held = tokio::time::timeout(Duration::from_secs(2), guard)
            .await
            .unwrap();
        clock_thread.join().unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while directory.join(&name).exists() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(core);
        assert!(display.try_read().is_err());
        assert!(blobs.try_read().is_err());
        assert!(collector.try_lock().is_err());
        let error = Core::open(
            root.clone(),
            crate::worker::WorkerConfig::bundled("unused-worker".into()),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.code(), "APP_UNAVAILABLE");
        drop(held);
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.workspace_id, ws);
        assert_eq!(event.kind, "refresh");
        let reopened = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match Core::open(
                    root.clone(),
                    crate::worker::WorkerConfig::bundled("unused-worker".into()),
                )
                .await
                {
                    Ok(core) => break core,
                    Err(error) if error.code() == "APP_UNAVAILABLE" => {
                        tokio::time::sleep(Duration::from_millis(2)).await
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        })
        .await
        .unwrap();
        drop(reopened);
    }

    #[tokio::test]
    async fn legacy_publications_without_recovery_fingerprints_are_protected_from_budget_eviction()
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let core = Core::open(
            root.clone(),
            crate::worker::WorkerConfig::bundled("unused-worker".into()),
        )
        .await
        .unwrap();
        let ws = core.create_workspace("legacy-test").await.unwrap();
        let rev = core.get_project(&ws).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let directory = root.join("artifacts").join(&ws).join(&rev);
        drop(Lease::shared(&directory).unwrap());
        let name = format!("{}.bin", "b".repeat(32));
        fs::write(directory.join(&name), b"old display").unwrap();
        {
            let mut workspaces = core.workspaces.lock().await;
            workspaces
                .get_mut(&ws)
                .unwrap()
                .stored
                .revisions
                .get_mut(&rev)
                .unwrap()
                .model["parts"] = serde_json::json!([{"mesh":{"format":crate::artifacts::MESH_FORMAT,"buffers":{"vertices":{"handle":name}}}}]);
        }
        let journal = fs::read(root.join("workspaces").join(&ws).join("workspace.json")).unwrap();
        let report = core.collect_cache_with_display_target(0).await.unwrap();
        assert!(report.errors.is_empty());
        assert_eq!(report.retained_unrecoverable_displays, 1);
        assert!(report.display_budget_exceeded);
        assert_eq!(report.evicted_display_revisions, 0);
        assert_eq!(fs::read(directory.join(name)).unwrap(), b"old display");
        assert_eq!(
            fs::read(root.join("workspaces").join(ws).join("workspace.json")).unwrap(),
            journal
        );
    }

    #[cfg(unix)]
    #[test]
    fn per_file_removal_stays_in_the_opened_directory_after_a_parent_path_swap() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let ws = crate::id();
        let rev = crate::id();
        let path = temp.path().join("artifacts").join(&ws).join(&rev);
        drop(Lease::shared(&path).unwrap());
        let name = format!("{}.bin", "a".repeat(64));
        fs::write(path.join(&name), b"derived").unwrap();
        let directory = MeshDirectory::open(temp.path(), &ws, &rev).unwrap();
        let original = path.with_file_name("moved-cache");
        fs::rename(&path, &original).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join(&name), b"unrelated data").unwrap();
        symlink(&outside, &path).unwrap();
        assert!(
            directory
                .entries()
                .unwrap()
                .any(|entry| entry.unwrap() == ".lease")
        );
        assert_eq!(
            directory
                .open_file(&name)
                .unwrap()
                .metadata()
                .unwrap()
                .len(),
            7
        );
        directory.remove_file(&name).unwrap();
        assert!(!original.join(&name).exists());
        assert_eq!(fs::read(outside.join(name)).unwrap(), b"unrelated data");
    }
    #[test]
    fn current_mesh_collection_preserves_references_readers_exports_and_unknown_files() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = crate::id();
        let revision = crate::id();
        let directory = temp
            .path()
            .join("artifacts")
            .join(&workspace)
            .join(&revision);
        drop(Lease::shared(&directory).unwrap());
        let live = format!("{}.bin", "a".repeat(64));
        let orphan = format!("{}.bin", "b".repeat(64));
        let reading = format!("{}.bin", "c".repeat(64));
        let legacy = format!("{}.bin", "d".repeat(32));
        let export = format!("{}.step", "e".repeat(32));
        for name in [&live, &orphan, &reading, &legacy, &export, "unknown.bin"] {
            fs::write(directory.join(name), b"retained or derived").unwrap();
        }
        let file = File::open(directory.join(&reading)).unwrap();
        file.try_lock_shared().unwrap();
        let roots = vec![(workspace, revision, HashSet::from([live.clone()]))];
        let mut report = Report::default();
        prune_meshes(temp.path(), &roots, &mut report).unwrap();
        assert_eq!(report.removed_mesh_buffers, 2);
        assert_eq!(report.retained_mesh_buffers, 1);
        assert_eq!(report.retained_leased, 1);
        for name in [&live, &reading, &export, "unknown.bin"] {
            assert!(directory.join(name).exists());
        }
        assert!(!directory.join(&orphan).exists());
        assert!(!directory.join(&legacy).exists());
        drop(file);
        prune_meshes(temp.path(), &roots, &mut report).unwrap();
        assert!(!directory.join(reading).exists());
        assert_eq!(report.removed_mesh_buffers, 3);
    }
    #[tokio::test]
    async fn managed_initialization_waits_for_collection_before_advertising_unlocked_leases() {
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        let core = Core::open(
            temp.path().join("app"),
            crate::worker::WorkerConfig::bundled("unused".into()),
        )
        .await
        .unwrap();
        let workspace = core.create_workspace("user").await.unwrap();
        let job = crate::id();
        let guard = core.display_publication.clone().write_owned().await;
        let inputs = crate::Inputs::default();
        let mut materialize = Box::pin(core.materialize(&workspace, &job, &inputs));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut materialize)
                .await
                .is_err()
        );
        assert!(!core.root.join("runs").join(&workspace).join(&job).exists());
        drop(guard);
        let run = materialize.await.unwrap();
        let (send, cancel) = tokio::sync::watch::channel(false);
        let (admission, _construction) = core.native_construction(cancel.clone()).await.unwrap();
        let cache = core
            .root
            .join("artifacts")
            .join(&workspace)
            .join(crate::id());
        let guard = core.display_publication.clone().write_owned().await;
        let mut spawn = Box::pin(core.spawn_managed_worker(cache.clone(), run, admission, cancel));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut spawn)
                .await
                .is_err()
        );
        assert!(!cache.exists());
        send.send_replace(true);
        let error = match spawn.await {
            Ok(_) => panic!("Cancelled initialization launched a worker"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "CANCELLED");
        assert!(!cache.exists());
        drop(guard);
    }
    #[test]
    fn a_scan_keeps_current_display_readable_while_reserving_retired_directories() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = crate::id();
        let current = crate::id();
        let retired = crate::id();
        let root = temp.path().join("artifacts").join(&workspace);
        for revision in [&current, &retired] {
            drop(Lease::shared(&root.join(revision)).unwrap());
        }
        let protected = HashSet::from([(workspace, current.clone())]);
        let held = candidates(temp.path(), &protected, &mut Report::default()).unwrap();
        // Keep the scan's exclusive leases alive, as the collector does while
        // checking metadata and removing other directories.
        let reader = Lease::shared(&root.join(&current)).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].2, retired);
        assert!(Lease::shared(&root.join(&retired)).is_err());
        drop(held);
        assert!(Lease::shared(&root.join(&retired)).is_ok());
        drop(reader);
    }
}
