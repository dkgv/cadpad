//! Versioned shared tool contracts and explicit transport projections. Actor identity comes exclusively
//! from the authenticated transport's ToolContext, never model-generated JSON.
use crate::{Core, Error, Result};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const PROTOCOL_VERSION: u32 = 1;
pub struct ToolContext {
    pub actor: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetProject {
    pub workspace_id: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadSource {
    workspace_id: String,
    version: String,
    path: String,
    /// Request a bounded page even when no pagination arguments are supplied.
    paged: Option<bool>,
    /// One-based first line. Defaults to 1.
    start_line: Option<usize>,
    /// Maximum lines to read (1–200). Defaults to 80.
    max_lines: Option<usize>,
    /// UTF-8 byte offset within the first line. Use next_byte_offset from a previous page.
    byte_offset: Option<usize>,
}

#[derive(Serialize, JsonSchema)]
struct SourceLines {
    path: String,
    hash: String,
    source: String,
    start_line: usize,
    byte_offset: usize,
    next_line: Option<usize>,
    next_byte_offset: usize,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchSource {
    workspace_id: String,
    version: String,
    path: String,
    /// Case-sensitive literal text within a single line.
    query: String,
    /// One-based first line to search. Defaults to 1.
    start_line: Option<usize>,
    /// Maximum matching lines (1–50). Defaults to 20.
    max_matches: Option<usize>,
}

#[derive(Serialize, JsonSchema)]
struct SourceMatch {
    line: usize,
    /// Zero-based UTF-8 byte offset of the first match on this line.
    byte_offset: usize,
    /// Bounded excerpt around the first match.
    preview: String,
}

#[derive(Serialize, JsonSchema)]
struct SourceMatches {
    path: String,
    hash: String,
    matches: Vec<SourceMatch>,
    next_line: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FindSymbol {
    workspace_id: String,
    version: String,
    /// Exact, case-sensitive Python identifier, for example make_slab.
    symbol: String,
    /// Optional project-relative Python path to search.
    path: Option<String>,
    kind: Option<SymbolKind>,
    /// Maximum matches (1–50). Defaults to 20.
    max_matches: Option<usize>,
    /// Use next_cursor with the same version, symbol, path and kind.
    cursor: Option<SymbolCursor>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SymbolKind {
    Definition,
    Import,
    Call,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SymbolCursor {
    path: String,
    skip: usize,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct SymbolHit {
    path: String,
    line: usize,
    /// Zero-based UTF-8 byte column.
    byte_column: usize,
    kind: SymbolKind,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct SymbolReply {
    matches: Vec<SymbolHit>,
    /// Up to 20 parser diagnostics from the scanned portion of the page.
    diagnostics: Vec<String>,
    next_cursor: Option<SymbolCursor>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSource {
    pub workspace_id: String,
    pub expected_base_revision: String,
    pub draft_id: Option<String>,
    pub changes: BTreeMap<String, String>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
pub struct SourceContent {
    pub path: String,
    pub source: String,
    pub hash: String,
}
#[derive(Serialize, JsonSchema)]
#[serde(untagged)]
enum SourceReply {
    Page(SourceLines),
    Full(SourceContent),
}

#[derive(Deserialize, Serialize, JsonSchema)]
pub struct ChangedSource {
    pub path: String,
    /// UTF-8 byte length, rather than a character count.
    pub bytes: usize,
    pub hash: String,
}
#[derive(Deserialize, Serialize, JsonSchema)]
pub struct SourceUpdate {
    pub id: String,
    pub base_revision: String,
    /// All draft files differing from the base, including earlier edits.
    pub changed_files: Vec<ChangedSource>,
    /// Original hashes of the base revision's files.
    pub source_hashes: BTreeMap<String, String>,
    pub actor: String,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub workspace_id: String,
    pub expected_base_revision: String,
    pub draft_id: String,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub workspace_id: String,
    pub job_id: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    pub revision: String,
    pub instance_id: String,
    pub entity_id: String,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Inspect {
    pub workspace_id: String,
    pub revision: String,
    pub entities: Vec<Entity>,
    pub query: Option<String>,
    pub pose_version: Option<u64>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TraceMeshEdges {
    workspace_id: String,
    revision: String,
    /// One to sixteen selected STL edges, in selection order.
    entities: Vec<Entity>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TracePartSource {
    workspace_id: String,
    revision: String,
    instance_id: String,
}

#[derive(Serialize, JsonSchema)]
struct PartSourceTrace {
    revision: String,
    instance_id: String,
    /// Known component publication site. This is not face or edge provenance.
    publication: Option<crate::geometry::SourceLocation>,
    /// Asset path when the linked source is a mesh or CAD exchange file.
    asset_path: Option<String>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TraceCadEdge {
    workspace_id: String,
    revision: String,
    /// Pass the current pose version for a moving model.
    pose_version: Option<u64>,
    /// One to sixteen native edge references.
    entities: Vec<Entity>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MatchMeshEdge {
    /// Workspace containing the selected STL edges.
    workspace_id: String,
    revision: String,
    entities: Vec<Entity>,
    /// Workspace containing the corresponding native CAD model or STEP file.
    target_workspace_id: String,
    target_revision: String,
    target_instance_id: String,
    target_pose_version: Option<u64>,
    /// Maximum sampled point-to-curve distance, in millimetres. Defaults to 0.15.
    tolerance_mm: Option<f64>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct CadOperationSite {
    file: String,
    line: u64,
    operation: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct CadIncidentFace {
    face_id: String,
    surface_type: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct CadEdgeTrace {
    reference: Entity,
    curve_type: String,
    length_mm: f64,
    incident_face_count: usize,
    incident_faces: Vec<CadIncidentFace>,
    /// Observed operation whose returned edge shares the published topology identity.
    operation: Option<CadOperationSite>,
    publication: Option<crate::geometry::SourceLocation>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct CadEdgeTraceReply {
    revision: String,
    accuracy: crate::geometry::Accuracy,
    units: crate::geometry::Units,
    edges: Vec<CadEdgeTrace>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct CadEdgeCandidate {
    reference: Entity,
    max_sample_distance_mm: f64,
    curve_type: String,
    operation: Option<CadOperationSite>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct MeshEdgeMatch {
    candidate_count: usize,
    candidates: Vec<CadEdgeCandidate>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct MeshEdgeMatchResult {
    target_revision: String,
    target_pose_version: u64,
    tolerance_mm: f64,
    /// One result per source entity, in the same order. Multiple candidates mean the match is ambiguous.
    matches: Vec<MeshEdgeMatch>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetParameter {
    pub workspace_id: String,
    pub expected_base_revision: String,
    pub parameter_id: String,
    pub value: f64,
    pub unit: String,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub workspace_id: String,
    pub revision: String,
    pub format: String,
    pub pose_version: Option<u64>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Undo {
    pub workspace_id: String,
    pub expected_current_revision: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckPrintability {
    pub workspace_id: String,
    /// Must be the current committed revision.
    pub revision: String,
    pub pose_version: Option<u64>,
    /// fdm | resin | sla | generic. Defaults to fdm.
    pub process: Option<String>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetSelection {
    pub workspace_id: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetAnnotation {
    pub workspace_id: String,
    pub expected_base_revision: String,
    pub annotation: crate::annotations::Annotation,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteAnnotation {
    pub workspace_id: String,
    pub expected_base_revision: String,
    pub annotation_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Structured content after an MCP image/resource adapter transformation.
    /// Absent when the native and MCP shapes are identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_output_schema: Option<Value>,
}
/// Machine-readable execution failure. Non-domain errors omit details.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ToolFailure {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}
/// MCP keeps isError outside structuredContent; both structured variants must
/// satisfy outputSchema. Native dispatch still returns Result, without wrapping.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum StructuredResult<T> {
    Success(T),
    Failure(ToolFailure),
}
fn result_schema<T: JsonSchema>() -> Value {
    let schema = schemars::generate::SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<StructuredResult<T>>();
    let mut value = serde_json::to_value(schema).expect("output schema serialization");
    value["type"] = json!("object");
    value
}
fn spec<T: JsonSchema>(name: &str, description: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: serde_json::to_value(schema_for!(T)).expect("schema serialization"),
        output_schema: None,
        mcp_output_schema: None,
    }
}
fn build_definitions() -> Vec<ToolSpec> {
    let mut tools = vec![
        spec::<GetProject>(
            "get_project",
            "List workspaces, or inspect one explicit workspace and its committed revision, parameters, parts and activity.",
        ),
        spec::<GetSelection>(
            "get_selection",
            "Get the current revision-scoped selection, semantic labels, units and bounded exact part context. Entities are in the user's selection order; with several, the viewport badges them A, B, C…. Parts include the component source file and show() line when known. Expired references are reported instead of relocated silently.",
        ),
        spec::<crate::views::CaptureViews>(
            "capture_views",
            "Capture PNGs from the connected viewport at the explicit workspace, revision and pose. By default capture one to four preset views (default iso/front/right/top, 640×480) and save a temporary baseline. Returns labeled images, camera/pose context and capture_id for compare_views; only the last eight sets are retained across the app. Set single=true for a standalone PNG with numeric context and no baseline. In single mode omit views, optionally pass camera or camera_preset, or use the current camera; default size is 1024×768. Both modes accept width, height and dimension visibility options. Images are not authoritative measurements.",
        ),
        spec::<crate::views::CompareViews>(
            "compare_views",
            "Compare the current model against a capture_views baseline owned by this session/workspace. Reuses baseline cameras, dimensions and image sizes; returns labeled before/after PNG pairs and revision/pose metadata. Capture the baseline before editing; historical revisions cannot be rendered on demand. Camera stays fixed, so enlarged geometry may extend outside the frame.",
        ),
        spec::<crate::views::SetDimensions>(
            "set_dimensions",
            "Set transient dimension visibility and selected/pinned/all-visible scope in the connected viewport. This does not create or edit saved annotation definitions.",
        ),
        spec::<ReadSource>(
            "read_source",
            "Read a project-relative Python file from a revision, editable draft, or immutable job snapshot, with its content hash. With no paging options, return the full file. For large files or a known line, set paged=true or pass start_line, max_lines, or byte_offset to read up to 8 KiB (default starting line 1 and 80 lines, maximum 200 lines). Continue with next_line as start_line and next_byte_offset as byte_offset; null next_line means the end. Use search_source to locate literal text first.",
        ),
        spec::<SearchSource>(
            "search_source",
            "Find a part name, builder name, or other case-sensitive literal in one project-relative Python file. Use the revision, draft ID, or job snapshot ID as version. Returns one hit per matching line with a short excerpt. Pass next_line as start_line to continue, or use read_source with start_line to inspect a hit in context.",
        ),
        spec::<FindSymbol>(
            "find_symbol",
            "Find an exact Python identifier across a project revision, draft, or job snapshot. Reports function/class definitions, imports including aliases, and calls by name or attribute, without executing project code. Optional path and kind narrow the search. Results are syntactic candidates and do not resolve dynamic dispatch. Use read_source with start_line at a hit and pass next_cursor as cursor to scan more files or matches, even if the current page has no hits.",
        ),
        spec::<SetAnnotation>(
            "set_annotation",
            "Save or repair an individual pinned dimension, then rebuild as an undoable revision. Anchors use named instances, local points/bounds or unique semantic features, never stale face indices. Values regenerate; unresolved anchors are marked broken without stale numbers. Lengths remain mm and three-anchor angles remain radians; angle display requires deg/rad, length display mm/cm/m/in. Format controls display only.",
        ),
        spec::<DeleteAnnotation>(
            "delete_annotation",
            "Remove one saved annotation in an undoable revision and return a build job.",
        ),
        spec::<UpdateSource>(
            "update_source",
            "Stage full file replacements in a candidate draft. Does not execute or alter the committed scene. Relative Python paths only; no implicit deletion.",
        ),
        spec::<Build>(
            "build",
            "Build an immutable draft snapshot in a separate worker. Returns a job immediately; only valid success on an unchanged base commits.",
        ),
        spec::<crate::recovery::RepairDisplay>(
            "repair_display",
            "Restore missing/corrupt display buffers in a cancellable job, with explicit workspace and current revision. Reuses a live native worker or rebuilds committed inputs without tessellation, verifies exact geometry/publication fingerprints, then regenerates buffers. Does not change design revision, saved state or live poses. A changed recovery result fails and requires an explicit new build revision; context reads never execute model code for cache repair. Returns the ordinary job contract; use get_job/cancel_job.",
        ),
        spec::<Job>(
            "get_job",
            "Poll build state and bounded diagnostics. snapshot_id identifies the durable immutable inputs readable through read_source; draft_id remains the editable repair target. Failed drafts and original job snapshots survive recovery.",
        ),
        spec::<Job>(
            "cancel_job",
            "Request cancellation. Poll until terminal state; cancellation is complete only after native execution stops.",
        ),
        spec::<Inspect>(
            "inspect",
            "Query CAD or STL mesh properties and distances between revision-scoped entities. The result reports exact CAD or mesh accuracy. Coordinates and lengths are millimetres; quaternions use xyzw.",
        ),
        spec::<TraceMeshEdges>(
            "trace_mesh_edges",
            "Use when the user asks what selected STL edges separate. Pass one to sixteen edge references from the message or get_selection, in the same order and with their current revision. Returns endpoints in part-local millimetres, incident face IDs and normals, and the unsigned angle between two incident facet planes in degrees. Classifies each edge as open, coplanar, crease, or non-manifold. The result describes mesh topology and cannot establish which CAD operation made the edge.",
        ),
        spec::<MatchMeshEdge>(
            "match_mesh_edge",
            "Match selected STL segments against one instance in a corresponding native CAD or STEP revision. Pass STL edge references, an explicit target workspace, revision and instance, and its current pose version if moving. The mesh and posed CAD model must already share millimetre coordinates. Ranks up to five candidates by the maximum of five sampled distances to exact curves. Zero candidates means no match within tolerance; multiple candidates are ambiguous. A geometric match does not establish shared build history. Candidates include an observed Python operation site when available. Use trace_cad_edge for native face adjacency.",
        ),
        spec::<TraceCadEdge>(
            "trace_cad_edge",
            "Trace selected native CAD edges to their incident BRep faces and any observed Python operation that returned the surviving edge topology. Use edge references from get_selection or match_mesh_edge. A null operation means no reliable operation site was captured, for example with imported STEP, cached geometry, or unsupported operations. The publication location, when available, is the part's show() site. Use read_source with start_line to inspect either location.",
        ),
        spec::<TracePartSource>(
            "trace_part_source",
            "Use when the user asks where a selected face, edge, or part comes from. Pass its instance_id and current revision from the message or get_selection. Returns a Python show() file and line when known, or the path of a linked STL or STEP asset. Use read_source with start_line around the publication line, then find_symbol for the builder. This traces the containing part. Individual face and edge creation history is unavailable.",
        ),
        spec::<SetParameter>(
            "set_parameter",
            "Stage and build a published numeric parameter. Keep the last committed model and values on failure.",
        ),
        spec::<crate::motion::DefineMechanism>(
            "define_mechanism",
            "Save and build a complete mechanism definition as an undoable design revision. Optional draft_id combines staged source changes and joint definitions atomically. Fixed, prismatic, revolute and arc-length path joints use explicit local frames in metres, normalized xyzw quaternions and unit axes. Moving joints require limits and initial coordinates in m or rad. Paths use metre line/arc segments with continuous tangents and a nonparallel up direction; closed tracks distribute transport roll to close their orientation seam. Path coordinates are arc length in metres. Base joints form a spanning tree. Optional residual constraints solve declared passive joints for coordinate ratios, point coincidence, distance, aligned axes and closing hinge frames. Initial coordinates select the assembly branch. Limits, singularities, infeasible targets and uncertifiable motion reject the request; optional collision blocking requires conservative BRep-verified proxies for every instance. Compound box/sphere pieces preserve openings; bounds proxies can fill them.",
        ),
        spec::<crate::motion::GetMotion>(
            "get_motion",
            "Get authoritative revision/pose version, joint coordinates (m/rad) and instance poses (mm, xyzw). Manipulation does not rebuild geometry. Collision metadata states whether swept blocking is enabled, its approximate proxy accuracy, clearance and ignored pairs.",
        ),
        spec::<crate::motion::SetCoordinate>(
            "set_joint_coordinate",
            "Set a manipulation joint coordinate with expected revision and pose version. Clamp to explicit limits and evaluate downstream poses without rebuilding CAD. Requires m for a slider/path or rad for a hinge; returns the authoritative snapshot. When configured, swept conservative proxy checks block at the first impact with backoff unless check_collision is false for interactive preview (endpoint solve only; a later call with check_collision true commits from the last validated pose). Results identify limiting pairs; numerical/unsupported queries reject the target. Dynamics is separate.",
        ),
        spec::<crate::playback::Control>(
            "control_playback",
            "List/start_recording/finish_recording/cancel_recording/select/leave/delete revision-bound motion recordings. Uses explicit workspace, current revision and expected pose version. Records every accepted simulation tick or manipulation target, with mm poses, joint coordinates and monotonic seconds. Completed recordings persist locally and travel with native project Save/Open; design geometry stays unchanged. Imported current-design recordings rebind only after the first explicit rebuild verifies source inputs, exact geometry/publication fingerprints and sample compatibility. Changed designs retain stale archived recordings. Up to 3600 frames/32768 body-frames per recording, 8 MiB each, 16 recordings/64 MiB per workspace; capacity truncation is explicit. Select an existing frame for exact pose-tagged inspection/export; live manipulation/simulation is frozen until leave restores its prior pose. Another revision's recording cannot be applied. List returns metadata; select also returns bounded frame times. Finish recording before switching modes.",
        ),
        spec::<crate::dynamics::Control>(
            "control_simulation",
            "Start/advance/motor/loads/stop fixed-step f64 Simulate mode at an explicit revision and expected pose version. Start requires explicit fixed/dynamic bodies, mass in kg or density in kg/m³, friction, restitution and gravity in m/s², plus BRep-verified collision proxies. Fixed/prismatic/revolute trees and closing hinge, coincident-point, fixed-distance and aligned-axis constraints are supported. Revolute and prismatic gear couplings are enforced as workless coordinate constraints, including parallel-axis inertia for offset hinges and optional backlash tooth contact. Prescribed paths are integrated along arc length using tangent mass and frame inertia; on-track contact impulses from the Rapier tick project onto the path speed. get_motion reports simulation_unavailable_reason when a mechanism cannot be simulated. Hinge limits must stay within −π..π. Closure residuals measure actual physical poses and never certify a geometric swept path. Passive joints cannot be motor targets. Advance takes 1..120 ticks at 1/120 s; returns adjacent authoritative frames for rendering. Motors use force-based bounded springs in N or N m and m/rad targets. Stop restores the pre-simulation manipulation pose. Simulated contacts and proxy inertias are approximate; exact pose-tagged CAD queries/exports remain available. The loads command atomically replaces all persistent world-space body loads: force_n (N) acts at the center of mass and torque_nm (N m) is an independent couple. Only dynamic bodies accept loads; each component must be finite and at most 1e9 in absolute value. Empty loads clears all external loads. Loads persist across ticks without accumulating on repeated replacement, and stop clears the simulation. Simulation never rewrites design geometry.",
        ),
        spec::<crate::motion_tests::Control>(
            "control_motion_tests",
            "Create/list/edit/delete/run/replay saved CAD simulation tests (requirements) from natural-language intent. A project keeps one requirement catalog shared by all of its model workspaces; each requirement records definition.model (source entry + builder) and is listed, run and replayed through that model's workspace. Address the model's workspace: upsert without definition.model assigns that model; run/replay of a requirement assigned to another model is rejected. Addressing the project workspace edits the catalog only. List first for catalog CAS. Ground body/joint IDs in get_project/get_motion; inspect the full mechanism before defining actions. Upsert saves a named intent, explicit initial coordinates for every moving joint (m/rad), body masses/friction/gravity, timed motor/load actions and measurable checks. Ask the user to resolve missing targets/tolerances rather than inventing a requirement. Every list/run/upsert reply includes tests[].review: the original requirement, plain-language acceptance statements, Met/Not met per check, and measured values — use that review when explaining the test; do not rephrase thresholds. Run uses a fresh private 1/120 s simulation and never changes live poses or geometry. Run waits for completion; start_run returns a queued run ID immediately. List reports the latest queued/running/cancelling/completed/cancelled/failed operation. After start_run, poll list for that same run ID to become terminal before interpreting results. Existing outcomes belong to the previous run while an operation is active. Completed means execution finished; outcome.passed determines assertion success. cancel_run takes that exact workspace-scoped run_id, requires its actor or the local user, and needs no catalog/revision freshness. Stop preserves the previous result and stays cancelling until entered CPU work ends. Actor revocation cancels owned runs. There is one active run per workspace, two per actor and eight per application; queued and cancelling runs count. A 60-second wall deadline signals cancellation at safe tick boundaries; it is not a hard thread kill. Push requires direct signed normal contact, final receiver displacement and additional displacement versus an identical run without the selected driver motor. Contact threshold is peak sampled last-solver-substep impulse in N s, not force or total impulse. Approximate verified collision proxies/inertia only. Reports pass/fail measurements with thresholds; results become stale when the model's inputs or the engine change; design_compared is false when no workspace of that model was available to compare. Replay selects an accepted frame into canonical playback for visual and exact pose inspection; use control_playback leave to restore live pose. Up to 16 tests, 10 seconds/run, 32768 body-frames and 8 MiB catalog. Definitions travel with Save/Open; imported runs must be rerun. CAD simulation only.",
        ),
        spec::<crate::motion::CheckInterference>(
            "check_interference",
            "Check 1..64 named instance pairs at an explicit revision and settled pose version. Exact BRep distances/intersection volumes are enabled by default. Optional conservative proxy results identify their clearance expansion and explicitly ignored pairs. Motion during the query invalidates the result; proxy contact is not proof of CAD interference.",
        ),
        spec::<Export>(
            "export",
            "Export the committed model to STEP or STL at the authoritative motion pose; return an opaque artifact handle. Moving mechanisms require the current pose_version from get_motion. STL uses binary float32 millimetre coordinates and private fresh tessellation with absolute linear deflection setting 0.025 mm and angular setting 0.1 rad. Nonzero facets that collapse/reverse under float32 or triangulation vertices rounded by more than 0.025 mm reject the export without publication; use STEP or explicitly reposition the design. Degenerate kernel seams are removed and normals match exported vertices. These are meshing settings, not a certified universal surface-error bound; exact CAD geometry is unchanged. STEP retains exact CAD surfaces.",
        ),
        spec::<Undo>(
            "undo",
            "Build a previous successful input state as a new revision, preserving history and current geometry on failure.",
        ),
        spec::<crate::mesh_ops::TransformMesh>(
            "transform_mesh",
            "Apply a rigid transform (translate mm, rotate axis+angle_deg, per-axis scale, mirror_axis) to a managed STL mesh asset, then rebuild as a new revision. Use for simple dimensional tweaks on STL assets — inspect get_project's assets map for the asset key. Only .stl assets qualify; CadQuery/BRep parts must be edited through source or set_parameter. Returns a build job; poll get_job.",
        ),
        spec::<CheckPrintability>(
            "check_printability",
            "Run heuristic printability checks on the current revision: watertight/manifold edges, degenerate triangles, build-volume fit, minimum feature size and overhang fraction for a process preset (fdm default, resin, sla, generic). Works on STL mesh projects and CadQuery projects (tessellated through export). Results are estimates for triage — not a slicer. Requires the current revision; pass pose_version for a posed mechanism.",
        ),
    ];
    for tool in &mut tools {
        tool.output_schema = match tool.name.as_str() {
            "set_annotation" | "delete_annotation" | "build" | "repair_display" | "get_job"
            | "cancel_job" | "set_parameter" | "define_mechanism" | "undo" | "transform_mesh" => {
                Some(result_schema::<crate::Job>())
            }
            "get_motion" | "control_simulation" => Some(result_schema::<crate::motion::Snapshot>()),
            "set_joint_coordinate" => Some(result_schema::<crate::motion::CoordinateResult>()),
            "control_playback" => Some(result_schema::<crate::playback::Reply>()),
            "control_motion_tests" => Some(result_schema::<crate::motion_tests::Reply>()),
            "read_source" => Some(result_schema::<SourceReply>()),
            "find_symbol" => Some(result_schema::<SymbolReply>()),
            "search_source" => Some(result_schema::<SourceMatches>()),
            "update_source" => Some(result_schema::<SourceUpdate>()),
            "set_dimensions" => Some(result_schema::<crate::views::DimensionsReply>()),
            "get_selection" => Some(result_schema::<crate::geometry::SelectionReply>()),
            "trace_mesh_edges" => Some(result_schema::<crate::stl_model::EdgeTraceReply>()),
            "trace_cad_edge" => Some(result_schema::<CadEdgeTraceReply>()),
            "match_mesh_edge" => Some(result_schema::<MeshEdgeMatchResult>()),
            "trace_part_source" => Some(result_schema::<PartSourceTrace>()),
            "inspect" => Some(result_schema::<crate::geometry::InspectionReply>()),
            "check_printability" => {
                Some(result_schema::<crate::printability::PrintabilityReport>())
            }
            "check_interference" => Some(result_schema::<crate::geometry::InterferenceReply>()),
            "capture_views" => Some(result_schema::<crate::views::CaptureToolReply>()),
            "compare_views" => Some(result_schema::<crate::views::ViewsReply>()),
            "export" => Some(result_schema::<crate::artifacts::ExportReply>()),
            "get_project" => Some(result_schema::<crate::project_result::Reply>()),
            _ => None,
        };
        tool.mcp_output_schema = match tool.name.as_str() {
            "capture_views" => Some(result_schema::<crate::views::CaptureToolMetadata>()),
            "compare_views" => {
                Some(result_schema::<crate::views::ViewsMetadata>())
            }
            "export" => Some(result_schema::<crate::artifacts::McpExportReply>()),
            _ => None,
        };
    }
    tools
}
pub fn definitions_ref() -> &'static [ToolSpec] {
    static DEFINITIONS: std::sync::OnceLock<Vec<ToolSpec>> = std::sync::OnceLock::new();
    DEFINITIONS.get_or_init(build_definitions)
}
pub fn definitions() -> Vec<ToolSpec> {
    definitions_ref().to_vec()
}
/// Input-only projection for providers that do not accept result schemas. Do
/// not clone/count unused output metadata against their model context budget.
pub fn input_definitions() -> Vec<ToolSpec> {
    definitions_ref()
        .iter()
        .map(|tool| ToolSpec {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
            output_schema: None,
            mcp_output_schema: None,
        })
        .collect()
}
pub fn definition(name: &str) -> Option<&'static ToolSpec> {
    definitions_ref().iter().find(|tool| tool.name == name)
}
/// Legacy names remain callable for clients with a cached tool catalog.
pub fn canonical_name(name: &str) -> &str {
    match name {
        "read_source_lines" => "read_source",
        "capture_view" => "capture_views",
        _ => name,
    }
}
fn parse<T: for<'de> Deserialize<'de>>(arguments: Value) -> Result<T> {
    serde_json::from_value(arguments)
        .map_err(|error| Error::domain("INVALID_REQUEST", error.to_string()))
}
fn source_limit(value: Option<usize>, default: usize, max: usize, field: &str) -> Result<usize> {
    let value = value.unwrap_or(default);
    if !(1..=max).contains(&value) {
        return Err(Error::domain(
            "INVALID_REQUEST",
            format!("{field} must be between 1 and {max}"),
        ));
    }
    Ok(value)
}

fn source_page(
    file: SourceContent,
    start: usize,
    limit: usize,
    offset: usize,
) -> Result<SourceLines> {
    const MAX_BYTES: usize = 8192;
    let mut lines = file
        .source
        .split_inclusive('\n')
        .enumerate()
        .skip(start - 1)
        .peekable();
    if offset != 0
        && !lines
            .peek()
            .is_some_and(|(_, line)| offset < line.len() && line.is_char_boundary(offset))
    {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "byte_offset must point to a UTF-8 character on the first line",
        ));
    }
    let mut page = SourceLines {
        path: file.path,
        hash: file.hash,
        source: String::new(),
        start_line: start,
        byte_offset: offset,
        next_line: None,
        next_byte_offset: 0,
    };
    for (index, line) in lines.by_ref().take(limit) {
        let skip = if index + 1 == start { offset } else { 0 };
        let remaining = &line[skip..];
        let count = remaining.floor_char_boundary(MAX_BYTES - page.source.len());
        page.source.push_str(&remaining[..count]);
        if count < remaining.len() {
            page.next_line = Some(index + 1);
            page.next_byte_offset = skip + count;
            return Ok(page);
        }
    }
    page.next_line = lines.next().map(|(index, _)| index + 1);
    Ok(page)
}

impl Core {
    pub async fn dispatch(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: Value,
    ) -> Result<Value> {
        if name.len() > 80 || context.actor.len() > 256 || name.chars().any(char::is_control) {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Invalid tool or actor identifier",
            ));
        }
        let workspace = arguments["workspace_id"].as_str().unwrap_or("").to_string();
        let activity = crate::events::ToolActivity::start(
            self.activity.clone(),
            workspace,
            &context.actor,
            name,
        );
        let result = self.dispatch_inner(context, name, arguments).await;
        activity.finish(result.is_ok());
        result
    }
    async fn dispatch_inner(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: Value,
    ) -> Result<Value> {
        if context.actor.is_empty() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Transport actor identity is required",
            ));
        }
        self.ensure_actor_active(&context.actor)?;
        match name {
            "get_project" => {
                let args: GetProject = parse(arguments)?;
                match args.workspace_id {
                    Some(workspace) => {
                        let mut project = self.get_project(&workspace).await?;
                        if let Some(parts) = project["model"]["parts"].as_array_mut() {
                            for part in parts {
                                if let Some(part) = part.as_object_mut() {
                                    part.remove("mesh");
                                }
                            }
                        }
                        if let Some(activity) = project["activity"].as_array_mut() {
                            if activity.len() > 20 {
                                activity.drain(..activity.len() - 20);
                            }
                            for job in activity {
                                if let Some(diagnostics) = job.get_mut("diagnostics") {
                                    *diagnostics = json!({"code":diagnostics["code"],"message":diagnostics["message"]});
                                }
                            }
                        }
                        let project: crate::project_result::Project =
                            serde_json::from_value(project)?;
                        Ok(json!(project))
                    }
                    None => Ok(json!(crate::project_result::WorkspaceListing {
                        workspace_ids: self.list_workspaces().await,
                        protocol_version: PROTOCOL_VERSION,
                        tools: definitions_ref().iter().map(|s| s.name.clone()).collect(),
                    })),
                }
            }
            "get_selection" => {
                let args: GetSelection = parse(arguments)?;
                self.get_selection(&args.workspace_id).await
            }
            "capture_view" => {
                let args: crate::views::Capture = parse(arguments)?;
                self.capture_view(&context.actor, args).await
            }
            "capture_views" => self.capture_views(&context.actor, parse(arguments)?).await,
            "compare_views" => self.compare_views(&context.actor, parse(arguments)?).await,
            "set_dimensions" => {
                let args: crate::views::SetDimensions = parse(arguments)?;
                self.set_dimensions(&context.actor, args).await
            }
            "find_symbol" => {
                let args: FindSymbol = parse(arguments)?;
                let limit = source_limit(args.max_matches, 20, 50, "max_matches")?;
                if args.symbol.is_empty() || args.symbol.len() > 256 {
                    return Err(Error::domain(
                        "INVALID_REQUEST",
                        "symbol must be 1–256 UTF-8 bytes",
                    ));
                }
                let files = self
                    .source_files_at(&args.workspace_id, &args.version)
                    .await?;
                if let Some(path) = &args.path {
                    crate::validate_path(path)?;
                    if !files.contains_key(path) {
                        return Err(Error::domain("INVALID_REQUEST", "Unknown source file"));
                    }
                }
                let skip = if let Some(cursor) = &args.cursor {
                    if !files.contains_key(&cursor.path)
                        || args.path.as_ref().is_some_and(|path| path != &cursor.path)
                    {
                        return Err(Error::domain(
                            "INVALID_REQUEST",
                            "Unknown symbol cursor path",
                        ));
                    }
                    cursor.skip
                } else {
                    0
                };
                let mut sources = BTreeMap::new();
                let mut bytes = 0;
                let mut next_batch = None;
                for (path, source) in &files {
                    if args.path.as_ref().is_some_and(|filter| filter != path)
                        || args
                            .cursor
                            .as_ref()
                            .is_some_and(|cursor| path < &cursor.path)
                    {
                        continue;
                    }
                    // Keep the request below 64 MiB even with worst-case JSON escaping.
                    if !sources.is_empty() && bytes + source.len() + path.len() > 8 * 1024 * 1024 {
                        next_batch = Some(SymbolCursor {
                            path: path.clone(),
                            skip: 0,
                        });
                        break;
                    }
                    bytes += source.len() + path.len();
                    sources.insert(path, source);
                }
                let data = self.entry_analysis_with_cancel(
                    "find_symbol",
                    json!({"sources": sources, "symbol": args.symbol, "kind": args.kind, "limit": limit, "skip": skip}),
                    self.actor_signal(&context.actor),
                ).await?;
                let mut result: SymbolReply = serde_json::from_value(data)?;
                result.next_cursor = result.next_cursor.or(next_batch);
                Ok(json!(result))
            }
            "search_source" => {
                let args: SearchSource = parse(arguments)?;
                let start = source_limit(args.start_line, 1, usize::MAX, "start_line")?;
                let limit = source_limit(args.max_matches, 20, 50, "max_matches")?;
                if args.query.is_empty()
                    || args.query.len() > 256
                    || args.query.contains(['\r', '\n'])
                {
                    return Err(Error::domain(
                        "INVALID_REQUEST",
                        "query must be 1–256 UTF-8 bytes on one line",
                    ));
                }
                let file = self
                    .source_content(&args.workspace_id, &args.version, &args.path)
                    .await?;
                let mut hits =
                    file.source
                        .lines()
                        .enumerate()
                        .skip(start - 1)
                        .filter_map(|(index, line)| {
                            line.find(&args.query)
                                .map(|offset| (index + 1, offset, line))
                        });
                let matches = hits
                    .by_ref()
                    .take(limit)
                    .map(|(line, byte_offset, text)| {
                        let start = text.ceil_char_boundary(byte_offset.saturating_sub(96));
                        let end = text.floor_char_boundary((start + 384).min(text.len()));
                        SourceMatch {
                            line,
                            byte_offset,
                            preview: text[start..end].into(),
                        }
                    })
                    .collect();
                Ok(json!(SourceMatches {
                    path: file.path,
                    hash: file.hash,
                    matches,
                    next_line: hits.next().map(|(line, _, _)| line),
                }))
            }
            "read_source" | "read_source_lines" => {
                let args: ReadSource = parse(arguments)?;
                if name == "read_source"
                    && !args.paged.unwrap_or(false)
                    && args.start_line.is_none()
                    && args.max_lines.is_none()
                    && args.byte_offset.is_none()
                {
                    return Ok(json!(SourceReply::Full(
                        self.source_content(&args.workspace_id, &args.version, &args.path)
                            .await?
                    )));
                }
                let start = source_limit(args.start_line, 1, usize::MAX, "start_line")?;
                let limit = source_limit(args.max_lines, 80, 200, "max_lines")?;
                let file = self
                    .source_content(&args.workspace_id, &args.version, &args.path)
                    .await?;
                Ok(json!(SourceReply::Page(source_page(
                    file,
                    start,
                    limit,
                    args.byte_offset.unwrap_or(0)
                )?)))
            }
            "set_annotation" => {
                let args: SetAnnotation = parse(arguments)?;
                let id = args.annotation.id.clone();
                Ok(json!(
                    self.edit_annotation(
                        &args.workspace_id,
                        &args.expected_base_revision,
                        &id,
                        Some(args.annotation),
                        &context.actor
                    )
                    .await?
                ))
            }
            "delete_annotation" => {
                let args: DeleteAnnotation = parse(arguments)?;
                Ok(json!(
                    self.edit_annotation(
                        &args.workspace_id,
                        &args.expected_base_revision,
                        &args.annotation_id,
                        None,
                        &context.actor
                    )
                    .await?
                ))
            }
            "update_source" => {
                let args: UpdateSource = parse(arguments)?;
                let draft = self
                    .update_source(
                        &args.workspace_id,
                        &args.expected_base_revision,
                        args.draft_id.as_deref(),
                        args.changes,
                        &context.actor,
                    )
                    .await?;
                let changed_files = draft
                    .inputs
                    .files
                    .iter()
                    .filter(|(path, source)| {
                        draft.source_hashes.get(*path) != Some(&crate::hash(source))
                    })
                    .map(|(path, source)| ChangedSource {
                        path: path.clone(),
                        bytes: source.len(),
                        hash: crate::hash(source),
                    })
                    .collect();
                Ok(json!(SourceUpdate {
                    id: draft.id,
                    base_revision: draft.base_revision,
                    changed_files,
                    source_hashes: draft.source_hashes,
                    actor: draft.actor,
                }))
            }
            "build" => {
                let args: Build = parse(arguments)?;
                Ok(json!(
                    self.build(
                        &args.workspace_id,
                        &args.expected_base_revision,
                        &args.draft_id,
                        &context.actor
                    )
                    .await?
                ))
            }
            "transform_mesh" => {
                let args: crate::mesh_ops::TransformMesh = parse(arguments)?;
                Ok(json!(self.transform_mesh(&context.actor, args).await?))
            }
            "check_printability" => {
                let args: CheckPrintability = parse(arguments)?;
                Ok(json!(
                    self.check_printability(
                        &context.actor,
                        &args.workspace_id,
                        &args.revision,
                        args.pose_version,
                        args.process
                    )
                    .await?
                ))
            }
            "repair_display" => {
                let args: crate::recovery::RepairDisplay = parse(arguments)?;
                Ok(json!(self.repair_display(&context.actor, args).await?))
            }
            "get_job" => {
                let args: Job = parse(arguments)?;
                Ok(json!(self.get_job(&args.workspace_id, &args.job_id).await?))
            }
            "cancel_job" => {
                let args: Job = parse(arguments)?;
                Ok(json!(
                    self.cancel_job(&args.workspace_id, &args.job_id).await?
                ))
            }
            "inspect" => {
                let args: Inspect = parse(arguments)?;
                let mut payload = serde_json::to_value(&args)?;
                payload.as_object_mut().unwrap().retain(|_, v| !v.is_null());
                let result = self
                    .query_as_actor(
                        &context.actor,
                        &args.workspace_id,
                        &args.revision,
                        "inspect",
                        payload,
                    )
                    .await?;
                let reply: crate::geometry::InspectionReply = serde_json::from_value(result)?;
                Ok(json!(reply))
            }
            "trace_mesh_edges" => {
                let args: TraceMeshEdges = parse(arguments)?;
                self.query_as_actor(
                    &context.actor,
                    &args.workspace_id,
                    &args.revision,
                    "trace_mesh_edges",
                    json!({"entities": args.entities}),
                )
                .await
            }
            "trace_cad_edge" => {
                let args: TraceCadEdge = parse(arguments)?;
                let reply: CadEdgeTraceReply = serde_json::from_value(
                    self.query_as_actor(
                        &context.actor,
                        &args.workspace_id,
                        &args.revision,
                        "trace_cad_edge",
                        json!({"entities": args.entities, "pose_version": args.pose_version}),
                    )
                    .await?,
                )?;
                Ok(json!(reply))
            }
            "match_mesh_edge" => {
                let args: MatchMeshEdge = parse(arguments)?;
                let tolerance = args.tolerance_mm.unwrap_or(0.15);
                if !(1e-6..=5.).contains(&tolerance) {
                    return Err(Error::domain(
                        "INVALID_REQUEST",
                        "tolerance_mm must be between 0.000001 and 5 mm",
                    ));
                }
                let segments = self
                    .query_as_actor(
                        &context.actor,
                        &args.workspace_id,
                        &args.revision,
                        "mesh_edge_segments",
                        json!({"entities": args.entities}),
                    )
                    .await?;
                let reply: MeshEdgeMatchResult = serde_json::from_value(
                    self.query_as_actor(
                        &context.actor,
                        &args.target_workspace_id,
                        &args.target_revision,
                        "match_mesh_edge",
                        json!({"segments": segments, "instance_id": args.target_instance_id,
                           "pose_version": args.target_pose_version, "tolerance_mm": tolerance}),
                    )
                    .await?,
                )?;
                Ok(json!(reply))
            }
            "trace_part_source" => {
                let args: TracePartSource = parse(arguments)?;
                let _memory = self.pin_workspace(&args.workspace_id).await?;
                let workspaces = self.workspace(&args.workspace_id).await?;
                let current = workspaces[&args.workspace_id].stored.current_revision();
                if current.id != args.revision {
                    return Err(Error::domain(
                        "STALE_ENTITY",
                        "Source trace requires the current revision",
                    ));
                }
                let part = current.model["parts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|part| part["instance_id"] == args.instance_id)
                    .ok_or_else(|| Error::domain("STALE_ENTITY", "Unknown part instance"))?;
                let source: Option<crate::geometry::SourceLocation> =
                    serde_json::from_value(part["source"].clone())?;
                let linked_entry = (args.instance_id == "model")
                    .then(|| crate::project_entry::model_entry_asset(&current.inputs))
                    .flatten()
                    .map(|(entry, _, _)| entry);
                let (publication, asset_path) = match (linked_entry, source) {
                    (Some(entry), _) => (None, Some(entry)),
                    (None, Some(location)) if current.inputs.files.contains_key(&location.file) => {
                        (Some(location), None)
                    }
                    (None, Some(location))
                        if crate::portable::model_file_format(&location.file).is_some() =>
                    {
                        (None, Some(location.file))
                    }
                    _ => (None, None),
                };
                Ok(json!(PartSourceTrace {
                    revision: args.revision,
                    instance_id: args.instance_id,
                    publication,
                    asset_path
                }))
            }
            "set_parameter" => {
                let args: SetParameter = parse(arguments)?;
                Ok(json!(
                    self.set_parameter(
                        &args.workspace_id,
                        &args.expected_base_revision,
                        &args.parameter_id,
                        args.value,
                        &args.unit,
                        &context.actor
                    )
                    .await?
                ))
            }
            "define_mechanism" => {
                let args: crate::motion::DefineMechanism = parse(arguments)?;
                Ok(json!(self.define_mechanism(&context.actor, args).await?))
            }
            "get_motion" => {
                let args: crate::motion::GetMotion = parse(arguments)?;
                Ok(json!(
                    self.get_motion(&args.workspace_id, &args.revision).await?
                ))
            }
            "set_joint_coordinate" => {
                let args: crate::motion::SetCoordinate = parse(arguments)?;
                Ok(json!(self.set_coordinate(&context.actor, args).await?))
            }
            "control_playback" => {
                let args: crate::playback::Control = parse(arguments)?;
                Ok(json!(self.control_playback(&context.actor, args).await?))
            }
            "control_simulation" => {
                let args: crate::dynamics::Control = parse(arguments)?;
                Ok(json!(self.control_simulation(&context.actor, args).await?))
            }
            "control_motion_tests" => {
                let args: crate::motion_tests::Control = parse(arguments)?;
                Ok(json!(
                    self.control_motion_tests(&context.actor, args).await?
                ))
            }
            "check_interference" => {
                let args: crate::motion::CheckInterference = parse(arguments)?;
                self.check_interference(&context.actor, args).await
            }
            "export" => {
                let args: Export = parse(arguments)?;
                let mut payload = serde_json::to_value(&args)?;
                payload.as_object_mut().unwrap().retain(|_, v| !v.is_null());
                let result = self
                    .query_as_actor(
                        &context.actor,
                        &args.workspace_id,
                        &args.revision,
                        "export",
                        payload,
                    )
                    .await?;
                let reply: crate::artifacts::ExportReply = serde_json::from_value(result)?;
                Ok(json!(reply))
            }
            "undo" => {
                let args: Undo = parse(arguments)?;
                Ok(json!(
                    self.undo(
                        &args.workspace_id,
                        &args.expected_current_revision,
                        &context.actor
                    )
                    .await?
                ))
            }
            _ => Err(Error::domain(
                "UNSUPPORTED_OPERATION",
                format!("Unknown tool: {name}"),
            )),
        }
    }
}
