//! Provenance check (spec §3.6): every value and claim says where it comes
//! from, and the compiler verifies it. A claim that fails is not an error:
//! it is downgraded to `inferred`, so the review shows it marked and approving
//! the review confirms it. The LLM can't introduce anything unverified without
//! it being marked.
use super::bind::Bindings;
use super::ir::{
    Allowance, Arg, Claim, Comparator, Literal, ParamValues, Requirement, Source, StateSpace,
    Sweep, Token,
};
use super::parse::{Parsed, parse_quantities, quantity_in_span, token_in_span};
use super::settings::ProjectSettings;
use super::snapshot::{BuiltModel, WorkspaceSnapshot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One thing whose claimed source did not check out, and is now `inferred`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Downgrade {
    /// JSON path of the value, claim, allowance or set.
    pub at: String,
    pub was: Source,
    pub reason: String,
}

pub enum ValueMut<'a> {
    Literal(&'a mut Literal),
    Token(&'a mut Token),
}

fn lit(l: &mut Literal, at: String, f: &mut dyn FnMut(&str, ValueMut)) {
    f(&at, ValueMut::Literal(l));
}

fn cmp_values(c: &mut Comparator, at: &str, f: &mut dyn FnMut(&str, ValueMut)) {
    match c {
        Comparator::Ge { value } | Comparator::Le { value } => lit(value, format!("{at}.value"), f),
        Comparator::In { lo, hi } => {
            lit(lo, format!("{at}.lo"), f);
            lit(hi, format!("{at}.hi"), f);
        }
        Comparator::Approx { target, tol } => {
            lit(target, format!("{at}.target"), f);
            lit(tol, format!("{at}.tol"), f);
        }
        Comparator::IsTrue {} | Comparator::IsFalse {} => {}
    }
}

fn sweep_values_mut(s: &mut Sweep, at: &str, f: &mut dyn FnMut(&str, ValueMut)) {
    lit(&mut s.from, format!("{at}.from"), f);
    lit(&mut s.to, format!("{at}.to"), f);
    lit(&mut s.step, format!("{at}.step"), f);
}

/// Every literal in a state space.
pub fn state_values_mut(s: &mut StateSpace, at: &str, f: &mut dyn FnMut(&str, ValueMut)) {
    match s {
        StateSpace::Rest {} | StateSpace::Simulate { .. } => {}
        StateSpace::Param { values, .. } => match values {
            ParamValues::Value { value } => lit(value, format!("{at}.values.value"), f),
            ParamValues::Range { from, to, step } => {
                lit(from, format!("{at}.values.from"), f);
                lit(to, format!("{at}.values.to"), f);
                lit(step, format!("{at}.values.step"), f);
            }
            ParamValues::List { values } => {
                for (i, v) in values.iter_mut().enumerate() {
                    lit(v, format!("{at}.values.values[{i}]"), f);
                }
            }
        },
        StateSpace::Rotate { range, .. }
        | StateSpace::Translate { range, .. }
        | StateSpace::Joint { range, .. } => sweep_values_mut(range, &format!("{at}.range"), f),
        StateSpace::Product { factors } => {
            for (i, x) in factors.iter_mut().enumerate() {
                state_values_mut(x, &format!("{at}.factors[{i}]"), f);
            }
        }
    }
}

/// Every literal and token in an argument.
pub fn arg_values_mut(a: &mut Arg, at: &str, f: &mut dyn FnMut(&str, ValueMut)) {
    match a {
        Arg::Quantity(l) => lit(l, at.to_string(), f),
        Arg::Token(t) => f(at, ValueMut::Token(t)),
        Arg::Compare(c) => cmp_values(c, at, f),
        Arg::States(s) => state_values_mut(s, at, f),
        Arg::List(items) => {
            for (i, x) in items.iter_mut().enumerate() {
                arg_values_mut(x, &format!("{at}[{i}]"), f);
            }
        }
        _ => {}
    }
}

struct Context<'a> {
    text: &'a str,
    span: Option<[usize; 2]>,
    built: Option<&'a BuiltModel>,
    settings: &'a ProjectSettings,
}

impl Context<'_> {
    fn verify(&self, source: Source, v: &ValueMut) -> Result<(), String> {
        match (source, v) {
            (Source::Inferred, _) => Ok(()),
            (Source::Text, v) => {
                let span = self.span.ok_or("no prose span to find it in")?;
                let found = match v {
                    ValueMut::Literal(l) => quantity_in_span(self.text, span, &l.quantity()),
                    ValueMut::Token(t) => token_in_span(self.text, span, &t.text),
                };
                if found {
                    Ok(())
                } else {
                    Err("not found in the claim's prose".into())
                }
            }
            (Source::Model, ValueMut::Literal(l)) => {
                let built = self.built.ok_or("the subject model isn't built")?;
                let q = l.quantity();
                let known = built
                    .params
                    .iter()
                    .flat_map(|p| [Some(p.value), p.min, p.max])
                    .flatten()
                    .chain(
                        built
                            .constants
                            .iter()
                            .flat_map(|c| c.values.iter().copied()),
                    )
                    .any(|m| m.same_value(&q));
                if known {
                    Ok(())
                } else {
                    Err("not a parameter value, bound or constant of the model".into())
                }
            }
            (Source::Model, ValueMut::Token(t)) => {
                let built = self.built.ok_or("the subject model isn't built")?;
                let known = built.params.iter().any(|p| p.name == t.text)
                    || built.parts.iter().any(|p| p.name == t.text)
                    || built.joints.iter().any(|j| j.name == t.text)
                    || built.constants.iter().any(|c| c.name == t.text);
                if known {
                    Ok(())
                } else {
                    Err("not a name in the model".into())
                }
            }
            (Source::Library, ValueMut::Literal(l)) => {
                if l.quantity().same_value(&self.settings.overlap_tolerance) {
                    Ok(())
                } else {
                    Err(
                        "library values are notation lookups and computational settings only"
                            .into(),
                    )
                }
            }
            (Source::Library, ValueMut::Token(t)) => {
                let whole = parse_quantities(&t.text).into_iter().any(|m| {
                    m.span == [0, t.text.chars().count()]
                        && matches!(m.value, Parsed::Thread { .. } | Parsed::Fit { .. })
                });
                if whole {
                    Ok(())
                } else {
                    Err("not notation the library knows (thread size, fit class)".into())
                }
            }
        }
    }
}

fn downgrade_value(v: ValueMut, reason: String, at: &str, out: &mut Vec<Downgrade>) {
    let (source, note): (&mut Source, &mut Option<String>) = match v {
        ValueMut::Literal(l) => (&mut l.source, &mut l.note),
        ValueMut::Token(t) => (&mut t.source, &mut t.note),
    };
    out.push(Downgrade {
        at: at.into(),
        was: *source,
        reason: reason.clone(),
    });
    *source = Source::Inferred;
    note.get_or_insert(format!("not verified: {reason}"));
}

/// A function that feeds every value of something to a callback.
type Visitor<'a> = dyn FnMut(&mut dyn FnMut(&str, ValueMut)) + 'a;

fn check_values(ctx: &Context, visit: &mut Visitor, out: &mut Vec<Downgrade>) {
    visit(&mut |path, v| {
        let source = match &v {
            ValueMut::Literal(l) => l.source,
            ValueMut::Token(t) => t.source,
        };
        if let Err(reason) = ctx.verify(source, &v) {
            downgrade_value(v, reason, path, out);
        }
    });
}

fn claim_source(claim: &mut Claim, i: usize, out: &mut Vec<Downgrade>) {
    if claim.source == Source::Text && claim.span.is_none() {
        out.push(Downgrade {
            at: format!("claims[{i}].source"),
            was: Source::Text,
            reason: "a claim from the text needs the span it implements".into(),
        });
        claim.source = Source::Inferred;
        claim.note.get_or_insert("no prose span".into());
    }
}

fn allowance_source(a: &mut Allowance, i: usize, out: &mut Vec<Downgrade>) {
    if a.source == Source::Text && a.span.is_none() {
        out.push(Downgrade {
            at: format!("allow[{i}].source"),
            was: Source::Text,
            reason: "an allowance from the text needs the span it implements".into(),
        });
        a.source = Source::Inferred;
    }
}

/// Verify every source in the requirement. Returns the requirement with
/// failures downgraded to `inferred`, and a record of each downgrade.
pub fn check_provenance(
    req: &Requirement,
    bindings: &Bindings,
    snap: &WorkspaceSnapshot,
    settings: &ProjectSettings,
) -> (Requirement, Vec<Downgrade>) {
    let mut out_req = req.clone();
    let mut downgrades = Vec::new();
    let text = req.text.clone();
    for (i, claim) in out_req.claims.iter_mut().enumerate() {
        claim_source(claim, i, &mut downgrades);
        let built = bindings
            .subjects
            .get(&claim.id)
            .and_then(|m| snap.model(m))
            .and_then(|m| m.built.as_ref());
        let ctx = Context {
            text: &text,
            span: claim.span,
            built,
            settings,
        };
        for (name, arg) in claim.args.iter_mut() {
            let at = format!("claims[{i}].args.{name}");
            check_values(&ctx, &mut |f| arg_values_mut(arg, &at, f), &mut downgrades);
        }
    }
    for (i, a) in out_req.allow.iter_mut().enumerate() {
        allowance_source(a, i, &mut downgrades);
        // An allowance's subject is the model of the parts it names.
        let built = a
            .pair
            .iter()
            .find_map(|t| super::bind::members_of_target(t, bindings))
            .and_then(|m| m.first().map(|p| p.model.clone()))
            .and_then(|m| snap.model(&m))
            .and_then(|m| m.built.as_ref());
        let ctx = Context {
            text: &text,
            span: a.span,
            built,
            settings,
        };
        let at = format!("allow[{i}]");
        if let Some(max) = a.max.as_mut() {
            check_values(
                &ctx,
                &mut |f| f(&format!("{at}.max"), ValueMut::Literal(max)),
                &mut downgrades,
            );
        }
        if let Some(s) = a.at.as_mut() {
            check_values(
                &ctx,
                &mut |f| state_values_mut(s, &format!("{at}.at"), f),
                &mut downgrades,
            );
        }
    }
    for (name, set) in out_req.sets.iter_mut() {
        if set.source == Source::Text && set.span.is_none() {
            downgrades.push(Downgrade {
                at: format!("sets.{name}.source"),
                was: Source::Text,
                reason: "a set from the text needs the span it implements".into(),
            });
            set.source = Source::Inferred;
        }
    }
    (out_req, downgrades)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requirements::bind::bind;
    use crate::requirements::snapshot::*;
    use crate::requirements::{ModelRef, Quantity, Unit};
    use serde_json::json;

    fn snapshot() -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            models: vec![ModelSnapshot {
                model: ModelRef {
                    entry: "c.py".into(),
                    builder: "c".into(),
                },
                label: None,
                build_error: None,
                built: Some(BuiltModel {
                    parts: vec![PartInfo {
                        name: "door".into(),
                        geometry_hash: "h".into(),
                        source: None,
                        fastener: None,
                        source_assets: vec![],
                    }],
                    params: vec![ParamInfo {
                        name: "door_angle".into(),
                        value: Quantity::new(0.0, Unit::Deg),
                        min: Some(Quantity::new(0.0, Unit::Deg)),
                        max: Some(Quantity::new(130.0, Unit::Deg)),
                    }],
                    constants: vec![ConstantInfo {
                        name: "TRAVEL".into(),
                        values: vec![Quantity::new(4.0, Unit::Mm)],
                    }],
                    ..Default::default()
                }),
            }],
        }
    }

    fn run(claim: serde_json::Value, text: &str) -> (Requirement, Vec<Downgrade>) {
        let req: Requirement = serde_json::from_value(json!({
            "ir_version": 1, "id": "r", "name": "n", "text": text, "claims": [claim]}))
        .unwrap();
        let snap = snapshot();
        let (b, _) = bind(&req, &snap);
        check_provenance(&req, &b, &snap, &ProjectSettings::default())
    }

    fn door() -> serde_json::Value {
        json!({"part": {"model": {"entry": "c.py", "builder": "c"}, "part": {"name": "door"}}})
    }

    #[test]
    fn text_values_must_appear_in_the_span() {
        let claim = json!({"id": "c1", "rule": "x", "span": [0, 31], "args": {
            "a": door(),
            "ok": {"quantity": {"value": 110.0, "unit": "deg"}},
            "bad": {"quantity": {"value": 120.0, "unit": "deg"}},
            "size": {"token": {"text": "M8"}},
            "wrong": {"token": {"text": "M10"}}}});
        let (req, d) = run(claim, "opens to 110° with m8s attached");
        let at: Vec<_> = d.iter().map(|x| x.at.as_str()).collect();
        assert_eq!(at, ["claims[0].args.bad", "claims[0].args.wrong"]);
        let Arg::Quantity(l) = &req.claims[0].args["bad"] else {
            panic!()
        };
        assert_eq!(l.source, Source::Inferred);
        assert!(l.note.as_deref().unwrap().contains("not found"));
        let Arg::Quantity(l) = &req.claims[0].args["ok"] else {
            panic!()
        };
        assert_eq!(l.source, Source::Text);
    }

    #[test]
    fn m8s_style_tokens_match_in_the_whole_span() {
        let claim = json!({"id": "c1", "rule": "x", "span": [0, 30], "args": {"size": {"token": {"text": "M8"}}}});
        let (_, d) = run(claim, "attach using m8s please");
        assert!(d.is_empty());
    }

    #[test]
    fn model_values_must_come_from_the_model() {
        let claim = json!({"id": "c1", "rule": "x", "span": [0, 4], "args": {
            "a": door(),
            "bound": {"quantity": {"value": 130.0, "unit": "deg", "source": "model"}},
            "const": {"quantity": {"value": 4.0, "unit": "mm", "source": "model"}},
            "made_up": {"quantity": {"value": 7.0, "unit": "mm", "source": "model"}},
            "name": {"token": {"text": "door_angle", "source": "model"}}}});
        let (_, d) = run(claim, "text");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].at, "claims[0].args.made_up");
        assert_eq!(d[0].was, Source::Model);
    }

    #[test]
    fn library_values_are_notation_or_settings_only() {
        let claim = json!({"id": "c1", "rule": "x", "span": [0, 4], "args": {
            "eps": {"quantity": {"value": 0.001, "unit": "mm3", "source": "library"}},
            "clearance": {"quantity": {"value": 0.4, "unit": "mm", "source": "library"}},
            "fit": {"token": {"text": "H7", "source": "library"}},
            "word": {"token": {"text": "freely", "source": "library"}}}});
        let (_, d) = run(claim, "text");
        let at: Vec<_> = d.iter().map(|x| x.at.as_str()).collect();
        assert_eq!(at, ["claims[0].args.clearance", "claims[0].args.word"]);
    }

    #[test]
    fn claims_without_a_span_and_nested_values_are_covered() {
        let claim = json!({"id": "c1", "rule": "x", "args": {
            "a": door(),
            "cmp": {"compare": {"op": "in", "lo": {"value": 1.0, "unit": "mm"}, "hi": {"value": 2.0, "unit": "mm", "source": "inferred"}}},
            "over": {"states": {"kind": "param", "param": {"name": "door_angle"}, "values": {"shape": "range",
                "from": {"value": 0.0, "unit": "deg", "source": "model"},
                "to": {"value": 110.0, "unit": "deg"}, "step": {"value": 2.0, "unit": "deg", "source": "inferred"}}}}}});
        let (req, d) = run(claim, "anything 110°");
        let at: Vec<_> = d.iter().map(|x| x.at.as_str()).collect();
        // claim has no span: the claim itself and each text value downgrade
        assert_eq!(
            at,
            [
                "claims[0].source",
                "claims[0].args.cmp.lo",
                "claims[0].args.over.values.to"
            ]
        );
        assert_eq!(req.claims[0].source, Source::Inferred);
        let again = check_provenance(
            &req,
            &Bindings::default(),
            &snapshot(),
            &ProjectSettings::default(),
        )
        .1;
        assert!(again.iter().all(|x| x.at != "claims[0].source"));
    }

    #[test]
    fn allowances_and_sets_need_spans_too() {
        let req: Requirement = serde_json::from_value(json!({
            "ir_version": 1, "id": "r", "name": "n", "text": "crush 0.5 mm3 ribs", "claims": [],
            "sets": {"s": {"members": [door()["part"].clone()]}},
            "allow": [{"pair": [{"set": "s"}, {"set": "s"}], "kind": "interference", "reason": "x",
                       "max": {"value": 0.5, "unit": "mm3"}, "span": [0, 18]},
                      {"pair": [{"set": "s"}, {"set": "s"}], "kind": "interference", "reason": "x",
                       "max": {"value": 0.9, "unit": "mm3"}, "span": [0, 18]}]}))
        .unwrap();
        let snap = snapshot();
        let (b, _) = bind(&req, &snap);
        let (out, d) = check_provenance(&req, &b, &snap, &ProjectSettings::default());
        let at: Vec<_> = d.iter().map(|x| x.at.as_str()).collect();
        assert_eq!(at, ["allow[1].max", "sets.s.source"]);
        assert_eq!(out.allow[0].max.as_ref().unwrap().source, Source::Text);
    }
}
