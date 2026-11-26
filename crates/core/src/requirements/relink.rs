//! Drift: relink and diff (spec §6.3, P1.57-P1.58). A run rebuilds, then
//! compares what the approved manifest recorded with what the new build
//! offers, under one fixed policy:
//!
//! | difference | effect |
//! |---|---|
//! | none, or geometry only | run |
//! | reference broke, old exact hash matches exactly one entity now | relink to it, run |
//! | feature hash only, set / glob / `others` membership, parameter bounds or value, expanded states | run, "changed since approval" with the diff |
//! | reference gone or ambiguous, feature kind or count, subject or chip mapping, library or settings version | do not run: `needs_review` with the diff |
//! | a managed STEP input behind a referenced part changed | do not run: `needs_review` |
//!
//! Auto-relink is by exact hash only and only for a unique match: if the old
//! hash matches no entity, several entities, or another broken reference
//! wants it too, the reference is `gone` / `ambiguous` and needs review.
//! Nothing is matched approximately.
use super::bind::{glob_match, model_key, part_ref_key};
use super::ir::{AxisRef, FeatureRef, PartName, PartRef, Target};
use super::manifest::{FeatureLink, Manifest, MemberLink, ParamLink, PlacementLink};
use super::settings::ProjectSettings;
use super::snapshot::WorkspaceSnapshot;
use super::states::{ConcreteState, StateValue};
use super::suggestions::{ShapeSignature, Suggestion, rank};
use super::worker_plan::split_part_key;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// What the new build offers: every published part and feature with exact
/// hashes, the parameters, and which models built.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuildIndex {
    /// Every published part of every built model (`entry#builder/name`).
    pub parts: Vec<MemberLink>,
    /// `entry#builder/part#name` to feature.
    pub features: BTreeMap<String, FeatureLink>,
    /// `entry#builder:name` to parameter.
    pub params: BTreeMap<String, ParamLink>,
    /// `entry#builder` of every model that built.
    pub models: BTreeSet<String>,
    /// `entry#builder/name` to the part's shape signature (spec §3.1).
    pub part_signatures: BTreeMap<String, ShapeSignature>,
    /// `entry#builder/part#suffix` to the feature's shape signature.
    pub feature_signatures: BTreeMap<String, ShapeSignature>,
    /// `entry#builder/part` to the recorded placement.
    pub placements: BTreeMap<String, PlacementLink>,
    /// Exact managed STEP inputs for each published part.
    pub part_source_assets: BTreeMap<String, Vec<super::snapshot::SourceAssetInfo>>,
}

impl BuildIndex {
    pub fn from_snapshot(snap: &WorkspaceSnapshot) -> Self {
        let mut out = BuildIndex::default();
        let (part_signatures, feature_signatures) = super::manifest::signature_links(snap);
        out.part_signatures = part_signatures;
        out.feature_signatures = feature_signatures;
        out.placements = super::manifest::placement_links(snap);
        out.part_source_assets = super::manifest::source_asset_links(snap);
        for m in &snap.models {
            let Some(b) = &m.built else { continue };
            let mk = model_key(&m.model);
            out.models.insert(mk.clone());
            for p in &b.parts {
                out.parts.push(MemberLink {
                    key: format!("{mk}/{}", p.name),
                    geometry_hash: p.geometry_hash.clone(),
                });
            }
            for f in &b.features {
                let suffix = f.link_suffix();
                let key = format!("{mk}/{}#{suffix}", f.part);
                out.features.insert(
                    key.clone(),
                    FeatureLink {
                        entity: f.entity,
                        count: f.count,
                        hash: f.hash.clone(),
                        source: f.source,
                        positional: f.positional,
                        key_size: key.len() as u64,
                    },
                );
            }
            for p in &b.params {
                out.params.insert(
                    format!("{mk}:{}", p.name),
                    ParamLink {
                        value: p.value,
                        min: p.min,
                        max: p.max,
                    },
                );
            }
        }
        out.parts.sort();
        out
    }
}

/// What a difference does to the run, ordered by severity.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Run as approved (the difference is still listed).
    Run,
    /// Run and show "changed since approval" with the diff.
    RunChanged,
    /// Do not run: `needs_review` with the diff.
    NeedsReview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A broken reference was relinked by exact hash.
    AutoRelink,
    /// A feature's exact hash changed (kind and count the same).
    FeatureGeometry,
    SetMembership,
    GlobMembership,
    OthersMembership,
    ParamBounds,
    ParamValue,
    StatesChanged,
    ReferenceGone,
    ReferenceAmbiguous,
    FeatureKind,
    FeatureCount,
    FeatureSource,
    ParamGone,
    SubjectChanged,
    ChipRemapped,
    LibraryVersion,
    SettingsChanged,
    /// A placed part's placement method or target changed (spec §6.3).
    PlacementMethod,
    /// A managed STEP file used by a referenced part was replaced.
    ImportedFileChanged,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Change {
    pub kind: ChangeKind,
    pub effect: Effect,
    /// The reference concerned (part key, set name, `claim:others`, ...).
    pub at: String,
    /// Human-readable.
    pub message: String,
    /// Shape-signature-ranked candidates for a broken reference (spec §3.1):
    /// suggestions only, never a relink.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion<String>>,
}

/// A reference relinked to the entity with the same exact hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Relink {
    pub from: String,
    pub to: String,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RelinkReport {
    /// The worst effect of any change.
    pub decision: Effect,
    /// Relinks applied; the caller compiles with these names substituted.
    pub relinks: Vec<Relink>,
    /// Every difference that is not plain geometry, in a fixed order.
    pub changes: Vec<Change>,
    /// Parts whose geometry hash changed, same binding (the normal case).
    pub geometry_changed: Vec<String>,
    /// The fresh manifest was linked for a different IR than the approved
    /// one (prose edited): the approved version keeps running, but the
    /// requirement is `needs_review` (see [`super::lifecycle`]).
    pub requirement_changed: bool,
}

impl RelinkReport {
    pub fn runs(&self) -> bool {
        self.decision != Effect::NeedsReview
    }
    /// "changed since approval" lines for the UI, one per change that is not
    /// a silent relink.
    pub fn changed_since_approval(&self) -> Vec<&Change> {
        self.changes
            .iter()
            .filter(|c| c.effect >= Effect::RunChanged)
            .collect()
    }
    /// The whole diff as text, one line per change.
    pub fn diff_text(&self) -> String {
        self.changes
            .iter()
            .map(|c| {
                let tag = match c.effect {
                    Effect::Run => "relinked",
                    Effect::RunChanged => "changed",
                    Effect::NeedsReview => "needs review",
                };
                format!("{tag}: {}", c.message)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn push(changes: &mut Vec<Change>, kind: ChangeKind, effect: Effect, at: &str, message: String) {
    changes.push(Change {
        kind,
        effect,
        at: at.to_string(),
        message,
        suggestions: Vec::new(),
    });
}

/// Shape-ranked candidates for a reference that no longer resolves: the
/// approved signature is the query, every signature of the new build a
/// candidate. Returns an empty list when either side has no comparable
/// signature (older manifests, parts without published geometry).
fn signature_suggestions(
    query: Option<&ShapeSignature>,
    candidates: &BTreeMap<String, ShapeSignature>,
    skip: &str,
) -> Vec<Suggestion<String>> {
    let Some(query) = query else {
        return Vec::new();
    };
    rank(
        query,
        candidates
            .iter()
            .filter(|(key, _)| key.as_str() != skip)
            .map(|(k, s)| (k.clone(), s.clone())),
        3,
    )
}

fn is_glob(key: &str) -> bool {
    split_part_key(key).is_some_and(|(_, n)| n.contains('*') || n.contains('?'))
}

fn list(keys: &BTreeSet<&String>) -> String {
    keys.iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn rename_state_parts(state: &mut ConcreteState, renamed: &BTreeMap<String, String>) {
    let rename = |part: &mut PartRef| {
        if let Some((_, name)) = renamed
            .get(&part_ref_key(part))
            .and_then(|key| split_part_key(key))
        {
            part.part = PartName::Name(name.to_owned());
        }
    };
    for value in &mut state.values {
        let (moving, axis) = match value {
            StateValue::Rotate { moving, axis, .. } => (moving, axis),
            StateValue::Translate {
                moving, direction, ..
            } => (moving, direction),
            _ => continue,
        };
        match moving {
            Target::Part(part) => rename(part),
            Target::Parts(parts) => parts.iter_mut().for_each(rename),
            Target::Set(_) => {}
        }
        if let AxisRef::Feature(feature) = axis {
            match feature {
                FeatureRef::Explicit { part, .. }
                | FeatureRef::History { part, .. }
                | FeatureRef::Geometric { part, .. } => rename(part),
            }
        }
    }
}

/// Resolve unique-by-hash candidates for references that disappeared.
/// `wanted`: broken `(key, hash)`; `pool`: `(key, hash)` of everything in the
/// new build. Returns per broken key: the new key, or why not.
fn match_by_hash<'a>(
    wanted: &[(&'a String, &'a String)],
    pool: &[(&'a String, &'a String)],
) -> BTreeMap<&'a String, Result<&'a String, Vec<&'a String>>> {
    let mut out = BTreeMap::new();
    for (key, hash) in wanted {
        let claimants = wanted.iter().filter(|(_, h)| h == hash).count();
        let cands: Vec<&String> = pool
            .iter()
            .filter(|(_, h)| h == hash && !h.is_empty())
            .map(|(k, _)| *k)
            .collect();
        out.insert(
            *key,
            if cands.len() == 1 && claimants == 1 {
                Ok(cands[0])
            } else {
                Err(cands)
            },
        );
    }
    out
}

/// Relink and diff the approved manifest against the new build (P1.57,
/// P1.58). `fresh` is the manifest linked from the new build when it
/// compiled (`None` when binding failed: the checks that need it are
/// skipped, and the broken references explain why).
pub fn relink(
    approved: &Manifest,
    fresh: Option<&Manifest>,
    build: &BuildIndex,
    settings: &ProjectSettings,
) -> RelinkReport {
    let mut changes: Vec<Change> = Vec::new();
    let mut relinks: Vec<Relink> = Vec::new();
    let mut geometry_changed: Vec<String> = Vec::new();
    // Versions first: nothing below matters if the rules themselves changed.
    let lib = super::library::library_version();
    if approved.library_version != lib {
        push(
            &mut changes,
            ChangeKind::LibraryVersion,
            Effect::NeedsReview,
            "library",
            format!(
                "the rule library changed from {} to {lib}",
                approved.library_version
            ),
        );
    }
    if approved.settings_hash != settings.content_hash() {
        push(
            &mut changes,
            ChangeKind::SettingsChanged,
            Effect::NeedsReview,
            "settings",
            "the project settings changed since approval".into(),
        );
    }

    // Exact part references.
    let now: BTreeMap<&String, &String> = build
        .parts
        .iter()
        .map(|p| (&p.key, &p.geometry_hash))
        .collect();
    let pool: Vec<(&String, &String)> = now.iter().map(|(k, h)| (*k, *h)).collect();
    let mut exact: BTreeMap<&String, &String> = BTreeMap::new(); // key -> approved hash
    for (g, members) in &approved.parts {
        if !is_glob(g) {
            for m in members {
                exact.insert(&m.key, &m.geometry_hash);
            }
        }
    }
    let mut broken: Vec<(&String, &String)> = Vec::new();
    for (key, hash) in &exact {
        match now.get(key) {
            Some(h) if h != hash => geometry_changed.push((*key).clone()),
            Some(_) => {}
            None => broken.push((*key, *hash)),
        }
    }
    let matched = match_by_hash(&broken, &pool);
    let mut renamed: BTreeMap<String, String> = BTreeMap::new();
    for (key, hash) in &broken {
        match &matched[key] {
            Ok(to) => {
                renamed.insert((*key).clone(), (*to).clone());
                relinks.push(Relink {
                    from: (*key).clone(),
                    to: (*to).clone(),
                    hash: (*hash).clone(),
                });
                push(
                    &mut changes,
                    ChangeKind::AutoRelink,
                    Effect::Run,
                    key,
                    format!("{key} is now {to} (same exact geometry)"),
                );
            }
            Err(c) if c.is_empty() => {
                push(
                    &mut changes,
                    ChangeKind::ReferenceGone,
                    Effect::NeedsReview,
                    key,
                    format!("{key} no longer exists and no entity has its exact geometry"),
                );
                if let Some(change) = changes.last_mut() {
                    change.suggestions = signature_suggestions(
                        approved.part_signatures.get(*key),
                        &build.part_signatures,
                        key,
                    );
                }
            }
            Err(c) => {
                push(
                    &mut changes,
                    ChangeKind::ReferenceAmbiguous,
                    Effect::NeedsReview,
                    key,
                    format!(
                        "{key} no longer exists and its geometry matches {} entities or is claimed twice: {}",
                        c.len(),
                        c.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                );
                if let Some(change) = changes.last_mut() {
                    change.suggestions = signature_suggestions(
                        approved.part_signatures.get(*key),
                        &build.part_signatures,
                        key,
                    );
                }
            }
        }
    }

    // Replacing the managed STEP input behind a published part needs review,
    // even when OCCT produces the same geometry hash. A STEP file is the
    // reference; geometry signatures may suggest alternatives but never
    // silently preserve its approval.
    let mut referenced: BTreeSet<String> = approved.part_source_assets.keys().cloned().collect();
    for members in approved
        .parts
        .values()
        .chain(approved.sets.values())
        .chain(approved.others.values())
    {
        referenced.extend(members.iter().map(|member| member.key.clone()));
    }
    for key in referenced {
        let old = approved
            .part_source_assets
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let new = build
            .part_source_assets
            .get(&key)
            .cloned()
            .unwrap_or_default();
        if old != new && (!old.is_empty() || !new.is_empty()) {
            push(
                &mut changes,
                ChangeKind::ImportedFileChanged,
                Effect::NeedsReview,
                &key,
                format!("managed STEP input for {key} changed; this reference needs review"),
            );
            if let Some(change) = changes.last_mut() {
                change.suggestions = signature_suggestions(
                    approved.part_signatures.get(&key),
                    &build.part_signatures,
                    &key,
                );
            }
        }
    }

    // Globs, computed from the new build.
    for (g, members) in &approved.parts {
        if !is_glob(g) {
            continue;
        }
        let (model, pattern) = split_part_key(g).expect("glob key splits");
        let have: BTreeSet<&String> = members
            .iter()
            .map(|m| renamed.get(&m.key).unwrap_or(&m.key))
            .collect();
        let new: Vec<&MemberLink> = build
            .parts
            .iter()
            .filter(|p| {
                split_part_key(&p.key).is_some_and(|(m, n)| m == model && glob_match(pattern, n))
            })
            .collect();
        let want: BTreeSet<&String> = new.iter().map(|p| &p.key).collect();
        for m in members {
            let k = renamed.get(&m.key).unwrap_or(&m.key);
            if let Some(p) = new.iter().find(|p| &p.key == k)
                && p.geometry_hash != m.geometry_hash
                && !geometry_changed.contains(k)
            {
                geometry_changed.push(k.clone());
            }
        }
        if want.is_empty() {
            push(
                &mut changes,
                ChangeKind::ReferenceGone,
                Effect::NeedsReview,
                g,
                format!("{g} matches nothing in the new build"),
            );
        } else if have != want {
            let added: BTreeSet<&String> = want.difference(&have).copied().collect();
            let removed: BTreeSet<&String> = have.difference(&want).copied().collect();
            push(
                &mut changes,
                ChangeKind::GlobMembership,
                Effect::RunChanged,
                g,
                membership_text(g, &added, &removed),
            );
        }
    }

    // Features.
    let fpool: Vec<(&String, &String)> = build.features.iter().map(|(k, f)| (k, &f.hash)).collect();
    let mut fbroken: Vec<(&String, &String)> = Vec::new();
    for (key, old) in &approved.features {
        match build.features.get(key) {
            Some(new) if new.entity != old.entity => push(
                &mut changes,
                ChangeKind::FeatureKind,
                Effect::NeedsReview,
                key,
                format!("{key} was a {:?} and is now a {:?}", old.entity, new.entity),
            ),
            Some(new) if new.count != old.count => push(
                &mut changes,
                ChangeKind::FeatureCount,
                Effect::NeedsReview,
                key,
                format!(
                    "{key} selected {} entities and now selects {}",
                    old.count, new.count
                ),
            ),
            Some(new) if new.source != old.source || new.positional != old.positional => push(
                &mut changes,
                ChangeKind::FeatureSource,
                Effect::NeedsReview,
                key,
                format!("{key} changed feature naming basis"),
            ),
            Some(new) if new.hash != old.hash => push(
                &mut changes,
                ChangeKind::FeatureGeometry,
                Effect::RunChanged,
                key,
                format!("{key} changed geometry (its size or position moved)"),
            ),
            Some(_) => {}
            None => fbroken.push((key, &old.hash)),
        }
    }
    let fmatched = match_by_hash(&fbroken, &fpool);
    for (key, hash) in &fbroken {
        let old = &approved.features[*key];
        // Only a unique exact hash with the same entity kind and naming basis can stand in.
        let res = match &fmatched[key] {
            Ok(to)
                if build.features[*to].entity == old.entity
                    && build.features[*to].source == old.source
                    && build.features[*to].positional == old.positional =>
            {
                Ok(*to)
            }
            Ok(_) => Err(Vec::new()),
            Err(c) => Err(c.clone()),
        };
        match res {
            Ok(to) => {
                relinks.push(Relink {
                    from: (*key).clone(),
                    to: to.clone(),
                    hash: (*hash).clone(),
                });
                push(
                    &mut changes,
                    ChangeKind::AutoRelink,
                    Effect::Run,
                    key,
                    format!("{key} is now {to} (same exact geometry)"),
                );
            }
            Err(c) if c.is_empty() => {
                push(
                    &mut changes,
                    ChangeKind::ReferenceGone,
                    Effect::NeedsReview,
                    key,
                    format!("feature {key} no longer exists and no entity has its exact geometry"),
                );
                if let Some(change) = changes.last_mut() {
                    change.suggestions = signature_suggestions(
                        approved.feature_signatures.get(*key),
                        &build.feature_signatures,
                        key,
                    );
                }
            }
            Err(c) => {
                push(
                    &mut changes,
                    ChangeKind::ReferenceAmbiguous,
                    Effect::NeedsReview,
                    key,
                    format!(
                        "feature {key} no longer exists and its geometry is ambiguous or of another kind ({} candidates)",
                        c.len()
                    ),
                );
                if let Some(change) = changes.last_mut() {
                    change.suggestions = signature_suggestions(
                        approved.feature_signatures.get(*key),
                        &build.feature_signatures,
                        key,
                    );
                }
            }
        }
    }

    // Placements (spec §6.3): a changed method or target runs with a diff.
    let live_parts: BTreeSet<&String> = build.parts.iter().map(|p| &p.key).collect();
    for (key, old) in &approved.placements {
        match build.placements.get(key) {
            Some(new) if new != old => {
                let message = if new.method == old.method {
                    format!("placement target of {key} changed")
                } else {
                    format!(
                        "placement of {key} changed from {} to {}",
                        old.method, new.method
                    )
                };
                push(
                    &mut changes,
                    ChangeKind::PlacementMethod,
                    Effect::RunChanged,
                    key,
                    message,
                );
            }
            // A gone part is already reported by the reference checks.
            None if live_parts.contains(key) => push(
                &mut changes,
                ChangeKind::PlacementMethod,
                Effect::RunChanged,
                key,
                format!("{key} lost its placement record"),
            ),
            _ => {}
        }
    }
    for (key, new) in &build.placements {
        if !approved.placements.contains_key(key) {
            push(
                &mut changes,
                ChangeKind::PlacementMethod,
                Effect::RunChanged,
                key,
                format!("{key} gained a {} placement", new.method),
            );
        }
    }

    // Parameters.
    for (key, old) in &approved.params {
        let Some(new) = build.params.get(key) else {
            push(
                &mut changes,
                ChangeKind::ParamGone,
                Effect::NeedsReview,
                key,
                format!("parameter {key} no longer exists"),
            );
            continue;
        };
        let same = |a: &Option<super::quantity::Quantity>,
                    b: &Option<super::quantity::Quantity>| match (a, b) {
            (Some(x), Some(y)) => x.same_value(y),
            (None, None) => true,
            _ => false,
        };
        if !same(&old.min, &new.min) || !same(&old.max, &new.max) {
            let show = |q: &Option<super::quantity::Quantity>| {
                q.map(|q| q.to_string()).unwrap_or_else(|| "none".into())
            };
            push(
                &mut changes,
                ChangeKind::ParamBounds,
                Effect::RunChanged,
                key,
                format!(
                    "bounds of {key} changed from [{}, {}] to [{}, {}]",
                    show(&old.min),
                    show(&old.max),
                    show(&new.min),
                    show(&new.max)
                ),
            );
        }
        if !old.value.same_value(&new.value) {
            push(
                &mut changes,
                ChangeKind::ParamValue,
                Effect::RunChanged,
                key,
                format!(
                    "default of {key} changed from {} to {}",
                    old.value, new.value
                ),
            );
        }
    }

    // Subjects: a different builder is a different requirement.
    let subj = |m: &Manifest| -> BTreeSet<String> {
        m.subjects.iter().map(|s| model_key(&s.model)).collect()
    };
    let approved_subjects = subj(approved);
    for s in &approved_subjects {
        if !build.models.contains(s) {
            push(
                &mut changes,
                ChangeKind::SubjectChanged,
                Effect::NeedsReview,
                s,
                format!("subject model {s} is no longer built from the project"),
            );
        }
    }

    let mut requirement_changed = false;
    if let Some(f) = fresh {
        requirement_changed = f.requirement_hash != approved.requirement_hash;
        let fresh_subjects = subj(f);
        if fresh_subjects != approved_subjects {
            let a: BTreeSet<&String> = approved_subjects.iter().collect();
            let b: BTreeSet<&String> = fresh_subjects.iter().collect();
            push(
                &mut changes,
                ChangeKind::SubjectChanged,
                Effect::NeedsReview,
                "subjects",
                format!(
                    "subject models changed from [{}] to [{}]",
                    list(&a),
                    list(&b)
                ),
            );
        }
        // Chips: same label must map to the same builder, part and covered part.
        for old in &approved.chips {
            match f.chips.iter().find(|c| c.label == old.label) {
                Some(new)
                    if new.model == old.model
                        && new.part == old.part
                        && new.mapped_to.as_ref()
                            == old
                                .mapped_to
                                .as_ref()
                                .map(|key| renamed.get(key).unwrap_or(key)) => {}
                Some(new) => push(
                    &mut changes,
                    ChangeKind::ChipRemapped,
                    Effect::NeedsReview,
                    &old.label,
                    format!(
                        "@{} now maps to {}{} instead of {}{}",
                        old.label,
                        model_key(&new.model),
                        new.part
                            .as_deref()
                            .map(|p| format!("/{p}"))
                            .unwrap_or_default(),
                        model_key(&old.model),
                        old.part
                            .as_deref()
                            .map(|p| format!("/{p}"))
                            .unwrap_or_default(),
                    ),
                ),
                None => push(
                    &mut changes,
                    ChangeKind::ChipRemapped,
                    Effect::NeedsReview,
                    &old.label,
                    format!("@{} no longer maps to anything", old.label),
                ),
            }
        }
        // Sets (resolved membership, with relinks applied).
        for (name, members) in &approved.sets {
            let Some(new) = f.sets.get(name) else {
                push(
                    &mut changes,
                    ChangeKind::ReferenceGone,
                    Effect::NeedsReview,
                    name,
                    format!("set {name} no longer links"),
                );
                continue;
            };
            membership_change(
                &mut changes,
                ChangeKind::SetMembership,
                name,
                &format!("set {name}"),
                members,
                new,
                &renamed,
            );
        }
        for (claim, members) in &approved.others {
            let empty = Vec::new();
            let new = f.others.get(claim).unwrap_or(&empty);
            membership_change(
                &mut changes,
                ChangeKind::OthersMembership,
                claim,
                &format!("`others` of {claim}"),
                members,
                new,
                &renamed,
            );
        }
        if f.states != approved.states {
            let mut before = approved.states.clone();
            for state in before.values_mut().flatten() {
                rename_state_parts(state, &renamed);
            }
            let ids: BTreeSet<&String> = before
                .keys()
                .chain(f.states.keys())
                .filter(|k| before.get(*k) != f.states.get(*k))
                .collect();
            if !ids.is_empty() {
                changes.push(Change {
                    kind: ChangeKind::StatesChanged,
                    effect: Effect::RunChanged,
                    at: "states".into(),
                    message: format!("expanded states changed for {}", list(&ids)),
                    suggestions: Vec::new(),
                });
            }
        }
    }

    let decision = changes
        .iter()
        .map(|c| c.effect)
        .max()
        .unwrap_or(Effect::Run);
    geometry_changed.sort();
    geometry_changed.dedup();
    relinks.sort_by(|a, b| a.from.cmp(&b.from));
    RelinkReport {
        decision,
        relinks,
        changes,
        geometry_changed,
        requirement_changed,
    }
}

fn membership_text(what: &str, added: &BTreeSet<&String>, removed: &BTreeSet<&String>) -> String {
    let mut parts = Vec::new();
    if !added.is_empty() {
        parts.push(format!("gained {}", list(added)));
    }
    if !removed.is_empty() {
        parts.push(format!("lost {}", list(removed)));
    }
    format!("{what} {}", parts.join(" and "))
}

fn membership_change(
    changes: &mut Vec<Change>,
    kind: ChangeKind,
    at: &str,
    what: &str,
    old: &[MemberLink],
    new: &[MemberLink],
    renamed: &BTreeMap<String, String>,
) {
    let have: BTreeSet<&String> = old
        .iter()
        .map(|m| renamed.get(&m.key).unwrap_or(&m.key))
        .collect();
    let want: BTreeSet<&String> = new.iter().map(|m| &m.key).collect();
    if have == want {
        return;
    }
    let added: BTreeSet<&String> = want.difference(&have).copied().collect();
    let removed: BTreeSet<&String> = have.difference(&want).copied().collect();
    changes.push(Change {
        kind,
        effect: Effect::RunChanged,
        at: at.to_string(),
        message: membership_text(what, &added, &removed),
        suggestions: Vec::new(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(value: serde_json::Value) -> Manifest {
        serde_json::from_value(value).expect("manifest json")
    }

    #[test]
    fn broken_part_reference_offers_signature_ranked_suggestions() {
        let approved = manifest(json!({
            "requirement_hash": "r", "library_version": "lib", "settings_hash": "s",
            "subjects": [], "chips": [], "sets": {},
            "parts": {"machine.py#machine/arm": [
                {"key": "machine.py#machine/arm", "geometry_hash": "h-old"}]},
            "others": {}, "features": {}, "params": {}, "states": {},
            "part_signatures": {"machine.py#machine/arm":
                {"volume_mm3": 1000.0, "face_count": 6}}
        }));
        let mut build = BuildIndex::default();
        build.parts.push(MemberLink {
            key: "machine.py#machine/outrigger".into(),
            geometry_hash: "h-new".into(),
        });
        build.part_signatures.insert(
            "machine.py#machine/outrigger".into(),
            ShapeSignature {
                volume_mm3: Some(1000.0),
                face_count: Some(6),
                ..Default::default()
            },
        );
        let report = relink(&approved, None, &build, &ProjectSettings::default());
        let change = report
            .changes
            .iter()
            .find(|c| c.kind == ChangeKind::ReferenceGone && c.at == "machine.py#machine/arm")
            .expect("gone-part change");
        assert_eq!(change.effect, Effect::NeedsReview);
        assert!(
            change
                .suggestions
                .iter()
                .any(|s| s.key == "machine.py#machine/outrigger"),
            "suggestions: {:?}",
            change.suggestions
        );
    }

    #[test]
    fn changed_placement_method_is_a_run_changed_diff() {
        // Match the live library and settings so only the placement differs.
        let approved = manifest(json!({
            "requirement_hash": "r",
            "library_version": crate::requirements::library::library_version(),
            "settings_hash": ProjectSettings::default().content_hash(),
            "subjects": [], "chips": [], "sets": {}, "parts": {}, "others": {},
            "features": {}, "params": {}, "states": {},
            "placements": {"machine.py#machine/lid": {"method": "place", "hash": "a"}}
        }));
        let mut build = BuildIndex::default();
        build.parts.push(MemberLink {
            key: "machine.py#machine/lid".into(),
            geometry_hash: "a".into(),
        });
        build.placements.insert(
            "machine.py#machine/lid".into(),
            PlacementLink {
                method: "position".into(),
                hash: "b".into(),
            },
        );
        let report = relink(&approved, None, &build, &ProjectSettings::default());
        assert!(report.runs(), "{:?}", report.changes);
        let change = report
            .changes
            .iter()
            .find(|c| c.kind == ChangeKind::PlacementMethod)
            .expect("placement change");
        assert_eq!(change.effect, Effect::RunChanged);
        assert!(change.message.contains("place") && change.message.contains("position"));
    }

    #[test]
    fn replaced_step_file_needs_review_even_when_geometry_is_unchanged() {
        let key = "machine.py#machine/imported";
        let approved = manifest(json!({
            "requirement_hash": "r",
            "library_version": crate::requirements::library::library_version(),
            "settings_hash": ProjectSettings::default().content_hash(),
            "subjects": [], "chips": [], "sets": {},
            "parts": {"claim": [{"key": key, "geometry_hash": "same-shape"}]},
            "others": {}, "features": {}, "params": {}, "states": {},
            "part_source_assets": {key: [{"path":"assets/housing.step", "sha256":"old-file"}]}
        }));
        let mut build = BuildIndex::default();
        build.parts.push(MemberLink {
            key: key.into(),
            geometry_hash: "same-shape".into(),
        });
        build.part_source_assets.insert(
            key.into(),
            vec![super::super::snapshot::SourceAssetInfo {
                path: "assets/housing.step".into(),
                sha256: "new-file".into(),
            }],
        );

        let report = relink(&approved, None, &build, &ProjectSettings::default());
        assert_eq!(report.decision, Effect::NeedsReview);
        assert!(report.changes.iter().any(|change| {
            change.kind == ChangeKind::ImportedFileChanged
                && change.at == key
                && change.effect == Effect::NeedsReview
        }));
    }
}
