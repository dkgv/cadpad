//! The requirement IR (spec §3). JSON is the storage and tool format: agents
//! emit it against the schema generated from these types. Every type denies
//! unknown fields so a drifting front end fails loudly instead of silently
//! losing meaning.
use super::IR_VERSION;
use super::diagnostics::{Diagnostic, DiagnosticCode};
use super::quantity::{Dimension, Quantity, Unit};
use crate::portable;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Character range `[start, end)` of the requirement `text` (Unicode scalar
/// values, not bytes).
pub type Span = [usize; 2];

/// Where a value, claim, allowance or set membership comes from (spec §3.6).
/// Checked mechanically; anything not from the text, model or library must be
/// `inferred`, which is marked in the review and confirmed by approving it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Appears in the claim's `span` of the user's prose (numbers after unit
    /// normalisation).
    #[default]
    Text,
    /// Read from the model: a parameter's bounds or default, a constant.
    Model,
    /// A versioned library rule default, standards table row or preset.
    Library,
    /// The LLM judged it implied, or a number with no other source.
    Inferred,
}

impl Source {
    fn is_default(&self) -> bool {
        *self == Source::Text
    }
}

/// A number with a unit and a provenance. Written in JSON as
/// `{"value": 0.2, "unit": "mm"}`; `source` defaults to `text`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Literal {
    pub value: f64,
    pub unit: Unit,
    #[serde(default, skip_serializing_if = "Source::is_default")]
    pub source: Source,
    /// Why this value, for `inferred` and `library` sources (shown in review).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Literal {
    pub fn new(value: f64, unit: Unit) -> Self {
        Self {
            value,
            unit,
            source: Source::Text,
            note: None,
        }
    }
    pub fn quantity(&self) -> Quantity {
        Quantity::new(self.value, self.unit)
    }
}

impl From<Quantity> for Literal {
    fn from(q: Quantity) -> Self {
        Literal::new(q.value, q.unit)
    }
}

// ---------------------------------------------------------------- references

/// A model in the project's source catalog: source file plus builder ID.
/// `@cabinet`.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    pub entry: String,
    pub builder: String,
}

/// Which published part(s) of a model.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PartName {
    /// Exact published name, `show(..., name="door")`; imported STEP sub-parts
    /// by their STEP names (`pcb/J1`).
    Name(String),
    /// Glob over published names (`tower_*`); matching nothing is an error.
    Glob(String),
}

/// A published part, or a glob of parts, of a model: `@cabinet/door`,
/// `@cabinet/shelf_*`.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct PartRef {
    pub model: ModelRef,
    pub part: PartName,
}

impl PartRef {
    pub fn is_glob(&self) -> bool {
        matches!(self.part, PartName::Glob(_))
    }
}

/// A named set from the requirement's `sets` table (`let turntable = ...`).
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct SetRef(pub String);

/// Some parts: one part or glob, a named set, or an inline list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Part(PartRef),
    Set(SetRef),
    Parts(Vec<PartRef>),
}

/// A model parameter, `parameter("door_angle", ...)`. `model` may be omitted
/// when the claim's subject is unambiguous.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ParamRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    pub name: String,
}

/// A mechanism joint, where the model defines one: `@cabinet:hinge`.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct JointRef {
    pub model: ModelRef,
    pub name: String,
}

/// Kind of a geometric entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Face,
    Edge,
    Vertex,
}

/// A feature (face/edge, or a set of them) of a published part:
/// `@cabinet/door#hinge_hole`. Resolution order on each build: explicit name,
/// then history name, then exact geometric reference. Each resolves exactly or
/// fails; there is no closest match.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FeatureRef {
    /// Published by the source: `show(..., features={"hinge_hole": ...})`.
    Explicit { part: PartRef, name: String },
    /// History name: the operation that created the entity, e.g.
    /// `make_door/hole#1/wall`.
    History { part: PartRef, name: String },
    /// Exact geometric reference for imported geometry: SHA-256 of the
    /// entity's geometry. Never matched approximately.
    Geometric {
        part: PartRef,
        entity: EntityKind,
        hash: String,
    },
}

/// Axis or direction of a rigid state space.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AxisRef {
    X,
    Y,
    Z,
    /// World-space vector (need not be normalised, must be non-zero).
    Vector([f64; 3]),
    /// Cylinder axis, line edge or plane normal of a feature.
    Feature(FeatureRef),
}

// -------------------------------------------------------------- state spaces

/// `from .. to` sampled every `step`; `to` is always included.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sweep {
    pub from: Literal,
    pub to: Literal,
    pub step: Literal,
}

/// Values a parameter takes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParamValues {
    /// `param(p, v)`.
    Value { value: Literal },
    /// `param(p, a .. b, step s)`.
    Range {
        from: Literal,
        to: Literal,
        step: Literal,
    },
    /// `param(p, each of [v1, v2, ...])`.
    List { values: Vec<Literal> },
}

/// Where the model is when we measure; a claim holds at every state of its
/// space (spec §3.3). Sampling is always explicit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StateSpace {
    /// As built with default parameters.
    Rest {},
    /// Rebuild per value.
    Param {
        param: ParamRef,
        values: ParamValues,
    },
    /// Rigid rotation of a set about an axis, no rebuild.
    Rotate {
        moving: Target,
        axis: AxisRef,
        range: Sweep,
    },
    /// Rigid translation of a set along a direction, no rebuild.
    Translate {
        moving: Target,
        direction: AxisRef,
        range: Sweep,
    },
    /// Mechanism joint coordinate.
    Joint { joint: JointRef, range: Sweep },
    /// Every combination of the factors.
    Product { factors: Vec<StateSpace> },
}

fn sweep_count(from: &Literal, to: &Literal, step: &Literal) -> Option<u64> {
    let dim = from.unit.dimension();
    if to.unit.dimension() != dim || step.unit.dimension() != dim {
        return None;
    }
    let (a, b, s) = (
        from.quantity().internal_value(),
        to.quantity().internal_value(),
        step.quantity().internal_value(),
    );
    if !(a.is_finite() && b.is_finite() && s.is_finite()) || s <= 0.0 || b < a {
        return None;
    }
    let steps = (b - a) / s;
    if steps > 1e9 {
        return Some(u64::MAX);
    }
    let whole = (steps + 1e-9).floor();
    let mut count = whole as u64 + 1;
    if (steps - whole).abs() > 1e-9 {
        count += 1; // `to` is always included
    }
    Some(count)
}

impl StateSpace {
    /// Number of states, or `None` if the space is malformed (see
    /// [`StateSpace::validate`]). Saturates instead of overflowing.
    pub fn state_count(&self) -> Option<u64> {
        match self {
            StateSpace::Rest {} => Some(1),
            StateSpace::Param { values, .. } => match values {
                ParamValues::Value { .. } => Some(1),
                ParamValues::Range { from, to, step } => sweep_count(from, to, step),
                ParamValues::List { values } => Some(values.len() as u64),
            },
            StateSpace::Rotate { range, .. }
            | StateSpace::Translate { range, .. }
            | StateSpace::Joint { range, .. } => sweep_count(&range.from, &range.to, &range.step),
            StateSpace::Product { factors } => factors
                .iter()
                .try_fold(1u64, |n, f| f.state_count().map(|c| n.saturating_mul(c))),
        }
    }

    /// Shape checks that need no model: finite numbers, consistent units,
    /// `step > 0`, `from <= to`, non-empty lists, non-zero axis vectors.
    /// The `MAX_STATES` limit and parameter bounds are checked later, with the
    /// settings and the parameter registry.
    pub fn validate(&self, at: &str) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        match self {
            StateSpace::Rest {} => {}
            StateSpace::Param { values, .. } => match values {
                ParamValues::Value { value } => {
                    check_finite(value, &format!("{at}.values.value"), &mut out)
                }
                ParamValues::Range { from, to, step } => {
                    check_sweep(from, to, step, &format!("{at}.values"), &mut out)
                }
                ParamValues::List { values } => {
                    if values.is_empty() {
                        out.push(Diagnostic::new(
                            DiagnosticCode::ETypeRange,
                            format!("{at}.values.values"),
                            "a value list needs at least one value",
                        ));
                    }
                    let dim = values.first().map(|v| v.unit.dimension());
                    for (i, v) in values.iter().enumerate() {
                        let p = format!("{at}.values.values[{i}]");
                        check_finite(v, &p, &mut out);
                        if Some(v.unit.dimension()) != dim {
                            out.push(Diagnostic::new(
                                DiagnosticCode::ETypeUnit,
                                p,
                                "list values must share one dimension",
                            ));
                        }
                    }
                }
            },
            StateSpace::Rotate { axis, range, .. } => {
                check_sweep(
                    &range.from,
                    &range.to,
                    &range.step,
                    &format!("{at}.range"),
                    &mut out,
                );
                check_dimension(
                    &range.from,
                    Dimension::Angle,
                    &format!("{at}.range.from"),
                    &mut out,
                );
                check_axis(axis, &format!("{at}.axis"), &mut out);
            }
            StateSpace::Translate {
                direction, range, ..
            } => {
                check_sweep(
                    &range.from,
                    &range.to,
                    &range.step,
                    &format!("{at}.range"),
                    &mut out,
                );
                check_dimension(
                    &range.from,
                    Dimension::Length,
                    &format!("{at}.range.from"),
                    &mut out,
                );
                check_axis(direction, &format!("{at}.direction"), &mut out);
            }
            StateSpace::Joint { range, .. } => {
                check_sweep(
                    &range.from,
                    &range.to,
                    &range.step,
                    &format!("{at}.range"),
                    &mut out,
                );
            }
            StateSpace::Product { factors } => {
                if factors.is_empty() {
                    out.push(Diagnostic::new(
                        DiagnosticCode::ETypeRange,
                        format!("{at}.factors"),
                        "a product needs at least one factor",
                    ));
                }
                for (i, f) in factors.iter().enumerate() {
                    out.extend(f.validate(&format!("{at}.factors[{i}]")));
                }
            }
        }
        out
    }
}

fn check_finite(lit: &Literal, at: &str, out: &mut Vec<Diagnostic>) {
    if !lit.value.is_finite() {
        out.push(Diagnostic::new(
            DiagnosticCode::ETypeRange,
            at,
            "value must be a finite number",
        ));
    }
}

fn check_dimension(lit: &Literal, expected: Dimension, at: &str, out: &mut Vec<Diagnostic>) {
    if let Err(e) = lit.quantity().expect(expected) {
        out.push(e.diagnostic(at));
    }
}

fn check_axis(axis: &AxisRef, at: &str, out: &mut Vec<Diagnostic>) {
    if let AxisRef::Vector(v) = axis
        && (!v.iter().all(|c| c.is_finite()) || v.iter().all(|c| *c == 0.0))
    {
        out.push(Diagnostic::new(
            DiagnosticCode::ETypeRange,
            at,
            "axis vector must be finite and non-zero",
        ));
    }
}

fn check_sweep(from: &Literal, to: &Literal, step: &Literal, at: &str, out: &mut Vec<Diagnostic>) {
    for (name, lit) in [("from", from), ("to", to), ("step", step)] {
        check_finite(lit, &format!("{at}.{name}"), out);
    }
    let dim = from.unit.dimension();
    for (name, lit) in [("to", to), ("step", step)] {
        if lit.unit.dimension() != dim {
            out.push(Diagnostic::new(
                DiagnosticCode::ETypeUnit,
                format!("{at}.{name}"),
                format!(
                    "expected a {} but found a {}",
                    dim.name(),
                    lit.unit.dimension().name()
                ),
            ));
        }
    }
    if out.iter().any(|d| d.at.starts_with(at)) {
        return;
    }
    if step.quantity().internal_value() <= 0.0 {
        out.push(Diagnostic::new(
            DiagnosticCode::ETypeRange,
            format!("{at}.step"),
            "step must be greater than zero",
        ));
    }
    if to.quantity().internal_value() < from.quantity().internal_value() {
        out.push(Diagnostic::new(
            DiagnosticCode::ETypeRange,
            at,
            "range start is greater than its end",
        ));
    }
}

// ------------------------------------------------------ comparators, quantifiers

/// How a measured value is compared (spec §3.4). No bare `==` on quantities.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum Comparator {
    #[serde(rename = ">=")]
    Ge { value: Literal },
    #[serde(rename = "<=")]
    Le { value: Literal },
    /// Inclusive range `[lo, hi]`.
    #[serde(rename = "in")]
    In { lo: Literal, hi: Literal },
    /// Within `tol` of `target`; `tol` must be greater than zero.
    #[serde(rename = "approx")]
    Approx { target: Literal, tol: Literal },
    #[serde(rename = "is_true")]
    IsTrue {},
    #[serde(rename = "is_false")]
    IsFalse {},
}

impl Comparator {
    /// Dimension the compared quantity must have; `None` for boolean comparators.
    pub fn dimension(&self) -> Option<Dimension> {
        match self {
            Comparator::Ge { value } | Comparator::Le { value } => Some(value.unit.dimension()),
            Comparator::In { lo, .. } => Some(lo.unit.dimension()),
            Comparator::Approx { target, .. } => Some(target.unit.dimension()),
            Comparator::IsTrue {} | Comparator::IsFalse {} => None,
        }
    }

    /// `E_TYPE_RANGE` for lo > hi, tol <= 0 and non-finite numbers;
    /// `E_TYPE_UNIT` when the operands differ in dimension.
    pub fn validate(&self, at: &str) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        match self {
            Comparator::Ge { value } | Comparator::Le { value } => {
                check_finite(value, &format!("{at}.value"), &mut out)
            }
            Comparator::In { lo, hi } => {
                check_finite(lo, &format!("{at}.lo"), &mut out);
                check_finite(hi, &format!("{at}.hi"), &mut out);
                match lo.quantity().expect(hi.unit.dimension()) {
                    Err(e) => out.push(e.diagnostic(format!("{at}.hi"))),
                    Ok(l) if out.is_empty() && l > hi.quantity().internal_value() => {
                        out.push(Diagnostic::new(
                            DiagnosticCode::ETypeRange,
                            at,
                            format!(
                                "range lower bound {} is greater than upper bound {}",
                                lo.quantity(),
                                hi.quantity()
                            ),
                        ))
                    }
                    Ok(_) => {}
                }
            }
            Comparator::Approx { target, tol } => {
                check_finite(target, &format!("{at}.target"), &mut out);
                check_finite(tol, &format!("{at}.tol"), &mut out);
                if let Err(e) = tol.quantity().expect(target.unit.dimension()) {
                    out.push(e.diagnostic(format!("{at}.tol")));
                } else if tol.value.is_finite() && tol.value <= 0.0 {
                    out.push(Diagnostic::new(
                        DiagnosticCode::ETypeRange,
                        format!("{at}.tol"),
                        "tolerance must be greater than zero",
                    ));
                }
            }
            Comparator::IsTrue {} | Comparator::IsFalse {} => {}
        }
        out
    }
}

/// Quantifier over a state space. Clash and clearance rules default to
/// `for_all` (result reports the worst state); `exists` reports a witness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Quantifier {
    #[default]
    ForAll,
    Exists,
}

impl Quantifier {
    fn is_default(&self) -> bool {
        *self == Quantifier::ForAll
    }
}

// ------------------------------------------------------------ claims & friends

/// A rule or primitive argument. Which keys a rule takes and what kind each
/// must be is the library's signature (checked at typecheck, not here).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Arg {
    Model(ModelRef),
    Part(PartRef),
    Set(SetRef),
    /// Every part of the subject not in the claim's moving set.
    Others,
    Param(ParamRef),
    Joint(JointRef),
    Feature(FeatureRef),
    Axis(AxisRef),
    Quantity(Literal),
    /// Plain number (counts, ratios).
    Number(f64),
    /// Structural keyword from the library: a primitive name (`fn`), a
    /// direction (`"+x"`). Carries no provenance; the library fixes the
    /// allowed words.
    Text(String),
    /// A word taken from the prose or from elsewhere, with provenance:
    /// thread size `M8`, fit class `H7`, an orientation, a preset name.
    Token(Token),
    Bool(bool),
    Compare(Comparator),
    States(Box<StateSpace>),
    List(Vec<Arg>),
}

/// A word-valued argument with a source, checked like a number: a `text`
/// token must appear in the claim's span (see `parse::token_in_span`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Token {
    pub text: String,
    #[serde(default, skip_serializing_if = "Source::is_default")]
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Token {
    pub fn new(text: impl Into<String>, source: Source) -> Self {
        Self {
            text: text.into(),
            source,
            note: None,
        }
    }
}

/// One checkable statement: a library rule or primitive applied to arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub id: String,
    /// Library rule (`moves_freely`) or primitive (`measure` with `fn` arg).
    pub rule: String,
    #[serde(default)]
    pub args: BTreeMap<String, Arg>,
    #[serde(default, skip_serializing_if = "Quantifier::is_default")]
    pub quantifier: Quantifier,
    /// Prose this claim implements. Omitted for claims with no prose
    /// (`inferred`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    #[serde(default, skip_serializing_if = "Source::is_default")]
    pub source: Source,
    /// Why, for `inferred` claims; shown in the review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AllowKind {
    /// Contact with overlap up to ε.
    Touch,
    /// Overlap up to `max`.
    Interference,
}

/// Declared intended contact between two groups of parts (spec §3.5).
/// Everything else clashing is a failure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Allowance {
    pub pair: [Target; 2],
    pub kind: AllowKind,
    /// Required for `interference` (a volume); there is no unbounded allowance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<Literal>,
    /// Limit the allowance to some states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<StateSpace>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Source::is_default")]
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
}

/// Prose the IR can't express. The requirement can then be at most
/// `met_partial`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Unsupported {
    pub span: Span,
    pub reason: String,
}

/// `let name = set(...)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetDef {
    /// Parts or globs; flat (no nested sets).
    pub members: Vec<PartRef>,
    #[serde(default, skip_serializing_if = "Source::is_default")]
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
}

/// The user's answer to a question about a vague word ("freely": no overlap).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    /// The word or phrase asked about, as in the vague-word table.
    pub phrase: String,
    /// The chosen reading, in the user's words (shown in the review).
    pub answer: String,
}

/// Maps a tagged model to a part of the assembly that checks it (spec §3.2):
/// `@lid -> @enclosure/lid`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cover {
    pub model: ModelRef,
    pub part: PartRef,
}

/// A compiled requirement: what the prose says, as claims over named parts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    /// Shape version of this IR; currently 1.
    pub ir_version: u32,
    pub id: String,
    pub name: String,
    /// The user's prose, with `@[Label](entry#builder)` chip tokens. Spans
    /// index this text.
    pub text: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sets: BTreeMap<String, SetDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<Cover>,
    pub claims: Vec<Claim>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<Allowance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported: Vec<Unsupported>,
    /// The user's recorded answers to questions about vague words, so the
    /// compiler does not ask again (`E_VAGUE`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<Answer>,
}

pub(crate) fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap());
                out.push(':');
                canonical(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(v, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

impl Requirement {
    /// Canonical JSON (sorted keys, no whitespace); the input to hashing.
    pub fn canonical_json(&self) -> String {
        let value = serde_json::to_value(self).expect("IR serializes");
        let mut out = String::new();
        canonical(&value, &mut out);
        out
    }

    /// SHA-256 of the canonical JSON.
    pub fn content_hash(&self) -> String {
        portable::bytes_hash(self.canonical_json().as_bytes())
    }

    /// Structural checks that need no model or library: version, unique ids,
    /// spans inside the text, known set names, comparator and state-space
    /// validity. Everything deeper is binding and typechecking.
    pub fn validate(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if self.ir_version != IR_VERSION {
            out.push(Diagnostic::new(
                DiagnosticCode::ESchema,
                "ir_version",
                format!(
                    "unsupported ir_version {} (expected {IR_VERSION})",
                    self.ir_version
                ),
            ));
        }
        let len = self.text.chars().count();
        let check_span = |span: Span, at: String, out: &mut Vec<Diagnostic>| {
            if span[0] >= span[1] || span[1] > len {
                out.push(
                    Diagnostic::new(
                        DiagnosticCode::ESchema,
                        at,
                        format!(
                            "span [{}, {}] is empty or outside the text (length {len})",
                            span[0], span[1]
                        ),
                    )
                    .with_span(None),
                );
            }
        };
        let mut seen = std::collections::HashSet::new();
        for (i, claim) in self.claims.iter().enumerate() {
            let at = format!("claims[{i}]");
            if !seen.insert(claim.id.as_str()) {
                out.push(Diagnostic::new(
                    DiagnosticCode::ESchema,
                    format!("{at}.id"),
                    format!("duplicate claim id {:?}", claim.id),
                ));
            }
            if let Some(span) = claim.span {
                check_span(span, format!("{at}.span"), &mut out);
            }
            for (name, arg) in &claim.args {
                self.validate_arg(arg, &format!("{at}.args.{name}"), &mut out);
            }
        }
        for (i, a) in self.allow.iter().enumerate() {
            let at = format!("allow[{i}]");
            if let Some(span) = a.span {
                check_span(span, format!("{at}.span"), &mut out);
            }
            match (&a.kind, &a.max) {
                (AllowKind::Interference, None) => out.push(Diagnostic::new(
                    DiagnosticCode::ETypeRange,
                    format!("{at}.max"),
                    "an interference allowance needs a max (no unbounded allowances)",
                )),
                (AllowKind::Touch, Some(_)) => out.push(Diagnostic::new(
                    DiagnosticCode::ETypeRange,
                    format!("{at}.max"),
                    "a touch allowance takes no max",
                )),
                (_, Some(max)) => {
                    check_finite(max, &format!("{at}.max"), &mut out);
                    if let Err(e) = max.quantity().expect(Dimension::Volume) {
                        out.push(e.diagnostic(format!("{at}.max")));
                    } else if max.value <= 0.0 {
                        out.push(Diagnostic::new(
                            DiagnosticCode::ETypeRange,
                            format!("{at}.max"),
                            "max must be greater than zero",
                        ));
                    }
                }
                _ => {}
            }
            if let Some(states) = &a.at {
                out.extend(states.validate(&format!("{at}.at")));
            }
            for (j, t) in a.pair.iter().enumerate() {
                self.validate_target(t, &format!("{at}.pair[{j}]"), &mut out);
            }
        }
        for (i, u) in self.unsupported.iter().enumerate() {
            check_span(u.span, format!("unsupported[{i}].span"), &mut out);
        }
        for (name, set) in &self.sets {
            if let Some(span) = set.span {
                check_span(span, format!("sets.{name}.span"), &mut out);
            }
            if set.members.is_empty() {
                out.push(Diagnostic::new(
                    DiagnosticCode::ETypeRange,
                    format!("sets.{name}.members"),
                    "a set needs at least one member",
                ));
            }
        }
        out
    }

    fn validate_target(&self, target: &Target, at: &str, out: &mut Vec<Diagnostic>) {
        if let Target::Set(SetRef(name)) = target
            && !self.sets.contains_key(name)
        {
            out.push(Diagnostic::new(
                DiagnosticCode::EBindPartMissing,
                at,
                format!("set {name:?} is not defined"),
            ));
        }
    }

    fn validate_arg(&self, arg: &Arg, at: &str, out: &mut Vec<Diagnostic>) {
        match arg {
            Arg::Set(SetRef(name)) => {
                if !self.sets.contains_key(name) {
                    out.push(Diagnostic::new(
                        DiagnosticCode::EBindPartMissing,
                        at,
                        format!("set {name:?} is not defined"),
                    ));
                }
            }
            Arg::Quantity(lit) => check_finite(lit, at, out),
            Arg::Number(n) if !n.is_finite() => out.push(Diagnostic::new(
                DiagnosticCode::ETypeRange,
                at,
                "value must be a finite number",
            )),
            Arg::Axis(axis) => check_axis(axis, at, out),
            Arg::Compare(c) => out.extend(c.validate(at)),
            Arg::States(s) => {
                out.extend(s.validate(at));
                self.validate_states_targets(s, at, out);
            }
            Arg::List(items) => {
                for (i, item) in items.iter().enumerate() {
                    self.validate_arg(item, &format!("{at}[{i}]"), out);
                }
            }
            _ => {}
        }
    }

    fn validate_states_targets(&self, s: &StateSpace, at: &str, out: &mut Vec<Diagnostic>) {
        match s {
            StateSpace::Rotate { moving, .. } | StateSpace::Translate { moving, .. } => {
                self.validate_target(moving, &format!("{at}.moving"), out)
            }
            StateSpace::Product { factors } => {
                for (i, f) in factors.iter().enumerate() {
                    self.validate_states_targets(f, &format!("{at}.factors[{i}]"), out);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lit(v: f64, u: Unit) -> Literal {
        Literal::new(v, u)
    }
    fn codes(d: &[Diagnostic]) -> Vec<DiagnosticCode> {
        d.iter().map(|d| d.code).collect()
    }

    #[test]
    fn comparator_json_shapes() {
        let ge: Comparator =
            serde_json::from_value(json!({"op": ">=", "value": {"value": 0, "unit": "mm"}}))
                .unwrap();
        assert_eq!(
            ge,
            Comparator::Ge {
                value: lit(0.0, Unit::Mm)
            }
        );
        let t: Comparator = serde_json::from_value(json!({"op": "is_true"})).unwrap();
        assert_eq!(t, Comparator::IsTrue {});
        assert!(serde_json::from_value::<Comparator>(json!({"op": "=="})).is_err());
        assert!(serde_json::from_value::<Comparator>(json!({"op": "is_true", "x": 1})).is_err());
    }

    #[test]
    fn comparator_validation() {
        let ok = Comparator::In {
            lo: lit(0.2, Unit::Mm),
            hi: lit(0.4, Unit::Mm),
        };
        assert!(ok.validate("c").is_empty());
        // Mixed units compare after normalisation: 0.2 cm = 2 mm > 0.4 mm.
        let bad = Comparator::In {
            lo: lit(0.2, Unit::Cm),
            hi: lit(0.4, Unit::Mm),
        };
        assert_eq!(codes(&bad.validate("c")), [DiagnosticCode::ETypeRange]);
        let unit = Comparator::In {
            lo: lit(0.2, Unit::Mm),
            hi: lit(0.4, Unit::Deg),
        };
        assert_eq!(codes(&unit.validate("c")), [DiagnosticCode::ETypeUnit]);
        for tol in [0.0, -1.0] {
            let c = Comparator::Approx {
                target: lit(8.0, Unit::Mm),
                tol: lit(tol, Unit::Mm),
            };
            assert_eq!(codes(&c.validate("c")), [DiagnosticCode::ETypeRange]);
        }
        let c = Comparator::Approx {
            target: lit(8.0, Unit::Mm),
            tol: lit(0.1, Unit::Mm),
        };
        assert!(c.validate("c").is_empty());
        let nan = Comparator::Ge {
            value: lit(f64::NAN, Unit::Mm),
        };
        assert_eq!(codes(&nan.validate("c")), [DiagnosticCode::ETypeRange]);
        assert!(Comparator::IsFalse {}.validate("c").is_empty());
        assert_eq!(
            bad.validate("claims[0].args.cmp")[0].at,
            "claims[0].args.cmp"
        );
    }

    fn param_range(from: f64, to: f64, step: f64) -> StateSpace {
        StateSpace::Param {
            param: ParamRef {
                model: None,
                name: "p".into(),
            },
            values: ParamValues::Range {
                from: lit(from, Unit::Deg),
                to: lit(to, Unit::Deg),
                step: lit(step, Unit::Deg),
            },
        }
    }

    #[test]
    fn state_counts_include_the_end() {
        assert_eq!(StateSpace::Rest {}.state_count(), Some(1));
        assert_eq!(param_range(0.0, 110.0, 2.0).state_count(), Some(56));
        assert_eq!(param_range(0.0, 111.0, 2.0).state_count(), Some(57));
        assert_eq!(param_range(48.0, 132.0, 2.0).state_count(), Some(43));
        assert_eq!(param_range(0.0, 360.0, 5.0).state_count(), Some(73));
        assert_eq!(param_range(0.0, 1.0, 0.1).state_count(), Some(11));
        assert_eq!(param_range(5.0, 5.0, 1.0).state_count(), Some(1));
        assert_eq!(param_range(0.0, 1.0, 0.0).state_count(), None);
        assert_eq!(param_range(2.0, 1.0, 1.0).state_count(), None);
        let product = StateSpace::Product {
            factors: vec![
                param_range(48.0, 132.0, 2.0),
                StateSpace::Param {
                    param: ParamRef {
                        model: None,
                        name: "t".into(),
                    },
                    values: ParamValues::List {
                        values: vec![lit(0.0, Unit::Mm), lit(4.0, Unit::Mm), lit(8.0, Unit::Mm)],
                    },
                },
            ],
        };
        assert_eq!(product.state_count(), Some(43 * 3));
    }

    #[test]
    fn state_space_validation() {
        assert!(param_range(0.0, 110.0, 2.0).validate("s").is_empty());
        assert_eq!(
            codes(&param_range(0.0, 110.0, 0.0).validate("s")),
            [DiagnosticCode::ETypeRange]
        );
        assert_eq!(
            codes(&param_range(10.0, 0.0, 1.0).validate("s")),
            [DiagnosticCode::ETypeRange]
        );
        let mixed = StateSpace::Param {
            param: ParamRef {
                model: None,
                name: "p".into(),
            },
            values: ParamValues::Range {
                from: lit(0.0, Unit::Deg),
                to: lit(5.0, Unit::Mm),
                step: lit(1.0, Unit::Deg),
            },
        };
        assert_eq!(codes(&mixed.validate("s")), [DiagnosticCode::ETypeUnit]);
        let part = Target::Part(PartRef {
            model: ModelRef {
                entry: "a.py".into(),
                builder: "b".into(),
            },
            part: PartName::Name("d".into()),
        });
        let rot = |axis: AxisRef, unit: Unit| StateSpace::Rotate {
            moving: part.clone(),
            axis,
            range: Sweep {
                from: lit(0.0, unit),
                to: lit(90.0, unit),
                step: lit(5.0, unit),
            },
        };
        assert!(rot(AxisRef::Z, Unit::Deg).validate("s").is_empty());
        assert!(
            !rot(AxisRef::Vector([0.0; 3]), Unit::Deg)
                .validate("s")
                .is_empty()
        );
        assert_eq!(
            codes(&rot(AxisRef::X, Unit::Mm).validate("s")),
            [DiagnosticCode::ETypeUnit]
        );
        assert!(
            !StateSpace::Product { factors: vec![] }
                .validate("s")
                .is_empty()
        );
    }

    #[test]
    fn state_space_json_round_trips() {
        for value in [
            json!({"kind": "rest"}),
            json!({"kind": "param", "param": {"name": "door_angle"}, "values": {"shape": "value", "value": {"value": 0, "unit": "deg"}}}),
            json!({"kind": "rotate", "moving": {"set": "turntable"}, "axis": "z",
                   "range": {"from": {"value": 0.0, "unit": "deg"}, "to": {"value": 360.0, "unit": "deg"}, "step": {"value": 5.0, "unit": "deg"}}}),
            json!({"kind": "product", "factors": [{"kind": "rest"}]}),
        ] {
            let s: StateSpace = serde_json::from_value(value.clone()).unwrap();
            let back = serde_json::to_value(&s).unwrap();
            let again: StateSpace = serde_json::from_value(back).unwrap();
            assert_eq!(s, again);
        }
        assert!(serde_json::from_value::<StateSpace>(json!({"kind": "rest", "step": 1})).is_err());
    }

    #[test]
    fn source_defaults_to_text_and_is_omitted() {
        let l: Literal = serde_json::from_value(json!({"value": 1.0, "unit": "mm"})).unwrap();
        assert_eq!(l.source, Source::Text);
        assert_eq!(
            serde_json::to_value(&l).unwrap(),
            json!({"value": 1.0, "unit": "mm"})
        );
        let l: Literal = serde_json::from_value(
            json!({"value": 1.0, "unit": "mm", "source": "inferred", "note": "assumed"}),
        )
        .unwrap();
        assert_eq!(l.source, Source::Inferred);
        assert!(
            serde_json::from_value::<Literal>(
                json!({"value": 1.0, "unit": "mm", "source": "standard"})
            )
            .is_err()
        );
    }

    #[test]
    fn requirement_structural_validation() {
        let mut req: Requirement = serde_json::from_value(json!({
            "ir_version": 1, "id": "r", "name": "n", "text": "door opens",
            "claims": [
                {"id": "c1", "rule": "single_solid", "args": {"a": {"set": "nope"}}, "span": [0, 4]},
                {"id": "c1", "rule": "single_solid", "span": [0, 99]}
            ],
            "allow": [{"pair": [{"set": "nope"}, {"set": "nope"}], "kind": "interference", "reason": "x"}]
        })).unwrap();
        let d = req.validate();
        let at: Vec<_> = d.iter().map(|d| d.at.as_str()).collect();
        assert!(at.contains(&"claims[0].args.a"));
        assert!(at.contains(&"claims[1].id"));
        assert!(at.contains(&"claims[1].span"));
        assert!(at.contains(&"allow[0].max"));
        assert!(at.contains(&"allow[0].pair[1]"));
        req.ir_version = 2;
        assert!(req.validate().iter().any(|d| d.at == "ir_version"));
    }

    #[test]
    fn content_hash_ignores_key_order_and_tracks_changes() {
        let a: Requirement = serde_json::from_value(
            json!({"ir_version": 1, "id": "r", "name": "n", "text": "t", "claims": []}),
        )
        .unwrap();
        let b: Requirement = serde_json::from_str(
            r#"{"claims": [], "text": "t", "name": "n", "id": "r", "ir_version": 1}"#,
        )
        .unwrap();
        assert_eq!(a.content_hash(), b.content_hash());
        let mut c = a.clone();
        c.name = "other".into();
        assert_ne!(a.content_hash(), c.content_hash());
        assert_eq!(a.content_hash().len(), 64);
    }
}
