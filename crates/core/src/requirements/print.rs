//! Canonical print (spec §5): the English the user approves. One line per
//! rule, rendered from the library's print templates with resolved names,
//! sampling and counts. Anything inferred is marked inline; numbers the user
//! didn't write are summarised on one line, with sources in details.
//!
//! Layout:
//!
//! ```text
//! Door opens without hitting frame
//!   1. Door does not overlap frame, door_angle 0°–110°, checked every 2° (56 states).
//!   2. Inferred: door rests on frame … [from "hitting"]
//!   Numbers I chose: door_angle step 2°
//!      ▸ details: 56 checks · ε 0.001 mm³ · accuracy exact (interference) · …
//! ```
use super::bind::{Bindings, BoundPart, part_ref_key};
use super::expand::PrimitiveClaim;
use super::ir::{
    AllowKind, Allowance, Arg, AxisRef, Claim, Comparator, FeatureRef, Literal, PartName,
    Requirement, SetRef, Source, StateSpace, Target, Token,
};
use super::library::{Accuracy, library};
use super::plan::Plan;
use super::quantity::{Quantity, Unit};
use super::settings::ProjectSettings;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Sets and member lists longer than this print as a count in the sentence,
/// with the full list in details.
const INLINE_MEMBERS: usize = 4;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PrintLine {
    /// The sentence, without numbering or marks.
    pub text: String,
    /// IR claim id (empty for sets and allowances).
    pub claim: String,
    pub span: Option<[usize; 2]>,
    pub inferred: bool,
    /// The prose or reason an inferred item came from.
    pub because: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Print {
    pub title: String,
    /// Named sets with their members.
    pub sets: Vec<PrintLine>,
    pub lines: Vec<PrintLine>,
    pub numbers_chosen: Option<String>,
    pub details: Vec<String>,
}

impl Print {
    /// The text the user reads and approves.
    pub fn render(&self) -> String {
        let mut out = vec![self.title.clone()];
        let mut n = 0;
        let mut put = |l: &PrintLine, numbered: bool, out: &mut Vec<String>| {
            let mut s = String::from("  ");
            if numbered {
                n += 1;
                s.push_str(&format!("{n}. "));
            }
            if l.inferred {
                s.push_str("Inferred: ");
                s.push_str(&l.text);
            } else {
                s.push_str(&capitalize(&l.text));
            }
            if l.inferred
                && let Some(b) = &l.because
            {
                s.push_str(&format!(" [{b}]"));
            }
            out.push(s);
        };
        for l in &self.sets {
            put(l, false, &mut out);
        }
        for l in &self.lines {
            put(l, true, &mut out);
        }
        if let Some(c) = &self.numbers_chosen {
            out.push(format!("  Numbers I chose: {c}"));
        }
        if !self.details.is_empty() {
            out.push(format!("     ▸ details: {}", self.details.join(" · ")));
        }
        out.join("\n") + "\n"
    }
}

/// Capitalise a sentence that starts with a plain word; names like `clamp_*`
/// or `tower_arm_1` are left as written.
fn capitalize(s: &str) -> String {
    let first: String = s.chars().take_while(|c| !c.is_whitespace()).collect();
    if !first.chars().all(char::is_alphabetic) {
        return s.to_string();
    }
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ----------------------------------------------------------------- numbers

pub fn fmt_num(v: f64) -> String {
    format!("{v}")
}

fn fmt_q(q: &Quantity) -> String {
    q.to_string()
}

fn fmt_lit(l: &Literal) -> String {
    fmt_q(&l.quantity())
}

/// `a–b unit` with the unit once, or `a°–b°` for degrees.
fn fmt_range(a: &Literal, b: &Literal) -> String {
    match a.unit {
        Unit::Deg => format!("{}–{}", fmt_lit(a), fmt_lit(b)),
        Unit::One => format!("{}–{}", fmt_num(a.value), fmt_num(b.value)),
        u if a.unit == b.unit => format!("{}–{} {}", fmt_num(a.value), fmt_num(b.value), u),
        _ => format!("{}–{}", fmt_lit(a), fmt_lit(b)),
    }
}

/// `a, b, c unit`.
fn fmt_list(values: &[Literal]) -> String {
    let same = values.windows(2).all(|w| w[0].unit == w[1].unit);
    match (values.first(), same) {
        (Some(f), true) if f.unit != Unit::Deg => {
            let nums: Vec<_> = values.iter().map(|v| fmt_num(v.value)).collect();
            if f.unit == Unit::One {
                nums.join(", ")
            } else {
                format!("{} {}", nums.join(", "), f.unit)
            }
        }
        _ => values.iter().map(fmt_lit).collect::<Vec<_>>().join(", "),
    }
}

fn fmt_cmp(c: &Comparator) -> String {
    match c {
        Comparator::Ge { value } => format!("at least {}", fmt_lit(value)),
        Comparator::Le { value } => format!("at most {}", fmt_lit(value)),
        Comparator::In { lo, hi } => {
            if lo.unit == hi.unit && lo.unit != Unit::Deg && lo.unit != Unit::One {
                format!(
                    "between {} and {} {}",
                    fmt_num(lo.value),
                    fmt_num(hi.value),
                    lo.unit
                )
            } else {
                format!("between {} and {}", fmt_lit(lo), fmt_lit(hi))
            }
        }
        Comparator::Approx { target, tol } => {
            format!("{} within {}", fmt_lit(target), fmt_lit(tol))
        }
        Comparator::IsTrue {} => "true".into(),
        Comparator::IsFalse {} => "false".into(),
    }
}

// ------------------------------------------------------------ names & states

struct Ctx<'a> {
    req: &'a Requirement,
    b: &'a Bindings,
    /// Full member lists to show in details: (what, members).
    long_lists: std::cell::RefCell<Vec<(String, Vec<String>)>>,
    /// Id of the claim being printed, for `others`.
    claim: std::cell::RefCell<String>,
}

impl Ctx<'_> {
    fn members(&self, what: &str, parts: &[BoundPart]) -> String {
        let mut names: Vec<String> = parts.iter().map(|p| p.name.clone()).collect();
        names.sort();
        if names.len() <= INLINE_MEMBERS {
            format!("({})", names.join(", "))
        } else {
            self.long_lists
                .borrow_mut()
                .push((what.to_string(), names.clone()));
            format!("({} parts)", names.len())
        }
    }

    fn part(&self, p: &super::ir::PartRef) -> String {
        match &p.part {
            PartName::Name(n) => n.clone(),
            PartName::Glob(g) => match self.b.parts.get(&part_ref_key(p)) {
                Some(m) => format!("{g} {}", self.members(g, m)),
                None => g.clone(),
            },
        }
    }

    fn set(&self, name: &str) -> String {
        match self.b.sets.get(name) {
            Some(m) => format!("{name} {}", self.members(name, m)),
            None => name.to_string(),
        }
    }

    fn target(&self, t: &Target) -> String {
        match t {
            Target::Part(p) => self.part(p),
            Target::Set(SetRef(n)) => self.set(n),
            Target::Parts(ps) => ps
                .iter()
                .map(|p| self.part(p))
                .collect::<Vec<_>>()
                .join(", "),
        }
    }

    /// A moving set inside a state phrase: just its name, members are in the sentence.
    fn target_short(&self, t: &Target) -> String {
        match t {
            Target::Set(SetRef(n)) => n.clone(),
            other => self.target(other),
        }
    }

    fn axis(&self, a: &AxisRef) -> String {
        match a {
            AxisRef::X => "x".into(),
            AxisRef::Y => "y".into(),
            AxisRef::Z => "z".into(),
            AxisRef::Vector(v) => {
                format!("({}, {}, {})", fmt_num(v[0]), fmt_num(v[1]), fmt_num(v[2]))
            }
            AxisRef::Feature(f) => self.feature(f),
        }
    }

    fn feature(&self, f: &FeatureRef) -> String {
        match f {
            FeatureRef::Explicit { part, name } | FeatureRef::History { part, name, .. } => {
                format!("{name} of {}", self.part(part))
            }
            FeatureRef::Geometric { part, entity, hash } => format!(
                "{entity:?} {} of {}",
                &hash[..hash.len().min(8)],
                self.part(part)
            )
            .to_lowercase(),
        }
    }

    fn states_body(&self, s: &StateSpace) -> String {
        match s {
            StateSpace::Rest {} => "as built".into(),
            StateSpace::Param { param, values } => match values {
                super::ir::ParamValues::Value { value } => {
                    format!("at {} {}", param.name, fmt_lit(value))
                }
                super::ir::ParamValues::Range { from, to, step } => {
                    format!(
                        "{} {}, checked every {}",
                        param.name,
                        fmt_range(from, to),
                        fmt_lit(step)
                    )
                }
                super::ir::ParamValues::List { values } => {
                    format!("{} at {}", param.name, fmt_list(values))
                }
            },
            StateSpace::Rotate {
                moving,
                axis,
                range,
            } => format!(
                "{} rotated about {} {}, checked every {}",
                self.target_short(moving),
                self.axis(axis),
                fmt_range(&range.from, &range.to),
                fmt_lit(&range.step)
            ),
            StateSpace::Translate {
                moving,
                direction,
                range,
            } => format!(
                "{} moved along {} {}, checked every {}",
                self.target_short(moving),
                self.axis(direction),
                fmt_range(&range.from, &range.to),
                fmt_lit(&range.step)
            ),
            StateSpace::Joint { joint, range } => format!(
                "joint {} {}, checked every {}",
                joint.name,
                fmt_range(&range.from, &range.to),
                fmt_lit(&range.step)
            ),
            StateSpace::Simulate {
                model,
                simulation,
                when,
            } => format!(
                "proxy simulation of {} ({} actions, {} ticks, {})",
                model.entry,
                simulation.actions.len(),
                simulation.actions.iter().map(|a| a.ticks).sum::<u32>(),
                match when {
                    super::ir::When::End {} => "at end",
                    super::ir::When::At { time } =>
                        return format!(
                            "proxy simulation of {} ({} actions, {} ticks, at {})",
                            model.entry,
                            simulation.actions.len(),
                            simulation.actions.iter().map(|a| a.ticks).sum::<u32>(),
                            fmt_lit(time)
                        ),
                    super::ir::When::Always {} => "always",
                    super::ir::When::Eventually {} => "eventually",
                }
            ),
            StateSpace::Product { factors } => factors
                .iter()
                .map(|f| self.states_body(f))
                .collect::<Vec<_>>()
                .join(" × "),
        }
    }

    /// Body plus the state count, when there is more than one state.
    fn states(&self, s: &StateSpace) -> String {
        let body = self.states_body(s);
        match s.state_count() {
            Some(n) if n > 1 => format!("{body} ({n} states)"),
            _ => body,
        }
    }

    fn states_arg(&self, a: &Arg) -> String {
        match a {
            Arg::States(s) => self.states(s),
            Arg::List(items) => items
                .iter()
                .map(|i| self.states_arg(i))
                .collect::<Vec<_>>()
                .join(" and "),
            _ => String::new(),
        }
    }

    fn token(&self, t: &Token) -> String {
        t.text.replace('_', " ")
    }

    fn arg(&self, a: &Arg) -> String {
        match a {
            Arg::Model(m) => m.builder.clone(),
            Arg::Part(p) => self.part(p),
            Arg::Set(SetRef(n)) => self.set(n),
            Arg::Others => match self.b.others.get(self.claim.borrow().as_str()) {
                Some(m) => format!("all other parts {}", self.members("other parts", m)),
                None => "all other parts".into(),
            },
            Arg::Param(p) => p.name.clone(),
            Arg::Joint(j) => j.name.clone(),
            Arg::Feature(f) => self.feature(f),
            Arg::Axis(x) => self.axis(x),
            Arg::Quantity(l) => fmt_lit(l),
            Arg::Number(n) => fmt_num(*n),
            Arg::Text(t) => t.clone(),
            Arg::Token(t) => self.token(t),
            Arg::Bool(b) => if *b { "yes" } else { "no" }.into(),
            Arg::Compare(c) => fmt_cmp(c),
            Arg::States(_) => self.states_arg(a),
            Arg::List(items) => {
                let quantities: Vec<&Literal> = items
                    .iter()
                    .filter_map(|i| {
                        if let Arg::Quantity(l) = i {
                            Some(l)
                        } else {
                            None
                        }
                    })
                    .collect();
                if quantities.len() == items.len() && !items.is_empty() {
                    let units_same = quantities.windows(2).all(|w| w[0].unit == w[1].unit);
                    if units_same {
                        let nums: Vec<_> = quantities.iter().map(|q| fmt_num(q.value)).collect();
                        return format!("{} {}", nums.join(" × "), quantities[0].unit);
                    }
                }
                items
                    .iter()
                    .map(|i| self.arg(i))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        }
    }
}

/// Fill a library template. `{name}` takes the printed argument; a `[...]`
/// segment is dropped unless every placeholder in it has a value.
fn fill(template: &str, value: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '[' => {
                let seg: String = chars.by_ref().take_while(|c| *c != ']').collect();
                let names = super::library::template_args(&seg);
                if names.iter().all(|n| value(n).is_some()) {
                    out.push_str(&fill(&seg, value));
                }
            }
            '{' => {
                let name: String = chars.by_ref().take_while(|c| *c != '}').collect();
                out.push_str(&value(&name).unwrap_or_default());
            }
            c => out.push(c),
        }
    }
    out
}

/// Replace chip tokens `@[Label](...)` with `Label`.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    let chars: Vec<char> = text.chars().collect();
    for chip in super::bind::parse_chips(text) {
        out.extend(&chars[last..chip.span[0]]);
        out.push_str(&chip.label);
        last = chip.span[1];
    }
    out.extend(&chars[last..]);
    out
}

fn prose(req: &Requirement, span: [usize; 2]) -> String {
    let s: String = req
        .text
        .chars()
        .skip(span[0])
        .take(span[1].saturating_sub(span[0]))
        .collect();
    plain(&s)
}

fn because(req: &Requirement, span: Option<[usize; 2]>, note: &Option<String>) -> Option<String> {
    match (span, note) {
        (Some(s), _) => Some(format!("from \"{}\"", prose(req, s))),
        (None, Some(n)) => Some(n.clone()),
        (None, None) => None,
    }
}

// ----------------------------------------------------------------- sentences

fn claim_sentence(cx: &Ctx, claim: &Claim) -> String {
    let lib = library();
    let val = |name: &str| -> Option<String> {
        let arg = claim.args.get(name);
        match arg {
            Some(Arg::States(_)) | Some(Arg::List(_)) if matches!(name, "at" | "over") => {
                Some(cx.states_arg(arg.expect("present")))
            }
            Some(a) => Some(cx.arg(a)),
            None => lib
                .rule(&claim.rule)
                .and_then(|r| r.args.iter().find(|s| s.name == name))
                .and_then(|s| s.default.as_ref())
                .map(|d| cx.arg(d)),
        }
    };
    if claim.rule == "measure" {
        let Some(Arg::Text(name)) = claim.args.get("fn") else {
            return String::new();
        };
        let Some(def) = lib.primitive(name) else {
            return String::new();
        };
        let what = fill(def.template, &|n| claim.args.get(n).map(|a| cx.arg(a)));
        let cmp = claim.args.get("cmp").map(|a| cx.arg(a)).unwrap_or_default();
        let at = claim
            .args
            .get("at")
            .map(|a| cx.states_arg(a))
            .unwrap_or_else(|| "as built".into());
        return format!("{what} is {cmp}, {at}");
    }
    match lib.rule(&claim.rule) {
        Some(rule) => fill(rule.template, &val),
        None => String::new(),
    }
}

fn set_line(cx: &Ctx, name: &str, set: &super::ir::SetDef) -> PrintLine {
    let members = cx.b.sets.get(name).cloned().unwrap_or_default();
    let mut names: Vec<String> = members.iter().map(|p| p.name.clone()).collect();
    names.sort();
    let list = if names.len() <= INLINE_MEMBERS {
        format!("{} ({} parts)", names.join(", "), names.len())
    } else {
        format!("{} parts", names.len())
    };
    PrintLine {
        text: format!("set {name} = {list}"),
        claim: String::new(),
        span: set.span,
        inferred: set.source == Source::Inferred,
        because: because(cx.req, set.span, &None),
    }
}

fn allowance_line(cx: &Ctx, a: &Allowance) -> PrintLine {
    let (x, y) = (cx.target(&a.pair[0]), cx.target(&a.pair[1]));
    let when =
        a.at.as_ref()
            .map_or("at every state".to_string(), |s| cx.states(s));
    let what = match (&a.kind, &a.max) {
        (AllowKind::Touch, _) => format!("{x} may touch {y}, {when}"),
        (AllowKind::Interference, Some(m)) => {
            format!("{x} may overlap {y} by up to {}, {when}", fmt_lit(m))
        }
        (AllowKind::Interference, None) => format!("{x} may overlap {y}, {when}"),
    };
    PrintLine {
        text: format!("{what} ({})", a.reason),
        claim: String::new(),
        span: a.span,
        inferred: a.source == Source::Inferred,
        because: because(cx.req, a.span, &None),
    }
}

// ------------------------------------------------------------ numbers chosen

struct Chosen {
    label: String,
    value: String,
    source: Source,
    note: Option<String>,
}

fn push_lit(out: &mut Vec<Chosen>, label: String, l: &Literal) {
    if l.source != Source::Text {
        out.push(Chosen {
            label,
            value: fmt_lit(l),
            source: l.source,
            note: l.note.clone(),
        });
    }
}

fn chosen_from_sweep(
    out: &mut Vec<Chosen>,
    prefix: &str,
    from: &Literal,
    to: &Literal,
    step: &Literal,
) {
    push_lit(out, format!("{prefix} from"), from);
    push_lit(out, format!("{prefix} to"), to);
    push_lit(out, format!("{prefix} step"), step);
}

fn chosen_from_states(out: &mut Vec<Chosen>, s: &StateSpace) {
    match s {
        StateSpace::Rest {} => {}
        StateSpace::Param { param, values } => match values {
            super::ir::ParamValues::Value { value } => push_lit(out, param.name.clone(), value),
            super::ir::ParamValues::Range { from, to, step } => {
                chosen_from_sweep(out, &param.name, from, to, step)
            }
            super::ir::ParamValues::List { values } => {
                let same = values.windows(2).all(|w| w[0].source == w[1].source);
                match values.first() {
                    Some(first) if same => {
                        if first.source != Source::Text {
                            out.push(Chosen {
                                label: format!("{} values", param.name),
                                value: fmt_list(values),
                                source: first.source,
                                note: first.note.clone(),
                            });
                        }
                    }
                    _ => {
                        for (i, v) in values.iter().enumerate() {
                            push_lit(out, format!("{} value {}", param.name, i + 1), v);
                        }
                    }
                }
            }
        },
        StateSpace::Rotate { range, .. } => {
            chosen_from_sweep(out, "rotation", &range.from, &range.to, &range.step)
        }
        StateSpace::Translate { range, .. } => {
            chosen_from_sweep(out, "translation", &range.from, &range.to, &range.step)
        }
        StateSpace::Joint { joint, range } => chosen_from_sweep(
            out,
            &format!("joint {}", joint.name),
            &range.from,
            &range.to,
            &range.step,
        ),
        StateSpace::Product { factors } => factors.iter().for_each(|f| chosen_from_states(out, f)),
        StateSpace::Simulate { .. } => {}
    }
}

fn chosen_from_arg(out: &mut Vec<Chosen>, key: &str, arg: &Arg) {
    match arg {
        Arg::Quantity(l) => push_lit(out, key.to_string(), l),
        Arg::Token(t) if t.source != Source::Text => out.push(Chosen {
            label: key.to_string(),
            value: t.text.replace('_', " "),
            source: t.source,
            note: t.note.clone(),
        }),
        Arg::Compare(c) => match c {
            Comparator::Ge { value } | Comparator::Le { value } => {
                push_lit(out, key.to_string(), value)
            }
            Comparator::In { lo, hi } => {
                push_lit(out, format!("{key} lower"), lo);
                push_lit(out, format!("{key} upper"), hi);
            }
            Comparator::Approx { target, tol } => {
                push_lit(out, format!("{key} target"), target);
                push_lit(out, format!("{key} tolerance"), tol);
            }
            Comparator::IsTrue {} | Comparator::IsFalse {} => {}
        },
        Arg::States(s) => chosen_from_states(out, s),
        Arg::List(items) => {
            let axes = key == "bed" && items.len() == 3;
            for (i, item) in items.iter().enumerate() {
                let label = if axes {
                    format!("{key} {}", ["x", "y", "z"][i])
                } else {
                    format!("{key} {}", i + 1)
                };
                match item {
                    Arg::States(_) | Arg::List(_) => chosen_from_arg(out, key, item),
                    other => chosen_from_arg(out, &label, other),
                }
            }
        }
        _ => {}
    }
}

fn collect_chosen(req: &Requirement) -> Vec<Chosen> {
    let mut out = Vec::new();
    for claim in &req.claims {
        for (key, arg) in &claim.args {
            chosen_from_arg(&mut out, key, arg);
        }
    }
    for (i, a) in req.allow.iter().enumerate() {
        if let Some(max) = &a.max {
            push_lit(&mut out, format!("allowance {} limit", i + 1), max);
        }
        if let Some(s) = &a.at {
            let mut inner = Vec::new();
            chosen_from_states(&mut inner, s);
            for c in inner {
                out.push(Chosen {
                    label: format!("allowance {} {}", i + 1, c.label),
                    ..c
                });
            }
        }
    }
    out
}

// -------------------------------------------------------------------- entry

/// The canonical print of a compiled requirement. `plan` supplies the check
/// count for the details line.
pub fn print_requirement(
    req: &Requirement,
    bindings: &Bindings,
    prims: &[PrimitiveClaim],
    plan: Option<&Plan>,
    settings: &ProjectSettings,
) -> Print {
    let cx = Ctx {
        req,
        b: bindings,
        long_lists: Default::default(),
        claim: Default::default(),
    };
    let sets = req.sets.iter().map(|(n, s)| set_line(&cx, n, s)).collect();
    let mut lines = Vec::new();
    for claim in &req.claims {
        cx.claim.replace(claim.id.clone());
        lines.push(PrintLine {
            text: claim_sentence(&cx, claim) + ".",
            claim: claim.id.clone(),
            span: claim.span,
            inferred: claim.source == Source::Inferred,
            because: because(req, claim.span, &claim.note),
        });
    }
    for a in &req.allow {
        let mut l = allowance_line(&cx, a);
        l.text.push('.');
        lines.push(l);
    }
    let chosen = collect_chosen(req);
    let numbers_chosen = {
        let inferred: Vec<String> = chosen
            .iter()
            .filter(|c| c.source == Source::Inferred)
            .map(|c| format!("{} {}", c.label, c.value))
            .collect();
        (!inferred.is_empty()).then(|| inferred.join(", "))
    };
    let mut details = Vec::new();
    if let Some(p) = plan {
        details.push(format!("{} checks", p.job_count()));
    }
    let lib = library();
    let uses_eps = req.claims.iter().any(|c| {
        lib.rule(&c.rule)
            .is_some_and(|r| r.settings.contains(&"overlap_tolerance"))
    });
    if uses_eps {
        details.push(format!(
            "overlap tolerance ε {}",
            fmt_q(&settings.overlap_tolerance)
        ));
    }
    let used: BTreeSet<&str> = prims.iter().map(|p| p.primitive.as_str()).collect();
    for acc in [Accuracy::Exact, Accuracy::Sampled, Accuracy::Proxy] {
        let names: Vec<&str> = used
            .iter()
            .copied()
            .filter(|n| lib.primitive(n).is_some_and(|d| d.accuracy == acc))
            .collect();
        if !names.is_empty() {
            let label = match acc {
                Accuracy::Exact => "exact",
                Accuracy::Sampled => "sampled",
                Accuracy::Proxy => "proxy (simulation)",
            };
            details.push(format!("accuracy {label} ({})", names.join(", ")));
        }
    }
    for c in &chosen {
        let src = match c.source {
            Source::Text => continue,
            Source::Model => "from the model",
            Source::Library => "from the library",
            Source::Inferred => "my proposal",
        };
        let note = c
            .note
            .as_deref()
            .map(|n| format!("; {n}"))
            .unwrap_or_default();
        details.push(format!("{} {} ({src}{note})", c.label, c.value));
    }
    for (what, names) in cx.long_lists.borrow().iter() {
        details.push(format!("{what} = {}", names.join(", ")));
    }
    Print {
        title: req.name.clone(),
        sets,
        lines,
        numbers_chosen,
        details,
    }
}

/// The expanded primitive claims, one line each, for "expand on demand".
pub fn print_expansion(
    prims: &[PrimitiveClaim],
    bindings: &Bindings,
    req: &Requirement,
) -> Vec<String> {
    let cx = Ctx {
        req,
        b: bindings,
        long_lists: Default::default(),
        claim: Default::default(),
    };
    let lib = library();
    prims
        .iter()
        .map(|p| {
            let what = lib
                .primitive(&p.primitive)
                .map(|d| fill(d.template, &|n| p.args.get(n).map(|a| cx.arg(a))))
                .unwrap_or_default();
            let cmp = if p.strict {
                format!("more than {}", cmp_value(&p.cmp))
            } else {
                fmt_cmp(&p.cmp)
            };
            let how = match p.pairing {
                super::expand::Pairing::Whole => "",
                super::expand::Pairing::EachPair => ", every pair",
                super::expand::Pairing::Each => ", every part",
                super::expand::Pairing::AnyPair => ", some pair",
            };
            let note = p
                .note
                .as_deref()
                .map(|n| format!(" [{n}]"))
                .unwrap_or_default();
            let acc = lib
                .primitive(&p.primitive)
                .map_or("", |d| match d.accuracy {
                    Accuracy::Exact => "exact",
                    Accuracy::Sampled => "sampled",
                    Accuracy::Proxy => "proxy (simulation)",
                });
            format!(
                "{}: {what} is {cmp}{how}, {} [{acc}]{note}",
                p.id,
                cx.states(&p.states)
            )
        })
        .collect()
}

fn cmp_value(c: &Comparator) -> String {
    match c {
        Comparator::Ge { value } | Comparator::Le { value } => fmt_lit(value),
        other => fmt_cmp(other),
    }
}
