//! Clash findings (spec §6.2, P1.60-P1.61). A finding is one pair failing over
//! one contiguous run of states of one primitive claim. Identity is
//! `(claim, pair, run)`: a run in the next result that overlaps a previous run
//! of the same claim and pair is the same problem (`still_failing`), a run
//! with no earlier counterpart is `new`, and an earlier run with no
//! counterpart now is `resolved`. There is no ignore: accepting a clash drafts
//! an allowance, which goes through review like any IR edit.
use super::diagnostics::IrEdit;
use super::ir::{
    AllowKind, Allowance, Literal, ModelRef, ParamValues, PartName, PartRef, Requirement, Source,
    StateSpace, Sweep, Target,
};
use super::quantity::{Quantity, Unit};
use super::settings::ProjectSettings;
use super::states::{ConcreteState, StateValue, sweep_values};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FindingLabel {
    New,
    StillFailing,
    /// Failed last run, passes now.
    Resolved,
}

/// One failing state of a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindingState {
    /// Index into the claim's states.
    pub index: usize,
    /// Deterministic description (`door_angle = 108 deg`); the state's
    /// identity across runs, since sweeps can be resampled.
    pub label: String,
    pub state: ConcreteState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    /// Stable id: `claim|a|b|first state`, inherited from the earlier run's
    /// finding when this one continues it.
    pub id: String,
    /// Primitive claim id.
    pub claim: String,
    /// IR claim it came from.
    pub origin_claim: String,
    pub primitive: String,
    pub a: Vec<String>,
    pub b: Vec<String>,
    pub first_state: usize,
    pub last_state: usize,
    /// The contiguous failing states, ascending.
    pub states: Vec<FindingState>,
    /// States the claim has in total.
    pub state_count: usize,
    /// Value at the worst state of the run.
    pub worst_value: Option<Quantity>,
    /// Margin at the worst state of the run (negative = missed).
    pub worst_margin: Option<Quantity>,
    /// Set by [`label_findings`]; unlabelled inside a result body so that the
    /// result hash does not depend on the previous run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<FindingLabel>,
}

impl Finding {
    fn same_pair(&self, o: &Finding) -> bool {
        self.claim == o.claim && self.a == o.a && self.b == o.b
    }
    fn overlaps(&self, o: &Finding) -> bool {
        let mine: BTreeSet<&str> = self.states.iter().map(|s| s.label.as_str()).collect();
        o.states.iter().any(|s| mine.contains(s.label.as_str()))
    }
}

/// Label this run's findings against the previous run's (P1.60). Returns the
/// current findings (`new` / `still_failing`) followed by the earlier ones
/// that are gone (`resolved`), all in (claim, pair, run) order. A finding that
/// continues an earlier one takes its id; when a run splits, only the first
/// piece inherits it.
pub fn label_findings(current: &[Finding], previous: &[Finding]) -> Vec<Finding> {
    let mut prev: Vec<&Finding> = previous
        .iter()
        .filter(|f| f.label != Some(FindingLabel::Resolved))
        .collect();
    prev.sort_by(|x, y| x.id.cmp(&y.id));
    let mut matched = vec![false; prev.len()];
    let mut adopted = vec![false; prev.len()];
    let mut out = Vec::new();
    for cur in current {
        let hits: Vec<usize> = (0..prev.len())
            .filter(|&i| prev[i].same_pair(cur) && prev[i].overlaps(cur))
            .collect();
        let mut f = cur.clone();
        if hits.is_empty() {
            f.label = Some(FindingLabel::New);
        } else {
            f.label = Some(FindingLabel::StillFailing);
            for &i in &hits {
                matched[i] = true;
            }
            if let Some(&i) = hits.iter().find(|&&i| !adopted[i]) {
                adopted[i] = true;
                f.id = prev[i].id.clone();
            }
        }
        out.push(f);
    }
    for (i, p) in prev.iter().enumerate() {
        if !matched[i] {
            let mut f = (*p).clone();
            f.label = Some(FindingLabel::Resolved);
            out.push(f);
        }
    }
    out.sort_by(|x, y| {
        (
            &x.claim,
            &x.a,
            &x.b,
            x.first_state,
            x.label.map(|l| l as u8),
        )
            .cmp(&(
                &y.claim,
                &y.a,
                &y.b,
                y.first_state,
                y.label.map(|l| l as u8),
            ))
    });
    out
}

// ------------------------------------------------------------ accept a clash

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SkippedFinding {
    pub id: String,
    pub reason: String,
}

/// A drafted set of allowances (P1.61). Nothing is applied: the edits are for
/// the IR draft, which then goes through compile and review.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AllowanceDraft {
    pub allowances: Vec<Allowance>,
    /// Appends to `allow` (`allow[n]`, `allow[n+1]`, ...), ready to apply to
    /// the IR draft.
    pub edits: Vec<IrEdit>,
    /// Finding ids each allowance covers, parallel to `allowances`.
    pub covers: Vec<Vec<String>>,
    pub skipped: Vec<SkippedFinding>,
    /// Things the reviewer should look at (e.g. an allowance that is wider
    /// than the failing run).
    pub warnings: Vec<String>,
}

fn ceil_sig(v: f64, digits: i32) -> f64 {
    if v <= 0.0 || !v.is_finite() {
        return v;
    }
    let scale = 10f64.powi(digits - 1 - v.log10().floor() as i32);
    let r = (v * scale).ceil() / scale;
    if r < v { v } else { r }
}

fn part_target(key: &str) -> Option<Target> {
    let (model, name) = super::worker_plan::split_part_key(key)?;
    let (entry, builder) = model.split_once('#')?;
    Some(Target::Part(PartRef {
        model: ModelRef {
            entry: entry.to_string(),
            builder: builder.to_string(),
        },
        part: PartName::Name(name.to_string()),
    }))
}

/// The state space covering exactly a run, when the IR can say so.
fn run_states(f: &Finding) -> Result<Option<StateSpace>, String> {
    if f.states.len() == f.state_count {
        return Ok(None); // fails at every state: no `at`
    }
    let values: Vec<&Vec<StateValue>> = f.states.iter().map(|s| &s.state.values).collect();
    if values.iter().any(|v| v.len() != 1) {
        return Err("the failing states combine several state spaces".into());
    }
    let inferred = |q: Quantity| Literal {
        value: q.value,
        unit: q.unit,
        source: Source::Inferred,
        note: Some(format!("failing state of {}", f.claim)),
    };
    match values[0][0].clone() {
        StateValue::Param { param, .. } => {
            let mut qs = Vec::new();
            for v in &values {
                match &v[0] {
                    StateValue::Param { param: p, value } if *p == param => qs.push(*value),
                    _ => return Err("the failing states differ in kind".into()),
                }
            }
            let values = if qs.len() == 1 {
                ParamValues::Value {
                    value: inferred(qs[0]),
                }
            } else {
                ParamValues::List {
                    values: qs.into_iter().map(inferred).collect(),
                }
            };
            Ok(Some(StateSpace::Param { param, values }))
        }
        StateValue::Rotate { moving, axis, .. }
        | StateValue::Translate {
            moving,
            direction: axis,
            ..
        } => {
            let rotate = matches!(values[0][0], StateValue::Rotate { .. });
            let mut qs = Vec::new();
            for v in &values {
                match &v[0] {
                    StateValue::Rotate {
                        moving: m,
                        axis: a,
                        angle,
                    } if rotate && *m == moving && *a == axis => qs.push(*angle),
                    StateValue::Translate {
                        moving: m,
                        direction: a,
                        distance,
                    } if !rotate && *m == moving && *a == axis => qs.push(*distance),
                    _ => return Err("the failing states differ in kind".into()),
                }
            }
            let unit = qs[0].unit;
            let conv: Vec<f64> = qs
                .iter()
                .map(|q| q.convert_to(unit).map(|c| c.value))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            let step = if conv.len() > 1 {
                conv[1] - conv[0]
            } else {
                1.0
            };
            let range = Sweep {
                from: inferred(Quantity::new(conv[0], unit)),
                to: inferred(Quantity::new(*conv.last().expect("non-empty"), unit)),
                step: inferred(Quantity::new(step, unit)),
            };
            let regenerated = sweep_values(&range.from, &range.to, &range.step);
            let same = regenerated.len() == conv.len()
                && regenerated.iter().zip(&qs).all(|(r, q)| r.same_value(q));
            if !same {
                return Err("the failing states are not an evenly stepped sweep".into());
            }
            Ok(Some(if rotate {
                StateSpace::Rotate {
                    moving,
                    axis,
                    range,
                }
            } else {
                StateSpace::Translate {
                    moving,
                    direction: axis,
                    range,
                }
            }))
        }
        StateValue::Joint { .. } => Err("joint states cannot be expressed in `at` yet".into()),
        StateValue::Simulate { .. } => {
            Err("simulation traces cannot be drafted as allowances".into())
        }
    }
}

/// Draft allowances from selected findings (P1.61). Several findings give one
/// draft set: findings of the same pair and states merge into one allowance
/// whose limit covers the worst of them. Only overlap between two single
/// parts can be allowed; anything else is returned in `skipped` with the
/// reason. The limit is the measured overlap rounded up to 3 significant
/// digits, never unbounded (spec §3.5).
pub fn draft_allowances(
    req: &Requirement,
    findings: &[&Finding],
    settings: &ProjectSettings,
) -> AllowanceDraft {
    let eps = settings.overlap_tolerance.internal_value();
    let mut out = AllowanceDraft::default();
    // (a, b, at json) -> (max overlap, claims, finding ids, at)
    type Group = (f64, BTreeSet<String>, Vec<String>, Option<StateSpace>);
    let mut groups: BTreeMap<(String, String, String), Group> = BTreeMap::new();
    for f in findings {
        let skip = |out: &mut AllowanceDraft, why: &str| {
            out.skipped.push(SkippedFinding {
                id: f.id.clone(),
                reason: why.into(),
            });
        };
        if f.label == Some(FindingLabel::Resolved) {
            skip(&mut out, "it is resolved: nothing to accept");
            continue;
        }
        if f.primitive != "interference" {
            skip(&mut out, "only overlap between parts can be allowed");
            continue;
        }
        if f.a.len() != 1 || f.b.len() != 1 {
            skip(
                &mut out,
                "this check measures sets as a whole; allowances apply to part pairs",
            );
            continue;
        }
        let Some(v) = f
            .worst_value
            .as_ref()
            .map(Quantity::internal_value)
            .filter(|v| v.is_finite())
        else {
            skip(&mut out, "the finding has no measured overlap");
            continue;
        };
        let at = match run_states(f) {
            Ok(at) => at,
            Err(why) => {
                skip(
                    &mut out,
                    &format!("cannot limit the allowance to the failing states: {why}"),
                );
                continue;
            }
        };
        let at_key = at
            .as_ref()
            .map(|s| serde_json::to_string(s).expect("state space serializes"))
            .unwrap_or_default();
        let g = groups
            .entry((f.a[0].clone(), f.b[0].clone(), at_key))
            .or_insert((0.0, BTreeSet::new(), vec![], at));
        g.0 = g.0.max(v);
        g.1.insert(f.claim.clone());
        g.2.push(f.id.clone());
    }
    let base = req.allow.len();
    for ((a, b, _), (v, claims, ids, at)) in groups {
        let (Some(ta), Some(tb)) = (part_target(&a), part_target(&b)) else {
            for id in ids {
                out.skipped.push(SkippedFinding {
                    id,
                    reason: format!("cannot name parts {a} and {b}"),
                });
            }
            continue;
        };
        let (kind, max) = if v <= eps {
            (AllowKind::Touch, None)
        } else {
            let m = ceil_sig(v, 3);
            (
                AllowKind::Interference,
                Some(Literal {
                    value: m,
                    unit: Unit::Mm3,
                    source: Source::Inferred,
                    note: Some("measured overlap, rounded up".into()),
                }),
            )
        };
        let what = match &max {
            Some(m) => format!("overlap up to {} mm3", m.value),
            None => "contact".to_string(),
        };
        let claims: Vec<String> = claims.into_iter().collect();
        let allowance = Allowance {
            pair: [ta, tb],
            kind,
            max,
            at,
            reason: format!(
                "accepted clash {a} / {b}: {what} (claim {})",
                claims.join(", ")
            ),
            source: Source::Inferred,
            span: None,
        };
        out.edits.push(IrEdit {
            at: format!("allow[{}]", base + out.allowances.len()),
            value: Some(serde_json::to_value(&allowance).expect("allowance serializes")),
        });
        out.allowances.push(allowance);
        out.covers.push(ids);
    }
    out
}
