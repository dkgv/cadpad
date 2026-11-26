//! Walks every reference in a requirement with its JSON path, so binding,
//! typechecking and coverage share one traversal.
use super::ir::{
    Arg, AxisRef, FeatureRef, JointRef, ModelRef, ParamRef, PartRef, Requirement, SetRef,
    StateSpace, Target,
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug)]
pub enum Ref<'a> {
    Model(&'a ModelRef),
    Part(&'a PartRef),
    Set(&'a SetRef),
    Param(&'a ParamRef),
    Joint(&'a JointRef),
    Feature(&'a FeatureRef),
    Others,
}

/// A reference and where it is. `claim` is the index of the enclosing claim.
#[derive(Clone, Debug)]
pub struct Site<'a> {
    pub at: String,
    pub claim: Option<usize>,
    pub reference: Ref<'a>,
}

type Sink<'a, 'b> = &'b mut dyn FnMut(Site<'a>);

fn target<'a>(t: &'a Target, at: &str, claim: Option<usize>, f: Sink<'a, '_>) {
    match t {
        Target::Part(p) => f(Site {
            at: at.into(),
            claim,
            reference: Ref::Part(p),
        }),
        Target::Set(s) => f(Site {
            at: at.into(),
            claim,
            reference: Ref::Set(s),
        }),
        Target::Parts(ps) => {
            for (i, p) in ps.iter().enumerate() {
                f(Site {
                    at: format!("{at}[{i}]"),
                    claim,
                    reference: Ref::Part(p),
                });
            }
        }
    }
}

fn axis<'a>(a: &'a AxisRef, at: &str, claim: Option<usize>, f: Sink<'a, '_>) {
    if let AxisRef::Feature(feat) = a {
        feature(feat, at, claim, f);
    }
}

fn feature<'a>(feat: &'a FeatureRef, at: &str, claim: Option<usize>, f: Sink<'a, '_>) {
    f(Site {
        at: at.into(),
        claim,
        reference: Ref::Feature(feat),
    });
}

fn states<'a>(s: &'a StateSpace, at: &str, claim: Option<usize>, f: Sink<'a, '_>) {
    match s {
        StateSpace::Rest {} => {}
        StateSpace::Param { param, .. } => f(Site {
            at: format!("{at}.param"),
            claim,
            reference: Ref::Param(param),
        }),
        StateSpace::Rotate {
            moving, axis: a, ..
        } => {
            target(moving, &format!("{at}.moving"), claim, f);
            axis(a, &format!("{at}.axis"), claim, f);
        }
        StateSpace::Translate {
            moving, direction, ..
        } => {
            target(moving, &format!("{at}.moving"), claim, f);
            axis(direction, &format!("{at}.direction"), claim, f);
        }
        StateSpace::Joint { joint, .. } => f(Site {
            at: format!("{at}.joint"),
            claim,
            reference: Ref::Joint(joint),
        }),
        StateSpace::Simulate { model, .. } => f(Site {
            at: format!("{at}.model"),
            claim,
            reference: Ref::Model(model),
        }),
        StateSpace::Product { factors } => {
            for (i, x) in factors.iter().enumerate() {
                states(x, &format!("{at}.factors[{i}]"), claim, f);
            }
        }
    }
}

fn arg<'a>(a: &'a Arg, at: &str, claim: Option<usize>, f: Sink<'a, '_>) {
    match a {
        Arg::Model(m) => f(Site {
            at: at.into(),
            claim,
            reference: Ref::Model(m),
        }),
        Arg::Part(p) => f(Site {
            at: at.into(),
            claim,
            reference: Ref::Part(p),
        }),
        Arg::Set(s) => f(Site {
            at: at.into(),
            claim,
            reference: Ref::Set(s),
        }),
        Arg::Others => f(Site {
            at: at.into(),
            claim,
            reference: Ref::Others,
        }),
        Arg::Param(p) => f(Site {
            at: at.into(),
            claim,
            reference: Ref::Param(p),
        }),
        Arg::Joint(j) => f(Site {
            at: at.into(),
            claim,
            reference: Ref::Joint(j),
        }),
        Arg::Feature(x) => feature(x, at, claim, f),
        Arg::Axis(x) => axis(x, at, claim, f),
        Arg::States(s) => states(s, at, claim, f),
        Arg::List(items) => {
            for (i, x) in items.iter().enumerate() {
                arg(x, &format!("{at}[{i}]"), claim, f);
            }
        }
        Arg::Quantity(_)
        | Arg::Number(_)
        | Arg::Text(_)
        | Arg::Token(_)
        | Arg::Bool(_)
        | Arg::Compare(_) => {}
    }
}

/// Every reference in the requirement, in document order: sets, covers,
/// claims, allowances.
pub fn walk<'a>(req: &'a Requirement, f: Sink<'a, '_>) {
    for (name, set) in &req.sets {
        for (i, p) in set.members.iter().enumerate() {
            f(Site {
                at: format!("sets.{name}.members[{i}]"),
                claim: None,
                reference: Ref::Part(p),
            });
        }
    }
    for (i, c) in req.covers.iter().enumerate() {
        f(Site {
            at: format!("covers[{i}].model"),
            claim: None,
            reference: Ref::Model(&c.model),
        });
        f(Site {
            at: format!("covers[{i}].part"),
            claim: None,
            reference: Ref::Part(&c.part),
        });
    }
    for (i, claim) in req.claims.iter().enumerate() {
        for (name, a) in &claim.args {
            arg(a, &format!("claims[{i}].args.{name}"), Some(i), f);
        }
    }
    for (i, a) in req.allow.iter().enumerate() {
        for (j, t) in a.pair.iter().enumerate() {
            target(t, &format!("allow[{i}].pair[{j}]"), None, f);
        }
        if let Some(s) = &a.at {
            states(s, &format!("allow[{i}].at"), None, f);
        }
    }
}

/// Models a claim refers to, directly or through named sets. A claim is
/// evaluated in one subject model, so more than one is a type error.
pub fn claim_models(req: &Requirement, claim: usize) -> BTreeSet<ModelRef> {
    let mut models = BTreeSet::new();
    let mut add = |site: Site| {
        if site.claim != Some(claim) {
            return;
        }
        match site.reference {
            Ref::Model(m) => {
                models.insert(m.clone());
            }
            Ref::Part(p) => {
                models.insert(p.model.clone());
            }
            Ref::Set(SetRef(name)) => {
                for p in req
                    .sets
                    .get(name)
                    .map(|s| s.members.as_slice())
                    .unwrap_or(&[])
                {
                    models.insert(p.model.clone());
                }
            }
            Ref::Param(p) => {
                if let Some(m) = &p.model {
                    models.insert(m.clone());
                }
            }
            Ref::Joint(j) => {
                models.insert(j.model.clone());
            }
            Ref::Feature(FeatureRef::Explicit { part, .. })
            | Ref::Feature(FeatureRef::History { part, .. })
            | Ref::Feature(FeatureRef::Geometric { part, .. }) => {
                models.insert(part.model.clone());
            }
            Ref::Others => {}
        }
    };
    walk(req, &mut add);
    models
}

/// The model an allowance is about: the model of the first part its pair names.
pub fn allowance_subject(req: &Requirement, allowance: usize) -> Option<ModelRef> {
    let a = req.allow.get(allowance)?;
    a.pair.iter().find_map(|t| match t {
        Target::Part(p) => Some(p.model.clone()),
        Target::Parts(ps) => ps.first().map(|p| p.model.clone()),
        Target::Set(SetRef(n)) => req.sets.get(n)?.members.first().map(|p| p.model.clone()),
    })
}

/// Index `i` of a path starting `allow[i]`.
pub fn allowance_index(at: &str) -> Option<usize> {
    at.strip_prefix("allow[")?.split(']').next()?.parse().ok()
}
