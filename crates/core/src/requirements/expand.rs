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
        let quantifier = match &states {
            StateSpace::Simulate {
                when: super::ir::When::Always {},
                ..
            } => Quantifier::ForAll,
            StateSpace::Simulate {
                when: super::ir::When::Eventually {},
                ..
            } => Quantifier::Exists,
            _ => self.claim.quantifier,
        };
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
            quantifier,
            cmp,
            strict,
            source: self.claim.source,
            span: self.claim.span,
            note,
        });
    }
}

/// Pairing implied by a primitive's signature: `single` part or entity
/// arguments mean "each" (one) or "each pair" (two); set arguments are
/// measured whole.
///
/// `clearance_along` is the exception: it measures one side against every
/// entity of the other and reports the tightest, so a multi-face feature
/// selection stays one pairwise argument. Without this, allowing sets on both
/// sides would downgrade it to `Each` and drop the second side from the job.
fn pairing_of(def: &PrimitiveDef) -> Pairing {
    if def.name == "clearance_along" {
        return Pairing::EachPair;
    }
    match def
        .args
        .iter()
        .filter(|a| {
            matches!(
                a.kind,
                ArgKind::Parts { single: true } | ArgKind::Entities { single: true }
            )
        })
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

fn expand_rule(e: &mut Emit, at: &str) -> Result<(), Vec<Diagnostic>> {
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
        "aligned" => {
            // The profile normal is `plane` when given, else B's own axis or
            // plane normal (a feature face/edge axis, resolved by the worker).
            let axis = claim
                .args
                .get("plane")
                .cloned()
                .unwrap_or_else(|| arg(claim, "b").clone());
            let (Arg::Quantity(tol), Arg::Quantity(margin)) =
                (arg(claim, "tol"), arg(claim, "margin"))
            else {
                unreachable!("tol/margin checked against the signature");
            };
            for s in states_or_rest(claim, "at") {
                e.push(
                    "profile_centre",
                    vec![
                        ("a", arg(claim, "a").clone()),
                        ("b", arg(claim, "b").clone()),
                        ("axis", axis.clone()),
                    ],
                    None,
                    s.clone(),
                    Comparator::Le { value: tol.clone() },
                    false,
                    Some("centre".into()),
                );
                e.push(
                    "profile_size",
                    vec![
                        ("a", arg(claim, "a").clone()),
                        ("b", arg(claim, "b").clone()),
                        ("axis", axis.clone()),
                    ],
                    None,
                    s,
                    Comparator::Ge {
                        value: margin.clone(),
                    },
                    false,
                    Some("opening margin".into()),
                );
            }
        }
        "press_fit" => {
            let Some(Arg::Compare(range)) = claim.args.get("range") else {
                unreachable!("range checked against the signature");
            };
            // Coaxial: the axis offset must stay within half the smallest
            // guaranteed radial engagement of the fit, i.e. half the smallest
            // |diametral difference| the range allows. A range that reaches
            // zero gives no scale for "coaxial" and is rejected.
            let tol = coaxial_tolerance(range).ok_or_else(|| {
                vec![Diagnostic::new(
                    DiagnosticCode::ETypeKind,
                    format!("{at}.args.range"),
                    "press_fit needs a range bounded away from zero: the coaxial \
                     check is scaled by the guaranteed diametral difference",
                )]
            })?;
            for s in states_or_rest(claim, "at") {
                e.push(
                    "diameter_difference",
                    vec![
                        ("a", arg(claim, "pin").clone()),
                        ("b", arg(claim, "hole").clone()),
                    ],
                    None,
                    s.clone(),
                    range.clone(),
                    false,
                    Some("diameter difference".into()),
                );
                e.push(
                    "axis_offset",
                    vec![
                        ("a", arg(claim, "pin").clone()),
                        ("b", arg(claim, "hole").clone()),
                    ],
                    None,
                    s,
                    Comparator::Le { value: tol.clone() },
                    false,
                    Some("coaxial".into()),
                );
            }
        }
        "fit" => {
            let class_text = match arg(claim, "class") {
                Arg::Token(t) => t.text.clone(),
                _ => unreachable!("class checked against the signature"),
            };
            let nominal_mm = match arg(claim, "nominal") {
                Arg::Quantity(l) => l
                    .quantity()
                    .expect(super::quantity::Dimension::Length)
                    .map_err(|e| vec![e.diagnostic(format!("{at}.args.nominal"))])?,
                Arg::Token(t) => super::fasteners::parse_size_label(&t.text).ok_or_else(|| {
                    vec![Diagnostic::new(
                        DiagnosticCode::ETypeKind,
                        format!("{at}.args.nominal"),
                        format!(
                            "nominal must be a length or a fastener size like M8, not {:?}",
                            t.text
                        ),
                    )]
                })?,
                _ => unreachable!("nominal checked against the signature"),
            };
            let Some(fit) = super::iso286::parse_fit_class(&class_text) else {
                return Err(vec![Diagnostic::new(
                    DiagnosticCode::ETypeKind,
                    format!("{at}.args.class"),
                    format!("{class_text:?} is not an ISO 286 tolerance class"),
                )]);
            };
            let Some(dev) = super::iso286::deviation_um(fit, nominal_mm) else {
                return Err(vec![Diagnostic::new(
                    DiagnosticCode::ETypeKind,
                    format!("{at}.args.class"),
                    format!(
                        "{class_text} is not tabulated at Ø{nominal_mm} mm (ISO 286 covers \
                         holes H and shafts f/g/h/js/k/n/p/s at IT5–IT12, 0 < d ≤ 50 mm)"
                    ),
                )]);
            };
            let note = format!("ISO 286-2 {class_text} at Ø{nominal_mm} mm");
            let lit = |mm: f64| Literal {
                value: mm,
                unit: Unit::Mm,
                source: Source::Library,
                note: Some(note.clone()),
            };
            let cmp = Comparator::In {
                lo: lit(nominal_mm + dev.lower_um * 0.001),
                hi: lit(nominal_mm + dev.upper_um * 0.001),
            };
            for s in states_or_rest(claim, "at") {
                e.push(
                    "diameter",
                    vec![("a", arg(claim, "feature").clone())],
                    None,
                    s,
                    cmp.clone(),
                    false,
                    Some(note.clone()),
                );
            }
        }
        "seated" => {
            // No area given: any positive contact counts, so compare strictly.
            let (contact, strict) = match claim.args.get("area") {
                Some(Arg::Quantity(l)) => (Comparator::Ge { value: l.clone() }, false),
                _ => (
                    Comparator::Ge {
                        value: Literal {
                            value: 0.0,
                            unit: Unit::Mm2,
                            source: Source::Library,
                            note: Some("any contact area".into()),
                        },
                    },
                    true,
                ),
            };
            for s in states_or_rest(claim, "at") {
                e.push(
                    "interference",
                    vec![
                        ("a", arg(claim, "a").clone()),
                        ("b", arg(claim, "on").clone()),
                    ],
                    None,
                    s.clone(),
                    le_eps.clone(),
                    false,
                    Some("seated: no overlap".into()),
                );
                e.push(
                    "contact_area",
                    vec![
                        ("a", arg(claim, "a").clone()),
                        ("b", arg(claim, "on").clone()),
                    ],
                    // Whole: the summed common-face area of the sets, so one
                    // seated member is enough when `area` is omitted.
                    Some(Pairing::Whole),
                    s,
                    contact.clone(),
                    strict,
                    Some("seated: contact area".into()),
                );
            }
        }
        "bolted" => {
            // Expansion per spec §4.1, limited to what whole-part primitives
            // can measure (the holes themselves are not separate arguments):
            // - hole Ø: the consistent bore diameter of each bolted part;
            // - coaxial holes: enforced by the no-overlap checks — a straight
            //   fastener through misaligned holes intersects the walls;
            // - hardware overlaps no printed part: interference vs each side;
            // - fasteners bear on both parts (nut seated, shank spans):
            //   positive contact area vs each side;
            // - parts bear on each other: contact_area within `bearing`;
            // - nut engaged with thread_protrusion within `protrusion`.
            let cmp = |name: &str| match arg(claim, name) {
                Arg::Compare(c) => c.clone(),
                _ => unreachable!("{name} checked against the signature"),
            };
            let hole = cmp("hole");
            let protrusion = cmp("protrusion");
            let bearing = cmp("bearing");
            let any_contact = |note: &str| Literal {
                value: 0.0,
                unit: Unit::Mm2,
                source: Source::Library,
                note: Some(note.into()),
            };
            for side in ["p1", "p2"] {
                e.push(
                    "diameter",
                    vec![("a", arg(claim, side).clone())],
                    None,
                    StateSpace::Rest {},
                    hole.clone(),
                    false,
                    Some("hole diameter".into()),
                );
                e.push(
                    "interference",
                    vec![
                        ("a", arg(claim, "fasteners").clone()),
                        ("b", arg(claim, side).clone()),
                    ],
                    None,
                    StateSpace::Rest {},
                    le_eps.clone(),
                    false,
                    Some("hardware overlaps no printed part".into()),
                );
                e.push(
                    "contact_area",
                    vec![
                        ("a", arg(claim, "fasteners").clone()),
                        ("b", arg(claim, side).clone()),
                    ],
                    // Whole: total bearing area of the hardware on this side;
                    // per-pair would demand every nut touch both parts.
                    Some(Pairing::Whole),
                    StateSpace::Rest {},
                    Comparator::Ge {
                        value: any_contact("fastener bears on the part"),
                    },
                    true,
                    Some("fastener bears on the part".into()),
                );
            }
            e.push(
                "contact_area",
                vec![
                    ("a", arg(claim, "p1").clone()),
                    ("b", arg(claim, "p2").clone()),
                ],
                Some(Pairing::Whole),
                StateSpace::Rest {},
                bearing,
                false,
                Some("parts bear on each other".into()),
            );
            e.push(
                "thread_protrusion",
                vec![("a", arg(claim, "fasteners").clone())],
                None,
                StateSpace::Rest {},
                protrusion,
                false,
                Some("thread engagement".into()),
            );
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

/// Half the smallest |diametral difference| `range` guarantees, in mm — the
/// coaxial tolerance for `press_fit` (a press fit needs the axes aligned to
/// better than half the guaranteed radial engagement). `None` when the range
/// permits a zero difference: there is then no engagement to scale by.
fn coaxial_tolerance(range: &Comparator) -> Option<Literal> {
    use super::quantity::Dimension;
    let mm = |l: &Literal| l.quantity().expect(Dimension::Length).ok();
    // The allowed interval [lo, hi]; an open end is unbounded.
    let (lo, hi) = match range {
        Comparator::Ge { value } => (mm(value)?, f64::INFINITY),
        Comparator::Le { value } => (f64::NEG_INFINITY, mm(value)?),
        Comparator::In { lo, hi } => (mm(lo)?, mm(hi)?),
        Comparator::Approx { target, tol } => {
            let (t, d) = (mm(target)?, mm(tol)?);
            (t - d, t + d)
        }
        Comparator::IsTrue {} | Comparator::IsFalse {} => return None,
    };
    // Smallest |x| over the interval, or infinity when it contains no bound.
    let min_abs = if lo <= 0.0 && 0.0 <= hi {
        0.0
    } else if lo > 0.0 {
        lo
    } else {
        hi.abs()
    };
    if !(min_abs > 0.0) || !min_abs.is_finite() {
        return None;
    }
    Some(Literal {
        value: min_abs * 0.5,
        unit: Unit::Mm,
        source: Source::Library,
        note: Some("half the guaranteed diametral difference".into()),
    })
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
                Some(d)
                    if (d != want
                        && !(name == "joint" && d == super::quantity::Dimension::Angle)) =>
                {
                    errors.push(
                        super::quantity::DimensionMismatch {
                            expected: want,
                            found: d,
                        }
                        .diagnostic(format!("{at}.args.cmp")),
                    )
                }
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
        let pairing = library()
            .primitive(&def.name)
            .is_some_and(|d| d.accuracy == super::library::Accuracy::Proxy)
            .then_some(Pairing::Whole);
        e.push(
            def.name,
            args.clone(),
            pairing,
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
        expand_rule(&mut emit, at)?;
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
        let err =
            expand(json!({"id": "c", "rule": "measure", "args": {"fn": {"text": "no_such_fn"}}}))
                .unwrap_err();
        assert_eq!(err[0].at, "claims[0].args.fn");
        let err = expand(json!({"id": "c", "rule": "measure", "args": {
            "fn": {"text": "min_clearance"}, "a": part("a"), "b": part("b"),
            "cmp": {"compare": {"op": ">=", "value": {"value": 2.0, "unit": "deg"}}}}}))
        .unwrap_err();
        assert_eq!(err[0].code, DiagnosticCode::ETypeUnit);
    }

    #[test]
    fn measure_accepts_feature_entities_direction_words_and_feature_axes() {
        let feat = |n: &str| {
            json!({"feature": {"kind": "explicit",
                "part": {"model": {"entry": "m.py", "builder": "m"}, "part": {"name": "door"}},
                "name": n}})
        };
        let cmp = json!({"compare": {"op": ">=", "value": {"value": 8.0, "unit": "mm"}}});
        let out = expand(json!({"id": "c", "rule": "measure", "args": {
            "fn": {"text": "diameter"}, "a": feat("bore"), "cmp": cmp}}))
        .unwrap();
        assert_eq!(out[0].primitive, "diameter");
        assert!(matches!(out[0].args["a"], Arg::Feature(_)));
        let out = expand(json!({"id": "c", "rule": "measure", "args": {
            "fn": {"text": "clearance_along"}, "a": part("a"), "b": feat("walls"),
            "directions": {"list": [{"text": "+x"}, {"text": "-y"}]}, "cmp": cmp}}))
        .unwrap();
        assert_eq!(out[0].primitive, "clearance_along");
        // `along` takes a feature (its axis is measured), an axis or a word.
        let out = expand(json!({"id": "c", "rule": "measure", "args": {
            "fn": {"text": "overlap_depth"}, "a": feat("lip"), "b": feat("ledge"),
            "along": feat("axis"), "cmp": cmp}}))
        .unwrap();
        assert_eq!(out[0].primitive, "overlap_depth");
        assert!(matches!(out[0].args["along"], Arg::Feature(_)));
    }

    #[test]
    fn aligned_expands_to_profile_claims_on_bs_axis() {
        let q = |v: f64| json!({"quantity": {"value": v, "unit": "mm"}});
        let out = expand(json!({"id": "c", "rule": "aligned", "args": {
            "a": part("j1"), "b": part("cutout"),
            "tol": q(0.2), "margin": q(0.3)}}))
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].primitive, "profile_centre");
        assert_eq!(out[1].primitive, "profile_size");
        // No `plane`: the axis argument is B itself (its own axis/normal).
        assert!(matches!(out[0].args["axis"], Arg::Part(_)));
        assert_eq!(
            out[0].args["axis"],
            out[0].args["b"].clone(),
            "axis defaults to the b argument"
        );
        assert!(matches!(&out[0].cmp, Comparator::Le { value } if value.value == 0.2));
        assert!(matches!(&out[1].cmp, Comparator::Ge { value } if value.value == 0.3));
        // An explicit plane wins over the default.
        let out = expand(json!({"id": "c", "rule": "aligned", "args": {
            "a": part("j1"), "b": part("cutout"), "plane": {"axis": "z"},
            "tol": q(0.2), "margin": q(0.3)}}))
        .unwrap();
        assert_eq!(out[0].args["axis"], Arg::Axis(AxisRef::Z));
    }

    #[test]
    fn press_fit_scales_the_coaxial_check_to_the_range() {
        let cmp = |lo: f64, hi: f64| {
            json!({"compare": {"op": "in", "lo": {"value": lo, "unit": "mm"},
                "hi": {"value": hi, "unit": "mm"}}})
        };
        let out = expand(json!({"id": "c", "rule": "press_fit", "args": {
            "pin": part("pin"), "hole": part("bore"), "range": cmp(0.02, 0.05)}}))
        .unwrap();
        assert_eq!(out[0].primitive, "diameter_difference");
        assert_eq!(out[1].primitive, "axis_offset");
        // Half the smallest guaranteed |difference|: min(|0.02|,|0.05|)/2.
        assert!(matches!(&out[1].cmp, Comparator::Le { value } if value.value == 0.01));
        // A range reaching zero gives no engagement scale: signature-level error.
        let err = expand(json!({"id": "c", "rule": "press_fit", "args": {
            "pin": part("pin"), "hole": part("bore"), "range": cmp(-0.02, 0.05)}}))
        .unwrap_err();
        assert_eq!(err[0].at, "claims[0].args.range");
    }

    #[test]
    fn fit_expands_to_a_diameter_in_the_iso286_zone() {
        let out = expand(json!({"id": "c", "rule": "fit", "args": {
            "feature": {"feature": {"kind": "explicit",
                "part": {"model": {"entry": "h.py", "builder": "h"}, "part": {"name": "housing"}},
                "name": "shaft_bore"}},
            "class": {"token": {"text": "H7"}},
            "nominal": {"quantity": {"value": 8.0, "unit": "mm"}}}}))
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].primitive, "diameter");
        // H7 at Ø8 mm: +0/+15 µm -> [8.000, 8.015].
        let Comparator::In { lo, hi } = &out[0].cmp else {
            panic!()
        };
        assert!((lo.value - 8.0).abs() < 1e-9 && (hi.value - 8.015).abs() < 1e-9);
        let err = expand(json!({"id": "c", "rule": "fit", "args": {
            "feature": part("bore"), "class": {"token": {"text": "ZZ9"}},
            "nominal": {"quantity": {"value": 8.0, "unit": "mm"}}}}))
        .unwrap_err();
        assert_eq!(err[0].at, "claims[0].args.class");
    }

    #[test]
    fn seated_and_bolted_emit_their_documented_pieces() {
        let q = |v: f64, u: &str| json!({"quantity": {"value": v, "unit": u}});
        let out = expand(json!({"id": "c", "rule": "seated", "args": {
            "a": part("lid"), "on": part("base")}}))
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].primitive, "interference");
        assert_eq!(out[1].primitive, "contact_area");
        assert_eq!(out[1].pairing, Pairing::Whole);
        assert!(out[1].strict, "no area: any positive contact counts");
        let out = expand(json!({"id": "c", "rule": "seated", "args": {
            "a": part("lid"), "on": part("base"), "area": q(100.0, "mm2")}}))
        .unwrap();
        assert!(!out[1].strict);
        assert!(matches!(&out[1].cmp, Comparator::Ge { value } if value.value == 100.0));

        let cmp = |op: &str, v: f64, u: &str| json!({"compare": {"op": op, "value": {"value": v, "unit": u}}});
        let out = expand(json!({"id": "c", "rule": "bolted", "args": {
            "p1": part("slab"), "p2": part("cover"), "fasteners": part("bolt_1"),
            "size": {"token": {"text": "M8"}},
            "hole": {"compare": {"op": "in",
                "lo": {"value": 8.4, "unit": "mm"}, "hi": {"value": 9.0, "unit": "mm"}}},
            "protrusion": cmp(">=", 1.0, "mm"), "bearing": cmp(">=", 100.0, "mm2")}}))
        .unwrap();
        let prims: Vec<_> = out.iter().map(|p| p.primitive.as_str()).collect();
        assert_eq!(
            prims,
            [
                "diameter",
                "interference",
                "contact_area",
                "diameter",
                "interference",
                "contact_area",
                "contact_area",
                "thread_protrusion",
            ]
        );
        assert!(
            out.iter()
                .filter(|p| p.primitive == "contact_area")
                .all(|p| p.pairing == Pairing::Whole)
        );
    }
}
