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
    pub parts: Vec<SelectedPart>,
    pub expired: bool,
    pub pose_version: u64,
    pub joint_states: Vec<kinematics::JointState>,
    pub units: Units,
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
    pub accuracy: Accuracy,
    pub units: Units,
    pub geometry_fingerprint: Fingerprint,
    pub dimensions: Vec<OverallDimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mass_kg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeometryCenter {
    GeometryCenter {
        instance_id: String,
        entity: annotations::GeometryRef,
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
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct InterferenceReply {
    pub workspace_id: String,
    pub revision: String,
    pub pose_version: u64,
    pub proxy: Option<collision::Report>,
    pub exact: Option<ExactInterference>,
}
