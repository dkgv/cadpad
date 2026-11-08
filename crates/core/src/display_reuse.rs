//! Reuse only independently copied, checksummed display data after a candidate's
//! exact geometry is known. Identical whole-project digests may skip source via
//! authoritative reuse in `geometry_cache`; otherwise source still executes.
use crate::{Core, Error, Result, Worker, artifacts, portable, recovery};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

fn write_candidate(
    root: &Path,
    workspace: &str,
    revision: &str,
    target: &Path,
    handle: &str,
    data: &[u8],
) -> Result<()> {
    #[cfg(unix)]
    {
        use std::{
            io::Write,
            os::fd::{AsRawFd, FromRawFd},
        };
        let _ = target;
        let directory =
            artifacts::open_scoped_kind(root, &["artifacts", workspace, revision], true)?;
        let temporary = std::ffi::CString::new(format!(".reuse-{}.tmp", crate::id())).unwrap();
        let destination = std::ffi::CString::new(handle).unwrap();
        // SAFETY: generated single-component names relative to a pinned managed
        // directory. Neither a replaced root nor a final symlink is followed.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let result = (|| -> Result<()> {
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            file.write_all(data)?;
            file.sync_all()?;
            drop(file);
            if unsafe {
                libc::renameat(
                    directory.as_raw_fd(),
                    temporary.as_ptr(),
                    directory.as_raw_fd(),
                    destination.as_ptr(),
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        })();
        if result.is_err() {
            unsafe {
                libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0);
            }
        }
        result
    }
    #[cfg(not(unix))]
    {
        let _ = (root, workspace, revision);
        if std::fs::symlink_metadata(target)?.file_type().is_symlink() {
            return Err(Error::domain(
                "INVALID_ARTIFACT",
                "Candidate cache is a symlink",
            ));
        }
        portable::atomic_write(target, handle, data)
    }
}
use tokio::sync::watch;

const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BUFFER_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 512 * 1024;
const MAX_OFFERS: usize = 128;
fn cancelled(cancel: &watch::Receiver<bool>) -> Result<()> {
    if *cancel.borrow() {
        Err(Error::domain("CANCELLED", "Display reuse cancelled"))
    } else {
        Ok(())
    }
}
fn descriptors(mesh: &Value) -> Option<Vec<(&str, u64, &str)>> {
    let buffers = mesh["buffers"].as_object()?;
    if buffers.is_empty() || buffers.len() > 9 {
        return None;
    }
    buffers
        .iter()
        .map(|(key, buffer)| {
            if ![
                "vertices",
                "normals",
                "triangles",
                "edges",
                "triangles_per_face",
                "segments_per_edge",
                "obj_vertices",
                "face_types",
                "edge_types",
            ]
            .contains(&key.as_str())
            {
                return None;
            }
            let handle = buffer["handle"].as_str()?;
            let hash = buffer["sha256"].as_str()?;
            let bytes = buffer["bytes"].as_u64()?;
            (portable::valid_hash(hash)
                && handle == format!("{hash}.bin")
                && bytes <= MAX_BUFFER_BYTES)
                .then_some((handle, bytes, hash))
        })
        .collect()
}
#[derive(Clone, Copy)]
pub(crate) struct Scope<'a> {
    pub root: &'a Path,
    pub workspace: &'a str,
    pub base: &'a str,
    pub revision: &'a str,
}
pub(crate) fn copy_buffer(
    scope: Scope<'_>,
    target: &Path,
    handle: &str,
    bytes: u64,
    hash: &str,
) -> Result<()> {
    let Scope {
        root,
        workspace,
        base,
        revision,
    } = scope;
    let source = artifacts::open_scoped_kind(root, &["artifacts", workspace, base, handle], false)?;
    if !source.metadata()?.is_file()
        || source.metadata()?.len() != bytes
        || source.try_lock_shared().is_err()
    {
        return Err(Error::domain(
            "DISPLAY_CACHE_MISSING",
            "Reuse buffer is unavailable",
        ));
    }
    let mut data = Vec::new();
    source.take(bytes + 1).read_to_end(&mut data)?;
    if data.len() as u64 != bytes || portable::bytes_hash(&data) != hash {
        return Err(Error::domain(
            "DISPLAY_CACHE_MISSING",
            "Reuse buffer checksum changed",
        ));
    }
    // A new inode: candidate code can never write through to the prior revision.
    write_candidate(root, workspace, revision, target, handle, &data)
}
fn prepare(
    scope: Scope<'_>,
    old: &Value,
    new: &Value,
    policy: &str,
    cancel: &watch::Receiver<bool>,
) -> Result<Value> {
    let Scope {
        root,
        workspace,
        base: _,
        revision,
    } = scope;
    let mut offers = BTreeMap::<String, Value>::new();
    if !portable::valid_hash(policy) || old["dependencies"] != new["dependencies"] {
        return Ok(json!(offers));
    }
    let Some(old_parts) = old["parts"].as_array() else {
        return Ok(json!(offers));
    };
    let Some(new_parts) = new["parts"].as_array() else {
        return Ok(json!(offers));
    };
    let mut indexed = BTreeMap::<&str, Vec<&Value>>::new();
    for part in old_parts {
        if let Some(hash) = part["geometry_fingerprint"]["sha256"].as_str() {
            indexed.entry(hash).or_default().push(part);
        }
    }
    let target = root.join("artifacts").join(workspace).join(revision);
    let mut copied = BTreeSet::new();
    let mut total = 0_u64;
    let mut metadata = 2_usize;
    for part in new_parts {
        cancelled(cancel)?;
        if offers.len() >= MAX_OFFERS {
            break;
        }
        let Some(fingerprint) = part["geometry_fingerprint"]["sha256"].as_str() else {
            continue;
        };
        if offers.contains_key(fingerprint) {
            continue;
        }
        let Some(previous) = indexed.get(fingerprint).and_then(|parts| {
            parts.iter().find(|old| {
                old["geometry_fingerprint"] == part["geometry_fingerprint"]
                    && old["mesh"]["cache_policy"] == policy
                    && old["mesh"]["format"] == artifacts::MESH_FORMAT
            })
        }) else {
            continue;
        };
        let mesh = &previous["mesh"];
        let Some(buffers) = descriptors(mesh) else {
            continue;
        };
        let additional = buffers
            .iter()
            .filter(|(handle, _, _)| !copied.contains(*handle))
            .map(|(_, bytes, _)| *bytes)
            .sum::<u64>();
        let encoded = serde_json::to_vec(mesh)?.len() + fingerprint.len() + 4;
        if total + additional > MAX_BYTES || metadata + encoded > MAX_MANIFEST_BYTES {
            continue;
        }
        let mut valid = true;
        for (handle, bytes, hash) in buffers {
            cancelled(cancel)?;
            if copied.contains(handle) {
                continue;
            }
            if copy_buffer(scope, &target, handle, bytes, hash).is_err() {
                valid = false;
                break;
            }
            copied.insert(handle.to_owned());
            total += bytes;
        }
        if valid {
            offers.insert(fingerprint.into(), mesh.clone());
            metadata += encoded;
        }
    }
    cancelled(cancel)?;
    Ok(json!(offers))
}
impl Core {
    async fn reusable_meshes(
        &self,
        workspace: &str,
        base: &str,
        revision: &str,
        new: Value,
        policy: String,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Value> {
        cancelled(&cancel)?;
        let old = {
            let all = self.workspaces.lock().await;
            let ws = &all[workspace];
            if ws.stored.current != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Revision changed before display generation",
                ));
            }
            ws.stored.revisions[base].model.clone()
        };
        let publication = tokio::select! { biased;
            _=cancel.changed()=>return Err(Error::domain("CANCELLED","Cancelled while waiting for display reuse")),
            guard=self.display_publication.clone().read_owned()=>guard,
        };
        let root = self.root.clone();
        let owner = self._process_lock.clone();
        let workspace = workspace.to_owned();
        let base = base.to_owned();
        let revision = revision.to_owned();
        tokio::task::spawn_blocking(move || {
            let _publication = publication;
            let _owner = owner;
            prepare(
                Scope {
                    root: &root,
                    workspace: &workspace,
                    base: &base,
                    revision: &revision,
                },
                &old,
                &new,
                &policy,
                &cancel,
            )
        })
        .await
        .map_err(|_| Error::domain("WORKER_UNAVAILABLE", "Display reuse task failed"))?
    }
    pub(crate) async fn build_display_candidate(
        &self,
        worker: &mut Worker,
        workspace: &str,
        base: &str,
        revision: &str,
        mut params: Value,
        cancel: watch::Receiver<bool>,
    ) -> Result<Value> {
        params["generate_mesh"] = json!(false);
        let digest = params["input_digest"].as_str().unwrap_or("").to_owned();
        params["geometry_cache"] = self
            .reusable_geometry(workspace, base, revision, &digest, cancel.clone())
            .await?;
        let mut built = worker
            .request(revision, "build", params, cancel.clone())
            .await?;
        recovery::validate_publication(&built["data"])?;
        let policy = built["display_cache_policy"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        let reuse = self
            .reusable_meshes(
                workspace,
                base,
                revision,
                built["data"].clone(),
                policy,
                cancel.clone(),
            )
            .await?;
        cancelled(&cancel)?;
        {
            let all = self.workspaces.lock().await;
            if all[workspace].stored.current != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Revision changed before tessellation",
                ));
            }
        }
        let displayed = worker
            .request(
                revision,
                "remesh",
                json!({"reuse_meshes":reuse}),
                cancel.clone(),
            )
            .await?;
        cancelled(&cancel)?;
        if recovery::contract(&built["data"]) != recovery::contract(&displayed["data"]) {
            return Err(Error::domain(
                "RECOVERY_GEOMETRY_CHANGED",
                "Geometry/publication changed during candidate display generation",
            ));
        }
        recovery::validate_display_publication(&displayed["data"])?;
        let combined = format!(
            "{}{}",
            built["logs"].as_str().unwrap_or(""),
            displayed["logs"].as_str().unwrap_or("")
        );
        built["logs_truncated"] = json!(
            built["logs_truncated"] == true
                || displayed["logs_truncated"] == true
                || combined.chars().count() > 32768
        );
        built["logs"] = json!(combined.chars().take(32768).collect::<String>());
        built["data"] = displayed["data"].clone();
        Ok(built)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope(root: &Path) -> Scope<'_> {
        Scope {
            root,
            workspace: "w",
            base: "old",
            revision: "new",
        }
    }
    fn fixture(root: &Path) -> (Value, Value, String, String) {
        std::fs::create_dir_all(root.join("artifacts/w/old")).unwrap();
        std::fs::create_dir_all(root.join("artifacts/w/new")).unwrap();
        let data = b"display bytes";
        let digest = portable::bytes_hash(data);
        let handle = format!("{digest}.bin");
        std::fs::write(root.join("artifacts/w/old").join(&handle), data).unwrap();
        let policy = "a".repeat(64);
        let old = json!({"dependencies":{"test":"1"},"parts":[{"geometry_fingerprint":{"format":"occt-brep-v3-no-display-v1","sha256":"b".repeat(64)},"mesh":{"format":artifacts::MESH_FORMAT,"cache_policy":policy,"buffers":{"vertices":{"handle":handle,"sha256":digest,"bytes":data.len()}},"face_ids":[],"edge_ids":[]}}]});
        let mut new = old.clone();
        new["parts"][0].as_object_mut().unwrap().remove("mesh");
        (old, new, policy, handle)
    }
    #[test]
    fn copies_are_independent_and_missing_corrupt_policy_or_dependency_changes_are_cache_misses() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let (old, new, policy, handle) = fixture(root);
        let (_sender, cancel) = watch::channel(false);
        let offered = prepare(scope(root), &old, &new, &policy, &cancel).unwrap();
        assert_eq!(offered.as_object().unwrap().len(), 1);
        let source = root.join("artifacts/w/old").join(&handle);
        let target = root.join("artifacts/w/new").join(&handle);
        std::fs::write(&target, b"changed candidate").unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"display bytes");
        let mut deps = new.clone();
        deps["dependencies"]["test"] = json!("2");
        assert_eq!(
            prepare(scope(root), &old, &deps, &policy, &cancel).unwrap(),
            json!({})
        );
        assert_eq!(
            prepare(scope(root), &old, &new, &"c".repeat(64), &cancel).unwrap(),
            json!({})
        );
        std::fs::write(&source, b"wrong bytes!!").unwrap();
        assert_eq!(
            prepare(scope(root), &old, &new, &policy, &cancel).unwrap(),
            json!({})
        );
        std::fs::remove_file(&source).unwrap();
        assert_eq!(
            prepare(scope(root), &old, &new, &policy, &cancel).unwrap(),
            json!({})
        );
        let (_sender, cancel) = watch::channel(true);
        assert_eq!(
            prepare(scope(root), &old, &new, &policy, &cancel)
                .unwrap_err()
                .code(),
            "CANCELLED"
        );
    }
    #[test]
    fn offer_count_fits_the_worker_limit_without_discarding_eligible_entries() {
        let temp = tempfile::tempdir().unwrap();
        let (mut old, mut new, policy, _) = fixture(temp.path());
        let template = old["parts"][0].clone();
        let parts = (0..129)
            .map(|index| {
                let mut part = template.clone();
                part["geometry_fingerprint"]["sha256"] = json!(format!("{index:064x}"));
                part
            })
            .collect::<Vec<_>>();
        old["parts"] = json!(parts);
        new["parts"] = old["parts"].clone();
        let (_sender, cancel) = watch::channel(false);
        let result = prepare(scope(temp.path()), &old, &new, &policy, &cancel).unwrap();
        assert_eq!(result.as_object().unwrap().len(), MAX_OFFERS);
        assert!(result.get(format!("{:064x}", MAX_OFFERS - 1)).is_some());
        assert!(result.get(format!("{MAX_OFFERS:064x}")).is_none());
    }
    #[cfg(unix)]
    #[test]
    fn source_and_candidate_directory_symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let (old, new, policy, handle) = fixture(root);
        let (_sender, cancel) = watch::channel(false);
        let source = root.join("artifacts/w/old").join(&handle);
        let outside = root.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let external = outside.join("private");
        std::fs::write(&external, b"display bytes").unwrap();
        std::fs::remove_file(&source).unwrap();
        symlink(&external, &source).unwrap();
        assert_eq!(
            prepare(scope(root), &old, &new, &policy, &cancel).unwrap(),
            json!({})
        );
        assert!(
            std::fs::read_dir(root.join("artifacts/w/new"))
                .unwrap()
                .next()
                .is_none()
        );
        std::fs::remove_file(&source).unwrap();
        std::fs::write(&source, b"display bytes").unwrap();
        std::fs::remove_dir(root.join("artifacts/w/new")).unwrap();
        symlink(&outside, root.join("artifacts/w/new")).unwrap();
        assert_eq!(
            prepare(scope(root), &old, &new, &policy, &cancel).unwrap(),
            json!({})
        );
        assert_eq!(std::fs::read(&external).unwrap(), b"display bytes");
        assert!(!outside.join(&handle).exists());
    }
}
