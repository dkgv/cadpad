//! The manifest (spec §6.3): everything the plan depends on, as resolved by
//! the link stage from a build. Generated, never edited, like a lockfile. A
//! later run relinks and diffs against the approved manifest.
use super::bind::{Bindings, BoundPart, model_key, part_ref_key};
use super::expand::PrimitiveClaim;
use super::ir::{ModelRef, Requirement};
use super::quantity::Quantity;
use super::settings::ProjectSettings;
use super::snapshot::WorkspaceSnapshot;
use super::states::{ConcreteState, expand_states};
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
    // TODO(placement): per placed pair placement method; needs `place(...)`
    // helpers in source (spec §3.2), not part of the v1 library.
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
