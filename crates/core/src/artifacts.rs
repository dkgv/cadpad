//! Scoped bounded export reads. No whole-file base64 for large artifacts.
use crate::{Core, Error, Result};
use base64::Engine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub(crate) const MESH_FORMAT: &str = "cadpad-mesh-v2";
pub const INLINE_LIMIT: u64 = 8 * 1024 * 1024;
pub const CHUNK_LIMIT: u64 = 1024 * 1024;
pub const EXPORT_LIMIT: u64 = 1024 * 1024 * 1024;
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum ExportFormat {
    Step,
    Stl,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ExportReply {
    pub handle: String,
    pub bytes: u64,
    pub sha256: String,
    pub format: ExportFormat,
    pub units: crate::geometry::Units,
    pub revision: String,
    pub pose_version: u64,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct McpExportReply {
    #[serde(flatten)]
    pub export: ExportReply,
    pub resource_uri: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub offset: u64,
    pub length: u64,
    pub content_id: String,
}
fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_REQUEST", message)
}
fn changed() -> Error {
    Error::domain(
        "ARTIFACT_CHANGED",
        "Export changed during download; read its root resource again",
    )
}
fn handle_valid(handle: &str) -> bool {
    handle
        .strip_suffix(".step")
        .or_else(|| handle.strip_suffix(".stl"))
        .is_some_and(|stem| stem.len() == 32 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn content_id(metadata: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        crate::hash(&format!(
            "{}:{}:{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec()
        ))
    }
    #[cfg(not(unix))]
    {
        crate::hash(&format!(
            "{}:{:?}:{:?}",
            metadata.len(),
            metadata.modified(),
            metadata.created()
        ))
    }
}
/// Open each component relative to an already-open directory. Symlink and FIFO
/// replacement cannot redirect a scoped read into another workspace or file.
#[cfg(unix)]
fn open_scoped(root: &Path, components: &[&str]) -> Result<File> {
    open_scoped_kind(root, components, false)
}
#[cfg(unix)]
pub(crate) fn open_scoped_kind(
    root: &Path,
    components: &[&str],
    directory_only: bool,
) -> Result<File> {
    use std::{
        ffi::CString,
        os::fd::{AsRawFd, FromRawFd},
    };
    let mut directory = File::open(root)?;
    for (index, component) in components.iter().enumerate() {
        if component.is_empty()
            || component.contains('/')
            || *component == ".."
            || *component == "."
        {
            return Err(invalid("Invalid artifact path component"));
        }
        let name =
            CString::new(*component).map_err(|_| invalid("Invalid artifact path component"))?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if index + 1 < components.len() || directory_only {
                libc::O_DIRECTORY
            } else {
                0
            };
        // SAFETY: directory and name remain live for openat; the returned fd is
        // owned exactly once after checking failure. O_NOFOLLOW applies per hop.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    if if directory_only {
        !directory.metadata()?.is_dir()
    } else {
        !directory.metadata()?.is_file()
    } {
        return Err(invalid("Artifact must be a regular file"));
    }
    Ok(directory)
}
#[cfg(not(unix))]
fn open_scoped(root: &Path, components: &[&str]) -> Result<File> {
    let mut path = root.to_path_buf();
    for component in components {
        if component.is_empty()
            || component.contains(['/', '\\'])
            || *component == ".."
            || *component == "."
        {
            return Err(invalid("Invalid artifact path component"));
        }
        path.push(component);
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(invalid("Artifact symlinks are not followed"));
        }
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("Artifact must be a regular file"));
    }
    Ok(file)
}
#[cfg(unix)]
fn artifact_lease(root: &Path, workspace: &str, revision: &str) -> Result<crate::cache::Lease> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let directory = open_scoped_kind(root, &["artifacts", workspace, revision], true)?;
    // SAFETY: the directory descriptor and C string remain valid, and a
    // successful descriptor is transferred into exactly one File.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".lease".as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    crate::cache::Lease::from_file(unsafe { File::from_raw_fd(fd) })
}
#[cfg(not(unix))]
fn artifact_lease(root: &Path, workspace: &str, revision: &str) -> Result<crate::cache::Lease> {
    crate::cache::Lease::shared(&root.join("artifacts").join(workspace).join(revision))
}
fn check_cancel(cancel: &tokio::sync::watch::Receiver<bool>) -> Result<()> {
    if *cancel.borrow() {
        Err(Error::domain("CANCELLED", "Artifact download cancelled"))
    } else {
        Ok(())
    }
}
impl Core {
    pub(crate) async fn check_artifact_revision(
        &self,
        workspace: &str,
        revision: &str,
    ) -> Result<()> {
        let workspaces = self.workspaces.lock().await;
        let ws = workspaces
            .get(workspace)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        if ws.stored.current != revision {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Artifact revision is no longer active",
            ));
        }
        Ok(())
    }
    /// Actor identity comes from the authenticated app transport, not a URI.
    pub async fn export_resource(
        &self,
        actor: &str,
        workspace: &str,
        revision: &str,
        handle: &str,
        chunk: Option<Chunk>,
    ) -> Result<Value> {
        self.ensure_actor_active(actor)?;
        self.check_artifact_revision(workspace, revision).await?;
        if !handle_valid(handle) {
            return Err(invalid(
                "Only opaque STEP/STL export handles are readable resources",
            ));
        }
        let root = self.root.clone();
        let workspace_name = workspace.to_owned();
        let revision_name = revision.to_owned();
        let name = handle.to_owned();
        let cancel = self.actor_signal(actor);
        let value=tokio::task::spawn_blocking(move ||->Result<_>{
            check_cancel(&cancel)?;
            let mut file=open_scoped(&root,&["artifacts",&workspace_name,&revision_name,&name])?;
            let _lease=artifact_lease(&root,&workspace_name,&revision_name)?;
            let metadata=file.metadata()?;let bytes=metadata.len();let identity=content_id(&metadata);
            if bytes>EXPORT_LIMIT{return Err(invalid("Export exceeds the 1 GiB resource capacity"));}
            let uri=format!("cadpad://{workspace_name}/{revision_name}/{name}");
            let mime=if name.ends_with(".step"){"model/step"}else{"model/stl"};
            if let Some(chunk)=chunk {
                if !crate::portable::valid_hash(&chunk.content_id)||chunk.content_id!=identity{return Err(changed());}
                let end=chunk.offset.checked_add(chunk.length).ok_or_else(||invalid("Artifact byte range overflow"))?;
                if chunk.length==0||chunk.length>CHUNK_LIMIT||chunk.offset>=bytes||end>bytes{return Err(invalid("Artifact range must contain 1..1048576 bytes within the file"));}
                file.seek(SeekFrom::Start(chunk.offset))?;
                let mut data=vec![0;chunk.length as usize];file.read_exact(&mut data)?;
                check_cancel(&cancel)?;
                if content_id(&file.metadata()?)!=identity{return Err(changed());}
                return Ok(json!({"blob":base64::engine::general_purpose::STANDARD.encode(&data),"mime_type":mime,"offset":chunk.offset,"length":chunk.length,"total_bytes":bytes,"content_id":identity,"sha256":crate::portable::bytes_hash(&data)}));
            }
            let mut hasher=Sha256::new();let mut inline=Vec::new();let mut buffer=[0u8;65536];let mut read=0u64;
            loop {
                check_cancel(&cancel)?;
                let count=file.read(&mut buffer)?;if count==0{break;}
                read+=count as u64;if read>bytes{return Err(changed());}
                hasher.update(&buffer[..count]);
                if bytes<=INLINE_LIMIT{inline.extend_from_slice(&buffer[..count]);}
            }
            if read!=bytes||content_id(&file.metadata()?)!=identity{return Err(changed());}
            let digest=format!("{:x}",hasher.finalize());
            if bytes<=INLINE_LIMIT {
                Ok(json!({"uri":uri,"blob":base64::engine::general_purpose::STANDARD.encode(inline),"mime_type":mime,"bytes":bytes,"sha256":digest,"content_id":identity}))
            }else{
                Ok(json!({"uri":uri,"mime_type":"application/vnd.cadpad.export-manifest+json","manifest":{"schema_version":1,"resource_uri":uri,"format":if name.ends_with(".step"){"STEP"}else{"STL"},"mime_type":mime,"bytes":bytes,"sha256":digest,"content_id":identity,"chunk_size":CHUNK_LIMIT,"chunk_count":bytes.div_ceil(CHUNK_LIMIT),"chunk_uri_template":format!("{uri}?offset={{offset}}&length={{length}}&content_id={identity}"),"instructions":"Read consecutive ranges up to chunk_size bytes, concatenate decoded blobs in byte-offset order, and verify total bytes and sha256. If ARTIFACT_CHANGED or STALE_ENTITY occurs, discard partial data and refresh the root resource. No model regeneration is needed for resuming an unchanged export."}}))
            }
        }).await.map_err(|_|invalid("Artifact read task failed"))??;
        self.ensure_actor_active(actor)?;
        self.check_artifact_revision(workspace, revision).await?;
        Ok(value)
    }
    /// Shared mesh transport remains binary. Reads occur outside the workspace lock.
    pub(crate) async fn read_artifact(
        &self,
        workspace: &str,
        revision: &str,
        handle: &str,
    ) -> Result<Vec<u8>> {
        if handle.is_empty()
            || handle.ends_with(".brep")
            || handle.len() > 68
            || handle.contains("..")
            || !handle
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b'.' || b.is_ascii_lowercase())
        {
            return Err(invalid("Invalid artifact handle"));
        }
        self.check_artifact_revision(workspace, revision).await?;
        let root = self.root.clone();
        let names = [workspace.to_owned(), revision.to_owned(), handle.to_owned()];
        let data = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut file = open_scoped(&root, &["artifacts", &names[0], &names[1], &names[2]])?;
            let _lease = artifact_lease(&root, &names[0], &names[1])?;
            // A same-revision repair may have replaced this buffer descriptor
            // after the caller obtained it. Pin this file until the read ends;
            // collection only unlinks exclusively leased derived buffers.
            file.try_lock_shared().map_err(|_| Error::domain("CACHE_BUSY", "Display buffer is being collected; refresh the project"))?;
            let metadata = file.metadata()?;
            if metadata.len() > EXPORT_LIMIT {
                return Err(invalid("Artifact exceeds 1 GiB"));
            }
            let identity = content_id(&metadata);
            let mut data = Vec::new();
            file.by_ref()
                .take(EXPORT_LIMIT + 1)
                .read_to_end(&mut data)?;
            if data.len() as u64 != metadata.len() || content_id(&file.metadata()?) != identity {
                return Err(changed());
            }
            if let Some(expected)=names[2].strip_suffix(".bin").filter(|hash|crate::portable::valid_hash(hash))
                && crate::portable::bytes_hash(&data)!=expected{return Err(Error::domain("ARTIFACT_CORRUPT","Display buffer checksum differs from its content-addressed handle; restore display"));}
            Ok(data)
        })
        .await
        .map_err(|_| invalid("Artifact read task failed"))??;
        self.check_artifact_revision(workspace, revision).await?;
        self.display_use.touch(workspace, revision);
        Ok(data)
    }
}

/// A scoped open file retained across the native Save dialog. The selected
/// export survives a later design commit; content changes still reject the save.
pub struct FrozenExport {
    file: File,
    identity: String,
    bytes: u64,
    _lease: crate::cache::Lease,
}
impl FrozenExport {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    /// The native dialog authorizes this destination. Copy uses bounded buffers
    /// and replaces the destination only after a complete, unchanged source copy.
    pub fn save_to(mut self, destination: &Path) -> Result<()> {
        use std::io::Write;
        if std::fs::symlink_metadata(destination).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(invalid("Export destination cannot be a symlink"));
        }
        if content_id(&self.file.metadata()?) != self.identity {
            return Err(changed());
        }
        let parent = destination
            .parent()
            .ok_or_else(|| invalid("Export destination needs a parent directory"))?
            .canonicalize()?;
        let destination = parent.join(
            destination
                .file_name()
                .ok_or_else(|| invalid("Export destination needs a file name"))?,
        );
        let temporary = parent.join(format!(".cadpad-export-{}.tmp", crate::id()));
        let result = (|| -> Result<()> {
            let mut output = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            let mut buffer = [0u8; 65536];
            let mut copied = 0u64;
            loop {
                let count = self.file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                copied += count as u64;
                if copied > self.bytes {
                    return Err(changed());
                }
                output.write_all(&buffer[..count])?;
            }
            if copied != self.bytes || content_id(&self.file.metadata()?) != self.identity {
                return Err(changed());
            }
            output.sync_all()?;
            drop(output);
            std::fs::rename(&temporary, destination)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
}
impl Core {
    pub async fn freeze_export(
        &self,
        workspace: &str,
        revision: &str,
        handle: &str,
    ) -> Result<FrozenExport> {
        self.check_artifact_revision(workspace, revision).await?;
        if !handle_valid(handle) {
            return Err(invalid("Invalid STEP/STL export handle"));
        }
        let root = self.root.clone();
        let names = [
            workspace.to_string(),
            revision.to_string(),
            handle.to_string(),
        ];
        let frozen = tokio::task::spawn_blocking(move || -> Result<_> {
            let file = open_scoped(&root, &["artifacts", &names[0], &names[1], &names[2]])?;
            let lease = artifact_lease(&root, &names[0], &names[1])?;
            let metadata = file.metadata()?;
            if metadata.len() > EXPORT_LIMIT {
                return Err(invalid("Export exceeds 1 GiB"));
            }
            Ok(FrozenExport {
                identity: content_id(&metadata),
                bytes: metadata.len(),
                file,
                _lease: lease,
            })
        })
        .await
        .map_err(|_| invalid("Could not retain export file"))??;
        self.check_artifact_revision(workspace, revision).await?;
        Ok(frozen)
    }
}

impl Core {
    /// Validate cached display bytes once per unique handle, with bounded read
    /// buffers. Missing/corrupt bytes request regeneration; symlink/scope errors
    /// remain errors rather than following an untrusted replacement.
    pub(crate) async fn mesh_buffers_valid(
        &self,
        workspace: &str,
        revision: &str,
        model: &Value,
    ) -> Result<bool> {
        let root = self.root.clone();
        let workspace = workspace.to_string();
        let revision = revision.to_string();
        let model = model.clone();
        tokio::task::spawn_blocking(move || -> Result<_> {
            let parts = model["parts"]
                .as_array()
                .ok_or_else(|| invalid("Model parts are missing"))?;
            if parts.is_empty() {
                return Ok(true);
            }
            let mut seen = std::collections::HashMap::<String, (u64, String)>::new();
            for part in parts {
                let Some(buffers) = part["mesh"]["buffers"].as_object() else {
                    return Ok(false);
                };
                if buffers.is_empty() || part["mesh"]["format"] != MESH_FORMAT {
                    return Ok(false);
                }
                for descriptor in buffers.values() {
                    let Some(handle) = descriptor["handle"].as_str() else {
                        return Ok(false);
                    };
                    if handle.len() > 68
                        || handle.contains("..")
                        || !handle
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() || b == b'.' || b.is_ascii_lowercase())
                    {
                        return Ok(false);
                    }
                    let Some(expected) = descriptor["bytes"].as_u64() else {
                        return Ok(false);
                    };
                    let Some(digest) = descriptor["sha256"].as_str() else {
                        return Ok(false);
                    };
                    if expected > EXPORT_LIMIT || !crate::portable::valid_hash(digest) {
                        return Ok(false);
                    }
                    if let Some((bytes, hash)) = seen.get(handle) {
                        if *bytes != expected || hash != digest {
                            return Ok(false);
                        }
                        continue;
                    }
                    seen.insert(handle.to_string(), (expected, digest.to_string()));
                    let mut file =
                        match open_scoped(&root, &["artifacts", &workspace, &revision, handle]) {
                            Ok(file) => file,
                            Err(Error::Io(error))
                                if error.kind() == std::io::ErrorKind::NotFound =>
                            {
                                return Ok(false);
                            }
                            Err(error) => return Err(error),
                        };
                    let _lease = artifact_lease(&root, &workspace, &revision)?;
                    let metadata = file.metadata()?;
                    if metadata.len() != expected {
                        return Ok(false);
                    }
                    let identity = content_id(&metadata);
                    let mut hasher = Sha256::new();
                    let mut buffer = [0u8; 65536];
                    let mut count = 0;
                    loop {
                        let n = file.read(&mut buffer)?;
                        if n == 0 {
                            break;
                        }
                        count += n as u64;
                        if count > expected {
                            return Ok(false);
                        }
                        hasher.update(&buffer[..n]);
                    }
                    if count != expected
                        || format!("{:x}", hasher.finalize()) != digest
                        || content_id(&file.metadata()?) != identity
                    {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        })
        .await
        .map_err(|_| invalid("Display cache verification task failed"))?
    }
}
