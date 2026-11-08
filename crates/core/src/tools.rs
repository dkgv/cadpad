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
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadSource {
    pub workspace_id: String,
    pub version: String,
    pub path: String,
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
        spec::<crate::views::Capture>(
            "capture_view",
            "Capture a bounded PNG from the connected viewport with explicit workspace, revision, pose, camera and dimension options. Includes numeric context; images are not authoritative measurements.",
        ),
        spec::<crate::views::SetDimensions>(
            "set_dimensions",
            "Set transient dimension visibility and selected/pinned/all-visible scope in the connected viewport. This does not create or edit saved annotation definitions.",
        ),
        spec::<ReadSource>(
            "read_source",
            "Read a relative Python source path from a revision, editable draft, or immutable job snapshot_id, including its content hash.",
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
            "Query exact BRep properties or the distance between two revision-scoped entities. Coordinates and lengths are millimetres; quaternions use xyzw.",
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
            "Create/list/edit/delete/run/replay saved CAD simulation tests from natural-language intent. List first for catalog CAS. Ground body/joint IDs in get_project/get_motion; inspect the full mechanism before defining actions. Upsert saves a named intent, explicit initial coordinates for every moving joint (m/rad), body masses/friction/gravity, timed motor/load actions and measurable checks. Ask the user to resolve missing targets/tolerances rather than inventing a requirement. Every list/run/upsert reply includes tests[].review: the original requirement, plain-language acceptance statements, Met/Not met per check, and measured values — use that review when explaining the test; do not rephrase thresholds. Run uses a fresh private 1/120 s simulation and never changes live poses or geometry. Run waits for completion; start_run returns a queued run ID immediately. List reports the latest queued/running/cancelling/completed/cancelled/failed operation. After start_run, poll list for that same run ID to become terminal before interpreting results. Existing outcomes belong to the previous run while an operation is active. Completed means execution finished; outcome.passed determines assertion success. cancel_run takes that exact workspace-scoped run_id, requires its actor or the local user, and needs no catalog/revision freshness. Stop preserves the previous result and stays cancelling until entered CPU work ends. Actor revocation cancels owned runs. There is one active run per workspace, two per actor and eight per application; queued and cancelling runs count. A 60-second wall deadline signals cancellation at safe tick boundaries; it is not a hard thread kill. Push requires direct signed normal contact, final receiver displacement and additional displacement versus an identical run without the selected driver motor. Contact threshold is peak sampled last-solver-substep impulse in N s, not force or total impulse. Approximate verified collision proxies/inertia only. Reports pass/fail measurements with thresholds; results become stale when revision/engine changes. Replay selects an accepted frame into canonical playback for visual and exact pose inspection; use control_playback leave to restore live pose. Up to 16 tests, 10 seconds/run, 32768 body-frames and 8 MiB catalog. Definitions travel with Save/Open; imported runs must be rerun. CAD simulation only.",
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
    ];
    for tool in &mut tools {
        tool.output_schema = match tool.name.as_str() {
            "set_annotation" | "delete_annotation" | "build" | "repair_display" | "get_job"
            | "cancel_job" | "set_parameter" | "define_mechanism" | "undo" => {
                Some(result_schema::<crate::Job>())
            }
            "get_motion" | "control_simulation" => Some(result_schema::<crate::motion::Snapshot>()),
            "set_joint_coordinate" => Some(result_schema::<crate::motion::CoordinateResult>()),
            "control_playback" => Some(result_schema::<crate::playback::Reply>()),
            "control_motion_tests" => Some(result_schema::<crate::motion_tests::Reply>()),
            "read_source" => Some(result_schema::<SourceContent>()),
            "update_source" => Some(result_schema::<SourceUpdate>()),
            "set_dimensions" => Some(result_schema::<crate::views::DimensionsReply>()),
            "get_selection" => Some(result_schema::<crate::geometry::SelectionReply>()),
            "inspect" => Some(result_schema::<crate::geometry::InspectionReply>()),
            "check_interference" => Some(result_schema::<crate::geometry::InterferenceReply>()),
            "capture_view" => Some(result_schema::<crate::views::CaptureReply>()),
            "export" => Some(result_schema::<crate::artifacts::ExportReply>()),
            "get_project" => Some(result_schema::<crate::project_result::Reply>()),
            _ => None,
        };
        tool.mcp_output_schema = match tool.name.as_str() {
            "capture_view" => Some(result_schema::<crate::views::CaptureMetadata>()),
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
fn parse<T: for<'de> Deserialize<'de>>(arguments: Value) -> Result<T> {
    serde_json::from_value(arguments)
        .map_err(|error| Error::domain("INVALID_REQUEST", error.to_string()))
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
            "set_dimensions" => {
                let args: crate::views::SetDimensions = parse(arguments)?;
                self.set_dimensions(&context.actor, args).await
            }
            "read_source" => {
                let args: ReadSource = parse(arguments)?;
                self.read_source(&args.workspace_id, &args.version, &args.path)
                    .await
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
