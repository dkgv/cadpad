//! Binding (spec §3.1): resolve every name in a requirement against a
//! [`WorkspaceSnapshot`]. Nothing here touches the worker; the snapshot is the
//! symbol table.
use super::diagnostics::{Diagnostic, DiagnosticCode, Fix};
use super::ir::{
    Arg, EntityKind, FeatureRef, ModelRef, ParamRef, PartName, PartRef, Requirement, SetRef, Target,
};
use super::quantity::Quantity;
use super::refs::{self, Ref};
use super::snapshot::{BuiltModel, WorkspaceSnapshot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `entry#builder`.
pub fn model_key(m: &ModelRef) -> String {
    format!("{}#{}", m.entry, m.builder)
}

/// A published part, resolved.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct BoundPart {
    pub model: ModelRef,
    pub name: String,
    pub geometry_hash: String,
}

impl BoundPart {
    /// `entry#builder/name`: the stable key plans and manifests use.
    pub fn key(&self) -> String {
        format!("{}/{}", model_key(&self.model), self.name)
    }
}

/// `entry#builder/name` or `entry#builder/pattern`.
pub fn part_ref_key(p: &PartRef) -> String {
    let (PartName::Name(n) | PartName::Glob(n)) = &p.part;
    format!("{}/{n}", model_key(&p.model))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BoundParam {
    pub model: ModelRef,
    pub name: String,
    pub value: Quantity,
    pub min: Option<Quantity>,
    pub max: Option<Quantity>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BoundJoint {
    pub model: ModelRef,
    pub name: String,
    pub limits: Option<[f64; 2]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BoundFeature {
    pub part: BoundPart,
    pub name: String,
    pub entity: EntityKind,
    pub count: u64,
    pub hash: String,
}

/// An `@[Label](entry#builder[/part])` token in the prose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Chip {
    pub label: String,
    /// Character span of the whole token in the requirement text.
    pub span: [usize; 2],
    pub model: ModelRef,
    pub part: Option<String>,
}

/// Everything binding resolved. Keys are stable strings (see [`part_ref_key`],
/// [`model_key`]) so the whole value serializes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Bindings {
    /// Resolved members of every `PartRef` in the requirement, build order.
    pub parts: BTreeMap<String, Vec<BoundPart>>,
    /// Resolved members of each named set.
    pub sets: BTreeMap<String, Vec<BoundPart>>,
    /// `entry#builder:name` to parameter.
    pub params: BTreeMap<String, BoundParam>,
    /// `entry#builder:name` to joint.
    pub joints: BTreeMap<String, BoundJoint>,
    /// `entry#builder/part#name` to feature.
    pub features: BTreeMap<String, BoundFeature>,
    pub chips: Vec<Chip>,
    /// Claim id to the parts `others` stands for in that claim.
    pub others: BTreeMap<String, Vec<BoundPart>>,
    /// Claim id to its subject model, when it has exactly one.
    pub subjects: BTreeMap<String, ModelRef>,
}

// ------------------------------------------------------------------ helpers

/// Glob match with `*` (any run) and `?` (one character).
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let mut reach = vec![false; n.len() + 1];
    reach[0] = true;
    for pc in &p {
        let mut next = vec![false; n.len() + 1];
        if *pc == '*' {
            let mut any = false;
            for i in 0..=n.len() {
                any |= reach[i];
                next[i] = any;
            }
        } else {
            for i in 0..n.len() {
                next[i + 1] = reach[i] && (*pc == '?' || *pc == n[i]);
            }
        }
        reach = next;
    }
    reach[n.len()]
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// Up to three candidates near `name`, closest first (ties by name):
/// small edit distance, or one contains the other.
pub fn suggest(name: &str, candidates: &[String]) -> Vec<String> {
    let lower = name.to_lowercase();
    let limit = (lower.chars().count() / 3).max(2);
    let mut scored: Vec<(usize, &String)> = candidates
        .iter()
        .filter_map(|c| {
            let cl = c.to_lowercase();
            let d = edit_distance(&lower, &cl);
            let contains = !lower.is_empty() && (cl.contains(&lower) || lower.contains(&cl));
            (d <= limit || contains).then_some((if contains { d.min(limit) } else { d }, c))
        })
        .collect();
    scored.sort();
    scored.into_iter().take(3).map(|(_, c)| c.clone()).collect()
}

/// Parse `@[Label](entry#builder)` and `@[Label](entry#builder/part)` tokens.
/// The part may contain `/` (STEP sub-parts). Malformed tokens are skipped.
pub fn parse_chips(text: &str) -> Vec<Chip> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < c.len() {
        if c[i] == '@' && c[i + 1] == '[' {
            let Some(close) = (i + 2..c.len()).find(|&k| c[k] == ']') else {
                break;
            };
            if c.get(close + 1) == Some(&'(')
                && let Some(end) = (close + 2..c.len()).find(|&k| c[k] == ')')
            {
                let label: String = c[i + 2..close].iter().collect();
                let target: String = c[close + 2..end].iter().collect();
                if let Some((entry, rest)) = target.split_once('#') {
                    let (builder, part) = match rest.split_once('/') {
                        Some((b, p)) => (b, Some(p.to_string())),
                        None => (rest, None),
                    };
                    if !entry.is_empty() && !builder.is_empty() {
                        out.push(Chip {
                            label,
                            span: [i, end + 1],
                            model: ModelRef {
                                entry: entry.into(),
                                builder: builder.into(),
                            },
                            part,
                        });
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

fn model_names(snap: &WorkspaceSnapshot) -> Vec<String> {
    snap.models.iter().map(|m| model_key(&m.model)).collect()
}

/// P1.24: a model of the project catalog, built.
pub fn bind_model<'a>(
    snap: &'a WorkspaceSnapshot,
    model: &ModelRef,
    at: &str,
) -> Result<&'a BuiltModel, Diagnostic> {
    let Some(m) = snap.model(model) else {
        let wanted = model_key(model);
        // Same builder in another file, or a near name, first.
        let mut options = suggest(
            &model.builder,
            &snap
                .models
                .iter()
                .map(|m| m.model.builder.clone())
                .collect::<Vec<_>>(),
        )
        .into_iter()
        .filter_map(|b| {
            snap.models
                .iter()
                .find(|m| m.model.builder == b)
                .map(|m| model_key(&m.model))
        })
        .collect::<Vec<_>>();
        if options.is_empty() {
            options = suggest(&wanted, &model_names(snap));
        }
        let mut d = Diagnostic::new(
            DiagnosticCode::EBindModelMissing,
            at,
            format!("@{wanted} is not a model of this project"),
        );
        if !options.is_empty() {
            d = d.with_fix(Fix::AskUser {
                options,
                question: Some("Which model did you mean?".into()),
            });
        }
        return Err(d);
    };
    if let Some(e) = &m.build_error {
        return Err(Diagnostic::new(
            DiagnosticCode::EBuild,
            at,
            format!("@{} failed to build: {e}", model_key(model)),
        ));
    }
    m.built.as_ref().ok_or_else(|| {
        Diagnostic::new(
            DiagnosticCode::EBuild,
            at,
            format!("@{} has not been built", model_key(model)),
        )
    })
}

/// P1.25: parts by name or glob, in build order.
pub fn bind_part(
    built: &BuiltModel,
    model: &ModelRef,
    name: &PartName,
    at: &str,
) -> Result<Vec<BoundPart>, Diagnostic> {
    let names: Vec<String> = built.parts.iter().map(|p| p.name.clone()).collect();
    let bound = |p: &super::snapshot::PartInfo| BoundPart {
        model: model.clone(),
        name: p.name.clone(),
        geometry_hash: p.geometry_hash.clone(),
    };
    match name {
        PartName::Name(n) => built.part(n).map(|p| vec![bound(p)]).ok_or_else(|| {
            let mut d = Diagnostic::new(
                DiagnosticCode::EBindPartMissing,
                at,
                format!(
                    "@{}/{n} is not a part of {}",
                    model_key(model),
                    model.builder
                ),
            );
            let options = suggest(n, &names);
            if !options.is_empty() {
                d = d.with_fix(Fix::AskUser {
                    options,
                    question: Some(format!("{n:?} isn't a part. Which one did you mean?")),
                });
            }
            d
        }),
        PartName::Glob(g) => {
            let members: Vec<_> = built
                .parts
                .iter()
                .filter(|p| glob_match(g, &p.name))
                .map(bound)
                .collect();
            if members.is_empty() {
                let literal: String = g.chars().filter(|c| *c != '*' && *c != '?').collect();
                let mut d = Diagnostic::new(
                    DiagnosticCode::EBindGlobEmpty,
                    at,
                    format!(
                        "@{}/{g} matches no parts of {}",
                        model_key(model),
                        model.builder
                    ),
                );
                let options = suggest(&literal, &names);
                if !options.is_empty() {
                    d = d.with_fix(Fix::AskUser {
                        options,
                        question: Some(format!("{g:?} matches nothing. Which parts did you mean?")),
                    });
                }
                Err(d)
            } else {
                Ok(members)
            }
        }
    }
}

/// P1.26: a parameter with its value and declared bounds.
pub fn bind_param(
    built: &BuiltModel,
    model: &ModelRef,
    name: &str,
    at: &str,
) -> Result<BoundParam, Diagnostic> {
    built
        .param(name)
        .map(|p| BoundParam {
            model: model.clone(),
            name: p.name.clone(),
            value: p.value,
            min: p.min,
            max: p.max,
        })
        .ok_or_else(|| {
            let names: Vec<String> = built.params.iter().map(|p| p.name.clone()).collect();
            let mut d = Diagnostic::new(
                DiagnosticCode::EBindParamMissing,
                at,
                format!("{} has no parameter {name:?}", model.builder),
            );
            let options = suggest(name, &names);
            if !options.is_empty() {
                d = d.with_fix(Fix::AskUser {
                    options,
                    question: Some(format!(
                        "{name:?} isn't a parameter. Which one did you mean?"
                    )),
                });
            }
            d
        })
}

/// P1.27: a mechanism joint, where the model defines one.
pub fn bind_joint(
    built: &BuiltModel,
    model: &ModelRef,
    name: &str,
    at: &str,
) -> Result<BoundJoint, Diagnostic> {
    built
        .joint(name)
        .map(|j| BoundJoint {
            model: model.clone(),
            name: j.name.clone(),
            limits: j.limits,
        })
        .ok_or_else(|| {
            let names: Vec<String> = built.joints.iter().map(|j| j.name.clone()).collect();
            let mut d = Diagnostic::new(
                DiagnosticCode::EBindJointMissing,
                at,
                format!("{} has no joint {name:?}", model.builder),
            );
            let options = suggest(name, &names);
            if !options.is_empty() {
                d = d.with_fix(Fix::AskUser {
                    options,
                    question: Some(format!("{name:?} isn't a joint. Which one did you mean?")),
                });
            }
            d
        })
}

/// P1.28: the parts of the subject that are not in the moving set.
pub fn resolve_others(moving: &[BoundPart], subject_parts: &[BoundPart]) -> Vec<BoundPart> {
    subject_parts
        .iter()
        .filter(|p| !moving.contains(p))
        .cloned()
        .collect()
}

fn subject_parts(built: &BuiltModel, model: &ModelRef) -> Vec<BoundPart> {
    built
        .parts
        .iter()
        .map(|p| BoundPart {
            model: model.clone(),
            name: p.name.clone(),
            geometry_hash: p.geometry_hash.clone(),
        })
        .collect()
}

/// Members of a part-valued argument, once bound; `None` if it is not
/// part-valued or something in it failed to bind.
pub fn members_of_arg(arg: &Arg, b: &Bindings) -> Option<Vec<BoundPart>> {
    match arg {
        Arg::Part(p) => b.parts.get(&part_ref_key(p)).cloned(),
        Arg::Set(SetRef(n)) => b.sets.get(n).cloned(),
        Arg::List(items) => {
            let mut all = Vec::new();
            for i in items {
                all.extend(members_of_arg(i, b)?);
            }
            Some(all)
        }
        _ => None,
    }
}

pub fn members_of_target(t: &Target, b: &Bindings) -> Option<Vec<BoundPart>> {
    match t {
        Target::Part(p) => b.parts.get(&part_ref_key(p)).cloned(),
        Target::Set(SetRef(n)) => b.sets.get(n).cloned(),
        Target::Parts(ps) => {
            let mut all = Vec::new();
            for p in ps {
                all.extend(b.parts.get(&part_ref_key(p))?.iter().cloned());
            }
            Some(all)
        }
    }
}

/// Bind every reference of a requirement. Always returns what could be
/// resolved plus a diagnostic for everything that couldn't; compilation
/// continues past binding errors so the user sees all of them at once.
pub fn bind(req: &Requirement, snap: &WorkspaceSnapshot) -> (Bindings, Vec<Diagnostic>) {
    let mut b = Bindings::default();
    let mut out = Vec::new();

    // P1.29 chips
    for chip in parse_chips(&req.text) {
        let at = "text";
        match bind_model(snap, &chip.model, at) {
            Ok(built) => {
                if let Some(p) = &chip.part
                    && let Err(d) = bind_part(built, &chip.model, &PartName::Name(p.clone()), at)
                {
                    out.push(d.with_span(Some(chip.span)));
                }
            }
            Err(d) => out.push(d.with_span(Some(chip.span))),
        }
        b.chips.push(chip);
    }

    // Subjects: the one model a claim refers to.
    for (i, claim) in req.claims.iter().enumerate() {
        let models = refs::claim_models(req, i);
        if models.len() == 1 {
            b.subjects.insert(
                claim.id.clone(),
                models.into_iter().next().expect("one model"),
            );
        }
    }

    let mut sites = Vec::new();
    refs::walk(req, &mut |s| sites.push(s));
    for site in &sites {
        let subject = match site.claim {
            Some(c) => b.subjects.get(&req.claims[c].id).cloned(),
            None => refs::allowance_index(&site.at).and_then(|i| refs::allowance_subject(req, i)),
        };
        match site.reference {
            Ref::Model(m) => {
                if let Err(d) = bind_model(snap, m, &site.at) {
                    out.push(d);
                }
            }
            Ref::Part(p) => {
                let key = part_ref_key(p);
                match bind_model(snap, &p.model, &site.at)
                    .and_then(|built| bind_part(built, &p.model, &p.part, &site.at))
                {
                    Ok(members) => {
                        b.parts.insert(key, members);
                    }
                    Err(d) => out.push(d),
                }
            }
            Ref::Param(ParamRef { model, name }) => {
                let Some(model) = model.clone().or(subject) else {
                    out.push(Diagnostic::new(
                        DiagnosticCode::EBindParamMissing,
                        &site.at,
                        format!("can't tell which model parameter {name:?} belongs to"),
                    ));
                    continue;
                };
                match bind_model(snap, &model, &site.at)
                    .and_then(|built| bind_param(built, &model, name, &site.at))
                {
                    Ok(p) => {
                        b.params.insert(format!("{}:{name}", model_key(&model)), p);
                    }
                    Err(d) => out.push(d),
                }
            }
            Ref::Joint(j) => {
                match bind_model(snap, &j.model, &site.at)
                    .and_then(|built| bind_joint(built, &j.model, &j.name, &site.at))
                {
                    Ok(x) => {
                        b.joints
                            .insert(format!("{}:{}", model_key(&j.model), j.name), x);
                    }
                    Err(d) => out.push(d),
                }
            }
            Ref::Feature(f) => bind_feature(snap, f, &site.at, &mut b, &mut out),
            Ref::Set(_) | Ref::Others => {}
        }
    }

    // Sets resolve from their members' bindings.
    for (name, set) in &req.sets {
        let mut members = Vec::new();
        for p in &set.members {
            if let Some(m) = b.parts.get(&part_ref_key(p)) {
                for x in m {
                    if !members.contains(x) {
                        members.push(x.clone());
                    }
                }
            }
        }
        b.sets.insert(name.clone(), members);
    }

    // P1.28 others
    for (i, claim) in req.claims.iter().enumerate() {
        let uses_others = claim
            .args
            .iter()
            .any(|(k, a)| matches!(a, Arg::Others) && k != "moving");
        if !uses_others {
            continue;
        }
        let at = format!("claims[{i}].args");
        let moving = ["moving", "a"].iter().find_map(|k| claim.args.get(*k));
        let (Some(moving), Some(subject)) = (moving, b.subjects.get(&claim.id)) else {
            out.push(Diagnostic::new(
                DiagnosticCode::ETypeKind,
                at,
                "`others` needs a moving set and a subject model",
            ));
            continue;
        };
        let (Some(moving), Some(built)) = (
            members_of_arg(moving, &b),
            snap.model(subject).and_then(|m| m.built.as_ref()),
        ) else {
            continue; // the failure was already reported while binding
        };
        let all = subject_parts(built, subject);
        let others = resolve_others(&moving, &all);
        if others.is_empty() {
            out.push(Diagnostic::new(
                DiagnosticCode::EBindGlobEmpty,
                at,
                "`others` is empty: the moving set is every part of the model",
            ));
        }
        b.others.insert(claim.id.clone(), others);
    }
    (b, out)
}

fn bind_feature(
    snap: &WorkspaceSnapshot,
    f: &FeatureRef,
    at: &str,
    b: &mut Bindings,
    out: &mut Vec<Diagnostic>,
) {
    let FeatureRef::Explicit { part, name } = f else {
        out.push(Diagnostic::new(
            DiagnosticCode::EBindFeatureMissing,
            at,
            "history and geometric feature references can't be bound yet; use a name published with show(..., features=...)",
        ));
        return;
    };
    let PartName::Name(part_name) = &part.part else {
        out.push(Diagnostic::new(
            DiagnosticCode::EBindFeatureAmbiguous,
            at,
            "a feature belongs to one named part, not a glob",
        ));
        return;
    };
    let built = match bind_model(snap, &part.model, at) {
        Ok(x) => x,
        Err(d) => {
            out.push(d);
            return;
        }
    };
    let bound_part = match bind_part(built, &part.model, &part.part, at) {
        Ok(mut v) => v.remove(0),
        Err(d) => {
            out.push(d);
            return;
        }
    };
    match built
        .features
        .iter()
        .find(|x| &x.part == part_name && &x.name == name)
    {
        Some(x) => {
            b.features.insert(
                format!("{}/{part_name}#{name}", model_key(&part.model)),
                BoundFeature {
                    part: bound_part,
                    name: name.clone(),
                    entity: x.entity,
                    count: x.count,
                    hash: x.hash.clone(),
                },
            );
        }
        None => {
            let names: Vec<String> = built
                .features
                .iter()
                .filter(|x| &x.part == part_name)
                .map(|x| x.name.clone())
                .collect();
            let mut d = Diagnostic::new(
                DiagnosticCode::EBindFeatureMissing,
                at,
                format!("{part_name} has no feature {name:?}"),
            );
            let options = suggest(name, &names);
            if !options.is_empty() {
                d = d.with_fix(Fix::AskUser {
                    options,
                    question: Some(format!("{name:?} isn't a feature. Which one did you mean?")),
                });
            }
            out.push(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requirements::snapshot::*;
    use serde_json::json;

    fn m(entry: &str, builder: &str) -> ModelRef {
        ModelRef {
            entry: entry.into(),
            builder: builder.into(),
        }
    }

    fn snapshot() -> WorkspaceSnapshot {
        let part = |n: &str| PartInfo {
            name: n.into(),
            geometry_hash: format!("h-{n}"),
            source: None,
        };
        WorkspaceSnapshot {
            models: vec![
                ModelSnapshot {
                    model: m("machine.py", "machine"),
                    label: None,
                    build_error: None,
                    built: Some(BuiltModel {
                        fingerprint: "fp".into(),
                        parts: ["base", "rotor", "tower_a", "tower_b", "clamp_1"]
                            .map(part)
                            .to_vec(),
                        params: vec![ParamInfo {
                            name: "extension".into(),
                            value: Quantity::new(5.0, crate::requirements::Unit::Mm),
                            min: Some(Quantity::new(5.0, crate::requirements::Unit::Mm)),
                            max: None,
                        }],
                        joints: vec![JointInfo {
                            name: "hinge".into(),
                            limits: None,
                        }],
                        ..Default::default()
                    }),
                },
                ModelSnapshot {
                    model: m("broken.py", "broken"),
                    label: None,
                    build_error: Some("boom".into()),
                    built: None,
                },
            ],
        }
    }

    fn requirement(v: serde_json::Value) -> Requirement {
        let mut base = json!({"ir_version": 1, "id": "r", "name": "n", "text": "t", "claims": []});
        for (k, x) in v.as_object().unwrap() {
            base[k] = x.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    fn part(model: &str, name: &str) -> serde_json::Value {
        json!({"part": {"model": {"entry": format!("{model}.py"), "builder": model}, "part": {"name": name}}})
    }
    fn glob(model: &str, pat: &str) -> serde_json::Value {
        json!({"part": {"model": {"entry": format!("{model}.py"), "builder": model}, "part": {"glob": pat}}})
    }

    #[test]
    fn globs_match_like_shell_patterns() {
        assert!(glob_match("tower_*", "tower_a"));
        assert!(glob_match("*", ""));
        assert!(glob_match("c?amp", "clamp"));
        assert!(!glob_match("tower_*", "rotor"));
        assert!(glob_match("*_1", "clamp_1"));
        assert!(!glob_match("a?", "a"));
    }

    #[test]
    fn suggestions_are_close_names_nearest_first() {
        let names: Vec<String> = ["upright_near", "upright_far", "rail", "slab"]
            .map(String::from)
            .to_vec();
        assert_eq!(suggest("upright", &names), ["upright_far", "upright_near"]);
        assert_eq!(suggest("rails", &names), ["rail"]);
        assert!(suggest("zzzzzz", &names).is_empty());
    }

    #[test]
    fn chips_parse_with_spans_labels_and_parts() {
        let text = "ok @[Door](cabinet.py#cabinet/door) and @[Pcb](dev.py#dev/pcb/J1) or @[Box](a/b.py#box)";
        let chips = parse_chips(text);
        assert_eq!(chips.len(), 3);
        assert_eq!(chips[0].label, "Door");
        assert_eq!(chips[0].model, m("cabinet.py", "cabinet"));
        assert_eq!(chips[0].part.as_deref(), Some("door"));
        assert_eq!(chips[1].part.as_deref(), Some("pcb/J1"));
        assert_eq!(chips[2].model, m("a/b.py", "box"));
        assert_eq!(chips[2].part, None);
        let span = chips[0].span;
        let slice: String = text.chars().skip(span[0]).take(span[1] - span[0]).collect();
        assert_eq!(slice, "@[Door](cabinet.py#cabinet/door)");
        assert!(parse_chips("@[broken](no-hash) @[x]( ) @ [y](a#b)").is_empty());
    }

    #[test]
    fn parts_globs_and_sets_bind_in_build_order() {
        let req = requirement(json!({
            "sets": {"turn": {"members": [part("machine", "rotor")["part"].clone(), glob("machine", "tower_*")["part"].clone()]}},
            "claims": [{"id": "c1", "rule": "no_overlap", "args": {"a": {"set": "turn"}, "b": part("machine", "base")}}]}));
        let (b, d) = bind(&req, &snapshot());
        assert!(d.is_empty(), "{d:?}");
        let names: Vec<_> = b.sets["turn"].iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["rotor", "tower_a", "tower_b"]);
        assert_eq!(b.sets["turn"][0].key(), "machine.py#machine/rotor");
        assert_eq!(b.subjects["c1"], m("machine.py", "machine"));
    }

    #[test]
    fn missing_things_get_codes_paths_and_suggestions() {
        let req = requirement(
            json!({"claims": [{"id": "c1", "rule": "no_overlap", "args": {
            "a": part("machine", "rotr"), "b": glob("machine", "tower*x"),
            "c": {"param": {"name": "extensions"}},
            "d": {"joint": {"model": {"entry": "machine.py", "builder": "machine"}, "name": "hinje"}},
            "e": part("machin", "base"), "f": part("broken", "x")}}]}),
        );
        let (_, d) = bind(&req, &snapshot());
        let by: BTreeMap<_, _> = d.iter().map(|x| (x.at.as_str(), x)).collect();
        assert_eq!(
            by["claims[0].args.a"].code,
            DiagnosticCode::EBindPartMissing
        );
        assert_eq!(
            by["claims[0].args.a"].fix,
            Some(Fix::AskUser {
                options: vec!["rotor".into()],
                question: Some("\"rotr\" isn't a part. Which one did you mean?".into())
            })
        );
        assert_eq!(by["claims[0].args.b"].code, DiagnosticCode::EBindGlobEmpty);
        assert_eq!(
            by["claims[0].args.c"].code,
            DiagnosticCode::EBindParamMissing
        );
        assert_eq!(
            by["claims[0].args.d"].code,
            DiagnosticCode::EBindJointMissing
        );
        assert_eq!(
            by["claims[0].args.e"].code,
            DiagnosticCode::EBindModelMissing
        );
        assert!(
            matches!(&by["claims[0].args.e"].fix, Some(Fix::AskUser { options, .. }) if options == &["machine.py#machine".to_string()])
        );
        assert_eq!(by["claims[0].args.f"].code, DiagnosticCode::EBuild);
    }

    #[test]
    fn params_use_the_claims_subject_when_unqualified() {
        let req = requirement(
            json!({"claims": [{"id": "c1", "rule": "no_overlap", "args": {
            "a": part("machine", "base"),
            "at": {"states": {"kind": "param", "param": {"name": "extension"}, "values": {"shape": "value", "value": {"value": 5.0, "unit": "mm"}}}}}}]}),
        );
        let (b, d) = bind(&req, &snapshot());
        assert!(d.is_empty(), "{d:?}");
        let p = &b.params["machine.py#machine:extension"];
        assert_eq!(
            p.min,
            Some(Quantity::new(5.0, crate::requirements::Unit::Mm))
        );
        // No refs at all: no subject, so the parameter can't be placed.
        let req = requirement(json!({"claims": [{"id": "c1", "rule": "x", "args": {
            "at": {"states": {"kind": "param", "param": {"name": "extension"}, "values": {"shape": "value", "value": {"value": 5.0, "unit": "mm"}}}}}}]}));
        let (_, d) = bind(&req, &snapshot());
        assert_eq!(d[0].code, DiagnosticCode::EBindParamMissing);
    }

    #[test]
    fn others_is_the_subject_minus_the_moving_set() {
        let req = requirement(
            json!({"claims": [{"id": "c1", "rule": "moves_freely", "args": {
            "moving": glob("machine", "tower_*"), "against": "others"}}]}),
        );
        let (b, d) = bind(&req, &snapshot());
        assert!(d.is_empty(), "{d:?}");
        let names: Vec<_> = b.others["c1"].iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["base", "rotor", "clamp_1"]);
        let all = glob("machine", "*");
        let req = requirement(
            json!({"claims": [{"id": "c1", "rule": "moves_freely", "args": {"moving": all, "against": "others"}}]}),
        );
        let (_, d) = bind(&req, &snapshot());
        assert_eq!(d[0].code, DiagnosticCode::EBindGlobEmpty);
        let req = requirement(
            json!({"claims": [{"id": "c1", "rule": "moves_freely", "args": {"against": "others"}}]}),
        );
        let (_, d) = bind(&req, &snapshot());
        assert_eq!(d[0].code, DiagnosticCode::ETypeKind);
    }

    #[test]
    fn chips_bind_against_the_catalog_with_their_span() {
        let req =
            requirement(json!({"text": "a @[Mach](machine.py#machine/base) b @[Gone](nope.py#x)"}));
        let (b, d) = bind(&req, &snapshot());
        assert_eq!(b.chips.len(), 2);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, DiagnosticCode::EBindModelMissing);
        assert_eq!(d[0].span, Some(b.chips[1].span));
    }

    #[test]
    fn explicit_features_bind_and_others_are_deferred() {
        let mut snap = snapshot();
        snap.models[0]
            .built
            .as_mut()
            .unwrap()
            .features
            .push(FeatureInfo {
                part: "base".into(),
                name: "rim".into(),
                entity: EntityKind::Face,
                count: 2,
                hash: "fh".into(),
            });
        let feat = |kind: &str, name: &str| json!({"feature": {"kind": kind, "part": part("machine", "base")["part"], "name": name}});
        let req = requirement(json!({"claims": [{"id": "c1", "rule": "x", "args": {
            "a": feat("explicit", "rim"), "b": feat("explicit", "rin"), "c": feat("history", "make/hole#1/wall")}}]}));
        let (b, d) = bind(&req, &snap);
        assert_eq!(b.features["machine.py#machine/base#rim"].count, 2);
        assert_eq!(d.len(), 2);
        assert!(
            d.iter()
                .all(|x| x.code == DiagnosticCode::EBindFeatureMissing)
        );
    }
}
