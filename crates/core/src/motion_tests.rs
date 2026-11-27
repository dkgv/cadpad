//! The project requirement catalog and the legacy motion-test entries still
//! stored in it. `control_motion_tests` is retired: model-bound tests migrate
//! to requirements on read ([`crate::motion_migration`]) and model-less ones
//! are dropped. What remains here is the shared catalog type, its bounds and
//! the simulation-state runner requirements evaluate through.
use crate::{Core, Error, Result, dynamics, kinematics, playback, portable};
use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub(crate) type Catalog = BTreeMap<String, Saved>;
fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_MOTION_TEST", message)
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// 1/120 s ticks. Motor targets persist between actions; loads replace.
    pub ticks: u32,
    pub motors: Vec<dynamics::Motor>,
    pub loads: BTreeMap<String, dynamics::Load>,
}

/// Reusable input portion of a motion test, embedded by requirement simulation
/// states. Keeps the legacy action and dynamics payloads byte-compatible.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SimulationState {
    pub initial_coordinates: BTreeMap<String, f64>,
    pub simulation: dynamics::Definition,
    pub actions: Vec<Action>,
}

/// One captured sample for requirements proxy evaluation. Pose positions use
/// the existing kinematics convention (mm); contact impulses are N s.
#[derive(Clone, Debug)]
pub(crate) struct RequirementSimulationSample {
    pub evaluation: kinematics::Evaluation,
    pub contact_impulses: BTreeMap<(String, String), [f64; 3]>,
    pub baseline_poses: BTreeMap<String, BTreeMap<String, kinematics::Pose>>,
}

/// Run a requirement simulation with the same Rust dynamics engine and action
/// semantics as saved motion tests, capturing every fixed tick. Push baselines
/// omit only their selected driver motor while retaining all loads and other
/// motors, exactly as the retired motion-test checker did.
pub(crate) fn simulate_requirement_state(
    revision: &crate::Revision,
    state: &SimulationState,
    contact_pairs: &[(String, String)],
    driver_joints: &[String],
    cancel: &tokio::sync::watch::Receiver<bool>,
) -> Result<Vec<RequirementSimulationSample>> {
    let mut definition = kinematics::decode(&revision.inputs.mechanism)?;
    let moving: std::collections::BTreeSet<_> = definition
        .joints
        .iter()
        .filter(|joint| !matches!(joint.kind, kinematics::JointKind::Fixed))
        .map(|joint| joint.id.clone())
        .collect();
    if state
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
    for joint in &mut definition.joints {
        if let Some(value) = state.initial_coordinates.get(&joint.id) {
            joint.initial = *value;
        }
    }
    let mechanism = kinematics::Mechanism::new(definition, &revision.model["parts"])?;
    mechanism.evaluate()?;
    let mut simulation =
        dynamics::Simulation::new(state.simulation.clone(), &mechanism, &revision.model)?;
    // Validate every action before the bounded simulation spends any ticks.
    for action in &state.actions {
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
    let mut baselines: BTreeMap<String, dynamics::Simulation> = driver_joints
        .iter()
        .cloned()
        .map(|joint| (joint, simulation.clone()))
        .collect();
    let capture = |simulation: &dynamics::Simulation,
                   baselines: &BTreeMap<String, dynamics::Simulation>,
                   mechanism: &kinematics::Mechanism|
     -> Result<RequirementSimulationSample> {
        let evaluation = simulation.evaluation(mechanism)?;
        let mut contact_impulses = BTreeMap::new();
        for (a, b) in contact_pairs {
            contact_impulses.insert((a.clone(), b.clone()), simulation.contact_impulse(a, b)?);
        }
        let mut baseline_poses = BTreeMap::new();
        for (driver, baseline) in baselines {
            baseline_poses.insert(driver.clone(), baseline.evaluation(mechanism)?.poses);
        }
        Ok(RequirementSimulationSample {
            evaluation,
            contact_impulses,
            baseline_poses,
        })
    };
    let mut samples = vec![capture(&simulation, &baselines, &mechanism)?];
    for action in &state.actions {
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
            simulation.step(1, Some(cancel))?;
            for baseline in baselines.values_mut() {
                baseline.step(1, Some(cancel))?;
            }
            samples.push(capture(&simulation, &baselines, &mechanism)?);
        }
    }
    Ok(samples)
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
/// A model in the project's source catalog: source file plus builder ID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    pub entry: String,
    pub builder: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub name: String,
    pub intent: String,
    /// Project model this requirement checks. Omit when saving from a model's
    /// workspace: that model is recorded. None means the addressed design itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
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
pub struct SavedTest {
    pub definition: Definition,
    pub last_run: Option<Run>,
}
/// One entry of the project catalog. Motion tests and compiled requirements
/// share the catalog (one CAS fingerprint, one archive, one Save/Open path).
/// Test entries serialize exactly as before; requirement entries carry
/// `kind: "requirement"`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)] // Test entries are the long-standing shape; keep their layout.
pub enum Saved {
    Test(SavedTest),
    Requirement(Box<crate::requirements::storage::RequirementEntry>),
    ProjectSettings(crate::requirements::settings::ProjectSettingsRecord),
}
impl Saved {
    pub fn test(&self) -> Option<&SavedTest> {
        match self {
            Saved::Test(test) => Some(test),
            Saved::Requirement(_) => None,
            Saved::ProjectSettings(_) => None,
        }
    }
    pub fn test_mut(&mut self) -> Option<&mut SavedTest> {
        match self {
            Saved::Test(test) => Some(test),
            Saved::Requirement(_) => None,
            Saved::ProjectSettings(_) => None,
        }
    }
    pub fn requirement(&self) -> Option<&crate::requirements::storage::RequirementEntry> {
        match self {
            Saved::Requirement(entry) => Some(entry),
            Saved::Test(_) => None,
            Saved::ProjectSettings(_) => None,
        }
    }
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
pub(crate) fn fingerprint<T: Serialize>(value: &T) -> Result<String> {
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
        if let Some(model) = &self.model
            && [&model.entry, &model.builder].iter().any(|value| {
                value.trim().is_empty() || value.len() > 1024 || value.chars().any(char::is_control)
            })
        {
            return Err(invalid("Model needs a source entry and builder ID"));
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

pub(crate) fn catalog_fingerprint(catalog: &Catalog) -> Result<Option<String>> {
    if catalog.is_empty() {
        Ok(None)
    } else {
        Ok(Some(fingerprint(catalog)?))
    }
}
pub(crate) fn encode(catalog: &Catalog) -> Result<Vec<u8>> {
    if catalog
        .values()
        .filter(|saved| saved.test().is_some())
        .count()
        > 16
    {
        return Err(invalid("At most 16 saved motion tests"));
    }
    crate::requirements::storage::check_count(catalog)?;
    if catalog
        .values()
        .filter(|saved| matches!(saved, Saved::ProjectSettings(_)))
        .count()
        > 1
    {
        return Err(invalid("At most one project settings record"));
    }
    for (id, saved) in catalog {
        match saved {
            Saved::Test(test) => {
                if id.is_empty() || id.len() > 120 || id.chars().any(char::is_control) {
                    return Err(invalid("Invalid test ID"));
                }
                test.definition.validate()?;
            }
            Saved::Requirement(entry) => entry.validate(id)?,
            Saved::ProjectSettings(settings) => {
                if !id.starts_with(crate::requirements::settings::CATALOG_SETTINGS_PREFIX) {
                    return Err(invalid("Invalid project settings catalog key"));
                }
                settings
                    .project_settings
                    .validate()
                    .into_iter()
                    .next()
                    .map_or(Ok(()), |d| {
                        Err(Error::domain(
                            "INVALID_REQUIREMENT",
                            format!("Invalid project settings: {}", d.message),
                        ))
                    })?
            }
        }
    }
    let bytes = serde_json::to_vec(catalog)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid(
            "Motion tests and requirements exceed 8 MiB; delete one or shorten traces",
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
        match saved {
            Saved::Test(test) => test.last_run = None,
            Saved::Requirement(entry) => entry.mark_imported(),
            Saved::ProjectSettings(_) => {}
        }
    }
    encode(&catalog)?;
    Ok(catalog)
}

/// A design workspace as seen by the requirements catalog. Model views of a
/// project share the project's catalog; standalone workspaces own their own.
#[derive(Clone)]
pub(crate) struct View {
    pub(crate) id: String,
    /// Workspace that stores the requirement catalog.
    pub(crate) owner: String,
    /// The project model this view displays; None for the project/standalone design.
    pub(crate) model: Option<ModelRef>,
    pub(crate) inputs: String,
    /// Copy of the catalog left in a model view by earlier versions.
    legacy: Catalog,
}
impl Core {
    pub(crate) async fn requirement_revision(
        &self,
        workspace_id: &str,
        revision_id: &str,
    ) -> Result<crate::Revision> {
        let _memory = self.pin_workspace(workspace_id).await?;
        let all = self.workspace(workspace_id).await?;
        let ws = all
            .get(workspace_id)
            .ok_or_else(|| invalid("Unknown workspace"))?;
        Ok((*ws.stored.revisions.load(revision_id).await?).clone())
    }

    /// Requirements are stored once per project, on the workspace that model
    /// views branch from. Standalone workspaces store their own.
    pub(crate) async fn requirement_owner(&self, workspace_id: &str) -> Result<String> {
        let parent = {
            let all = self.workspace_metadata(workspace_id).await?;
            all[workspace_id].stored.model_parent.clone()
        };
        Ok(match parent {
            Some(parent) if self.has_workspace(&parent).await => parent,
            _ => workspace_id.to_owned(),
        })
    }
    pub(crate) async fn requirement_view(&self, workspace_id: &str) -> Result<View> {
        let owner = self.requirement_owner(workspace_id).await?;
        let all = self.workspace(workspace_id).await?;
        let ws = all
            .get(workspace_id)
            .ok_or_else(|| invalid("Unknown workspace"))?;
        let revision = ws.stored.current_revision();
        let model = (owner != workspace_id)
            .then(|| crate::project_entry::inputs_model_entry(&revision.inputs))
            .flatten()
            .map(|(entry, builder)| ModelRef { entry, builder });
        Ok(View {
            id: workspace_id.to_owned(),
            owner,
            model,
            inputs: portable::fingerprint(&revision.inputs)?,
            legacy: ws.stored.motion_tests.clone(),
        })
    }
    /// Earlier versions copied the project catalog into every model view and
    /// edited that copy. Fold such copies into the project once, assigning
    /// requirements that only exist in the view to the view's model.
    async fn adopt_view_requirements(&self, view: &View, actor: &str) -> Result<()> {
        if view.owner == view.id || view.legacy.is_empty() {
            return Ok(());
        }
        {
            let mut all = self.workspace(&view.owner).await?;
            let ws = all
                .get_mut(&view.owner)
                .ok_or_else(|| invalid("Unknown project workspace"))?;
            let mut next = ws.stored.motion_tests.clone();
            for (id, saved) in &view.legacy {
                if next.contains_key(id) {
                    continue;
                }
                let mut saved = saved.clone();
                if let Some(test) = saved.test_mut()
                    && test.definition.model.is_none()
                {
                    test.definition.model = view.model.clone();
                }
                next.insert(id.clone(), saved);
            }
            if next.len() != ws.stored.motion_tests.len() {
                // Over capacity: keep the view copy so nothing is silently lost.
                if encode(&next).is_err() {
                    return Ok(());
                }
                let mut stored = ws.stored.clone();
                stored.motion_tests = next;
                self.persist(&stored).await?;
                ws.stored = stored;
                let _ = self.activity.send(crate::events::Activity {
                    workspace_id: view.owner.clone(),
                    kind: "requirements_changed".into(),
                    job: None,
                    actor: Some(actor.into()),
                    tool: Some("control_requirements".into()),
                });
            }
        }
        let mut all = self.workspace(&view.id).await?;
        if let Some(ws) = all.get_mut(&view.id)
            && !ws.stored.motion_tests.is_empty()
        {
            let mut stored = ws.stored.clone();
            stored.motion_tests.clear();
            self.persist(&stored).await?;
            ws.stored = stored;
        }
        Ok(())
    }
    /// saving and unsaved-change tracking use instead of the view's own.
    pub(crate) async fn project_requirements(
        &self,
        workspace_id: &str,
    ) -> Result<Option<(Catalog, Option<portable::Binding>)>> {
        let owner = self.requirement_owner(workspace_id).await?;
        if owner == workspace_id {
            return Ok(None);
        }
        let legacy = {
            let all = self.workspace(workspace_id).await?;
            all.get(workspace_id)
                .is_some_and(|ws| !ws.stored.motion_tests.is_empty())
        };
        if legacy {
            let view = self.requirement_view(workspace_id).await?;
            self.adopt_view_requirements(&view, "local-user").await?;
        }
        let all = self.workspace(&owner).await?;
        let ws = all
            .get(&owner)
            .ok_or_else(|| invalid("Unknown project workspace"))?;
        Ok(Some((
            ws.stored.motion_tests.clone(),
            ws.stored.project.clone(),
        )))
    }
    /// After a model view saved into the project folder, record the written
    /// requirement archive on the project's binding too.
    pub(crate) async fn note_project_requirements_saved(
        &self,
        workspace_id: &str,
        saved: &portable::Binding,
    ) -> Result<()> {
        let owner = self.requirement_owner(workspace_id).await?;
        if owner == workspace_id {
            return Ok(());
        }
        let mut all = self.workspace(&owner).await?;
        let Some(ws) = all.get_mut(&owner) else {
            return Ok(());
        };
        let Some(binding) = ws.stored.project.as_ref() else {
            return Ok(());
        };
        if binding.directory != saved.directory
            || (binding.motion_tests == saved.motion_tests
                && binding.motion_test_fingerprint == saved.motion_test_fingerprint)
        {
            return Ok(());
        }
        let mut stored = ws.stored.clone();
        let binding = stored.project.as_mut().unwrap();
        binding.motion_tests = saved.motion_tests.clone();
        binding.motion_test_fingerprint = saved.motion_test_fingerprint.clone();
        self.persist(&stored).await?;
        ws.stored = stored;
        Ok(())
    }
    async fn catalog(&self, owner: &str) -> Result<Catalog> {
        let all = self.workspace(owner).await?;
        Ok(all
            .get(owner)
            .ok_or_else(|| invalid("Unknown workspace"))?
            .stored
            .motion_tests
            .clone())
    }

    /// Owner workspace and current catalog, for requirement operations that
    /// share this catalog (compiled requirements, `requirements::storage`).
    pub(crate) async fn requirement_catalog(
        &self,
        actor: &str,
        workspace_id: &str,
    ) -> Result<(String, Catalog)> {
        let addressed = self.requirement_view(workspace_id).await?;
        self.adopt_view_requirements(&addressed, actor).await?;
        let catalog = self.catalog(&addressed.owner).await?;
        let catalog = crate::motion_migration::migrate(self, actor, &addressed, catalog).await?;
        Ok((addressed.owner, catalog))
    }
    /// Replace the owner's catalog if it still has the `expected` fingerprint.
    pub(crate) async fn commit_requirement_catalog(
        &self,
        actor: &str,
        owner: &str,
        workspace_id: &str,
        expected: &str,
        next: &Catalog,
        run: Option<crate::requirements::run::ResultGuard<'_>>,
    ) -> Result<()> {
        encode(next)?;
        let mut workspaces = self.workspace(owner).await?;
        self.ensure_actor_active(actor)?;
        if let Some(run) = run {
            if *run.cancel.borrow() {
                return Err(Error::domain("CANCELLED", "Requirement run cancelled"));
            }
            if run.views.values().any(|view| {
                workspaces
                    .get(&view.workspace)
                    .is_none_or(|ws| ws.stored.current != view.revision)
            }) {
                return Err(Error::domain(
                    "STALE_ENTITY",
                    "The model changed before the result could be saved",
                ));
            }
        }
        let ws = workspaces
            .get_mut(owner)
            .ok_or_else(|| invalid("Unknown workspace"))?;
        if fingerprint(&ws.stored.motion_tests)? != expected {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Requirements changed; list and review before retrying",
            ));
        }
        let mut stored = ws.stored.clone();
        stored.motion_tests = next.clone();
        self.persist(&stored).await?;
        ws.stored = stored;
        drop(workspaces);
        for workspace_id in std::collections::BTreeSet::from([owner, workspace_id]) {
            let _ = self.activity.send(crate::events::Activity {
                workspace_id: workspace_id.to_owned(),
                kind: "requirements_changed".into(),
                job: None,
                actor: Some(actor.into()),
                tool: Some("control_requirements".into()),
            });
        }
        Ok(())
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
pub(crate) fn check_statement(check: &Check) -> String {
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

#[cfg(test)]
mod tests {
    //! Engine-level golden coverage for the simulation-state runner that
    //! requirement `simulate` states evaluate through. The retired
    //! `control_motion_tests` check semantics (push baselines, repeatability,
    //! cancellation) live on in these traces.
    use super::*;
    use serde_json::json;

    fn fixture() -> (crate::Revision, SimulationState) {
        // Engine-only synthetic coverage. Packaged CAD coverage is a separate check.
        let mechanism = json!({"joints":[
            {"id":"lever_hinge","parent":null,"child":"lever","parent_frame":{"position_m":[0,0,0]},"child_frame":{"position_m":[-0.04,0,0]},"kind":{"type":"revolute","axis":[0,0,1]},"initial":0,"limits":[0,1]},
            {"id":"shoe_slide","parent":null,"child":"shoe","parent_frame":{"position_m":[0.075,0.012,0]},"kind":{"type":"prismatic","axis":[0,1,0]},"initial":0,"limits":[0,0.04]}
        ],"collision":{"bodies":{"lever":[{"shape":{"type":"bounds"}}],"shoe":[{"shape":{"type":"bounds"}}]},"clearance_m":0,"backoff_m":0.000001}});
        let revision = crate::Revision {
            id: uuid::Uuid::new_v4().to_string(),
            parent: None,
            actor: "test".into(),
            restored_from: None,
            inputs: crate::Inputs {
                mechanism,
                ..crate::Inputs::default()
            },
            model: json!({"parts":[
                {"instance_id":"lever","position":[40,0,0],"bounds":{"min":[-40,-5,-5],"max":[40,5,5]},"volume":8000},
                {"instance_id":"shoe","position":[75,12,0],"bounds":{"min":[-10,-5,-5],"max":[10,5,5]},"volume":2000}
            ],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["lever","shoe"]}}),
        };
        let state: SimulationState = serde_json::from_value(json!({
            "initial_coordinates":{"lever_hinge":0,"shoe_slide":0},
            "simulation":{"gravity_m_s2":[0,0,0],"bodies":{
                "lever":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.2},"friction":0,"restitution":0},
                "shoe":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.1},"friction":0,"restitution":0}
            }},
            "actions":[{"ticks":120,"motors":[{"joint_id":"lever_hinge","target":0.3,"unit":"rad","stiffness":0.5,"damping":0.03,"max_force":0.2}],"loads":{"shoe":{"force_n":[0,-0.5,0],"torque_nm":[0,0,0]}}}]
        }))
        .unwrap();
        (revision, state)
    }

    fn run(
        revision: &crate::Revision,
        state: &SimulationState,
        cancel: &tokio::sync::watch::Receiver<bool>,
    ) -> Result<Vec<RequirementSimulationSample>> {
        simulate_requirement_state(
            revision,
            state,
            &[("lever".to_owned(), "shoe".to_owned())],
            &["lever_hinge".to_owned()],
            cancel,
        )
    }

    #[test]
    fn lever_contact_push_trace_is_repeatable_and_the_driver_baseline_stays_put() {
        let (revision, state) = fixture();
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        let first = run(&revision, &state, &cancel).unwrap();
        assert_eq!(first.len(), 121);
        let second = run(&revision, &state, &cancel).unwrap();
        assert_eq!(
            format!("{first:?}"),
            format!("{second:?}"),
            "identical inputs must produce identical traces"
        );
        // Direct normal contact happened during the push.
        assert!(
            first.iter().any(|sample| sample
                .contact_impulses
                .get(&("lever".to_owned(), "shoe".to_owned()))
                .is_some_and(|impulse| impulse.iter().any(|v| v.abs() > 0.))),
            "expected sampled contact between lever and shoe"
        );
        // The shoe travelled in +y (pose positions are mm).
        let start_y = first[0].evaluation.poses["shoe"].position[1];
        let end_y = first.last().unwrap().evaluation.poses["shoe"].position[1];
        assert!(
            end_y - start_y > 4.,
            "shoe barely moved: {start_y} -> {end_y}"
        );
        // The driver-disabled baseline kept the lever parked, so its shoe
        // trace stays at the start (modulo the small persistent load).
        let baseline = &first
            .last()
            .unwrap()
            .baseline_poses
            .get("lever_hinge")
            .expect("driver baseline pose");
        let baseline_y = baseline["shoe"].position[1];
        assert!(
            (baseline_y - start_y).abs() < (end_y - start_y) / 2.,
            "baseline shoe moved as much as the driven run: {baseline_y} vs {end_y}"
        );
    }

    #[test]
    fn cancelled_missing_coordinate_and_out_of_limit_motor_never_run() {
        let (revision, mut state) = fixture();
        let (_sender, cancel) = tokio::sync::watch::channel(true);
        assert_eq!(
            run(&revision, &state, &cancel).unwrap_err().code(),
            "CANCELLED"
        );
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        state.initial_coordinates.remove("shoe_slide");
        assert_eq!(
            run(&revision, &state, &cancel).unwrap_err().code(),
            "INVALID_MOTION_TEST"
        );
        state = fixture().1;
        state.actions[0].motors[0].target = 2.; // past the declared [0, 1] limit
        assert!(run(&revision, &state, &cancel).is_err());
    }
}
