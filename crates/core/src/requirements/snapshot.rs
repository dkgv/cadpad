//! A manifest-like snapshot of a built workspace: everything binding needs, as
//! plain data. Binding runs against this type, so it is testable without the
//! worker; the adapters at the bottom produce it from existing core data (the
//! `get_project` JSON and the project source catalog).
use super::ir::{EntityKind, ModelRef};
use super::parse::parse_unit;
use super::quantity::{Quantity, Unit};
use super::suggestions::{ShapeSignature, signature_from_part};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Where a part was published from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceInfo {
    pub file: String,
    pub line: u64,
    /// Enclosing builder function, when the worker reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
}

/// A published part of a built model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartInfo {
    pub name: String,
    /// SHA-256 of the part's exact geometry; empty when the build could not
    /// verify one (legacy revisions).
    pub geometry_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceInfo>,
    /// Explicit fastener properties published by the worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fastener: Option<crate::geometry::FastenerMetadata>,
    /// Managed STEP inputs used by this published part, recorded as exact
    /// workspace-relative path and content digest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_assets: Vec<SourceAssetInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceAssetInfo {
    pub path: String,
    pub sha256: String,
}

/// A model parameter with its declared bounds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ParamInfo {
    pub name: String,
    pub value: Quantity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<Quantity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<Quantity>,
}

/// A mechanism joint; `limits` in m or rad.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JointInfo {
    pub name: String,
    /// Native joint coordinate unit (`m` or `rad`); absent only in older catalogs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<[f64; 2]>,
}

/// A named constant in the model source (`TOWER_COMB_TRAVEL`), for `model`
/// provenance of listed values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConstantInfo {
    pub name: String,
    pub values: Vec<Quantity>,
}

/// A feature published with `show(..., features={...})`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeatureSource {
    #[default]
    Explicit,
    History,
    Geometric,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeatureInfo {
    pub part: String,
    pub name: String,
    pub entity: EntityKind,
    /// Number of entities the name selects.
    pub count: u64,
    pub hash: String,
    #[serde(default, skip_serializing_if = "is_explicit_feature")]
    pub source: FeatureSource,
    /// Positional history hints are never accepted as stable references.
    #[serde(default)]
    pub positional: bool,
}

impl FeatureInfo {
    /// Suffix of this feature's manifest key: the bare name for explicit and
    /// geometric features, `history:<name>` for history names. Matches the
    /// suffix [`super::bind::feature_ref_key`] produces for the same
    /// reference.
    pub fn link_suffix(&self) -> String {
        match self.source {
            FeatureSource::History => format!("history:{}", self.name),
            FeatureSource::Explicit | FeatureSource::Geometric => self.name.clone(),
        }
    }
}

fn is_explicit_feature(value: &FeatureSource) -> bool {
    *value == FeatureSource::Explicit
}

/// What a `place()` call targeted: a face, edge or whole part, optionally a
/// published part's named feature.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlacementOn {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<serde_json::Value>,
}

/// `origin="entry.py#builder"` provenance recorded by `place()`/`show()`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlacementOrigin {
    pub entry: String,
    pub builder: String,
}

/// How one published part was positioned (spec §3.2/§6.4). `method` is
/// `place`, `position`, `assembly` or `source`; only `place` records carry
/// `on`/`align`/`offset_mm`/`mate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlacementInfo {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<PlacementOn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_mm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PlacementOrigin>,
    /// Forward-compatible extras the worker may add later.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BuiltModel {
    /// Input fingerprint of the build (sources, parameters, assets).
    pub fingerprint: String,
    #[serde(default)]
    pub parts: Vec<PartInfo>,
    #[serde(default)]
    pub params: Vec<ParamInfo>,
    #[serde(default)]
    pub joints: Vec<JointInfo>,
    #[serde(default)]
    pub constants: Vec<ConstantInfo>,
    #[serde(default)]
    pub features: Vec<FeatureInfo>,
    /// Per-part placement records; only present when the build used `place()`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub placements: BTreeMap<String, PlacementInfo>,
    /// Shape signatures of this build's parts, keyed by part name (spec §3.1).
    /// Used only to *suggest* candidates for broken references; a signature
    /// never decides a binding.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub part_signatures: BTreeMap<String, ShapeSignature>,
    /// Shape signatures of this build's features, keyed `part#suffix` where
    /// the suffix is [`FeatureInfo::link_suffix`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub feature_signatures: BTreeMap<String, ShapeSignature>,
    /// Shape signatures the *approved* manifest recorded, keyed
    /// `entry#builder/...` like [`super::manifest::Manifest::part_signatures`]
    /// and `feature_signatures`. Suggestion queries only: binding never reads
    /// them. Empty for requirements that were never approved.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reference_signatures: BTreeMap<String, ShapeSignature>,
}

impl BuiltModel {
    pub fn part(&self, name: &str) -> Option<&PartInfo> {
        self.parts.iter().find(|p| p.name == name)
    }
    pub fn param(&self, name: &str) -> Option<&ParamInfo> {
        self.params.iter().find(|p| p.name == name)
    }
    pub fn joint(&self, name: &str) -> Option<&JointInfo> {
        self.joints.iter().find(|j| j.name == name)
    }
}

/// One project model: in the source catalog, and built or not.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelSnapshot {
    pub model: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Set when the build failed (`E_BUILD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_error: Option<String>,
    /// Absent when the model failed to build or was not built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built: Option<BuiltModel>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSnapshot {
    /// The project model catalog, in catalog order.
    pub models: Vec<ModelSnapshot>,
}

impl WorkspaceSnapshot {
    pub fn model(&self, model: &ModelRef) -> Option<&ModelSnapshot> {
        self.models.iter().find(|m| &m.model == model)
    }

    /// Give every built model the shape signatures an approved manifest
    /// recorded, so broken references can offer signature-ranked suggestions
    /// (spec §3.1). The map is keyed `entry#builder/...` exactly like the
    /// manifest's signature maps; it is suggestion data only and never
    /// participates in binding or hashing.
    pub fn attach_reference_signatures(&mut self, signatures: &BTreeMap<String, ShapeSignature>) {
        if signatures.is_empty() {
            return;
        }
        for model in &mut self.models {
            if let Some(built) = &mut model.built {
                built.reference_signatures = signatures.clone();
            }
        }
    }
}

// -------------------------------------------------------------------- adapters
//
// These read the JSON core already produces. `Core::snapshot_for_workspace`
// (in `dispatch.rs`) wires them to a live workspace: the project source
// catalog plus one built model view per model.

/// Models in the project source catalog JSON
/// (`Core::project_source_catalog`: `files[].builders[]`). Only available
/// builders are listed. Entry is the project-relative file, builder the
/// builder id.
pub fn models_from_catalog(catalog: &Value) -> Vec<(ModelRef, Option<String>)> {
    let mut out = Vec::new();
    for file in catalog["files"].as_array().into_iter().flatten() {
        let Some(path) = file["path"].as_str() else {
            continue;
        };
        for b in file["builders"].as_array().into_iter().flatten() {
            if b["available"].as_bool() == Some(false) {
                continue;
            }
            if let Some(id) = b["id"].as_str() {
                out.push((
                    ModelRef {
                        entry: path.to_string(),
                        builder: id.to_string(),
                    },
                    b["label"].as_str().map(str::to_string),
                ));
            }
        }
    }
    out
}

fn quantity_from(value: f64, unit: Option<&str>) -> Quantity {
    // Unknown unit strings are kept as plain numbers rather than guessed.
    let unit = unit.and_then(parse_unit).unwrap_or(Unit::One);
    Quantity::new(value, unit)
}

/// A built model from a `get_project` reply (`model.parts`, `model.parameters`,
/// `mechanism.joints`). Parts without a verified geometry fingerprint get an
/// empty hash. Builder function names, feature kinds and constants come from
/// the worker's `builder_function`, `feature_kinds` and `constants` fields and
/// are `None`/placeholder/empty for revisions built before those existed.
pub fn built_from_project(project: &Value) -> Result<BuiltModel, String> {
    let model = &project["model"];
    if !model["parts"].is_array() {
        return Err("project reply has no built model".into());
    }
    let mut built = BuiltModel {
        fingerprint: model["revision"]
            .as_str()
            .or_else(|| project["revision"].as_str())
            .unwrap_or_default()
            .to_string(),
        ..Default::default()
    };
    // `place()` evidence is emitted at the build-result top level; depending on
    // the reply shape it lands under `model` or next to it.
    if let Some(map) = model
        .get("placements")
        .or_else(|| project.get("placements"))
        .and_then(Value::as_object)
    {
        for (name, record) in map {
            match serde_json::from_value::<PlacementInfo>(record.clone()) {
                Ok(info) => {
                    built.placements.insert(name.clone(), info);
                }
                Err(_) => {
                    // Unknown shape: keep the raw record rather than drop it.
                    built.placements.insert(
                        name.clone(),
                        PlacementInfo {
                            method: record["method"].as_str().unwrap_or("unknown").to_string(),
                            on: None,
                            align: None,
                            offset_mm: None,
                            mate: None,
                            origin: None,
                            extra: BTreeMap::from([("raw".to_string(), record.clone())]),
                        },
                    );
                }
            }
        }
    }
    for p in model["parts"].as_array().into_iter().flatten() {
        let Some(name) = p["name"].as_str() else {
            continue;
        };
        let source_assets = if p["source_assets"].is_null() {
            Vec::new()
        } else {
            serde_json::from_value(p["source_assets"].clone()).map_err(|error| {
                format!("part {name} has invalid source asset metadata: {error}")
            })?
        };
        built
            .part_signatures
            .insert(name.to_string(), signature_from_part(p));
        built.parts.push(PartInfo {
            name: name.to_string(),
            geometry_hash: p["geometry_fingerprint"]["sha256"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            source: p["source"]["file"].as_str().map(|file| SourceInfo {
                file: file.to_string(),
                line: p["source"]["line"].as_u64().unwrap_or(0),
                function: p["builder_function"].as_str().map(str::to_string),
            }),
            fastener: if p["fastener"].is_null() {
                None
            } else {
                Some(
                    serde_json::from_value(p["fastener"].clone()).map_err(|error| {
                        format!("part {name} has invalid fastener metadata: {error}")
                    })?,
                )
            },
            source_assets,
        });
        for (feature, ids) in p["features"].as_object().into_iter().flatten() {
            let kind = &p["feature_kinds"][feature.as_str()];
            let listed = ids.as_array().map_or(0, |a| a.len() as u64);
            built.features.push(FeatureInfo {
                part: name.to_string(),
                name: feature.clone(),
                // Older builds don't say face or edge; a mixed selection is
                // reported as faces (the count still covers every entity).
                entity: match kind["entity"].as_str() {
                    Some("edge") => EntityKind::Edge,
                    _ => EntityKind::Face,
                },
                count: kind["count"].as_u64().unwrap_or(listed),
                hash: kind["hash"].as_str().unwrap_or_default().to_string(),
                source: FeatureSource::Explicit,
                positional: false,
            });
            // The project reply publishes only the selection's face/edge
            // counts per feature — enough for a weak signature ranking, not
            // for histograms (those need an `inspect` reply).
            let suffix = built
                .features
                .last()
                .map(|info| info.link_suffix())
                .unwrap_or_default();
            built.feature_signatures.insert(
                format!("{name}#{suffix}"),
                ShapeSignature {
                    face_count: kind["faces"].as_u64(),
                    edge_count: kind["edges"].as_u64(),
                    ..Default::default()
                },
            );
        }
    }
    // Constants are raw numbers from the entry file. Their unit is the
    // author's, normally millimetres, but counts and ratios are common too, so
    // each value is offered as both.
    for c in model["constants"].as_array().into_iter().flatten() {
        let Some(name) = c["name"].as_str() else {
            continue;
        };
        let numbers: Vec<f64> = c["values"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_f64)
            .collect();
        built.constants.push(ConstantInfo {
            name: name.to_string(),
            values: numbers
                .iter()
                .flat_map(|v| [Quantity::new(*v, Unit::Mm), Quantity::new(*v, Unit::One)])
                .collect(),
        });
    }
    let params: BTreeMap<_, _> = model["parameters"]
        .as_object()
        .into_iter()
        .flatten()
        .collect();
    for (name, p) in params {
        let unit = p["unit"].as_str();
        let Some(value) = p["value"].as_f64().or_else(|| p.as_f64()) else {
            continue;
        };
        built.params.push(ParamInfo {
            name: name.clone(),
            value: quantity_from(value, unit),
            min: p["minimum"].as_f64().map(|v| quantity_from(v, unit)),
            max: p["maximum"].as_f64().map(|v| quantity_from(v, unit)),
        });
    }
    for j in project["mechanism"]["joints"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(id) = j["id"].as_str() {
            built.joints.push(JointInfo {
                name: id.to_string(),
                unit: match j["kind"]["type"].as_str() {
                    Some("prismatic" | "path") => Some("m".into()),
                    Some("revolute") => Some("rad".into()),
                    _ => None,
                },
                limits: j["limits"]
                    .as_array()
                    .and_then(|l| Some([l.first()?.as_f64()?, l.get(1)?.as_f64()?])),
            });
        }
    }
    Ok(built)
}

/// Assemble a snapshot from the catalog JSON and one build result per model
/// (`Ok(get_project reply)` or `Err(build message)`). Models without an entry
/// in `builds` are listed as not built.
pub fn snapshot_from_values(
    catalog: &Value,
    builds: &BTreeMap<ModelRef, Result<Value, String>>,
) -> WorkspaceSnapshot {
    let models = models_from_catalog(catalog)
        .into_iter()
        .map(|(model, label)| {
            let (built, build_error) = match builds.get(&model) {
                None => (None, None),
                Some(Ok(project)) => match built_from_project(project) {
                    Ok(b) => (Some(b), None),
                    Err(e) => (None, Some(e)),
                },
                Some(Err(e)) => (None, Some(e.clone())),
            };
            ModelSnapshot {
                model,
                label,
                build_error,
                built,
            }
        })
        .collect();
    WorkspaceSnapshot { models }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn catalog_lists_available_builders() {
        let catalog = json!({"files": [
            {"path": "machine.py", "builders": [
                {"id": "machine", "label": "Machine", "available": true},
                {"id": "broken", "label": "Broken", "available": false}]},
            {"path": "notes.py", "builders": []}]});
        let models = models_from_catalog(&catalog);
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].0,
            ModelRef {
                entry: "machine.py".into(),
                builder: "machine".into()
            }
        );
        assert_eq!(models[0].1.as_deref(), Some("Machine"));
    }

    #[test]
    fn project_reply_becomes_a_built_model() {
        let project = json!({
            "revision": "r1",
            "model": {
                "parts": [
                    {"name": "door", "geometry_fingerprint": {"format": "x", "sha256": "abc"},
                     "fastener": {"kind": "bolt", "size": "M8", "length": 20.0, "mates": ["frame"]},
                     "source_assets": [{"path":"assets/housing.step", "sha256":"step-hash"}],
                     "source": {"file": "cabinet.py", "line": 12}, "builder_function": "make_door",
                     "features": {"hinge": ["f1", "f2"], "rim": ["edge:0", "edge:1", "edge:2"]},
                     "feature_kinds": {"hinge": {"entity": "face", "count": 2, "hash": "fh"},
                                       "rim": {"entity": "edge", "count": 3, "hash": "eh"}}},
                    {"name": "frame"}],
                "parameters": {"door_angle": {"value": 0.0, "unit": "deg", "minimum": 0.0, "maximum": 130.0},
                               "count": 3.0}, "constants": [{"name": "WIDTH", "values": [30.0]}]},
            "mechanism": {"joints": [{"id": "hinge", "limits": [0.0, 2.0]}]}});
        let b = built_from_project(&project).unwrap();
        assert_eq!(b.fingerprint, "r1");
        assert_eq!(b.part("door").unwrap().geometry_hash, "abc");
        assert_eq!(
            b.part("door").unwrap().source_assets,
            vec![SourceAssetInfo {
                path: "assets/housing.step".into(),
                sha256: "step-hash".into(),
            }]
        );
        assert_eq!(b.part("door").unwrap().source.as_ref().unwrap().line, 12);
        assert_eq!(
            b.part("door").unwrap().fastener.as_ref().unwrap().kind,
            "bolt"
        );
        assert_eq!(b.part("frame").unwrap().geometry_hash, "");
        assert_eq!(
            b.param("door_angle").unwrap().max,
            Some(Quantity::new(130.0, Unit::Deg))
        );
        assert_eq!(
            b.param("count").unwrap().value,
            Quantity::new(3.0, Unit::One)
        );
        assert_eq!(b.joint("hinge").unwrap().limits, Some([0.0, 2.0]));
        assert_eq!(
            b.part("door").unwrap().source.as_ref().unwrap().function,
            Some("make_door".into())
        );
        let feature = |n: &str| b.features.iter().find(|f| f.name == n).unwrap();
        assert_eq!(feature("hinge").count, 2);
        assert_eq!(
            (feature("hinge").entity, feature("hinge").hash.as_str()),
            (EntityKind::Face, "fh")
        );
        assert_eq!(
            (feature("rim").entity, feature("rim").count),
            (EntityKind::Edge, 3)
        );
        let width = b.constants.iter().find(|c| c.name == "WIDTH").unwrap();
        assert!(width.values.contains(&Quantity::new(30.0, Unit::Mm)));
        assert!(width.values.contains(&Quantity::new(30.0, Unit::One)));
        assert!(built_from_project(&json!({})).is_err());
    }

    #[test]
    fn snapshot_marks_failed_and_unbuilt_models() {
        let catalog = json!({"files": [{"path": "a.py", "builders": [
            {"id": "a"}, {"id": "b"}, {"id": "c"}]}]});
        let key = |b: &str| ModelRef {
            entry: "a.py".into(),
            builder: b.into(),
        };
        let mut builds = BTreeMap::new();
        builds.insert(key("a"), Ok(json!({"model": {"parts": []}})));
        builds.insert(key("b"), Err("boom".to_string()));
        let snap = snapshot_from_values(&catalog, &builds);
        assert!(snap.model(&key("a")).unwrap().built.is_some());
        assert_eq!(
            snap.model(&key("b")).unwrap().build_error.as_deref(),
            Some("boom")
        );
        let c = snap.model(&key("c")).unwrap();
        assert!(c.built.is_none() && c.build_error.is_none());
    }

    #[test]
    fn project_reply_records_part_feature_signatures_and_placements() {
        let project = json!({
            "model": {
                "parts": [{
                    "name": "arm",
                    "volume": 1200.0, "area": 640.0, "shape_kind": "Solid",
                    "bounds": {"size": [40.0, 20.0, 10.0]},
                    "face_count": 6, "edge_count": 12,
                    "features": {"wall": ["f1"]},
                    "feature_kinds": {"wall": {"entity": "face", "count": 1, "hash": "h",
                                               "faces": 1, "edges": 0}}}],
                "placements": {"arm": {"method": "place", "on": {"kind": "face", "part": "base"},
                                       "align": "top", "offset_mm": 5.0}}}});
        let b = built_from_project(&project).unwrap();
        let part_sig = b.part_signatures.get("arm").expect("part signature");
        assert_eq!(part_sig.volume_mm3, Some(1200.0));
        assert_eq!(part_sig.area_mm2, Some(640.0));
        assert_eq!(part_sig.extents_mm, Some([40.0, 20.0, 10.0]));
        assert_eq!(part_sig.face_count, Some(6));
        assert_eq!(part_sig.edge_count, Some(12));
        assert_eq!(part_sig.shape_kind.as_deref(), Some("Solid"));
        let feature_sig = b
            .feature_signatures
            .get("arm#wall")
            .expect("feature signature");
        assert_eq!(feature_sig.face_count, Some(1));
        assert_eq!(feature_sig.edge_count, Some(0));
        let placement = b.placements.get("arm").expect("placement");
        assert_eq!(placement.method, "place");
        assert_eq!(placement.align.as_deref(), Some("top"));
        assert_eq!(placement.offset_mm, Some(5.0));
        assert!(placement.on.is_some());
    }
}
