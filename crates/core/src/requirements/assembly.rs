//! Assemblies (spec §3.2): verifying that an `@` chip's covered assembly part
//! really comes from the tagged model, and the claims a requirement inherits
//! from how that assembly was positioned (P2.18–P2.20).
//!
//! Three jobs, all deterministic and pure:
//!
//! * **Chip mapping** (`E_BIND_CHIP_MISMATCH`): the covered part must record
//!   the tagged `entry#builder` as its origin, and any parameter the covering
//!   model and the tagged model share must have the same value.
//! * **Inferred seating**: a `place()` record names the part it was placed on,
//!   so the compiler adds an inferred `seated` (or `no_overlap` for an edge
//!   target) claim for every referenced placed part. This is what makes a
//!   requirement that relies on the assembly's placement actually check it.
//! * **`W_PLACEMENT_UNVERIFIED`**: a referenced part positioned with a raw
//!   transform (`position`/`assembly`) and no contact or seating claim is not
//!   checked for where it sits; say so instead of assuming.
//!
//! Cross-model claims are a typed error in typecheck; [`cross_model_fix`]
//! answers it with an `edit_source` proposal describing the assembly the
//! assistant must generate with `place(…)`.
use super::bind::{Bindings, BoundPart, model_key, part_ref_key};
use super::diagnostics::{Diagnostic, DiagnosticCode, Fix};
use super::ir::{Arg, Claim, ModelRef, PartName, PartRef, Requirement, SetRef, Source};
use super::refs::{self, Ref};
use super::snapshot::WorkspaceSnapshot;
use std::collections::{BTreeMap, BTreeSet};

/// Rules whose expansion asserts contact or non-overlap between parts.
fn asserts_contact(claim: &Claim) -> bool {
    match claim.rule.as_str() {
        "seated" | "no_overlap" | "bolted" | "moves_freely" | "blocked" | "inside" | "clear_of"
        | "press_fit" => true,
        "measure" => matches!(
            claim.args.get("fn"),
            Some(Arg::Text(name))
                if matches!(
                    name.as_str(),
                    "interference" | "contact" | "contact_area" | "min_clearance" | "distance"
                )
        ),
        _ => false,
    }
}

/// Bound part keys a claim refers to, directly or through named sets.
fn claim_part_keys(req: &Requirement, b: &Bindings, claim: usize) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    refs::walk(req, &mut |site| {
        if site.claim != Some(claim) {
            return;
        }
        match site.reference {
            Ref::Part(p) => keys.extend(
                b.parts
                    .get(&part_ref_key(p))
                    .into_iter()
                    .flatten()
                    .map(BoundPart::key),
            ),
            Ref::Set(SetRef(name)) => {
                keys.extend(b.sets.get(name).into_iter().flatten().map(BoundPart::key))
            }
            _ => {}
        }
    });
    keys
}

/// Part keys referenced by a claim, with the first claim argument path that
/// names each one (used to place `W_PLACEMENT_UNVERIFIED`).
fn claim_part_sites(req: &Requirement, b: &Bindings) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (i, _) in req.claims.iter().enumerate() {
        refs::walk(req, &mut |site| {
            if site.claim != Some(i) {
                return;
            }
            let members: &[BoundPart] = match site.reference {
                Ref::Part(p) => b
                    .parts
                    .get(&part_ref_key(p))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                Ref::Set(SetRef(name)) => b.sets.get(name).map(Vec::as_slice).unwrap_or(&[]),
                _ => return,
            };
            for part in members {
                out.entry(part.key()).or_insert_with(|| site.at.clone());
            }
        });
    }
    out
}

/// Every part the requirement resolves, with the first IR path that names it.
fn referenced_parts(req: &Requirement, b: &Bindings) -> BTreeMap<String, (String, BoundPart)> {
    let mut out = BTreeMap::new();
    refs::walk(req, &mut |site| {
        let members: &[BoundPart] = match site.reference {
            Ref::Part(p) => b
                .parts
                .get(&part_ref_key(p))
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            Ref::Set(SetRef(name)) => b.sets.get(name).map(Vec::as_slice).unwrap_or(&[]),
            _ => return,
        };
        for part in members {
            out.entry(part.key())
                .or_insert_with(|| (site.at.clone(), part.clone()));
        }
    });
    out
}

/// Parts referenced by at least one claim (not only by a set, cover or
/// allowance): those are the placements the requirement relies on.
fn claim_referenced_parts(req: &Requirement, b: &Bindings) -> BTreeSet<String> {
    (0..req.claims.len())
        .flat_map(|i| claim_part_keys(req, b, i))
        .collect()
}

/// Diagnostic for a chip whose covered part does not come from the tagged
/// model (spec §3.2). `label` is the chip's label when one maps to the model.
fn chip_mismatch(
    at: &str,
    label: Option<&str>,
    covered: &BoundPart,
    message: String,
    fix: Fix,
) -> Diagnostic {
    let what = match label {
        Some(label) => format!("@{label}"),
        None => model_key(&covered.model),
    };
    Diagnostic::new(
        DiagnosticCode::EBindChipMismatch,
        at,
        format!("{what} → {}: {message}", covered.key()),
    )
    .with_fix(fix)
}

/// Verify every `covers` mapping against the build: the covered part must
/// record the tagged model as its origin, and shared parameters must agree.
pub fn verify_covers(req: &Requirement, b: &Bindings, snap: &WorkspaceSnapshot) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (i, cover) in req.covers.iter().enumerate() {
        let at = format!("covers[{i}].part");
        let label = b
            .chips
            .iter()
            .find(|c| c.model == cover.model)
            .map(|c| c.label.as_str());
        let Some(members) = b.parts.get(&part_ref_key(&cover.part)) else {
            continue; // Binding already reported the missing covered part.
        };
        let Some(built) = snap.model(&cover.part.model).and_then(|m| m.built.as_ref()) else {
            continue;
        };
        let tagged = snap.model(&cover.model).and_then(|m| m.built.as_ref());
        for part in members {
            let origin = built
                .placements
                .get(&part.name)
                .and_then(|placement| placement.origin.as_ref());
            match origin {
                Some(origin)
                    if origin.entry == cover.model.entry
                        && origin.builder == cover.model.builder => {}
                Some(origin) => out.push(chip_mismatch(
                    &at,
                    label,
                    part,
                    format!(
                        "it was published from {}, not {}; the assembly no longer checks the \
                         tagged model",
                        model_key(&ModelRef {
                            entry: origin.entry.clone(),
                            builder: origin.builder.clone(),
                        }),
                        model_key(&cover.model)
                    ),
                    Fix::EditSource {
                        description: format!(
                            "In {}: publish {} with origin=\"{}\" so @{} is verified against {}.",
                            cover.part.model.entry,
                            part.name,
                            model_key(&cover.model),
                            label.unwrap_or(&cover.model.entry),
                            model_key(&cover.model)
                        ),
                        path: Some(cover.part.model.entry.clone()),
                        proposed: Some(format!("origin=\"{}\"", model_key(&cover.model))),
                    },
                )),
                None => out.push(chip_mismatch(
                    &at,
                    label,
                    part,
                    format!(
                        "it records no source builder, so it cannot be verified against {}",
                        model_key(&cover.model)
                    ),
                    Fix::EditSource {
                        description: format!(
                            "In {}: add origin=\"{}\" to the show(...) call publishing {} so @{} is verified against {}.",
                            cover.part.model.entry,
                            model_key(&cover.model),
                            part.name,
                            label.unwrap_or(&cover.model.entry),
                            model_key(&cover.model)
                        ),
                        path: Some(cover.part.model.entry.clone()),
                        proposed: Some(format!("origin=\"{}\"", model_key(&cover.model))),
                    },
                )),
            }
        }
        // Parameters the covering model declares as well must agree, or the
        // assembly checks a different value than the tagged model.
        if let Some(tagged) = tagged {
            for param in &tagged.params {
                let Some(other) = built.param(&param.name) else {
                    continue;
                };
                if (other.value.internal_value() - param.value.internal_value()).abs() > 1e-9 {
                    out.push(Diagnostic::new(
                        DiagnosticCode::EBindChipMismatch,
                        &at,
                        format!(
                            "{} declares {} = {}, but {} builds with {}; the assembly checks a \
                             different value than the tagged model",
                            model_key(&cover.part.model),
                            param.name,
                            other.value,
                            model_key(&cover.model),
                            param.value
                        ),
                    ));
                }
            }
        }
    }
    out
}

/// An inferred `seated`/`no_overlap` claim for one placed pair.
fn placement_claim(
    id: String,
    placed: &BoundPart,
    target: &BoundPart,
    kind: &str,
    note: String,
) -> Claim {
    let part = |model: &ModelRef, name: &str| {
        Arg::Part(PartRef {
            model: model.clone(),
            part: PartName::Name(name.to_string()),
        })
    };
    let (rule, args) = if kind == "edge" {
        (
            "no_overlap",
            BTreeMap::from([
                ("a".to_string(), part(&placed.model, &placed.name)),
                ("b".to_string(), part(&target.model, &target.name)),
            ]),
        )
    } else {
        (
            "seated",
            BTreeMap::from([
                ("a".to_string(), part(&placed.model, &placed.name)),
                ("on".to_string(), part(&target.model, &target.name)),
            ]),
        )
    };
    Claim {
        id,
        rule: rule.into(),
        args,
        quantifier: Default::default(),
        span: None,
        source: Source::Inferred,
        note: Some(note),
    }
}

/// The claims and diagnostics an assembly contributes: inferred seating for
/// referenced `place()`ments, and `W_PLACEMENT_UNVERIFIED` for referenced
/// parts positioned by raw transforms with no contact claim.
pub struct AssemblyReport {
    pub claims: Vec<Claim>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn assembly_report(
    req: &Requirement,
    b: &Bindings,
    snap: &WorkspaceSnapshot,
) -> AssemblyReport {
    let referenced = referenced_parts(req, b);
    let claim_sites = claim_part_sites(req, b);
    let relied_on = claim_referenced_parts(req, b);
    let mut claims = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen_pairs = BTreeSet::new();

    // Pairs a claim already relates: never add a second, inferred, claim.
    let mut related: Vec<(BTreeSet<String>, bool)> = (0..req.claims.len())
        .map(|i| (claim_part_keys(req, b, i), asserts_contact(&req.claims[i])))
        .collect();

    let mut inferred: Vec<(String, BoundPart, BoundPart, String)> = Vec::new();
    for model in &snap.models {
        let Some(built) = &model.built else { continue };
        for (name, placement) in &built.placements {
            if placement.method != "place" {
                continue;
            }
            let placed_key = format!("{}/{name}", model_key(&model.model));
            // The requirement only relies on placements of parts it checks.
            if !relied_on.contains(&placed_key) {
                continue;
            }
            let Some((_, placed)) = referenced.get(&placed_key) else {
                continue;
            };
            let Some(on) = &placement.on else { continue };
            let Some(target_name) = on.part.as_deref().filter(|t| *t != name) else {
                continue;
            };
            // The target need not be referenced elsewhere: placing the part
            // on it is what the requirement relies on.
            let Some(target_part) = built.part(target_name) else {
                continue;
            };
            let target = BoundPart {
                model: model.model.clone(),
                name: target_name.to_string(),
                geometry_hash: target_part.geometry_hash.clone(),
            };
            let target_key = target.key();
            if !seen_pairs.insert((placed_key.clone(), target_key.clone()))
                || related
                    .iter()
                    .any(|(keys, _)| keys.contains(&placed_key) && keys.contains(&target_key))
            {
                continue;
            }
            inferred.push((placed_key, placed.clone(), target, on.kind.clone()));
        }
    }

    let mut used: BTreeSet<String> = req.claims.iter().map(|c| c.id.clone()).collect();
    let mut next = 1usize;
    for (placed_key, placed, target, kind) in inferred {
        let id = loop {
            let candidate = format!("placement{next}");
            next += 1;
            if used.insert(candidate.clone()) {
                break candidate;
            }
        };
        related.push((BTreeSet::from([placed_key, target.key()]), true));
        claims.push(placement_claim(
            id,
            &placed,
            &target,
            &kind,
            format!(
                "positioned by the assembly model: {} is placed on {}",
                placed.name, target.name
            ),
        ));
    }

    // W_PLACEMENT_UNVERIFIED: raw transforms the requirement relies on.
    for (key, (at, part)) in &referenced {
        if !relied_on.contains(key) {
            continue;
        }
        let Some(built) = snap.model(&part.model).and_then(|m| m.built.as_ref()) else {
            continue;
        };
        let Some(placement) = built.placements.get(&part.name) else {
            continue;
        };
        if !matches!(placement.method.as_str(), "position" | "assembly") {
            continue;
        }
        if related
            .iter()
            .any(|(keys, contact)| *contact && keys.contains(key))
        {
            continue;
        }
        let at = claim_sites.get(key).unwrap_or(at);
        diagnostics.push(Diagnostic::new(
            DiagnosticCode::WPlacementUnverified,
            at.clone(),
            format!(
                "{} is positioned with a raw {} transform and no seating or contact claim; \
                 anything checked about it assumes the placement is right",
                key, placement.method
            ),
        ));
    }

    AssemblyReport {
        claims,
        diagnostics,
    }
}

/// The `edit_source` fix for a claim that spans several models: the assistant
/// must generate an assembly that publishes both builders' parts with their
/// `origin` and positions them with `place(…)`.
pub fn cross_model_fix(req: &Requirement, claim: usize, snap: &WorkspaceSnapshot) -> Option<Fix> {
    let models: Vec<ModelRef> = refs::claim_models(req, claim).into_iter().collect();
    if models.len() < 2 {
        return None;
    }
    let mut described: Vec<(ModelRef, Vec<String>, String)> = Vec::new();
    for model in &models {
        let built = snap.model(model).and_then(|m| m.built.as_ref());
        let parts = built
            .map(|b| b.parts.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default();
        let features = built
            .map(|b| {
                b.features
                    .iter()
                    .map(|f| format!("{}#{}", f.part, f.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        described.push((model.clone(), parts, features));
    }
    let listing = described
        .iter()
        .map(|(model, parts, features)| {
            let parts = if parts.is_empty() {
                "no published parts".to_string()
            } else {
                parts.join(", ")
            };
            if features.is_empty() {
                format!("{}: {parts}", model_key(model))
            } else {
                format!("{}: {parts} (features: {features})", model_key(model))
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    let mut skeleton = String::from(
        "# Assembly generated so one claim can check parts from separate models.\n\
         # 1. Publish each tagged model's parts from this entry, recording its builder:\n",
    );
    for (model, parts, features) in &described {
        if parts.is_empty() {
            skeleton.push_str(&format!("#    {}: no published parts\n", model_key(model)));
            continue;
        }
        skeleton.push_str(&format!(
            "#    {} parts: {}\n",
            model_key(model),
            parts.join(", ")
        ));
        if !features.is_empty() {
            skeleton.push_str(&format!("#      features: {features}\n"));
        }
    }
    let base = &described[0];
    for (model, parts, _) in described.iter().skip(1) {
        let part = parts.first().map(String::as_str).unwrap_or("<part>");
        skeleton.push_str(&format!(
            "# 2. Place {}'s parts on the base and show the result:\n\
             #    show(place({part}, on=\"<{} part>#<feature>\", align=\"center\",\n\
             #               origin=\"{}\"), name={part:?}, origin=\"{}\")",
            model_key(model),
            model_key(&base.0),
            model_key(model),
            model_key(model)
        ));
        for extra in parts.iter().skip(1) {
            skeleton.push_str(&format!(
                "\n#    show({extra}, name={extra:?}, origin=\"{}\")",
                model_key(model)
            ));
        }
        skeleton.push('\n');
    }
    skeleton.push_str(
        "# 3. Point the claim at the assembly's parts (@[Label](assembly.py#builder/part)).\n",
    );
    Some(Fix::EditSource {
        description: format!(
            "This claim refers to more than one model, so it needs an assembly that places them \
             together. Create a model entry that publishes the parts of {listing}, each with \
             origin=\"entry.py#builder\", positions the interacting parts with \
             place(part, on=\"<other part>#<feature>\", align=\"center\", origin=\"entry.py#builder\") \
             (the placement helper is available as place/context.place), then retarget the claim to \
             the assembly's parts. Inferred seating claims then check the placement."
        ),
        path: None,
        proposed: Some(skeleton),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requirements::bind::bind;
    use crate::requirements::ir::Requirement;
    use crate::requirements::snapshot::{BuiltModel, ModelSnapshot, PlacementInfo};
    use serde_json::json;

    fn model(entry: &str, builder: &str) -> ModelRef {
        ModelRef {
            entry: entry.into(),
            builder: builder.into(),
        }
    }

    fn snapshot(parts: &[&str], placements: &[(&str, &str, &str, &str)]) -> WorkspaceSnapshot {
        let mut built = BuiltModel {
            fingerprint: "fp".into(),
            ..Default::default()
        };
        for name in parts {
            built.parts.push(crate::requirements::snapshot::PartInfo {
                name: (*name).into(),
                geometry_hash: format!("h-{name}"),
                source: None,
                fastener: None,
                source_assets: vec![],
            });
        }
        for (name, method, target, origin) in placements {
            let (entry, builder) = origin.split_once('#').unwrap();
            built.placements.insert(
                (*name).into(),
                PlacementInfo {
                    method: (*method).into(),
                    on: Some(crate::requirements::snapshot::PlacementOn {
                        kind: "part".into(),
                        part: Some((*target).into()),
                        feature: None,
                    }),
                    align: Some("center".into()),
                    offset_mm: Some(0.0),
                    mate: None,
                    origin: Some(crate::requirements::snapshot::PlacementOrigin {
                        entry: entry.into(),
                        builder: builder.into(),
                    }),
                    extra: Default::default(),
                },
            );
        }
        WorkspaceSnapshot {
            models: vec![ModelSnapshot {
                model: model("device.py", "device"),
                label: None,
                build_error: None,
                built: Some(built),
            }],
        }
    }

    /// A requirement that measures the placed part without asserting any
    /// contact, so the placement pair (lid on box) is not already claimed.
    fn requirement(extra: serde_json::Value) -> Requirement {
        let mut base = json!({
            "ir_version": 1, "id": "r", "name": "n", "text": "lid volume",
            "claims": [{"id": "c1", "rule": "measure", "args": {
                "fn": {"text": "volume"},
                "a": {"part": {"model": {"entry": "device.py", "builder": "device"},
                                "part": {"name": "lid"}}},
                "cmp": {"compare": {"op": ">=", "value": {"value": 1.0, "unit": "mm3"}}}}}]
        });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn a_referenced_placement_adds_an_inferred_seated_claim() {
        let snap = snapshot(
            &["base", "box", "lid"],
            &[
                ("base", "source", "base", "enclosure.py#base"),
                ("box", "source", "box", "enclosure.py#box"),
                ("lid", "place", "box", "enclosure.py#lid"),
            ],
        );
        let req = requirement(json!({}));
        let (b, d) = bind(&req, &snap);
        assert!(d.is_empty(), "{d:?}");
        let report = assembly_report(&req, &b, &snap);
        assert_eq!(report.claims.len(), 1);
        let claim = &report.claims[0];
        assert_eq!(claim.id, "placement1");
        assert_eq!(claim.rule, "seated");
        assert_eq!(claim.source, Source::Inferred);
        assert!(claim.span.is_none());
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    }

    #[test]
    fn an_existing_contact_claim_suppresses_the_inferred_one() {
        let snap = snapshot(
            &["base", "box", "lid"],
            &[
                ("base", "source", "base", "enclosure.py#base"),
                ("box", "source", "box", "enclosure.py#box"),
                ("lid", "place", "box", "enclosure.py#lid"),
            ],
        );
        // The requirement already checks the same pair.
        let req = requirement(json!({}));
        let (b, _) = bind(&req, &snap);
        let report = assembly_report(&req, &b, &snap);
        assert_eq!(report.claims.len(), 1);
        let mut twice = req.clone();
        twice.claims.extend(report.claims);
        let (b, _) = bind(&twice, &snap);
        assert!(assembly_report(&twice, &b, &snap).claims.is_empty());
    }

    #[test]
    fn raw_transform_placement_without_a_contact_claim_warns() {
        let snap = snapshot(
            &["base", "box", "lid"],
            &[
                ("base", "source", "base", "enclosure.py#base"),
                ("box", "source", "box", "enclosure.py#box"),
                ("lid", "position", "box", "enclosure.py#lid"),
            ],
        );
        let req = requirement(json!({}));
        let (b, _) = bind(&req, &snap);
        let report = assembly_report(&req, &b, &snap);
        assert!(report.claims.is_empty());
        let d = report
            .diagnostics
            .iter()
            .find(|d| d.code == DiagnosticCode::WPlacementUnverified)
            .expect("placement warning");
        assert!(d.message.contains("lid"), "{}", d.message);
        assert_eq!(d.at, "claims[0].args.a");
    }

    #[test]
    fn a_seating_claim_verifies_the_raw_transform() {
        let snap = snapshot(
            &["base", "box", "lid"],
            &[
                ("base", "source", "base", "enclosure.py#base"),
                ("box", "source", "box", "enclosure.py#box"),
                ("lid", "position", "box", "enclosure.py#lid"),
            ],
        );
        let mut req = requirement(json!({}));
        req.claims.push(
            serde_json::from_value(json!({"id": "c2", "rule": "seated", "args": {
                "a": {"part": {"model": {"entry": "device.py", "builder": "device"},
                                "part": {"name": "lid"}}},
                "on": {"part": {"model": {"entry": "device.py", "builder": "device"},
                                "part": {"name": "box"}}}}}))
            .unwrap(),
        );
        let (b, _) = bind(&req, &snap);
        assert!(
            assembly_report(&req, &b, &snap)
                .diagnostics
                .iter()
                .all(|d| d.code != DiagnosticCode::WPlacementUnverified)
        );
    }

    #[test]
    fn a_cover_must_record_the_tagged_builder() {
        let snap = snapshot(&["lid"], &[("lid", "place", "base", "other.py#lid")]);
        let mut req = requirement(json!({}));
        req.covers.push(crate::requirements::ir::Cover {
            model: model("enclosure.py", "enclosure"),
            part: PartRef {
                model: model("device.py", "device"),
                part: PartName::Name("lid".into()),
            },
        });
        let (b, _) = bind(&req, &snap);
        let d = verify_covers(&req, &b, &snap);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, DiagnosticCode::EBindChipMismatch);
        assert_eq!(d[0].at, "covers[0].part");
        assert!(matches!(d[0].fix, Some(Fix::EditSource { .. })));
        assert!(d[0].message.contains("other.py#lid"), "{}", d[0].message);
    }

    #[test]
    fn a_cover_without_recorded_origin_is_a_mismatch() {
        let mut snap = snapshot(&["lid"], &[]);
        snap.models[0].built.as_mut().unwrap().placements.insert(
            "lid".into(),
            PlacementInfo {
                method: "position".into(),
                on: None,
                align: None,
                offset_mm: None,
                mate: None,
                origin: None,
                extra: Default::default(),
            },
        );
        let mut req = requirement(json!({}));
        req.covers.push(crate::requirements::ir::Cover {
            model: model("enclosure.py", "enclosure"),
            part: PartRef {
                model: model("device.py", "device"),
                part: PartName::Name("lid".into()),
            },
        });
        let (b, _) = bind(&req, &snap);
        let d = verify_covers(&req, &b, &snap);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(
            d[0].message.contains("no source builder"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn a_verified_cover_passes() {
        let snap = snapshot(&["lid"], &[("lid", "place", "box", "enclosure.py#lid")]);
        let mut req = requirement(json!({}));
        req.covers.push(crate::requirements::ir::Cover {
            model: model("enclosure.py", "lid_maker"),
            part: PartRef {
                model: model("device.py", "device"),
                part: PartName::Name("lid".into()),
            },
        });
        let (b, _) = bind(&req, &snap);
        // The record names enclosure.py#lid, not #lid_maker.
        assert_eq!(verify_covers(&req, &b, &snap).len(), 1);
        req.covers[0].model.builder = "lid".into();
        assert!(verify_covers(&req, &b, &snap).is_empty());
    }

    #[test]
    fn a_cross_model_claim_gets_a_place_based_assembly_fix() {
        let mut req = requirement(json!({}));
        // Point the second argument at another model.
        req.claims[0].args.insert(
            "b".into(),
            serde_json::from_value(json!({"part": {
                "model": {"entry": "box.py", "builder": "box"},
                "part": {"name": "box"}}}))
            .unwrap(),
        );
        let snap = snapshot(&["box", "lid"], &[]);
        let fix = cross_model_fix(&req, 0, &snap).expect("cross-model fix");
        match fix {
            Fix::EditSource {
                description,
                proposed,
                ..
            } => {
                assert!(description.contains("place("), "{description}");
                assert!(description.contains("box.py#box"), "{description}");
                let proposed = proposed.expect("skeleton");
                assert!(proposed.contains("show(place("), "{proposed}");
                assert!(proposed.contains("box.py#box"), "{proposed}");
            }
            other => panic!("expected edit_source, got {other:?}"),
        }
        assert!(cross_model_fix(&req, 1, &snap).is_none());
    }

    #[test]
    fn generated_ids_do_not_collide_with_authored_ones() {
        let snap = snapshot(
            &["base", "box", "lid"],
            &[
                ("base", "source", "base", "enclosure.py#base"),
                ("box", "place", "base", "enclosure.py#box"),
                ("lid", "place", "base", "enclosure.py#lid"),
            ],
        );
        let mut req = requirement(json!({}));
        req.claims[0].id = "placement1".into();
        // Reference the placed parts without another related pair.
        req.claims[0].args.insert(
            "b".into(),
            serde_json::from_value(json!({"part": {
                "model": {"entry": "device.py", "builder": "device"},
                "part": {"name": "lid"}}}))
            .unwrap(),
        );
        req.claims.push(
            serde_json::from_value(json!({"id": "c2", "rule": "measure", "args": {
                "fn": {"text": "volume"},
                "a": {"part": {"model": {"entry": "device.py", "builder": "device"},
                                "part": {"name": "box"}}},
                "cmp": {"compare": {"op": ">=", "value": {"value": 1.0, "unit": "mm3"}}}}}))
            .unwrap(),
        );
        let (b, d) = bind(&req, &snap);
        assert!(d.is_empty(), "{d:?}");
        let ids: Vec<String> = assembly_report(&req, &b, &snap)
            .claims
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, ["placement2", "placement3"]);
    }
}
