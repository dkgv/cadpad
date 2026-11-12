//! Sole owner of ephemeral manipulation poses. Definitions are portable inputs;
//! moving a coordinate never edits design geometry or starts a CAD build.
use crate::{
    Core, Draft, Error, Job, Result, Workspace, hash, id,
    kinematics::{self, Definition, Evaluation, Mechanism},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) struct State {
    pub mechanism: Mechanism,
    /// Last pose that passed swept collision (or the initial pose when collision
    /// is not configured). Interactive previews advance `mechanism` without this.
    validated_coordinates: Vec<f64>,
    pub version: u64,
    pub collision: Option<crate::collision::World>,
    pub simulation: Option<std::sync::Arc<crate::dynamics::Simulation>>,
    pub(crate) recorder: Option<crate::playback::Recorder>,
    pub(crate) playback: Option<crate::playback::Playback>,
}
impl State {
    pub(crate) fn new(inputs: &crate::Inputs, model: &Value) -> Result<Self> {
        let mechanism = Mechanism::new(kinematics::decode(&inputs.mechanism)?, &model["parts"])?;
        let collision = crate::collision::World::for_model(&mechanism, model)?;
        let validated_coordinates = mechanism.coordinates().to_vec();
        Ok(Self {
            mechanism,
            validated_coordinates,
            collision,
            version: 0,
            simulation: None,
            recorder: None,
            playback: None,
        })
    }
    fn mark_validated(&mut self) {
        self.validated_coordinates = self.mechanism.coordinates().to_vec();
    }
}
#[derive(JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotMode {
    Manipulate,
    Simulate,
}
#[derive(JsonSchema)]
pub enum CadUnits {
    #[serde(rename = "mm")]
    Millimetres,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Snapshot {
    pub workspace_id: String,
    pub revision: String,
    pub pose_version: u64,
    #[schemars(with = "SnapshotMode")]
    pub mode: String,
    #[schemars(with = "CadUnits")]
    pub cad_units: String,
    pub collision_checked: bool,
    pub collision: Option<crate::collision::Policy>,
    pub simulation_unavailable_reason: Option<String>,
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simulation: Option<crate::dynamics::Snapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording: Option<crate::playback::Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playback: Option<crate::playback::Selection>,
    #[serde(flatten)]
    pub evaluation: Evaluation,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetMotion {
    pub workspace_id: String,
    pub revision: String,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DefineMechanism {
    pub workspace_id: String,
    pub expected_base_revision: String,
    pub draft_id: Option<String>,
    pub definition: Definition,
}
fn default_check_collision() -> bool {
    true
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetCoordinate {
    pub workspace_id: String,
    pub revision: String,
    pub expected_pose_version: u64,
    pub joint_id: String,
    pub coordinate: f64,
    pub unit: String,
    /// When false, solve the endpoint for interactive feedback and skip swept
    /// collision. A later call with the default true commits from the last
    /// collision-validated pose through the requested target.
    #[serde(default = "default_check_collision")]
    pub check_collision: bool,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CoordinateResult {
    pub snapshot: Snapshot,
    pub requested: f64,
    pub applied: f64,
    pub clamped: bool,
    pub collision: Option<crate::collision::Sweep>,
}
pub(crate) fn state(ws: &mut Workspace) -> Result<&mut State> {
    if ws.motion.is_none() {
        let revision = &ws.stored.revisions[&ws.stored.current];
        ws.motion = Some(if revision.model["rebuild_required"] == true {
            // Open stages source history before an explicit native build. There
            // are no reconstructed bodies to attach saved joints to yet.
            State::new(&crate::Inputs::default(), &json!({"parts":[]}))?
        } else {
            State::new(&revision.inputs, &revision.model)?
        });
    }
    Ok(ws.motion.as_mut().unwrap())
}
/// Swept collision for long interactive commits is applied in driver-bounded
/// chunks so each certified segment stays inside the path/query budgets.
fn sweep_in_chunks(
    mechanism: Mechanism,
    validated: &[f64],
    world: &crate::collision::World,
    joint: &str,
    coordinate: f64,
    unit: &str,
    cancel: &tokio::sync::watch::Receiver<bool>,
) -> Result<(Mechanism, f64, Option<crate::collision::Sweep>, bool)> {
    let start = mechanism.at_coordinates(validated)?;
    let evaluation = start.evaluate()?;
    let current = evaluation
        .joints
        .iter()
        .find(|j| j.id == joint)
        .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown driven joint"))?
        .coordinate;
    let kind = start
        .definition()
        .joints
        .iter()
        .find(|j| j.id == joint)
        .map(|j| &j.kind)
        .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown driven joint"))?;
    let delta = coordinate - current;
    let span = crate::constraints::drive_scale(kind);
    let chunks = if delta == 0. {
        1
    } else {
        ((delta.abs() / span).ceil() as usize).max(1)
    };
    let mut cursor = start;
    let mut queries = 0usize;
    let mut expansion = 0.0_f64;
    let mut enclosure = false;
    let mut linear = false;
    for index in 1..=chunks {
        let waypoint = if index == chunks {
            coordinate
        } else {
            current + delta * (index as f64) / (chunks as f64)
        };
        let mut next = cursor.clone();
        let (target, _) = next.set_cancel(joint, waypoint, unit, Some(cancel))?;
        let sweep = if let Some(trajectory) = next.last_trajectory() {
            world.sweep_trajectory(&cursor, trajectory, Some(cancel))?
        } else {
            world.sweep_cancel(&cursor, joint, target, Some(cancel))?
        };
        queries += sweep.query_count;
        expansion = expansion.max(sweep.curve_expansion_m);
        enclosure |= sweep.trajectory_enclosure_used;
        linear |= sweep.linear_fallback_used;
        let local_from = cursor
            .evaluate()?
            .joints
            .iter()
            .find(|j| j.id == joint)
            .unwrap()
            .coordinate;
        if sweep.blocked {
            let applied = if sweep.fraction == 0. {
                local_from
            } else {
                local_from + (target - local_from) * sweep.fraction
            };
            if let Some(trajectory) = next.last_trajectory() {
                if sweep.fraction < 1. {
                    next = cursor.at_trajectory(trajectory, sweep.fraction)?;
                }
            } else {
                next.set(joint, applied, unit)?;
            }
            next.clear_trajectory();
            let global = ((index - 1) as f64 + sweep.fraction) / chunks as f64;
            return Ok((
                next,
                applied,
                Some(crate::collision::Sweep {
                    trajectory_enclosure_used: enclosure,
                    linear_fallback_used: linear,
                    curve_expansion_m: expansion,
                    query_count: queries,
                    blocked: true,
                    fraction: global.clamp(0., 1.),
                    instances: sweep.instances,
                    accuracy: sweep.accuracy,
                    clearance_m: sweep.clearance_m,
                    backoff_m: sweep.backoff_m,
                }),
                true,
            ));
        }
        next.clear_trajectory();
        cursor = next;
    }
    Ok((
        cursor,
        coordinate,
        Some(crate::collision::Sweep {
            trajectory_enclosure_used: enclosure,
            linear_fallback_used: linear,
            curve_expansion_m: expansion,
            query_count: queries,
            blocked: false,
            fraction: 1.,
            instances: None,
            accuracy: "conservative_proxy".into(),
            clearance_m: world.policy().clearance_m,
            backoff_m: world.policy().backoff_m,
        }),
        true,
    ))
}
pub(crate) fn snapshot(ws: &mut Workspace) -> Result<Snapshot> {
    let workspace_id = ws.stored.id.clone();
    let revision = ws.stored.current.clone();
    let state = state(ws)?;
    Ok(Snapshot {
        workspace_id,
        revision,
        pose_version: state.version,
        mode: if state.simulation.is_some() {
            "simulate"
        } else {
            "manipulate"
        }
        .into(),
        cad_units: "mm".into(),
        simulation_unavailable_reason: crate::dynamics::unavailable(&state.mechanism)
            .map(|(_, reason)| reason.into()),
        collision_checked: state.playback.is_none()
            && state.simulation.is_none()
            && state.collision.is_some()
            && state.mechanism.coordinates() == state.validated_coordinates.as_slice(),
        collision: state.collision.as_ref().map(|world| world.policy()),
        actor: None,
        recording: state.recorder.as_ref().map(|r| r.status()),
        playback: state.playback.as_ref().map(|p| p.selection()),
        simulation: if state.playback.is_none() {
            state
                .simulation
                .as_ref()
                .map(|s| s.snapshot())
                .transpose()?
        } else {
            None
        },
        evaluation: if let Some(playback) = &state.playback {
            playback.evaluation(&state.mechanism)?
        } else if let Some(simulation) = &state.simulation {
            simulation.evaluation(&state.mechanism)?
        } else {
            state.mechanism.evaluate()?
        },
    })
}
impl Core {
    pub fn subscribe_motion(&self) -> tokio::sync::broadcast::Receiver<Snapshot> {
        self.motion_events.subscribe()
    }
    pub async fn get_motion(&self, workspace: &str, revision: &str) -> Result<Snapshot> {
        let mut workspaces = self.workspaces.lock().await;
        let ws = workspaces
            .get_mut(workspace)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        if ws.stored.current != revision {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Motion revision is no longer current",
            ));
        }
        snapshot(ws)
    }
    pub async fn set_coordinate(
        &self,
        actor: &str,
        args: SetCoordinate,
    ) -> Result<CoordinateResult> {
        self.ensure_actor_active(actor)?;
        let check_collision = args.check_collision;
        let (mechanism, validated, world) = {
            let mut workspaces = self.workspaces.lock().await;
            self.ensure_actor_active(actor)?;
            let ws = workspaces
                .get_mut(&args.workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != args.revision {
                return Err(Error::domain(
                    "STALE_ENTITY",
                    "Joint target revision is no longer current",
                ));
            }
            let state = state(ws)?;
            if state.playback.is_some() {
                return Err(Error::domain(
                    "MODE_CONFLICT",
                    "Leave playback before changing a joint",
                ));
            }
            if state.simulation.is_some() {
                return Err(Error::domain(
                    "MODE_CONFLICT",
                    "Simulate mode requires a bounded motor target",
                ));
            }
            if state.version != args.expected_pose_version {
                return Err(Error::domain(
                    "POSE_CONFLICT",
                    "Pose changed; get_motion before sending another target",
                ));
            }
            (
                state.mechanism.clone(),
                state.validated_coordinates.clone(),
                state.collision.clone(),
            )
        };
        let joint = args.joint_id.clone();
        let coordinate = args.coordinate;
        let unit = args.unit.clone();
        let cancel = self.actor_signal(actor);
        let (next, applied, collision, validate) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                if check_collision {
                    if let Some(world) = world {
                        return sweep_in_chunks(
                            mechanism, &validated, &world, &joint, coordinate, &unit, &cancel,
                        );
                    }
                }
                let mut next = mechanism;
                let (target, _) = next.set_pose_cancel(&joint, coordinate, &unit, Some(&cancel))?;
                next.clear_trajectory();
                Ok((next, target, None, world.is_none() || check_collision))
            })
            .await
            .map_err(|_| {
                Error::domain(
                    "COLLISION_QUERY_FAILED",
                    "Motion calculation failed; pose preserved",
                )
            })??;
        let mut workspaces = self.workspaces.lock().await;
        self.ensure_actor_active(actor)?;
        let ws = workspaces
            .get_mut(&args.workspace_id)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        if ws.stored.current != args.revision {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Revision changed during swept motion",
            ));
        }
        let state = state(ws)?;
        if state.version != args.expected_pose_version {
            return Err(Error::domain(
                "POSE_CONFLICT",
                "Pose changed during swept motion; target was not accepted",
            ));
        }
        let version = state
            .version
            .checked_add(1)
            .filter(|version| *version <= 9_007_199_254_740_991)
            .ok_or_else(|| {
                Error::domain(
                    "POSE_CONFLICT",
                    "Pose version exhausted; rebuild the revision",
                )
            })?;
        state.mechanism = next;
        state.version = version;
        if validate {
            state.mark_validated();
        }
        if let Some(recorder) = &mut state.recorder {
            recorder.capture(state.mechanism.evaluate()?, version, None);
        }
        let mut snapshot = snapshot(ws)?;
        snapshot.actor = Some(actor.into());
        let _ = self.motion_events.send(snapshot.clone());
        Ok(CoordinateResult {
            snapshot,
            requested: coordinate,
            applied,
            clamped: applied != coordinate,
            collision,
        })
    }
    /// Add a joint without replacing existing mechanism settings.
    pub async fn add_joint(
        &self,
        workspace: &str,
        base: &str,
        joint: crate::kinematics::Joint,
        actor: &str,
    ) -> Result<Job> {
        let mut definition = {
            let workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get(workspace)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Expected revision no longer current",
                ));
            }
            crate::kinematics::decode(&ws.stored.revisions[base].inputs.mechanism)?
        };
        definition.joints.push(joint);
        let draft = self
            .stage_mechanism(workspace, base, None, definition, actor)
            .await?;
        self.build(workspace, base, &draft.id, actor).await
    }
    pub async fn stage_mechanism(
        &self,
        workspace: &str,
        base: &str,
        draft_id: Option<&str>,
        definition: Definition,
        actor: &str,
    ) -> Result<Draft> {
        definition.validate()?;
        self.ensure_actor_active(actor)?;
        let mut workspaces = self.workspaces.lock().await;
        self.ensure_actor_active(actor)?;
        let ws = workspaces
            .get_mut(workspace)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        if ws.stored.current != base {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Expected revision no longer current",
            ));
        }
        let mut draft = if let Some(draft_id) = draft_id {
            let draft = ws
                .stored
                .drafts
                .get(draft_id)
                .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown draft"))?;
            if draft.actor != actor || draft.base_revision != base {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Draft ownership or base mismatch",
                ));
            }
            draft.clone()
        } else {
            let inputs = ws.stored.revisions[base].inputs.clone();
            Draft {
                id: id(),
                base_revision: base.into(),
                source_hashes: inputs
                    .files
                    .iter()
                    .map(|(path, source)| (path.clone(), hash(source)))
                    .collect(),
                inputs,
                actor: actor.into(),
                external_fingerprint: None,
                python_origin: None,
                restore_revision: None,
            }
        };
        draft.inputs.mechanism = serde_json::to_value(definition)?;
        draft.restore_revision = None;
        crate::portable::validate_inputs(&draft.inputs)?;
        let mut next = ws.stored.clone();
        next.drafts.insert(draft.id.clone(), draft.clone());
        self.persist(&next).await?;
        ws.stored = next;
        Ok(draft)
    }
    pub async fn define_mechanism(&self, actor: &str, args: DefineMechanism) -> Result<Job> {
        let draft = self
            .stage_mechanism(
                &args.workspace_id,
                &args.expected_base_revision,
                args.draft_id.as_deref(),
                args.definition,
                actor,
            )
            .await?;
        self.build(
            &args.workspace_id,
            &args.expected_base_revision,
            &draft.id,
            actor,
        )
        .await
    }
    /// Freeze one authoritative pose for a native operation. Transforms cross
    /// the private worker boundary only after being obtained from this owner.
    pub(crate) async fn query_pose(
        &self,
        workspace: &str,
        revision: &str,
        args: &mut Value,
    ) -> Result<Option<u64>> {
        let motion = self.get_motion(workspace, revision).await?;
        if !args["poses"].is_null() {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Poses are owned by the motion module; use set_joint_coordinate",
            ));
        }
        if args["pose_version"]
            .as_u64()
            .or_else(|| motion.evaluation.joints.is_empty().then_some(0))
            != Some(motion.pose_version)
        {
            return Err(Error::domain(
                "POSE_CONFLICT",
                "Exact mechanism operations require the current pose_version from get_motion",
            ));
        }
        args["poses"] = json!(motion.evaluation.poses);
        args["pose_version"] = json!(motion.pose_version);
        Ok(Some(motion.pose_version))
    }
}

fn exact_default() -> bool {
    true
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckInterference {
    pub workspace_id: String,
    pub revision: String,
    pub pose_version: u64,
    pub pairs: Vec<[String; 2]>,
    #[serde(default = "exact_default")]
    pub exact: bool,
}
impl Core {
    pub async fn check_interference(&self, actor: &str, args: CheckInterference) -> Result<Value> {
        self.ensure_actor_active(actor)?;
        let snapshot = self.get_motion(&args.workspace_id, &args.revision).await?;
        if snapshot.pose_version != args.pose_version {
            return Err(Error::domain(
                "POSE_CONFLICT",
                "Interference check requires the current settled pose",
            ));
        }
        if args.pairs.is_empty() || args.pairs.len() > 64 {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Check 1..64 explicit instance pairs",
            ));
        }
        let mut unique = std::collections::HashSet::new();
        for [a, b] in &args.pairs {
            if a == b
                || !snapshot.evaluation.poses.contains_key(a)
                || !snapshot.evaluation.poses.contains_key(b)
                || !unique.insert(if a < b { (a, b) } else { (b, a) })
            {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "Pairs must be unique distinct published instances",
                ));
            }
        }
        let world = {
            let mut workspaces = self.workspaces.lock().await;
            let ws = workspaces.get_mut(&args.workspace_id).unwrap();
            if ws.stored.current != args.revision {
                return Err(Error::domain(
                    "STALE_ENTITY",
                    "Revision changed during interference check",
                ));
            }
            state(ws)?.collision.clone()
        };
        if world.is_none() && !args.exact {
            return Err(Error::domain(
                "COLLISION_NOT_CONFIGURED",
                "Configure collision proxies or request an exact BRep check",
            ));
        }
        let proxy = if let Some(world) = world {
            let evaluation = snapshot.evaluation;
            let pairs = args.pairs.clone();
            Some(
                tokio::task::spawn_blocking(move || world.report_pairs(&evaluation, &pairs))
                    .await
                    .map_err(|_| Error::domain("COLLISION_QUERY_FAILED", "Proxy query failed"))??,
            )
        } else {
            None
        };
        self.ensure_actor_active(actor)?;
        let exact = if args.exact {
            Some(serde_json::from_value::<crate::geometry::ExactInterference>(self.query_as_actor(actor,&args.workspace_id,&args.revision,"inspect",json!({"query":"interference","pairs":args.pairs,"pose_version":args.pose_version})).await?)?)
        } else {
            None
        };
        self.ensure_actor_active(actor)?;
        if self
            .get_motion(&args.workspace_id, &args.revision)
            .await?
            .pose_version
            != args.pose_version
        {
            return Err(Error::domain(
                "POSE_CONFLICT",
                "Pose changed during interference check; retry when settled",
            ));
        }
        Ok(json!(crate::geometry::InterferenceReply {
            workspace_id: args.workspace_id,
            revision: args.revision,
            pose_version: args.pose_version,
            proxy,
            exact,
        }))
    }
}
