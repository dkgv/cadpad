//! Plan to worker translation (spec §2, §6.1): a pure function from plan jobs
//! to `evaluate_plan_chunk` request JSON, chunked by state, and a parser for
//! the response into typed raw results. The evaluator is dumb: it only
//! returns numbers. No worker call happens here; [`PlanExecutor`] is the seam
//! the live wiring implements, and [`FakeExecutor`] stands in for tests.
//!
//! Part keys in the plan are `entry#builder/name`; the worker knows published
//! names of one subject, so a request is made per model and names are
//! prefixed back on the way in. The live executor adds the build `revision`
//! to the request params.
use super::bind::{model_key, part_ref_key};
use super::ir::{AxisRef, ParamRef, Target};
use super::manifest::Manifest;
use super::plan::Plan;
use super::quantity::Unit;
use super::settings::ProjectSettings;
use super::states::{ConcreteState, StateValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const EVAL_FORMAT: &str = "requirements-eval-v1";

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum EvalError {
    #[error("{claim}: {reason}")]
    UnsupportedState { claim: String, reason: String },
    #[error("{claim}: a measurement spans several models ({})", .parts.join(", "))]
    CrossModel { claim: String, parts: Vec<String> },
    #[error("worker chunk was cancelled ({0}); nothing is claimed")]
    Cancelled(String),
    #[error("bad worker response: {0}")]
    BadResponse(String),
    #[error("executor failed: {0}")]
    Executor(String),
}

/// Knobs of the translation.
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkOptions {
    /// Worker states per request (at least 1).
    pub chunk_size: usize,
    pub pair_time_limit_s: Option<f64>,
    pub chunk_time_limit_s: Option<f64>,
    pub workers: Option<usize>,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            chunk_size: 32,
            pair_time_limit_s: None,
            chunk_time_limit_s: None,
            workers: None,
        }
    }
}

impl ChunkOptions {
    pub fn from_settings(s: &ProjectSettings) -> Self {
        Self {
            pair_time_limit_s: Some(f64::from(s.pair_time_limit_secs)),
            ..Self::default()
        }
    }
}

/// Where a worker measurement came from in the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeasurementRef {
    /// Worker measurement id (`m<n>`), unique across the whole plan.
    pub id: String,
    /// Worker state id (`s<n>`) within the chunk.
    pub state_id: String,
    /// Index into `Plan::claims`.
    pub claim: usize,
    /// Index into that claim's `jobs`.
    pub job: usize,
}

/// One `evaluate_plan_chunk` call.
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkRequest {
    pub chunk_id: String,
    /// `entry#builder` of the subject this chunk runs on.
    pub model: String,
    /// The JSON-RPC params.
    pub params: Value,
    pub measurements: Vec<MeasurementRef>,
}

/// Split `entry#builder/name` into (`entry#builder`, `name`).
pub fn split_part_key(key: &str) -> Option<(&str, &str)> {
    let hash = key.find('#')?;
    let slash = key[hash..].find('/')? + hash;
    Some((&key[..slash], &key[slash + 1..]))
}

fn names_of(
    claim: &str,
    keys: &[String],
    model: &mut Option<String>,
) -> Result<Vec<String>, EvalError> {
    let mut out = Vec::new();
    for k in keys {
        let (m, n) = split_part_key(k).ok_or_else(|| EvalError::UnsupportedState {
            claim: claim.to_string(),
            reason: format!("malformed part key {k:?}"),
        })?;
        match model {
            Some(prev) if prev != m => {
                return Err(EvalError::CrossModel {
                    claim: claim.to_string(),
                    parts: keys.to_vec(),
                });
            }
            _ => *model = Some(m.to_string()),
        }
        out.push(n.to_string());
    }
    Ok(out)
}

fn target_keys(t: &Target, manifest: &Manifest) -> Vec<String> {
    let mut keys: Vec<String> = match t {
        Target::Part(p) => manifest
            .parts
            .get(&part_ref_key(p))
            .map(|v| v.iter().map(|l| l.key.clone()).collect())
            .unwrap_or_default(),
        Target::Set(s) => manifest
            .sets
            .get(&s.0)
            .map(|v| v.iter().map(|l| l.key.clone()).collect())
            .unwrap_or_default(),
        Target::Parts(ps) => ps
            .iter()
            .flat_map(|p| {
                manifest
                    .parts
                    .get(&part_ref_key(p))
                    .into_iter()
                    .flatten()
                    .map(|l| l.key.clone())
            })
            .collect(),
    };
    keys.sort();
    keys.dedup();
    keys
}

fn axis_vector(claim: &str, a: &AxisRef) -> Result<[f64; 3], EvalError> {
    match a {
        AxisRef::X => Ok([1.0, 0.0, 0.0]),
        AxisRef::Y => Ok([0.0, 1.0, 0.0]),
        AxisRef::Z => Ok([0.0, 0.0, 1.0]),
        AxisRef::Vector(v) => Ok(*v),
        AxisRef::Feature(_) => Err(EvalError::UnsupportedState {
            claim: claim.to_string(),
            reason: "feature-derived axes are not evaluated yet; name x, y, z or a vector".into(),
        }),
    }
}

fn axis_json(claim: &str, a: &AxisRef) -> Result<Value, EvalError> {
    Ok(match a {
        AxisRef::X => json!("x"),
        AxisRef::Y => json!("y"),
        AxisRef::Z => json!("z"),
        other => json!(axis_vector(claim, other)?),
    })
}

fn param_key(p: &ParamRef, manifest: &Manifest) -> Option<String> {
    match &p.model {
        Some(m) => Some(format!("{}:{}", model_key(m), p.name)),
        None => {
            let suffix = format!(":{}", p.name);
            let mut hits = manifest.params.keys().filter(|k| k.ends_with(&suffix));
            let first = hits.next()?;
            hits.next().is_none().then(|| first.clone())
        }
    }
}

/// A state as the worker takes it: `params` plus ordered rigid `moves`.
pub(super) struct WorkerState {
    pub(super) params: BTreeMap<String, f64>,
    pub(super) moves: Vec<Value>,
}

pub(super) fn worker_state(
    claim: &str,
    state: &ConcreteState,
    manifest: &Manifest,
    model: &mut Option<String>,
) -> Result<WorkerState, EvalError> {
    let mut out = WorkerState {
        params: BTreeMap::new(),
        moves: vec![],
    };
    let unsupported = |reason: String| EvalError::UnsupportedState {
        claim: claim.to_string(),
        reason,
    };
    for v in &state.values {
        match v {
            StateValue::Param { param, value } => {
                let key = param_key(param, manifest);
                if let Some((m, _)) = key.as_deref().and_then(|k| k.rsplit_once(':'))
                    && let Some(have) = model.as_deref()
                    && have != m
                {
                    return Err(EvalError::CrossModel {
                        claim: claim.to_string(),
                        parts: vec![format!("{m}:{}", param.name)],
                    });
                }
                // Send the value in the unit the model declares the parameter in.
                let value = key
                    .as_ref()
                    .and_then(|k| manifest.params.get(k))
                    .and_then(|l| {
                        if l.value.unit == value.unit {
                            Some(*value) // no round trip through internal units
                        } else {
                            value.convert_to(l.value.unit).ok()
                        }
                    })
                    .unwrap_or(*value);
                out.params.insert(param.name.clone(), value.value);
            }
            StateValue::Rotate {
                moving,
                axis,
                angle,
            } => {
                let deg = if angle.unit == Unit::Deg {
                    *angle
                } else {
                    angle
                        .convert_to(Unit::Deg)
                        .map_err(|e| unsupported(e.to_string()))?
                };
                let names = names_of(claim, &target_keys(moving, manifest), model)?;
                out.moves.push(json!({
                    "op": "rotate", "parts": names, "axis": axis_json(claim, axis)?,
                    "origin": [0, 0, 0], "angle_deg": deg.value,
                }));
            }
            StateValue::Translate {
                moving,
                direction,
                distance,
            } => {
                let mm = if distance.unit == Unit::Mm {
                    *distance
                } else {
                    distance
                        .convert_to(Unit::Mm)
                        .map_err(|e| unsupported(e.to_string()))?
                };
                let d = axis_vector(claim, direction)?;
                let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                if n == 0.0 || !n.is_finite() {
                    return Err(unsupported("translation direction is zero".into()));
                }
                let names = names_of(claim, &target_keys(moving, manifest), model)?;
                out.moves.push(json!({
                    "op": "translate", "parts": names,
                    "vector": [d[0] / n * mm.value, d[1] / n * mm.value, d[2] / n * mm.value],
                }));
            }
            StateValue::Joint { joint, .. } => {
                return Err(unsupported(format!(
                    "joint state {:?} is not evaluated yet",
                    joint.name
                )));
            }
        }
    }
    Ok(out)
}

struct PendingState {
    key: String,
    params: BTreeMap<String, f64>,
    moves: Vec<Value>,
    measurements: Vec<(MeasurementRef, Value)>,
}

/// The worker requests for a whole plan, deterministic in plan order: one or
/// more chunks per subject model, `opts.chunk_size` distinct worker states
/// each. States identical across claims share a worker state.
pub fn build_requests(
    plan: &Plan,
    manifest: &Manifest,
    opts: &ChunkOptions,
) -> Result<Vec<ChunkRequest>, EvalError> {
    // model -> states in first-seen order
    let mut by_model: BTreeMap<String, Vec<PendingState>> = BTreeMap::new();
    let mut next_m = 0usize;
    for (ci, claim) in plan.claims.iter().enumerate() {
        let states = manifest.states.get(&claim.id);
        for (ji, job) in claim.jobs.iter().enumerate() {
            let mut model: Option<String> = None;
            let a = names_of(&claim.id, &job.a, &mut model)?;
            let b = names_of(&claim.id, &job.b, &mut model)?;
            let state = states
                .and_then(|s| s.get(job.state))
                .cloned()
                .unwrap_or_default();
            let ws = worker_state(&claim.id, &state, manifest, &mut model)?;
            let model = model.unwrap_or_default();
            let mut spec = json!({"primitive": claim.primitive, "a": a});
            match claim.primitive.as_str() {
                "outside_volume" => spec["env"] = json!(b),
                "extent" => {
                    let axis = claim
                        .axis
                        .as_ref()
                        .ok_or_else(|| EvalError::UnsupportedState {
                            claim: claim.id.clone(),
                            reason: "extent without an axis".into(),
                        })?;
                    spec["axis"] = axis_json(&claim.id, axis)?;
                }
                "valid_solid_count" => {}
                _ => spec["b"] = json!(b),
            }
            let key = serde_json::to_string(&json!({"p": ws.params, "m": ws.moves}))
                .expect("state key serializes");
            let list = by_model.entry(model).or_default();
            let at = match list.iter().position(|s| s.key == key) {
                Some(i) => i,
                None => {
                    list.push(PendingState {
                        key,
                        params: ws.params,
                        moves: ws.moves,
                        measurements: vec![],
                    });
                    list.len() - 1
                }
            };
            next_m += 1;
            list[at].measurements.push((
                MeasurementRef {
                    id: format!("m{next_m}"),
                    state_id: String::new(),
                    claim: ci,
                    job: ji,
                },
                spec,
            ));
        }
    }
    let size = opts.chunk_size.max(1);
    let mut out = Vec::new();
    let mut next_c = 0usize;
    for (model, states) in by_model {
        for chunk in states.chunks(size) {
            next_c += 1;
            let mut refs = Vec::new();
            let mut state_json = Vec::new();
            for (si, st) in chunk.iter().enumerate() {
                let sid = format!("s{si}");
                let mut ms = Vec::new();
                for (r, spec) in &st.measurements {
                    let mut spec = spec.clone();
                    spec["id"] = json!(r.id);
                    ms.push(spec);
                    refs.push(MeasurementRef {
                        state_id: sid.clone(),
                        ..r.clone()
                    });
                }
                let mut s = json!({"id": sid, "measurements": ms});
                if !st.params.is_empty() {
                    s["params"] = json!(st.params);
                }
                if !st.moves.is_empty() {
                    s["moves"] = json!(st.moves);
                }
                state_json.push(s);
            }
            let mut limits = serde_json::Map::new();
            if let Some(v) = opts.pair_time_limit_s {
                limits.insert("pair_time_limit_s".into(), json!(v));
            }
            if let Some(v) = opts.chunk_time_limit_s {
                limits.insert("chunk_time_limit_s".into(), json!(v));
            }
            if let Some(v) = opts.workers {
                limits.insert("workers".into(), json!(v));
            }
            let chunk_id = format!("c{next_c}");
            out.push(ChunkRequest {
                chunk_id: chunk_id.clone(),
                model: model.clone(),
                params: json!({"chunk_id": chunk_id, "limits": limits, "states": state_json}),
                measurements: refs,
            });
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ responses

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasStatus {
    #[default]
    Ok,
    Inconclusive,
    Error,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RawItem {
    pub a: String,
    #[serde(default)]
    pub b: Option<String>,
    #[serde(default)]
    pub status: MeasStatus,
    #[serde(default)]
    pub value: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RawMeasurement {
    pub id: String,
    #[serde(default)]
    pub primitive: String,
    #[serde(default)]
    pub status: MeasStatus,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub witness: Option<Vec<String>>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub items: Vec<RawItem>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct RawError {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RawState {
    pub id: String,
    #[serde(default = "ok_state")]
    pub status: String,
    #[serde(default)]
    pub error: Option<RawError>,
    #[serde(default)]
    pub measurements: Vec<RawMeasurement>,
}

fn ok_state() -> String {
    "ok".into()
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct MeasurementTiming {
    #[serde(default)]
    pub state: String,
    pub id: String,
    #[serde(default)]
    pub broad_s: f64,
    #[serde(default)]
    pub job_cpu_s: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct RawTimings {
    #[serde(default)]
    pub total_s: f64,
    #[serde(default)]
    pub measurements: Vec<MeasurementTiming>,
}

/// A parsed `evaluate_plan_chunk` result. Names are already prefixed back to
/// part keys.
#[derive(Clone, Debug, PartialEq)]
pub struct RawChunk {
    pub chunk_id: Option<String>,
    pub states: Vec<RawState>,
    pub results_sha256: Option<String>,
    pub timings: RawTimings,
}

#[derive(Deserialize)]
struct WireChunk {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    chunk_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    results: Option<WireResults>,
    #[serde(default)]
    results_sha256: Option<String>,
    #[serde(default)]
    timings: RawTimings,
}

#[derive(Deserialize)]
struct WireResults {
    #[serde(default)]
    states: Vec<RawState>,
}

/// Parse the `result` object of an `evaluate_plan_chunk` reply. A cancelled
/// chunk is an error: it never contributes partial results. Items are put in
/// sorted order so the order the worker produced them in cannot matter.
pub fn parse_chunk_response(req: &ChunkRequest, value: &Value) -> Result<RawChunk, EvalError> {
    let wire: WireChunk =
        serde_json::from_value(value.clone()).map_err(|e| EvalError::BadResponse(e.to_string()))?;
    if wire.format.as_deref() != Some(EVAL_FORMAT) {
        return Err(EvalError::BadResponse(format!(
            "format {:?}, expected {EVAL_FORMAT}",
            wire.format
        )));
    }
    if wire.status.as_deref() == Some("cancelled") {
        return Err(EvalError::Cancelled(
            wire.reason.unwrap_or_else(|| "cancelled".into()),
        ));
    }
    let results = wire
        .results
        .ok_or_else(|| EvalError::BadResponse("complete chunk without results".into()))?;
    let prefix = |n: &str| format!("{}/{n}", req.model);
    let mut states = results.states;
    for s in &mut states {
        for m in &mut s.measurements {
            if let Some(w) = &mut m.witness {
                for n in w.iter_mut() {
                    *n = prefix(n);
                }
            }
            for it in &mut m.items {
                it.a = prefix(&it.a);
                it.b = it.b.as_deref().map(prefix);
            }
            m.items.sort_by(|x, y| {
                (&x.a, &x.b).cmp(&(&y.a, &y.b)).then(
                    x.value
                        .partial_cmp(&y.value)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
            });
        }
        s.measurements.sort_by(|x, y| x.id.cmp(&y.id));
    }
    states.sort_by(|x, y| x.id.cmp(&y.id));
    Ok(RawChunk {
        chunk_id: wire.chunk_id,
        states,
        results_sha256: wire.results_sha256,
        timings: wire.timings,
    })
}

/// Raw outcome of one plan job.
#[derive(Clone, Debug, PartialEq)]
pub struct JobOutcome {
    pub status: MeasStatus,
    /// In the primitive's internal unit.
    pub value: Option<f64>,
    pub witness: Option<Vec<String>>,
    /// Error text, or `pair_time_limit`.
    pub reason: Option<String>,
    /// Inconclusive pairs named by the worker (sorted).
    pub slow: Vec<(String, Option<String>)>,
}

/// Everything a run measured, keyed by (claim index, job index).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawResults {
    pub jobs: BTreeMap<(usize, usize), JobOutcome>,
    /// Worker seconds per primitive claim id (broad phase plus job cpu).
    pub claim_seconds: BTreeMap<String, f64>,
    /// Wall seconds, summed over chunks.
    pub total_s: f64,
}

/// Match parsed chunks back to plan jobs. A job the worker did not answer is
/// an error outcome, never a silent pass.
pub fn collect_results(plan: &Plan, chunks: &[(&ChunkRequest, RawChunk)]) -> RawResults {
    let mut out = RawResults::default();
    for (req, chunk) in chunks {
        out.total_s += chunk.timings.total_s;
        let mut answered: BTreeMap<(&str, &str), (&RawState, Option<&RawMeasurement>)> =
            BTreeMap::new();
        for s in &chunk.states {
            if s.measurements.is_empty() {
                continue;
            }
            for m in &s.measurements {
                answered.insert((s.id.as_str(), m.id.as_str()), (s, Some(m)));
            }
        }
        let state_by_id: BTreeMap<&str, &RawState> =
            chunk.states.iter().map(|s| (s.id.as_str(), s)).collect();
        for r in &req.measurements {
            let outcome = match answered.get(&(r.state_id.as_str(), r.id.as_str())) {
                Some((s, Some(m))) if s.status != "error" => JobOutcome {
                    status: m.status,
                    value: if m.status == MeasStatus::Ok {
                        m.value
                    } else {
                        None
                    },
                    witness: m.witness.clone(),
                    reason: m.reason.clone(),
                    slow: m
                        .items
                        .iter()
                        .filter(|i| i.status == MeasStatus::Inconclusive)
                        .map(|i| (i.a.clone(), i.b.clone()))
                        .collect(),
                },
                _ => {
                    let msg = state_by_id
                        .get(r.state_id.as_str())
                        .and_then(|s| s.error.as_ref())
                        .map(|e| format!("{}: {}", e.code, e.message))
                        .unwrap_or_else(|| "worker returned no result".to_string());
                    JobOutcome {
                        status: MeasStatus::Error,
                        value: None,
                        witness: None,
                        reason: Some(msg),
                        slow: vec![],
                    }
                }
            };
            out.jobs.insert((r.claim, r.job), outcome);
        }
        let claim_of: BTreeMap<&str, usize> = req
            .measurements
            .iter()
            .map(|r| (r.id.as_str(), r.claim))
            .collect();
        for t in &chunk.timings.measurements {
            if let Some(&ci) = claim_of.get(t.id.as_str()) {
                *out.claim_seconds
                    .entry(plan.claims[ci].id.clone())
                    .or_default() += t.broad_s + t.job_cpu_s;
            }
        }
    }
    out
}

// ------------------------------------------------------------------- executor

/// Runs one chunk on a worker. The live implementation sends
/// `req.params` (plus the build revision) as `evaluate_plan_chunk` and feeds
/// the reply to [`parse_chunk_response`].
pub trait PlanExecutor {
    fn run_chunk(&mut self, req: &ChunkRequest) -> Result<RawChunk, EvalError>;
}

/// Build requests, execute every chunk and collect the raw results. Any
/// failed or cancelled chunk fails the run: no partial result.
pub fn run_plan(
    plan: &Plan,
    manifest: &Manifest,
    exec: &mut dyn PlanExecutor,
    opts: &ChunkOptions,
) -> Result<RawResults, EvalError> {
    let requests = build_requests(plan, manifest, opts)?;
    let mut parsed = Vec::with_capacity(requests.len());
    for r in &requests {
        parsed.push(exec.run_chunk(r)?);
    }
    let pairs: Vec<(&ChunkRequest, RawChunk)> = requests.iter().zip(parsed).collect();
    Ok(collect_results(plan, &pairs))
}

/// Test executor: answers every measurement through a closure that gets the
/// state spec and the measurement spec (request JSON) and returns the
/// worker's measurement JSON (`status`, `value`, `witness`, `items`, ...; `id`
/// and `primitive` are filled in). The reply goes through the real JSON
/// parser. `shuffle` reverses the order of states, measurements and items to
/// prove order independence.
pub struct FakeExecutor<F>
where
    F: FnMut(&Value, &Value) -> Value,
{
    answer: F,
    pub shuffle: bool,
    pub requests: Vec<ChunkRequest>,
    /// When set, every chunk comes back cancelled.
    pub cancel: bool,
}

impl<F: FnMut(&Value, &Value) -> Value> FakeExecutor<F> {
    pub fn new(answer: F) -> Self {
        Self {
            answer,
            shuffle: false,
            requests: vec![],
            cancel: false,
        }
    }
}

impl<F: FnMut(&Value, &Value) -> Value> PlanExecutor for FakeExecutor<F> {
    fn run_chunk(&mut self, req: &ChunkRequest) -> Result<RawChunk, EvalError> {
        self.requests.push(req.clone());
        if self.cancel {
            let v = json!({"format": EVAL_FORMAT, "status": "cancelled",
                           "reason": "chunk_time_limit", "results": null});
            return parse_chunk_response(req, &v);
        }
        let mut states = Vec::new();
        for s in req.params["states"].as_array().cloned().unwrap_or_default() {
            let mut ms = Vec::new();
            for m in s["measurements"].as_array().cloned().unwrap_or_default() {
                let mut r = (self.answer)(&s, &m);
                r["id"] = m["id"].clone();
                r["primitive"] = m["primitive"].clone();
                if self.shuffle
                    && let Some(items) = r["items"].as_array_mut()
                {
                    items.reverse();
                }
                ms.push(r);
            }
            if self.shuffle {
                ms.reverse();
            }
            states.push(json!({"id": s["id"], "status": "ok", "measurements": ms}));
        }
        if self.shuffle {
            states.reverse();
        }
        let v = json!({
            "format": EVAL_FORMAT, "chunk_id": req.chunk_id, "status": "complete",
            "results": {"states": states},
            "timings": {"total_s": 0.0, "measurements": []},
        });
        parse_chunk_response(req, &v)
    }
}
