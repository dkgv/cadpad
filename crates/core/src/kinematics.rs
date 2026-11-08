//! Double-precision open-tree kinematics. SI internally; CAD wire poses are mm.
use crate::{Error, Result};
use glam::{DAffine3, DQuat, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

fn schema_version() -> u32 {
    1
}
fn identity() -> [f64; 4] {
    [0., 0., 0., 1.]
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    #[serde(default)]
    pub position_m: [f64; 3],
    #[serde(default = "identity")]
    pub quaternion: [f64; 4],
}
impl Default for Frame {
    fn default() -> Self {
        Self {
            position_m: [0.; 3],
            quaternion: identity(),
        }
    }
}
impl Frame {
    pub(crate) fn transform(&self) -> Result<DAffine3> {
        if self
            .position_m
            .iter()
            .chain(self.quaternion.iter())
            .any(|v| !v.is_finite())
            || (self.quaternion.iter().map(|v| v * v).sum::<f64>() - 1.).abs() > 1e-6
        {
            return Err(invalid(
                "Joint frames require finite metre coordinates and a normalized xyzw quaternion",
            ));
        }
        Ok(DAffine3::from_rotation_translation(
            DQuat::from_array(self.quaternion).normalize(),
            DVec3::from_array(self.position_m),
        ))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum JointKind {
    Fixed,
    Prismatic { axis: [f64; 3] },
    Revolute { axis: [f64; 3] },
    Path { path: crate::paths::Definition },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Joint {
    pub id: String,
    pub parent: Option<String>,
    pub child: String,
    #[serde(default)]
    pub parent_frame: Frame,
    #[serde(default)]
    pub child_frame: Frame,
    pub kind: JointKind,
    #[serde(default)]
    pub initial: f64,
    pub limits: Option<[f64; 2]>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub joints: Vec<Joint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collision: Option<crate::collision::Definition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraints: Option<crate::constraints::Definition>,
}
impl Default for Definition {
    fn default() -> Self {
        Self {
            schema_version: 1,
            joints: vec![],
            collision: None,
            constraints: None,
        }
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_MECHANISM", message)
}
fn identifier(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        Err(invalid("Invalid joint or instance identifier"))
    } else {
        Ok(())
    }
}
impl Definition {
    pub fn validate(&self) -> Result<Vec<usize>> {
        if let Some(constraints) = &self.constraints {
            constraints.validate(self)?;
        }
        if let Some(collision) = &self.collision {
            collision.validate()?;
        }
        if self.schema_version != 1 || self.joints.len() > 128 {
            return Err(invalid(
                "Mechanism requires schema 1 and at most 128 joints",
            ));
        }
        let mut ids = HashSet::new();
        let mut children = HashMap::new();
        for (index, joint) in self.joints.iter().enumerate() {
            identifier(&joint.id)?;
            identifier(&joint.child)?;
            if let Some(parent) = &joint.parent {
                identifier(parent)?;
                if parent == &joint.child {
                    return Err(invalid("A joint cannot parent its own child"));
                }
            }
            if !ids.insert(&joint.id) || children.insert(joint.child.as_str(), index).is_some() {
                return Err(invalid("Joint IDs and child ownership must be unique"));
            }
            joint.parent_frame.transform()?;
            joint.child_frame.transform()?;
            if !joint.initial.is_finite() {
                return Err(invalid("Joint coordinate must be finite"));
            }
            match &joint.kind {
                JointKind::Fixed => {
                    if joint.initial != 0. || joint.limits.is_some() {
                        return Err(invalid(
                            "Fixed joints require zero coordinate and no limits",
                        ));
                    }
                }
                JointKind::Path { path } => {
                    let path = crate::paths::Path::compile(path)?;
                    let [min, max] = joint.limits.ok_or_else(|| {
                        invalid("Path joints require explicit arc-length limits in metres")
                    })?;
                    if !min.is_finite()
                        || !max.is_finite()
                        || min < 0.
                        || max > path.length
                        || min > max
                        || joint.initial < min
                        || joint.initial > max
                    {
                        return Err(invalid(format!(
                            "Path limits and initial coordinate must lie in [0, {}] metres",
                            path.length
                        )));
                    }
                }
                JointKind::Prismatic { axis } | JointKind::Revolute { axis } => {
                    let axis = DVec3::from_array(*axis);
                    if !axis.is_finite() || (axis.length_squared() - 1.).abs() > 1e-9 {
                        return Err(invalid(
                            "Joint axis must be a finite unit vector in the joint frame",
                        ));
                    }
                    let [min, max] = joint.limits.ok_or_else(|| {
                        invalid("Moving joints require explicit limits in metres or radians")
                    })?;
                    if !min.is_finite()
                        || !max.is_finite()
                        || min > max
                        || joint.initial < min
                        || joint.initial > max
                    {
                        return Err(invalid("Joint limits or initial coordinate are invalid"));
                    }
                }
            }
        }
        fn visit(
            index: usize,
            definition: &Definition,
            children: &HashMap<&str, usize>,
            marks: &mut [u8],
            order: &mut Vec<usize>,
        ) -> Result<()> {
            if marks[index] == 2 {
                return Ok(());
            }
            if marks[index] == 1 {
                return Err(invalid(
                    "Base joints must form a spanning tree; represent additional loop joints with hinge_closure residual equations",
                ));
            }
            marks[index] = 1;
            if let Some(parent) = &definition.joints[index].parent
                && let Some(parent) = children.get(parent.as_str())
            {
                visit(*parent, definition, children, marks, order)?;
            }
            marks[index] = 2;
            order.push(index);
            Ok(())
        }
        let mut marks = vec![0; self.joints.len()];
        let mut order = vec![];
        for index in 0..self.joints.len() {
            visit(index, self, &children, &mut marks, &mut order)?;
        }
        Ok(order)
    }
}
pub fn decode(value: &serde_json::Value) -> Result<Definition> {
    let definition: Definition =
        serde_json::from_value(value.clone()).map_err(|e| invalid(e.to_string()))?;
    definition.validate()?;
    Ok(definition)
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pose {
    pub position: [f64; 3],
    pub quaternion: [f64; 4],
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct JointState {
    pub id: String,
    pub parent: Option<String>,
    pub child: String,
    pub kind: String,
    pub coordinate: f64,
    pub unit: String,
    pub limits: Option<[f64; 2]>,
    pub axis_world: [f64; 3],
    pub origin_mm: [f64; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<crate::paths::State>,
    pub drivable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Evaluation {
    pub poses: BTreeMap<String, Pose>,
    pub joints: Vec<JointState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constraints: Option<crate::constraints::Report>,
}
#[derive(Clone)]
pub struct Mechanism {
    definition: Definition,
    order: Vec<usize>,
    roots: BTreeMap<String, DAffine3>,
    coordinates: Vec<f64>,
    report: Option<crate::constraints::Report>,
    trajectory: Option<std::sync::Arc<crate::constraints::Trajectory>>,
    paths: HashMap<usize, crate::paths::Path>,
}
impl Mechanism {
    pub fn new(definition: Definition, parts: &serde_json::Value) -> Result<Self> {
        let order = definition.validate()?;
        let parts = parts
            .as_array()
            .ok_or_else(|| invalid("Model parts are missing"))?;
        let mut roots = BTreeMap::new();
        for part in parts {
            let name = part["instance_id"]
                .as_str()
                .ok_or_else(|| invalid("Model instance ID missing"))?;
            let position: [f64; 3] = serde_json::from_value(part["position"].clone())
                .map_err(|_| invalid("Model instance position missing"))?;
            let position = DVec3::from_array(position) * 0.001;
            if !position.is_finite() {
                return Err(invalid("Non-finite model placement"));
            }
            if roots
                .insert(name.into(), DAffine3::from_translation(position))
                .is_some()
            {
                return Err(invalid("Duplicate model instances"));
            }
        }
        for joint in &definition.joints {
            if !roots.contains_key(&joint.child)
                || joint
                    .parent
                    .as_ref()
                    .is_some_and(|p| !roots.contains_key(p))
            {
                return Err(invalid(format!(
                    "Joint {} refers to an unpublished instance",
                    joint.id
                )));
            }
        }
        let coordinates = definition
            .joints
            .iter()
            .map(|joint| joint.initial)
            .collect();
        let paths = definition
            .joints
            .iter()
            .enumerate()
            .filter_map(|(index, joint)| {
                if let JointKind::Path { path } = &joint.kind {
                    Some(crate::paths::Path::compile(path).map(|path| (index, path)))
                } else {
                    None
                }
            })
            .collect::<Result<HashMap<_, _>>>()?;
        let mut mechanism = Self {
            definition,
            order,
            roots,
            coordinates,
            paths,
            report: None,
            trajectory: None,
        };
        mechanism.evaluate()?;
        if let Some(constraints) = &mechanism.definition.constraints {
            let (q, mut report) = constraints.solve(&mechanism, &mechanism.coordinates, None)?;
            report.path_certified = true;
            mechanism.coordinates = q;
            mechanism.report = Some(report);
        }
        Ok(mechanism)
    }
    pub fn set(&mut self, id: &str, target: f64, unit: &str) -> Result<(f64, bool)> {
        self.set_cancel(id, target, unit, None)
    }
    pub fn set_cancel(
        &mut self,
        id: &str,
        target: f64,
        unit: &str,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Result<(f64, bool)> {
        self.drive(id, target, unit, cancel, true)
    }
    /// Interactive pose update: solve the endpoint only. Skips certified path
    /// continuation used by swept collision. Callers that need a continuous
    /// collision path must use [`Self::set_cancel`].
    pub fn set_pose_cancel(
        &mut self,
        id: &str,
        target: f64,
        unit: &str,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Result<(f64, bool)> {
        self.drive(id, target, unit, cancel, false)
    }
    fn drive(
        &mut self,
        id: &str,
        target: f64,
        unit: &str,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
        certify_path: bool,
    ) -> Result<(f64, bool)> {
        if !target.is_finite() {
            return Err(invalid("Joint target must be finite"));
        }
        let index = self
            .definition
            .joints
            .iter()
            .position(|j| j.id == id)
            .ok_or_else(|| invalid("Unknown joint"))?;
        let joint = &self.definition.joints[index];
        let expected = match &joint.kind {
            JointKind::Fixed => return Err(invalid("Fixed joints cannot be driven")),
            JointKind::Prismatic { .. } | JointKind::Path { .. } => "m",
            JointKind::Revolute { .. } => "rad",
        };
        if unit != expected {
            return Err(invalid(format!("Joint {} requires {expected}", joint.id)));
        }
        let [min, max] = joint.limits.unwrap();
        let coordinate = target.clamp(min, max);
        if let Some(constraints) = &self.definition.constraints {
            if certify_path {
                let trajectory =
                    constraints.trajectory(self, &self.coordinates, index, coordinate, cancel)?;
                self.coordinates = trajectory.nodes.last().unwrap().coordinates.clone();
                self.report = Some(trajectory.report.clone());
                self.trajectory = Some(std::sync::Arc::new(trajectory));
            } else {
                let mut seed = self.coordinates.clone();
                seed[index] = coordinate;
                // Prefer a single endpoint solve for interactive steps. Large
                // jumps walk the branch with successive local solves.
                let (q, report) = match constraints.solve(self, &seed, cancel) {
                    Ok((q, mut report)) => {
                        report.path_certified = false;
                        report.continuation_steps = 0;
                        (q, report)
                    }
                    Err(_) => constraints.continue_endpoint(
                        self,
                        &self.coordinates,
                        index,
                        coordinate,
                        cancel,
                    )?,
                };
                self.coordinates = q;
                self.report = Some(report);
                self.trajectory = None;
                self.evaluate()?;
            }
        } else {
            let previous = self.coordinates[index];
            self.coordinates[index] = coordinate;
            if let Err(error) = self.evaluate() {
                self.coordinates[index] = previous;
                return Err(error);
            }
        }
        let applied = self.coordinates[index];
        Ok((applied, (applied - target).abs() > 0.))
    }
    pub(crate) fn coordinates(&self) -> &[f64] {
        &self.coordinates
    }
    pub(crate) fn last_trajectory(&self) -> Option<&crate::constraints::Trajectory> {
        self.trajectory.as_deref()
    }
    pub(crate) fn clear_trajectory(&mut self) {
        self.trajectory = None;
    }
    pub(crate) fn at_trajectory(
        &self,
        trajectory: &crate::constraints::Trajectory,
        fraction: f64,
    ) -> Result<Self> {
        let mut result = self.at_coordinates(&trajectory.sample(fraction))?;
        if let Some(report) = &mut result.report {
            report.iterations = trajectory.report.iterations;
            report.continuation_steps = trajectory
                .nodes
                .iter()
                .filter(|node| node.fraction < fraction)
                .count();
        }
        Ok(result)
    }
    pub(crate) fn at_coordinates(&self, q: &[f64]) -> Result<Self> {
        let mut result = self.clone();
        result.coordinates = q.to_vec();
        result.trajectory = None;
        if let Some(definition) = &self.definition.constraints {
            let mut report = definition.report(&result, q)?;
            if report.max_normalized_residual > 0.500001 {
                return Err(Error::domain(
                    "CONSTRAINT_PATH_LIMIT",
                    "Interpolated pose exceeds its certified residual budget",
                ));
            }
            report.path_certified = true;
            result.report = Some(report);
        }
        result.evaluate_at(q, false)?;
        Ok(result)
    }
    pub fn definition(&self) -> &Definition {
        &self.definition
    }
    pub fn descendants(&self, id: &str) -> Result<HashSet<String>> {
        let joint = self
            .definition
            .joints
            .iter()
            .find(|joint| joint.id == id)
            .ok_or_else(|| invalid("Unknown joint"))?;
        let mut descendants = HashSet::from([joint.child.clone()]);
        let mut changed = true;
        while changed {
            changed = false;
            for joint in &self.definition.joints {
                if joint
                    .parent
                    .as_ref()
                    .is_some_and(|parent| descendants.contains(parent))
                    && descendants.insert(joint.child.clone())
                {
                    changed = true;
                }
            }
        }
        Ok(descendants)
    }
    pub fn sweep_spans(&self, id: &str, target: f64) -> Result<Vec<crate::paths::Span>> {
        let evaluation = self.evaluate()?;
        let index = self
            .definition
            .joints
            .iter()
            .position(|j| j.id == id)
            .ok_or_else(|| invalid("Unknown driven joint"))?;
        let joint = &self.definition.joints[index];
        let current = self.coordinates[index];
        let delta = target - current;
        if delta == 0. {
            return Ok(vec![]);
        }
        let parent = joint
            .parent
            .as_ref()
            .map(|id| {
                let pose = &evaluation.poses[id];
                DAffine3::from_rotation_translation(
                    DQuat::from_array(pose.quaternion),
                    DVec3::from_array(pose.position) * 0.001,
                )
            })
            .unwrap_or(DAffine3::IDENTITY);
        let frame = parent * joint.parent_frame.transform()?;
        if let Some(path) = self.paths.get(&index) {
            return Ok(path
                .spans(current, target)?
                .into_iter()
                .map(|span| span.world(frame))
                .collect());
        }
        let (axis, rotation) = match &joint.kind {
            JointKind::Prismatic { axis } => (DVec3::from_array(*axis), false),
            JointKind::Revolute { axis } => (DVec3::from_array(*axis), true),
            _ => return Err(invalid("Fixed joints cannot be swept")),
        };
        let steps = if rotation {
            (delta.abs() / std::f64::consts::FRAC_PI_8).ceil().max(1.)
        } else {
            1.
        };
        if steps > 128. {
            return Err(Error::domain(
                "COLLISION_SWEEP_LIMIT",
                "A swept hinge target may span at most eight turns per request",
            ));
        }
        Ok((0..steps as usize)
            .map(|step| {
                let from = step as f64 / steps;
                let to = (step + 1) as f64 / steps;
                crate::paths::Span {
                    from,
                    to,
                    translation_m: if rotation {
                        DVec3::ZERO
                    } else {
                        axis * delta * (to - from)
                    },
                    rotation_vector: if rotation {
                        axis * delta * (to - from)
                    } else {
                        DVec3::ZERO
                    },
                    pivot_m: DVec3::ZERO,
                    path_point_m: if rotation {
                        DVec3::ZERO
                    } else {
                        axis * (current + delta * from)
                    },
                    bend_radius_m: 0.,
                    omitted_roll: 0.,
                }
                .world(frame)
            })
            .collect())
    }
    pub fn evaluate(&self) -> Result<Evaluation> {
        self.evaluate_at(&self.coordinates, true)
    }
    pub(crate) fn evaluate_at(&self, coordinates: &[f64], metadata: bool) -> Result<Evaluation> {
        let mut poses = self.roots.clone();
        let mut joints = vec![];
        for &index in &self.order {
            let joint = &self.definition.joints[index];
            let coordinate = coordinates[index];
            let parent = joint
                .parent
                .as_ref()
                .map(|p| poses[p])
                .unwrap_or(DAffine3::IDENTITY);
            let frame = parent * joint.parent_frame.transform()?;
            let (motion, kind, unit, axis) = match &joint.kind {
                JointKind::Fixed => (DAffine3::IDENTITY, "fixed", "fixed", DVec3::Z),
                JointKind::Prismatic { axis } => {
                    let axis = DVec3::from_array(*axis).normalize();
                    (
                        DAffine3::from_translation(axis * coordinate),
                        "prismatic",
                        "m",
                        axis,
                    )
                }
                JointKind::Path { .. } => {
                    let path = &self.paths[&index];
                    let pose = path.pose(coordinate);
                    (pose, "path", "m", pose.transform_vector3(DVec3::X))
                }
                JointKind::Revolute { axis } => {
                    let axis = DVec3::from_array(*axis).normalize();
                    (
                        DAffine3::from_quat(DQuat::from_axis_angle(axis, coordinate)),
                        "revolute",
                        "rad",
                        axis,
                    )
                }
            };
            let pose = frame * motion * joint.child_frame.transform()?.inverse();
            if !pose.is_finite() || !(frame.translation * 1000.).is_finite() {
                return Err(invalid("Joint solution is not finite"));
            }
            poses.insert(joint.child.clone(), pose);
            joints.push(JointState {
                id: joint.id.clone(),
                parent: joint.parent.clone(),
                child: joint.child.clone(),
                kind: kind.into(),
                coordinate,
                unit: unit.into(),
                limits: joint.limits,
                axis_world: frame.transform_vector3(axis).normalize().to_array(),
                origin_mm: (frame.transform_point3(if kind == "path" {
                    motion.translation
                } else {
                    DVec3::ZERO
                }) * 1000.)
                    .to_array(),
                path: if metadata {
                    self.paths.get(&index).map(|path| path.state(frame))
                } else {
                    None
                },
                drivable: !matches!(joint.kind, JointKind::Fixed)
                    && !self
                        .definition
                        .constraints
                        .as_ref()
                        .is_some_and(|constraints| constraints.is_passive(&joint.id)),
            });
        }
        let poses = poses
            .into_iter()
            .map(|(id, pose)| {
                let (_, rotation, translation) = pose.to_scale_rotation_translation();
                if !(translation * 1000.).is_finite() || !rotation.is_finite() {
                    return Err(invalid(
                        "Joint pose cannot be represented in CAD millimetres",
                    ));
                }
                Ok((
                    id,
                    Pose {
                        position: (translation * 1000.).to_array(),
                        quaternion: rotation.normalize().to_array(),
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(Evaluation {
            poses,
            joints,
            constraints: if metadata { self.report.clone() } else { None },
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Differential {
    pub speed: f64,
    pub acceleration: f64,
    pub angular_speed: f64,
    pub smooth: bool,
}
impl Mechanism {
    /// Bounds derivatives with respect to t on q(t)=a+(b-a)t. Products of rigid
    /// transforms obey |(Rx)''| <= (w²+alpha)|x|+2w|x'|+|x''|.
    pub(crate) fn differential(
        &self,
        body: Option<&str>,
        point: DVec3,
        a: &[f64],
        b: &[f64],
        vector: bool,
    ) -> Result<Differential> {
        let mut result = Differential {
            speed: 0.,
            acceleration: 0.,
            angular_speed: 0.,
            smooth: true,
        };
        let mut radius = if vector { 1. } else { point.length() };
        let mut current = body;
        while let Some(body) = current {
            let Some(index) = self
                .definition
                .joints
                .iter()
                .position(|joint| joint.child == body)
            else {
                break;
            };
            let joint = &self.definition.joints[index];
            if !vector {
                radius += DVec3::from_array(joint.child_frame.position_m).length();
            }
            let ds = (b[index] - a[index]).abs();
            let (w, alpha, position_bound, position_speed, position_acceleration, smooth) =
                match &joint.kind {
                    JointKind::Fixed => (0., 0., 0., 0., 0., true),
                    JointKind::Prismatic { .. } => (
                        0.,
                        0.,
                        if vector {
                            0.
                        } else {
                            a[index].abs().max(b[index].abs())
                        },
                        if vector { 0. } else { ds },
                        0.,
                        true,
                    ),
                    JointKind::Revolute { .. } => (ds, 0., 0., 0., 0., true),
                    JointKind::Path { .. } => {
                        let (bound, w, alpha, acceleration, smooth) =
                            self.paths[&index].differential(a[index], b[index]);
                        (
                            w,
                            alpha,
                            if vector { 0. } else { bound },
                            if vector { 0. } else { ds },
                            if vector { 0. } else { acceleration },
                            smooth,
                        )
                    }
                };
            result.acceleration +=
                (w * w + alpha) * radius + 2. * w * result.speed + position_acceleration;
            result.speed += w * radius + position_speed;
            result.angular_speed += w;
            result.smooth &= smooth;
            radius += position_bound;
            if !vector {
                radius += DVec3::from_array(joint.parent_frame.position_m).length();
            }
            current = joint.parent.as_deref();
        }
        if !result.speed.is_finite() || !result.acceleration.is_finite() {
            return Err(Error::domain(
                "CONSTRAINT_NUMERIC_FAILURE",
                "Trajectory derivative bound is not finite",
            ));
        }
        Ok(result)
    }
}
