//! Desktop-authorized entry adapters. Analysis parses source without running it.
use crate::{Core, Error, Result, portable, worker::Worker};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

impl Core {
    pub async fn project_directory(&self, workspace: &str) -> Result<PathBuf> {
        let all = self.workspaces.lock().await;
        let ws = all
            .get(workspace)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        ws.stored
            .project
            .as_ref()
            .map(|binding| binding.directory.clone())
            .or_else(|| {
                ws.stored
                    .python_origin
                    .as_ref()
                    .and_then(|origin| origin.entry.parent().map(Path::to_path_buf))
            })
            .ok_or_else(|| {
                Error::domain(
                    "INVALID_REQUEST",
                    "Save this model to a project folder first",
                )
            })
    }
    pub async fn analyze_project_entry(&self, directory: PathBuf, entry: String) -> Result<Value> {
        let text = tokio::task::spawn_blocking(move || -> Result<_> {
            crate::validate_path(&entry)?;
            let root = directory.canonicalize()?;
            let bytes = portable::read_bounded(
                &portable::checked_path(&root, &entry)?,
                portable::SOURCE_LIMIT,
            )?;
            let digest = portable::bytes_hash(&bytes);
            let source = String::from_utf8(bytes)
                .map_err(|_| Error::domain("INVALID_REQUEST", "Python source must be UTF-8"))?;
            Ok((entry, source, digest))
        })
        .await
        .map_err(|_| Error::domain("IO_ERROR", "Could not read Python source"))??;
        let (_send, cancel) = tokio::sync::watch::channel(false);
        let (admission, _construction) = self.native_construction(cancel.clone()).await?;
        let cache = self.root.join("entry-analysis").join(crate::id());
        let mut config = self.config.clone();
        config.limits.operation_timeout_ms = 15000;
        let result = async {
            let mut worker =
                Worker::spawn_admitted(&config, cache.clone(), None, admission).await?;
            let result = worker
                .request(
                    "entry-analysis",
                    "analyze_entry",
                    json!({"entry":text.0,"source":text.1}),
                    cancel,
                )
                .await;
            let stopped = worker.stop().await;
            drop(worker);
            let mut plan = result?["data"].clone();
            stopped?;
            plan["source_hash"] = json!(text.2);
            Ok(plan)
        }
        .await;
        let _ = tokio::fs::remove_dir_all(cache).await;
        result
    }
}

pub fn manifest_hash(directory: &Path) -> Result<Option<String>> {
    let metadata = portable::metadata_directory(directory)?;
    let path = portable::checked_path(&metadata, "project.json")?;
    if !path.exists() || portable::read_manifest(directory).is_err() {
        return Ok(None);
    }
    Ok(Some(portable::bytes_hash(&portable::read_bounded(
        &path,
        portable::SOURCE_LIMIT,
    )?)))
}
/// Publish a new adapter without overwriting existing source, then select it in
/// configuration. The manifest and analyzed file must still match the preview.
pub fn configure_entry(
    directory: &Path,
    entry: &str,
    expected_manifest: Option<&str>,
    adapter: Option<(&str, &str)>,
) -> Result<String> {
    let root = directory.canonicalize()?;
    if manifest_hash(&root)?.as_deref() != expected_manifest {
        return Err(Error::domain(
            "REVISION_CONFLICT",
            "Project settings changed. Reopen entry setup to review them.",
        ));
    }
    crate::validate_path(entry)?;
    let mut created: Option<PathBuf> = None;
    let selected = if let Some((hash, code)) = adapter {
        let original = portable::read_bounded(
            &portable::checked_path(&root, entry)?,
            portable::SOURCE_LIMIT,
        )?;
        if portable::bytes_hash(&original) != hash {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Python source changed. Select it again to refresh the adapter preview.",
            ));
        }
        let mut name = String::new();
        for suffix in 0..1000 {
            name = if suffix == 0 {
                "cadpad_entry.py".into()
            } else {
                format!("cadpad_entry_{suffix}.py")
            };
            let path = portable::checked_path(&root, &name)?;
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    if let Err(error) = file
                        .write_all(code.as_bytes())
                        .and_then(|_| file.sync_all())
                    {
                        drop(file);
                        let _ = std::fs::remove_file(&path);
                        return Err(error.into());
                    }
                    created = Some(path);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        if created.is_none() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Choose another name for the Cadpad entry file",
            ));
        }
        name
    } else {
        entry.into()
    };
    let result = (|| -> Result<()> {
        if expected_manifest.is_none() {
            return portable::initialize_folder(&root, &selected);
        }
        let mut manifest = portable::read_manifest(&root)?;
        if !portable::python_entry_points(&root)?.contains(&selected) {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Choose a Python file in this project folder",
            ));
        }
        manifest.entry_point = selected.clone();
        manifest.saved_revision.clear();
        if !manifest.source_files.contains(&selected) {
            manifest.source_files.push(selected.clone());
        }
        let metadata = portable::metadata_directory(&root)?;
        portable::atomic_write(
            &metadata,
            "project.json",
            &serde_json::to_vec_pretty(&manifest)?,
        )
    })();
    if result.is_err() {
        if let Some(path) = created {
            let _ = std::fs::remove_file(path);
        }
    }
    result?;
    Ok(selected)
}
