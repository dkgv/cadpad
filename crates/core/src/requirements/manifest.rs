//! The manifest (spec §6.3): everything the plan depends on, as resolved by
//! the link stage from a build. Generated, never edited, like a lockfile. A
//! later run relinks and diffs against the approved manifest.
use super::bind::{Bindings, BoundPart, model_key, part_ref_key};
use super::expand::PrimitiveClaim;
use super::ir::{ModelRef, Requirement};
use super::quantity::Quantity;
use super::settings::ProjectSettings;
use super::snapshot::{FeatureSource, SourceAssetInfo, WorkspaceSnapshot};
use super::states::{ConcreteState, expand_states};
use super::suggestions::ShapeSignature;
use crate::portable;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A resolved part and the exact hash of its geometry.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct MemberLink {
    pub key: String,
    pub geometry_hash: String,
}

impl From<&BoundPart> for MemberLink {
    fn from(p: &BoundPart) -> Self {
        Self {
            key: p.key(),
            geometry_hash: p.geometry_hash.clone(),
        }
    }
}

/// A subject model and the input fingerprint of the build it was linked from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubjectLink {
    pub model: ModelRef,
    pub fingerprint: String,
}

/// An `@` chip and, when the requirement covers it, the assembly part it
/// stands for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChipLink {
    pub label: String,
    pub model: ModelRef,
    pub part: Option<String>,
    /// Key of the assembly part this chip maps to (`covers`).
    pub mapped_to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ParamLink {
    pub value: Quantity,
    pub min: Option<Quantity>,
    pub max: Option<Quantity>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FeatureLink {
    pub entity: super::ir::EntityKind,
    pub count: u64,
    pub hash: String,
    #[serde(default, skip_serializing_if = "is_explicit_source")]
    pub source: FeatureSource,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub positional: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub key_size: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_explicit_source(value: &FeatureSource) -> bool {
    *value == FeatureSource::Explicit
}

/// The recorded placement of one published part (spec §6.3 "placement method
/// per placed pair"): `method` for readable diffs, `hash` the canonical hash
/// of the whole [`PlacementInfo`] so any change to `on`/`align`/`offset`/
/// `mate`/`origin` is detected, not only a method swap.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlacementLink {
    pub method: String,
    pub hash: String,
}

/// Placement records of a snapshot as manifest links, keyed
/// `entry#builder/part`.
pub fn placement_links(snap: &WorkspaceSnapshot) -> BTreeMap<String, PlacementLink> {
    let mut out = BTreeMap::new();
    for m in &snap.models {
        let Some(built) = &m.built else { continue };
        let mk = model_key(&m.model);
        for (name, info) in &built.placements {
            out.insert(
                format!("{mk}/{name}"),
                PlacementLink {
                    method: info.method.clone(),
                    hash: canonical_hash(info),
                },
            );
        }
    }
    out
}

/// Source STEP inputs of every published part, keyed `entry#builder/part`.
pub fn source_asset_links(snap: &WorkspaceSnapshot) -> BTreeMap<String, Vec<SourceAssetInfo>> {
    let mut out = BTreeMap::new();
    for model in &snap.models {
        let Some(built) = &model.built else { continue };
        let mk = model_key(&model.model);
        for part in &built.parts {
            if !part.source_assets.is_empty() {
                let mut assets = part.source_assets.clone();
                assets.sort_by(|a, b| a.path.cmp(&b.path));
                out.insert(format!("{mk}/{}", part.name), assets);
            }
        }
    }
    out
}

/// Part and feature shape signatures of a snapshot as manifest signature
/// maps, keyed `entry#builder/name` and `entry#builder/part#suffix`.
pub fn signature_links(
    snap: &WorkspaceSnapshot,
) -> (
    BTreeMap<String, ShapeSignature>,
    BTreeMap<String, ShapeSignature>,
) {
    let mut parts = BTreeMap::new();
    let mut features = BTreeMap::new();
    for m in &snap.models {
        let Some(built) = &m.built else { continue };
        let mk = model_key(&m.model);
        for (name, sig) in &built.part_signatures {
            parts.insert(format!("{mk}/{name}"), sig.clone());
        }
        for (key, sig) in &built.feature_signatures {
            features.insert(format!("{mk}/{key}"), sig.clone());
        }
    }
    (parts, features)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    /// Hash of the compiled IR this manifest was linked for.
    pub requirement_hash: String,
    pub library_version: String,
    /// Hash of the project settings (ε, state limit) used.
    pub settings_hash: String,
    pub subjects: Vec<SubjectLink>,
    pub chips: Vec<ChipLink>,
    /// Members of every named set.
    pub sets: BTreeMap<String, Vec<MemberLink>>,
    /// Members of every part name and glob a claim uses (`entry#builder/pattern`).
    pub parts: BTreeMap<String, Vec<MemberLink>>,
    /// Members `others` stands for, by claim id.
    pub others: BTreeMap<String, Vec<MemberLink>>,
    pub features: BTreeMap<String, FeatureLink>,
    /// Parameters referenced, with the value and declared bounds seen.
    pub params: BTreeMap<String, ParamLink>,
    /// Expanded states of every primitive claim, by primitive claim id.
    pub states: BTreeMap<String, Vec<ConcreteState>>,
    /// Shape signatures of every published part (`entry#builder/name`),
    /// recorded so a later run can rank relink candidates when a reference
    /// breaks (spec §3.1/§6.3). Suggestion data only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub part_signatures: BTreeMap<String, ShapeSignature>,
    /// Shape signatures of every published feature
    /// (`entry#builder/part#suffix`, the [`FeatureLink`] key format).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub feature_signatures: BTreeMap<String, ShapeSignature>,
    /// Placement method per placed part (`entry#builder/part`), spec §6.3.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub placements: BTreeMap<String, PlacementLink>,
    /// Exact managed STEP inputs for each referenced published part.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub part_source_assets: BTreeMap<String, Vec<SourceAssetInfo>>,
}

fn links(parts: &[BoundPart]) -> Vec<MemberLink> {
    let mut v: Vec<MemberLink> = parts.iter().map(MemberLink::from).collect();
    v.sort();
    v
}

/// Link stage (P1.40): the manifest of a compiled requirement against a build.
pub fn link(
    req: &Requirement,
    bindings: &Bindings,
    primitives: &[PrimitiveClaim],
    settings: &ProjectSettings,
    snap: &WorkspaceSnapshot,
) -> Manifest {
    let mut subjects: Vec<SubjectLink> = bindings
        .subjects
        .values()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|m| SubjectLink {
            model: m.clone(),
            fingerprint: snap
                .model(m)
                .and_then(|s| s.built.as_ref())
                .map(|b| b.fingerprint.clone())
                .unwrap_or_default(),
        })
        .collect();
    subjects.sort_by_key(|s| model_key(&s.model));
    let chips = bindings
        .chips
        .iter()
        .map(|c| ChipLink {
            label: c.label.clone(),
            model: c.model.clone(),
            part: c.part.clone(),
            mapped_to: req
                .covers
                .iter()
                .find(|cv| cv.model == c.model)
                .map(|cv| part_ref_key(&cv.part)),
        })
        .collect();
    let (part_signatures, feature_signatures) = signature_links(snap);
    let all_source_assets = source_asset_links(snap);
    let referenced_parts: std::collections::BTreeSet<String> = bindings
        .parts
        .values()
        .chain(bindings.sets.values())
        .chain(bindings.others.values())
        .flatten()
        .map(BoundPart::key)
        .collect();
    let part_source_assets = all_source_assets
        .into_iter()
        .filter(|(key, _)| referenced_parts.contains(key))
        .collect();
    Manifest {
        requirement_hash: req.content_hash(),
        library_version: super::library::library_version().to_string(),
        settings_hash: settings.content_hash(),
        subjects,
        chips,
        sets: bindings
            .sets
            .iter()
            .map(|(k, v)| (k.clone(), links(v)))
            .collect(),
        parts: bindings
            .parts
            .iter()
            .map(|(k, v)| (k.clone(), links(v)))
            .collect(),
        others: bindings
            .others
            .iter()
            .map(|(k, v)| (k.clone(), links(v)))
            .collect(),
        features: bindings
            .features
            .iter()
            .map(|(k, f)| {
                (
                    k.clone(),
                    FeatureLink {
                        entity: f.entity,
                        count: f.count,
                        hash: f.hash.clone(),
                        source: f.source,
                        positional: f.positional,
                        key_size: k.len() as u64,
                    },
                )
            })
            .collect(),
        params: bindings
            .params
            .iter()
            .map(|(k, p)| {
                (
                    k.clone(),
                    ParamLink {
                        value: p.value,
                        min: p.min,
                        max: p.max,
                    },
                )
            })
            .collect(),
        states: primitives
            .iter()
            .map(|p| (p.id.clone(), expand_states(&p.states)))
            .collect(),
        placements: placement_links(snap),
        part_source_assets,
        part_signatures,
        feature_signatures,
    }
}

impl Manifest {
    /// Every recorded shape signature under one map (`entry#builder/...`
    /// keys), for attaching to a fresh [`WorkspaceSnapshot`] as suggestion
    /// queries (see [`WorkspaceSnapshot::attach_reference_signatures`]).
    pub fn signature_map(&self) -> BTreeMap<String, ShapeSignature> {
        self.part_signatures
            .iter()
            .chain(self.feature_signatures.iter())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// SHA-256 of canonical JSON (sorted keys, no whitespace). The hash of IR,
/// manifest and plan. (`portable::fingerprint` hashes project `Inputs`, so
/// this uses the same `bytes_hash` primitive underneath.)
pub fn canonical_hash<T: Serialize>(value: &T) -> String {
    let v: Value = serde_json::to_value(value).expect("serializes");
    let mut text = String::new();
    super::ir::canonical(&v, &mut text);
    portable::bytes_hash(text.as_bytes())
}

impl Manifest {
    pub fn content_hash(&self) -> String {
        canonical_hash(self)
    }
}
