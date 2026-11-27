//! Live wiring for the requirements compiler: a workspace snapshot built from
//! real model views, and raw dispatch of plan chunks to the Python worker.
//!
//! Nothing here interprets measurements. [`execute_plan_chunks`] sends
//! `evaluate_plan_chunk` requests (the evaluator's own JSON, see
//! `python/cadpad_worker/requirements_eval.py`) to the worker that holds a
//! built view and returns its replies untouched; the evaluation layer adapts
//! that to its executor trait.
//!
//! Resource accounting. The worker's CPU and memory limits are sampled over
//! its whole process tree (`worker_limits::sample`), so the evaluator's forked
//! pair-check and rebuild children count against the same budget on macOS.
//! Production containment denies `fork` outright, so contained workers run
//! the evaluator in-process; [`pool_cap`] makes that explicit from Rust
//! instead of relying on the worker noticing. Platforms without a sampler
//! (anything but macOS) have no limits to account against.
use super::ir::ModelRef;
use super::snapshot::{WorkspaceSnapshot, models_from_catalog};
use crate::{Core, Error, JobState, Result, worker::Containment, worker::WorkerConfig};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::watch;

/// The model file builder id (STL, STEP, ...): geometry without functions.
const FILE_BUILDER: &str = "$file";
/// Chunk limit the worker applies when a request names none.
const DEFAULT_CHUNK_LIMIT_S: f64 = 120.0;

/// A model's built view: the workspace to address and the revision it was
/// built at.
#[derive(Clone, Debug)]
pub struct ModelView {
    pub workspace: String,
    pub revision: String,
    // Clones held by running checks keep a retired workspace alive.
    pub(super) lease: Option<Arc<tokio::sync::oneshot::Sender<()>>>,
}

impl PartialEq for ModelView {
    fn eq(&self, other: &Self) -> bool {
        self.workspace == other.workspace && self.revision == other.revision
    }
}
impl Eq for ModelView {}

/// A compile-ready snapshot plus the built views it was read from, so plan
/// chunks can be dispatched to the same workers afterwards.
#[derive(Clone, Debug, Default)]
pub struct LiveSnapshot {
    pub snapshot: WorkspaceSnapshot,
    /// One view per model that built; failed and skipped models have none.
    pub views: BTreeMap<ModelRef, ModelView>,
}

impl Core {
    /// Snapshot the project that `workspace` belongs to (the workspace itself,
    /// or the project a model tab branched from).
    ///
    /// Reads the project source catalog, then builds a private view for each
    /// model in `referenced` (all catalog models when `None`) with the same
    /// machinery model tabs use, without opening any UI tab. If the caller
    /// addressed an existing model view, read its current built revision.
    /// A model that
    /// fails to build is listed with its build error; models outside
    /// `referenced` are listed as not built. `cached_views` maps models to
    /// view workspaces from an earlier snapshot, reused when still current.
    pub async fn snapshot_for_workspace(
        &self,
        workspace: &str,
        actor: &str,
        referenced: Option<&[ModelRef]>,
        cached_views: &BTreeMap<ModelRef, String>,
    ) -> Result<LiveSnapshot> {
        let project = self.requirement_owner(workspace).await?;
        let revision = {
            let all = self.workspace_metadata(&project).await?;
            all[&project].stored.current.clone()
        };
        let overrides = self.unsaved_source_overrides(workspace).await?;
        let catalog = self
            .project_source_catalog_with_overrides(&project, &revision, None, &overrides)
            .await?;
        let current_model = self.requirement_view_model(workspace).await?;
        let mut builds = BTreeMap::new();
        let mut views = BTreeMap::new();
        for (model, _) in models_from_catalog(&catalog) {
            if referenced.is_some_and(|wanted| !wanted.contains(&model)) {
                continue;
            }
            let built = if current_model.as_ref() == Some(&model) {
                self.requirement_model_snapshot(workspace, &model).await
            } else {
                let opened = self
                    .open_requirement_model(
                        &project,
                        &revision,
                        &model.entry,
                        &model.builder,
                        actor,
                        cached_views.get(&model).map(String::as_str),
                        &overrides,
                    )
                    .await;
                match opened {
                    Ok((view, job)) => {
                        let built = self.read_built_view(&view, &job.id, &model).await;
                        if built.is_err() && cached_views.get(&model) != Some(&view) {
                            let _ = self.forget_workspace(&view).await;
                        }
                        built
                    }
                    Err(error) => Err(error.to_string()),
                }
            };
            if let Ok((view, project_json)) = built {
                views.insert(model.clone(), view);
                builds.insert(model, Ok(project_json));
            } else if let Err(message) = built {
                builds.insert(model, Err(message));
            }
        }
        Ok(LiveSnapshot {
            snapshot: super::snapshot::snapshot_from_values(&catalog, &builds),
            views,
        })
    }

    pub(crate) async fn requirement_view_model(&self, workspace: &str) -> Result<Option<ModelRef>> {
        let all = self.workspace(workspace).await?;
        let stored = &all[workspace].stored;
        Ok(stored.model_parent.as_ref().and_then(|_| {
            crate::project_entry::inputs_model_entry(&stored.current_revision().inputs)
                .map(|(entry, builder)| ModelRef { entry, builder })
        }))
    }

    /// Wait for the build job of a model view, then read it. Returns the view
    /// and its `get_project` reply, with builder functions of adapter-built
    /// parts filled in.
    async fn read_built_view(
        &self,
        view: &str,
        job_id: &str,
        model: &ModelRef,
    ) -> std::result::Result<(ModelView, Value), String> {
        // A build is bounded by the worker's operation timeout; allow queueing
        // for native capacity on top of it.
        let patience = Duration::from_millis(
            self.config
                .limits
                .operation_timeout_ms
                .saturating_mul(4)
                .max(30_000),
        );
        let waited = tokio::time::timeout(patience, async {
            loop {
                let job = self.get_job(view, job_id).await?;
                if !matches!(job.state, JobState::Queued | JobState::Running) {
                    return Ok::<_, Error>(job);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let job = match waited {
            Ok(Ok(job)) => job,
            Ok(Err(error)) => return Err(error.to_string()),
            Err(_) => return Err("the model is still building".into()),
        };
        match job.state {
            JobState::Succeeded => {}
            JobState::Cancelled => return Err("the model build was cancelled".into()),
            _ => {
                return Err(job.diagnostics["message"]
                    .as_str()
                    .unwrap_or("the model failed to build")
                    .to_string());
            }
        }
        self.requirement_model_snapshot(view, model).await
    }

    async fn requirement_model_snapshot(
        &self,
        view: &str,
        model: &ModelRef,
    ) -> std::result::Result<(ModelView, Value), String> {
        let mut project = self.get_project(view).await.map_err(|e| e.to_string())?;
        name_adapter_builders(&mut project, model);
        let revision = project["revision"].as_str().unwrap_or_default().to_string();
        Ok((
            ModelView {
                workspace: view.to_string(),
                revision,
                lease: None,
            },
            project,
        ))
    }
}

/// Parts of a builder that returns geometry are published by Cadpad's
/// generated adapter (`_publish`), so the worker can only name that helper as
/// their function. Report the user's builder instead. Builders that call
/// `context.show` themselves already carry their own function name.
fn name_adapter_builders(project: &mut Value, model: &ModelRef) {
    if model.builder == FILE_BUILDER {
        return;
    }
    let entry_point = project["entry_point"].as_str().map(str::to_string);
    for part in project["model"]["parts"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        let adapter = part["source"]["file"].as_str().map(str::to_string) == entry_point
            && entry_point.is_some();
        if adapter && part["builder_function"].as_str() == Some("_publish") {
            part["builder_function"] = json!(model.builder);
        }
    }
}

// ------------------------------------------------------------------ dispatch

/// Seconds a chunk may run inside one worker operation: the worker's
/// operation timeout minus a margin for preparation overhead, result
/// serialization and the reply, so the worker's own `ChunkCancelled` (which
/// keeps the connection and the retained model) always fires before the
/// native timeout (which kills the worker).
pub fn chunk_budget_s(operation_timeout_ms: u64) -> f64 {
    let operation = operation_timeout_ms as f64 / 1000.0;
    (operation * 0.85 - 5.0).max(operation * 0.5)
}

/// Largest evaluator process pool this worker configuration allows, or `None`
/// when the evaluator's own default applies. Contained workers cannot fork
/// (the sandbox profile has no grant), so their pool is zero: tasks run
/// serially in the worker, and per-pair limits are judged after the fact.
pub fn pool_cap(config: &WorkerConfig) -> Option<u64> {
    (config.containment == Containment::Required).then_some(0)
}

/// Apply the dispatcher's limits to one `evaluate_plan_chunk` request:
/// `chunk_time_limit_s` is kept below the operation timeout and `workers`
/// within the pool cap. Values the caller chose below the limits stay.
pub fn bound_chunk_request(
    request: &mut Value,
    operation_timeout_ms: u64,
    pool_cap: Option<u64>,
) -> Result<()> {
    let invalid = |m: &str| Error::domain("INVALID_REQUEST", m);
    let object = request
        .as_object_mut()
        .ok_or_else(|| invalid("A plan chunk request must be a JSON object"))?;
    let limits = object.entry("limits").or_insert_with(|| json!({}));
    let limits = limits
        .as_object_mut()
        .ok_or_else(|| invalid("limits must be an object"))?;
    let budget = chunk_budget_s(operation_timeout_ms);
    let chunk = match limits.get("chunk_time_limit_s") {
        None | Some(Value::Null) => DEFAULT_CHUNK_LIMIT_S,
        Some(v) => v
            .as_f64()
            .ok_or_else(|| invalid("chunk_time_limit_s must be a number"))?,
    };
    limits.insert("chunk_time_limit_s".into(), json!(chunk.min(budget)));
    if let Some(cap) = pool_cap {
        let workers = limits
            .get("workers")
            .and_then(Value::as_u64)
            .map_or(cap, |w| w.min(cap));
        limits.insert("workers".into(), json!(workers));
    }
    Ok(())
}

/// Dispatch plan-chunk requests to the worker holding the built view
/// `view` (a workspace id, as in [`ModelView::workspace`]), one after another,
/// and return the raw replies in request order. Each request is the
/// `evaluate_plan_chunk` params object. Limits are bounded as in
/// [`bound_chunk_request`]. A chunk the worker cancelled at its time limit is
/// returned as such (`status: "cancelled"`), not as an error; transport and
/// resource failures abort with the underlying error.
pub async fn execute_plan_chunks(
    core: &Core,
    view: &str,
    requests: Vec<Value>,
) -> Result<Vec<Value>> {
    let (_keep, cancel) = watch::channel(false);
    execute_plan_chunks_cancellable(core, view, requests, cancel).await
}

/// [`execute_plan_chunks`] with a cancellation signal; cancelling stops the
/// worker (it is reaped, and recovered on next use).
pub async fn execute_plan_chunks_cancellable(
    core: &Core,
    view: &str,
    requests: Vec<Value>,
    cancel: watch::Receiver<bool>,
) -> Result<Vec<Value>> {
    let mut replies = Vec::with_capacity(requests.len());
    for request in requests {
        replies.push(plan_chunk(core, view, None, request, cancel.clone()).await?);
    }
    Ok(replies)
}

pub(super) async fn execute_compiled_chunk(
    core: &Core,
    view: &ModelView,
    args: Value,
    cancel: watch::Receiver<bool>,
) -> Result<Value> {
    plan_chunk(core, &view.workspace, Some(&view.revision), args, cancel).await
}

async fn plan_chunk(
    core: &Core,
    view: &str,
    expected_revision: Option<&str>,
    mut args: Value,
    cancel: watch::Receiver<bool>,
) -> Result<Value> {
    bound_chunk_request(
        &mut args,
        core.config.limits.operation_timeout_ms,
        pool_cap(&core.config),
    )?;
    let _memory = core.pin_workspace(view).await?;
    let revision = {
        let all = core.workspace(view).await?;
        let current = all[view].stored.current_revision();
        if expected_revision.is_some_and(|expected| current.id != expected) {
            return Err(Error::domain(
                "STALE_ENTITY",
                "The model changed after preparing the requirement",
            ));
        }
        if crate::stl_model::is_mesh(&current.model) {
            return Err(Error::domain(
                "UNSUPPORTED_OPERATION",
                "Plan chunks need native CAD geometry, not a mesh model",
            ));
        }
        current.id.clone()
    };
    // The worker evaluates against the build it holds. Builds and native
    // recovery both pass the input digest, which keys its rebuild cache.
    let worker = core
        .verified_worker(view, &revision, cancel.clone())
        .await?;
    let mut waiting = cancel.clone();
    if *waiting.borrow() {
        return Err(Error::domain("CANCELLED", "Plan chunk cancelled"));
    }
    let mut native = tokio::select! {
        guard = worker.lock() => guard,
        _ = waiting.changed() => return Err(Error::domain("CANCELLED", "Cancelled while waiting for native geometry")),
    };
    let result = native
        .request(&revision, "evaluate_plan_chunk", args, cancel)
        .await;
    drop(native);
    if let Err(error) = &result
        && matches!(
            error.code(),
            "CANCELLED" | "WORKER_UNAVAILABLE" | "IO_ERROR" | "RESOURCE_LIMIT"
        )
    {
        // The worker is gone or was stopped; never hand it out again.
        let mut all = core.workspace(view).await?;
        if let Some(ws) = all.get_mut(view)
            && ws
                .worker
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &worker))
        {
            ws.worker = None;
        }
    }
    let data = result?["data"].clone();
    let all = core.workspace(view).await?;
    if all[view].stored.current != revision {
        return Err(Error::domain(
            "STALE_ENTITY",
            "Revision changed during plan evaluation",
        ));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn compiled_chunk_rejects_another_revision_before_native_dispatch() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::open(
            temp.path().to_path_buf(),
            WorkerConfig::bundled("unused".into()),
        )
        .await
        .unwrap();
        let view = ModelView {
            workspace: core.create_workspace("user").await.unwrap(),
            revision: "superseded".into(),
            lease: None,
        };
        let (_send, cancel) = watch::channel(false);
        let error = execute_compiled_chunk(&core, &view, json!({}), cancel)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "STALE_ENTITY");
    }

    #[test]
    fn chunk_limit_stays_below_the_worker_operation_timeout() {
        for operation_ms in [1_000, 10_000, 60_000, 300_000, 3_600_000] {
            let budget = chunk_budget_s(operation_ms);
            assert!(budget > 0.0 && budget < operation_ms as f64 / 1000.0);
        }
        assert_eq!(chunk_budget_s(300_000), 250.0);
        let mut request = json!({"states": []});
        bound_chunk_request(&mut request, 300_000, None).unwrap();
        assert_eq!(request["limits"]["chunk_time_limit_s"], 120.0);
        let mut request = json!({"limits": {"chunk_time_limit_s": 900.0, "workers": 8}});
        bound_chunk_request(&mut request, 300_000, None).unwrap();
        assert_eq!(request["limits"]["chunk_time_limit_s"], 250.0);
        assert_eq!(
            request["limits"]["workers"], 8,
            "no cap without containment"
        );
        let mut request = json!({"limits": {"chunk_time_limit_s": 5.0}});
        bound_chunk_request(&mut request, 300_000, None).unwrap();
        assert_eq!(request["limits"]["chunk_time_limit_s"], 5.0);
    }

    #[test]
    fn pool_is_capped_and_bad_requests_are_rejected() {
        let mut request = json!({"limits": {"workers": 8}});
        bound_chunk_request(&mut request, 300_000, Some(0)).unwrap();
        assert_eq!(request["limits"]["workers"], 0);
        let mut request = json!({});
        bound_chunk_request(&mut request, 300_000, Some(2)).unwrap();
        assert_eq!(request["limits"]["workers"], 2);
        for bad in [
            json!(3),
            json!({"limits": 3}),
            json!({"limits": {"chunk_time_limit_s": "soon"}}),
        ] {
            let mut bad = bad;
            assert!(bound_chunk_request(&mut bad, 300_000, None).is_err());
        }
    }

    #[test]
    fn adapter_published_parts_report_the_user_builder() {
        let model = ModelRef {
            entry: "cabinet.py".into(),
            builder: "cabinet".into(),
        };
        let mut project = json!({"entry_point": "model.py", "model": {"parts": [
            {"name": "model", "source": {"file": "model.py", "line": 9}, "builder_function": "_publish"},
            {"name": "door", "source": {"file": "cabinet.py", "line": 4}, "builder_function": "make_door"},
            {"name": "frame", "source": {"file": "cabinet.py", "line": 5}}]}});
        name_adapter_builders(&mut project, &model);
        let parts = &project["model"]["parts"];
        assert_eq!(parts[0]["builder_function"], "cabinet");
        assert_eq!(parts[1]["builder_function"], "make_door");
        assert!(parts[2]["builder_function"].is_null());
        let built = super::super::snapshot::built_from_project(&project).unwrap();
        assert_eq!(
            built
                .part("model")
                .unwrap()
                .source
                .as_ref()
                .unwrap()
                .function,
            Some("cabinet".into())
        );
    }
}
