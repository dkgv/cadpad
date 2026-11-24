//! Evaluation (spec §6.2, P1.54): raw measurements in, results out. The
//! worker only measures; every decision lives here: allowances are applied
//! before comparison, comparators and quantifiers decide met / not met, and
//! claims aggregate to a requirement status. Pure and deterministic: the same
//! raw numbers always give the same [`ResultBody`] (and hash); timings and
//! warnings live beside it, outside the hash.
//!
//! Combination rules (all deterministic, no wall-clock input except the
//! worker's own `inconclusive`):
//!
//! * within a state, pairs combine with *all* (`Whole`, `Each`, `EachPair`)
//!   or *any* (`AnyPair`); across states, `for_all` is *all*, `exists` is
//!   *any*; claims combine as *all*;
//! * *all*: error beats not_met beats inconclusive beats met;
//! * *any*: met beats error beats inconclusive beats not_met, so a found
//!   witness is not undone by a state that timed out.
use super::diagnostics::{Diagnostic, DiagnosticCode};
use super::expand::Pairing;
use super::findings::{Finding, FindingState};
use super::ir::{AxisRef, Comparator, Literal, Quantifier, Target};
use super::library::library;
use super::manifest::{Manifest, canonical_hash};
use super::plan::{ClaimPlan, Job, Plan};
use super::quantity::{Quantity, Unit};
use super::settings::ProjectSettings;
use super::states::{ConcreteState, StateValue};
use super::status::RequirementStatus;
use super::worker_plan::{JobOutcome, MeasStatus, RawResults};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const RESULT_FORMAT: &str = "requirements-result-v1";
/// Failing states listed per primitive claim (the count is always exact).
pub const MAX_FAILING_STATES: usize = 64;
/// Inconclusive pairs and error messages listed per primitive claim.
pub const MAX_LISTED: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Met,
    NotMet,
    /// A pair hit its time limit: nothing is claimed.
    Inconclusive,
    /// A measurement failed (or was not answered).
    Error,
}

fn all_of(it: impl IntoIterator<Item = CheckStatus>) -> CheckStatus {
    let v: Vec<_> = it.into_iter().collect();
    for s in [
        CheckStatus::Error,
        CheckStatus::NotMet,
        CheckStatus::Inconclusive,
    ] {
        if v.contains(&s) {
            return s;
        }
    }
    CheckStatus::Met
}

fn any_of(it: impl IntoIterator<Item = CheckStatus>) -> CheckStatus {
    let v: Vec<_> = it.into_iter().collect();
    for s in [
        CheckStatus::Met,
        CheckStatus::Error,
        CheckStatus::Inconclusive,
    ] {
        if v.contains(&s) {
            return s;
        }
    }
    CheckStatus::NotMet
}

/// The parts a job measured.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct PairRef {
    pub a: Vec<String>,
    pub b: Vec<String>,
}

/// The state and pair that decide a claim: the worst one for `for_all`, the
/// witness for `exists`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Worst {
    /// Index into the claim's states.
    pub state: usize,
    pub state_label: String,
    pub pair: PairRef,
    /// The worker's argmax / argmin pair inside a set job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<Vec<String>>,
    pub value: Option<Quantity>,
    /// Signed distance to failing, in the comparator's unit; negative = missed.
    pub margin: Option<Quantity>,
    /// Index of the allowance the pair was measured under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowance: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SlowPair {
    pub state: usize,
    pub state_label: String,
    pub pair: PairRef,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PrimitiveResult {
    pub id: String,
    pub claim: String,
    pub primitive: String,
    pub quantifier: Quantifier,
    pub status: CheckStatus,
    pub met: Option<bool>,
    pub margin: Option<Quantity>,
    /// `for_all`: the most critical state and pair (also when met). `exists`
    /// not met: the closest attempt.
    pub worst: Option<Worst>,
    /// `exists` met: the state that satisfies it.
    pub witness: Option<Worst>,
    pub states: usize,
    pub jobs: usize,
    /// States that fail, ascending, at most [`MAX_FAILING_STATES`].
    pub failing_states: Vec<usize>,
    pub failing_state_count: usize,
    pub inconclusive: Vec<SlowPair>,
    pub inconclusive_count: usize,
    pub errors: Vec<String>,
    pub error_count: usize,
}

/// A rule or claim of the requirement: its primitive claims combined.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimResult {
    pub claim: String,
    pub rule: String,
    pub status: CheckStatus,
    pub met: Option<bool>,
    /// Margin and worst state of the first failing primitive claim, else of
    /// the first primitive claim.
    pub margin: Option<Quantity>,
    pub worst: Option<Worst>,
    pub witness: Option<Worst>,
    pub primitives: Vec<String>,
}

/// What a result depends on and says; this is what is hashed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResultBody {
    pub format: String,
    pub requirement_id: String,
    pub plan_hash: String,
    pub manifest_hash: String,
    pub library_version: String,
    pub settings_hash: String,
    pub status: RequirementStatus,
    pub unsupported: usize,
    pub claims: Vec<ClaimResult>,
    pub primitives: Vec<PrimitiveResult>,
    /// Failing runs, unlabelled (see [`EvalResult::findings_vs`]).
    pub findings: Vec<Finding>,
}

impl ResultBody {
    /// Canonical hash; stable under any worker ordering, excludes timings.
    pub fn hash(&self) -> String {
        canonical_hash(self)
    }
}

/// Wall and worker time of one run. Never part of the hash.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunTimings {
    pub total_s: f64,
    /// Seconds per IR claim, the sum of its primitive claims.
    pub claims: BTreeMap<String, f64>,
    /// Seconds per primitive claim.
    pub primitives: BTreeMap<String, f64>,
    /// The IR claim that cost the most.
    pub costliest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EvalResult {
    pub body: ResultBody,
    pub timings: RunTimings,
    /// `W_SLOW` when the run is over the settings budget.
    pub warnings: Vec<Diagnostic>,
}

impl EvalResult {
    pub fn hash(&self) -> String {
        self.body.hash()
    }
    /// This run's findings labelled against the previous run's (P1.60).
    pub fn findings_vs(&self, previous: Option<&ResultBody>) -> Vec<Finding> {
        super::findings::label_findings(
            &self.body.findings,
            previous.map(|p| p.findings.as_slice()).unwrap_or(&[]),
        )
    }
}

// ----------------------------------------------------------------- comparing

/// 12 significant digits, so float noise like `0.30000000000000004` does not
/// reach results.
fn clean(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    format!("{v:.11e}").parse().unwrap_or(v)
}

fn quantity(internal: f64, unit: Unit) -> Quantity {
    Quantity::new(clean(internal / unit.factor()), unit)
}

fn cmp_unit(c: &Comparator) -> Option<Unit> {
    match c {
        Comparator::Ge { value } | Comparator::Le { value } => Some(value.unit),
        Comparator::In { lo, .. } => Some(lo.unit),
        Comparator::Approx { target, .. } => Some(target.unit),
        Comparator::IsTrue {} | Comparator::IsFalse {} => None,
    }
}

/// Margin (signed distance to failing) in internal units, `None` for the
/// boolean comparators, and whether the comparator holds. `strict` makes the
/// limit exclusive.
fn judge(c: &Comparator, strict: bool, v: f64) -> (bool, Option<f64>) {
    let lim = |l: &Literal| l.quantity().internal_value();
    let margin = match c {
        Comparator::Ge { value } => Some(v - lim(value)),
        Comparator::Le { value } => Some(lim(value) - v),
        Comparator::In { lo, hi } => Some((v - lim(lo)).min(lim(hi) - v)),
        Comparator::Approx { target, tol } => Some(lim(tol) - (v - lim(target)).abs()),
        Comparator::IsTrue {} => return (v != 0.0, None),
        Comparator::IsFalse {} => return (v == 0.0, None),
    };
    let m = margin.expect("set above");
    (if strict { m > 0.0 } else { m >= 0.0 }, Some(m))
}

struct JobEval {
    status: CheckStatus,
    /// Margin in `unit`.
    margin: Option<f64>,
    value: Option<f64>,
    unit: Option<Unit>,
    allowance: Option<usize>,
}

fn eval_job(
    claim: &ClaimPlan,
    job: &Job,
    outcome: Option<&JobOutcome>,
    eps: &Quantity,
    result_unit: Unit,
) -> JobEval {
    let fail = |status| JobEval {
        status,
        margin: None,
        value: None,
        unit: Some(result_unit),
        allowance: job.allowed.as_ref().map(|a| a.allowance),
    };
    let Some(o) = outcome else {
        return fail(CheckStatus::Error);
    };
    match o.status {
        MeasStatus::Error => return fail(CheckStatus::Error),
        MeasStatus::Inconclusive => return fail(CheckStatus::Inconclusive),
        MeasStatus::Ok => {}
    }
    let Some(v) = o.value.filter(|v| v.is_finite()) else {
        return fail(CheckStatus::Error);
    };
    // An allowance replaces the claim's limit for its pair: the pair passes
    // while the overlap stays within what was allowed.
    let (cmp, strict) = match &job.allowed {
        Some(a) => {
            let max = a.max.unwrap_or(*eps);
            (
                Comparator::Le {
                    value: Literal::from(max),
                },
                false,
            )
        }
        None => (claim.cmp.clone(), claim.strict),
    };
    let (met, margin) = judge(&cmp, strict, v);
    let unit = cmp_unit(&cmp).unwrap_or(result_unit);
    JobEval {
        status: if met {
            CheckStatus::Met
        } else {
            CheckStatus::NotMet
        },
        margin: margin.map(|m| clean(m / unit.factor())),
        value: Some(v),
        unit: Some(unit),
        allowance: job.allowed.as_ref().map(|a| a.allowance),
    }
}

// -------------------------------------------------------------------- labels

fn axis_label(a: &AxisRef) -> String {
    match a {
        AxisRef::X => "x".into(),
        AxisRef::Y => "y".into(),
        AxisRef::Z => "z".into(),
        AxisRef::Vector(v) => format!("[{}, {}, {}]", v[0], v[1], v[2]),
        AxisRef::Feature(_) => "feature axis".into(),
    }
}

fn target_label(t: &Target) -> String {
    match t {
        Target::Part(p) => super::bind::part_ref_key(p),
        Target::Set(s) => format!("set {}", s.0),
        Target::Parts(ps) => ps
            .iter()
            .map(super::bind::part_ref_key)
            .collect::<Vec<_>>()
            .join("+"),
    }
}

/// A short deterministic description of a state, used in results and as the
/// state's identity across runs (`rest`, `door_angle = 108 deg`, ...).
pub fn state_label(s: &ConcreteState) -> String {
    if s.values.is_empty() {
        return "rest".into();
    }
    s.values
        .iter()
        .map(|v| match v {
            StateValue::Param { param, value } => format!("{} = {value}", param.name),
            StateValue::Rotate {
                moving,
                axis,
                angle,
            } => format!(
                "rotate {} about {} by {angle}",
                target_label(moving),
                axis_label(axis)
            ),
            StateValue::Translate {
                moving,
                direction,
                distance,
            } => format!(
                "translate {} along {} by {distance}",
                target_label(moving),
                axis_label(direction)
            ),
            StateValue::Joint { joint, value } => format!("{} = {value}", joint.name),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ------------------------------------------------------------ one primitive

struct StateEval {
    index: usize,
    status: CheckStatus,
    margin: Option<f64>,
    /// Job that decides the state (index into `claim.jobs`).
    job: usize,
}

fn eval_primitive(
    claim: &ClaimPlan,
    ci: usize,
    raw: &RawResults,
    manifest: &Manifest,
    eps: &Quantity,
) -> (PrimitiveResult, Vec<Finding>) {
    let states: &[ConcreteState] = manifest
        .states
        .get(&claim.id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let label = |i: usize| {
        states
            .get(i)
            .map(state_label)
            .unwrap_or_else(|| format!("state {i}"))
    };
    let result_unit = library()
        .primitive(&claim.primitive)
        .map(|p| p.result.internal_unit())
        .unwrap_or(Unit::One);
    let evals: Vec<JobEval> = claim
        .jobs
        .iter()
        .enumerate()
        .map(|(ji, j)| eval_job(claim, j, raw.jobs.get(&(ci, ji)), eps, result_unit))
        .collect();
    let pair_of = |ji: usize| PairRef {
        a: claim.jobs[ji].a.clone(),
        b: claim.jobs[ji].b.clone(),
    };
    let worst_of = |s: &StateEval| -> Worst {
        let e = &evals[s.job];
        let unit = e.unit.unwrap_or(result_unit);
        Worst {
            state: s.index,
            state_label: label(s.index),
            pair: pair_of(s.job),
            witness: raw.jobs.get(&(ci, s.job)).and_then(|o| o.witness.clone()),
            value: e.value.map(|v| quantity(v, unit)),
            margin: e.margin.map(|m| Quantity::new(m, unit)),
            allowance: e.allowance,
        }
    };

    let mut by_state: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (ji, j) in claim.jobs.iter().enumerate() {
        by_state.entry(j.state).or_default().push(ji);
    }
    let any_pair = claim.pairing == Pairing::AnyPair;
    let state_evals: Vec<StateEval> = by_state
        .iter()
        .map(|(&index, jobs)| {
            let statuses = jobs.iter().map(|&j| evals[j].status);
            let status = if any_pair {
                any_of(statuses)
            } else {
                all_of(statuses)
            };
            // The job that decides: tightest margin (loosest for any-pair).
            let measured = jobs.iter().copied().filter(|&j| {
                matches!(evals[j].status, CheckStatus::Met | CheckStatus::NotMet)
                    && evals[j].margin.is_some()
            });
            let pick = if any_pair {
                measured.fold(None::<usize>, |best, j| match best {
                    Some(b) if evals[b].margin >= evals[j].margin => Some(b),
                    _ => Some(j),
                })
            } else {
                measured.fold(None::<usize>, |best, j| match best {
                    Some(b) if evals[b].margin <= evals[j].margin => Some(b),
                    _ => Some(j),
                })
            };
            let job = pick
                .or_else(|| {
                    jobs.iter()
                        .copied()
                        .find(|&j| evals[j].status != CheckStatus::Met)
                })
                .unwrap_or(jobs[0]);
            StateEval {
                index,
                status,
                margin: pick.and_then(|j| evals[j].margin),
                job,
            }
        })
        .collect();

    let status = match claim.quantifier {
        Quantifier::ForAll => all_of(state_evals.iter().map(|s| s.status)),
        Quantifier::Exists => any_of(state_evals.iter().map(|s| s.status)),
    };
    // Smallest margin first (ties: lowest state); `max` flips it.
    let by_margin = |cands: Vec<&StateEval>, max: bool| -> Option<usize> {
        let mut best: Option<&StateEval> = None;
        for s in cands.into_iter().filter(|s| s.margin.is_some()) {
            best = match best {
                Some(b) if (max && b.margin >= s.margin) || (!max && b.margin <= s.margin) => {
                    Some(b)
                }
                _ => Some(s),
            };
        }
        best.map(|s| s.index)
    };
    let find = |i: usize| {
        state_evals
            .iter()
            .find(|s| s.index == i)
            .expect("state exists")
    };
    let (worst, witness) = match claim.quantifier {
        Quantifier::ForAll => {
            let i = by_margin(state_evals.iter().collect(), false).or_else(|| {
                state_evals
                    .iter()
                    .find(|s| s.status != CheckStatus::Met)
                    .map(|s| s.index)
            });
            (i.map(|i| worst_of(find(i))), None)
        }
        Quantifier::Exists => {
            let met: Vec<&StateEval> = state_evals
                .iter()
                .filter(|s| s.status == CheckStatus::Met)
                .collect();
            let w = by_margin(met.clone(), true).or_else(|| met.first().map(|s| s.index));
            match w {
                Some(i) => (None, Some(worst_of(find(i)))),
                None => {
                    let i = by_margin(state_evals.iter().collect(), true);
                    (i.map(|i| worst_of(find(i))), None)
                }
            }
        }
    };
    let margin = witness.as_ref().or(worst.as_ref()).and_then(|w| w.margin);

    let failing: Vec<usize> = state_evals
        .iter()
        .filter(|s| s.status == CheckStatus::NotMet)
        .map(|s| s.index)
        .collect();
    let slow: Vec<SlowPair> = claim
        .jobs
        .iter()
        .enumerate()
        .filter(|(ji, _)| evals[*ji].status == CheckStatus::Inconclusive)
        .flat_map(|(ji, j)| {
            let named = raw
                .jobs
                .get(&(ci, ji))
                .map(|o| o.slow.clone())
                .unwrap_or_default();
            let reason = raw.jobs.get(&(ci, ji)).and_then(|o| o.reason.clone());
            let pairs: Vec<PairRef> = if named.is_empty() {
                vec![pair_of(ji)]
            } else {
                named
                    .into_iter()
                    .map(|(a, b)| PairRef {
                        a: vec![a],
                        b: b.into_iter().collect(),
                    })
                    .collect()
            };
            let (state, state_label) = (j.state, label(j.state));
            pairs.into_iter().map(move |pair| SlowPair {
                state,
                state_label: state_label.clone(),
                pair,
                reason: reason.clone(),
            })
        })
        .collect();
    let errors: Vec<String> = claim
        .jobs
        .iter()
        .enumerate()
        .filter(|(ji, _)| evals[*ji].status == CheckStatus::Error)
        .map(|(ji, j)| {
            let why = raw
                .jobs
                .get(&(ci, ji))
                .and_then(|o| o.reason.clone())
                .unwrap_or_else(|| "worker returned no result".into());
            format!("{} / {}: {why}", label(j.state), pair_text(&pair_of(ji)))
        })
        .collect();

    // Findings: runs of consecutive failing states per pair (for_all only).
    let mut findings = Vec::new();
    if claim.quantifier == Quantifier::ForAll {
        let mut per_pair: BTreeMap<PairRef, Vec<usize>> = BTreeMap::new();
        for s in state_evals
            .iter()
            .filter(|s| s.status == CheckStatus::NotMet)
        {
            for &ji in &by_state[&s.index] {
                if evals[ji].status == CheckStatus::NotMet {
                    per_pair.entry(pair_of(ji)).or_default().push(ji);
                }
            }
        }
        for (pair, jobs) in per_pair {
            let mut runs: Vec<Vec<usize>> = Vec::new();
            for ji in jobs {
                let st = claim.jobs[ji].state;
                match runs.last_mut() {
                    Some(r) if claim.jobs[*r.last().expect("non-empty")].state + 1 == st => {
                        r.push(ji);
                    }
                    _ => runs.push(vec![ji]),
                }
            }
            for run in runs {
                // Worst job of the run: tightest margin, else the first.
                let worst_job = run
                    .iter()
                    .copied()
                    .filter(|&j| evals[j].margin.is_some())
                    .fold(None::<usize>, |b, j| match b {
                        Some(b) if evals[b].margin <= evals[j].margin => Some(b),
                        _ => Some(j),
                    })
                    .unwrap_or(run[0]);
                let e = &evals[worst_job];
                let unit = e.unit.unwrap_or(result_unit);
                let first = claim.jobs[run[0]].state;
                let last = claim.jobs[*run.last().expect("non-empty")].state;
                findings.push(Finding {
                    id: format!(
                        "{}|{}|{}|{}",
                        claim.id,
                        pair.a.join(","),
                        pair.b.join(","),
                        label(first)
                    ),
                    claim: claim.id.clone(),
                    origin_claim: claim.origin.claim.clone(),
                    primitive: claim.primitive.clone(),
                    a: pair.a.clone(),
                    b: pair.b.clone(),
                    first_state: first,
                    last_state: last,
                    states: run
                        .iter()
                        .map(|&j| {
                            let i = claim.jobs[j].state;
                            FindingState {
                                index: i,
                                label: label(i),
                                state: states.get(i).cloned().unwrap_or_default(),
                            }
                        })
                        .collect(),
                    state_count: claim.state_count,
                    worst_value: e.value.map(|v| quantity(v, unit)),
                    worst_margin: e.margin.map(|m| Quantity::new(m, unit)),
                    label: None,
                });
            }
        }
    }

    let capped = |n: usize| n.min(MAX_LISTED);
    let (slow_n, err_n) = (slow.len(), errors.len());
    (
        PrimitiveResult {
            id: claim.id.clone(),
            claim: claim.origin.claim.clone(),
            primitive: claim.primitive.clone(),
            quantifier: claim.quantifier,
            status,
            met: match status {
                CheckStatus::Met => Some(true),
                CheckStatus::NotMet => Some(false),
                _ => None,
            },
            margin,
            worst,
            witness,
            states: claim.state_count,
            jobs: claim.jobs.len(),
            failing_state_count: failing.len(),
            failing_states: failing.into_iter().take(MAX_FAILING_STATES).collect(),
            inconclusive: slow.into_iter().take(capped(slow_n)).collect(),
            inconclusive_count: slow_n,
            errors: errors.into_iter().take(capped(err_n)).collect(),
            error_count: err_n,
        },
        findings,
    )
}

fn pair_text(p: &PairRef) -> String {
    if p.b.is_empty() {
        p.a.join("+")
    } else {
        format!("{} x {}", p.a.join("+"), p.b.join("+"))
    }
}

// ----------------------------------------------------------------- the whole

/// Evaluate a plan against raw measurements (P1.54). `unsupported` is the
/// number of unsupported prose items of the requirement: when everything is
/// met but it is non-zero, the status is `met_partial`.
pub fn evaluate(
    plan: &Plan,
    manifest: &Manifest,
    raw: &RawResults,
    settings: &ProjectSettings,
    requirement_id: &str,
    unsupported: usize,
) -> EvalResult {
    let eps = settings.overlap_tolerance;
    let mut primitives = Vec::new();
    let mut findings = Vec::new();
    for (ci, claim) in plan.claims.iter().enumerate() {
        let (p, f) = eval_primitive(claim, ci, raw, manifest, &eps);
        primitives.push(p);
        findings.extend(f);
    }
    findings.sort_by(|x, y| {
        (&x.claim, &x.a, &x.b, x.first_state).cmp(&(&y.claim, &y.a, &y.b, y.first_state))
    });

    // IR claims in plan order.
    let mut order: Vec<&str> = Vec::new();
    for c in &plan.claims {
        if !order.contains(&c.origin.claim.as_str()) {
            order.push(&c.origin.claim);
        }
    }
    let claims: Vec<ClaimResult> = order
        .iter()
        .map(|&id| {
            let ps: Vec<&PrimitiveResult> = primitives.iter().filter(|p| p.claim == id).collect();
            let status = all_of(ps.iter().map(|p| p.status));
            let lead = ps
                .iter()
                .find(|p| p.status == CheckStatus::NotMet)
                .or(ps.first())
                .expect("claim has a primitive");
            ClaimResult {
                claim: id.to_string(),
                rule: plan
                    .claims
                    .iter()
                    .find(|c| c.origin.claim == id)
                    .map(|c| c.origin.rule.clone())
                    .unwrap_or_default(),
                status,
                met: match status {
                    CheckStatus::Met => Some(true),
                    CheckStatus::NotMet => Some(false),
                    _ => None,
                },
                margin: lead.margin,
                worst: lead.worst.clone(),
                witness: lead.witness.clone(),
                primitives: ps.iter().map(|p| p.id.clone()).collect(),
            }
        })
        .collect();

    let status = match all_of(claims.iter().map(|c| c.status)) {
        CheckStatus::Error => RequirementStatus::Error,
        CheckStatus::NotMet => RequirementStatus::NotMet,
        CheckStatus::Inconclusive => RequirementStatus::Inconclusive,
        CheckStatus::Met if unsupported > 0 => RequirementStatus::MetPartial,
        CheckStatus::Met => RequirementStatus::Met,
    };

    let mut timings = RunTimings {
        total_s: raw.total_s,
        primitives: raw.claim_seconds.clone(),
        ..Default::default()
    };
    for (pid, s) in &raw.claim_seconds {
        if let Some(p) = primitives.iter().find(|p| &p.id == pid) {
            *timings.claims.entry(p.claim.clone()).or_default() += s;
        }
    }
    timings.costliest = timings
        .claims
        .iter()
        .fold(None::<(&String, f64)>, |b, (k, &v)| match b {
            Some((_, bv)) if bv >= v => b,
            _ => Some((k, v)),
        })
        .map(|(k, _)| k.clone());
    let mut warnings = Vec::new();
    let budget = f64::from(settings.auto_run_budget_secs);
    if timings.total_s > budget {
        let message = match &timings.costliest {
            Some(c) => format!(
                "the run took {:.1} s, over the {budget:.0} s budget; claim {c} cost the most ({:.1} s)",
                timings.total_s, timings.claims[c]
            ),
            None => format!(
                "the run took {:.1} s, over the {budget:.0} s budget",
                timings.total_s
            ),
        };
        let at = timings
            .costliest
            .as_ref()
            .map(|c| format!("claims[{c}]"))
            .unwrap_or_default();
        warnings.push(Diagnostic::new(DiagnosticCode::WSlow, at, message));
    }

    EvalResult {
        body: ResultBody {
            format: RESULT_FORMAT.into(),
            requirement_id: requirement_id.to_string(),
            plan_hash: plan.content_hash(),
            manifest_hash: plan.manifest_hash.clone(),
            library_version: plan.library_version.clone(),
            settings_hash: manifest.settings_hash.clone(),
            status,
            unsupported,
            claims,
            primitives,
            findings,
        },
        timings,
        warnings,
    }
}
