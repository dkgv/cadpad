//! Saved, bounded simulation experiments. Runs are isolated from live motion;
//! a pass describes the declared proxy model, never physical certification.
use crate::{Core, Error, Result, dynamics, kinematics, motion, playback, portable};
use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
type Catalog = BTreeMap<String, Saved>;
fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_MOTION_TEST", message)
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// 1/120 s ticks. Motor targets persist between actions; loads replace.
    pub ticks: u32,
    pub motors: Vec<dynamics::Motor>,
    pub loads: BTreeMap<String, dynamics::Load>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Check {
    /// Final projected displacement relative to the saved initial pose.
    Displacement {
        body: String,
        direction: [f64; 3],
        minimum_mm: f64,
    },
    /// Final joint coordinate, with an explicit absolute tolerance in m/rad.
    Coordinate {
        joint_id: String,
        target: f64,
        unit: String,
        tolerance: f64,
    },
    /// Direct normal contact plus final receiver displacement, and additional
    /// displacement versus an otherwise identical run without driver_joint.
    Push {
        pusher: String,
        receiver: String,
        driver_joint: String,
        direction: [f64; 3],
        minimum_mm: f64,
        minimum_additional_mm: f64,
        /// Peak sampled last-substep normal impulse, N s; not average force.
        minimum_sampled_impulse_ns: f64,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub name: String,
    pub intent: String,
    /// Every moving joint must have an explicit starting coordinate, m/rad.
    pub initial_coordinates: BTreeMap<String, f64>,
    pub simulation: dynamics::Definition,
    pub actions: Vec<Action>,
    pub checks: Vec<Check>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Measurement {
    pub label: String,
    pub actual: f64,
    pub expected: f64,
    pub unit: String,
    pub passed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Outcome {
    pub passed: bool,
    pub measurements: Vec<Measurement>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub revision: String,
    pub engine: String,
    pub input_fingerprint: String,
    pub definition_fingerprint: String,
    pub outcome: Outcome,
    pub samples: Vec<playback::Sample>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    pub definition: Definition,
    pub last_run: Option<Run>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CheckReview {
    /// Plain-language acceptance criterion, matching the Motion tests panel.
    pub statement: String,
    /// Set when this check has measured results from the latest run.
    pub met: Option<bool>,
    pub measurements: Vec<Measurement>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TestReview {
    /// Original free-text requirement / intent.
    pub requirement: String,
    pub checks: Vec<CheckReview>,
    /// Short status for conversation: "Not run" or "N of M requirements met".
    pub summary: String,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TestInfo {
    pub id: String,
    pub definition: Definition,
    pub outcome: Option<Outcome>,
    /// Deterministic plain-language requirements and results for agents/MCP/UI.
    pub review: TestReview,
    pub stale: bool,
    pub frame_count: usize,
    pub duration_s: f64,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    List,
    Upsert {
        id: String,
        definition: Definition,
    },
    Delete {
        id: String,
    },
    Run {
        id: String,
    },
    StartRun {
        id: String,
    },
    CancelRun {
        run_id: String,
    },
    Replay {
        id: String,
        frame_index: usize,
        expected_pose_version: u64,
    },
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub workspace_id: String,
    pub revision: String,
    /// Returned by list. Required for edits/runs to prevent lost updates.
    pub expected_catalog: Option<String>,
    pub command: Command,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Reply {
    pub catalog: String,
    pub tests: Vec<TestInfo>,
    pub accuracy: String,
    pub snapshot: Option<motion::Snapshot>,
    pub run: Option<crate::motion_test_runs::Status>,
}
pub fn engine() -> String {
    static ENGINE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ENGINE
        .get_or_init(|| {
            portable::bytes_hash(
                format!(
                    "{}{}{}{}{}{}{}{}{}{}",
                    include_str!("dynamics.rs"),
                    include_str!("motion_tests.rs"),
                    include_str!("../../../Cargo.lock"),
                    include_str!("kinematics.rs"),
                    include_str!("collision.rs"),
                    include_str!("constraints.rs"),
                    include_str!("paths.rs"),
                    std::env::consts::ARCH,
                    std::env::consts::OS,
                    cfg!(debug_assertions)
                )
                .as_bytes(),
            )
        })
        .clone()
}
fn fingerprint<T: Serialize>(value: &T) -> Result<String> {
    Ok(portable::bytes_hash(&serde_json::to_vec(value)?))
}
fn direction(value: [f64; 3]) -> Result<DVec3> {
    let vector = DVec3::from_array(value);
    if !vector.is_finite() || (vector.length_squared() - 1.).abs() > 1e-6 {
        return Err(invalid(
            "Check directions must be finite unit world vectors",
        ));
    }
    Ok(vector)
}
fn positive(value: f64) -> Result<()> {
    if !value.is_finite() || value <= 0. || value > 1e9 {
        return Err(invalid(
            "Thresholds must be finite, positive and at most 1e9",
        ));
    }
    Ok(())
}
impl Definition {
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty()
            || self.name.len() > 120
            || self.name.chars().any(char::is_control)
            || self.intent.len() > 4096
            || self.initial_coordinates.len() > 128
            || self.initial_coordinates.values().any(|q| !q.is_finite())
            || self.actions.is_empty()
            || self.actions.len() > 16
            || self.checks.is_empty()
            || self.checks.len() > 16
            || self.simulation.bodies.is_empty()
            || self.simulation.bodies.len() > 128
        {
            return Err(invalid(
                "Test needs a name, bounded intent, bodies, 1..16 actions and 1..16 checks",
            ));
        }
        let mut ticks = 0_u64;
        for action in &self.actions {
            if action.ticks == 0 || action.motors.len() > 128 || action.loads.len() > 128 {
                return Err(invalid(
                    "Actions need positive ticks and at most 128 motors/loads",
                ));
            }
            ticks += u64::from(action.ticks);
        }
        if ticks > 1200 || (ticks + 1) * self.simulation.bodies.len() as u64 > 32768 {
            return Err(invalid("Test exceeds 10 seconds or 32768 body-frames"));
        }
        for check in &self.checks {
            match check {
                Check::Displacement {
                    direction: axis,
                    minimum_mm,
                    ..
                } => {
                    direction(*axis)?;
                    positive(*minimum_mm)?;
                }
                Check::Coordinate {
                    target,
                    tolerance,
                    unit,
                    ..
                } => {
                    if !target.is_finite() || !matches!(unit.as_str(), "m" | "rad") {
                        return Err(invalid(
                            "Coordinate check needs finite target and m/rad units",
                        ));
                    }
                    positive(*tolerance)?;
                }
                Check::Push {
                    pusher,
                    receiver,
                    driver_joint,
                    direction: axis,
                    minimum_mm,
                    minimum_additional_mm,
                    minimum_sampled_impulse_ns,
                } => {
                    direction(*axis)?;
                    positive(*minimum_mm)?;
                    positive(*minimum_additional_mm)?;
                    positive(*minimum_sampled_impulse_ns)?;
                    if pusher == receiver
                        || !self
                            .actions
                            .iter()
                            .any(|a| a.motors.iter().any(|m| m.joint_id == *driver_joint))
                    {
                        return Err(invalid(
                            "Push needs distinct bodies and a motor action for its driver",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}
pub fn execute(
    definition: &Definition,
    revision: &crate::Revision,
    cancel: &tokio::sync::watch::Receiver<bool>,
) -> Result<Run> {
    definition.validate()?;
    let mut mechanism_definition = kinematics::decode(&revision.inputs.mechanism)?;
    let moving = mechanism_definition
        .joints
        .iter()
        .filter(|j| !matches!(j.kind, kinematics::JointKind::Fixed))
        .map(|j| j.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if definition
        .initial_coordinates
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        != moving
    {
        return Err(invalid(
            "Specify every moving joint's initial coordinate exactly once",
        ));
    }
    for joint in &mut mechanism_definition.joints {
        if let Some(value) = definition.initial_coordinates.get(&joint.id) {
            joint.initial = *value;
        }
    }
    let mechanism = kinematics::Mechanism::new(mechanism_definition, &revision.model["parts"])?;
    let initial = mechanism.evaluate()?;
    let mut simulation =
        dynamics::Simulation::new(definition.simulation.clone(), &mechanism, &revision.model)?;
    // Validate all actions and references before spending the tick budget.
    for action in &definition.actions {
        let mut candidate = simulation.clone();
        candidate.set_loads(action.loads.clone())?;
        for motor in &action.motors {
            if candidate.motor(motor)? != motor.target {
                return Err(invalid(
                    "Test motor targets must lie inside declared joint limits",
                ));
            }
        }
    }
    for check in &definition.checks {
        match check {
            Check::Displacement { body, .. } if !initial.poses.contains_key(body) => {
                return Err(invalid("Missing displacement body"));
            }
            Check::Coordinate { joint_id, unit, .. }
                if !initial
                    .joints
                    .iter()
                    .any(|j| j.id == *joint_id && j.unit == *unit) =>
            {
                return Err(invalid("Missing joint or incorrect coordinate units"));
            }
            Check::Push {
                pusher, receiver, ..
            } if !initial.poses.contains_key(pusher) || !initial.poses.contains_key(receiver) => {
                return Err(invalid("Missing push body"));
            }
            _ => (),
        }
    }
    let mut baselines = BTreeMap::new();
    for check in &definition.checks {
        if let Check::Push { driver_joint, .. } = check {
            baselines
                .entry(driver_joint.clone())
                .or_insert_with(|| simulation.clone());
        }
    }
    let mut peaks = vec![0_f64; definition.checks.len()];
    let mut samples = vec![playback::Sample::capture(0., 0, initial.clone())];
    for action in &definition.actions {
        simulation.set_loads(action.loads.clone())?;
        for motor in &action.motors {
            simulation.motor(motor)?;
        }
        for (driver, baseline) in &mut baselines {
            baseline.set_loads(action.loads.clone())?;
            for motor in &action.motors {
                if motor.joint_id != *driver {
                    baseline.motor(motor)?;
                }
            }
        }
        for _ in 0..action.ticks {
            let frame = simulation.step(1, Some(cancel))?.current;
            for baseline in baselines.values_mut() {
                baseline.step(1, Some(cancel))?;
            }
            for (index, check) in definition.checks.iter().enumerate() {
                if let Check::Push {
                    pusher,
                    receiver,
                    direction: axis,
                    ..
                } = check
                {
                    peaks[index] = peaks[index].max(
                        DVec3::from_array(simulation.contact_impulse(pusher, receiver)?)
                            .dot(direction(*axis)?),
                    );
                }
            }
            samples.push(playback::Sample::capture(
                frame.time_s,
                frame.tick,
                simulation.evaluation(&mechanism)?,
            ));
        }
    }
    let final_state = simulation.evaluation(&mechanism)?;
    let displacement = |body: &str, axis: [f64; 3], evaluation: &kinematics::Evaluation| {
        (DVec3::from_array(evaluation.poses[body].position)
            - DVec3::from_array(initial.poses[body].position))
        .dot(DVec3::from_array(axis))
    };
    let mut measurements = Vec::new();
    let mut minimum = |label: String, actual: f64, expected: f64, unit: &str| {
        measurements.push(Measurement {
            label,
            actual,
            expected,
            unit: unit.into(),
            passed: actual >= expected,
        });
    };
    for (index, check) in definition.checks.iter().enumerate() {
        match check {
            Check::Displacement {
                body,
                direction,
                minimum_mm,
            } => minimum(
                format!("{body}: final displacement"),
                displacement(body, *direction, &final_state),
                *minimum_mm,
                "mm",
            ),
            Check::Coordinate {
                joint_id,
                target,
                unit,
                tolerance,
            } => {
                let actual = final_state
                    .joints
                    .iter()
                    .find(|j| j.id == *joint_id)
                    .unwrap()
                    .coordinate;
                minimum(
                    format!("{joint_id}: remaining tolerance (target {target} {unit})"),
                    tolerance - (actual - target).abs(),
                    0.,
                    unit,
                );
            }
            Check::Push {
                pusher,
                receiver,
                driver_joint,
                direction,
                minimum_mm,
                minimum_additional_mm,
                minimum_sampled_impulse_ns,
            } => {
                let actual = displacement(receiver, *direction, &final_state);
                let baseline = displacement(
                    receiver,
                    *direction,
                    &baselines[driver_joint].evaluation(&mechanism)?,
                );
                minimum(
                    format!("{receiver}: final displacement"),
                    actual,
                    *minimum_mm,
                    "mm",
                );
                minimum(
                    format!(
                        "{receiver}: additional displacement versus disabled {driver_joint} (baseline {baseline:.6} mm)"
                    ),
                    actual - baseline,
                    *minimum_additional_mm,
                    "mm",
                );
                minimum(
                    format!(
                        "{pusher} → {receiver}: peak sampled normal impulse (last solver substep)"
                    ),
                    peaks[index],
                    *minimum_sampled_impulse_ns,
                    "N s",
                );
            }
        }
    }
    let run = Run {
        revision: revision.id.clone(),
        engine: engine(),
        input_fingerprint: portable::fingerprint(&revision.inputs)?,
        definition_fingerprint: fingerprint(definition)?,
        outcome: Outcome {
            passed: measurements.iter().all(|m| m.passed),
            measurements,
        },
        samples,
    };
    if serde_json::to_vec(&run)?.len() > MAX_BYTES / 2 {
        return Err(invalid("Test trace exceeds 4 MiB"));
    }
    Ok(run)
}

pub(crate) fn catalog_fingerprint(catalog: &Catalog) -> Result<Option<String>> {
    if catalog.is_empty() {
        Ok(None)
    } else {
        Ok(Some(fingerprint(catalog)?))
    }
}
pub(crate) fn encode(catalog: &Catalog) -> Result<Vec<u8>> {
    if catalog.len() > 16 {
        return Err(invalid("At most 16 saved motion tests"));
    }
    for (id, saved) in catalog {
        if id.is_empty() || id.len() > 120 || id.chars().any(char::is_control) {
            return Err(invalid("Invalid test ID"));
        }
        saved.definition.validate()?;
    }
    let bytes = serde_json::to_vec(catalog)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid(
            "Motion tests exceed 8 MiB; delete a test or shorten traces",
        ));
    }
    Ok(bytes)
}
pub(crate) fn load(root: &std::path::Path, digest: Option<&str>) -> Result<Catalog> {
    let Some(digest) = digest else {
        return Ok(BTreeMap::new());
    };
    if !portable::valid_hash(digest) {
        return Err(invalid("Invalid test archive hash"));
    }
    let bytes = portable::read_bounded(
        &portable::checked_path(root, &format!("motion-tests/{digest}.json"))?,
        MAX_BYTES,
    )?;
    if portable::bytes_hash(&bytes) != digest {
        return Err(invalid("Test archive digest mismatch"));
    }
    let mut catalog: Catalog = serde_json::from_slice(&bytes)?;
    // Imported outcomes remain historical. A fresh CAD build/run is necessary
    // before replay; do not trust imported poses or an asserted pass status.
    for saved in catalog.values_mut() {
        saved.last_run = None;
    }
    encode(&catalog)?;
    Ok(catalog)
}

impl Core {
    pub async fn control_motion_tests(&self, actor: &str, args: Control) -> Result<Reply> {
        use crate::motion_test_runs::{CancelOnDrop, Lease};
        self.ensure_actor_active(actor)?;
        if let Command::CancelRun { run_id } = &args.command {
            // Serialize Stop with the result's final commit, but allow it after
            // a design revision/catalog change: the operation ID is explicit.
            let workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get(&args.workspace_id)
                .ok_or_else(|| invalid("Unknown workspace"))?;
            self.motion_test_runs
                .stop(&args.workspace_id, run_id, actor)?;
            return reply(
                &ws.stored.motion_tests,
                &ws.stored.current,
                None,
                self.motion_test_runs.status(&args.workspace_id),
            );
        }
        let test_id = match &args.command {
            Command::Run { id } | Command::StartRun { id } => Some(id.clone()),
            _ => None,
        };
        let Some(test_id) = test_id else {
            return self.control_motion_tests_inner(actor, args, None).await;
        };
        let (lease, accepted) = {
            let workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get(&args.workspace_id)
                .ok_or_else(|| invalid("Unknown workspace"))?;
            if ws.stored.current != args.revision {
                return Err(Error::domain("STALE_ENTITY", "Design revision changed"));
            }
            if args.expected_catalog.as_deref() != Some(&fingerprint(&ws.stored.motion_tests)?) {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Motion tests changed; list before running",
                ));
            }
            let saved = ws
                .stored
                .motion_tests
                .get(&test_id)
                .ok_or_else(|| invalid("Unknown test"))?;
            saved.definition.validate()?;
            if ws.stored.revisions[&args.revision].model["rebuild_required"] == true {
                return Err(invalid("Build the design before running tests"));
            }
            let lease: Lease =
                self.motion_test_runs
                    .start(&args.workspace_id, &args.revision, &test_id, actor)?;
            let accepted = reply(
                &ws.stored.motion_tests,
                &args.revision,
                None,
                Some(lease.status()),
            )?;
            (lease, accepted)
        };
        let background = matches!(args.command, Command::StartRun { .. });
        let args = Control {
            command: Command::Run { id: test_id },
            ..args
        };
        let core = self.clone();
        let actor = actor.to_owned();
        let task_lease = lease.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            struct Watchers(Vec<tokio::task::JoinHandle<()>>);
            impl Drop for Watchers {
                fn drop(&mut self) {
                    for task in &self.0 {
                        task.abort();
                    }
                }
            }
            let mut actor_signal = core.actor_signal(&actor);
            let actor_lease = task_lease.clone();
            let actor_watch = tokio::spawn(async move {
                if *actor_signal.borrow() || actor_signal.changed().await.is_ok() {
                    actor_lease.cancel();
                }
            });
            let deadline_lease = task_lease.clone();
            let deadline_watch = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(
                    crate::motion_test_runs::DEADLINE_S,
                ))
                .await;
                deadline_lease.timeout();
            });
            let _watchers = Watchers(vec![actor_watch, deadline_watch]);
            let workspace = args.workspace_id.clone();
            let mut result = core
                .control_motion_tests_inner(&actor, args, Some(task_lease.clone()))
                .await;
            let status = task_lease.finish(&mut result);
            if let Ok(reply) = &mut result {
                reply.run = status;
            }
            let _ = core.activity.send(crate::events::Activity {
                workspace_id: workspace,
                kind: "motion_test_run_changed".into(),
                job: None,
                actor: Some(actor),
                tool: Some("control_motion_tests".into()),
            });
            let _ = send.send(result);
        });
        if background {
            return Ok(accepted);
        }
        // Dropping an inline request cancels the owned operation. It never
        // abandons an entered CPU task or an atomic result-persistence boundary.
        let _cancel = CancelOnDrop(lease);
        receive
            .await
            .map_err(|_| invalid("Simulation test task ended unexpectedly"))?
    }

    async fn control_motion_tests_inner(
        &self,
        actor: &str,
        args: Control,
        lease: Option<crate::motion_test_runs::Lease>,
    ) -> Result<Reply> {
        self.ensure_actor_active(actor)?;
        let (mut next, revision) = {
            let workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get(&args.workspace_id)
                .ok_or_else(|| invalid("Unknown workspace"))?;
            if ws.stored.current != args.revision {
                return Err(Error::domain("STALE_ENTITY", "Design revision changed"));
            }
            if matches!(args.command, Command::List) {
                return reply(
                    &ws.stored.motion_tests,
                    &args.revision,
                    None,
                    self.motion_test_runs.status(&args.workspace_id),
                );
            }
            let current = fingerprint(&ws.stored.motion_tests)?;
            if !matches!(args.command, Command::List)
                && args.expected_catalog.as_deref() != Some(&current)
            {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Motion tests changed; list and review before retrying",
                ));
            }
            (
                if matches!(args.command, Command::Replay { .. }) {
                    BTreeMap::new()
                } else {
                    ws.stored.motion_tests.clone()
                },
                matches!(args.command, Command::Run { .. })
                    .then(|| ws.stored.revisions[&args.revision].clone()),
            )
        };
        let changed = matches!(
            args.command,
            Command::Upsert { .. } | Command::Delete { .. } | Command::Run { .. }
        );
        match &args.command {
            Command::List | Command::Replay { .. } => (),
            Command::Upsert { id, definition } => {
                definition.validate()?;
                next.insert(
                    id.clone(),
                    Saved {
                        definition: definition.clone(),
                        last_run: None,
                    },
                );
            }
            Command::Delete { id } => {
                next.remove(id).ok_or_else(|| invalid("Unknown test"))?;
            }
            Command::Run { id } => {
                let lease = lease
                    .as_ref()
                    .ok_or_else(|| invalid("Missing simulation run lease"))?;
                let revision = revision.ok_or_else(|| invalid("Missing test revision"))?;
                if revision.model["rebuild_required"] == true {
                    return Err(invalid("Build the design before running tests"));
                }
                let definition = next
                    .get(id)
                    .ok_or_else(|| invalid("Unknown test"))?
                    .definition
                    .clone();
                let cancel = lease.signal();
                // Bounded construction admission also bounds CPU simulation runs.
                let permit = self.construction_slot(cancel.clone()).await?;
                lease.check()?;
                // A queued run must not spend CPU on a catalog/design already
                // superseded while it waited for construction admission.
                self.ensure_actor_active(actor)?;
                {
                    let workspaces = self.workspaces.lock().await;
                    let ws = workspaces
                        .get(&args.workspace_id)
                        .ok_or_else(|| invalid("Unknown workspace"))?;
                    if ws.stored.current != args.revision {
                        return Err(Error::domain(
                            "STALE_ENTITY",
                            "Design changed while the test was queued",
                        ));
                    }
                    if args.expected_catalog.as_deref()
                        != Some(&fingerprint(&ws.stored.motion_tests)?)
                    {
                        return Err(Error::domain(
                            "REVISION_CONFLICT",
                            "Motion tests changed while the run was queued",
                        ));
                    }
                }
                lease.running();
                let entered = lease.clone();
                let owner = self._process_lock.clone();
                let run = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let _entered = entered;
                    let _owner = owner;
                    execute(&definition, &revision, &cancel)
                })
                .await
                .map_err(|_| invalid("Simulation test task failed"))??;
                next.get_mut(id).unwrap().last_run = Some(run);
            }
            Command::StartRun { .. } | Command::CancelRun { .. } => {
                return Err(invalid("Invalid internal simulation operation"));
            }
        }
        if changed {
            if let Some(lease) = &lease {
                lease.check()?;
            }
            encode(&next)?;
        }
        let mut workspaces = self.workspaces.lock().await;
        self.ensure_actor_active(actor)?;
        let ws = workspaces
            .get_mut(&args.workspace_id)
            .ok_or_else(|| invalid("Unknown workspace"))?;
        if ws.stored.current != args.revision {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Design changed during test operation",
            ));
        }
        if !matches!(args.command, Command::List)
            && args.expected_catalog.as_deref() != Some(&fingerprint(&ws.stored.motion_tests)?)
        {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Motion tests changed during operation",
            ));
        }
        if changed {
            // Copy only the catalog: unrelated jobs/title/save binding may have
            // changed while the private simulation ran.
            if let Some(lease) = &lease {
                lease.check()?;
            }
            let mut stored = ws.stored.clone();
            stored.motion_tests = next;
            self.persist(&stored).await?;
            ws.stored = stored;
            let _ = self.activity.send(crate::events::Activity {
                workspace_id: args.workspace_id.clone(),
                kind: "motion_tests_changed".into(),
                job: None,
                actor: Some(actor.into()),
                tool: Some("control_motion_tests".into()),
            });
        }
        let mut snapshot = None;
        if let Command::Replay {
            id,
            frame_index,
            expected_pose_version,
        } = &args.command
        {
            let saved = ws
                .stored
                .motion_tests
                .get(id)
                .ok_or_else(|| invalid("Unknown test"))?;
            let run = saved
                .last_run
                .as_ref()
                .ok_or_else(|| invalid("Run the test before replaying it"))?;
            if run.revision != args.revision || run.engine != engine() {
                return Err(Error::domain(
                    "STALE_ENTITY",
                    "Rerun this test for the current design and engine",
                ));
            }
            let name = saved.definition.name.clone();
            let samples = run.samples.clone();
            let state = motion::state(ws)?;
            if state.version != *expected_pose_version || state.recorder.is_some() {
                return Err(invalid(
                    "Motion changed or recording is active; finish recording and retry",
                ));
            }
            let playback = playback::Playback::test_trace(
                id,
                &name,
                &args.revision,
                samples,
                *frame_index,
                &state.mechanism,
            )?;
            state.playback = Some(playback);
            state.version = state
                .version
                .checked_add(1)
                .ok_or_else(|| invalid("Pose version exhausted"))?;
            let mut value = motion::snapshot(ws)?;
            value.actor = Some(actor.into());
            let _ = self.motion_events.send(value.clone());
            snapshot = Some(value);
        }
        reply(
            &ws.stored.motion_tests,
            &args.revision,
            snapshot,
            self.motion_test_runs.status(&args.workspace_id),
        )
    }
}

fn format_number(value: f64) -> String {
    format!("{value:.8}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}
fn human_name(value: &str) -> String {
    value.replace('_', " ")
}
fn format_coordinate(value: f64, unit: &str) -> String {
    match unit {
        "m" => format!("{} mm", format_number(value * 1000.)),
        "rad" => format!("{}°", format_number(value * 180. / std::f64::consts::PI)),
        _ => format!("{} {unit}", format_number(value)),
    }
}
fn format_direction(value: [f64; 3]) -> String {
    let axes = ["X", "Y", "Z"];
    if let Some((axis, component)) = value
        .iter()
        .enumerate()
        .find(|(_, component)| component.abs() == 1.)
        && value
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != axis)
            .all(|(_, component)| *component == 0.)
    {
        let sign = if *component < 0. { '\u{2212}' } else { '+' };
        return format!("along world {sign}{}", axes[axis]);
    }
    format!(
        "along world direction ({})",
        value
            .iter()
            .map(|component| format_number(*component))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
fn check_statement(check: &Check) -> String {
    match check {
        Check::Displacement {
            body,
            direction,
            minimum_mm,
        } => format!(
            "{} finishes at least {} mm from its starting position, {}.",
            human_name(body),
            format_number(*minimum_mm),
            format_direction(*direction)
        ),
        Check::Coordinate {
            joint_id,
            target,
            unit,
            tolerance,
        } => format!(
            "{} finishes at {}, within ±{}.",
            human_name(joint_id),
            format_coordinate(*target, unit),
            format_coordinate(*tolerance, unit)
        ),
        Check::Push {
            pusher,
            receiver,
            driver_joint,
            direction,
            minimum_mm,
            minimum_additional_mm,
            minimum_sampled_impulse_ns,
        } => format!(
            "{} pushes {}. The receiver finishes at least {} mm from its starting position, {}, and moves at least {} mm farther than in the same simulation with the {} motor disabled. Direct pushing contact must reach a sampled normal impulse of at least {} N s.",
            human_name(pusher),
            human_name(receiver),
            format_number(*minimum_mm),
            format_direction(*direction),
            format_number(*minimum_additional_mm),
            human_name(driver_joint),
            format_number(*minimum_sampled_impulse_ns)
        ),
    }
}
fn measurement_count(check: &Check) -> usize {
    match check {
        Check::Push { .. } => 3,
        _ => 1,
    }
}
/// Deterministic plain-language requirements and results for agents, MCP and UI.
pub fn test_review(definition: &Definition, outcome: Option<&Outcome>) -> TestReview {
    let measurements = outcome
        .map(|value| value.measurements.as_slice())
        .unwrap_or(&[]);
    let mut offset = 0;
    let checks = definition
        .checks
        .iter()
        .map(|check| {
            let count = measurement_count(check);
            let slice = if outcome.is_some() && offset + count <= measurements.len() {
                measurements[offset..offset + count].to_vec()
            } else {
                Vec::new()
            };
            if outcome.is_some() {
                offset += count;
            }
            let met = (!slice.is_empty()).then(|| slice.iter().all(|item| item.passed));
            CheckReview {
                statement: check_statement(check),
                met,
                measurements: slice,
            }
        })
        .collect::<Vec<_>>();
    let summary = match outcome {
        None => "Not run".into(),
        Some(outcome) => {
            let scored = checks.iter().filter(|check| check.met.is_some()).count();
            let passed = checks
                .iter()
                .filter(|check| check.met == Some(true))
                .count();
            if scored == 0 {
                format!(
                    "{} of {} checks passed",
                    outcome
                        .measurements
                        .iter()
                        .filter(|item| item.passed)
                        .count(),
                    outcome.measurements.len()
                )
            } else {
                format!("{passed} of {scored} requirements met")
            }
        }
    };
    TestReview {
        requirement: definition.intent.clone(),
        checks,
        summary,
    }
}
fn reply(
    catalog: &Catalog,
    revision: &str,
    snapshot: Option<motion::Snapshot>,
    run: Option<crate::motion_test_runs::Status>,
) -> Result<Reply> {
    let tests = catalog
        .iter()
        .map(|(id, saved)| {
            let outcome = saved.last_run.as_ref().map(|run| run.outcome.clone());
            let review = test_review(&saved.definition, outcome.as_ref());
            TestInfo {
                id: id.clone(),
                definition: saved.definition.clone(),
                outcome,
                review,
                stale: saved
                    .last_run
                    .as_ref()
                    .is_none_or(|run| run.revision != revision || run.engine != engine()),
                frame_count: saved.last_run.as_ref().map_or(0, |run| run.samples.len()),
                duration_s: saved
                    .last_run
                    .as_ref()
                    .and_then(|run| run.samples.last())
                    .map_or(0., |s| s.time_s),
            }
        })
        .collect();
    Ok(Reply { catalog: fingerprint(catalog)?, tests, run,
            accuracy: "Approximate collision proxies and inertia. Push checks use peak sampled last-substep normal impulse, not average force; motion compared with driver disabled. CAD simulation only.".into(), snapshot })
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn plain_language_review_matches_ui_wording_and_pairs_measurements() {
        let definition: Definition = serde_json::from_value(json!({
            "name":"Lever pushes shoe",
            "intent":"Make sure the lever reaches the shoe and pushes it.",
            "initial_coordinates":{"slide":0,"hinge":0},
            "simulation":{"gravity_m_s2":[0,0,-9.81],"bodies":{"lever":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.2},"friction":0.3,"restitution":0},"shoe":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":1},"friction":0.4,"restitution":0.1}}},
            "actions":[{"ticks":60,"motors":[],"loads":{}}],
            "checks":[
                {"type":"push","pusher":"lever","receiver":"shoe","driver_joint":"hinge","direction":[1,0,0],"minimum_mm":5,"minimum_additional_mm":4,"minimum_sampled_impulse_ns":0.000001},
                {"type":"coordinate","joint_id":"slide","target":0.01,"unit":"m","tolerance":0.0005},
                {"type":"coordinate","joint_id":"hinge","target":1.5707963267948966,"unit":"rad","tolerance":0.017453292519943295},
                {"type":"displacement","body":"shoe","direction":[0,-1,0],"minimum_mm":2}
            ]
        }))
        .unwrap();
        let unread = test_review(&definition, None);
        assert_eq!(unread.summary, "Not run");
        assert_eq!(unread.requirement, definition.intent);
        assert!(unread.checks[0].statement.contains("lever pushes shoe"));
        assert!(unread.checks[0].statement.contains("at least 5 mm"));
        assert!(unread.checks[0].statement.contains("world +X"));
        assert!(unread.checks[0].statement.contains("hinge motor disabled"));
        assert!(
            unread.checks[0]
                .statement
                .contains("normal impulse of at least 0.000001 N s")
        );
        assert_eq!(
            unread.checks[1].statement,
            "slide finishes at 10 mm, within ±0.5 mm."
        );
        assert_eq!(
            unread.checks[2].statement,
            "hinge finishes at 90°, within ±1°."
        );
        assert!(unread.checks[3].statement.contains("world \u{2212}Y"));
        assert!(unread.checks.iter().all(|check| check.met.is_none()));

        let outcome = Outcome {
            passed: false,
            measurements: vec![
                Measurement {
                    label: "shoe: final displacement".into(),
                    actual: 6.,
                    expected: 5.,
                    unit: "mm".into(),
                    passed: true,
                },
                Measurement {
                    label: "shoe: additional".into(),
                    actual: 3.,
                    expected: 4.,
                    unit: "mm".into(),
                    passed: false,
                },
                Measurement {
                    label: "impulse".into(),
                    actual: 1e-6,
                    expected: 1e-6,
                    unit: "N s".into(),
                    passed: true,
                },
                Measurement {
                    label: "slide".into(),
                    actual: 0.,
                    expected: 0.,
                    unit: "m".into(),
                    passed: true,
                },
                Measurement {
                    label: "hinge".into(),
                    actual: 0.,
                    expected: 0.,
                    unit: "rad".into(),
                    passed: true,
                },
                Measurement {
                    label: "shoe: final displacement".into(),
                    actual: 1.,
                    expected: 2.,
                    unit: "mm".into(),
                    passed: false,
                },
            ],
        };
        let reviewed = test_review(&definition, Some(&outcome));
        assert_eq!(reviewed.summary, "2 of 4 requirements met");
        assert_eq!(reviewed.checks[0].met, Some(false));
        assert_eq!(reviewed.checks[0].measurements.len(), 3);
        assert_eq!(reviewed.checks[1].met, Some(true));
        assert_eq!(reviewed.checks[3].met, Some(false));
    }
}

#[cfg(test)]
mod run_control_tests {
    use super::*;
    use crate::motion_test_runs::State;
    use serde_json::json;
    async fn fixture() -> (tempfile::TempDir, Core, String, String, String) {
        // Synthetic coverage metadata isolates scheduling/transaction tests.
        // The native integration test verifies BRep-covered execution separately.
        let temp = tempfile::tempdir().unwrap();
        let core = Core::open(
            temp.path().join("app"),
            crate::worker::WorkerConfig::bundled("unused-worker".into()),
        )
        .await
        .unwrap();
        let ws = core.create_workspace("actor").await.unwrap();
        let rev = {
            let mut all = core.workspaces.lock().await;
            let w = all.get_mut(&ws).unwrap();
            let rev = w.stored.current.clone();
            let revision = w.stored.revisions.get_mut(&rev).unwrap();
            revision.inputs.mechanism = json!({"joints":[{"id":"slide","parent":null,"child":"block","kind":{"type":"prismatic","axis":[1,0,0]},"initial":0,"limits":[0,0.02]}],"collision":{"bodies":{"block":[{"shape":{"type":"bounds"}}]},"clearance_m":0,"backoff_m":1e-6}});
            revision.model = json!({"parts":[{"instance_id":"block","position":[0,0,0],"bounds":{"min":[-5,-5,-5],"max":[5,5,5]},"volume":1000}],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["block"]}});
            rev
        };
        let catalog = call(&core, &ws, &rev, "actor", None, json!({"action":"list"}))
            .await
            .unwrap()
            .catalog;
        let saved=call(&core,&ws,&rev,"actor",Some(catalog),json!({"action":"upsert","id":"test","definition":{"name":"Slider response","intent":"Move at least 0.1 mm","initial_coordinates":{"slide":0},"simulation":{"gravity_m_s2":[0,0,0],"bodies":{"block":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":1},"friction":0,"restitution":0}}},"actions":[{"ticks":30,"motors":[{"joint_id":"slide","target":0.01,"unit":"m","stiffness":100,"damping":2,"max_force":1}],"loads":{}}],"checks":[{"type":"displacement","body":"block","direction":[1,0,0],"minimum_mm":0.1}]}})).await.unwrap();
        let completed = call(
            &core,
            &ws,
            &rev,
            "actor",
            Some(saved.catalog),
            json!({"action":"run","id":"test"}),
        )
        .await
        .unwrap();
        assert_eq!(completed.run.as_ref().unwrap().state, State::Completed);
        (temp, core, ws, rev, completed.catalog)
    }
    async fn call(
        core: &Core,
        ws: &str,
        rev: &str,
        actor: &str,
        catalog: Option<String>,
        command: serde_json::Value,
    ) -> Result<Reply> {
        core.control_motion_tests(actor,serde_json::from_value(json!({"workspace_id":ws,"revision":rev,"expected_catalog":catalog,"command":command})).unwrap()).await
    }
    async fn terminal(core: &Core, ws: &str, rev: &str) -> Reply {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let reply = call(core, ws, rev, "local-user", None, json!({"action":"list"}))
                    .await
                    .unwrap();
                if reply.run.as_ref().is_some_and(|run| !run.state.active()) {
                    break reply;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn queued_background_stop_is_scoped_and_preserves_the_previous_result_definition_and_live_pose()
     {
        let (_temp, core, ws, rev, catalog) = fixture().await;
        let before = serde_json::to_value(core.get_motion(&ws, &rev).await.unwrap()).unwrap();
        let (_send, signal) = tokio::sync::watch::channel(false);
        let one = core.construction_slot(signal.clone()).await.unwrap();
        let two = core.construction_slot(signal).await.unwrap();
        let started = call(
            &core,
            &ws,
            &rev,
            "actor",
            Some(catalog.clone()),
            json!({"action":"start_run","id":"test"}),
        )
        .await
        .unwrap();
        let run = started.run.unwrap();
        assert_eq!(run.state, State::Queued);
        assert!(
            call(
                &core,
                &ws,
                &rev,
                "actor",
                Some(catalog.clone()),
                json!({"action":"start_run","id":"test"})
            )
            .await
            .is_err()
        );
        assert_eq!(
            call(
                &core,
                &ws,
                &rev,
                "intruder",
                None,
                json!({"action":"cancel_run","run_id":run.id})
            )
            .await
            .err()
            .unwrap()
            .code(),
            "UNSUPPORTED_OPERATION"
        );
        // Stop remains possible even if the caller's design/catalog observation
        // is old. Only the exact workspace-scoped operation is targeted.
        let stopped = call(
            &core,
            &ws,
            "old-revision",
            "local-user",
            None,
            json!({"action":"cancel_run","run_id":run.id}),
        )
        .await
        .unwrap();
        assert!(matches!(
            stopped.run.unwrap().state,
            State::Cancelling | State::Cancelled
        ));
        let stopped = terminal(&core, &ws, &rev).await;
        assert_eq!(stopped.run.unwrap().state, State::Cancelled);
        assert_eq!(stopped.catalog, catalog);
        assert!(stopped.tests[0].outcome.is_some());
        assert_eq!(
            before,
            serde_json::to_value(core.get_motion(&ws, &rev).await.unwrap()).unwrap()
        );
        assert_eq!(core.construction_slots.available_permits(), 0);
        drop((one, two));
    }
    #[tokio::test]
    async fn revoked_actor_and_dropped_inline_wait_cancel_owned_queued_work_without_result_commits()
    {
        let (_temp, core, ws, rev, catalog) = fixture().await;
        let (_send, signal) = tokio::sync::watch::channel(false);
        let one = core.construction_slot(signal.clone()).await.unwrap();
        let two = core.construction_slot(signal).await.unwrap();
        call(
            &core,
            &ws,
            &rev,
            "agent",
            Some(catalog.clone()),
            json!({"action":"start_run","id":"test"}),
        )
        .await
        .unwrap();
        assert_eq!(core.revoke_actor("agent").await, 1);
        assert_eq!(
            terminal(&core, &ws, &rev).await.run.unwrap().state,
            State::Cancelled
        );
        let task = {
            let core = core.clone();
            let ws = ws.clone();
            let rev = rev.clone();
            let catalog = catalog.clone();
            tokio::spawn(async move {
                call(
                    &core,
                    &ws,
                    &rev,
                    "inline",
                    Some(catalog),
                    json!({"action":"run","id":"test"}),
                )
                .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !core
                .motion_test_runs
                .status(&ws)
                .is_some_and(|run| run.actor == "inline" && run.state.active())
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        task.abort();
        let _ = task.await;
        let ended = terminal(&core, &ws, &rev).await;
        assert_eq!(ended.run.unwrap().state, State::Cancelled);
        assert_eq!(ended.catalog, catalog);
        drop((one, two));
    }
    #[tokio::test]
    async fn catalog_changes_while_queued_fail_without_replacing_the_edited_definition() {
        let (_temp, core, ws, rev, catalog) = fixture().await;
        let (_send, signal) = tokio::sync::watch::channel(false);
        let one = core.construction_slot(signal.clone()).await.unwrap();
        let two = core.construction_slot(signal).await.unwrap();
        let started = call(
            &core,
            &ws,
            &rev,
            "actor",
            Some(catalog.clone()),
            json!({"action":"start_run","id":"test"}),
        )
        .await
        .unwrap();
        let mut definition = serde_json::to_value(&started.tests[0].definition).unwrap();
        definition["name"] = json!("Edited while waiting");
        let edited = call(
            &core,
            &ws,
            &rev,
            "editor",
            Some(catalog),
            json!({"action":"upsert","id":"test","definition":definition}),
        )
        .await
        .unwrap();
        drop((one, two));
        let ended = terminal(&core, &ws, &rev).await;
        assert_eq!(ended.run.as_ref().unwrap().state, State::Failed);
        assert_eq!(ended.run.unwrap().error.unwrap().code, "REVISION_CONFLICT");
        assert_eq!(ended.catalog, edited.catalog);
        assert_eq!(ended.tests[0].definition.name, "Edited while waiting");
        assert!(ended.tests[0].outcome.is_none());
    }
}
