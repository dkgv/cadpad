//! Exact geometry result contracts shared by the native dispatcher and agents.
//! Display meshes and collision proxies have separate accuracy contracts.
use crate::{annotations, collision, kinematics, tools};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, JsonSchema)]
pub enum Units {
    #[serde(rename = "mm")]
    Millimetres,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Accuracy {
    Exact,
    Mesh,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Bounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub size: [f64; 3],
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ExactContext {
    pub revision: String,
    pub pose_version: u64,
    pub accuracy: Accuracy,
    pub units: Units,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SelectedPart {
    pub instance_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,
    pub bounds: Bounds,
    pub volume: Option<f64>,
    pub features: BTreeMap<String, Vec<String>>,
    pub units: Units,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SelectionReply {
    pub workspace_id: String,
    pub revision: String,
    pub entities: Vec<tools::Entity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_names: Option<HistorySelection>,
    pub parts: Vec<SelectedPart>,
    pub expired: bool,
    pub pose_version: u64,
    pub joint_states: Vec<kinematics::JointState>,
    pub units: Units,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct HistoryReference {
    pub name: String,
    pub entity_kind: String,
    pub exact_hash: String,
    pub count: u64,
    pub set_hash: String,
    pub positional: bool,
    pub basis: String,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SelectedHistory {
    pub reference: tools::Entity,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry_reference: Option<annotations::GeometryRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub references: Vec<HistoryReference>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct HistorySelection {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub entities: Vec<SelectedHistory>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSpace {
    PartLocal,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Fingerprint {
    pub format: String,
    pub sha256: String,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverallKind {
    Overall,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct OverallDimension {
    pub id: String,
    pub kind: OverallKind,
    pub axis: annotations::Axis,
    pub value: f64,
    pub unit: Units,
}
/// Project-relative Python file and line of the `show()` call that published a part.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SourceLocation {
    pub file: String,
    pub line: u64,
}
/// Explicit fastener properties published by the source model. Dimensions are
/// metadata supplied by the author; size labels never imply standard values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FastenerMetadata {
    pub kind: String,
    pub size: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mates: Option<Vec<String>>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct PartProperties {
    pub id: String,
    pub instance_id: String,
    pub name: String,
    pub position: [f64; 3],
    pub volume: Option<f64>,
    pub area: f64,
    pub shape_kind: String,
    pub face_count: u64,
    pub edge_count: u64,
    pub features: BTreeMap<String, Vec<String>>,
    /// Entity kind, counts and selection hash per named feature.
    #[serde(default)]
    pub feature_kinds: BTreeMap<String, FeatureKind>,
    pub accuracy: Accuracy,
    pub units: Units,
    pub geometry_fingerprint: Fingerprint,
    pub dimensions: Vec<OverallDimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mass_kg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,
    /// Project function whose `show()` published this part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder_function: Option<String>,
    /// Explicit fastener metadata, when supplied to `show()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fastener: Option<FastenerMetadata>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct FeatureKind {
    /// `face`, `edge` or `mixed`.
    pub entity: String,
    pub count: u64,
    pub faces: u64,
    pub edges: u64,
    /// Identifies the selection within this exact geometry.
    pub hash: String,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeometryCenter {
    GeometryCenter {
        instance_id: String,
        entity: annotations::GeometryRef,
    },
    LocalPoint {
        instance_id: String,
        point: [f64; 3],
    },
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeometryCircle {
    GeometryCircle {
        instance_id: String,
        entity: annotations::GeometryRef,
        measure: annotations::CircleMeasure,
    },
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct FaceProperties {
    pub surface_type: String,
    pub area: f64,
    pub anchor: GeometryCenter,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circular_definition: Option<GeometryCircle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub world_normal: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct EdgeProperties {
    pub curve_type: String,
    pub length: f64,
    pub anchor: GeometryCenter,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circular_definition: Option<GeometryCircle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntityProperties {
    Part(PartProperties),
    Face(FaceProperties),
    Edge(EdgeProperties),
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct InspectedEntity {
    pub reference: tools::Entity,
    pub bounds: Bounds,
    pub center: [f64; 3],
    pub coordinate_space: CoordinateSpace,
    pub world_bounds: Bounds,
    pub world_center: [f64; 3],
    pub semantic_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_references: Option<Vec<HistoryReference>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_references_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_references_reason: Option<String>,
    #[serde(flatten)]
    pub properties: EntityProperties,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ExactPair {
    pub instances: [String; 2],
    pub distance_mm: f64,
    pub intersection_volume_mm3: f64,
    pub interfering: bool,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct InterferenceData {
    pub pairs: Vec<ExactPair>,
    pub kernel_tolerance_mm: f64,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ExactInterference {
    #[serde(flatten)]
    pub context: ExactContext,
    #[serde(flatten)]
    pub data: InterferenceData,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum InspectionData {
    Entities { entities: Vec<InspectedEntity> },
    Distance { distance: f64 },
    Interference(InterferenceData),
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct InspectionReply {
    #[serde(flatten)]
    pub context: ExactContext,
    #[serde(flatten)]
    pub data: InspectionData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_names: Option<HistoryTraceStatus>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct HistoryTraceStatus {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct InterferenceReply {
    pub workspace_id: String,
    pub revision: String,
    pub pose_version: u64,
    pub proxy: Option<collision::Report>,
    pub exact: Option<ExactInterference>,
}

#[cfg(test)]
mod fastener_contract_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn part_properties_round_trip_explicit_fastener_metadata() {
        let value = json!({
            "id": "part-id", "instance_id": "bolt", "name": "bolt", "position": [0,0,0],
            "volume": 10.0, "area": 20.0, "shape_kind": "Solid", "face_count": 6,
            "edge_count": 12, "features": {}, "feature_kinds": {}, "accuracy": "exact",
            "units": "mm", "geometry_fingerprint": {"format": "brep", "sha256": "hash"},
            "dimensions": [], "fastener": {"kind": "bolt", "size": "M8", "length": 20.0, "mates": ["plate"]}
        });
        let part: PartProperties = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(part).unwrap()["fastener"],
            value["fastener"]
        );
    }
}
