use base64::Engine;
use cadpad_core::{
    Core,
    artifacts::{CHUNK_LIMIT, Chunk, EXPORT_LIMIT},
    worker::WorkerConfig,
};
use serde_json::Value;
async fn setup() -> (
    tempfile::TempDir,
    Core,
    String,
    String,
    String,
    std::path::PathBuf,
) {
    let temp = tempfile::tempdir().unwrap();
    let core = Core::open(
        temp.path().join("app"),
        WorkerConfig::bundled("unused".into()),
    )
    .await
    .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let rev = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let handle = "0123456789abcdef0123456789abcdef.step".to_string();
    let directory = temp.path().join("app/artifacts").join(&ws).join(&rev);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(&handle);
    (temp, core, ws, rev, handle, path)
}
fn decode(value: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(value["blob"].as_str().unwrap())
        .unwrap()
}
#[tokio::test]
async fn large_resources_are_manifested_resumable_bounded_and_content_checked() {
    let (_temp, core, ws, rev, handle, path) = setup().await;
    // Transport fixture, independent of CAD validity and tessellation.
    let bytes = (0..(9 * 1024 * 1024 + 73))
        .map(|i| (i % 251) as u8)
        .collect::<Vec<_>>();
    std::fs::write(&path, &bytes).unwrap();
    let root = core
        .export_resource("user", &ws, &rev, &handle, None)
        .await
        .unwrap();
    assert!(root.get("blob").is_none());
    let manifest = &root["manifest"];
    assert_eq!(manifest["bytes"], bytes.len());
    assert_eq!(
        manifest["sha256"],
        cadpad_core::portable::bytes_hash(&bytes)
    );
    assert_eq!(manifest["chunk_count"], 10);
    let id = manifest["content_id"].as_str().unwrap().to_string();
    let mut assembled = Vec::new();
    for offset in (0..bytes.len()).step_by(CHUNK_LIMIT as usize) {
        let length = CHUNK_LIMIT.min((bytes.len() - offset) as u64);
        let value = core
            .export_resource(
                "user",
                &ws,
                &rev,
                &handle,
                Some(Chunk {
                    offset: offset as u64,
                    length,
                    content_id: id.clone(),
                }),
            )
            .await
            .unwrap();
        let data = decode(&value);
        assert_eq!(value["sha256"], cadpad_core::portable::bytes_hash(&data));
        assert!(data.len() <= CHUNK_LIMIT as usize);
        assembled.extend(data);
    }
    assert_eq!(assembled, bytes);
    let resumed = core
        .export_resource(
            "user",
            &ws,
            &rev,
            &handle,
            Some(Chunk {
                offset: 2 * CHUNK_LIMIT + 19,
                length: 127,
                content_id: id.clone(),
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        decode(&resumed),
        bytes[2 * CHUNK_LIMIT as usize + 19..2 * CHUNK_LIMIT as usize + 146]
    );
    for (offset, length) in [
        (0, 0),
        (0, CHUNK_LIMIT + 1),
        (u64::MAX, 3),
        (bytes.len() as u64, 1),
    ] {
        assert_eq!(
            core.export_resource(
                "user",
                &ws,
                &rev,
                &handle,
                Some(Chunk {
                    offset,
                    length,
                    content_id: id.clone()
                })
            )
            .await
            .unwrap_err()
            .code(),
            "INVALID_REQUEST"
        );
    }
    std::fs::write(&path, b"modified").unwrap();
    assert_eq!(
        core.export_resource(
            "user",
            &ws,
            &rev,
            &handle,
            Some(Chunk {
                offset: 0,
                length: 1,
                content_id: id
            })
        )
        .await
        .unwrap_err()
        .code(),
        "ARTIFACT_CHANGED"
    );
    let fresh = core
        .export_resource("user", &ws, &rev, &handle, None)
        .await
        .unwrap();
    assert_eq!(decode(&fresh), b"modified");
    core.revoke_actor("user").await;
    assert!(
        core.export_resource("user", &ws, &rev, &handle, None)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn frozen_native_copy_is_atomic_and_failed_source_checks_preserve_destination() {
    let (temp, core, ws, rev, handle, path) = setup().await;
    let bytes = vec![0x6a; 9 * 1024 * 1024];
    std::fs::write(&path, &bytes).unwrap();
    let destination = temp.path().join("chosen.step");
    std::fs::write(&destination, b"prior").unwrap();
    let frozen = core.freeze_export(&ws, &rev, &handle).await.unwrap();
    assert_eq!(frozen.bytes(), bytes.len() as u64);
    frozen.save_to(&destination).unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    let frozen = core.freeze_export(&ws, &rev, &handle).await.unwrap();
    std::fs::write(&path, b"changed").unwrap();
    assert_eq!(
        frozen.save_to(&destination).unwrap_err().code(),
        "ARTIFACT_CHANGED"
    );
    assert_eq!(
        std::fs::metadata(&destination).unwrap().len(),
        bytes.len() as u64
    );
    assert!(
        !std::fs::read_dir(temp.path()).unwrap().any(|p| p
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp"))
    );
    let other = core.create_workspace("user").await.unwrap();
    let other_rev = core.get_project(&other).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        core.freeze_export(&other, &other_rev, &handle)
            .await
            .is_err()
    );
    assert_eq!(
        core.export_resource("user", &ws, "obsolete", &handle, None)
            .await
            .unwrap_err()
            .code(),
        "STALE_ENTITY"
    );
    assert!(
        core.export_resource("user", &ws, &rev, "geometry.mesh", None)
            .await
            .is_err()
    );
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(EXPORT_LIMIT + 1)
        .unwrap();
    assert!(core.freeze_export(&ws, &rev, &handle).await.is_err());
    assert!(
        core.export_resource("user", &ws, &rev, &handle, None)
            .await
            .is_err()
    );
}
#[cfg(unix)]
#[tokio::test]
async fn symlinked_scope_hops_and_special_files_cannot_be_read() {
    use std::os::unix::fs::symlink;
    let (temp, core, ws, rev, handle, path) = setup().await;
    let secret = temp.path().join("private");
    std::fs::write(&secret, b"secret").unwrap();
    symlink(&secret, &path).unwrap();
    assert!(core.artifact(&ws, &rev, &handle).await.is_err());
    assert!(
        core.export_resource("user", &ws, &rev, &handle, None)
            .await
            .is_err()
    );
    std::fs::remove_file(&path).unwrap();
    let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            core.artifact(&ws, &rev, &handle)
        )
        .await
        .unwrap()
        .is_err()
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"own").unwrap();
    let revision_directory = path.parent().unwrap();
    let moved = revision_directory.with_extension("moved");
    std::fs::rename(revision_directory, &moved).unwrap();
    symlink(&moved, revision_directory).unwrap();
    assert!(core.artifact(&ws, &rev, &handle).await.is_err());
}

#[tokio::test]
async fn native_save_retains_the_selected_export_across_a_background_design_commit() {
    use std::{collections::BTreeMap, path::PathBuf, time::Duration};
    let temp = tempfile::tempdir().unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let core = Core::open(temp.path().join("app"), WorkerConfig::development(&repo))
        .await
        .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let mut revision = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    async fn build(core: &Core, ws: &str, base: &str, width: u32) -> String {
        let source =
            format!("import cadquery as cq\nshow(cq.Workplane().box({width},20,6),name='part')");
        let draft = core
            .update_source(
                ws,
                base,
                None,
                BTreeMap::from([("model.py".into(), source)]),
                "user",
            )
            .await
            .unwrap();
        let job = core.build(ws, base, &draft.id, "user").await.unwrap();
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let job = core.get_job(ws, &job.id).await.unwrap();
                if !matches!(
                    job.state,
                    cadpad_core::JobState::Queued | cadpad_core::JobState::Running
                ) {
                    assert_eq!(job.state, cadpad_core::JobState::Succeeded);
                    break job.resulting_revision.unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    revision = build(&core, &ws, &revision, 10).await;
    let export = core
        .query(
            &ws,
            &revision,
            "export",
            serde_json::json!({"format":"STEP"}),
        )
        .await
        .unwrap();
    let handle = export["handle"].as_str().unwrap();
    let original = core.artifact(&ws, &revision, handle).await.unwrap();
    let frozen = core.freeze_export(&ws, &revision, handle).await.unwrap();
    let next = build(&core, &ws, &revision, 30).await;
    assert_ne!(next, revision);
    assert_eq!(
        core.export_resource("user", &ws, &revision, handle, None)
            .await
            .unwrap_err()
            .code(),
        "STALE_ENTITY"
    );
    let destination = temp.path().join("selected-pose.step");
    frozen.save_to(&destination).unwrap();
    assert_eq!(std::fs::read(destination).unwrap(), original);
}
