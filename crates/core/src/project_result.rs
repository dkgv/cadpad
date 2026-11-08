//! Agent-facing project context. Portable source retains its exact JSON field
//! presence while remaining validated and described by the underlying Rust type.
use crate::{JobKind, JobState, annotations, geometry, kinematics, motion, playback, tools};
use schemars::{JsonSchema, Schema, SchemaGenerator, generate::SchemaSettings};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::Value;
use std::{borrow::Cow, collections::BTreeMap, marker::PhantomData};

pub(crate) fn validate_publication(model: &Value) -> crate::Result<()> {
    Model::deserialize(model).map_err(|error| {
        crate::Error::domain(
            "INVALID_GEOMETRY",
            format!("Publication metadata does not match the project contract: {error}"),
        )
    })?;
    Ok(())
}

/// A validated JSON representation of T, not a normalized reserialization of T.
/// For example, `{}` is valid mechanism source and must stay `{}` on a read.
pub struct Source<T> {
    value: Value,
    marker: PhantomData<T>,
}
impl<'de, T: DeserializeOwned> Deserialize<'de> for Source<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        T::deserialize(&value).map_err(serde::de::Error::custom)?;
        Ok(Self {
            value,
            marker: PhantomData,
        })
    }
}
impl<T> Serialize for Source<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
}
impl<T: JsonSchema> JsonSchema for Source<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("Source_{}", T::schema_name()).into()
    }
    fn schema_id() -> Cow<'static, str> {
        format!("cadpad::Source<{}>", T::schema_id()).into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        // Source is emitted as originally accepted: serde defaults stay optional.
        // These portable types are acyclic. Inline their subschemas so their
        // references cannot collide with the outer result's serialization defs.
        let mut schema = SchemaSettings::draft2020_12()
            .for_deserialize()
            .with(|settings| settings.inline_subschemas = true)
            .into_generator()
            .into_root_schema_for::<T>();
        assert!(
            schema.get("$defs").is_none(),
            "Portable source schema must be acyclic"
        );
        schema.remove("$schema");
        schema
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceListing {
    pub workspace_ids: Vec<String>,
    pub protocol_version: u32,
    pub tools: Vec<String>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ParameterValue {
    Number(f64),
    Published(Parameter),
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Parameter {
    pub value: f64,
    pub unit: String,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub description: Option<String>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Part {
    pub id: String,
    pub instance_id: String,
    pub name: String,
    pub position: [f64; 3],
    pub bounds: geometry::Bounds,
    pub volume: Option<f64>,
    pub area: f64,
    pub shape_kind: String,
    pub face_count: u64,
    pub edge_count: u64,
    pub features: BTreeMap<String, Vec<String>>,
    pub accuracy: geometry::Accuracy,
    pub units: geometry::Units,
    // Legacy revisions can lack a verified fingerprint and report display_error.
    pub geometry_fingerprint: Option<geometry::Fingerprint>,
    pub dimensions: Vec<geometry::OverallDimension>,
    pub mass_kg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<geometry::SourceLocation>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DimensionKind {
    Distance,
    Angle,
    Overall,
    Radius,
    Diameter,
    Published,
    GeometryCircle,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DimensionStatus {
    Resolved,
    Broken,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub enum DimensionUnit {
    #[serde(rename = "mm")]
    Millimetres,
    #[serde(rename = "rad")]
    Radians,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct DimensionError {
    pub code: String,
    pub message: String,
}
/// Publication, automatic, resolved and broken records share this typed vocabulary.
/// Missing measurements on broken records remain missing in Source<Model>.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Dimension {
    pub id: String,
    pub label: String,
    pub kind: DimensionKind,
    pub parameter: Option<String>,
    pub unit: DimensionUnit,
    pub part: Option<String>,
    pub end_part: Option<String>,
    pub vertex_part: Option<String>,
    pub start: Option<[f64; 3]>,
    pub end: Option<[f64; 3]>,
    pub vertex: Option<[f64; 3]>,
    pub value: Option<f64>,
    pub saved_id: Option<String>,
    pub definition: Option<annotations::Definition>,
    pub format: Option<annotations::Format>,
    pub label_offset: Option<[f64; 2]>,
    pub status: Option<DimensionStatus>,
    pub accuracy: Option<geometry::Accuracy>,
    pub error: Option<DimensionError>,
    pub automatic: Option<bool>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageMethod {
    BrepBooleanCoverage,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Coverage {
    pub method: CoverageMethod,
    pub covered_instances: Vec<String>,
    pub kernel_tolerance_mm: f64,
    pub bounds_padding_mm: f64,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Model {
    pub parts: Vec<Part>,
    pub parameters: Option<BTreeMap<String, Parameter>>,
    pub annotations: Option<Vec<Dimension>>,
    pub units: Option<geometry::Units>,
    pub revision: Option<String>,
    pub dependencies: Option<BTreeMap<String, String>>,
    pub collision_validation: Option<Coverage>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Diagnostics {
    pub code: Option<String>,
    pub message: Option<String>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Activity {
    pub kind: JobKind,
    pub id: String,
    pub draft_id: String,
    pub snapshot_id: Option<String>,
    pub actor: String,
    pub state: JobState,
    pub resulting_revision: Option<String>,
    pub diagnostics: Diagnostics,
    pub created_at: u64,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Project {
    pub workspace_id: String,
    pub revision: String,
    pub display_generation: String,
    pub display_error: Option<Source<tools::ToolFailure>>,
    pub files: Vec<String>,
    pub parameters: Source<BTreeMap<String, ParameterValue>>,
    pub entry_point: String,
    pub model: Source<Model>,
    #[schemars(length(max = 20))]
    pub activity: Vec<Activity>,
    pub total_jobs: usize,
    pub protocol_version: u32,
    pub assets: BTreeMap<String, String>,
    pub dependencies: BTreeMap<String, String>,
    pub annotations: Source<Vec<annotations::Annotation>>,
    pub mechanism: Source<kinematics::Definition>,
    pub motion: motion::Snapshot,
    pub external_tracking: bool,
    pub recordings: Vec<playback::Metadata>,
    pub name: Option<String>,
    pub saved_revision: Option<String>,
    pub dirty: bool,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Reply {
    Listing(WorkspaceListing),
    Project(Box<Project>),
}
