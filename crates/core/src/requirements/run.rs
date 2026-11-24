//! Requirement runs end to end (spec §6, P1.55, P1.82): live snapshot, relink
//! against the approved manifest, run the plan on the workers, evaluate, label
//! findings against the previous run, compute the status and store the result.
//!
//! Runs go through the same admission queue as motion tests
//! ([`crate::motion_test_runs`]): one active run per project, two per actor,
//! eight per application, cancellable, with a deadline (the sweep cap). A run
//! that is cancelled, times out or fails to reach the workers stores nothing:
//! the previous result stays. A run that cannot evaluate for a reason the
//! user must see (`needs_review`, `error`) stores that status next to the
//! previous result, which is then shown as "previous".
//!
//! # Stored result (`last_result`)
//!
//! ```text
//! { format: "requirements-run-v1",
//!   status, reasons[], show_previous, changed_since_approval[],   // StatusReport
//!   ran, stale, queued,               // evaluated now / waiting for an on-demand run / queued
//!   plan_hash, result_hash,           // of the plan that ran, of body (null when not run)
//!   body,                             // ResultBody of the latest evaluation (maybe previous)
//!   findings[],                       // labelled new / still_failing / resolved
//!   timings, warnings[],              // RunTimings, W_SLOW diagnostics
//!   relink, diff,                     // RelinkReport and its plain text
//!   subjects { "entry#builder": fingerprint },
//!   estimate_s, auto_run_budget_s,    // cost estimate when auto-run was considered
//!   completed_at_ms, finished_at }   // finished_at: ISO 8601 UTC
//!
//! `relink.changes[]` carry `text` (the human-readable message) next to `message`.
//! ```
use super::bind::model_key;
use super::compile::{Compiled, compile};
use super::diagnostics::Diagnostic;
use super::dispatch::{LiveSnapshot, ModelView, execute_compiled_chunk, pool_cap};
use super::evaluate::{EvalResult, ResultBody, evaluate};
use super::ir::{ModelRef, Requirement};
use super::lifecycle::{StatusInputs, compute_status};
use super::manifest::Manifest;
use super::plan::Plan;
use super::relink::{BuildIndex, Effect, Relink, RelinkReport, relink};
use super::settings::ProjectSettings;
use super::snapshot::WorkspaceSnapshot;
use super::status::RequirementStatus;
use super::storage::{Approved, RequirementEntry, project_settings};
use super::worker_plan::{
    ChunkOptions, ChunkRequest, EvalError, PlanExecutor, RawChunk, build_requests,
    parse_chunk_response, run_plan,
};
use crate::motion_test_runs::{self as queue, Lease, State, Status};
use crate::motion_tests::{Saved, fingerprint};
use crate::{Core, Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::sync::{Mutex, oneshot, watch};

pub const RUN_FORMAT: &str = "requirements-run-v1";
/// Seconds one uncached `param` rebuild costs (spec §6.4, phase 0).
pub const REBUILD_S: f64 = 9.0;
/// Seconds one uncached pair job costs on one process.
pub const PAIR_JOB_S: f64 = 0.15;
/// Fixed overhead of a run in the estimate.
pub const BASE_S: f64 = 1.0;
/// Longest an auto-run waits for a busy project before leaving the rest for
/// an on-demand run.
const AUTO_RUN_PATIENCE_S: u64 = 120;
pub(crate) type AutoRunQueue = std::sync::Mutex<
    std::collections::HashMap<String, std::collections::VecDeque<(String, String)>>,
>;
pub(crate) type ModelViewCache =
    std::sync::Mutex<BTreeMap<String, Arc<Mutex<BTreeMap<ModelRef, CachedView>>>>>;

pub(crate) struct CachedView {
    view: ModelView,
    released: oneshot::Receiver<()>,
}

impl CachedView {
    fn new(view: &mut ModelView) -> Self {
        let (lease, released) = oneshot::channel();
        view.lease = Some(Arc::new(lease));
        Self {
            view: view.clone(),
            released,
        }
    }

    fn retire(self, core: Core) {
        let workspace = self.view.workspace.clone();
        drop(self.view);
        tokio::spawn(async move {
            let _ = self.released.await;
            let _ = core.forget_workspace(&workspace).await;
        });
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_REQUIREMENT", message)
}

/// How one evaluation is run. The defaults are what the app uses.
#[derive(Clone, Debug)]
pub struct RunOptions {
    /// Evaluator processes per chunk; `None` leaves it to the worker (and the
    /// containment cap). `Some(0)` evaluates in-process, serially.
    pub workers: Option<usize>,
    /// Use the worker's result and rebuild caches.
    pub use_cache: bool,
    /// Store the result in the catalog.
    pub store: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            workers: None,
            use_cache: true,
            store: true,
        }
    }
}

/// The state of a run as the UI reads it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RunInfo {
    pub id: String,
    pub workspace_id: String,
    pub revision: String,
    pub requirement_id: String,
    pub actor: String,
    pub state: State,
    pub error: Option<queue::Failure>,
}

impl From<Status> for RunInfo {
    fn from(s: Status) -> Self {
        Self {
            id: s.id,
            workspace_id: s.workspace_id,
            revision: s.revision,
            requirement_id: s.test_id,
            actor: s.actor,
            state: s.state,
            error: s.error,
        }
    }
}

// --------------------------------------------------------------- the executor

/// Runs plan chunks on the live workers: maps each chunk to the built view of
/// its subject model and sends it through [`execute_compiled_chunk`]
/// (which adds the build revision and bounds the limits). Blocking: use it from
/// `spawn_blocking`.
pub struct LiveExecutor {
    core: Core,
    handle: tokio::runtime::Handle,
    /// `entry#builder` to the view that holds its build.
    views: BTreeMap<String, ModelView>,
    cancel: watch::Receiver<bool>,
    use_cache: bool,
    /// The transport or resource error behind the last failed chunk, with its
    /// code (an [`EvalError`] has no room for it).
    pub failure: Option<Error>,
}

impl LiveExecutor {
    pub fn new(
        core: Core,
        views: BTreeMap<String, ModelView>,
        cancel: watch::Receiver<bool>,
        use_cache: bool,
    ) -> Self {
        Self {
            core,
            handle: tokio::runtime::Handle::current(),
            views,
            cancel,
            use_cache,
            failure: None,
        }
    }
}

impl PlanExecutor for LiveExecutor {
    fn run_chunk(&mut self, req: &ChunkRequest) -> std::result::Result<RawChunk, EvalError> {
        let view = self
            .views
            .get(&req.model)
            .ok_or_else(|| EvalError::Executor(format!("model {} has no built view", req.model)))?;
        let mut params = req.params.clone();
        if !self.use_cache {
            params["cache"] = json!({"results": false, "rebuild": false});
        }
        let reply = self.handle.block_on(execute_compiled_chunk(
            &self.core,
            view,
            params,
            self.cancel.clone(),
        ));
        match reply {
            Ok(reply) => parse_chunk_response(req, &reply),
            Err(error) => {
                let message = error.to_string();
                self.failure = Some(error);
                Err(EvalError::Executor(message))
            }
        }
    }
}

// ------------------------------------------------------------------ preparing

/// What a run will do, decided before any worker call.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub id: String,
    pub approved: Approved,
    /// The draft differs from the approved IR (the approved one still runs).
    pub edited: bool,
    pub relink: Option<RelinkReport>,
    /// The plan to execute; `None` when the run must not evaluate.
    pub run: Option<RunPlan>,
    pub build_failed: bool,
    pub bind_failed: bool,
    /// Why, in addition to the status reasons (build errors, diagnostics).
    pub notes: Vec<String>,
    pub views: BTreeMap<String, ModelView>,
    /// Subject model key to the fingerprint of the build seen now.
    pub subjects: BTreeMap<String, String>,
    pub snapshot: WorkspaceSnapshot,
}

pub(crate) struct ResultGuard<'a> {
    pub views: &'a BTreeMap<String, ModelView>,
    pub cancel: &'a watch::Receiver<bool>,
}

#[derive(Clone, Debug)]
pub struct RunPlan {
    pub requirement: Requirement,
    pub plan: Plan,
    pub manifest: Manifest,
}

/// Models a requirement's manifest refers to.
fn referenced_models(approved: &Approved) -> Vec<ModelRef> {
    let m = &approved.manifest;
    let mut out: BTreeSet<ModelRef> = m.subjects.iter().map(|s| s.model.clone()).collect();
    out.extend(m.chips.iter().map(|c| c.model.clone()));
    let keys = m
        .parts
        .keys()
        .chain(m.sets.values().flatten().map(|l| &l.key))
        .chain(m.parts.values().flatten().map(|l| &l.key))
        .chain(m.params.keys())
        .chain(m.features.keys());
    for key in keys {
        if let Some((entry, rest)) = key.split_once('#') {
            let builder: String = rest
                .chars()
                .take_while(|c| !matches!(c, '/' | ':' | '#'))
                .collect();
            out.insert(ModelRef {
                entry: entry.to_owned(),
                builder,
            });
        }
    }
    out.into_iter().collect()
}

/// The IR with auto-relinked part names substituted (P1.57): every part
/// reference whose `entry#builder/name` is a relink source now names the entity
/// with the same exact geometry. A relink into another model cannot be
/// expressed by renaming and is returned as an error.
pub fn substitute_relinks(
    req: &Requirement,
    relinks: &[Relink],
) -> std::result::Result<Requirement, String> {
    fn model_of(key: &str) -> Option<(&str, &str)> {
        let (m, n) = key.split_once('#')?;
        let slash = n.find('/')?;
        let model = &key[..m.len() + 1 + slash];
        Some((model, &n[slash + 1..]))
    }
    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    for r in relinks {
        // Feature relinks name `part#feature`; they are not part names.
        if r.from.contains('#')
            && r.from
                .rsplit_once('#')
                .is_some_and(|(a, _)| a.contains('/'))
        {
            continue;
        }
        let (Some((from_model, _)), Some((to_model, to_name))) =
            (model_of(&r.from), model_of(&r.to))
        else {
            continue;
        };
        if from_model != to_model {
            return Err(format!(
                "{} moved to {}, a different model; review the requirement",
                r.from, r.to
            ));
        }
        renames.insert(r.from.clone(), to_name.to_owned());
    }
    fn walk(v: &mut Value, renames: &BTreeMap<String, String>) {
        match v {
            Value::Object(map) => {
                let key = match (map.get("model"), map.get("part")) {
                    (Some(m), Some(p)) => match (
                        m["entry"].as_str(),
                        m["builder"].as_str(),
                        p["name"].as_str(),
                    ) {
                        (Some(e), Some(b), Some(n)) => Some(format!("{e}#{b}/{n}")),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(to) = key.and_then(|k| renames.get(&k)) {
                    map["part"] = json!({"name": to});
                }
                for child in map.values_mut() {
                    walk(child, renames);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|i| walk(i, renames)),
            _ => {}
        }
    }
    let mut value = serde_json::to_value(req).map_err(|e| e.to_string())?;
    walk(&mut value, &renames);
    serde_json::from_value(value).map_err(|e| e.to_string())
}

fn error_notes(c: &Compiled) -> Vec<String> {
    c.diagnostics
        .iter()
        .filter(|d| d.is_error())
        .take(5)
        .map(|d: &Diagnostic| format!("{} at {}: {}", d.code, d.at, d.message))
        .collect()
}

/// Decide what to do with `approved` on the snapshot: relink, recompile with
/// relinked names, and pick the plan to run (P1.57, P1.58).
pub fn prepare(
    id: &str,
    approved: &Approved,
    edited: bool,
    live: &LiveSnapshot,
    settings: &ProjectSettings,
) -> Prepared {
    let snap = &live.snapshot;
    let mut notes = Vec::new();
    let mut build_failed = false;
    let mut subjects = BTreeMap::new();
    for s in &approved.manifest.subjects {
        let model = snap.model(&s.model);
        if let Some(error) = model.and_then(|m| m.build_error.as_ref()) {
            build_failed = true;
            notes.push(format!("{} fails to build: {error}", model_key(&s.model)));
        }
        if let Some(b) = model.and_then(|m| m.built.as_ref()) {
            subjects.insert(model_key(&s.model), b.fingerprint.clone());
        }
    }
    let fresh = compile(&approved.requirement, snap, settings);
    let build = BuildIndex::from_snapshot(snap);
    let report = relink(
        &approved.manifest,
        fresh.manifest.as_ref(),
        &build,
        settings,
    );
    let views = live
        .views
        .iter()
        .map(|(m, v)| (model_key(m), v.clone()))
        .collect();
    let mut out = Prepared {
        id: id.to_owned(),
        approved: approved.clone(),
        edited,
        relink: Some(report.clone()),
        run: None,
        build_failed,
        bind_failed: false,
        notes,
        views,
        subjects,
        snapshot: snap.clone(),
    };
    if build_failed || report.decision == Effect::NeedsReview {
        return out;
    }
    let compiled = if report.relinks.is_empty() && !fresh.has_errors() {
        fresh
    } else {
        match substitute_relinks(&approved.requirement, &report.relinks) {
            Ok(req) => compile(&req, snap, settings),
            Err(message) => {
                out.bind_failed = true;
                out.notes.push(message);
                return out;
            }
        }
    };
    if compiled.has_errors() {
        out.bind_failed = true;
        out.notes.extend(error_notes(&compiled));
    } else if let (Some(plan), Some(manifest)) = (compiled.plan, compiled.manifest) {
        if !report.relinks.is_empty() {
            let mut rebound = relink(&approved.manifest, Some(&manifest), &build, settings);
            // This IR change was produced by exact-hash relinking.
            rebound.requirement_changed = report.requirement_changed;
            let runs = rebound.runs();
            out.relink = Some(rebound);
            if !runs {
                return out;
            }
        }
        out.run = Some(RunPlan {
            requirement: compiled.requirement,
            plan,
            manifest,
        });
    } else {
        out.bind_failed = true;
    }
    out
}

// ------------------------------------------------------------------ estimating

/// Evaluator processes per chunk that a run with these options would use.
pub fn pool_size(core: &Core, workers: Option<usize>) -> usize {
    let cap = pool_cap(&core.config).map(|c| c as usize);
    let default = std::thread::available_parallelism()
        .map_or(2, usize::from)
        .saturating_sub(1)
        .clamp(1, 4);
    let wanted = workers.or(cap).unwrap_or(default);
    cap.map_or(wanted, |c| wanted.min(c))
}

/// Distinct parameter rebuilds the run needs: one per distinct parameter set
/// that differs from the current build's values, per model (spec §6.4).
pub fn param_rebuilds(p: &Prepared, run: &RunPlan) -> usize {
    let Ok(requests) = build_requests(&run.plan, &run.manifest, &ChunkOptions::default()) else {
        return 0;
    };
    let mut sets: BTreeSet<(String, String)> = BTreeSet::new();
    for r in &requests {
        let built = p
            .snapshot
            .models
            .iter()
            .find(|m| model_key(&m.model) == r.model)
            .and_then(|m| m.built.as_ref());
        for state in r.params["states"].as_array().into_iter().flatten() {
            let Some(params) = state["params"].as_object().filter(|o| !o.is_empty()) else {
                continue;
            };
            let at_rest = params.iter().all(|(name, v)| {
                built
                    .and_then(|b| b.param(name))
                    .is_some_and(|b| v.as_f64() == Some(b.value.value))
            });
            if !at_rest {
                sets.insert((r.model.clone(), Value::Object(params.clone()).to_string()));
            }
        }
    }
    sets.len()
}

/// Estimated cost in seconds (spec §6.4): about 9 s per uncached rebuild plus
/// about 0.15 s per uncached pair job over the pool, plus a second. A run on
/// the same plan and builds as the previous one is fully cached.
pub fn estimate_cost_s(p: &Prepared, previous: Option<&Value>, pool: usize) -> Option<f64> {
    let run = p.run.as_ref()?;
    let plan_hash = run.plan.content_hash();
    let warm = previous.is_some_and(|v| {
        v["ran"] == true && v["plan_hash"] == json!(plan_hash) && v["subjects"] == json!(p.subjects)
    });
    if warm {
        return Some(BASE_S);
    }
    let jobs = run.plan.job_count() as f64;
    Some(
        param_rebuilds(p, run) as f64 * REBUILD_S + jobs * PAIR_JOB_S / pool.max(1) as f64 + BASE_S,
    )
}

// ------------------------------------------------------------------ assembling

/// How a stored result was produced when no evaluation ran.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    /// Waiting for an on-demand run: `(estimate, budget)` in seconds.
    pub stale: Option<(f64, f64)>,
    pub estimate_s: Option<f64>,
}

fn previous_body(previous: Option<&Value>) -> Option<ResultBody> {
    previous.and_then(|v| serde_json::from_value(v.get("body")?.clone()).ok())
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` from Unix milliseconds.
fn iso(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let rest = ms % 86_400_000;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

/// The relink report as JSON, each change with a human-readable `text`.
fn relink_json(report: Option<&RelinkReport>) -> Value {
    let Some(report) = report else {
        return Value::Null;
    };
    let mut v = json!(report);
    if let Some(changes) = v["changes"].as_array_mut() {
        for c in changes {
            c["text"] = c["message"].clone();
        }
    }
    v
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The `last_result` of a run (or of a run that could not evaluate): the
/// evaluation if there is one, else the previous body shown as previous.
pub fn assemble(
    p: &Prepared,
    eval: Option<&EvalResult>,
    previous: Option<&Value>,
    marks: &Marks,
) -> Value {
    let prev_body = previous_body(previous);
    let body: Option<&ResultBody> = eval.map(|e| &e.body).or(prev_body.as_ref());
    let plan_hash = p.run.as_ref().map(|r| r.plan.content_hash());
    let mut report = compute_status(&StatusInputs {
        has_errors: false,
        approved: true,
        edited_since_approval: p.edited,
        build_failed: p.build_failed,
        bind_failed: p.bind_failed,
        running: false,
        relink: p.relink.as_ref(),
        current_plan_hash: plan_hash.as_deref(),
        result: body,
    });
    // An evaluated result is for the plan that ran, whatever the hash says.
    if let Some(e) = eval
        && report.status == RequirementStatus::Pending
    {
        report.status = e.body.status;
        report.reasons.clear();
        report.show_previous = false;
    }
    if let Some((estimate, budget)) = marks.stale
        && !matches!(
            report.status,
            RequirementStatus::NeedsReview | RequirementStatus::Error
        )
    {
        report.status = RequirementStatus::Pending;
        report.reasons = vec![format!(
            "estimated {estimate:.0} s, over the {budget:.0} s auto-run budget; run it on demand"
        )];
        report.show_previous = body.is_some();
    }
    report.reasons.extend(p.notes.iter().cloned());
    let previous_field = |key: &str| previous.and_then(|v| v.get(key).cloned());
    let findings = match eval {
        Some(e) => json!(e.findings_vs(prev_body.as_ref())),
        None => previous_field("findings").unwrap_or_else(|| json!([])),
    };
    let (timings, warnings) = match eval {
        Some(e) => (json!(e.timings), json!(e.warnings)),
        None => (
            previous_field("timings").unwrap_or(Value::Null),
            previous_field("warnings").unwrap_or_else(|| json!([])),
        ),
    };
    let finished = now_ms();
    json!({
        "format": RUN_FORMAT,
        "status": report.status,
        "reasons": report.reasons,
        "show_previous": report.show_previous,
        "changed_since_approval": report.changed_since_approval,
        "ran": eval.is_some(),
        "stale": marks.stale.is_some(),
        "queued": false,
        "plan_hash": if eval.is_some() { json!(plan_hash) } else { previous_field("plan_hash").unwrap_or(Value::Null) },
        "result_hash": body.map(ResultBody::hash),
        "body": body,
        "findings": findings,
        "timings": timings,
        "warnings": warnings,
        "relink": relink_json(p.relink.as_ref()),
        "diff": p.relink.as_ref().map(RelinkReport::diff_text),
        "subjects": p.subjects,
        "estimate_s": marks.estimate_s,
        "auto_run_budget_s": marks.stale.map(|(_, b)| b),
        "completed_at_ms": finished,
        "finished_at": iso(finished),
    })
}

// ------------------------------------------------------------------- the Core

/// Outcome of [`Core::evaluate_requirement`].
#[derive(Clone, Debug)]
pub struct RunOutcome {
    /// The `last_result` (stored when `RunOptions::store`).
    pub result: Value,
    /// Present when the plan was evaluated.
    pub eval: Option<EvalResult>,
}

/// What [`Core::auto_run_requirements`] did.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct AutoRunReport {
    pub budget_s: u32,
    /// Re-run now, within budget.
    pub ran: Vec<String>,
    /// Over budget: marked pending, run on demand.
    pub stale: Vec<StaleRequirement>,
    /// Could not evaluate (needs review, error); that status was stored.
    pub blocked: Vec<String>,
    /// Failed, was cancelled or timed out; the previous result was kept.
    pub failed: Vec<FailedRequirement>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StaleRequirement {
    pub id: String,
    pub estimate_s: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FailedRequirement {
    pub id: String,
    pub code: String,
    pub message: String,
}

impl Core {
    pub(crate) fn requirement_run_status(&self, owner: &str) -> Option<RunInfo> {
        self.requirement_runs.status(owner).map(RunInfo::from)
    }

    /// A live snapshot of the project's models, reusing the built views of
    /// earlier calls while they are current.
    pub(crate) async fn live_snapshot(
        &self,
        workspace: &str,
        actor: &str,
        referenced: Option<&[ModelRef]>,
    ) -> Result<LiveSnapshot> {
        let owner = self.requirement_owner(workspace).await?;
        let cache = self
            .requirement_views
            .lock()
            .unwrap()
            .entry(owner)
            .or_default()
            .clone();
        let mut cache = cache.lock().await;
        let cached = cache
            .iter()
            .map(|(model, entry)| (model.clone(), entry.view.workspace.clone()))
            .collect();
        let mut live = self
            .snapshot_for_workspace(workspace, actor, referenced, &cached)
            .await?;
        for (_, old) in cache.extract_if(.., |model, _| {
            live.snapshot
                .model(model)
                .is_none_or(|model| model.build_error.is_some())
        }) {
            old.retire(self.clone());
        }
        for (model, view) in &mut live.views {
            if view.workspace == workspace {
                continue;
            }
            if let Some(entry) = cache.get(model)
                && entry.view.workspace == view.workspace
            {
                view.lease = entry.view.lease.clone();
                continue;
            }
            if let Some(old) = cache.insert(model.clone(), CachedView::new(view)) {
                old.retire(self.clone());
            }
        }
        Ok(live)
    }

    pub(crate) async fn forget_requirement_views(&self, owner: &str) {
        let cache = self.requirement_views.lock().unwrap().remove(owner);
        if let Some(cache) = cache {
            for (_, view) in std::mem::take(&mut *cache.lock().await) {
                view.retire(self.clone());
            }
        }
    }

    async fn approved_entry(
        &self,
        actor: &str,
        workspace: &str,
        id: &str,
    ) -> Result<(String, RequirementEntry)> {
        let (owner, catalog) = self.requirement_catalog(actor, workspace).await?;
        let entry = catalog
            .get(id)
            .and_then(Saved::requirement)
            .ok_or_else(|| invalid("Unknown requirement"))?
            .clone();
        if entry.approved.is_none() {
            return Err(Error::domain(
                "REQUIREMENT_NOT_APPROVED",
                format!("Requirement '{id}' is a draft and has to be approved first"),
            ));
        }
        Ok((owner, entry))
    }

    pub(super) async fn prepare_requirement(
        &self,
        actor: &str,
        workspace: &str,
        id: &str,
        cancel: &watch::Receiver<bool>,
    ) -> Result<(String, Prepared, Option<Value>)> {
        let (owner, entry) = self.approved_entry(actor, workspace, id).await?;
        let approved = entry.approved.clone().expect("checked approved");
        let edited = entry.approval() == super::storage::Approval::EditedSinceApproval;
        if *cancel.borrow() {
            return Err(Error::domain("CANCELLED", "Requirement run cancelled"));
        }
        let models = referenced_models(&approved);
        let live = self
            .live_snapshot(
                workspace,
                actor,
                (!models.is_empty()).then_some(models.as_slice()),
            )
            .await?;
        let prepared = prepare(id, &approved, edited, &live, &project_settings());
        Ok((owner, prepared, entry.last_result))
    }

    /// Replace the stored result under the catalog's compare-and-swap.
    /// Approval and evaluated revisions must remain current at publication.
    async fn store_requirement_result(
        &self,
        actor: &str,
        workspace: &str,
        prepared: &Prepared,
        cancel: &watch::Receiver<bool>,
        make: impl Fn(Option<&Value>) -> Value,
    ) -> Result<Value> {
        let mut last = None;
        for _ in 0..4 {
            let (owner, mut catalog) = self.requirement_catalog(actor, workspace).await?;
            let expected = fingerprint(&catalog)?;
            let Some(Saved::Requirement(entry)) = catalog.get_mut(&prepared.id) else {
                return Err(invalid("The requirement was deleted during the run"));
            };
            if entry.approved.as_ref().map(|a| &a.hashes) != Some(&prepared.approved.hashes) {
                return Err(Error::domain(
                    "STALE_ENTITY",
                    "The requirement was approved again during the run; run it again",
                ));
            }
            let result = make(entry.last_result.as_ref());
            entry.last_result = Some(result.clone());
            entry.rerun_required = false;
            match self
                .commit_requirement_catalog(
                    actor,
                    &owner,
                    workspace,
                    &expected,
                    &catalog,
                    Some(ResultGuard {
                        views: &prepared.views,
                        cancel,
                    }),
                )
                .await
            {
                Ok(()) => return Ok(result),
                Err(error) if error.code() == "REVISION_CONFLICT" => last = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(last.expect("conflict recorded"))
    }

    /// Evaluate `prepared` on the workers. Returns `Ok(None)` when it must not
    /// evaluate (needs review, error). Cancellation, a time-out and transport
    /// failures are errors: nothing is claimed and nothing stored.
    async fn evaluate_prepared(
        &self,
        prepared: &Prepared,
        opts: &RunOptions,
        cancel: watch::Receiver<bool>,
        lease: Option<Lease>,
    ) -> Result<std::result::Result<Option<EvalResult>, EvalError>> {
        let Some(run) = prepared.run.clone() else {
            return Ok(Ok(None));
        };
        let settings = project_settings();
        let mut chunk = ChunkOptions::from_settings(&settings);
        chunk.workers = opts.workers;
        let views = prepared.views.clone();
        let id = prepared.id.clone();
        let unsupported = prepared.approved.requirement.unsupported.len();
        let core = self.clone();
        let use_cache = opts.use_cache;
        let outcome = tokio::task::spawn_blocking(move || {
            // The lease lives until the entered CPU work ends.
            let _lease = lease;
            let mut exec = LiveExecutor::new(core, views, cancel, use_cache);
            let raw = run_plan(&run.plan, &run.manifest, &mut exec, &chunk);
            (
                raw.map(|raw| {
                    evaluate(&run.plan, &run.manifest, &raw, &settings, &id, unsupported)
                }),
                exec.failure,
            )
        })
        .await
        .map_err(|e| Error::domain("RUN_INTERRUPTED", e.to_string()))?;
        match outcome {
            (Ok(eval), _) => Ok(Ok(Some(eval))),
            (Err(_), Some(failure)) => Err(failure),
            (Err(EvalError::Cancelled(reason)), None) => Err(Error::domain(
                "RESOURCE_LIMIT",
                format!(
                    "A check hit the chunk time limit ({reason}); the previous result is kept. \
                     Narrow the sweep or run fewer states."
                ),
            )),
            (Err(error), None) => Ok(Err(error)),
        }
    }

    /// Prepare, evaluate and (optionally) store one approved requirement.
    /// `lease` is the queue admission of a leased run; inline callers pass
    /// `None` and are cancelled only through `cancel`.
    pub(crate) async fn run_requirement_inner(
        &self,
        actor: &str,
        workspace: &str,
        id: &str,
        opts: &RunOptions,
        cancel: watch::Receiver<bool>,
        lease: Option<Lease>,
    ) -> Result<RunOutcome> {
        let (_owner, prepared, _) = self
            .prepare_requirement(actor, workspace, id, &cancel)
            .await?;
        self.finish_requirement(
            actor,
            workspace,
            prepared,
            opts,
            cancel,
            lease,
            Marks::default(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_requirement(
        &self,
        actor: &str,
        workspace: &str,
        prepared: Prepared,
        opts: &RunOptions,
        cancel: watch::Receiver<bool>,
        lease: Option<Lease>,
        marks: Marks,
    ) -> Result<RunOutcome> {
        if *cancel.borrow() {
            return Err(Error::domain("CANCELLED", "Requirement run cancelled"));
        }
        if let Some(l) = &lease {
            l.running();
        }
        let evaluated = self
            .evaluate_prepared(&prepared, opts, cancel.clone(), lease)
            .await?;
        if *cancel.borrow() {
            return Err(Error::domain("CANCELLED", "Requirement run cancelled"));
        }
        let (eval, error) = match evaluated {
            Ok(eval) => (eval, None),
            Err(error) => (None, Some(error)),
        };
        let mut prepared = prepared;
        if let Some(error) = error {
            // The plan cannot be run as written (unsupported state, a
            // measurement spanning models): the user must see why.
            prepared.run = None;
            prepared.bind_failed = true;
            prepared.notes.push(error.to_string());
        }
        let make = |previous: Option<&Value>| assemble(&prepared, eval.as_ref(), previous, &marks);
        let result = if opts.store {
            self.store_requirement_result(actor, workspace, &prepared, &cancel, make)
                .await?
        } else {
            let (_, entry) = self.approved_entry(actor, workspace, &prepared.id).await?;
            make(entry.last_result.as_ref())
        };
        Ok(RunOutcome { result, eval })
    }

    /// Evaluate an approved requirement without the run queue and wait for the
    /// result. Embedders and tests use this; the agent tool goes through
    /// [`Core::control_requirements`].
    pub async fn evaluate_requirement(
        &self,
        actor: &str,
        workspace_id: &str,
        id: &str,
        opts: &RunOptions,
    ) -> Result<RunOutcome> {
        self.ensure_actor_active(actor)?;
        let (_keep, cancel) = watch::channel(false);
        self.run_requirement_inner(actor, workspace_id, id, opts, cancel, None)
            .await
    }

    fn announce_run(&self, workspaces: &[String], actor: &str) {
        for workspace_id in workspaces {
            let _ = self.activity.send(crate::events::Activity {
                workspace_id: workspace_id.clone(),
                kind: "requirement_run_changed".into(),
                job: None,
                actor: Some(actor.into()),
                tool: Some("control_requirements".into()),
            });
        }
    }

    /// Admit a run of `id` and execute `work` under the lease with its
    /// watchers; the receiver yields the outcome. Emits
    /// `requirement_run_changed` when the run starts and ends.
    async fn spawn_requirement_run<F, Fut>(
        &self,
        actor: &str,
        owner: &str,
        workspace: &str,
        id: &str,
        work: F,
    ) -> Result<(Lease, tokio::sync::oneshot::Receiver<Result<Value>>)>
    where
        F: FnOnce(Core, watch::Receiver<bool>, Lease) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
    {
        let revision = {
            let all = self.workspace_metadata(owner).await?;
            all[owner].stored.current.clone()
        };
        let lease = self.requirement_runs.start(owner, &revision, id, actor)?;
        let workspaces: Vec<String> = BTreeSet::from([owner.to_owned(), workspace.to_owned()])
            .into_iter()
            .collect();
        self.announce_run(&workspaces, actor);
        let core = self.clone();
        let actor = actor.to_owned();
        let task_lease = lease.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _watchers = task_lease.watch(core.actor_signal(&actor));
            let mut result = work(core.clone(), task_lease.signal(), task_lease.clone()).await;
            let status = task_lease.finish(&mut result);
            if let (Ok(value), Some(status)) = (&mut result, status) {
                let info = json!(RunInfo::from(status));
                value["run"] = info.clone();
                value["id"] = info["id"].clone();
                value["state"] = info["state"].clone();
            }
            core.announce_run(&workspaces, &actor);
            let _ = send.send(result);
        });
        Ok((lease, receive))
    }

    /// `run` and `start_run` of the agent tool. Returns the run state and, for
    /// a waited run, the stored result.
    pub(crate) async fn requirement_run(
        &self,
        actor: &str,
        workspace_id: &str,
        id: &str,
        background: bool,
    ) -> Result<Value> {
        let (owner, _) = self.approved_entry(actor, workspace_id, id).await?;
        let (lease, receive) = {
            let workspace = workspace_id.to_owned();
            let actor_name = actor.to_owned();
            let id_owned = id.to_owned();
            self.spawn_requirement_run(
                actor,
                &owner,
                workspace_id,
                id,
                move |core, cancel, lease| async move {
                    let outcome = core
                        .run_requirement_inner(
                            &actor_name,
                            &workspace,
                            &id_owned,
                            &RunOptions::default(),
                            cancel,
                            Some(lease),
                        )
                        .await?;
                    Ok(json!({"requirement_id": id_owned, "result": outcome.result}))
                },
            )
            .await?
        };
        if background {
            let info = json!(RunInfo::from(lease.status()));
            return Ok(
                json!({"requirement_id": id, "id": info["id"], "state": info["state"], "run": info}),
            );
        }
        // Dropping an inline request cancels the run it owns; an entered
        // worker call and the result commit are never abandoned.
        let _cancel = queue::CancelOnDrop(lease);
        receive
            .await
            .map_err(|_| invalid("Requirement run task ended unexpectedly"))?
    }

    pub(crate) async fn requirement_cancel(
        &self,
        actor: &str,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<RunInfo>> {
        let owner = self.requirement_owner(workspace_id).await?;
        self.requirement_runs.stop(&owner, run_id, actor)?;
        Ok(self.requirement_run_status(&owner))
    }

    /// Coalesce rebuilds of the same view while another build is being checked.
    /// Private builds for snapshots and replay never enter this queue.
    pub(crate) fn schedule_requirement_auto_run(
        &self,
        owner: &str,
        workspace: &str,
        revision: &str,
    ) {
        {
            use std::collections::hash_map::Entry;
            let mut pending = self.requirement_auto_runs.lock().unwrap();
            match pending.entry(owner.to_owned()) {
                Entry::Occupied(mut entry) => {
                    let queue = entry.get_mut();
                    queue.retain(|(view, _)| view != workspace);
                    queue.push_back((workspace.to_owned(), revision.to_owned()));
                    return;
                }
                Entry::Vacant(entry) => {
                    entry.insert([(workspace.to_owned(), revision.to_owned())].into());
                }
            }
        }
        let core = self.clone();
        let owner = owner.to_owned();
        tokio::spawn(async move {
            loop {
                let next = {
                    let mut pending = core.requirement_auto_runs.lock().unwrap();
                    let next = pending.get_mut(&owner).and_then(|queue| queue.pop_front());
                    if next.is_none() {
                        pending.remove(&owner);
                    }
                    next
                };
                let Some((workspace, revision)) = next else {
                    break;
                };
                let current = core
                    .workspace_metadata(&workspace)
                    .await
                    .ok()
                    .and_then(|all| all.get(&workspace).map(|ws| ws.stored.current.clone()));
                if current.as_deref() == Some(&revision) {
                    let _ = core.auto_run_requirements("local-user", &workspace).await;
                }
            }
        });
    }

    /// Backend of the background auto-run (spec §6.4, P1.82). Every affected approved requirement whose
    /// estimated cost is within `auto_run_budget_secs` is re-run (one at a time,
    /// through the run queue); the others are marked pending and keep their
    /// previous result for an on-demand run. Requirements that cannot run
    /// (needs review, error) get that status stored. Emits
    /// `requirement_run_changed` and `requirements_changed` so the UI refreshes.
    pub async fn auto_run_requirements(
        &self,
        actor: &str,
        workspace_id: &str,
    ) -> Result<AutoRunReport> {
        self.ensure_actor_active(actor)?;
        let _memory = self.pin_workspace(workspace_id).await?;
        let settings = project_settings();
        let budget = f64::from(settings.auto_run_budget_secs);
        let mut report = AutoRunReport {
            budget_s: settings.auto_run_budget_secs,
            ..Default::default()
        };
        let (owner, catalog) = self.requirement_catalog(actor, workspace_id).await?;
        let model = self.requirement_view_model(workspace_id).await?;
        let ids: Vec<String> = catalog
            .iter()
            .filter(|(_, saved)| {
                saved
                    .requirement()
                    .and_then(|entry| entry.approved.as_ref())
                    .is_some_and(|approved| {
                        model
                            .as_ref()
                            .is_none_or(|model| referenced_models(approved).contains(model))
                    })
            })
            .map(|(id, _)| id.clone())
            .collect();
        let (_idle, idle_cancel) = watch::channel(false);
        for id in ids {
            let (_, prepared, previous) = match self
                .prepare_requirement(actor, workspace_id, &id, &idle_cancel)
                .await
            {
                Ok(p) => p,
                Err(error) => {
                    report.failed.push(failure(&id, &error));
                    continue;
                }
            };
            let pool = pool_size(self, None);
            let Some(estimate) = estimate_cost_s(&prepared, previous.as_ref(), pool) else {
                // Cannot evaluate: store needs_review / error without a run.
                let stored = self
                    .store_requirement_result(actor, workspace_id, &prepared, &idle_cancel, |p| {
                        assemble(&prepared, None, p, &Marks::default())
                    })
                    .await;
                match stored {
                    Ok(_) => report.blocked.push(id),
                    Err(error) => report.failed.push(failure(&id, &error)),
                }
                continue;
            };
            if estimate > budget {
                let marks = Marks {
                    stale: Some((estimate, budget)),
                    estimate_s: Some(estimate),
                    ..Default::default()
                };
                let stored = self
                    .store_requirement_result(actor, workspace_id, &prepared, &idle_cancel, |p| {
                        assemble(&prepared, None, p, &marks)
                    })
                    .await;
                match stored {
                    Ok(_) => report.stale.push(StaleRequirement {
                        id,
                        estimate_s: estimate,
                    }),
                    Err(error) => report.failed.push(failure(&id, &error)),
                }
                continue;
            }
            // Queue state is transient. Preserve the previous result until
            // the admitted run finishes and commits a replacement.
            let marks = Marks {
                estimate_s: Some(estimate),
                ..Default::default()
            };
            match self
                .auto_run_one(actor, &owner, workspace_id, prepared.clone(), marks)
                .await
            {
                Ok(()) => report.ran.push(id),
                Err(error) => report.failed.push(failure(&id, &error)),
            }
        }
        Ok(report)
    }

    async fn auto_run_one(
        &self,
        actor: &str,
        owner: &str,
        workspace: &str,
        prepared: Prepared,
        marks: Marks,
    ) -> Result<()> {
        let started = tokio::time::Instant::now();
        loop {
            let id = prepared.id.clone();
            let workspace_owned = workspace.to_owned();
            let actor_owned = actor.to_owned();
            let attempt = self
                .spawn_requirement_run(actor, owner, workspace, &id, {
                    let prepared = prepared.clone();
                    let marks = marks.clone();
                    move |core, cancel, lease| async move {
                        let outcome = core
                            .finish_requirement(
                                &actor_owned,
                                &workspace_owned,
                                prepared,
                                &RunOptions::default(),
                                cancel,
                                Some(lease),
                                marks,
                            )
                            .await?;
                        Ok(outcome.result)
                    }
                })
                .await;
            match attempt {
                Ok((lease, receive)) => {
                    let _cancel = queue::CancelOnDrop(lease);
                    return receive
                        .await
                        .map_err(|_| invalid("Requirement run task ended unexpectedly"))?
                        .map(|_| ());
                }
                Err(error)
                    if error.code() == "RESOURCE_LIMIT"
                        && started.elapsed().as_secs() < AUTO_RUN_PATIENCE_S =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn failure(id: &str, error: &Error) -> FailedRequirement {
    FailedRequirement {
        id: id.to_owned(),
        code: error.code().to_owned(),
        message: error.to_string().chars().take(500).collect(),
    }
}
