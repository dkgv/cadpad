//! Shared application domain: immutable drafts, supervised candidate workers, and
//! serialized revision commits. UI and agent transports call this same core.
pub mod actors;
pub mod annotations;
pub mod artifacts;
pub mod cache;
pub mod collision;
pub mod constraints;
mod construction;
mod containment;
mod display_reuse;
pub mod dynamics;
pub mod events;
pub mod geometry;
mod geometry_cache;
pub mod kinematics;
pub mod motion;
pub mod motion_test_runs;
pub mod motion_tests;
pub mod native_admission;
pub mod origins;
pub mod paths;
pub mod playback;
pub mod portable;
mod portable_recordings;
pub mod project_result;
pub mod projects;
pub mod project_entry;
pub mod recovery;
pub mod runtime;
pub mod tools;
pub mod views;
pub mod worker;
mod worker_cache;
pub mod worker_limits;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;
use worker::{Worker, WorkerConfig};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{code}: {message}")]
    Domain {
        code: String,
        message: String,
        details: Value,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
impl Error {
    pub fn domain(code: &str, message: impl Into<String>) -> Self {
        Self::Domain {
            code: code.into(),
            message: message.into(),
            details: Value::Null,
        }
    }
    pub fn code(&self) -> &str {
        match self {
            Self::Domain { code, .. } => code,
            Self::Io(_) => "IO_ERROR",
            Self::Json(_) => "INVALID_REQUEST",
        }
    }
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Inputs {
    pub files: BTreeMap<String, String>,
    pub parameters: Value,
    pub annotations: Value,
    pub mechanism: Value,
    pub entry_point: String,
    #[serde(default)]
    pub assets: BTreeMap<String, String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
}
impl Default for Inputs {
    fn default() -> Self {
        Self {
            files: BTreeMap::from([(
                "model.py".into(),
                "# Describe a part to get started.\n".into(),
            )]),
            parameters: json!({}),
            annotations: json!([]),
            mechanism: json!({}),
            entry_point: "model.py".into(),
            assets: BTreeMap::new(),
            dependencies: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Revision {
    pub id: String,
    pub parent: Option<String>,
    pub inputs: Inputs,
    pub model: Value,
    pub actor: String,
    #[serde(default)]
    pub restored_from: Option<String>,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Draft {
    pub id: String,
    pub base_revision: String,
    pub source_hashes: BTreeMap<String, String>,
    pub inputs: Inputs,
    pub actor: String,
    #[serde(default)]
    pub external_fingerprint: Option<String>,
    #[serde(default)]
    pub python_origin: Option<origins::Origin>,
    #[serde(default)]
    pub restore_revision: Option<String>,
}
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}
#[derive(Clone, Serialize, Deserialize, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    #[default]
    Build,
    RepairDisplay,
}
#[derive(Clone, Serialize, Deserialize, Debug, JsonSchema)]
pub struct Job {
    #[serde(default)]
    pub kind: JobKind,
    pub id: String,
    pub draft_id: String,
    #[serde(default)]
    pub snapshot_id: Option<String>,
    pub actor: String,
    pub state: JobState,
    pub resulting_revision: Option<String>,
    pub diagnostics: Value,
    #[serde(default)]
    pub created_at: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct StoredWorkspace {
    schema_version: u32,
    id: String,
    current: String,
    revisions: BTreeMap<String, Revision>,
    drafts: BTreeMap<String, Draft>,
    jobs: BTreeMap<String, Job>,
    #[serde(default)]
    job_snapshots: BTreeMap<String, Draft>,
    #[serde(default)]
    project: Option<portable::Binding>,
    #[serde(default)]
    python_origin: Option<origins::Origin>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    recordings: BTreeMap<String, playback::Metadata>,
    #[serde(default)]
    recording_imports: BTreeMap<String, portable_recordings::Import>,
    #[serde(default)]
    motion_tests: BTreeMap<String, motion_tests::Saved>,
    #[serde(default)]
    geometry_cache: Value,
}
struct Workspace {
    motion: Option<motion::State>,
    recovery: Arc<Mutex<()>>,
    // Ephemeral identity of the committed display publication, even when a
    // repair produces byte-identical descriptors within the same revision.
    display_epoch: Arc<String>,
    stored: StoredWorkspace,
    worker: Option<Arc<Mutex<Worker>>>,
    worker_last_used: u64,
    cancellation: HashMap<String, watch::Sender<bool>>,
}
/// App-data ownership includes entered cache work after the final Core drops.
/// Explicit unlock is necessary because forked children can briefly retain a
/// duplicate open file description before CLOEXEC closes it. Native cache
/// leases have different lifetime semantics and do not use this wrapper.
struct ProcessLock(std::fs::File);
impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
#[derive(Clone)]
pub struct Core {
    root: PathBuf,
    config: WorkerConfig,
    disk_quota_bytes: u64,
    ram_quota_bytes: u64,
    reserved_ram_bytes: Arc<std::sync::atomic::AtomicU64>,
    workspaces: Arc<Mutex<HashMap<String, Workspace>>>,
    actor_tokens: Arc<std::sync::Mutex<HashMap<String, watch::Sender<bool>>>>,
    _process_lock: Arc<ProcessLock>,
    views: views::ViewService,
    activity: tokio::sync::broadcast::Sender<events::Activity>,
    motion_events: tokio::sync::broadcast::Sender<motion::Snapshot>,
    blob_publication: Arc<tokio::sync::RwLock<()>>,
    display_publication: Arc<tokio::sync::RwLock<()>>,
    cache_collection: Arc<Mutex<()>>,
    display_use: Arc<cache::DisplayUse>,
    construction_slots: Arc<tokio::sync::Semaphore>,
    worker_cache: Arc<worker_cache::WorkerCache>,
    native_admission_gate: Arc<Mutex<()>>,
    motion_test_runs: Arc<motion_test_runs::Runs>,
}
fn id() -> String {
    Uuid::new_v4().to_string()
}
fn hash(source: &str) -> String {
    format!("{:x}", Sha256::digest(source.as_bytes()))
}
fn validate_path(path: &str) -> Result<()> {
    portable::relative(path)?;
    if path.is_empty()
        || path.contains('\\')
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || !path.ends_with(".py")
        || path
            .split('/')
            .any(|s| matches!(s, ".venv" | "revisions" | "assets") || s.starts_with('.'))
    {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Source paths must be relative .py files inside the workspace",
        ));
    }
    Ok(())
}
impl Core {
    pub async fn open(root: PathBuf, config: WorkerConfig) -> Result<Self> {
        Self::open_with_quotas(
            root,
            config,
            cache::DEFAULT_DISK_QUOTA_BYTES,
            cache::DEFAULT_RAM_QUOTA_BYTES,
        )
        .await
    }
    pub async fn open_with_disk_quota(
        root: PathBuf,
        config: WorkerConfig,
        disk_quota_bytes: u64,
    ) -> Result<Self> {
        Self::open_with_quotas(
            root,
            config,
            disk_quota_bytes,
            cache::DEFAULT_RAM_QUOTA_BYTES,
        )
        .await
    }
    pub async fn open_with_quotas(
        root: PathBuf,
        config: WorkerConfig,
        disk_quota_bytes: u64,
        ram_quota_bytes: u64,
    ) -> Result<Self> {
        if disk_quota_bytes == 0 {
            return Err(Error::domain(
                "INVALID_PARAMETER",
                "Disk quota must be positive",
            ));
        }
        if ram_quota_bytes == 0 {
            return Err(Error::domain(
                "INVALID_PARAMETER",
                "RAM quota must be positive",
            ));
        }
        if config.limits.memory_bytes > ram_quota_bytes {
            return Err(Error::domain(
                "INVALID_PARAMETER",
                "Per-worker memory budget cannot exceed the aggregate RAM quota",
            ));
        }
        tokio::fs::create_dir_all(root.join("workspaces")).await?;
        let root = tokio::fs::canonicalize(root).await?;
        let process_lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("app.lock"))?;
        process_lock.try_lock().map_err(|_| {
            Error::domain(
                "APP_UNAVAILABLE",
                "Cadpad application data is already open in another core instance",
            )
        })?;
        let process_lock = ProcessLock(process_lock);
        let mut workspaces = HashMap::new();
        let mut entries = tokio::fs::read_dir(root.join("workspaces")).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path().join("workspace.json");
            if !path.exists() {
                continue;
            }
            let mut stored: StoredWorkspace =
                serde_json::from_slice(&tokio::fs::read(path).await?)?;
            if stored.schema_version != 1 {
                return Err(Error::domain(
                    "UNSUPPORTED_OPERATION",
                    "Unsupported workspace schema",
                ));
            }
            // A process restart cannot resurrect a running native operation.
            for job in stored.jobs.values_mut() {
                if matches!(job.state, JobState::Queued | JobState::Running) {
                    job.state = JobState::Failed;
                    job.diagnostics = json!({"code":"WORKER_UNAVAILABLE","message":"Application restarted during build"});
                }
            }
            let revision = &stored.revisions[&stored.current];
            let inputs = revision.inputs.clone();
            let model = revision.model.clone();
            let motion = tokio::task::spawn_blocking(move || motion::State::new(&inputs, &model))
                .await
                .map_err(|_| Error::domain("INVALID_MECHANISM", "Motion recovery failed"))??;
            workspaces.insert(
                stored.id.clone(),
                Workspace {
                    recovery: Arc::new(Mutex::new(())),
                    display_epoch: Arc::new(id()),
                    motion: Some(motion),
                    stored,
                    worker: None,
                    worker_last_used: 0,
                    cancellation: HashMap::new(),
                },
            );
        }
        let workspaces = Arc::new(Mutex::new(workspaces));
        let worker_cache = Arc::new(worker_cache::WorkerCache::new(&workspaces));
        let core = Self {
            root,
            config,
            disk_quota_bytes,
            ram_quota_bytes,
            reserved_ram_bytes: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            workspaces,
            worker_cache,
            native_admission_gate: Arc::new(Mutex::new(())),
            motion_test_runs: Arc::new(motion_test_runs::Runs::default()),
            actor_tokens: Arc::new(std::sync::Mutex::new(HashMap::new())),
            _process_lock: Arc::new(process_lock),
            views: views::ViewService::default(),
            activity: tokio::sync::broadcast::channel(128).0,
            motion_events: tokio::sync::broadcast::channel(32).0,
            blob_publication: Arc::new(tokio::sync::RwLock::new(())),
            display_publication: Arc::new(tokio::sync::RwLock::new(())),
            cache_collection: Arc::new(Mutex::new(())),
            display_use: Arc::new(cache::DisplayUse::default()),
            construction_slots: Arc::new(tokio::sync::Semaphore::new(
                construction::MAX_CONSTRUCTIONS,
            )),
        };
        let recovered = core
            .workspaces
            .lock()
            .await
            .values()
            .map(|ws| ws.stored.clone())
            .collect::<Vec<_>>();
        for stored in recovered {
            core.persist(&stored).await?;
        }
        // Cache maintenance cannot invalidate authoritative recovery if a cache
        // directory is unreadable. Explicit maintenance still reports the error.
        let _ = core.collect_cache().await;
        Ok(core)
    }
    async fn persist(&self, stored: &StoredWorkspace) -> Result<()> {
        let directory = self.root.join("workspaces").join(&stored.id);
        tokio::fs::create_dir_all(&directory).await?;
        let temporary = directory.join(format!("{}.tmp", id()));
        let bytes = serde_json::to_vec_pretty(stored)?;
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::File::create(&temporary).await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, directory.join("workspace.json")).await?;
        Ok(())
    }
    pub fn disk_usage(&self) -> Result<cache::DiskUsage> {
        cache::managed_disk_usage(&self.root, self.disk_quota_bytes)
    }
    async fn ensure_disk_quota(&self) -> Result<()> {
        let usage = self.disk_usage()?;
        if usage.within_quota {
            return Ok(());
        }
        let _ = self.collect_cache().await;
        let usage = self.disk_usage()?;
        if usage.within_quota {
            return Ok(());
        }
        Err(Error::domain(
            "DISK_QUOTA_EXCEEDED",
            format!(
                "Managed app data uses {} bytes; the hard quota is {} bytes. Free derived cache or raise the quota before starting another native job",
                usage.managed_bytes, usage.quota_bytes
            ),
        ))
    }
    pub async fn create_workspace(&self, actor: &str) -> Result<String> {
        let mut workspaces = self.workspaces.lock().await;
        let title = format!("Untitled {}", workspaces.len() + 1);
        let workspace_id = id();
        let revision_id = id();
        let revision = Revision {
            id: revision_id.clone(),
            parent: None,
            inputs: Inputs::default(),
            model: json!({"parts":[]}),
            actor: actor.into(),
            restored_from: None,
        };
        let stored = StoredWorkspace {
            geometry_cache: Value::Null,
            motion_tests: BTreeMap::new(),
            job_snapshots: BTreeMap::new(),
            recordings: BTreeMap::new(),
            recording_imports: BTreeMap::new(),
            schema_version: 1,
            id: workspace_id.clone(),
            current: revision_id.clone(),
            revisions: BTreeMap::from([(revision_id, revision)]),
            drafts: BTreeMap::new(),
            jobs: BTreeMap::new(),
            project: None,
            python_origin: None,
            title: Some(title),
        };
        self.persist(&stored).await?;
        workspaces.insert(
            workspace_id.clone(),
            Workspace {
                recovery: Arc::new(Mutex::new(())),
                display_epoch: Arc::new(id()),
                motion: None,
                stored,
                worker: None,
                worker_last_used: 0,
                cancellation: HashMap::new(),
            },
        );
        Ok(workspace_id)
    }
    pub async fn list_workspaces(&self) -> Vec<String> {
        self.workspaces.lock().await.keys().cloned().collect()
    }
    /// Admission checks must not hash display buffers or evaluate live motion.
    pub async fn has_workspace(&self, workspace_id: &str) -> bool {
        self.workspaces.lock().await.contains_key(workspace_id)
    }
    pub async fn get_project(&self, workspace_id: &str) -> Result<Value> {
        for _ in 0..3 {
            let checked = self.ensure_display_cache(workspace_id).await?;
            let mut workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != checked.revision
                || !Arc::ptr_eq(&ws.display_epoch, &checked.epoch)
            {
                continue;
            }
            self.display_use.touch(workspace_id, &checked.revision);
            let display_error = checked.error;
            let motion = motion::snapshot(ws)?;
            let recording_fingerprint = playback::metadata_fingerprint(&ws.stored.recordings)?;
            let tests_fingerprint = motion_tests::catalog_fingerprint(&ws.stored.motion_tests)?;
            let revision = &ws.stored.revisions[&ws.stored.current];
            let mut activity = ws.stored.jobs.values().collect::<Vec<_>>();
            activity.sort_by_key(|job| (job.created_at, &job.id));
            if activity.len() > 50 {
                activity.drain(..activity.len() - 50);
            }
            return Ok(
                json!({"workspace_id":workspace_id,"revision":revision.id,"display_generation":ws.display_epoch.as_str(),"display_error":display_error,"files":revision.inputs.files.keys().collect::<Vec<_>>(),
            "parameters":revision.inputs.parameters,"entry_point":revision.inputs.entry_point,"model":revision.model,"activity":activity,"total_jobs":ws.stored.jobs.len(),"protocol_version":1,
            "assets":revision.inputs.assets,"dependencies":revision.inputs.dependencies,"annotations":revision.inputs.annotations,"mechanism":revision.inputs.mechanism,"motion":motion,"external_tracking":ws.stored.project.is_some()||ws.stored.python_origin.is_some(),"recordings":ws.stored.recordings.values().collect::<Vec<_>>(),
            "name":ws.stored.project.as_ref().and_then(|binding|binding.directory.file_name()).map(|name|name.to_string_lossy().to_string()).or_else(||ws.stored.title.clone()),
            "saved_revision":ws.stored.project.as_ref().map(|binding|&binding.saved_revision),
            "dirty":ws.stored.project.as_ref().is_none_or(|binding|binding.saved_revision!=revision.id||binding.recording_fingerprint!=recording_fingerprint||binding.motion_test_fingerprint!=tests_fingerprint)}),
            );
        }
        Err(Error::domain(
            "REVISION_CONFLICT",
            "Project changed while checking display data; retry the context read",
        ))
    }
    pub async fn read_source(
        &self,
        workspace_id: &str,
        version: &str,
        path: &str,
    ) -> Result<Value> {
        validate_path(path)?;
        let workspaces = self.workspaces.lock().await;
        let ws = workspaces
            .get(workspace_id)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        let inputs = if let Some(draft) = ws
            .stored
            .drafts
            .get(version)
            .or_else(|| ws.stored.job_snapshots.get(version))
        {
            &draft.inputs
        } else {
            &ws.stored
                .revisions
                .get(version)
                .ok_or_else(|| Error::domain("REVISION_CONFLICT", "Unknown revision or draft"))?
                .inputs
        };
        let source = inputs
            .files
            .get(path)
            .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown source file"))?;
        Ok(json!(tools::SourceContent {
            path: path.into(),
            source: source.clone(),
            hash: hash(source),
        }))
    }
    pub async fn update_source(
        &self,
        workspace_id: &str,
        base: &str,
        draft_id: Option<&str>,
        changes: BTreeMap<String, String>,
        actor: &str,
    ) -> Result<Draft> {
        for (path, source) in &changes {
            validate_path(path)?;
            if source.len() > 1024 * 1024 {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Source file exceeds 1 MiB",
                ));
            }
        }
        let mut workspaces = self.workspaces.lock().await;
        self.ensure_actor_active(actor)?;
        let ws = workspaces
            .get_mut(workspace_id)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        if ws.stored.current != base {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Expected base is no longer current",
            ));
        }
        let mut draft = if let Some(draft_id) = draft_id {
            let draft = ws
                .stored
                .drafts
                .get(draft_id)
                .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown draft"))?;
            if draft.actor != actor || draft.base_revision != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Draft ownership or base mismatch",
                ));
            }
            draft.clone()
        } else {
            let inputs = ws.stored.revisions[base].inputs.clone();
            Draft {
                id: id(),
                base_revision: base.into(),
                source_hashes: inputs
                    .files
                    .iter()
                    .map(|(p, s)| (p.clone(), hash(s)))
                    .collect(),
                inputs,
                actor: actor.into(),
                external_fingerprint: None,
                python_origin: None,
                restore_revision: None,
            }
        };
        // Running builds own clones, so later edits never mutate running inputs.
        if !changes.is_empty() {
            draft.restore_revision = None;
        }
        draft.inputs.files.extend(changes);
        portable::validate_inputs(&draft.inputs)?;
        let mut next = ws.stored.clone();
        next.drafts.insert(draft.id.clone(), draft.clone());
        self.persist(&next).await?;
        ws.stored = next;
        Ok(draft)
    }
    pub async fn build(
        &self,
        workspace_id: &str,
        base: &str,
        draft_id: &str,
        actor: &str,
    ) -> Result<Job> {
        self.queue_job(workspace_id, base, draft_id, actor, JobKind::Build)
            .await
    }
    pub(crate) async fn queue_job(
        &self,
        workspace_id: &str,
        base: &str,
        draft_id: &str,
        actor: &str,
        kind: JobKind,
    ) -> Result<Job> {
        let (job, snapshot, receive) = {
            let mut workspaces = self.workspaces.lock().await;
            self.ensure_actor_active(actor)?;
            let ws = workspaces
                .get(workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Expected base is no longer current",
                ));
            }
            let draft = ws
                .stored
                .drafts
                .get(draft_id)
                .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown draft"))?;
            if draft.base_revision != base || draft.actor != actor {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Draft ownership or base mismatch",
                ));
            }
            construction::check_job_capacity(&workspaces, workspace_id, actor)?;
            let mut snapshot = draft.clone();
            snapshot.id = id();
            let job = Job {
                kind: kind.clone(),
                snapshot_id: Some(snapshot.id.clone()),
                id: id(),
                draft_id: draft.id.clone(),
                actor: actor.into(),
                state: JobState::Queued,
                resulting_revision: None,
                diagnostics: Value::Null,
                created_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            };
            let (send, receive) = watch::channel(false);
            let ws = workspaces.get_mut(workspace_id).unwrap();
            let mut next = ws.stored.clone();
            next.job_snapshots
                .insert(snapshot.id.clone(), snapshot.clone());
            next.jobs.insert(job.id.clone(), job.clone());
            self.persist(&next).await?;
            ws.stored = next;
            ws.cancellation.insert(job.id.clone(), send);
            (job, snapshot, receive)
        };
        self.job_activity(workspace_id, &job);
        let core = self.clone();
        let workspace_id = workspace_id.to_string();
        let job_id = job.id.clone();
        tokio::spawn(async move {
            if kind == JobKind::Build {
                core.run_build(&workspace_id, &job_id, snapshot, receive)
                    .await;
            } else {
                core.run_display_repair(&workspace_id, &job_id, snapshot, receive)
                    .await;
            }
        });
        Ok(job)
    }
    pub(crate) async fn materialize(
        &self,
        workspace_id: &str,
        build_id: &str,
        inputs: &Inputs,
    ) -> Result<cache::Run> {
        portable::validate_inputs(inputs)?;
        self.validate_asset_budget(inputs).await?;
        let root = self.root.join("runs").join(workspace_id).join(build_id);
        let run = {
            // Do not expose a newly created lease file to collection before
            // its first shared lock is held. The returned Run then pins it.
            let _publication = self.display_publication.read().await;
            cache::Run::new(root)?
        };
        let root = run.source.clone();
        for (path, source) in &inputs.files {
            validate_path(path)?;
            let path = root.join(path);
            tokio::fs::create_dir_all(path.parent().unwrap()).await?;
            tokio::fs::write(path, source).await?;
        }
        for (name, digest) in &inputs.assets {
            portable::relative(name)?;
            if !portable::valid_hash(digest) {
                return Err(Error::domain("INVALID_REQUEST", "Invalid asset digest"));
            }
            let blob = tokio::fs::read(self.root.join("blobs").join(digest)).await?;
            if portable::bytes_hash(&blob) != *digest {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Managed asset digest mismatch",
                ));
            }
            let target = root.join("assets").join(name);
            tokio::fs::create_dir_all(target.parent().unwrap()).await?;
            tokio::fs::write(target, blob).await?;
        }
        Ok(run)
    }
    async fn run_build(
        &self,
        workspace_id: &str,
        job_id: &str,
        draft: Draft,
        cancel: watch::Receiver<bool>,
    ) {
        let revision_id = id();
        let mut slot = None;
        let mut recording_publication = None;
        let outcome: Result<(
            Option<Worker>,
            Value,
            motion::State,
            Vec<portable_recordings::Rebinding>,
        )> = async {
            {
                let workspaces = self.workspaces.lock().await;
                let ws = workspaces.get(workspace_id).unwrap();
                if ws.stored.current != draft.base_revision {
                    return Err(Error::domain(
                        "REVISION_CONFLICT",
                        "Revision changed while waiting for geometry capacity",
                    ));
                }
            }
            let _ = self.collect_cache().await;
            self.ensure_disk_quota().await?;
            if *cancel.borrow() {
                return Err(Error::domain(
                    "CANCELLED",
                    "Cancelled before native execution",
                ));
            }
            {
                let mut workspaces = self.workspaces.lock().await;
                let ws = workspaces.get_mut(workspace_id).unwrap();
                if ws.stored.current != draft.base_revision {
                    return Err(Error::domain(
                        "REVISION_CONFLICT",
                        "Revision changed while waiting for native capacity",
                    ));
                }
                if *cancel.borrow() {
                    return Err(Error::domain(
                        "CANCELLED",
                        "Cancelled before native execution",
                    ));
                }
                ws.stored.jobs.get_mut(job_id).unwrap().state = JobState::Running;
                self.job_activity(workspace_id, &ws.stored.jobs[job_id]);
            }
            if let Some(reused) = self
                .try_authoritative_rebuild(
                    workspace_id,
                    &draft.base_revision,
                    &revision_id,
                    &draft,
                    cancel.clone(),
                )
                .await?
            {
                recording_publication = Some(self.blob_publication.read().await);
                return Ok((None, reused.result, reused.motion, reused.rebound));
            }
            if let Some(reused) = self
                .try_annotation_only_rebuild(
                    workspace_id,
                    &draft.base_revision,
                    &revision_id,
                    &draft,
                    cancel.clone(),
                )
                .await?
            {
                recording_publication = Some(self.blob_publication.read().await);
                return Ok((None, reused.result, reused.motion, reused.rebound));
            }
            // Acquire both capacities without retaining one while waiting for
            // the other, including warm repair's construction-only path.
            let (admission, construction) = self.native_construction(cancel.clone()).await?;
            slot = Some(construction);
            {
                let workspaces = self.workspaces.lock().await;
                let ws = workspaces.get(workspace_id).unwrap();
                if ws.stored.current != draft.base_revision {
                    return Err(Error::domain(
                        "REVISION_CONFLICT",
                        "Revision changed while waiting for geometry capacity",
                    ));
                }
            }
            let run = self.materialize(workspace_id, job_id, &draft.inputs).await?;
            let source_root = run.source.clone();
            let cache = self
                .root
                .join("artifacts")
                .join(workspace_id)
                .join(&revision_id);
            let mut worker = self
                .spawn_managed_worker(cache, run, admission, cancel.clone())
                .await?;
            let result = self
                .build_display_candidate(
                    &mut worker,
                    workspace_id,
                    &draft.base_revision,
                    &revision_id,
                    json!({"source_root":source_root,"entry_point":draft.inputs.entry_point,"parameters":draft.inputs.parameters,"annotations":draft.inputs.annotations,"collision":draft.inputs.mechanism.get("collision"),"expected_dependencies":draft.inputs.dependencies,"allow_empty":draft.inputs.files.get(&draft.inputs.entry_point).is_some_and(|source| source.trim()=="# Describe a part to get started.") && draft.inputs.parameters==json!({}),"input_digest":portable::fingerprint(&draft.inputs)?}),
                    cancel.clone(),
                )
                .await;
            match result {
                Ok(result) => {
                    if let Err(error) = recovery::validate_publication(&result["data"])
                        .and_then(|()| recovery::validate_display_publication(&result["data"]))
                        .and_then(|()| project_result::validate_publication(&result["data"]))
                    {
                        let _ = worker.stop().await;
                        return Err(error);
                    }
                    let inputs = draft.inputs.clone();
                    let model = result["data"].clone();
                    let validation = tokio::task::spawn_blocking(move || {
                        motion::State::new(&inputs, &model)
                    })
                    .await
                    .map_err(|_| Error::domain("INVALID_MECHANISM", "Motion validation failed"))
                    .and_then(|result| result);
                    match validation {
                        Ok(motion) => {
                            recording_publication = Some(self.blob_publication.read().await);
                            let (recordings, imports) = {
                                let workspaces = self.workspaces.lock().await;
                                let ws = &workspaces[workspace_id];
                                (
                                    ws.stored.recordings.clone(),
                                    ws.stored.recording_imports.clone(),
                                )
                            };
                            let mut inputs = draft.inputs.clone();
                            inputs.parameters = result["data"]["parameters"].clone();
                            inputs.dependencies = serde_json::from_value(
                                result["data"]["dependencies"].clone(),
                            )
                            .unwrap_or_default();
                            let model = result["data"].clone();
                            let blobs = self.root.join("blobs");
                            let base = draft.base_revision.clone();
                            let target = revision_id.clone();
                            let prepared = tokio::task::spawn_blocking(move || {
                                let rebound = portable_recordings::rebind(
                                    &blobs,
                                    portable_recordings::Rebuild {
                                        base: &base,
                                        revision: &target,
                                        inputs: &inputs,
                                        model: &model,
                                        mechanism: &motion.mechanism,
                                    },
                                    &recordings,
                                    &imports,
                                )?;
                                Ok::<_, Error>((motion, rebound))
                            })
                            .await
                            .map_err(|_| {
                                Error::domain(
                                    "INVALID_RECORDING",
                                    "Recording import validation task failed",
                                )
                            })
                            .and_then(|r| r);
                            match prepared {
                                Ok((motion, rebound)) => {
                                    Ok((Some(worker), result, motion, rebound))
                                }
                                Err(error) => {
                                    let _ = worker.stop().await;
                                    Err(error)
                                }
                            }
                        }
                        Err(error) => {
                            let _ = worker.stop().await;
                            Err(error)
                        }
                    }
                }
                Err(error) => {
                    let _ = worker.stop().await;
                    Err(error)
                }
            }
        }
        .await;
        let mut workspaces = self.workspaces.lock().await;
        let ws = workspaces.get_mut(workspace_id).unwrap();
        let mut next = ws.stored.clone();
        let mut candidate = None;
        let mut candidate_motion = None;
        let final_result = match outcome {
            Ok((worker, result, motion, rebound))
                if !*cancel.borrow() && next.current == draft.base_revision =>
            {
                if result["authoritative_reuse"].is_null() || !result["geometry_cache"].is_null()
                {
                    next.geometry_cache = geometry_cache::record(
                        &next.geometry_cache,
                        &revision_id,
                        &portable::fingerprint(&draft.inputs).unwrap_or_default(),
                        &result["geometry_cache"],
                    );
                }
                let mut inputs = draft.inputs;
                inputs.parameters = result["data"]["parameters"].clone();
                inputs.dependencies =
                    serde_json::from_value(result["data"]["dependencies"].clone())
                        .unwrap_or_default();
                let revision = Revision {
                    id: revision_id.clone(),
                    parent: Some(draft.base_revision.clone()),
                    inputs,
                    model: result["data"].clone(),
                    actor: draft.actor,
                    restored_from: draft.restore_revision,
                };
                for item in rebound {
                    if next.recordings.get(&item.id).is_some_and(|m| {
                        m.digest == item.old_digest && m.revision == draft.base_revision
                    }) && next.recording_imports.get(&item.id).is_some_and(|i| {
                        i.pending_revision.as_deref() == Some(&draft.base_revision)
                    }) {
                        next.recordings.insert(item.id, item.metadata);
                    }
                }
                // A changed first build leaves imported recordings archived and
                // stale. Ordinary rebuilds/undo never silently retarget them.
                for import in next.recording_imports.values_mut() {
                    if import.pending_revision.as_deref() == Some(&draft.base_revision) {
                        import.pending_revision = None;
                    }
                }
                if let Some(fingerprint) = draft.external_fingerprint
                    && let Some(binding) = &mut next.project
                {
                    binding.fingerprint = fingerprint.clone();
                    if portable::fingerprint(&revision.inputs).ok().as_ref() == Some(&fingerprint) {
                        binding.saved_revision = revision_id.clone();
                    }
                }
                if let Some(binding) = &mut next.project
                    && portable::fingerprint(&revision.inputs).ok().as_ref()
                        == Some(&binding.fingerprint)
                {
                    binding.saved_revision = revision_id.clone();
                }
                if let Some(mut origin) = draft.python_origin {
                    origin.applied = true;
                    next.python_origin = Some(origin);
                } else if let Some(origin) = &mut next.python_origin {
                    origin.applied = true;
                }
                next.revisions.insert(revision_id.clone(), revision);
                next.current = revision_id.clone();
                candidate = worker;
                candidate_motion = Some(motion);
                Ok(result)
            }
            Ok((worker, _, _, _)) => {
                let stopped = match worker {
                    Some(mut worker) => worker.stop().await,
                    None => Ok(()),
                };
                if let Err(error) = stopped {
                    Err(error)
                } else if *cancel.borrow() {
                    Err(Error::domain(
                        "CANCELLED",
                        "Cancelled before commit; worker reaped",
                    ))
                } else {
                    Err(Error::domain(
                        "REVISION_CONFLICT",
                        "Another operation committed during build",
                    ))
                }
            }
            Err(error) => Err(error),
        };
        let job = next.jobs.get_mut(job_id).unwrap();
        match final_result {
            Ok(result) => {
                job.state = JobState::Succeeded;
                job.resulting_revision = Some(revision_id);
                job.diagnostics =
                    json!({"logs":result["logs"],"logs_truncated":result["logs_truncated"]});
            }
            Err(error) => {
                job.state = if error.code() == "CANCELLED" {
                    JobState::Cancelled
                } else {
                    JobState::Failed
                };
                job.diagnostics = error_value(&error);
            }
        }
        match self.persist(&next).await {
            Ok(()) => {
                ws.stored = next;
                if let Some(worker) = candidate {
                    ws.motion = candidate_motion;
                    ws.worker = Some(Arc::new(Mutex::new(worker)));
                    ws.worker_last_used = self.worker_cache.next_use();
                }
            }
            Err(error) => {
                let job = ws.stored.jobs.get_mut(job_id).unwrap();
                job.state = JobState::Failed;
                job.diagnostics = error_value(&error);
            }
        }
        self.display_use.touch(workspace_id, &ws.stored.current);
        ws.cancellation.remove(job_id);
        self.job_activity(workspace_id, &ws.stored.jobs[job_id]);
        drop(workspaces);
        // Publication and terminal job persistence finish before the next
        // queued constructor can inspect the newly committed base revision.
        drop(slot);
        self.worker_cache.schedule();
    }
    pub async fn get_job(&self, workspace_id: &str, job_id: &str) -> Result<Job> {
        let workspaces = self.workspaces.lock().await;
        workspaces
            .get(workspace_id)
            .and_then(|ws| ws.stored.jobs.get(job_id))
            .cloned()
            .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown workspace/job"))
    }
    pub async fn cancel_job(&self, workspace_id: &str, job_id: &str) -> Result<Job> {
        let workspaces = self.workspaces.lock().await;
        let ws = workspaces
            .get(workspace_id)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        let job = ws
            .stored
            .jobs
            .get(job_id)
            .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown job"))?;
        if let Some(send) = ws.cancellation.get(job_id) {
            let _ = send.send(true);
        }
        // Report running until the supervisor has actually reaped the process.
        Ok(job.clone())
    }
    pub async fn query(
        &self,
        workspace_id: &str,
        revision: &str,
        method: &str,
        args: Value,
    ) -> Result<Value> {
        let (_send, receive) = watch::channel(false);
        self.query_with_cancel(workspace_id, revision, method, args, receive)
            .await
    }
    pub async fn query_as_actor(
        &self,
        actor: &str,
        workspace_id: &str,
        revision: &str,
        method: &str,
        args: Value,
    ) -> Result<Value> {
        self.ensure_actor_active(actor)?;
        self.query_with_cancel(
            workspace_id,
            revision,
            method,
            args,
            self.actor_signal(actor),
        )
        .await
    }
    async fn query_with_cancel(
        &self,
        workspace_id: &str,
        revision: &str,
        method: &str,
        args: Value,
        receive: watch::Receiver<bool>,
    ) -> Result<Value> {
        let mut args = args;
        let pose_version = self.query_pose(workspace_id, revision, &mut args).await?;
        if !matches!(method, "inspect" | "export") {
            return Err(Error::domain(
                "UNSUPPORTED_OPERATION",
                "Unknown geometry query",
            ));
        }
        let worker = self
            .verified_worker(workspace_id, revision, receive.clone())
            .await?;
        let mut waiting = receive.clone();
        if *waiting.borrow() {
            return Err(Error::domain("CANCELLED", "Query cancelled"));
        }
        let mut native = tokio::select! {guard=worker.lock()=>guard,_=waiting.changed()=>return Err(Error::domain("CANCELLED","Cancelled while waiting for native geometry"))};
        let result = native.request(revision, method, args, receive).await;
        drop(native);
        if let Err(error) = &result
            && matches!(
                error.code(),
                "CANCELLED" | "WORKER_UNAVAILABLE" | "IO_ERROR" | "RESOURCE_LIMIT"
            )
        {
            let mut workspaces = self.workspaces.lock().await;
            let ws = workspaces.get_mut(workspace_id).unwrap();
            if ws
                .worker
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &worker))
            {
                ws.worker = None;
            }
        }
        let result = result?;
        // Caller can still receive useful old data, but it must never be published as current.
        let workspaces = self.workspaces.lock().await;
        if workspaces[workspace_id].stored.current != revision {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Revision changed during query",
            ));
        }
        if let Some(pose_version) = pose_version {
            drop(workspaces);
            if self.get_motion(workspace_id, revision).await?.pose_version != pose_version {
                return Err(Error::domain(
                    "POSE_CONFLICT",
                    "Pose changed during exact operation; retry at the settled pose",
                ));
            }
        }
        Ok(result["data"].clone())
    }
    pub async fn artifact(
        &self,
        workspace_id: &str,
        revision: &str,
        handle: &str,
    ) -> Result<Vec<u8>> {
        self.read_artifact(workspace_id, revision, handle).await
    }

    pub async fn set_parameter(
        &self,
        workspace_id: &str,
        base: &str,
        parameter: &str,
        value: f64,
        unit: &str,
        actor: &str,
    ) -> Result<Job> {
        if !value.is_finite() {
            return Err(Error::domain(
                "INVALID_PARAMETER",
                "Parameter must be finite",
            ));
        }
        let draft = {
            let mut workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Expected revision no longer current",
                ));
            }
            let mut inputs = ws.stored.revisions[base].inputs.clone();
            let specification = inputs
                .parameters
                .get_mut(parameter)
                .ok_or_else(|| Error::domain("INVALID_PARAMETER", "Unknown published parameter"))?;
            if specification["unit"] != unit {
                return Err(Error::domain(
                    "INVALID_PARAMETER",
                    "Unit must match the published parameter",
                ));
            }
            if specification["minimum"]
                .as_f64()
                .is_some_and(|bound| value < bound)
                || specification["maximum"]
                    .as_f64()
                    .is_some_and(|bound| value > bound)
            {
                return Err(Error::domain(
                    "INVALID_PARAMETER",
                    "Parameter outside published bounds",
                ));
            }
            specification["value"] = json!(value);
            let draft = Draft {
                id: id(),
                base_revision: base.into(),
                source_hashes: inputs
                    .files
                    .iter()
                    .map(|(p, s)| (p.clone(), hash(s)))
                    .collect(),
                inputs,
                actor: actor.into(),
                external_fingerprint: None,
                python_origin: None,
                restore_revision: None,
            };
            let mut next = ws.stored.clone();
            next.drafts.insert(draft.id.clone(), draft.clone());
            self.persist(&next).await?;
            ws.stored = next;
            draft
        };
        self.build(workspace_id, base, &draft.id, actor).await
    }
    pub async fn restore_checkpoint(&self, workspace: &str, expected: &str, target: &str) -> Result<Job> {
        let draft = {
            let mut all = self.workspaces.lock().await;
            let ws = all.get_mut(workspace).ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != expected { return Err(Error::domain("REVISION_CONFLICT", "The model changed. Try reverting again.")); }
            let revision = ws.stored.revisions.get(target).ok_or_else(|| Error::domain("INVALID_REQUEST", "Restore point is unavailable"))?;
            let mut draft = Self::input_draft(revision.inputs.clone(), expected, "local-user");
            draft.restore_revision = Some(target.into());
            let mut next = ws.stored.clone();
            next.drafts.insert(draft.id.clone(), draft.clone());
            self.persist(&next).await?;
            ws.stored = next;
            draft
        };
        self.build(workspace, expected, &draft.id, "local-user").await
    }
    pub async fn undo(&self, workspace_id: &str, expected: &str, actor: &str) -> Result<Job> {
        let draft = {
            let mut workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != expected {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Expected revision no longer current",
                ));
            }
            let current = &ws.stored.revisions[expected];
            let cursor = current.restored_from.as_deref().unwrap_or(expected);
            let mut previous = ws.stored.revisions[cursor].parent.as_deref();
            while let Some(target) = previous {
                if portable::fingerprint(&ws.stored.revisions[target].inputs)?
                    != portable::fingerprint(&current.inputs)?
                {
                    break;
                }
                let cursor = ws.stored.revisions[target]
                    .restored_from
                    .as_deref()
                    .unwrap_or(target);
                previous = ws.stored.revisions[cursor].parent.as_deref();
            }
            let previous = previous
                .ok_or_else(|| Error::domain("UNSUPPORTED_OPERATION", "No previous revision"))?;
            let restore_revision = ws.stored.revisions[previous]
                .restored_from
                .clone()
                .unwrap_or_else(|| previous.to_string());
            let inputs = ws.stored.revisions[previous].inputs.clone();
            let draft = Draft {
                id: id(),
                base_revision: expected.into(),
                source_hashes: current
                    .inputs
                    .files
                    .iter()
                    .map(|(p, s)| (p.clone(), hash(s)))
                    .collect(),
                inputs,
                actor: actor.into(),
                external_fingerprint: None,
                python_origin: None,
                restore_revision: Some(restore_revision),
            };
            let mut next = ws.stored.clone();
            next.drafts.insert(draft.id.clone(), draft.clone());
            self.persist(&next).await?;
            ws.stored = next;
            draft
        };
        // Undo is another candidate build. The visible model survives failed recovery.
        self.build(workspace_id, expected, &draft.id, actor).await
    }
}
pub fn error_value(error: &Error) -> Value {
    match error {
        Error::Domain {
            code,
            message,
            details,
        } => json!({"code":code,"message":message,"details":details}),
        _ => json!({"code":error.code(),"message":error.to_string()}),
    }
}

#[cfg(test)]
mod process_lock_tests {
    use super::*;
    #[tokio::test]
    async fn app_lock_releases_with_its_owner_even_when_a_duplicate_description_survives() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let config = WorkerConfig::bundled("unused".into());
        let core = Core::open(root.clone(), config.clone()).await.unwrap();
        let owner = core.clone();
        // dup shares the same lock description, as fork does before exec. No
        // subprocess scheduling or retry loop is needed to reproduce this race.
        let inherited = core._process_lock.0.try_clone().unwrap();
        drop(core);
        assert!(
            matches!(Core::open(root.clone(),config.clone()).await,Err(error) if error.code()=="APP_UNAVAILABLE")
        );
        drop(owner);
        let reopened = Core::open(root.clone(), config.clone()).await.unwrap();
        // Dropping the old descriptor must not unlock the new owner's lock.
        drop(inherited);
        assert!(
            matches!(Core::open(root,config).await,Err(error) if error.code()=="APP_UNAVAILABLE")
        );
        drop(reopened);
    }
}
