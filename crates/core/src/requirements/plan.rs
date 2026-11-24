//! The plan (spec §2): the exact states × measurements × pairs to run, in
//! canonical order (claim, state, pair), with allowances applied. The
//! evaluator executes it and compares numbers; every decision about *what* to
//! check is made here and is visible.
use super::bind::{Bindings, BoundPart, members_of_arg};
use super::diagnostics::{Diagnostic, DiagnosticCode};
use super::expand::{Origin, Pairing, PrimitiveClaim};
use super::ir::{AllowKind, Allowance, Arg, AxisRef, Comparator, Quantifier, Requirement};
use super::manifest::{Manifest, canonical_hash};
use super::quantity::Quantity;
use super::states::expand_states;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The allowance a pair is covered by at a state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Allowed {
    /// Index into the requirement's `allow`.
    pub allowance: usize,
    pub kind: AllowKind,
    pub max: Option<Quantity>,
    pub reason: String,
}

/// One measurement: at state `state` (index into the manifest's states for
/// this claim), measure `a` against `b` (part keys, sorted).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Job {
    pub state: usize,
    pub a: Vec<String>,
    pub b: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<Allowed>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimPlan {
    /// Primitive claim id.
    pub id: String,
    pub origin: Origin,
    pub primitive: String,
    pub pairing: Pairing,
    pub quantifier: Quantifier,
    pub cmp: Comparator,
    pub strict: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<AxisRef>,
    pub state_count: usize,
    /// Canonical order: state, then pair.
    pub jobs: Vec<Job>,
}

/// What the plan was built against; a run relinks and refuses to execute when
/// any of it differs (spec §6.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Requires {
    /// Part key to exact geometry hash.
    pub parts: BTreeMap<String, String>,
    /// Subject model key to build fingerprint.
    pub models: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Plan {
    pub manifest_hash: String,
    pub library_version: String,
    pub requires: Requires,
    pub claims: Vec<ClaimPlan>,
}

impl Plan {
    pub fn content_hash(&self) -> String {
        canonical_hash(self)
    }
    pub fn job_count(&self) -> usize {
        self.claims.iter().map(|c| c.jobs.len()).sum()
    }
}

fn keys(parts: &[BoundPart]) -> Vec<String> {
    let mut k: Vec<String> = parts.iter().map(BoundPart::key).collect();
    k.sort();
    k.dedup();
    k
}

fn resolve(arg: &Arg, claim_id: &str, b: &Bindings) -> Option<Vec<String>> {
    match arg {
        Arg::Others => b.others.get(claim_id).map(|o| keys(o)),
        Arg::Feature(_) => None,
        a => members_of_arg(a, b).map(|m| keys(&m)),
    }
}

fn allowance_members(a: &Allowance, side: usize, b: &Bindings) -> Vec<String> {
    super::bind::members_of_target(&a.pair[side], b)
        .map(|m| keys(&m))
        .unwrap_or_default()
}

/// Plan stage (P1.42): jobs for every primitive claim, in canonical order,
/// with allowances applied per pair and state (P1.38).
pub fn plan(
    req: &Requirement,
    prims: &[PrimitiveClaim],
    bindings: &Bindings,
    manifest: &Manifest,
) -> Result<Plan, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    let mut claims = Vec::new();
    let allow_members: Vec<(Vec<String>, Vec<String>)> = req
        .allow
        .iter()
        .map(|a| {
            (
                allowance_members(a, 0, bindings),
                allowance_members(a, 1, bindings),
            )
        })
        .collect();
    let allow_states: Vec<_> = req
        .allow
        .iter()
        .map(|a| a.at.as_ref().map(expand_states))
        .collect();
    for p in prims {
        let at = format!("claims[{}]", p.origin.claim);
        let side = |name: &str| {
            p.args
                .get(name)
                .and_then(|a| resolve(a, &p.origin.claim, bindings))
        };
        let Some(a) = side("a") else {
            errors.push(Diagnostic::new(
                DiagnosticCode::ETypeKind,
                &at,
                format!("{}: its first argument has no resolved parts", p.id),
            ));
            continue;
        };
        let b = side("b").or_else(|| side("envelope")).unwrap_or_default();
        let axis = match p.args.get("axis") {
            Some(Arg::Axis(x)) => Some(x.clone()),
            _ => None,
        };
        let states = manifest
            .states
            .get(&p.id)
            .cloned()
            .unwrap_or_else(|| expand_states(&p.states));
        // Pairs in sorted order; (x, y) and (y, x) are one pair.
        let pairs: Vec<(String, String)> = match p.pairing {
            Pairing::EachPair => {
                let mut set = BTreeSet::new();
                for x in &a {
                    for y in &b {
                        if x != y {
                            set.insert(if x < y {
                                (x.clone(), y.clone())
                            } else {
                                (y.clone(), x.clone())
                            });
                        }
                    }
                }
                set.into_iter().collect()
            }
            _ => vec![],
        };
        let unit_jobs: Vec<(Vec<String>, Vec<String>)> = match p.pairing {
            Pairing::Whole | Pairing::AnyPair => vec![(a.clone(), b.clone())],
            Pairing::Each => a.iter().map(|x| (vec![x.clone()], vec![])).collect(),
            Pairing::EachPair => pairs
                .iter()
                .map(|(x, y)| (vec![x.clone()], vec![y.clone()]))
                .collect(),
        };
        if unit_jobs.is_empty() {
            errors.push(Diagnostic::new(
                DiagnosticCode::ETypeKind,
                &at,
                format!(
                    "{} checks nothing: there is no pair of different parts to measure",
                    p.id
                ),
            ));
            continue;
        }
        let mut jobs = Vec::new();
        for (si, state) in states.iter().enumerate() {
            for (ja, jb) in &unit_jobs {
                let allowed = (p.primitive == "interference" && p.pairing == Pairing::EachPair)
                    .then(|| {
                        req.allow.iter().enumerate().find_map(|(i, al)| {
                            let (m0, m1) = &allow_members[i];
                            let pair_hit = (m0.contains(&ja[0]) && m1.contains(&jb[0]))
                                || (m1.contains(&ja[0]) && m0.contains(&jb[0]));
                            let state_hit = allow_states[i]
                                .as_ref()
                                .is_none_or(|alts| alts.iter().any(|s| state.contains(s)));
                            (pair_hit && state_hit).then(|| Allowed {
                                allowance: i,
                                kind: al.kind,
                                max: al.max.as_ref().map(|m| m.quantity()),
                                reason: al.reason.clone(),
                            })
                        })
                    });
                jobs.push(Job {
                    state: si,
                    a: ja.clone(),
                    b: jb.clone(),
                    allowed: allowed.flatten(),
                });
            }
        }
        claims.push(ClaimPlan {
            id: p.id.clone(),
            origin: p.origin.clone(),
            primitive: p.primitive.clone(),
            pairing: p.pairing,
            quantifier: p.quantifier,
            cmp: p.cmp.clone(),
            strict: p.strict,
            axis,
            state_count: states.len(),
            jobs,
        });
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut requires = Requires {
        parts: BTreeMap::new(),
        models: BTreeMap::new(),
    };
    for links in manifest
        .parts
        .values()
        .chain(manifest.sets.values())
        .chain(manifest.others.values())
    {
        for l in links {
            requires
                .parts
                .insert(l.key.clone(), l.geometry_hash.clone());
        }
    }
    for s in &manifest.subjects {
        requires
            .models
            .insert(super::bind::model_key(&s.model), s.fingerprint.clone());
    }
    Ok(Plan {
        manifest_hash: manifest.content_hash(),
        library_version: manifest.library_version.clone(),
        requires,
        claims,
    })
}

/// `W_ALLOW_UNUSED` (P1.38): allowances that no job in the plan uses. This is
/// the static half; once results exist it can be refined to allowances whose
/// pairs never actually touched.
pub fn unused_allowances(req: &Requirement, plan: &Plan) -> Vec<Diagnostic> {
    let used: BTreeSet<usize> = plan
        .claims
        .iter()
        .flat_map(|c| {
            c.jobs
                .iter()
                .filter_map(|j| j.allowed.as_ref().map(|a| a.allowance))
        })
        .collect();
    req.allow
        .iter()
        .enumerate()
        .filter(|(i, _)| !used.contains(i))
        .map(|(i, a)| {
            Diagnostic::new(
                DiagnosticCode::WAllowUnused,
                format!("allow[{i}]"),
                format!("allowance \"{}\" matches no pair at any state", a.reason),
            )
            .with_span(a.span)
        })
        .collect()
}
