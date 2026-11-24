//! Rule expansion (spec §2, "Expand"): a pure function from an IR claim to
//! primitive claims, each tagged with the rule and library version it came
//! from. No geometry and no binding: sets, globs and `others` stay symbolic
//! and are resolved later (link and plan). Defaults are resolved here, from
//! the library and the project settings, and carry source `library`.
use super::diagnostics::{Diagnostic, DiagnosticCode};
use super::ir::{
    Arg, AxisRef, Claim, Comparator, Literal, Quantifier, Requirement, Source, Span, StateSpace,
};
use super::library::{ArgKind, PrimitiveDef, at_spec, check_args, library, library_version};
use super::quantity::Unit;
use super::settings::ProjectSettings;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How a primitive's single-part arguments are filled from sets, once the sets
/// are resolved to members.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Pairing {
    /// The arguments are used as given (sets measured as a whole).
    Whole,
    /// Every pair `(a, b)` of A × B, each checked.
    EachPair,
    /// Every member of A, each checked.
    Each,
    /// Satisfied when at least one pair holds (`blocked`).
    AnyPair,
}

/// What an expanded claim came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    /// Id of the IR claim.
    pub claim: String,
    /// Rule name, or `measure` for a primitive written directly.
    pub rule: String,
    /// Library version the expansion used.
    pub library_version: String,
}

/// One primitive measurement compared against a limit, over a state space.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveClaim {
    /// `<claim id>.<n>`, n counted in expansion order.
    pub id: String,
    pub origin: Origin,
    pub primitive: String,
    /// Primitive arguments, still symbolic.
    pub args: BTreeMap<String, Arg>,
    pub pairing: Pairing,
    pub states: StateSpace,
    pub quantifier: Quantifier,
    pub cmp: Comparator,
    /// Compare strictly (`>` rather than `>=`); used by `blocked`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
    /// Provenance of the claim this came from (inferred claims stay inferred).
    #[serde(default, skip_serializing_if = "is_text")]
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    /// What this piece checks, when the rule emits several (`x`, `y`, `z`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn is_text(s: &Source) -> bool {
    *s == Source::Text
}

struct Emit<'a> {
    claim: &'a Claim,
    settings: &'a ProjectSettings,
    out: Vec<PrimitiveClaim>,
}

impl Emit<'_> {
    fn eps(&self) -> Literal {
        Literal {
            value: self.settings.overlap_tolerance.value,
            unit: self.settings.overlap_tolerance.unit,
            source: Source::Library,
            note: Some("overlap tolerance ε (project setting)".into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        primitive: &str,
        args: Vec<(&str, Arg)>,
        pairing: Option<Pairing>,
        states: StateSpace,
        cmp: Comparator,
        strict: bool,
        note: Option<String>,
    ) {
        let def = library()
            .primitive(primitive)
            .expect("rule emits a library primitive");
        let n = self.out.len() + 1;
        self.out.push(PrimitiveClaim {
            id: format!("{}.{n}", self.claim.id),
            origin: Origin {
                claim: self.claim.id.clone(),
                rule: self.claim.rule.clone(),
                library_version: library_version().to_string(),
            },
            primitive: primitive.to_string(),
            args: args.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            pairing: pairing.unwrap_or_else(|| pairing_of(def)),
            states,
            quantifier: self.claim.quantifier,
            cmp,
            strict,
            source: self.claim.source,
            span: self.claim.span,
            note,
        });
    }
}

/// Pairing implied by a primitive's signature: one single-part argument means
/// "each", two mean "each pair", none means the sets are measured whole.
fn pairing_of(def: &PrimitiveDef) -> Pairing {
    match def
        .args
        .iter()
        .filter(|a| a.kind == ArgKind::Parts { single: true })
        .count()
    {
        0 => Pairing::Whole,
        1 => Pairing::Each,
        _ => Pairing::EachPair,
    }
}

/// States of a `States` argument: one space, or a union (list) of spaces.
fn states_of(arg: &Arg) -> Vec<StateSpace> {
    match arg {
        Arg::States(s) => vec![(**s).clone()],
        Arg::List(items) => items.iter().flat_map(states_of).collect(),
        _ => vec![],
    }
}

fn arg<'a>(claim: &'a Claim, name: &str) -> &'a Arg {
    &claim.args[name]
}

fn states_or_rest(claim: &Claim, name: &str) -> Vec<StateSpace> {
    claim
        .args
        .get(name)
        .map(states_of)
        .unwrap_or_else(|| vec![StateSpace::Rest {}])
}

fn expand_rule(e: &mut Emit) -> Result<(), Vec<Diagnostic>> {
    let claim = e.claim;
    let eps = e.eps();
    let le_eps = Comparator::Le { value: eps.clone() };
    match claim.rule.as_str() {
        "no_overlap" => {
            for s in states_or_rest(claim, "at") {
                e.push(
                    "interference",
                    vec![
                        ("a", arg(claim, "a").clone()),
                        ("b", arg(claim, "b").clone()),
                    ],
                    None,
                    s,
                    le_eps.clone(),
                    false,
                    None,
                );
            }
        }
        "moves_freely" => {
            let states = states_or_rest(claim, "over");
            for s in &states {
                e.push(
                    "interference",
                    vec![
                        ("a", arg(claim, "moving").clone()),
                        ("b", arg(claim, "against").clone()),
                    ],
                    None,
                    s.clone(),
                    le_eps.clone(),
                    false,
                    None,
                );
            }
            if let Some(Arg::Compare(c)) = claim.args.get("clearance") {
                for s in states {
                    e.push(
                        "min_clearance",
                        vec![
                            ("a", arg(claim, "moving").clone()),
                            ("b", arg(claim, "against").clone()),
                        ],
                        None,
                        s,
                        c.clone(),
                        false,
                        Some("clearance".into()),
                    );
                }
            }
        }
        "blocked" => {
            for s in states_or_rest(claim, "at") {
                e.push(
                    "interference",
                    vec![
                        ("a", arg(claim, "moving").clone()),
                        ("b", arg(claim, "by").clone()),
                    ],
                    Some(Pairing::AnyPair),
                    s,
                    Comparator::Ge { value: eps.clone() },
                    true,
                    None,
                );
            }
        }
        "inside" => {
            for s in states_or_rest(claim, "at") {
                e.push(
                    "outside_volume",
                    vec![
                        ("a", arg(claim, "a").clone()),
                        ("envelope", arg(claim, "envelope").clone()),
                    ],
                    None,
                    s,
                    le_eps.clone(),
                    false,
                    None,
                );
            }
        }
        "clear_of" => {
            for s in states_or_rest(claim, "at") {
                e.push(
                    "interference",
                    vec![
                        ("a", arg(claim, "a").clone()),
                        ("b", arg(claim, "envelope").clone()),
                    ],
                    None,
                    s,
                    le_eps.clone(),
                    false,
                    None,
                );
            }
        }
        "fits_bed" => {
            let bed = bed_limits(arg(claim, "bed"));
            for (axis, limit) in [
                (AxisRef::X, &bed[0]),
                (AxisRef::Y, &bed[1]),
                (AxisRef::Z, &bed[2]),
            ] {
                let name = match axis {
                    AxisRef::X => "x",
                    AxisRef::Y => "y",
                    _ => "z",
                };
                e.push(
                    "extent",
                    vec![("a", arg(claim, "a").clone()), ("axis", Arg::Axis(axis))],
                    None,
                    StateSpace::Rest {},
                    Comparator::Le {
                        value: limit.clone(),
                    },
                    false,
                    Some(name.into()),
                );
            }
        }
        "single_solid" => {
            let one = |note| Literal {
                value: 1.0,
                unit: Unit::One,
                source: Source::Library,
                note: Some(note),
            };
            e.push(
                "valid_solid_count",
                vec![("a", arg(claim, "a").clone())],
                None,
                StateSpace::Rest {},
                Comparator::In {
                    lo: one("exactly one solid".into()),
                    hi: one("exactly one solid".into()),
                },
                false,
                None,
            );
        }
        other => unreachable!("rule {other} is in the library but has no expansion"),
    }
    Ok(())
}

/// The three bed lengths, exactly as the claim states them (no presets: bed
/// size is a design value, never a library default).
fn bed_limits(bed: &Arg) -> Vec<Literal> {
    match bed {
        Arg::List(items) => items
            .iter()
            .filter_map(|i| {
                if let Arg::Quantity(l) = i {
                    Some(l.clone())
                } else {
                    None
                }
            })
            .collect(),
        _ => unreachable!("bed checked against the signature"),
    }
}

fn expand_measure(e: &mut Emit, at: &str) -> Result<(), Vec<Diagnostic>> {
    let claim = e.claim;
    let kind_err =
        |p: String, m: String| Err(vec![Diagnostic::new(DiagnosticCode::ETypeKind, p, m)]);
    let name = match claim.args.get("fn") {
        Some(Arg::Text(n)) => n.clone(),
        _ => {
            return kind_err(
                format!("{at}.args.fn"),
                "measure needs a `fn` keyword naming a primitive".into(),
            );
        }
    };
    let Some(def) = library().primitive(&name) else {
        return kind_err(
            format!("{at}.args.fn"),
            format!("unknown primitive {name:?}"),
        );
    };
    let mut errors = check_args(&def.args, &claim.args, at, &["fn", "cmp", "at"], true);
    let cmp = match claim.args.get("cmp") {
        Some(Arg::Compare(c)) => {
            errors.extend(c.validate(&format!("{at}.args.cmp")));
            let want = def.result;
            match c.dimension() {
                Some(d) if d != want => errors.push(
                    super::quantity::DimensionMismatch {
                        expected: want,
                        found: d,
                    }
                    .diagnostic(format!("{at}.args.cmp")),
                ),
                _ => {}
            }
            Some(c.clone())
        }
        _ => {
            errors.push(Diagnostic::new(
                DiagnosticCode::ETypeKind,
                format!("{at}.args"),
                "measure needs a `cmp` comparison",
            ));
            None
        }
    };
    if let Some(a) = claim.args.get("at") {
        let at_spec = [at_spec()];
        let given = BTreeMap::from([("at".to_string(), a.clone())]);
        errors.extend(check_args(&at_spec, &given, at, &[], true));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let args: Vec<(&str, Arg)> = def
        .args
        .iter()
        .filter_map(|s| claim.args.get(s.name).map(|v| (s.name, v.clone())))
        .collect();
    for s in states_or_rest(claim, "at") {
        e.push(
            def.name,
            args.clone(),
            None,
            s,
            cmp.clone().expect("checked"),
            false,
            None,
        );
    }
    Ok(())
}

/// Expand one claim into primitive claims. `at` is the claim's JSON path
/// (`claims[0]`) for diagnostics. Errors are signature violations.
pub fn expand_claim(
    claim: &Claim,
    at: &str,
    settings: &ProjectSettings,
) -> Result<Vec<PrimitiveClaim>, Vec<Diagnostic>> {
    let mut emit = Emit {
        claim,
        settings,
        out: Vec::new(),
    };
    if claim.rule == "measure" {
        expand_measure(&mut emit, at)?;
    } else {
        let Some(rule) = library().rule(&claim.rule) else {
            return Err(vec![Diagnostic::new(
                DiagnosticCode::ETypeKind,
                format!("{at}.rule"),
                format!("unknown rule {:?}", claim.rule),
            )]);
        };
        if !rule.available {
            return Err(vec![Diagnostic::new(
                DiagnosticCode::ETypeKind,
                format!("{at}.rule"),
                format!(
                    "rule {:?} is not available in this library version yet",
                    rule.name
                ),
            )]);
        }
        let errors = check_args(&rule.args, &claim.args, at, &[], false);
        if !errors.is_empty() {
            return Err(errors);
        }
        expand_rule(&mut emit)?;
    }
    Ok(emit.out)
}

/// Signature check only: the diagnostics `expand_claim` would give.
pub fn check_claim(claim: &Claim, at: &str) -> Vec<Diagnostic> {
    expand_claim(claim, at, &ProjectSettings::default())
        .err()
        .unwrap_or_default()
}

/// Expand every claim of a requirement, in claim order. All signature errors
/// are collected.
pub fn expand_requirement(
    req: &Requirement,
    settings: &ProjectSettings,
) -> Result<Vec<PrimitiveClaim>, Vec<Diagnostic>> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (i, claim) in req.claims.iter().enumerate() {
        match expand_claim(claim, &format!("claims[{i}]"), settings) {
            Ok(mut v) => out.append(&mut v),
            Err(mut e) => errors.append(&mut e),
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn claim(v: serde_json::Value) -> Claim {
        serde_json::from_value(v).unwrap()
    }
    fn part(n: &str) -> serde_json::Value {
        json!({"part": {"model": {"entry": "m.py", "builder": "m"}, "part": {"name": n}}})
    }
    fn expand(v: serde_json::Value) -> Result<Vec<PrimitiveClaim>, Vec<Diagnostic>> {
        expand_claim(&claim(v), "claims[0]", &ProjectSettings::default())
    }

    #[test]
    fn no_overlap_defaults_to_rest_and_epsilon() {
        let out = expand(
            json!({"id": "c1", "rule": "no_overlap", "args": {"a": part("a"), "b": part("b")}}),
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        let p = &out[0];
        assert_eq!(p.id, "c1.1");
        assert_eq!(p.primitive, "interference");
        assert_eq!(p.pairing, Pairing::EachPair);
        assert_eq!(p.states, StateSpace::Rest {});
        assert_eq!(p.origin.library_version, library_version());
        let Comparator::Le { value } = &p.cmp else {
            panic!()
        };
        assert_eq!(
            (value.value, value.unit, value.source),
            (1e-3, Unit::Mm3, Source::Library)
        );
    }

    #[test]
    fn epsilon_follows_project_settings() {
        let mut s = ProjectSettings::default();
        s.overlap_tolerance.value = 1e-3;
        let c = claim(
            json!({"id": "c", "rule": "clear_of", "args": {"a": part("a"), "envelope": part("e")}}),
        );
        let out = expand_claim(&c, "claims[0]", &s).unwrap();
        assert!(matches!(&out[0].cmp, Comparator::Le { value } if value.value == 1e-3));
    }

    #[test]
    fn moves_freely_adds_clearance_and_unions_states() {
        let rest = json!({"states": {"kind": "rest"}});
        let out = expand(json!({"id": "c1", "rule": "moves_freely", "args": {
            "moving": part("a"), "against": "others",
            "over": {"list": [rest, rest]},
            "clearance": {"compare": {"op": ">=", "value": {"value": 0.5, "unit": "mm"}}}}}))
        .unwrap();
        let prims: Vec<_> = out.iter().map(|p| p.primitive.as_str()).collect();
        assert_eq!(
            prims,
            [
                "interference",
                "interference",
                "min_clearance",
                "min_clearance"
            ]
        );
        assert_eq!(out[2].pairing, Pairing::Whole);
        assert_eq!(out[3].note.as_deref(), Some("clearance"));
        assert_eq!(out[0].args["b"], Arg::Others);
    }

    #[test]
    fn blocked_is_strict_and_any_pair() {
        let out = expand(
            json!({"id": "c", "rule": "blocked", "source": "inferred", "span": [0, 4], "args": {
            "moving": part("a"), "by": part("b"), "at": {"states": {"kind": "rest"}}}}),
        )
        .unwrap();
        assert!(out[0].strict);
        assert_eq!(out[0].pairing, Pairing::AnyPair);
        assert!(matches!(out[0].cmp, Comparator::Ge { .. }));
        assert_eq!(out[0].source, Source::Inferred);
        assert_eq!(out[0].span, Some([0, 4]));
    }

    #[test]
    fn fits_bed_uses_the_stated_values_only() {
        let q = |v: f64| json!({"quantity": {"value": v, "unit": "mm"}});
        let orient = json!({"token": {"text": "as_modelled"}});
        let out = expand(json!({"id": "c", "rule": "fits_bed", "args": {
            "a": part("a"), "bed": {"list": [q(250.0), q(210.0), q(200.0)]}, "orientation": orient}})).unwrap();
        let limits: Vec<f64> = out
            .iter()
            .map(|p| match &p.cmp {
                Comparator::Le { value } => value.value,
                _ => 0.0,
            })
            .collect();
        assert_eq!(limits, [250.0, 210.0, 200.0]);
        assert_eq!(out[1].args["axis"], Arg::Axis(AxisRef::Y));
        // A preset name is not a bed: sizes are never looked up.
        let err = expand(json!({"id": "c", "rule": "fits_bed", "args": {
            "a": part("a"), "bed": {"token": {"text": "mk4"}}, "orientation": orient}}))
        .unwrap_err();
        assert_eq!(err[0].at, "claims[0].args.bed");
    }

    #[test]
    fn single_solid_expects_exactly_one() {
        let out =
            expand(json!({"id": "c", "rule": "single_solid", "args": {"a": part("a")}})).unwrap();
        assert_eq!(out[0].primitive, "valid_solid_count");
        assert_eq!(out[0].pairing, Pairing::Each);
        assert!(
            matches!(&out[0].cmp, Comparator::In { lo, hi } if lo.value == 1.0 && hi.value == 1.0)
        );
    }

    #[test]
    fn signature_violations_are_diagnosed() {
        let kinds = |v: serde_json::Value| -> Vec<(DiagnosticCode, String)> {
            expand(v)
                .unwrap_err()
                .into_iter()
                .map(|d| (d.code, d.at))
                .collect()
        };
        assert_eq!(
            kinds(json!({"id": "c", "rule": "nope"})),
            [(DiagnosticCode::ETypeKind, "claims[0].rule".into())]
        );
        assert_eq!(
            kinds(json!({"id": "c", "rule": "no_overlap", "args": {"a": part("a")}})),
            [(DiagnosticCode::ETypeKind, "claims[0].args".into())]
        );
        assert_eq!(
            kinds(
                json!({"id": "c", "rule": "no_overlap", "args": {"a": part("a"), "b": part("b"), "zzz": {"bool": true}}})
            ),
            [(DiagnosticCode::ETypeKind, "claims[0].args.zzz".into())]
        );
        // clearance in degrees: wrong dimension
        assert_eq!(
            kinds(
                json!({"id": "c", "rule": "moves_freely", "args": {"moving": part("a"), "against": "others",
                "over": {"states": {"kind": "rest"}},
                "clearance": {"compare": {"op": ">=", "value": {"value": 1.0, "unit": "deg"}}}}})
            ),
            [(DiagnosticCode::ETypeUnit, "claims[0].args.clearance".into())]
        );
        // bad orientation word
        assert_eq!(
            kinds(
                json!({"id": "c", "rule": "fits_bed", "args": {"a": part("a"),
                "bed": {"list": [{"quantity": {"value": 1.0, "unit": "mm"}}]},
                "orientation": {"token": {"text": "upright"}}}})
            )
            .len(),
            2
        );
    }

    #[test]
    fn measure_passes_a_primitive_through() {
        let out = expand(json!({"id": "c", "rule": "measure", "args": {
            "fn": {"text": "min_clearance"}, "a": part("a"), "b": part("b"),
            "cmp": {"compare": {"op": ">=", "value": {"value": 2.0, "unit": "mm"}}}}}))
        .unwrap();
        assert_eq!(out[0].primitive, "min_clearance");
        assert_eq!(out[0].origin.rule, "measure");
        let err = expand(json!({"id": "c", "rule": "measure", "args": {"fn": {"text": "gap"}}}))
            .unwrap_err();
        assert_eq!(err[0].at, "claims[0].args.fn");
        let err = expand(json!({"id": "c", "rule": "measure", "args": {
            "fn": {"text": "min_clearance"}, "a": part("a"), "b": part("b"),
            "cmp": {"compare": {"op": ">=", "value": {"value": 2.0, "unit": "deg"}}}}}))
        .unwrap_err();
        assert_eq!(err[0].code, DiagnosticCode::ETypeUnit);
    }
}
