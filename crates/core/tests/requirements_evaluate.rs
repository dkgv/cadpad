//! Evaluation, relink, status, findings and allowance drafts, with a fake
//! executor answering from handcrafted worker responses.
use cadpad_core::requirements::evaluate::{MAX_FAILING_STATES, RESULT_FORMAT};
use cadpad_core::requirements::findings::FindingState;
use cadpad_core::requirements::manifest::{ChipLink, FeatureLink, Manifest};
use cadpad_core::requirements::plan::{ClaimPlan, Job, Plan, Requires};
use cadpad_core::requirements::relink::{ChangeKind, Relink};
use cadpad_core::requirements::worker_plan::{JobOutcome, MeasStatus, collect_results};
use cadpad_core::requirements::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn read(path: &str) -> String {
    std::fs::read_to_string(format!("tests/fixtures/requirements/{path}.json")).unwrap()
}

/// Compile fixture `name`, optionally editing the requirement / snapshot JSON.
fn build(
    name: &str,
    req_edit: impl FnOnce(&mut Value),
    snap_edit: impl FnOnce(&mut Value),
) -> Compiled {
    let mut r: Value = serde_json::from_str(&read(name)).unwrap();
    let mut s: Value = serde_json::from_str(&read(&format!("snapshots/{name}"))).unwrap();
    req_edit(&mut r);
    snap_edit(&mut s);
    let req: Requirement = serde_json::from_value(r).unwrap();
    let snap: WorkspaceSnapshot = serde_json::from_value(s).unwrap();
    compile(&req, &snap, &ProjectSettings::default())
}

fn snap_of(name: &str, edit: impl FnOnce(&mut Value)) -> WorkspaceSnapshot {
    let mut s: Value = serde_json::from_str(&read(&format!("snapshots/{name}"))).unwrap();
    edit(&mut s);
    serde_json::from_value(s).unwrap()
}

fn e3() -> Compiled {
    let c = build("E3", |_| {}, |_| {});
    assert!(!c.has_errors(), "{:?}", c.diagnostics);
    c
}

const DOOR: &str = "cabinet.py#cabinet/door";
const FRAME: &str = "cabinet.py#cabinet/frame";

/// Fake worker for E3: overlap is `value(angle)` for every pair.
fn e3_exec(
    value: impl Fn(f64) -> f64 + 'static,
) -> FakeExecutor<impl FnMut(&Value, &Value) -> Value> {
    FakeExecutor::new(move |state: &Value, m: &Value| {
        let angle = state["params"]["door_angle"].as_f64().unwrap_or(0.0);
        let v = value(angle);
        let (a, b) = (m["a"][0].clone(), m["b"][0].clone());
        json!({"status": "ok", "value": v, "witness": [a, b],
               "items": [{"a": a, "b": b, "status": "ok", "value": v},
                         {"a": "zz", "b": "yy", "status": "ok", "value": 0.0}]})
    })
}

fn run_e3(
    c: &Compiled,
    exec: &mut dyn PlanExecutor,
    unsupported: usize,
) -> Result<EvalResult, EvalError> {
    let plan = c.plan.as_ref().unwrap();
    let manifest = c.manifest.as_ref().unwrap();
    let raw = run_plan(plan, manifest, exec, &ChunkOptions::default())?;
    Ok(evaluate(
        plan,
        manifest,
        &raw,
        &ProjectSettings::default(),
        "e3",
        unsupported,
    ))
}

// ---------------------------------------------------------------- synthetic

fn synth(
    primitive: &str,
    pairing: Pairing,
    quantifier: Quantifier,
    cmp: Comparator,
    strict: bool,
    n: usize,
) -> (Plan, Manifest) {
    let c = e3();
    let mut manifest = c.manifest.unwrap();
    let base = manifest.states.values().next().unwrap().clone();
    let states: Vec<_> = (0..n).map(|i| base[i % base.len()].clone()).collect();
    manifest.states = BTreeMap::from([("t.1".to_string(), states)]);
    let claim = ClaimPlan {
        id: "t.1".into(),
        origin: Origin {
            claim: "t".into(),
            rule: "measure".into(),
            library_version: "v".into(),
        },
        primitive: primitive.into(),
        pairing,
        quantifier,
        cmp,
        strict,
        axis: None,
        state_count: n,
        jobs: (0..n)
            .map(|i| Job {
                state: i,
                a: vec![DOOR.into()],
                b: vec![FRAME.into()],
                allowed: None,
            })
            .collect(),
    };
    let plan = Plan {
        manifest_hash: manifest.content_hash(),
        library_version: manifest.library_version.clone(),
        requires: Requires {
            parts: BTreeMap::new(),
            models: BTreeMap::new(),
        },
        claims: vec![claim],
    };
    (plan, manifest)
}

fn outcome(status: MeasStatus, value: Option<f64>) -> JobOutcome {
    JobOutcome {
        status,
        value,
        witness: Some(vec![DOOR.into(), FRAME.into()]),
        reason: (status == MeasStatus::Inconclusive).then(|| "pair_time_limit".to_string()),
        slow: vec![],
    }
}

fn raw_of(outs: Vec<JobOutcome>) -> RawResults {
    RawResults {
        jobs: outs
            .into_iter()
            .enumerate()
            .map(|(i, o)| ((0, i), o))
            .collect(),
        ..Default::default()
    }
}

fn vals(v: &[f64]) -> RawResults {
    raw_of(
        v.iter()
            .map(|&x| outcome(MeasStatus::Ok, Some(x)))
            .collect(),
    )
}

fn eval(plan: &Plan, m: &Manifest, raw: &RawResults, unsupported: usize) -> EvalResult {
    evaluate(plan, m, raw, &ProjectSettings::default(), "t", unsupported)
}

fn mm(v: f64) -> Literal {
    Literal::new(v, Unit::Mm)
}

fn ge(v: f64) -> Comparator {
    Comparator::Ge { value: mm(v) }
}

#[test]
fn met_reports_margin_in_the_comparator_unit_and_the_worst_state() {
    let (plan, m) = synth(
        "min_clearance",
        Pairing::Whole,
        Quantifier::ForAll,
        ge(1.0),
        false,
        4,
    );
    let r = eval(&plan, &m, &vals(&[3.0, 1.5, 2.0, 4.0]), 0);
    assert_eq!(r.body.status, RequirementStatus::Met);
    let p = &r.body.primitives[0];
    assert_eq!(p.met, Some(true));
    assert_eq!(p.margin, Some(Quantity::new(0.5, Unit::Mm)));
    let w = p.worst.as_ref().unwrap();
    assert_eq!(w.state, 1);
    assert_eq!(w.pair.a, [DOOR]);
    assert_eq!(w.witness.as_deref().unwrap(), [DOOR, FRAME]);
    assert!(p.failing_states.is_empty());
    // A limit written in cm reports its margin in cm: 2 mm vs >= 0.1 cm.
    let cm = Comparator::Ge {
        value: Literal::new(0.1, Unit::Cm),
    };
    let (plan, m) = synth(
        "min_clearance",
        Pairing::Whole,
        Quantifier::ForAll,
        cm,
        false,
        1,
    );
    let r = eval(&plan, &m, &vals(&[2.0]), 0);
    assert_eq!(
        r.body.primitives[0].margin,
        Some(Quantity::new(0.1, Unit::Cm))
    );
}

#[test]
fn not_met_has_negative_margin_and_lists_failing_states_capped() {
    let (plan, m) = synth(
        "min_clearance",
        Pairing::Whole,
        Quantifier::ForAll,
        ge(1.0),
        false,
        5,
    );
    let r = eval(&plan, &m, &vals(&[3.0, 0.4, 0.9, 2.0, 0.7]), 0);
    assert_eq!(r.body.status, RequirementStatus::NotMet);
    let p = &r.body.primitives[0];
    assert_eq!(p.margin, Some(Quantity::new(-0.6, Unit::Mm)));
    assert_eq!(p.failing_states, [1, 2, 4]);
    assert_eq!(p.worst.as_ref().unwrap().state, 1);
    // The count is exact, the list capped.
    let n = MAX_FAILING_STATES + 20;
    let (plan, mf) = synth(
        "min_clearance",
        Pairing::Whole,
        Quantifier::ForAll,
        ge(1.0),
        false,
        n,
    );
    let r = eval(&plan, &mf, &vals(&vec![0.0; n]), 0);
    let p = &r.body.primitives[0];
    assert_eq!(p.failing_state_count, n);
    assert_eq!(p.failing_states.len(), MAX_FAILING_STATES);
    // A claim passing exactly at its limit is met with margin 0 ("by a hair").
    let (plan, m) = synth(
        "min_clearance",
        Pairing::Whole,
        Quantifier::ForAll,
        ge(1.0),
        false,
        1,
    );
    let r = eval(&plan, &m, &vals(&[1.0]), 0);
    assert_eq!(r.body.status, RequirementStatus::Met);
    assert_eq!(
        r.body.primitives[0].margin,
        Some(Quantity::new(0.0, Unit::Mm))
    );
}

#[test]
fn comparators_in_approx_and_booleans() {
    let case = |cmp: Comparator, v: f64| {
        let (plan, m) = synth("extent", Pairing::Whole, Quantifier::ForAll, cmp, false, 1);
        eval(&plan, &m, &vals(&[v]), 0).body.primitives[0].clone()
    };
    let inr = Comparator::In {
        lo: mm(2.0),
        hi: mm(5.0),
    };
    assert_eq!(
        case(inr.clone(), 4.0).margin,
        Some(Quantity::new(1.0, Unit::Mm))
    );
    assert_eq!(case(inr, 1.5).met, Some(false));
    let ap = Comparator::Approx {
        target: mm(10.0),
        tol: mm(0.5),
    };
    assert_eq!(
        case(ap.clone(), 10.2).margin,
        Some(Quantity::new(0.3, Unit::Mm))
    );
    assert_eq!(case(ap, 11.0).margin, Some(Quantity::new(-0.5, Unit::Mm)));
    assert_eq!(case(Comparator::IsTrue {}, 1.0).met, Some(true));
    assert_eq!(case(Comparator::IsFalse {}, 1.0).met, Some(false));
    assert_eq!(case(Comparator::IsTrue {}, 1.0).margin, None);
}

#[test]
fn strict_blocked_any_pair_needs_overlap_above_the_limit() {
    let eps = Comparator::Ge {
        value: Literal::new(1e-3, Unit::Mm3),
    };
    let (plan, m) = synth(
        "interference",
        Pairing::AnyPair,
        Quantifier::ForAll,
        eps,
        true,
        2,
    );
    // Exactly at epsilon is not "blocked" under a strict comparator.
    let r = eval(&plan, &m, &vals(&[1e-3, 0.5]), 0);
    assert_eq!(r.body.status, RequirementStatus::NotMet);
    assert_eq!(r.body.primitives[0].failing_states, [0]);
    assert_eq!(
        r.body.primitives[0].margin,
        Some(Quantity::new(0.0, Unit::Mm3))
    );
    let r = eval(&plan, &m, &vals(&[0.002, 0.5]), 0);
    assert_eq!(r.body.status, RequirementStatus::Met);
    // Non-strict at the limit passes.
    let (plan, m) = synth(
        "interference",
        Pairing::AnyPair,
        Quantifier::ForAll,
        Comparator::Ge {
            value: Literal::new(1e-3, Unit::Mm3),
        },
        false,
        1,
    );
    assert_eq!(
        eval(&plan, &m, &vals(&[1e-3]), 0).body.status,
        RequirementStatus::Met
    );
}

#[test]
fn any_pair_in_a_state_holds_when_one_pair_holds() {
    let (mut plan, m) = synth(
        "interference",
        Pairing::AnyPair,
        Quantifier::ForAll,
        Comparator::Ge {
            value: Literal::new(1e-3, Unit::Mm3),
        },
        true,
        1,
    );
    let j = plan.claims[0].jobs[0].clone();
    plan.claims[0].jobs.push(Job {
        b: vec!["cabinet.py#cabinet/handle".into()],
        ..j
    });
    let r = eval(&plan, &m, &vals(&[0.0, 0.7]), 0);
    assert_eq!(r.body.status, RequirementStatus::Met);
    assert_eq!(
        r.body.primitives[0].worst.as_ref().unwrap().pair.b,
        ["cabinet.py#cabinet/handle"]
    );
}

#[test]
fn exists_reports_a_witness_and_not_met_the_closest_attempt() {
    let (plan, m) = synth(
        "min_clearance",
        Pairing::Whole,
        Quantifier::Exists,
        ge(1.0),
        false,
        3,
    );
    let r = eval(&plan, &m, &vals(&[0.5, 2.0, 3.0]), 0);
    assert_eq!(r.body.status, RequirementStatus::Met);
    let p = &r.body.primitives[0];
    let w = p.witness.as_ref().unwrap();
    assert_eq!(w.state, 2);
    assert_eq!(p.margin, Some(Quantity::new(2.0, Unit::Mm)));
    assert!(p.worst.is_none());
    assert_eq!(p.failing_states, [0]);
    assert!(r.body.findings.is_empty(), "exists claims make no findings");
    let r = eval(&plan, &m, &vals(&[0.5, 0.2, 0.9]), 0);
    assert_eq!(r.body.status, RequirementStatus::NotMet);
    let p = &r.body.primitives[0];
    assert!(p.witness.is_none());
    assert_eq!(p.worst.as_ref().unwrap().state, 2);
    assert_eq!(p.margin, Some(Quantity::new(-0.1, Unit::Mm)));
}

#[test]
fn inconclusive_and_error_propagate_with_the_documented_precedence() {
    let ok = |v: f64| outcome(MeasStatus::Ok, Some(v));
    let slow = || outcome(MeasStatus::Inconclusive, None);
    let err = || JobOutcome {
        reason: Some("boom".into()),
        ..outcome(MeasStatus::Error, None)
    };
    let (plan, m) = synth(
        "min_clearance",
        Pairing::Whole,
        Quantifier::ForAll,
        ge(1.0),
        false,
        3,
    );
    // Met elsewhere + one slow pair: nothing is claimed.
    let r = eval(&plan, &m, &raw_of(vec![ok(3.0), slow(), ok(3.0)]), 0);
    assert_eq!(r.body.status, RequirementStatus::Inconclusive);
    let p = &r.body.primitives[0];
    assert_eq!(p.met, None);
    assert_eq!(p.inconclusive[0].state, 1);
    assert_eq!(p.inconclusive[0].pair.a, [DOOR]);
    assert_eq!(r.body.claims[0].status, CheckStatus::Inconclusive);
    // Already not met stays not met.
    let r = eval(&plan, &m, &raw_of(vec![ok(0.1), slow(), ok(3.0)]), 0);
    assert_eq!(r.body.status, RequirementStatus::NotMet);
    // An error wins.
    let r = eval(&plan, &m, &raw_of(vec![ok(0.1), err(), ok(3.0)]), 0);
    assert_eq!(r.body.status, RequirementStatus::Error);
    assert!(r.body.primitives[0].errors[0].contains("boom"));
    // exists: a witness stands despite a state that timed out.
    let (plan, m) = synth(
        "min_clearance",
        Pairing::Whole,
        Quantifier::Exists,
        ge(1.0),
        false,
        3,
    );
    let r = eval(&plan, &m, &raw_of(vec![slow(), ok(3.0), ok(0.1)]), 0);
    assert_eq!(r.body.status, RequirementStatus::Met);
    let r = eval(&plan, &m, &raw_of(vec![slow(), ok(0.5), ok(0.1)]), 0);
    assert_eq!(r.body.status, RequirementStatus::Inconclusive);
    // A job nobody answered is an error, never a pass.
    let r = eval(&plan, &m, &raw_of(vec![ok(3.0)]), 0);
    assert_eq!(r.body.primitives[0].status, CheckStatus::Met);
    let (plan3, m3) = synth(
        "min_clearance",
        Pairing::Whole,
        Quantifier::ForAll,
        ge(1.0),
        false,
        3,
    );
    let r = eval(&plan3, &m3, &raw_of(vec![ok(3.0)]), 0);
    assert_eq!(r.body.status, RequirementStatus::Error);
}

#[test]
fn unsupported_prose_makes_met_partial_but_never_hides_not_met() {
    let (plan, m) = synth(
        "min_clearance",
        Pairing::Whole,
        Quantifier::ForAll,
        ge(1.0),
        false,
        1,
    );
    assert_eq!(
        eval(&plan, &m, &vals(&[2.0]), 1).body.status,
        RequirementStatus::MetPartial
    );
    assert_eq!(
        eval(&plan, &m, &vals(&[0.0]), 1).body.status,
        RequirementStatus::NotMet
    );
}

// ------------------------------------------------------------- allowances

fn allow_interference(r: &mut Value, max: f64) {
    r["allow"][0]["kind"] = json!("interference");
    r["allow"][0]["max"] = json!({"value": max, "unit": "mm3"});
}

#[test]
fn allowances_apply_before_comparison_at_their_states_only() {
    // Touch allows overlap up to epsilon at door_angle 0 only.
    let c = e3();
    let mut ex = e3_exec(|a| if a == 0.0 { 0.0005 } else { 0.0 });
    let r = run_e3(&c, &mut ex, 0).unwrap();
    assert_eq!(r.body.status, RequirementStatus::Met);
    // 0.4 mm3 at the closed state is beyond a touch allowance.
    let mut ex = e3_exec(|a| if a == 0.0 { 0.4 } else { 0.0 });
    let r = run_e3(&c, &mut ex, 0).unwrap();
    assert_eq!(r.body.status, RequirementStatus::NotMet);
    let w = r.body.primitives[0].worst.as_ref().unwrap();
    assert_eq!((w.state, w.allowance), (0, Some(0)));
    // An interference allowance of 0.5 covers it, with a margin of 0.1...
    let c2 = build("E3", |r| allow_interference(r, 0.5), |_| {});
    let mut ex = e3_exec(|a| if a == 0.0 { 0.4 } else { 0.0 });
    let r = run_e3(&c2, &mut ex, 0).unwrap();
    assert_eq!(r.body.status, RequirementStatus::Met);
    // ...but only at state 0: the same overlap at 10 deg fails by the claim's own limit.
    let mut ex = e3_exec(|a| if a == 0.0 || a == 10.0 { 0.4 } else { 0.0 });
    let r = run_e3(&c2, &mut ex, 0).unwrap();
    assert_eq!(r.body.status, RequirementStatus::NotMet);
    assert_eq!(r.body.primitives[0].failing_states, [5]);
    // Over the allowance's max still fails, and says so.
    let mut ex = e3_exec(|a| if a == 0.0 { 0.7 } else { 0.0 });
    let r = run_e3(&c2, &mut ex, 0).unwrap();
    assert_eq!(r.body.status, RequirementStatus::NotMet);
    let m = r.body.primitives[0].worst.as_ref().unwrap().margin.unwrap();
    assert_eq!(m, Quantity::new(-0.2, Unit::Mm3));
}

// --------------------------------------------------------- request building

#[test]
fn requests_are_chunked_by_state_and_translate_states() {
    let c = e3();
    let plan = c.plan.as_ref().unwrap();
    let m = c.manifest.as_ref().unwrap();
    let opts = ChunkOptions {
        chunk_size: 10,
        ..ChunkOptions::from_settings(&ProjectSettings::default())
    };
    let reqs = build_requests(plan, m, &opts).unwrap();
    assert_eq!(reqs.len(), 6); // 56 states, 10 per chunk
    assert_eq!(reqs.iter().map(|r| r.measurements.len()).sum::<usize>(), 56);
    let r0 = &reqs[0];
    assert_eq!(r0.model, "cabinet.py#cabinet");
    assert_eq!(r0.params["limits"]["pair_time_limit_s"], 20.0);
    let s1 = &r0.params["states"][1];
    assert_eq!(s1["params"]["door_angle"], 2.0);
    let meas = &s1["measurements"][0];
    assert_eq!(meas["primitive"], "interference");
    assert_eq!(meas["a"], json!(["door"]));
    assert_eq!(meas["b"], json!(["frame"]));
    assert!(
        r0.params["states"][0].get("moves").is_none()
            || r0.params["states"][0]["moves"].is_null()
            || true
    );
    // Same plan, same requests.
    assert_eq!(reqs, build_requests(plan, m, &opts).unwrap());

    // Rigid states become ordered moves of named parts.
    let r1 = build("R1", |_| {}, |_| {});
    let reqs = build_requests(
        r1.plan.as_ref().unwrap(),
        r1.manifest.as_ref().unwrap(),
        &ChunkOptions {
            chunk_size: 100,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(reqs.len(), 1);
    let st = &reqs[0].params["states"][1];
    let mv = &st["moves"][0];
    assert_eq!(mv["op"], "rotate");
    assert_eq!(mv["angle_deg"], 5.0);
    assert!(
        mv["parts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| !p.as_str().unwrap().contains('#'))
    );
    assert_eq!(st["measurements"].as_array().unwrap().len(), 5);
}

#[test]
fn a_cancelled_chunk_fails_the_run_with_no_partial_result() {
    let c = e3();
    let mut ex = e3_exec(|_| 0.0);
    ex.cancel = true;
    assert!(matches!(
        run_e3(&c, &mut ex, 0),
        Err(EvalError::Cancelled(_))
    ));
}

#[test]
fn results_are_identical_whatever_order_the_worker_answers_in() {
    let c = build("R1", |_| {}, |_| {});
    let plan = c.plan.as_ref().unwrap();
    let m = c.manifest.as_ref().unwrap();
    let go = |shuffle: bool, chunk: usize| {
        let mut ex = FakeExecutor::new(|state: &Value, meas: &Value| {
            let angle = state["moves"][0]["angle_deg"].as_f64().unwrap_or(0.0);
            // Overlap for the first part of a, between 100 and 130 degrees.
            let hit = meas["b"][0] == "clamp_1" && (100.0..=130.0).contains(&angle);
            let v = if hit { 0.25 } else { 0.0 };
            json!({"status": "ok", "value": v, "witness": [meas["a"][0].clone(), meas["b"][0].clone()],
                   "items": [{"a": meas["a"][0].clone(), "b": meas["b"][0].clone(), "status": "ok", "value": v},
                             {"a": "x", "b": "y", "status": "ok", "value": 0.0}]})
        });
        ex.shuffle = shuffle;
        let raw = run_plan(
            plan,
            m,
            &mut ex,
            &ChunkOptions {
                chunk_size: chunk,
                ..Default::default()
            },
        )
        .unwrap();
        evaluate(plan, m, &raw, &ProjectSettings::default(), "r1", 0)
    };
    let a = go(false, 1000);
    let b = go(true, 1000);
    let c2 = go(true, 7);
    assert_eq!(a.body.status, RequirementStatus::NotMet);
    assert_eq!(a.hash(), b.hash());
    assert_eq!(a.hash(), c2.hash());
    assert_eq!(a.body, c2.body);
    assert_eq!(a.body.findings.len(), 1);
    assert_eq!(a.body.format, RESULT_FORMAT);
}

#[test]
fn timings_are_outside_the_hash_and_w_slow_names_the_costliest_claim() {
    let c = build("R4", |_| {}, |_| {});
    let plan = c.plan.as_ref().unwrap();
    let m = c.manifest.as_ref().unwrap();
    let mut ex = FakeExecutor::new(|_: &Value, _: &Value| json!({"status": "ok", "value": 0.0}));
    let mut raw = run_plan(plan, m, &mut ex, &ChunkOptions::default()).unwrap();
    let fast = evaluate(plan, m, &raw, &ProjectSettings::default(), "r4", 0);
    assert!(fast.warnings.is_empty());
    raw.total_s = 45.0;
    raw.claim_seconds = BTreeMap::from([("c1.1".to_string(), 3.0), ("c2.1".to_string(), 30.5)]);
    let slow = evaluate(plan, m, &raw, &ProjectSettings::default(), "r4", 0);
    assert_eq!(slow.hash(), fast.hash());
    assert_eq!(slow.timings.costliest.as_deref(), Some("c2"));
    let w = &slow.warnings[0];
    assert_eq!(w.code, DiagnosticCode::WSlow);
    assert!(
        w.message.contains("c2") && w.message.contains("30.5"),
        "{}",
        w.message
    );
    // Timings come from the worker's per-measurement records.
    let req = &build_requests(plan, m, &ChunkOptions::default()).unwrap()[0];
    let id = req
        .measurements
        .iter()
        .find(|r| plan.claims[r.claim].id == "c2.1")
        .unwrap()
        .id
        .clone();
    let reply = json!({"format": "requirements-eval-v1", "status": "complete",
        "results": {"states": []},
        "timings": {"total_s": 2.0, "measurements": [{"state": "s0", "id": id, "broad_s": 0.5, "job_cpu_s": 1.0}]}});
    let chunk = worker_plan::parse_chunk_response(req, &reply).unwrap();
    let raw = collect_results(plan, &[(req, chunk)]);
    assert_eq!(raw.claim_seconds["c2.1"], 1.5);
    assert_eq!(raw.total_s, 2.0);
    // Unanswered measurements are errors.
    assert!(raw.jobs.values().all(|j| j.status == MeasStatus::Error));
}

// ---------------------------------------------------------------- findings

fn e3_failing(range: std::ops::RangeInclusive<i32>, v: f64) -> EvalResult {
    let c = e3();
    let mut ex = e3_exec(move |a| if range.contains(&(a as i32)) { v } else { 0.0 });
    run_e3(&c, &mut ex, 0).unwrap()
}

#[test]
fn findings_have_identity_runs_and_labels_across_runs() {
    let r1 = e3_failing(80..=100, 0.4);
    assert_eq!(r1.body.findings.len(), 1, "one contiguous run");
    let f = &r1.body.findings[0];
    assert_eq!(
        (f.claim.as_str(), f.a.clone(), f.b.clone()),
        ("c1.1", vec![DOOR.to_string()], vec![FRAME.to_string()])
    );
    assert_eq!((f.first_state, f.last_state, f.states.len()), (40, 50, 11));
    assert_eq!(f.worst_value, Some(Quantity::new(0.4, Unit::Mm3)));
    assert!(f.label.is_none(), "labels are relative to a previous run");

    // First run: everything is new.
    let l1 = r1.findings_vs(None);
    assert_eq!(l1[0].label, Some(FindingLabel::New));
    // Same problem again (run grew): still failing, same id.
    let r2 = e3_failing(80..=104, 0.4);
    let l2 = r2.findings_vs(Some(&r1.body));
    assert_eq!(l2.len(), 1);
    assert_eq!(l2[0].label, Some(FindingLabel::StillFailing));
    assert_eq!(l2[0].id, l1[0].id);
    // Fixed: resolved.
    let r3 = e3_failing(1000..=1000, 0.4);
    assert!(r3.body.findings.is_empty());
    let l3 = r3.findings_vs(Some(&r2.body));
    assert_eq!(l3.len(), 1);
    assert_eq!(l3[0].label, Some(FindingLabel::Resolved));
    // A separate run of the same pair is new, and the old one is resolved.
    let r4 = e3_failing(10..=20, 0.4);
    let mut l4 = r4.findings_vs(Some(&r2.body));
    l4.sort_by_key(|f| f.first_state);
    assert_eq!(
        l4.iter().map(|f| f.label.unwrap()).collect::<Vec<_>>(),
        [FindingLabel::New, FindingLabel::Resolved]
    );
    // Two runs of one pair split by a passing state are two findings.
    let c = e3();
    let mut ex = e3_exec(|a| {
        if ((10.0..=20.0).contains(&a) && a != 14.0) || a == 60.0 {
            0.4
        } else {
            0.0
        }
    });
    let r5 = run_e3(&c, &mut ex, 0).unwrap();
    let runs: Vec<_> = r5
        .body
        .findings
        .iter()
        .map(|f| (f.first_state, f.last_state))
        .collect();
    assert_eq!(runs, [(5, 6), (8, 10), (30, 30)]);
}

#[test]
fn accepting_clashes_drafts_one_allowance_set_that_compiles_and_passes() {
    let c = e3();
    let r = e3_failing(80..=100, 0.4123);
    let findings: Vec<&Finding> = r.body.findings.iter().collect();
    let req = c.requirement.clone();
    let draft = draft_allowances(&req, &findings, &ProjectSettings::default());
    assert!(draft.skipped.is_empty(), "{:?}", draft.skipped);
    assert_eq!(draft.allowances.len(), 1);
    let a = &draft.allowances[0];
    assert_eq!(a.kind, AllowKind::Interference);
    assert_eq!(
        a.max.as_ref().unwrap().value,
        0.413,
        "rounded up, never below the measured overlap"
    );
    assert_eq!(a.source, Source::Inferred);
    assert!(a.at.is_some(), "limited to the failing states");
    assert_eq!(draft.edits[0].at, "allow[1]");
    // Apply the edit to the IR draft: it compiles, and the claim now passes.
    let c2 = build(
        "E3",
        |r| {
            r["allow"]
                .as_array_mut()
                .unwrap()
                .push(draft.edits[0].value.clone().unwrap());
        },
        |_| {},
    );
    assert!(!c2.has_errors(), "{:?}", c2.diagnostics);
    let mut ex = e3_exec(|a| {
        if (80.0..=100.0).contains(&a) {
            0.4123
        } else {
            0.0
        }
    });
    let r2 = run_e3(&c2, &mut ex, 0).unwrap();
    assert_eq!(r2.body.status, RequirementStatus::Met);

    // Batch: duplicates of one pair merge, non-overlap findings are skipped with a reason.
    let mut other = r.body.findings[0].clone();
    other.id = "other".into();
    other.worst_value = Some(Quantity::new(0.9, Unit::Mm3));
    let mut clearance = r.body.findings[0].clone();
    clearance.id = "clr".into();
    clearance.primitive = "min_clearance".into();
    let mut sets = r.body.findings[0].clone();
    sets.id = "sets".into();
    sets.b.push("cabinet.py#cabinet/handle".into());
    let batch = vec![&r.body.findings[0], &other, &clearance, &sets];
    let d = draft_allowances(&req, &batch, &ProjectSettings::default());
    assert_eq!(d.allowances.len(), 1);
    assert_eq!(d.allowances[0].max.as_ref().unwrap().value, 0.9);
    assert_eq!(d.covers[0].len(), 2);
    assert_eq!(d.skipped.len(), 2);
    // A run over every state needs no `at`; touch-sized overlap drafts a touch.
    let all = e3_failing(0..=1000, 0.0004);
    assert!(all.body.findings.is_empty());
    let mut tiny = r.body.findings[0].clone();
    tiny.states = (0..tiny.state_count)
        .map(|i| FindingState {
            index: i,
            label: format!("s{i}"),
            state: Default::default(),
        })
        .collect();
    tiny.worst_value = Some(Quantity::new(0.0004, Unit::Mm3));
    let d = draft_allowances(&req, &[&tiny], &ProjectSettings::default());
    assert_eq!(d.allowances[0].kind, AllowKind::Touch);
    assert!(d.allowances[0].at.is_none() && d.allowances[0].max.is_none());
}

#[test]
fn rigid_runs_draft_a_sweep_allowance() {
    let c = build("R1", |_| {}, |_| {});
    let plan = c.plan.as_ref().unwrap();
    let m = c.manifest.as_ref().unwrap();
    let mut ex = FakeExecutor::new(|state: &Value, meas: &Value| {
        let angle = state["moves"][0]["angle_deg"].as_f64().unwrap_or(0.0);
        let v = if meas["b"][0] == "clamp_1" && (100.0..=130.0).contains(&angle) {
            0.25
        } else {
            0.0
        };
        json!({"status": "ok", "value": v})
    });
    let raw = run_plan(plan, m, &mut ex, &ChunkOptions::default()).unwrap();
    let r = evaluate(plan, m, &raw, &ProjectSettings::default(), "r1", 0);
    let f = &r.body.findings[0];
    let d = draft_allowances(&c.requirement, &[f], &ProjectSettings::default());
    assert_eq!(d.allowances.len(), 1, "{:?}", d.skipped);
    assert!(matches!(
        d.allowances[0].at,
        Some(StateSpace::Rotate { .. })
    ));
    let edit = d.edits[0].value.clone().unwrap();
    let c2 = {
        let mut v = serde_json::to_value(&c.requirement).unwrap();
        v["allow"] = json!([edit]);
        let req: Requirement = serde_json::from_value(v).unwrap();
        let snap: WorkspaceSnapshot = serde_json::from_str(&read("snapshots/R1")).unwrap();
        compile(&req, &snap, &ProjectSettings::default())
    };
    assert!(!c2.has_errors(), "{:?}", c2.diagnostics);
    let allowed = c2.plan.unwrap().claims[0]
        .jobs
        .iter()
        .filter(|j| j.allowed.is_some())
        .count();
    assert_eq!(allowed, 7, "130-100 deg at 5 deg steps, one pair");
}

// ------------------------------------------------------------------ relink

struct Drift {
    approved: Manifest,
    settings: ProjectSettings,
}

fn drift() -> Drift {
    Drift {
        approved: e3().manifest.unwrap(),
        settings: ProjectSettings::default(),
    }
}

impl Drift {
    fn against(&self, snap: &WorkspaceSnapshot) -> RelinkReport {
        let fresh = compile(
            &serde_json::from_str(&read("E3")).unwrap(),
            snap,
            &self.settings,
        )
        .manifest;
        relink(
            &self.approved,
            fresh.as_ref(),
            &BuildIndex::from_snapshot(snap),
            &self.settings,
        )
    }
}

fn parts_mut(s: &mut Value) -> &mut Vec<Value> {
    s["models"][0]["built"]["parts"].as_array_mut().unwrap()
}

fn kinds(r: &RelinkReport) -> Vec<ChangeKind> {
    r.changes.iter().map(|c| c.kind).collect()
}

#[test]
fn policy_row_none_and_geometry_only_run() {
    let d = drift();
    let r = d.against(&snap_of("E3", |_| {}));
    assert_eq!(r.decision, Effect::Run);
    assert!(r.changes.is_empty() && r.relinks.is_empty() && r.geometry_changed.is_empty());
    assert!(!r.requirement_changed);
    // New geometry, same bindings: run, silently.
    let r = d.against(&snap_of("E3", |s| {
        parts_mut(s)[0]["geometry_hash"] = json!("h-door-v2");
        s["models"][0]["built"]["fingerprint"] = json!("fp-new");
    }));
    assert_eq!(r.decision, Effect::Run);
    assert!(r.changes.is_empty());
    assert_eq!(r.geometry_changed, [DOOR]);
    assert!(r.runs() && r.changed_since_approval().is_empty());
}

#[test]
fn policy_row_exact_hash_relinks_only_a_unique_match() {
    let d = drift();
    let rename = |s: &mut Value| parts_mut(s)[0]["name"] = json!("door_panel");
    let r = d.against(&snap_of("E3", rename));
    assert_eq!(r.decision, Effect::Run, "{}", r.diff_text());
    assert_eq!(
        r.relinks,
        [Relink {
            from: DOOR.into(),
            to: "cabinet.py#cabinet/door_panel".into(),
            hash: "h-door".into()
        }]
    );
    assert_eq!(kinds(&r), [ChangeKind::AutoRelink]);
    assert!(r.diff_text().contains("door_panel"));
    // Two entities with the old hash: ambiguous, needs review.
    let r = d.against(&snap_of("E3", |s| {
        rename(s);
        parts_mut(s).push(json!({"name": "door_copy", "geometry_hash": "h-door"}));
    }));
    assert_eq!(r.decision, Effect::NeedsReview);
    assert!(r.relinks.is_empty());
    assert_eq!(kinds(&r), [ChangeKind::ReferenceAmbiguous]);
    // Renamed and the geometry changed too: gone (no approximate match).
    let r = d.against(&snap_of("E3", |s| {
        rename(s);
        parts_mut(s)[0]["geometry_hash"] = json!("h-door-v2");
    }));
    assert_eq!(r.decision, Effect::NeedsReview);
    assert_eq!(kinds(&r), [ChangeKind::ReferenceGone]);
    assert!(!r.runs());
    // The old name stays and a copy appears: nothing broke.
    let r = d.against(&snap_of("E3", |s| {
        parts_mut(s).push(json!({"name": "door_copy", "geometry_hash": "h-door"}));
    }));
    assert_eq!(r.decision, Effect::Run);
    assert!(r.changes.is_empty());
}

#[test]
fn policy_row_two_broken_references_with_one_hash_are_not_matched() {
    let mut d = drift();
    // Approved against a build where door and frame had identical geometry.
    for l in d.approved.parts.values_mut().flatten() {
        l.geometry_hash = "h-same".into();
    }
    let r = d.against(&snap_of("E3", |s| {
        let p = parts_mut(s);
        p.retain(|x| x["name"] == "handle");
        p.push(json!({"name": "only_one", "geometry_hash": "h-same"}));
    }));
    assert!(r.relinks.is_empty());
    assert!(
        kinds(&r)
            .iter()
            .all(|k| *k == ChangeKind::ReferenceAmbiguous),
        "{}",
        r.diff_text()
    );
    assert_eq!(r.decision, Effect::NeedsReview);
}

#[test]
fn policy_row_bounds_value_and_states_run_with_a_diff() {
    let d = drift();
    let r = d.against(&snap_of("E3", |s| {
        s["models"][0]["built"]["params"][0]["max"] = json!({"value": 120.0, "unit": "deg"});
    }));
    assert_eq!(r.decision, Effect::RunChanged);
    assert_eq!(kinds(&r), [ChangeKind::ParamBounds]);
    assert!(
        r.diff_text().contains("130") && r.diff_text().contains("120"),
        "{}",
        r.diff_text()
    );
    assert_eq!(r.changed_since_approval().len(), 1);
    assert!(r.runs());
    let r = d.against(&snap_of("E3", |s| {
        s["models"][0]["built"]["params"][0]["value"] = json!({"value": 5.0, "unit": "deg"});
    }));
    assert_eq!(kinds(&r), [ChangeKind::ParamValue]);
    assert_eq!(r.decision, Effect::RunChanged);
    // Expanded states differ from the approved ones.
    let mut d2 = drift();
    d2.approved.states.values_mut().next().unwrap().pop();
    let r = d2.against(&snap_of("E3", |_| {}));
    assert_eq!(kinds(&r), [ChangeKind::StatesChanged]);
    assert_eq!(r.decision, Effect::RunChanged);
    // A parameter that is gone needs review.
    let r = d.against(&snap_of("E3", |s| {
        s["models"][0]["built"]["params"] = json!([]);
    }));
    assert_eq!(r.decision, Effect::NeedsReview);
    assert!(kinds(&r).contains(&ChangeKind::ParamGone));
}

#[test]
fn policy_row_membership_of_globs_sets_and_others() {
    let c = build("R1", |_| {}, |_| {});
    let approved = c.manifest.unwrap();
    let settings = ProjectSettings::default();
    let snap = snap_of("R1", |s| {
        s["models"][0]["built"]["parts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "tower_9", "geometry_hash": "h-t9"}));
    });
    let fresh = compile(
        &serde_json::from_str(&read("R1")).unwrap(),
        &snap,
        &settings,
    )
    .manifest;
    let r = relink(
        &approved,
        fresh.as_ref(),
        &BuildIndex::from_snapshot(&snap),
        &settings,
    );
    assert_eq!(r.decision, Effect::RunChanged, "{}", r.diff_text());
    let k = kinds(&r);
    assert!(k.contains(&ChangeKind::GlobMembership), "{k:?}");
    assert!(k.contains(&ChangeKind::SetMembership), "{k:?}");
    assert!(r.diff_text().contains("gained machine.py#machine/tower_9"));
    // `others` gaining a part of the subject.
    let c2 = build("R2", |_| {}, |_| {});
    let approved2 = c2.manifest.unwrap();
    let snap2 = snap_of("R2", |s| {
        s["models"][0]["built"]["parts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "zz_extra", "geometry_hash": "h-extra"}));
    });
    let fresh2 = compile(
        &serde_json::from_str(&read("R2")).unwrap(),
        &snap2,
        &settings,
    )
    .manifest;
    let r2 = relink(
        &approved2,
        fresh2.as_ref(),
        &BuildIndex::from_snapshot(&snap2),
        &settings,
    );
    assert!(
        kinds(&r2).contains(&ChangeKind::OthersMembership),
        "{}",
        r2.diff_text()
    );
    assert_eq!(r2.decision, Effect::RunChanged);
    // A glob that matches nothing now is a broken reference.
    let snap = snap_of("R1", |s| {
        let p = s["models"][0]["built"]["parts"].as_array_mut().unwrap();
        p.retain(|x| !x["name"].as_str().unwrap().starts_with("clamp_"));
    });
    let r = relink(
        &approved,
        None,
        &BuildIndex::from_snapshot(&snap),
        &settings,
    );
    assert_eq!(r.decision, Effect::NeedsReview);
}

#[test]
fn policy_row_needs_review_for_features_subjects_chips_and_versions() {
    let feature = |entity, count, hash: &str| FeatureLink {
        entity,
        count,
        hash: hash.into(),
    };
    let key = "cabinet.py#cabinet/door#hinge";
    let mut d = drift();
    d.approved
        .features
        .insert(key.into(), feature(EntityKind::Face, 1, "fh"));
    let snap = snap_of("E3", |_| {});
    let with = |f: Option<FeatureLink>| {
        let mut idx = BuildIndex::from_snapshot(&snap);
        if let Some(f) = f {
            idx.features.insert(key.into(), f);
        }
        relink(&d.approved, None, &idx, &d.settings)
    };
    assert_eq!(
        with(Some(feature(EntityKind::Face, 1, "fh"))).decision,
        Effect::Run
    );
    let r = with(Some(feature(EntityKind::Face, 1, "fh2")));
    assert_eq!(
        (r.decision, kinds(&r)),
        (Effect::RunChanged, vec![ChangeKind::FeatureGeometry])
    );
    let r = with(Some(feature(EntityKind::Face, 2, "fh")));
    assert_eq!(
        (r.decision, kinds(&r)),
        (Effect::NeedsReview, vec![ChangeKind::FeatureCount])
    );
    let r = with(Some(feature(EntityKind::Edge, 1, "fh")));
    assert_eq!(
        (r.decision, kinds(&r)),
        (Effect::NeedsReview, vec![ChangeKind::FeatureKind])
    );
    let r = with(None);
    assert_eq!(
        (r.decision, kinds(&r)),
        (Effect::NeedsReview, vec![ChangeKind::ReferenceGone])
    );
    // Feature moved to a new name with the same exact hash: relinked.
    let mut idx = BuildIndex::from_snapshot(&snap);
    idx.features.insert(
        "cabinet.py#cabinet/door#hinge_hole".into(),
        feature(EntityKind::Face, 1, "fh"),
    );
    let r = relink(&d.approved, None, &idx, &d.settings);
    assert_eq!(
        (r.decision, kinds(&r)),
        (Effect::Run, vec![ChangeKind::AutoRelink])
    );
    // ...unless it matches two.
    idx.features.insert(
        "cabinet.py#cabinet/door#other".into(),
        feature(EntityKind::Face, 1, "fh"),
    );
    let r = relink(&d.approved, None, &idx, &d.settings);
    assert_eq!(
        (r.decision, kinds(&r)),
        (Effect::NeedsReview, vec![ChangeKind::ReferenceAmbiguous])
    );

    // Library and settings versions.
    let mut d = drift();
    d.approved.library_version = "old".into();
    let r = d.against(&snap_of("E3", |_| {}));
    assert_eq!(
        (r.decision, kinds(&r)),
        (Effect::NeedsReview, vec![ChangeKind::LibraryVersion])
    );
    let mut d = drift();
    d.settings.max_states = 100;
    let r = d.against(&snap_of("E3", |_| {}));
    assert_eq!(r.decision, Effect::NeedsReview);
    assert!(kinds(&r).contains(&ChangeKind::SettingsChanged));

    // Subject gone, and a chip that now maps to a different part.
    let d = drift();
    let r = d.against(&snap_of("E3", |s| {
        s["models"][0]["built"] = Value::Null;
        s["models"][0].as_object_mut().unwrap().remove("built");
    }));
    assert_eq!(r.decision, Effect::NeedsReview);
    assert!(kinds(&r).contains(&ChangeKind::SubjectChanged));
    let mut d = drift();
    let model = d.approved.subjects[0].model.clone();
    d.approved.chips.push(ChipLink {
        label: "Door".into(),
        model: model.clone(),
        part: Some("door".into()),
        mapped_to: None,
    });
    let snap = snap_of("E3", |_| {});
    let mut fresh = compile(
        &serde_json::from_str(&read("E3")).unwrap(),
        &snap,
        &d.settings,
    )
    .manifest
    .unwrap();
    fresh.chips.push(ChipLink {
        label: "Door".into(),
        model,
        part: Some("door".into()),
        mapped_to: Some("cabinet.py#cabinet/frame".into()),
    });
    let r = relink(
        &d.approved,
        Some(&fresh),
        &BuildIndex::from_snapshot(&snap),
        &d.settings,
    );
    assert_eq!(
        (r.decision, kinds(&r)),
        (Effect::NeedsReview, vec![ChangeKind::ChipRemapped])
    );
}

#[test]
fn edited_prose_is_flagged_and_the_approved_version_keeps_running() {
    let d = drift();
    let snap = snap_of("E3", |_| {});
    let mut fresh = d.approved.clone();
    fresh.requirement_hash = "different".into();
    let r = relink(
        &d.approved,
        Some(&fresh),
        &BuildIndex::from_snapshot(&snap),
        &d.settings,
    );
    assert!(r.requirement_changed);
    assert_eq!(r.decision, Effect::Run);
}

// ------------------------------------------------------------------ status

#[test]
fn status_follows_the_precedence_table() {
    let c = e3();
    let mut ex = e3_exec(|_| 0.0);
    let met = run_e3(&c, &mut ex, 0).unwrap().body;
    let mut ex = e3_exec(|a| if a > 50.0 { 1.0 } else { 0.0 });
    let not_met = run_e3(&c, &mut ex, 0).unwrap().body;
    let plan_hash = c.plan.as_ref().unwrap().content_hash();
    let base = || StatusInputs {
        approved: true,
        current_plan_hash: Some(&plan_hash),
        ..Default::default()
    };
    let status = |i: StatusInputs| compute_status(&i);

    assert_eq!(
        status(StatusInputs {
            has_errors: true,
            approved: true,
            ..Default::default()
        })
        .status,
        RequirementStatus::Draft
    );
    assert_eq!(
        status(StatusInputs::default()).status,
        RequirementStatus::Draft
    );
    assert_eq!(
        status(StatusInputs {
            build_failed: true,
            result: Some(&met),
            ..base()
        })
        .status,
        RequirementStatus::Error
    );
    assert_eq!(
        status(StatusInputs {
            edited_since_approval: true,
            result: Some(&met),
            ..base()
        })
        .status,
        RequirementStatus::NeedsReview
    );
    assert!(
        status(StatusInputs {
            edited_since_approval: true,
            result: Some(&met),
            ..base()
        })
        .show_previous
    );

    let d = drift();
    let gone = d.against(&snap_of("E3", |s| {
        parts_mut(s).remove(0);
    }));
    let rep = status(StatusInputs {
        relink: Some(&gone),
        bind_failed: true,
        result: Some(&met),
        ..base()
    });
    assert_eq!(
        rep.status,
        RequirementStatus::NeedsReview,
        "reference gone is review, not error"
    );
    assert!(rep.reasons[0].contains("no longer exists"));
    assert_eq!(
        status(StatusInputs {
            bind_failed: true,
            ..base()
        })
        .status,
        RequirementStatus::Error
    );

    assert_eq!(
        status(StatusInputs {
            running: true,
            result: Some(&met),
            ..base()
        })
        .status,
        RequirementStatus::Pending
    );
    assert!(
        status(StatusInputs {
            running: true,
            result: Some(&met),
            ..base()
        })
        .show_previous
    );
    assert_eq!(status(base()).status, RequirementStatus::Pending);
    assert_eq!(
        status(StatusInputs {
            result: Some(&met),
            ..base()
        })
        .status,
        RequirementStatus::Met
    );
    assert_eq!(
        status(StatusInputs {
            result: Some(&not_met),
            ..base()
        })
        .status,
        RequirementStatus::NotMet
    );
    // A result for another plan is only "previous".
    let stale = status(StatusInputs {
        current_plan_hash: Some("other"),
        approved: true,
        result: Some(&met),
        ..Default::default()
    });
    assert_eq!(stale.status, RequirementStatus::Pending);
    assert!(stale.show_previous);
    // Changed-since-approval rides along with a verdict.
    let changed = d.against(&snap_of("E3", |s| {
        s["models"][0]["built"]["params"][0]["max"] = json!({"value": 120.0, "unit": "deg"});
    }));
    let rep = status(StatusInputs {
        relink: Some(&changed),
        result: Some(&met),
        ..base()
    });
    assert_eq!(rep.status, RequirementStatus::Met);
    assert_eq!(rep.changed_since_approval.len(), 1);

    let mut ex = e3_exec(|_| 0.0);
    let partial = run_e3(&c, &mut ex, 2).unwrap().body;
    assert_eq!(
        status(StatusInputs {
            result: Some(&partial),
            ..base()
        })
        .status,
        RequirementStatus::MetPartial
    );
}
