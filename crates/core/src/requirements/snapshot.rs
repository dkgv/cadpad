//! A manifest-like snapshot of a built workspace: everything binding needs, as
//! plain data. Binding runs against this type, so it is testable without the
//! worker; the adapters at the bottom produce it from existing core data (the
//! `get_project` JSON and the project source catalog).
use super::ir::{EntityKind, ModelRef};
use super::parse::parse_unit;
use super::quantity::{Quantity, Unit};
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeatureInfo {
    pub part: String,
    pub name: String,
    pub entity: EntityKind,
    /// Number of entities the name selects.
    pub count: u64,
    pub hash: String,
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
}

// -------------------------------------------------------------------- adapters
//
// These read the JSON core already produces. Wiring them to a live workspace
// (calling `project_source_catalog`, then `open_project_model` per catalog
// model and passing each `get_project` reply here) is the remaining TODO: it
// needs one worker build per model, which belongs with the run lifecycle
// (P1.40 link from a fresh build).

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
/// empty hash. Function names and constants are not in that reply yet: they
/// stay `None` and empty.
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
    for p in model["parts"].as_array().into_iter().flatten() {
        let Some(name) = p["name"].as_str() else {
            continue;
        };
        built.parts.push(PartInfo {
            name: name.to_string(),
            geometry_hash: p["geometry_fingerprint"]["sha256"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            source: p["source"]["file"].as_str().map(|file| SourceInfo {
                file: file.to_string(),
                line: p["source"]["line"].as_u64().unwrap_or(0),
                function: None,
            }),
        });
        for (feature, ids) in p["features"].as_object().into_iter().flatten() {
            built.features.push(FeatureInfo {
                part: name.to_string(),
                name: feature.clone(),
                // The published feature record doesn't say face or edge yet.
                entity: EntityKind::Face,
                count: ids.as_array().map_or(0, |a| a.len() as u64),
                hash: String::new(),
            });
        }
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
                     "source": {"file": "cabinet.py", "line": 12}, "features": {"hinge": ["f1", "f2"]}},
                    {"name": "frame"}],
                "parameters": {"door_angle": {"value": 0.0, "unit": "deg", "minimum": 0.0, "maximum": 130.0},
                               "count": 3.0}},
            "mechanism": {"joints": [{"id": "hinge", "limits": [0.0, 2.0]}]}});
        let b = built_from_project(&project).unwrap();
        assert_eq!(b.fingerprint, "r1");
        assert_eq!(b.part("door").unwrap().geometry_hash, "abc");
        assert_eq!(b.part("door").unwrap().source.as_ref().unwrap().line, 12);
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
        assert_eq!(b.features[0].count, 2);
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
}
