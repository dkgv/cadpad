//! Reconstruct native geometry without silently changing a revision or allocating
//! another display mesh. Derived buffers can be repaired from a verified worker.
use crate::{Core, Error, Result, Worker, id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
const FORMAT: &str = "occt-brep-v3-no-display-v1";
pub(crate) struct DisplayCheck {
    pub revision: String,
    pub epoch: Arc<String>,
    pub error: Option<Value>,
}
pub(crate) fn contract(model: &Value) -> Value {
    let mut result = model.clone();
    if let Some(parts) = result["parts"].as_array_mut() {
        for part in parts {
            if let Some(part) = part.as_object_mut() {
                part.remove("mesh");
            }
        }
    }
    result
}
pub(crate) fn validate_publication(model: &Value) -> Result<()> {
    let parts = model["parts"].as_array().ok_or_else(|| {
        Error::domain("INVALID_GEOMETRY", "Worker publication has no parts array")
    })?;
    if parts.iter().any(|part| {
        part["geometry_fingerprint"]["format"] != FORMAT
            || !part["geometry_fingerprint"]["sha256"]
                .as_str()
                .is_some_and(crate::portable::valid_hash)
    }) {
        return Err(Error::domain(
            "WORKER_UNAVAILABLE",
            "Worker lacks the required geometry fingerprint; rebuild the matching worker bundle",
        ));
    }
    Ok(())
}
pub(crate) fn validate_display_publication(model: &Value) -> Result<()> {
    if !model["parts"].as_array().is_some_and(|parts| {
        parts
            .iter()
            .all(|part| part["mesh"]["format"] == crate::artifacts::MESH_FORMAT)
    }) {
        return Err(Error::domain(
            "WORKER_UNAVAILABLE",
            "Worker display format is outdated; rebuild the matching worker bundle",
        ));
    }
    Ok(())
}
fn verify(expected: &Value, actual: &Value) -> Result<()> {
    let before = expected["parts"]
        .as_array()
        .ok_or_else(|| Error::domain("INVALID_GEOMETRY", "Stored parts are invalid"))?;
    if before.is_empty() && actual["parts"].as_array().is_some_and(Vec::is_empty) {
        return Ok(());
    }
    if validate_publication(expected).is_err() {
        return Err(Error::domain(
            "RECOVERY_VERIFICATION_REQUIRED",
            "This revision predates geometry fingerprints. Rebuild it explicitly before native recovery",
        ));
    }
    validate_publication(actual)?;
    if contract(expected) != contract(actual) {
        return Err(Error::domain(
            "RECOVERY_GEOMETRY_CHANGED",
            "Rebuilding the committed inputs produced different geometry or publication metadata. The committed model is unchanged; create a new build revision to accept the new result",
        ));
    }
    Ok(())
}
impl Core {
    pub(crate) async fn verified_worker(
        &self,
        workspace: &str,
        revision: &str,
        cancel: watch::Receiver<bool>,
    ) -> Result<crate::worker_cache::WorkerUse> {
        let recovery = {
            let workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get(workspace)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != revision {
                return Err(Error::domain(
                    "STALE_ENTITY",
                    "Recovery revision is no longer current",
                ));
            }
            ws.recovery.clone()
        };
        let mut cancel = cancel;
        if *cancel.borrow() {
            return Err(Error::domain("CANCELLED", "Recovery cancelled"));
        }
        let _recovery = tokio::select! {guard=recovery.lock()=>guard,_=cancel.changed()=>return Err(Error::domain("CANCELLED","Cancelled while waiting for native recovery"))};
        let (inputs, expected) = {
            let mut workspaces = self.workspaces.lock().await;
            let ws = workspaces.get_mut(workspace).unwrap();
            if ws.stored.current != revision {
                return Err(Error::domain(
                    "STALE_ENTITY",
                    "Revision changed before recovery",
                ));
            }
            if let Some(worker) = self.worker_cache.acquire(ws) {
                return Ok(worker);
            }
            (
                ws.stored.revisions[revision].inputs.clone(),
                ws.stored.revisions[revision].model.clone(),
            )
        };
        if !expected["parts"].as_array().is_some_and(Vec::is_empty) {
            validate_publication(&expected).map_err(|_| {
                Error::domain(
                    "RECOVERY_VERIFICATION_REQUIRED",
                    "This revision needs an explicit rebuild before native recovery",
                )
            })?;
        }
        let (admission, _slot) = self.native_construction(cancel.clone()).await?;
        {
            let mut workspaces = self.workspaces.lock().await;
            let ws = workspaces.get_mut(workspace).unwrap();
            if ws.stored.current != revision {
                return Err(Error::domain(
                    "STALE_ENTITY",
                    "Revision changed while waiting for geometry capacity",
                ));
            }
            if let Some(worker) = self.worker_cache.acquire(ws) {
                return Ok(worker);
            }
        }
        if *cancel.borrow() {
            return Err(Error::domain(
                "CANCELLED",
                "Recovery cancelled before native execution",
            ));
        }
        let run = self.materialize(workspace, &id(), &inputs).await?;
        let source = run.source.clone();
        let mut worker = self
            .spawn_managed_worker(
                self.root.join("artifacts").join(workspace).join(revision),
                run,
                admission,
                cancel.clone(),
            )
            .await?;
        let input_digest = crate::portable::fingerprint(&inputs)?;
        let offered = match self
            .reusable_geometry(workspace, revision, revision, &input_digest, cancel.clone())
            .await
        {
            Ok(value) => value,
            Err(error) => {
                let _ = worker.stop().await;
                return Err(error);
            }
        };
        let built=worker.request(revision,"build",json!({"source_root":source,"entry_point":inputs.entry_point,"parameters":inputs.parameters,"annotations":inputs.annotations,"collision":inputs.mechanism.get("collision"),"expected_dependencies":inputs.dependencies,"allow_empty":inputs.files.get(&inputs.entry_point).is_some_and(|source| source.trim()=="# Describe a part to get started.")&&inputs.parameters==json!({}),"generate_mesh":false,"input_digest":input_digest,"geometry_cache":offered}),cancel).await;
        let result = built.and_then(|result| verify(&expected, &result["data"]));
        if let Err(error) = result {
            let _ = worker.stop().await;
            return Err(error);
        }
        let mut workspaces = self.workspaces.lock().await;
        let ws = workspaces.get_mut(workspace).unwrap();
        if ws.stored.current != revision {
            drop(workspaces);
            let _ = worker.stop().await;
            return Err(Error::domain(
                "STALE_ENTITY",
                "Revision changed during native recovery",
            ));
        }
        ws.worker = Some(Arc::new(Mutex::new(worker)));
        Ok(self.worker_cache.acquire(ws).unwrap())
    }
    /// Reading context never re-executes model code. Explicit repair is an
    /// actor-owned cancellable job, including when native recovery is required.
    pub(crate) async fn ensure_display_cache(&self, workspace: &str) -> Result<DisplayCheck> {
        let (revision, expected, epoch) = {
            let workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get(workspace)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            (
                ws.stored.current.clone(),
                ws.stored.revisions[&ws.stored.current].model.clone(),
                ws.display_epoch.clone(),
            )
        };
        let result = self
            .mesh_buffers_valid(workspace, &revision, &expected)
            .await
            .and_then(|valid| if valid { Ok(()) } else { Err(Error::domain(
                "DISPLAY_CACHE_MISSING",
                "Display data is missing, corrupt or outdated. Restore display to regenerate it without changing the design revision",
            )) });
        Ok(DisplayCheck {
            revision,
            epoch,
            error: result.err().map(|error| crate::error_value(&error)),
        })
    }
    async fn repair_buffers(
        &self,
        workspace: &str,
        revision: &str,
        worker: Arc<Mutex<Worker>>,
        mut cancel: watch::Receiver<bool>,
        attempted: &mut bool,
    ) -> Result<()> {
        let expected = {
            let workspaces = self.workspaces.lock().await;
            let ws = &workspaces[workspace];
            if ws.stored.current != revision {
                return Err(Error::domain("STALE_ENTITY", "Display revision changed"));
            }
            ws.stored.revisions[revision].model.clone()
        };
        if *cancel.borrow() {
            return Err(Error::domain("CANCELLED", "Display repair cancelled"));
        }
        // Tessellation repair shares the construction budget, but never waits
        // for capacity while holding the committed interpreter's mutex.
        let _slot = self.construction_slot(cancel.clone()).await?;
        // Newly generated buffers are not roots until the matching model is
        // durable. Keep collection out of this publication interval, including
        // its wait for the native worker and the final metadata commit.
        let _publication = tokio::select! {
            guard=self.display_publication.read()=>guard,
            _=cancel.changed()=>return Err(Error::domain("CANCELLED","Cancelled while waiting for display publication"))
        };
        let mut native = tokio::select! {guard=worker.lock()=>guard,_=cancel.changed()=>return Err(Error::domain("CANCELLED","Cancelled while waiting for native geometry"))};
        if self
            .mesh_buffers_valid(workspace, revision, &expected)
            .await?
        {
            return Ok(());
        }
        *attempted = true;
        let repaired = native
            .request(revision, "remesh", json!({}), cancel.clone())
            .await?;
        verify(&expected, &repaired["data"])?;
        validate_display_publication(&repaired["data"])?;
        if *cancel.borrow() {
            return Err(Error::domain("CANCELLED", "Display repair cancelled"));
        }
        let mut workspaces = self.workspaces.lock().await;
        let ws = workspaces.get_mut(workspace).unwrap();
        if ws.stored.current != revision {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Revision changed during display cache repair",
            ));
        }
        verify(&ws.stored.revisions[revision].model, &repaired["data"])?;
        let mut next = ws.stored.clone();
        next.revisions.get_mut(revision).unwrap().model = repaired["data"].clone();
        self.persist(&next).await?;
        ws.stored = next;
        ws.display_epoch = Arc::new(id());
        self.display_use.touch(workspace, revision);
        Ok(())
    }
    pub async fn repair_display(&self, actor: &str, args: RepairDisplay) -> Result<crate::Job> {
        let draft = self
            .update_source(
                &args.workspace_id,
                &args.revision,
                None,
                std::collections::BTreeMap::new(),
                actor,
            )
            .await?;
        self.queue_job(
            &args.workspace_id,
            &args.revision,
            &draft.id,
            actor,
            crate::JobKind::RepairDisplay,
        )
        .await
    }
    pub(crate) async fn run_display_repair(
        &self,
        workspace: &str,
        job_id: &str,
        draft: crate::Draft,
        cancel: watch::Receiver<bool>,
    ) {
        {
            let mut workspaces = self.workspaces.lock().await;
            let ws = workspaces.get_mut(workspace).unwrap();
            ws.stored.jobs.get_mut(job_id).unwrap().state = crate::JobState::Running;
            self.job_activity(workspace, &ws.stored.jobs[job_id]);
        }
        let outcome = async {
            let model = {
                let workspaces = self.workspaces.lock().await;
                let ws = &workspaces[workspace];
                if ws.stored.current != draft.base_revision {
                    return Err(Error::domain(
                        "STALE_ENTITY",
                        "Display repair revision changed",
                    ));
                }
                ws.stored.revisions[&draft.base_revision].model.clone()
            };
            if self
                .mesh_buffers_valid(workspace, &draft.base_revision, &model)
                .await?
            {
                return Ok(());
            }
            let worker = self
                .verified_worker(workspace, &draft.base_revision, cancel.clone())
                .await?;
            let mut attempted = false;
            let result = self
                .repair_buffers(
                    workspace,
                    &draft.base_revision,
                    worker.clone(),
                    cancel.clone(),
                    &mut attempted,
                )
                .await;
            if result.is_err() && attempted {
                let mut workspaces = self.workspaces.lock().await;
                let ws = workspaces.get_mut(workspace).unwrap();
                if ws
                    .worker
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &worker))
                {
                    ws.worker = None;
                }
            }
            result
        }
        .await;
        let mut workspaces = self.workspaces.lock().await;
        let ws = workspaces.get_mut(workspace).unwrap();
        let mut next = ws.stored.clone();
        let outcome = outcome.and_then(|()| {
            if *cancel.borrow() {
                Err(Error::domain("CANCELLED", "Display repair cancelled"))
            } else if next.current != draft.base_revision {
                Err(Error::domain(
                    "STALE_ENTITY",
                    "Revision changed during display repair",
                ))
            } else {
                Ok(())
            }
        });
        let job = next.jobs.get_mut(job_id).unwrap();
        match outcome {
            Ok(()) => {
                job.state = crate::JobState::Succeeded;
                job.resulting_revision = Some(draft.base_revision);
                job.diagnostics =
                    json!({"message":"Display buffers restored; design and live pose unchanged"});
            }
            Err(error) => {
                job.state = if error.code() == "CANCELLED" {
                    crate::JobState::Cancelled
                } else {
                    crate::JobState::Failed
                };
                job.diagnostics = crate::error_value(&error);
            }
        }
        match self.persist(&next).await {
            Ok(()) => ws.stored = next,
            Err(error) => {
                let job = ws.stored.jobs.get_mut(job_id).unwrap();
                job.state = crate::JobState::Failed;
                job.diagnostics = crate::error_value(&error);
            }
        }
        ws.cancellation.remove(job_id);
        self.job_activity(workspace, &ws.stored.jobs[job_id]);
    }
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepairDisplay {
    pub workspace_id: String,
    pub revision: String,
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::time::Duration;
    #[tokio::test]
    async fn cancellation_interrupts_a_wait_for_the_recovery_owner() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::open(
            temp.path().join("app"),
            crate::worker::WorkerConfig::bundled("unused".into()),
        )
        .await
        .unwrap();
        let ws = core.create_workspace("user").await.unwrap();
        let revision = core.get_project(&ws).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_string();
        let recovery = core.workspaces.lock().await[&ws].recovery.clone();
        let _owner = recovery.lock().await;
        let (send, cancel) = watch::channel(false);
        let pending = core.verified_worker(&ws, &revision, cancel);
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut pending)
                .await
                .is_err()
        );
        send.send_replace(true);
        assert!(
            matches!(tokio::time::timeout(Duration::from_secs(1),&mut pending).await.unwrap(),Err(error) if error.code()=="CANCELLED")
        );
        assert!(!temp.path().join("app/runs").exists());
    }
    #[tokio::test]
    async fn cancelled_queries_waiting_for_a_shared_native_worker_do_not_evict_it() {
        let temp = tempfile::tempdir().unwrap();
        let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let config = crate::worker::WorkerConfig::development(&repo);
        let core = Core::open(temp.path().join("app"), config.clone())
            .await
            .unwrap();
        let ws = core.create_workspace("user").await.unwrap();
        let revision = core.get_project(&ws).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_string();
        // A controlled worker-mutex wait, independent of geometry execution.
        // The real recovery/cancellation integration test verifies native reaping.
        let config = crate::worker::WorkerConfig {
            executable: config.executable,
            guardian_executable: config.guardian_executable,
            args: vec!["-c".into(), "import time;time.sleep(30)".into()],
            module_path: None,
            containment: crate::worker::Containment::UncontainedForTrustedTests,
            limits: Default::default(),
        };
        let worker = Arc::new(Mutex::new(
            Worker::spawn(&config, temp.path().join("controlled-worker"))
                .await
                .unwrap(),
        ));
        core.workspaces.lock().await.get_mut(&ws).unwrap().worker = Some(worker.clone());
        let _owner = worker.lock().await;
        let pending = core.query_as_actor(
            "waiting-query",
            &ws,
            &revision,
            "inspect",
            json!({"entities":[]}),
        );
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut pending)
                .await
                .is_err()
        );
        core.revoke_actor("waiting-query").await;
        assert!(
            matches!(tokio::time::timeout(Duration::from_secs(1),&mut pending).await.unwrap(),Err(error) if error.code()=="CANCELLED")
        );
        assert!(
            core.workspaces.lock().await[&ws]
                .worker
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &worker))
        );
    }
}
