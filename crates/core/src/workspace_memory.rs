//! Closed workspaces retain only catalog and cache-reachability metadata.
//! Source, publication history and native state are loaded on explicit access.
use crate::{Core, Error, Job, JobState, Result, StoredWorkspace, Workspace};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::BufReader,
    path::Path,
    sync::Arc,
};
use tokio::sync::{Mutex, MutexGuard};

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Catalog {
    pub has_content: bool,
    pub blobs: HashSet<String>,
    pub meshes: Vec<(String, HashSet<String>)>,
    pub protected: bool,
}

impl Catalog {
    pub fn from_stored(stored: &StoredWorkspace) -> Self {
        let current = stored
            .revisions
            .header(&stored.current)
            .expect("current snapshot header");
        let has_content = current.has_content
            || !stored.jobs.is_empty()
            || !stored.recordings.is_empty()
            || !stored.motion_tests.is_empty();
        let mut blobs = HashSet::new();
        for (_, summary) in stored
            .revisions
            .headers()
            .chain(stored.drafts.headers())
            .chain(stored.job_snapshots.headers())
        {
            blobs.extend(summary.assets.values().cloned());
        }
        blobs.extend(stored.recordings.values().map(|r| r.digest.clone()));
        Self::with_geometry(
            has_content,
            blobs,
            current.mesh_handles.clone(),
            stored,
            current.protected,
        )
    }
    fn with_geometry(
        has_content: bool,
        blobs: HashSet<String>,
        mut handles: HashSet<String>,
        stored: &StoredWorkspace,
        protected: bool,
    ) -> Self {
        handles.extend(crate::geometry_cache::handles(
            &stored.geometry_cache,
            &stored.current,
        ));
        let mut meshes = vec![(stored.current.clone(), handles)];
        for revision in crate::geometry_cache::retained_revisions(&stored.geometry_cache) {
            if revision != stored.current {
                let handles = crate::geometry_cache::handles(&stored.geometry_cache, &revision)
                    .into_iter()
                    .collect();
                meshes.push((revision, handles));
            }
        }
        Self {
            has_content,
            blobs,
            meshes,
            protected,
        }
    }
}

fn stub(stored: &StoredWorkspace) -> StoredWorkspace {
    StoredWorkspace {
        schema_version: stored.schema_version,
        id: stored.id.clone(),
        current: stored.current.clone(),
        model_parent: stored.model_parent.clone(),
        model_source_revision: stored.model_source_revision.clone(),
        title: stored.title.clone(),
        project: stored.project.clone(),
        python_origin: stored.python_origin.clone(),
        revisions: crate::snapshots::Snapshots::default(),
        drafts: crate::snapshots::Snapshots::default(),
        jobs: stored.jobs.clone(),
        job_snapshots: crate::snapshots::Snapshots::default(),
        recordings: BTreeMap::new(),
        recording_imports: BTreeMap::new(),
        motion_tests: BTreeMap::new(),
        geometry_cache: Value::Null,
    }
}

// A derived sidecar avoids rescanning every source/history body on subsequent
// launches. Its exact file identity binds reachability to workspace.json; a
// missing, stale or corrupt sidecar is rebuilt from the authoritative journal.
#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Stamp {
    bytes: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}
impl Stamp {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        Self::from_metadata(std::fs::metadata(path)?)
    }
    pub(crate) fn of(file: &File) -> Result<Self> {
        Self::from_metadata(file.metadata()?)
    }
    fn from_metadata(metadata: std::fs::Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            bytes: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}
#[derive(Serialize, Deserialize)]
pub(crate) struct CatalogFile {
    version: u32,
    stamp: Stamp,
    stored: StoredWorkspace,
    catalog: Catalog,
}
impl CatalogFile {
    pub(crate) fn new(stamp: Stamp, stored: &StoredWorkspace) -> Self {
        Self {
            version: 1,
            stamp,
            stored: stub(stored),
            catalog: Catalog::from_stored(stored),
        }
    }
    pub(crate) fn write(&self, directory: &Path) -> Result<()> {
        use std::io::Write;
        let temporary = directory.join(format!("{}.tmp", crate::id()));
        let mut writer = std::io::BufWriter::new(File::create(&temporary)?);
        serde_json::to_writer(&mut writer, self)?;
        writer.flush()?;
        drop(writer);
        std::fs::rename(temporary, directory.join("workspace.catalog.json"))?;
        Ok(())
    }
}
pub(crate) fn read_catalog(path: &Path) -> Result<Workspace> {
    let stamp = Stamp::read(path)?;
    let directory = path.parent().unwrap();
    let cached = File::open(directory.join("workspace.catalog.json"))
        .ok()
        .and_then(|file| serde_json::from_reader::<_, CatalogFile>(BufReader::new(file)).ok());
    if let Some(mut cached) = cached {
        if cached.version == 1 && cached.stamp == stamp && cached.stored.schema_version == 1 {
            recover_jobs(&mut cached.stored.jobs);
            return Ok(Workspace {
                stored: cached.stored,
                archived: Some(cached.catalog),
                memory: Arc::new(()),
                saved_pose: None,
                motion: None,
                recovery: Arc::new(Mutex::new(())),
                display_epoch: Arc::new(crate::id()),
                worker: None,
                worker_last_used: 0,
                cancellation: HashMap::new(),
            });
        }
    }
    let mut workspace = read_catalog_projection(path)?;
    if Stamp::read(path)? != stamp {
        return Err(Error::domain(
            "IO_ERROR",
            "Workspace changed while reading its catalog",
        ));
    }
    let file = CatalogFile {
        version: 1,
        stamp,
        stored: stub(&workspace.stored),
        catalog: workspace.archived.take().unwrap(),
    };
    let _ = file.write(directory);
    workspace.archived = Some(file.catalog);
    Ok(workspace)
}

fn read_catalog_projection(path: &Path) -> Result<Workspace> {
    let stored = crate::snapshots::open(path)?;
    if stored.schema_version != 1 || !stored.revisions.contains_key(&stored.current) {
        return Err(Error::domain("INVALID_REQUEST", "Invalid workspace index"));
    }
    let catalog = Catalog::from_stored(&stored);
    let mut stored = stub(&stored);
    recover_jobs(&mut stored.jobs);
    Ok(Workspace {
        stored,
        archived: Some(catalog),
        memory: Arc::new(()),
        saved_pose: None,
        motion: None,
        recovery: Arc::new(Mutex::new(())),
        display_epoch: Arc::new(crate::id()),
        worker: None,
        worker_last_used: 0,
        cancellation: HashMap::new(),
    })
}

fn recover_jobs(jobs: &mut BTreeMap<String, Job>) -> bool {
    let mut changed = false;
    for job in jobs.values_mut() {
        if matches!(job.state, JobState::Queued | JobState::Running) {
            changed = true;
            job.state = JobState::Failed;
            job.diagnostics = serde_json::json!({"code":"WORKER_UNAVAILABLE","message":"Application restarted during build"});
        }
    }
    changed
}

impl Workspace {
    pub(crate) fn archive(&mut self) {
        if self.archived.is_some()
            || Arc::strong_count(&self.memory) != 1
            || self.worker.is_some()
            || !self.cancellation.is_empty()
            || Arc::strong_count(&self.recovery) != 1
            || self.motion.as_ref().is_some_and(|s| {
                s.simulation.is_some() || s.recorder.is_some() || s.playback.is_some()
            })
        {
            return;
        }
        self.saved_pose = self
            .motion
            .as_ref()
            .map(|state| state.idle_pose(&self.stored.current))
            .or_else(|| self.saved_pose.take());
        self.archived = Some(Catalog::from_stored(&self.stored));
        self.stored = stub(&self.stored);
        self.motion = None;
    }
}

impl Core {
    /// Hydration and mutation share the existing workspace lock. Recovery and
    /// native-use leases prevent eviction while an operation is in flight.
    pub(crate) async fn workspace(
        &self,
        id: &str,
    ) -> Result<MutexGuard<'_, HashMap<String, Workspace>>> {
        let mut all = self.workspace_metadata(id).await?;
        if all[id]
            .stored
            .revisions
            .contains_key(&all[id].stored.current)
        {
            all.get_mut(id).unwrap().stored.retain_current().await?;
        }
        Ok(all)
    }
    pub(crate) async fn workspace_metadata(
        &self,
        id: &str,
    ) -> Result<MutexGuard<'_, HashMap<String, Workspace>>> {
        let mut all = self.workspaces.lock().await;
        let ws = all
            .get_mut(id)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        if ws.archived.is_some() {
            let path = self.root.join("workspaces").join(id).join("workspace.json");
            let mut stored: StoredWorkspace =
                tokio::task::spawn_blocking(move || -> Result<_> { crate::snapshots::open(&path) })
                    .await
                    .map_err(|e| Error::domain("IO_ERROR", e.to_string()))??;
            if stored.id != id
                || stored.schema_version != 1
                || !stored.revisions.contains_key(&stored.current)
            {
                return Err(Error::domain("INVALID_REQUEST", "Invalid saved workspace"));
            }
            if recover_jobs(&mut stored.jobs) {
                self.persist(&stored).await?;
            }
            ws.stored = stored;
            ws.archived = None;
        }
        Ok(all)
    }
}

/// A full operation, including time spent outside the workspace mutex, keeps
/// its history and pose alive. Dropping the final lease retries hidden cleanup.
pub(crate) struct WorkspaceUse {
    memory: Option<Arc<()>>,
    cache: Arc<crate::worker_cache::WorkerCache>,
}
impl Drop for WorkspaceUse {
    fn drop(&mut self) {
        // Release the pin before scheduling, including on future cancellation.
        drop(self.memory.take());
        self.cache.schedule();
    }
}
impl Core {
    pub(crate) async fn pin_workspace(&self, id: &str) -> Result<WorkspaceUse> {
        let all = self.workspace_metadata(id).await?;
        Ok(WorkspaceUse {
            memory: Some(all[id].memory.clone()),
            cache: self.worker_cache.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Inputs, Revision, worker::WorkerConfig};

    async fn core(root: &Path) -> Core {
        Core::open(root.to_path_buf(), WorkerConfig::bundled("unused".into()))
            .await
            .unwrap()
    }
    async fn wait_archived(core: &Core, id: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if core.workspaces.lock().await[id].archived.is_some() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("closed workspace should release full history");
    }
    #[tokio::test]
    async fn disk_catalog_refresh_does_not_hydrate_archived_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("project");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("model.py"), "# source\n").unwrap();
        crate::portable::initialize_folder(&folder, "model.py").unwrap();
        let core = core(&temp.path().join("app")).await;
        let project = core.open_project(folder, "test").await.unwrap();
        let id = &project.workspace_id;
        core.retain_workspace_memory(vec![]);
        wait_archived(&core, id).await;

        let sources = BTreeMap::from([("model.py", "# source\n")]);
        let fingerprint =
            crate::hash(&serde_json::to_string(&(sources, Vec::<String>::new())).unwrap());
        let result = core
            .project_source_catalog(id, &project.base_revision, Some(&fingerprint))
            .await
            .unwrap();
        assert_eq!(result["unchanged"], true);
        assert!(core.workspaces.lock().await[id].archived.is_some());
    }

    #[test]
    fn desktop_visibility_updates_schedule_cleanup_outside_a_runtime_context() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let core = runtime.block_on(core(&temp.path().join("app")));
        let id = runtime.block_on(core.create_workspace("test")).unwrap();
        // Synchronous desktop commands run without an entered Tokio handle.
        core.retain_workspace_memory(vec![]);
        runtime.block_on(wait_archived(&core, &id));
    }
    #[tokio::test]
    async fn startup_and_recent_lists_keep_histories_unloaded_and_source_reads_restore_only_the_requested_workspace()
     {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let original = core(&root).await;
        let first = original.create_workspace("test").await.unwrap();
        let second = original.create_workspace("test").await.unwrap();
        let (old, current) = {
            let mut all = original.workspaces.lock().await;
            let ws = all.get_mut(&first).unwrap();
            let old = ws.stored.current.clone();
            ws.stored
                .revisions
                .resident_mut(&old)
                .unwrap()
                .inputs
                .files
                .insert("model.py".into(), "# history".into());
            let current = crate::id();
            let mut inputs = Inputs::default();
            inputs.files.insert("model.py".into(), "# current".into());
            ws.stored.revisions.insert(
                current.clone(),
                Revision {
                    id: current.clone(),
                    parent: Some(old.clone()),
                    inputs,
                    model: serde_json::json!({"parts":[]}),
                    actor: "test".into(),
                    restored_from: None,
                },
            );
            ws.stored.current = current.clone();
            original.persist(&ws.stored).await.unwrap();
            (old, current)
        };
        let before =
            std::fs::read(root.join("workspaces").join(&first).join("workspace.json")).unwrap();
        drop(original);
        let reopened = core(&root).await;
        assert_eq!(reopened.list_workspaces().await.len(), 2);
        assert_eq!(
            reopened
                .workspace_summaries()
                .await
                .as_array()
                .unwrap()
                .len(),
            1
        );
        {
            let all = reopened.workspaces.lock().await;
            assert!(all.values().all(|ws| ws.archived.is_some()
                && ws.stored.revisions.is_empty()
                && ws.motion.is_none()));
        }
        assert_eq!(
            reopened
                .read_source(&first, &old, "model.py")
                .await
                .unwrap()["source"],
            "# history"
        );
        assert_eq!(
            reopened
                .read_source(&first, &current, "model.py")
                .await
                .unwrap()["source"],
            "# current"
        );
        assert!(reopened.workspaces.lock().await[&second].archived.is_some());
        reopened.retain_workspace_memory(vec![]);
        wait_archived(&reopened, &first).await;
        assert_eq!(
            reopened
                .read_source(&first, &old, "model.py")
                .await
                .unwrap()["source"],
            "# history"
        );
        wait_archived(&reopened, &first).await;
        assert_eq!(
            before,
            std::fs::read(root.join("workspaces").join(&first).join("workspace.json")).unwrap()
        );
    }
    #[tokio::test]
    async fn closing_waits_for_operations_and_reloads_durable_drafts() {
        let temp = tempfile::tempdir().unwrap();
        let core = core(&temp.path().join("app")).await;
        let id = core.create_workspace("test").await.unwrap();
        let revision = core.workspaces.lock().await[&id].stored.current.clone();
        let pin = core.pin_workspace(&id).await.unwrap();
        core.retain_workspace_memory(vec![]);
        // Run maintenance directly so the assertion does not depend on scheduling.
        core.worker_cache.trim().await;
        assert!(core.workspaces.lock().await[&id].archived.is_none());
        let draft = core
            .update_source(
                &id,
                &revision,
                None,
                BTreeMap::from([("model.py".into(), "# edited draft".into())]),
                "test",
            )
            .await
            .unwrap();
        {
            let mut all = core.workspaces.lock().await;
            crate::motion::state(all.get_mut(&id).unwrap())
                .unwrap()
                .version = 9;
        }
        drop(pin);
        wait_archived(&core, &id).await;
        assert_eq!(
            core.read_source(&id, &draft.id, "model.py").await.unwrap()["source"],
            "# edited draft"
        );
        wait_archived(&core, &id).await;
        assert_eq!(
            core.get_motion(&id, &revision).await.unwrap().pose_version,
            9
        );
        wait_archived(&core, &id).await;
    }
    #[tokio::test]
    async fn catalog_size_does_not_grow_with_source_bodies_or_topology() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let original = core(&root).await;
        let id = original.create_workspace("test").await.unwrap();
        {
            let mut all = original.workspaces.lock().await;
            let ws = all.get_mut(&id).unwrap();
            let mut revision = ws.stored.current_revision().clone();
            revision
                .inputs
                .files
                .insert("model.py".into(), "# source body\n".repeat(30_000));
            revision.model = serde_json::json!({"parts":[{"name":"part", "face_ids":vec!["historical topology entity"; 20_000]}]});
            for _ in 0..4 {
                revision.id = crate::id();
                ws.stored.current = revision.id.clone();
                ws.stored
                    .revisions
                    .insert(revision.id.clone(), revision.clone());
            }
            original.persist(&ws.stored).await.unwrap();
        }
        drop(original);
        let directory = root.join("workspaces").join(&id);
        let path = directory.join("workspace.json");
        assert!(std::fs::metadata(&path).unwrap().len() > 3_000_000);
        std::fs::remove_file(directory.join("workspace.catalog.json")).unwrap();
        let projected = read_catalog(&path).unwrap();
        assert!(projected.stored.revisions.is_empty());
        assert!(projected.archived.as_ref().unwrap().has_content);
        assert!(
            std::fs::metadata(directory.join("workspace.catalog.json"))
                .unwrap()
                .len()
                < 20_000
        );
        let cached = read_catalog(&path).unwrap();
        assert!(cached.stored.revisions.is_empty());
        assert!(cached.archived.as_ref().unwrap().has_content);
    }
    #[tokio::test]
    async fn stale_or_corrupt_catalogs_rebuild_without_loading_history() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let original = core(&root).await;
        let id = original.create_workspace("test").await.unwrap();
        let directory = root.join("workspaces").join(&id);
        drop(original);
        let path = directory.join("workspace.json");
        let mut stored: StoredWorkspace =
            serde_json::from_reader(BufReader::new(File::open(&path).unwrap())).unwrap();
        stored.title = Some("Changed outside the catalog".into());
        let replacement = directory.join("replacement.json");
        serde_json::to_writer(File::create(&replacement).unwrap(), &stored).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let catalog = read_catalog(&path).unwrap();
        assert_eq!(
            catalog.stored.title.as_deref(),
            Some("Changed outside the catalog")
        );
        assert!(catalog.stored.revisions.is_empty());
        std::fs::write(directory.join("workspace.catalog.json"), b"broken cache").unwrap();
        let catalog = read_catalog(&path).unwrap();
        assert_eq!(
            catalog.stored.title.as_deref(),
            Some("Changed outside the catalog")
        );
        assert!(catalog.stored.revisions.is_empty());
    }
    #[tokio::test]
    async fn collection_keeps_blobs_referenced_only_by_archived_history() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let original = core(&root).await;
        let id = original.create_workspace("test").await.unwrap();
        let bytes = b"historical asset";
        let digest = crate::portable::bytes_hash(bytes);
        std::fs::create_dir_all(root.join("blobs")).unwrap();
        std::fs::write(root.join("blobs").join(&digest), bytes).unwrap();
        {
            let mut all = original.workspaces.lock().await;
            let ws = all.get_mut(&id).unwrap();
            let revision = ws.stored.current.clone();
            ws.stored
                .revisions
                .resident_mut(&revision)
                .unwrap()
                .inputs
                .assets
                .insert("asset.stl".into(), digest.clone());
            original.persist(&ws.stored).await.unwrap();
        }
        drop(original);
        let reopened = core(&root).await;
        reopened.collect_cache().await.unwrap();
        assert_eq!(
            std::fs::read(root.join("blobs").join(digest)).unwrap(),
            bytes
        );
        assert!(reopened.workspaces.lock().await[&id].archived.is_some());
    }
}
