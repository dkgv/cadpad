//! Shape-signature suggestions (spec §3.1 "shape signature", P2.21).
//!
//! When a part or feature reference breaks — a renamed part, a replaced
//! imported file, a history name that no longer resolves — candidates can be
//! ranked by *shape signature*: volume, area, extents, entity counts and a
//! face/edge-type histogram. A signature only ever *suggests*; it never
//! decides a binding (spec §3.1: the compiler has no "closest match").
//!
//! The data comes from replies core already produces, so nothing here needs
//! the worker:
//!
//! * part signatures: the `model.parts[]` entries of a `get_project` reply
//!   (`bounds.size`, `volume`, `area`, `shape_kind`, `face_count`,
//!   `edge_count`, published by the worker's `describe`);
//! * feature signatures: face/edge entities of an `inspect` reply grouped by
//!   their `semantic_labels` and `history_references` names, with
//!   `surface_type`/`curve_type`, `area` and `radius` per entity.
//!
//! Wiring note: `bind.rs` attaches name-similarity options to
//! `Fix::AskUser`; ranking these signatures into those options (or into a
//! `needs_review` relink report) is the caller's job — this module is the
//! pure ranking layer.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use super::ir::EntityKind;

/// Upper bound on candidate score for a suggestion to be offered. Scores are
/// weighted means of per-field distances in `[0, 1]`; below this the shapes
/// are plausibly the same part renamed or slightly edited.
pub const MAX_SUGGESTION_SCORE: f64 = 0.45;

// ------------------------------------------------------------- histograms

/// Half the L1 distance between two histograms' bucket proportions.
fn histogram_distance(a: &[f64], b: &[f64]) -> Option<f64> {
    let (ta, tb): (f64, f64) = (a.iter().sum(), b.iter().sum());
    if ta <= 0.0 || tb <= 0.0 {
        return None;
    }
    let d = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (x / ta - y / tb).abs())
        .sum::<f64>();
    Some(d / 2.0)
}

/// Counts of face surface types, bucketed from the OCCT `GeomAbs` names the
/// worker reports as `surface_type` (`PLANE`, `CYLINDER`, `CONE`, `SPHERE`,
/// `TORUS`, `BSPLINE`, ...). An all-zero histogram means "unknown", not
/// "no faces".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FaceHistogram {
    pub planar: u64,
    pub cylindrical: u64,
    pub conical: u64,
    pub spherical: u64,
    pub toroidal: u64,
    /// Everything else: B-spline, Bézier, offset, other.
    pub other: u64,
}

impl FaceHistogram {
    /// Bucket one `surface_type` string (case-insensitive).
    pub fn from_surface_type(surface_type: &str) -> FaceHistogram {
        let mut h = FaceHistogram::default();
        h.add(surface_type);
        h
    }
    /// Add one `surface_type` string (case-insensitive).
    pub fn add(&mut self, surface_type: &str) {
        match surface_type.to_ascii_uppercase().as_str() {
            "PLANE" | "PLANAR" => self.planar += 1,
            "CYLINDER" | "CYLINDRICAL" => self.cylindrical += 1,
            "CONE" | "CONICAL" => self.conical += 1,
            "SPHERE" | "SPHERICAL" => self.spherical += 1,
            "TORUS" | "TOROIDAL" => self.toroidal += 1,
            _ => self.other += 1,
        }
    }
    pub fn total(&self) -> u64 {
        self.planar + self.cylindrical + self.conical + self.spherical + self.toroidal + self.other
    }
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
    /// Distance between two histograms as proportions: half the L1 distance
    /// between the per-bucket fractions, in `[0, 1]`. Scale-invariant — a
    /// part twice as large with the same face mix scores 0. `None` when
    /// either side is unknown.
    pub fn distance(&self, other: &FaceHistogram) -> Option<f64> {
        histogram_distance(
            &[
                self.planar as f64,
                self.cylindrical as f64,
                self.conical as f64,
                self.spherical as f64,
                self.toroidal as f64,
                self.other as f64,
            ],
            &[
                other.planar as f64,
                other.cylindrical as f64,
                other.conical as f64,
                other.spherical as f64,
                other.toroidal as f64,
                other.other as f64,
            ],
        )
    }
    /// Short labels for the non-empty buckets, e.g. `"2 cylindrical faces"`.
    pub fn labels(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (count, what) in [
            (self.planar, "planar"),
            (self.cylindrical, "cylindrical"),
            (self.conical, "conical"),
            (self.spherical, "spherical"),
            (self.toroidal, "toroidal"),
            (self.other, "other-surface"),
        ] {
            if count > 0 {
                out.push(format!(
                    "{count} {} face{}",
                    what,
                    if count == 1 { "" } else { "s" }
                ));
            }
        }
        out
    }
}

/// Counts of edge curve types, bucketed from the worker's `curve_type`
/// (`LINE`, `CIRCLE`, `ELLIPSE`, `HYPERBOLA`, `PARABOLA`, `BEZIER`, ...).
/// An all-zero histogram means "unknown".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EdgeHistogram {
    pub linear: u64,
    pub circular: u64,
    /// Ellipse, hyperbola, parabola, Bézier, B-spline, offset, other.
    pub other: u64,
}

impl EdgeHistogram {
    pub fn from_curve_type(curve_type: &str) -> EdgeHistogram {
        let mut h = EdgeHistogram::default();
        h.add(curve_type);
        h
    }
    pub fn add(&mut self, curve_type: &str) {
        match curve_type.to_ascii_uppercase().as_str() {
            "LINE" | "LINEAR" => self.linear += 1,
            "CIRCLE" | "CIRCULAR" => self.circular += 1,
            _ => self.other += 1,
        }
    }
    pub fn total(&self) -> u64 {
        self.linear + self.circular + self.other
    }
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
    pub fn distance(&self, other: &EdgeHistogram) -> Option<f64> {
        let a = [self.linear as f64, self.circular as f64, self.other as f64];
        let b = [
            other.linear as f64,
            other.circular as f64,
            other.other as f64,
        ];
        histogram_distance(&a, &b)
    }
    pub fn labels(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (count, what) in [(self.linear, "linear"), (self.circular, "circular")] {
            if count > 0 {
                out.push(format!(
                    "{count} {} edge{}",
                    what,
                    if count == 1 { "" } else { "s" }
                ));
            }
        }
        if self.other > 0 {
            out.push(format!("{} other edges", self.other));
        }
        out
    }
}

// ------------------------------------------------------------- signature

/// The geometric fingerprint used only for *suggesting* candidates (spec
/// §3.1). Every field is optional: a signature compares only the fields both
/// sides know, so partial data (a `get_project` part without per-face types,
/// a single inspected face) still ranks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShapeSignature {
    /// Solid volume in mm³ (`null` for shells/wires).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_mm3: Option<f64>,
    /// Surface area in mm².
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_mm2: Option<f64>,
    /// World/local bounding-box size in mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extents_mm: Option<[f64; 3]>,
    /// Radius in mm when the shape is a single cylinder/sphere face or edge
    /// (hole vs boss candidates differ here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius_mm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_count: Option<u64>,
    /// Worker `shape_kind`/`ShapeType` name (`Solid`, `CompSolid`, ...), kept
    /// as a soft signal only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape_kind: Option<String>,
    #[serde(default)]
    pub faces: FaceHistogram,
    #[serde(default)]
    pub edges: EdgeHistogram,
}

/// Orders-of-magnitude distance for positive quantities: `|ln(a/b)| / ln 10`,
/// saturated at 1. Two values differing by a factor of 10 score 1.
fn magnitude_distance(a: f64, b: f64) -> Option<f64> {
    if !(a.is_finite() && b.is_finite()) || a <= 0.0 || b <= 0.0 {
        return None;
    }
    Some(((a / b).ln().abs() / std::f64::consts::LN_10).min(1.0))
}

fn relative_distance(a: f64, b: f64) -> Option<f64> {
    let m = a.abs().max(b.abs());
    (m.is_finite() && m > 0.0).then(|| ((a - b).abs() / m).min(1.0))
}

/// One ranked suggestion: `key` identifies the candidate (a part name, a
/// `part#feature` key, ...), `score` is the weighted mean distance (lower is
/// closer), `traits` describes the candidate's shape for display.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Suggestion<K = String> {
    pub key: K,
    pub score: f64,
    /// Human-readable shape traits, e.g. `["6 planar faces", "3 cylindrical faces"]`.
    pub traits: Vec<String>,
}

impl ShapeSignature {
    /// Minimal signature of a feature that selects `count` entities of one
    /// kind — all `resolve_features`/`inspect`-less data core has for history
    /// and geometric features. Comparable only on entity counts.
    pub fn entity_count(entity: EntityKind, count: u64) -> ShapeSignature {
        ShapeSignature {
            face_count: (entity == EntityKind::Face).then_some(count),
            edge_count: (entity == EntityKind::Edge).then_some(count),
            ..Default::default()
        }
    }

    /// True when at least one field can be compared with another signature.
    pub fn is_comparable(&self) -> bool {
        self.volume_mm3.is_some()
            || self.area_mm2.is_some()
            || self.extents_mm.is_some()
            || self.radius_mm.is_some()
            || self.face_count.is_some()
            || self.edge_count.is_some()
            || !self.faces.is_empty()
            || !self.edges.is_empty()
            || self.shape_kind.is_some()
    }

    /// Weighted mean distance to `other` over the fields both sides know, or
    /// `None` when nothing is comparable. Weights favour the discriminative
    /// fields (volume, face-type histogram) over raw counts.
    pub fn distance(&self, other: &ShapeSignature) -> Option<f64> {
        let mut total = 0.0;
        let mut weight = 0.0;
        let mut add = |w: f64, d: Option<f64>| {
            if let Some(d) = d {
                total += w * d;
                weight += w;
            }
        };
        add(
            2.0,
            match (self.volume_mm3, other.volume_mm3) {
                (Some(a), Some(b)) => magnitude_distance(a, b),
                _ => None,
            },
        );
        add(
            1.5,
            match (self.area_mm2, other.area_mm2) {
                (Some(a), Some(b)) => magnitude_distance(a, b),
                _ => None,
            },
        );
        add(
            1.5,
            match (self.extents_mm, other.extents_mm) {
                (Some(a), Some(b)) => Some(
                    a.iter()
                        .zip(b.iter())
                        .filter_map(|(x, y)| relative_distance(*x, *y))
                        .sum::<f64>()
                        / 3.0,
                ),
                _ => None,
            },
        );
        add(1.0, self.faces.distance(&other.faces));
        add(0.75, self.edges.distance(&other.edges));
        add(
            0.5,
            match (self.radius_mm, other.radius_mm) {
                (Some(a), Some(b)) => magnitude_distance(a, b),
                _ => None,
            },
        );
        add(
            0.5,
            match (self.face_count, other.face_count) {
                (Some(a), Some(b)) => relative_distance(a as f64, b as f64),
                _ => None,
            },
        );
        add(
            0.5,
            match (self.edge_count, other.edge_count) {
                (Some(a), Some(b)) => relative_distance(a as f64, b as f64),
                _ => None,
            },
        );
        // Shape kind is a weak signal: mismatch costs a little, never vetoes.
        if let (Some(a), Some(b)) = (&self.shape_kind, &other.shape_kind) {
            total += 0.5 * f64::from(a != b);
            weight += 0.5;
        }
        (weight > 0.0).then_some(total / weight)
    }

    /// Trait labels for display and suggestion reasons.
    pub fn traits(&self) -> Vec<String> {
        let mut out = self.faces.labels();
        out.extend(self.edges.labels());
        if let Some(kind) = &self.shape_kind {
            out.push(kind.to_ascii_lowercase());
        }
        out
    }
}

/// Rank candidates by signature distance, closest first. Candidates with no
/// comparable fields, or scoring above [`MAX_SUGGESTION_SCORE`], are dropped —
/// a signature suggests, it never decides, so weak matches are not offered.
pub fn rank<K: Clone + Ord>(
    query: &ShapeSignature,
    candidates: impl IntoIterator<Item = (K, ShapeSignature)>,
    limit: usize,
) -> Vec<Suggestion<K>> {
    let mut scored: Vec<Suggestion<K>> = candidates
        .into_iter()
        .filter_map(|(key, sig)| {
            let score = query.distance(&sig)?;
            (score <= MAX_SUGGESTION_SCORE).then(|| Suggestion {
                key,
                score,
                traits: sig.traits(),
            })
        })
        .collect();
    scored.sort_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });
    scored.truncate(limit);
    scored
}

/// Plain-language role hints a signature suggests, for the assistant or
/// reviewer to phrase candidate rules/parts (P2.21): cylindrical faces →
/// hole/shaft candidates, planar faces → seating candidates, circular edges →
/// holes/pins. Suggestions only — a hint never becomes a binding.
pub fn role_hints(sig: &ShapeSignature) -> Vec<String> {
    let mut out = Vec::new();
    if sig.faces.cylindrical > 0 {
        out.push("hole or shaft".to_string());
    }
    if sig.faces.conical > 0 {
        out.push("tapered hole or cone".to_string());
    }
    if sig.faces.planar >= 2 {
        out.push("seating or fixturing faces".to_string());
    }
    if sig.faces.spherical + sig.faces.toroidal > 0 {
        out.push("ball or ring surface".to_string());
    }
    if sig.edges.circular > 0 && sig.faces.cylindrical == 0 {
        out.push("hole or pin axis".to_string());
    }
    if sig.face_count == Some(6) && sig.edges.circular == 0 {
        out.push("prismatic solid".to_string());
    }
    out
}

// ------------------------------------------------------- project parsing

fn u64_at(value: &Value, path: &[&str]) -> Option<u64> {
    let mut node = value;
    for key in path {
        node = node.get(*key)?;
    }
    node.as_u64()
}

fn str_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut node = value;
    for key in path {
        node = node.get(*key)?;
    }
    node.as_str()
}

/// Optional published per-part face/edge-type histograms. The worker does not
/// publish these today; when it does (a `face_types`/`edge_types` object of
/// `surface_type`/`curve_type` → count on the part), they are picked up here
/// without any other change.
fn histogram_field(value: &Value, field: &str) -> FaceHistogram {
    let mut h = FaceHistogram::default();
    if let Some(map) = value.get(field).and_then(Value::as_object) {
        for (surface, count) in map {
            for _ in 0..count.as_u64().unwrap_or(0) {
                h.add(surface);
            }
        }
    }
    h
}

fn edge_histogram_field(value: &Value, field: &str) -> EdgeHistogram {
    let mut h = EdgeHistogram::default();
    if let Some(map) = value.get(field).and_then(Value::as_object) {
        for (curve, count) in map {
            for _ in 0..count.as_u64().unwrap_or(0) {
                h.add(curve);
            }
        }
    }
    h
}

/// Signature of one `model.parts[]` entry of a `get_project` reply. Missing
/// fields stay `None`; an unpublished histogram stays empty (unknown).
pub fn signature_from_part(part: &Value) -> ShapeSignature {
    ShapeSignature {
        volume_mm3: part.get("volume").and_then(Value::as_f64),
        area_mm2: part.get("area").and_then(Value::as_f64),
        extents_mm: part
            .get("bounds")
            .and_then(|b| b.get("size"))
            .and_then(Value::as_array)
            .and_then(|size| {
                Some([
                    size.first()?.as_f64()?,
                    size.get(1)?.as_f64()?,
                    size.get(2)?.as_f64()?,
                ])
            }),
        radius_mm: None,
        face_count: u64_at(part, &["face_count"]),
        edge_count: u64_at(part, &["edge_count"]),
        shape_kind: str_at(part, &["shape_kind"]).map(str::to_string),
        faces: histogram_field(part, "face_types"),
        edges: edge_histogram_field(part, "edge_types"),
    }
}

/// Signature of one inspected face/edge entity of an `inspect` reply
/// (`entities[]` item with `kind` `"face"`/`"edge"`).
pub fn signature_from_entity(entity: &Value) -> ShapeSignature {
    let edges = match entity.get("curve_type").and_then(Value::as_str) {
        Some(t) => EdgeHistogram::from_curve_type(t),
        None => EdgeHistogram::default(),
    };
    let faces = match entity.get("surface_type").and_then(Value::as_str) {
        Some(t) => FaceHistogram::from_surface_type(t),
        None => FaceHistogram::default(),
    };
    ShapeSignature {
        volume_mm3: None,
        area_mm2: entity.get("area").and_then(Value::as_f64),
        extents_mm: entity
            .get("bounds")
            .and_then(|b| b.get("size"))
            .and_then(Value::as_array)
            .and_then(|size| {
                Some([
                    size.first()?.as_f64()?,
                    size.get(1)?.as_f64()?,
                    size.get(2)?.as_f64()?,
                ])
            }),
        radius_mm: entity.get("radius").and_then(Value::as_f64),
        face_count: (entity.get("kind").and_then(Value::as_str) == Some("face")).then_some(1),
        edge_count: (entity.get("kind").and_then(Value::as_str) == Some("edge")).then_some(1),
        shape_kind: entity
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_string),
        faces,
        edges,
    }
}

/// `(part name, signature)` for every part of a `get_project` reply.
pub fn part_signatures(project: &Value) -> Vec<(String, ShapeSignature)> {
    project["model"]["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| {
            let name = str_at(part, &["name"])
                .or_else(|| str_at(part, &["instance_id"]))?
                .to_string();
            Some((name, signature_from_part(part)))
        })
        .collect()
}

/// Shape-ranked part candidates for a broken part/feature reference. Keys
/// are part names as published (`name`, falling back to `instance_id`).
pub fn suggest_parts(
    project: &Value,
    query: &ShapeSignature,
    limit: usize,
) -> Vec<Suggestion<String>> {
    rank(query, part_signatures(project), limit)
}

// -------------------------------------------------------- feature signatures

/// Signature of one named feature (explicit `features=` name or history
/// name) accumulated from an `inspect` reply's face/edge entities.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FeatureSignature {
    /// `instance_id` of the owning part.
    pub part: String,
    /// Feature name: a `semantic_labels` entry or a `history_references` name.
    pub name: String,
    /// Entity kinds seen (`face`, `edge`, or both).
    #[serde(default)]
    pub entity_kinds: Vec<String>,
    /// Number of entities the name selects.
    pub count: u64,
    /// Summed face area in mm².
    #[serde(default)]
    pub area_mm2: f64,
    /// Smallest/largest radius seen on cylindrical/spherical faces, mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_radius_mm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_radius_mm: Option<f64>,
    #[serde(default)]
    pub faces: FaceHistogram,
    #[serde(default)]
    pub edges: EdgeHistogram,
}

impl FeatureSignature {
    /// View as a [`ShapeSignature`] for ranking.
    pub fn signature(&self) -> ShapeSignature {
        ShapeSignature {
            volume_mm3: None,
            area_mm2: (self.area_mm2 > 0.0).then_some(self.area_mm2),
            extents_mm: None,
            radius_mm: match (self.min_radius_mm, self.max_radius_mm) {
                (Some(a), Some(b)) if (a - b).abs() < 1e-9 => Some(a),
                _ => None,
            },
            face_count: self
                .entity_kinds
                .iter()
                .any(|k| k == "face")
                .then_some(self.count),
            edge_count: self
                .entity_kinds
                .iter()
                .any(|k| k == "edge")
                .then_some(self.count),
            shape_kind: None,
            faces: self.faces,
            edges: self.edges,
        }
    }
    /// Stable suggestion key: `part#feature`, matching feature reference
    /// syntax (`@model/part#feature` resolves through the part).
    pub fn key(&self) -> String {
        format!("{}#{}", self.part, self.name)
    }
    pub fn traits(&self) -> Vec<String> {
        let mut out = self.faces.labels();
        out.extend(self.edges.labels());
        out.push(format!("{} {}", self.count, self.entity_kinds.join("/")));
        out
    }
}

/// Group an `inspect` reply's face/edge entities into per-feature
/// signatures, keyed internally by `(part, name)`. An entity contributes to
/// every name in `semantic_labels` and every `history_references[].name`.
pub fn feature_signatures_from_inspect(inspect: &Value) -> Vec<FeatureSignature> {
    #[derive(Default)]
    struct Acc {
        entity_kinds: Vec<String>,
        count: u64,
        area_mm2: f64,
        min_radius: Option<f64>,
        max_radius: Option<f64>,
        faces: FaceHistogram,
        edges: EdgeHistogram,
    }
    let mut map: BTreeMap<(String, String), Acc> = BTreeMap::new();
    for entity in inspect["entities"].as_array().into_iter().flatten() {
        let kind = entity.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind != "face" && kind != "edge" {
            continue;
        }
        let part = entity
            .get("instance_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if part.is_empty() {
            continue;
        }
        let mut names: Vec<String> = entity["semantic_labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        for r in entity["history_references"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(n) = r.get("name").and_then(Value::as_str) {
                names.push(n.to_string());
            }
        }
        names.sort();
        names.dedup();
        if names.is_empty() {
            continue;
        }
        let surface = entity.get("surface_type").and_then(Value::as_str);
        let curve = entity.get("curve_type").and_then(Value::as_str);
        let area = entity.get("area").and_then(Value::as_f64).unwrap_or(0.0);
        let radius = entity.get("radius").and_then(Value::as_f64);
        for name in names {
            let acc = map.entry((part.clone(), name)).or_default();
            if !acc.entity_kinds.iter().any(|k| k == kind) {
                acc.entity_kinds.push(kind.to_string());
            }
            acc.count += 1;
            acc.area_mm2 += area;
            if let Some(r) = radius {
                acc.min_radius = Some(acc.min_radius.map_or(r, |m| m.min(r)));
                acc.max_radius = Some(acc.max_radius.map_or(r, |m| m.max(r)));
            }
            if let Some(t) = surface {
                acc.faces.add(t);
            }
            if let Some(t) = curve {
                acc.edges.add(t);
            }
        }
    }
    map.into_iter()
        .map(|((part, name), acc)| FeatureSignature {
            part,
            name,
            entity_kinds: acc.entity_kinds,
            count: acc.count,
            area_mm2: acc.area_mm2,
            min_radius_mm: acc.min_radius,
            max_radius_mm: acc.max_radius,
            faces: acc.faces,
            edges: acc.edges,
        })
        .collect()
}

/// Shape-ranked feature candidates from an `inspect` reply. Suggestion keys
/// are `part#feature`.
pub fn suggest_features(
    inspect: &Value,
    query: &ShapeSignature,
    limit: usize,
) -> Vec<Suggestion<String>> {
    let features = feature_signatures_from_inspect(inspect);
    rank(
        query,
        features.iter().map(|f| (f.key(), f.signature())),
        limit,
    )
    .into_iter()
    .map(|s| {
        let traits = features
            .iter()
            .find(|f| f.key() == s.key)
            .map(|f| f.traits())
            .unwrap_or_default();
        Suggestion {
            key: s.key,
            score: s.score,
            traits,
        }
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sig(volume: f64, area: f64, faces: FaceHistogram) -> ShapeSignature {
        ShapeSignature {
            volume_mm3: Some(volume),
            area_mm2: Some(area),
            extents_mm: Some([10.0, 10.0, 10.0]),
            face_count: Some(faces.total()),
            faces,
            ..Default::default()
        }
    }

    #[test]
    fn histograms_bucket_occt_names() {
        let mut h = FaceHistogram::default();
        h.add("PLANE");
        h.add("plane");
        h.add("CYLINDER");
        h.add("TORUS");
        h.add("BSPLINE");
        assert_eq!((h.planar, h.cylindrical, h.toroidal, h.other), (2, 1, 1, 1));
        assert_eq!(h.total(), 5);
        assert!(!h.is_empty());
        let e = EdgeHistogram::from_curve_type("CIRCLE");
        assert_eq!((e.circular, e.linear, e.other), (1, 0, 0));
        assert!(FaceHistogram::default().is_empty());
        assert!(EdgeHistogram::default().is_empty());
    }

    #[test]
    fn histogram_distance_is_jaccard() {
        let a = FaceHistogram {
            planar: 2,
            cylindrical: 1,
            ..Default::default()
        };
        let same = FaceHistogram {
            planar: 4,
            cylindrical: 2,
            ..Default::default()
        };
        let b = FaceHistogram {
            planar: 1,
            spherical: 3,
            ..Default::default()
        };
        assert_eq!(a.distance(&a), Some(0.0));
        assert_eq!(a.distance(&same), Some(0.0)); // same proportions
        assert!(a.distance(&b).unwrap() > 0.5);
        // Same mix at double the count: still identical proportions.
        let double = FaceHistogram {
            planar: 4,
            cylindrical: 2,
            ..Default::default()
        };
        assert_eq!(a.distance(&double), Some(0.0));
        assert_eq!(a.distance(&FaceHistogram::default()), None);
    }

    #[test]
    fn distance_compares_only_shared_fields() {
        let rich = sig(
            1000.0,
            600.0,
            FaceHistogram {
                planar: 4,
                cylindrical: 2,
                ..Default::default()
            },
        );
        let sparse = ShapeSignature {
            volume_mm3: Some(1000.0),
            ..Default::default()
        };
        // Only volume is shared and it matches: distance 0 despite unknown rest.
        assert_eq!(rich.distance(&sparse), Some(0.0));
        let different = ShapeSignature {
            volume_mm3: Some(1.0e9),
            ..Default::default()
        };
        assert!(rich.distance(&different).unwrap() > 0.9);
        assert_eq!(
            ShapeSignature::default().distance(&ShapeSignature::default()),
            None
        );
    }

    #[test]
    fn rank_orders_closest_first_and_filters_weak_matches() {
        let query = sig(
            800.0,
            500.0,
            FaceHistogram {
                planar: 4,
                cylindrical: 1,
                ..Default::default()
            },
        );
        let candidates = vec![
            (
                "far".to_string(),
                sig(
                    9.0e9,
                    9.0e9,
                    FaceHistogram {
                        spherical: 9,
                        ..Default::default()
                    },
                ),
            ),
            (
                "near".to_string(),
                sig(
                    850.0,
                    520.0,
                    FaceHistogram {
                        planar: 4,
                        cylindrical: 1,
                        other: 1,
                        ..Default::default()
                    },
                ),
            ),
            ("empty".to_string(), ShapeSignature::default()),
        ];
        let ranked = rank(&query, candidates, 5);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].key, "near");
        assert!(ranked[0].score < MAX_SUGGESTION_SCORE);
        assert!(ranked[0].traits.iter().any(|t| t.contains("planar")));
    }

    #[test]
    fn part_signatures_come_from_project_parts() {
        let project = json!({"model": {"parts": [
            {"name": "arm", "volume": 500.0, "area": 400.0, "shape_kind": "Solid",
             "bounds": {"size": [20.0, 5.0, 5.0]}, "face_count": 6, "edge_count": 12},
            {"instance_id": "post", "volume": 300.0, "area": 250.0,
             "bounds": {"size": [10.0, 10.0, 30.0]}, "face_count": 3, "edge_count": 3},
            {"name": "wire", "volume": null}
        ]}});
        let parts = part_signatures(&project);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].0, "arm");
        assert_eq!(parts[0].1.extents_mm, Some([20.0, 5.0, 5.0]));
        assert_eq!(parts[0].1.face_count, Some(6));
        assert_eq!(parts[1].0, "post"); // falls back to instance_id
        assert_eq!(parts[2].1.volume_mm3, None);

        let query = signature_from_part(&project["model"]["parts"][0]);
        let suggested = suggest_parts(&project, &query, 3);
        assert_eq!(suggested[0].key, "arm");
        assert!(suggested.iter().any(|s| s.key == "post"));
    }

    #[test]
    fn published_face_type_histograms_are_picked_up() {
        let part = json!({"name": "bore", "volume": 10.0, "area": 20.0,
            "face_types": {"PLANE": 2, "CYLINDER": 1}, "edge_types": {"CIRCLE": 2}});
        let sig = signature_from_part(&part);
        assert_eq!(sig.faces.planar, 2);
        assert_eq!(sig.faces.cylindrical, 1);
        assert_eq!(sig.edges.circular, 2);
    }

    #[test]
    fn feature_signatures_group_inspect_entities_by_label_and_history_name() {
        let inspect = json!({"entities": [
            {"kind": "face", "instance_id": "base", "surface_type": "PLANE", "area": 100.0,
             "semantic_labels": ["top"], "history_references": [{"name": "make/base#1/top"}]},
            {"kind": "face", "instance_id": "base", "surface_type": "CYLINDER", "area": 12.5,
             "radius": 4.0, "semantic_labels": [], "history_references": [{"name": "make/base#1/bore"}]},
            {"kind": "edge", "instance_id": "base", "curve_type": "CIRCLE", "radius": 4.0,
             "semantic_labels": ["bore"], "history_references": []},
            {"kind": "part", "instance_id": "base", "name": "base"},
            {"kind": "face", "instance_id": "lid", "surface_type": "PLANE", "area": 90.0,
             "semantic_labels": [], "history_references": []}
        ]});
        let features = feature_signatures_from_inspect(&inspect);
        let by = |part: &str, name: &str| {
            features
                .iter()
                .find(|f| f.part == part && f.name == name)
                .unwrap_or_else(|| panic!("missing {part}#{name}"))
        };
        // One entity, two names: it counts under both.
        assert_eq!(by("base", "top").count, 1);
        assert_eq!(by("base", "make/base#1/top").faces.planar, 1);
        let bore = by("base", "make/base#1/bore");
        assert_eq!((bore.count, bore.area_mm2), (1, 12.5));
        assert_eq!(bore.min_radius_mm, Some(4.0));
        let bore_label = by("base", "bore");
        assert_eq!(bore_label.count, 1);
        assert_eq!(bore_label.edges.circular, 1);
        assert_eq!(features.len(), 4); // top, top-history, bore-history, bore-label
        assert!(features.iter().all(|f| f.part != "lid"));
    }

    #[test]
    fn feature_suggestions_rank_by_shape() {
        // Query: a cylindrical face of radius ~4 (a hole wall).
        let query = ShapeSignature {
            area_mm2: Some(12.0),
            radius_mm: Some(4.0),
            faces: FaceHistogram {
                cylindrical: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let inspect = json!({"entities": [
            {"kind": "face", "instance_id": "base", "surface_type": "CYLINDER", "area": 12.6,
             "radius": 4.1, "semantic_labels": ["bore"]},
            {"kind": "face", "instance_id": "base", "surface_type": "PLANE", "area": 400.0,
             "semantic_labels": ["top"]},
            {"kind": "face", "instance_id": "post", "surface_type": "CYLINDER", "area": 30.0,
             "radius": 6.0, "semantic_labels": ["shaft"]}
        ]});
        let suggested = suggest_features(&inspect, &query, 3);
        assert_eq!(suggested[0].key, "base#bore");
        assert!(suggested.iter().any(|s| s.key == "post#shaft"));
    }

    #[test]
    fn role_hints_describe_candidate_kinds() {
        let bore = ShapeSignature {
            faces: FaceHistogram {
                planar: 2,
                cylindrical: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let hints = role_hints(&bore);
        assert!(hints.iter().any(|h| h == "hole or shaft"));
        assert!(hints.iter().any(|h| h == "seating or fixturing faces"));

        let block = ShapeSignature {
            face_count: Some(6),
            edges: EdgeHistogram {
                linear: 12,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(role_hints(&block), vec!["prismatic solid".to_string()]);
        assert!(role_hints(&ShapeSignature::default()).is_empty());
    }

    #[test]
    fn signature_json_round_trips() {
        let sig = sig(
            10.0,
            20.0,
            FaceHistogram {
                planar: 1,
                ..Default::default()
            },
        );
        let value = serde_json::to_value(&sig).unwrap();
        assert_eq!(
            serde_json::from_value::<ShapeSignature>(value).unwrap(),
            sig
        );
    }
}
