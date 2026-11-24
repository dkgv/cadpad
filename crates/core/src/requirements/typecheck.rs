//! Typecheck (spec §3.4): argument kinds against library signatures, units,
//! comparator and state ranges, state-count limit, parameter bounds, and one
//! subject model per claim.
use super::bind::{Bindings, model_key};
use super::diagnostics::{Diagnostic, DiagnosticCode};
use super::expand::check_claim;
use super::ir::{Arg, Literal, ModelRef, ParamValues, Requirement, StateSpace};
use super::quantity::{Dimension, Quantity};
use super::refs::claim_models;
use super::settings::ProjectSettings;

fn state_spaces(arg: &Arg) -> Vec<&StateSpace> {
    match arg {
        Arg::States(s) => vec![s],
        Arg::List(items) => items.iter().flat_map(state_spaces).collect(),
        _ => vec![],
    }
}

fn within(q: &Quantity, min: Option<&Quantity>, max: Option<&Quantity>) -> Result<(), String> {
    let v = q.internal_value();
    let slack = 1e-9 * v.abs().max(1e-12);
    if let Some(lo) = min
        && v < lo.internal_value() - slack
    {
        return Err(format!("{q} is below the minimum {lo}"));
    }
    if let Some(hi) = max
        && v > hi.internal_value() + slack
    {
        return Err(format!("{q} is above the maximum {hi}"));
    }
    Ok(())
}

fn check_literal_bounds(
    lit: &Literal,
    at: &str,
    name: &str,
    bounds: (Quantity, Option<Quantity>, Option<Quantity>),
    out: &mut Vec<Diagnostic>,
) {
    let (declared, min, max) = bounds;
    if let Err(e) = lit.quantity().expect(declared.dimension()) {
        out.push(e.diagnostic(at));
    } else if let Err(why) = within(&lit.quantity(), min.as_ref(), max.as_ref()) {
        out.push(Diagnostic::new(
            DiagnosticCode::ETypeRange,
            at,
            format!("{name}: {why} (declared bounds of the model)"),
        ));
    }
}

fn check_bounds(
    space: &StateSpace,
    at: &str,
    subject: Option<&ModelRef>,
    b: &Bindings,
    out: &mut Vec<Diagnostic>,
) {
    match space {
        StateSpace::Param { param, values } => {
            let model = param.model.as_ref().or(subject);
            let Some(bound) =
                model.and_then(|m| b.params.get(&format!("{}:{}", model_key(m), param.name)))
            else {
                return; // unbound: already reported
            };
            let bounds = (bound.value, bound.min, bound.max);
            let name = param.name.as_str();
            match values {
                ParamValues::Value { value } => {
                    check_literal_bounds(value, &format!("{at}.values.value"), name, bounds, out)
                }
                ParamValues::Range { from, to, step } => {
                    check_literal_bounds(from, &format!("{at}.values.from"), name, bounds, out);
                    check_literal_bounds(to, &format!("{at}.values.to"), name, bounds, out);
                    if let Err(e) = step.quantity().expect(bound.value.dimension()) {
                        out.push(e.diagnostic(format!("{at}.values.step")));
                    }
                }
                ParamValues::List { values } => {
                    for (i, v) in values.iter().enumerate() {
                        check_literal_bounds(
                            v,
                            &format!("{at}.values.values[{i}]"),
                            name,
                            bounds,
                            out,
                        );
                    }
                }
            }
        }
        StateSpace::Joint { joint, range } => {
            let key = format!("{}:{}", model_key(&joint.model), joint.name);
            let Some(j) = b.joints.get(&key) else { return };
            let Some([lo, hi]) = j.limits else { return };
            // Joint limits are metres or radians; only angle and length ranges compare.
            let scale = |q: &Quantity| match q.dimension() {
                Dimension::Angle => Some(q.internal_value()),
                Dimension::Length => Some(q.internal_value() / 1000.0),
                _ => None,
            };
            for (field, lit) in [("from", &range.from), ("to", &range.to)] {
                if let Some(v) = scale(&lit.quantity())
                    && (v < lo - 1e-9 || v > hi + 1e-9)
                {
                    out.push(Diagnostic::new(
                        DiagnosticCode::ETypeRange,
                        format!("{at}.range.{field}"),
                        format!(
                            "{}: {} is outside the joint's limits [{lo}, {hi}]",
                            joint.name,
                            lit.quantity()
                        ),
                    ));
                }
            }
        }
        StateSpace::Product { factors } => {
            for (i, f) in factors.iter().enumerate() {
                check_bounds(f, &format!("{at}.factors[{i}]"), subject, b, out);
            }
        }
        StateSpace::Rest {} | StateSpace::Rotate { .. } | StateSpace::Translate { .. } => {}
    }
}

fn check_state_args(
    spaces: &[(&StateSpace, String)],
    subject: Option<&ModelRef>,
    b: &Bindings,
    settings: &ProjectSettings,
    at: &str,
    out: &mut Vec<Diagnostic>,
) {
    for (s, path) in spaces {
        check_bounds(s, path, subject, b, out);
    }
    let counts: Option<Vec<u64>> = spaces.iter().map(|(s, _)| s.state_count()).collect();
    if let Some(counts) = counts {
        let total = counts.iter().fold(0u64, |a, c| a.saturating_add(*c));
        if total > settings.max_states as u64 {
            out.push(Diagnostic::new(
                DiagnosticCode::ETypeStates,
                at,
                format!(
                    "{total} states is more than the limit of {}; use a larger step or fewer values",
                    settings.max_states
                ),
            ));
        }
    }
}

/// All type errors, in a stable order, without duplicates.
pub fn typecheck(req: &Requirement, b: &Bindings, settings: &ProjectSettings) -> Vec<Diagnostic> {
    let mut out = req.validate();
    for (i, claim) in req.claims.iter().enumerate() {
        let at = format!("claims[{i}]");
        out.extend(check_claim(claim, &at));
        let models = claim_models(req, i);
        if models.len() > 1 {
            let names: Vec<_> = models.iter().map(model_key).collect();
            out.push(Diagnostic::new(
                DiagnosticCode::ETypeKind,
                &at,
                format!(
                    "a claim is checked in one model, but this one refers to {}; check them in an assembly model",
                    names.join(" and ")
                ),
            ));
        }
        let subject = b.subjects.get(&claim.id);
        for (name, arg) in &claim.args {
            let path = format!("claims[{i}].args.{name}");
            let mut spaces: Vec<(&StateSpace, String)> = Vec::new();
            match arg {
                Arg::List(items) => {
                    for (j, it) in items.iter().enumerate() {
                        for sp in state_spaces(it) {
                            spaces.push((sp, format!("{path}[{j}]")));
                        }
                    }
                }
                other => {
                    for sp in state_spaces(other) {
                        spaces.push((sp, path.clone()));
                    }
                }
            }
            if !spaces.is_empty() {
                check_state_args(&spaces, subject, b, settings, &path, &mut out);
            }
        }
    }
    for (i, a) in req.allow.iter().enumerate() {
        if let Some(s) = &a.at {
            let at = format!("allow[{i}].at");
            let subject = super::refs::allowance_subject(req, i);
            check_state_args(
                &[(s, at.clone())],
                subject.as_ref(),
                b,
                settings,
                &at,
                &mut out,
            );
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|d| seen.insert((d.code, d.at.clone(), d.message.clone())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requirements::Unit;
    use crate::requirements::bind::bind;
    use crate::requirements::snapshot::*;
    use serde_json::json;

    fn snapshot() -> WorkspaceSnapshot {
        let part = |n: &str| PartInfo {
            name: n.into(),
            geometry_hash: "h".into(),
            source: None,
        };
        let model = |entry: &str, b: &str| ModelSnapshot {
            model: ModelRef {
                entry: entry.into(),
                builder: b.into(),
            },
            label: None,
            build_error: None,
            built: Some(BuiltModel {
                parts: vec![part("door"), part("frame")],
                params: vec![ParamInfo {
                    name: "door_angle".into(),
                    value: Quantity::new(0.0, Unit::Deg),
                    min: Some(Quantity::new(0.0, Unit::Deg)),
                    max: Some(Quantity::new(130.0, Unit::Deg)),
                }],
                joints: vec![JointInfo {
                    name: "hinge".into(),
                    limits: Some([0.0, 2.0]),
                }],
                ..Default::default()
            }),
        };
        WorkspaceSnapshot {
            models: vec![model("c.py", "c"), model("d.py", "d")],
        }
    }

    fn check(claims: serde_json::Value) -> Vec<Diagnostic> {
        let req: Requirement = serde_json::from_value(json!({
            "ir_version": 1, "id": "r", "name": "n", "text": "t", "claims": claims}))
        .unwrap();
        let (b, d) = bind(&req, &snapshot());
        assert!(d.is_empty(), "{d:?}");
        typecheck(&req, &b, &ProjectSettings::default())
    }

    fn part(model: &str, name: &str) -> serde_json::Value {
        json!({"part": {"model": {"entry": format!("{model}.py"), "builder": model}, "part": {"name": name}}})
    }
    fn angle(from: f64, to: f64, step: f64) -> serde_json::Value {
        let l = |v: f64, u: &str| json!({"value": v, "unit": u});
        json!({"states": {"kind": "param", "param": {"name": "door_angle"}, "values": {"shape": "range",
            "from": l(from, "deg"), "to": l(to, "deg"), "step": l(step, "deg")}}})
    }
    fn moves(over: serde_json::Value) -> serde_json::Value {
        json!([{"id": "c1", "rule": "moves_freely", "args": {"moving": part("c", "door"), "against": part("c", "frame"), "over": over}}])
    }
    fn at(d: &[Diagnostic]) -> Vec<(DiagnosticCode, &str)> {
        d.iter().map(|x| (x.code, x.at.as_str())).collect()
    }

    #[test]
    fn a_good_claim_has_no_diagnostics() {
        assert!(check(moves(angle(0.0, 110.0, 2.0))).is_empty());
    }

    #[test]
    fn parameter_bounds_are_checked_per_endpoint() {
        let d = check(moves(angle(0.0, 140.0, 2.0)));
        assert_eq!(
            at(&d),
            [(DiagnosticCode::ETypeRange, "claims[0].args.over.values.to")]
        );
        assert!(
            d[0].message.contains("above the maximum"),
            "{}",
            d[0].message
        );
        let d = check(moves(angle(-5.0, 10.0, 1.0)));
        assert_eq!(
            at(&d),
            [(
                DiagnosticCode::ETypeRange,
                "claims[0].args.over.values.from"
            )]
        );
    }

    #[test]
    fn units_must_match_the_parameter() {
        let mm = json!({"states": {"kind": "param", "param": {"name": "door_angle"}, "values": {"shape": "value", "value": {"value": 1.0, "unit": "mm"}}}});
        assert_eq!(
            at(&check(moves(mm))),
            [(
                DiagnosticCode::ETypeUnit,
                "claims[0].args.over.values.value"
            )]
        );
    }

    #[test]
    fn state_count_limit_is_enforced_including_unions() {
        let d = check(moves(angle(0.0, 130.0, 0.1)));
        assert_eq!(
            at(&d),
            [(DiagnosticCode::ETypeStates, "claims[0].args.over")]
        );
        // 3 x 200 is under 512 each, but a union sums.
        let union = json!({"list": [angle(0.0, 20.0, 0.1), angle(20.0, 40.0, 0.1), angle(40.0, 60.0, 0.1)]});
        let d = check(moves(union));
        assert_eq!(
            at(&d),
            [(DiagnosticCode::ETypeStates, "claims[0].args.over")]
        );
    }

    #[test]
    fn product_counts_multiply() {
        let big = json!({"states": {"kind": "product", "factors": [angle(0.0, 20.0, 1.0)["states"].clone(), angle(0.0, 25.0, 1.0)["states"].clone()]}});
        let d = check(moves(big));
        assert!(
            d.iter().any(|x| x.code == DiagnosticCode::ETypeStates),
            "{d:?}"
        );
    }

    #[test]
    fn signatures_comparators_and_ranges_are_checked() {
        let d =
            check(json!([{"id": "c1", "rule": "no_overlap", "args": {"a": part("c", "door")}}]));
        assert_eq!(at(&d), [(DiagnosticCode::ETypeKind, "claims[0].args")]);
        let bad = json!({"compare": {"op": ">=", "value": {"value": 1.0, "unit": "deg"}}});
        let d = check(json!([{"id": "c1", "rule": "moves_freely", "args": {
            "moving": part("c", "door"), "against": part("c", "frame"), "over": {"states": {"kind": "rest"}}, "clearance": bad}}]));
        assert_eq!(
            at(&d),
            [(DiagnosticCode::ETypeUnit, "claims[0].args.clearance")]
        );
        let inverted = json!({"compare": {"op": "in", "lo": {"value": 2.0, "unit": "mm"}, "hi": {"value": 1.0, "unit": "mm"}}});
        let d = check(json!([{"id": "c1", "rule": "moves_freely", "args": {
            "moving": part("c", "door"), "against": part("c", "frame"), "over": {"states": {"kind": "rest"}}, "clearance": inverted}}]));
        assert_eq!(
            at(&d),
            [(DiagnosticCode::ETypeRange, "claims[0].args.clearance")]
        );
    }

    #[test]
    fn a_claim_has_one_subject_model() {
        let d = check(
            json!([{"id": "c1", "rule": "no_overlap", "args": {"a": part("c", "door"), "b": part("d", "frame")}}]),
        );
        assert_eq!(at(&d), [(DiagnosticCode::ETypeKind, "claims[0]")]);
        assert!(d[0].message.contains("c.py#c and d.py#d"));
    }

    #[test]
    fn joint_ranges_respect_limits() {
        let joint = |to: f64| {
            json!({"states": {"kind": "joint", "joint": {"model": {"entry": "c.py", "builder": "c"}, "name": "hinge"},
            "range": {"from": {"value": 0.0, "unit": "rad"}, "to": {"value": to, "unit": "rad"}, "step": {"value": 0.5, "unit": "rad"}}}})
        };
        assert!(check(moves(joint(2.0))).is_empty());
        assert_eq!(
            at(&check(moves(joint(2.5)))),
            [(DiagnosticCode::ETypeRange, "claims[0].args.over.range.to")]
        );
    }
}
