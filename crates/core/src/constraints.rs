//! Bounded local residual solves and continuation on the previous assembly branch.
//! The accepted motion is an explicit piecewise joint-linear trajectory whose
//! residual error is bounded throughout every segment, not just at its endpoints.
use crate::{
    Error, Result,
    kinematics::{Definition as MechanismDefinition, Evaluation, Frame, JointKind, Mechanism},
};
use glam::{DQuat, DVec3};
use nalgebra::{DMatrix, DVector, linalg::SVD};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
fn length_tolerance() -> f64 {
    1e-6
}
fn angle_tolerance() -> f64 {
    1e-5
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub instance_id: Option<String>,
    pub point_m: [f64; 3],
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Axis {
    pub instance_id: Option<String>,
    pub direction: [f64; 3],
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub instance_id: Option<String>,
    #[serde(default)]
    pub frame: Frame,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Equation {
    Gear {
        id: String,
        joint_a: String,
        joint_b: String,
        ratio: f64,
        offset: f64,
        unit_a: String,
        unit_b: String,
        /// Total free play in the residual `q_b - ratio q_a - offset`. Zero is a
        /// tight mesh. Simulate mode only: Manipulate still solves the nominal ratio.
        #[serde(default)]
        backlash: f64,
    },
    Coincident {
        id: String,
        a: Point,
        b: Point,
    },
    Distance {
        id: String,
        a: Point,
        b: Point,
        distance_m: f64,
    },
    AlignedAxes {
        id: String,
        a: Axis,
        b: Axis,
    },
    HingeClosure {
        id: String,
        a: Attachment,
        b: Attachment,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub passive_joints: Vec<String>,
    pub equations: Vec<Equation>,
    #[serde(default = "length_tolerance")]
    pub length_tolerance_m: f64,
    #[serde(default = "angle_tolerance")]
    pub angle_tolerance_rad: f64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    pub position_error_m: f64,
    pub axis_error_rad: f64,
    pub coordinate_error_m: f64,
    pub coordinate_error_rad: f64,
    pub max_normalized_residual: f64,
    pub iterations: usize,
    pub continuation_steps: usize,
    pub path_certified: bool,
    pub length_tolerance_m: f64,
    pub angle_tolerance_rad: f64,
}
#[derive(Clone, Debug)]
pub struct Node {
    pub fraction: f64,
    pub coordinates: Vec<f64>,
}
#[derive(Clone, Debug)]
pub struct Trajectory {
    pub nodes: Vec<Node>,
    pub report: Report,
}
impl Trajectory {
    pub fn sample(&self, fraction: f64) -> Vec<f64> {
        let fraction = fraction.clamp(0., 1.);
        if fraction == 0. {
            return self.nodes[0].coordinates.clone();
        }
        if fraction == 1. {
            return self.nodes.last().unwrap().coordinates.clone();
        }
        let index = self
            .nodes
            .partition_point(|node| node.fraction < fraction)
            .max(1)
            .min(self.nodes.len() - 1);
        let a = &self.nodes[index - 1];
        let b = &self.nodes[index];
        let t = ((fraction - a.fraction) / (b.fraction - a.fraction)).clamp(0., 1.);
        a.coordinates
            .iter()
            .zip(&b.coordinates)
            .map(|(a, b)| a + (b - a) * t)
            .collect()
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_CONSTRAINT", message)
}
fn identifier(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(invalid("Invalid constraint or instance identifier"))
    } else {
        Ok(())
    }
}
pub fn unit(kind: &JointKind) -> &'static str {
    match kind {
        JointKind::Fixed => "fixed",
        JointKind::Revolute { .. } => "rad",
        _ => "m",
    }
}
fn scale(kind: &JointKind) -> f64 {
    if unit(kind) == "rad" { 0.1 } else { 0.001 }
}
pub(crate) fn drive_scale(kind: &JointKind) -> f64 {
    scale(kind)
}
fn validate_point(point: &Point) -> Result<()> {
    if let Some(id) = &point.instance_id {
        identifier(id)?;
    }
    if point
        .point_m
        .iter()
        .any(|v| !v.is_finite() || v.abs() > 1e6)
    {
        return Err(invalid(
            "Constraint anchors require finite local metres within 1e6 m",
        ));
    }
    Ok(())
}
fn validate_axis(axis: &Axis) -> Result<()> {
    if let Some(id) = &axis.instance_id {
        identifier(id)?;
    }
    let direction = DVec3::from_array(axis.direction);
    if !direction.is_finite() || (direction.length_squared() - 1.).abs() > 1e-9 {
        return Err(invalid("Constraint axes must be finite unit directions"));
    }
    Ok(())
}
fn attachment(value: &Attachment) -> Result<(Point, Axis)> {
    let frame = value.frame.transform()?;
    Ok((
        Point {
            instance_id: value.instance_id.clone(),
            point_m: frame.translation.to_array(),
        },
        Axis {
            instance_id: value.instance_id.clone(),
            direction: frame.transform_vector3(DVec3::Z).to_array(),
        },
    ))
}
impl Definition {
    pub fn validate(&self, mechanism: &MechanismDefinition) -> Result<()> {
        if self.equations.is_empty() || self.equations.len() > 64 || self.passive_joints.len() > 32
        {
            return Err(invalid("Use 1..64 equations and at most 32 passive joints"));
        }
        if !self.length_tolerance_m.is_finite()
            || !(1e-9..=0.001).contains(&self.length_tolerance_m)
            || !self.angle_tolerance_rad.is_finite()
            || !(1e-9..=0.01).contains(&self.angle_tolerance_rad)
        {
            return Err(invalid(
                "Constraint tolerances must be 1e-9..0.001 m and 1e-9..0.01 rad",
            ));
        }
        let mut passive = HashSet::new();
        for id in &self.passive_joints {
            identifier(id)?;
            let joint = mechanism
                .joints
                .iter()
                .find(|joint| &joint.id == id)
                .ok_or_else(|| invalid("Unknown passive joint"))?;
            if unit(&joint.kind) == "fixed" || !passive.insert(id) {
                return Err(invalid("Passive joints must be unique movable joints"));
            }
        }
        let mut ids = HashSet::new();
        for equation in &self.equations {
            let id = match equation {
                Equation::Gear {
                    id,
                    joint_a,
                    joint_b,
                    ratio,
                    offset,
                    unit_a,
                    unit_b,
                    backlash,
                } => {
                    let a = mechanism
                        .joints
                        .iter()
                        .find(|joint| &joint.id == joint_a)
                        .ok_or_else(|| invalid("Unknown gear coordinate"))?;
                    let b = mechanism
                        .joints
                        .iter()
                        .find(|joint| &joint.id == joint_b)
                        .ok_or_else(|| invalid("Unknown gear coordinate"))?;
                    if joint_a == joint_b
                        || unit(&a.kind) == "fixed"
                        || unit(&b.kind) == "fixed"
                        || unit_a != unit(&a.kind)
                        || unit_b != unit(&b.kind)
                        || !ratio.is_finite()
                        || *ratio == 0.
                        || ratio.abs() > 1e12
                        || !offset.is_finite()
                        || !backlash.is_finite()
                        || *backlash < 0.
                        || *backlash > 1e6
                    {
                        return Err(invalid(
                            "Gear coupling requires distinct movable coordinates, explicit units, finite nonzero ratio, finite offset and non-negative backlash",
                        ));
                    }
                    id
                }
                Equation::Coincident { id, a, b } => {
                    validate_point(a)?;
                    validate_point(b)?;
                    id
                }
                Equation::Distance {
                    id,
                    a,
                    b,
                    distance_m,
                } => {
                    validate_point(a)?;
                    validate_point(b)?;
                    if !distance_m.is_finite() || *distance_m < 1e-9 || *distance_m > 1e6 {
                        return Err(invalid(
                            "Distance constraints require a positive metre length",
                        ));
                    }
                    id
                }
                Equation::AlignedAxes { id, a, b } => {
                    validate_axis(a)?;
                    validate_axis(b)?;
                    id
                }
                Equation::HingeClosure { id, a, b } => {
                    for frame in [a, b] {
                        let (point, axis) = attachment(frame)?;
                        validate_point(&point)?;
                        validate_axis(&axis)?;
                    }
                    id
                }
            };
            identifier(id)?;
            if !ids.insert(id) {
                return Err(invalid("Constraint IDs must be unique"));
            }
        }
        Ok(())
    }
    fn passive(&self, mechanism: &Mechanism) -> Vec<usize> {
        self.passive_joints
            .iter()
            .map(|id| {
                mechanism
                    .definition()
                    .joints
                    .iter()
                    .position(|joint| &joint.id == id)
                    .unwrap()
            })
            .collect()
    }
    pub fn is_passive(&self, id: &str) -> bool {
        self.passive_joints.iter().any(|passive| passive == id)
    }
}
fn point(value: &Point, evaluation: &Evaluation) -> Result<DVec3> {
    let local = DVec3::from_array(value.point_m);
    if let Some(id) = &value.instance_id {
        let pose = evaluation
            .poses
            .get(id)
            .ok_or_else(|| invalid(format!("Missing constraint instance {id}")))?;
        Ok(DQuat::from_array(pose.quaternion) * local + DVec3::from_array(pose.position) * 0.001)
    } else {
        Ok(local)
    }
}
fn axis(value: &Axis, evaluation: &Evaluation) -> Result<DVec3> {
    let local = DVec3::from_array(value.direction).normalize();
    if let Some(id) = &value.instance_id {
        let pose = evaluation
            .poses
            .get(id)
            .ok_or_else(|| invalid(format!("Missing constraint instance {id}")))?;
        Ok(DQuat::from_array(pose.quaternion) * local)
    } else {
        Ok(local)
    }
}
#[derive(Clone)]
enum GroupKind {
    Position(Point, Point),
    Distance(Point, Point),
    Axes(Axis, Axis),
    Coordinate { unit: String },
}
#[derive(Clone)]
struct Group {
    values: Vec<f64>,
    tolerance: f64,
    kind: GroupKind,
}
impl Group {
    fn norm(&self) -> f64 {
        self.values.iter().map(|v| v * v).sum::<f64>().sqrt()
    }
}
impl Definition {
    fn groups(&self, mechanism: &Mechanism, coordinates: &[f64]) -> Result<Vec<Group>> {
        let evaluation = mechanism.evaluate_at(coordinates, false)?;
        self.groups_at(mechanism, coordinates, &evaluation)
    }
    fn groups_at(
        &self,
        mechanism: &Mechanism,
        coordinates: &[f64],
        evaluation: &Evaluation,
    ) -> Result<Vec<Group>> {
        let mut groups = vec![];
        let position = |a: &Point, b: &Point| -> Result<Group> {
            Ok(Group {
                values: (point(a, evaluation)? - point(b, evaluation)?)
                    .to_array()
                    .to_vec(),
                tolerance: self.length_tolerance_m,
                kind: GroupKind::Position(a.clone(), b.clone()),
            })
        };
        let axes = |a: &Axis, b: &Axis| -> Result<Group> {
            Ok(Group {
                values: (axis(a, evaluation)? - axis(b, evaluation)?)
                    .to_array()
                    .to_vec(),
                tolerance: 2. * (self.angle_tolerance_rad * 0.5).sin(),
                kind: GroupKind::Axes(a.clone(), b.clone()),
            })
        };
        for equation in &self.equations {
            match equation {
                Equation::Coincident { a, b, .. } => groups.push(position(a, b)?),
                Equation::AlignedAxes { a, b, .. } => groups.push(axes(a, b)?),
                Equation::HingeClosure { a, b, .. } => {
                    let (pa, aa) = attachment(a)?;
                    let (pb, ab) = attachment(b)?;
                    groups.push(position(&pa, &pb)?);
                    groups.push(axes(&aa, &ab)?);
                }
                Equation::Distance {
                    a, b, distance_m, ..
                } => groups.push(Group {
                    values: vec![
                        point(a, evaluation)?.distance(point(b, evaluation)?) - distance_m,
                    ],
                    tolerance: self.length_tolerance_m,
                    kind: GroupKind::Distance(a.clone(), b.clone()),
                }),
                Equation::Gear {
                    joint_a,
                    joint_b,
                    ratio,
                    offset,
                    unit_b,
                    ..
                } => {
                    let index = |id: &str| {
                        mechanism
                            .definition()
                            .joints
                            .iter()
                            .position(|joint| joint.id == id)
                            .unwrap()
                    };
                    groups.push(Group {
                        values: vec![
                            coordinates[index(joint_b)]
                                - ratio * coordinates[index(joint_a)]
                                - offset,
                        ],
                        tolerance: if unit_b == "rad" {
                            self.angle_tolerance_rad
                        } else {
                            self.length_tolerance_m
                        },
                        kind: GroupKind::Coordinate {
                            unit: unit_b.clone(),
                        },
                    });
                }
            }
        }
        if groups.iter().any(|group| !group.norm().is_finite()) {
            return Err(Error::domain(
                "CONSTRAINT_NUMERIC_FAILURE",
                "Constraint residual is not finite",
            ));
        }
        Ok(groups)
    }
    fn vector(&self, mechanism: &Mechanism, coordinates: &[f64]) -> Result<DVector<f64>> {
        let groups = self.groups(mechanism, coordinates)?;
        Ok(DVector::from_vec(
            groups
                .iter()
                .flat_map(|group| group.values.iter().map(|v| v / group.tolerance))
                .collect(),
        ))
    }
    pub fn report(&self, mechanism: &Mechanism, coordinates: &[f64]) -> Result<Report> {
        self.report_groups(self.groups(mechanism, coordinates)?)
    }
    /// Measure actual physical/recorded poses, without solving or projecting them
    /// back onto the geometric branch. No swept-path certificate is implied.
    pub(crate) fn report_evaluation(
        &self,
        mechanism: &Mechanism,
        evaluation: &Evaluation,
    ) -> Result<Report> {
        let coordinates = mechanism
            .definition()
            .joints
            .iter()
            .map(|joint| {
                evaluation
                    .joints
                    .iter()
                    .find(|state| state.id == joint.id)
                    .map(|state| state.coordinate)
                    .ok_or_else(|| invalid("Missing evaluated joint"))
            })
            .collect::<Result<Vec<_>>>()?;
        self.report_groups(self.groups_at(mechanism, &coordinates, evaluation)?)
    }
    fn report_groups(&self, groups: Vec<Group>) -> Result<Report> {
        let mut report = Report {
            length_tolerance_m: self.length_tolerance_m,
            angle_tolerance_rad: self.angle_tolerance_rad,
            ..Report::default()
        };
        for group in groups {
            let norm = group.norm();
            report.max_normalized_residual =
                report.max_normalized_residual.max(norm / group.tolerance);
            match group.kind {
                GroupKind::Position(..) | GroupKind::Distance(..) => {
                    report.position_error_m = report.position_error_m.max(norm)
                }
                GroupKind::Axes(..) => {
                    report.axis_error_rad =
                        report.axis_error_rad.max(2. * (norm * 0.5).min(1.).asin())
                }
                GroupKind::Coordinate { unit } => {
                    if unit == "rad" {
                        report.coordinate_error_rad = report.coordinate_error_rad.max(norm)
                    } else {
                        report.coordinate_error_m = report.coordinate_error_m.max(norm)
                    }
                }
            }
        }
        Ok(report)
    }
    fn jacobian(
        &self,
        mechanism: &Mechanism,
        q: &[f64],
        variables: &[usize],
        rows: usize,
    ) -> Result<DMatrix<f64>> {
        let mut jacobian = DMatrix::zeros(rows, variables.len());
        for (column, &index) in variables.iter().enumerate() {
            let joint = &mechanism.definition().joints[index];
            let s = scale(&joint.kind);
            let h = s * 1e-5;
            let [min, max] = joint.limits.unwrap();
            let mut a = q.to_vec();
            let mut b = q.to_vec();
            a[index] = (q[index] - h).max(min);
            b[index] = (q[index] + h).min(max);
            let distance = b[index] - a[index];
            if distance <= 1e-15 {
                continue;
            }
            let va = self.vector(mechanism, &a)?;
            let vb = self.vector(mechanism, &b)?;
            jacobian.set_column(column, &((vb - va) * (s / distance)));
        }
        Ok(jacobian)
    }
    pub fn solve(
        &self,
        mechanism: &Mechanism,
        seed: &[f64],
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Result<(Vec<f64>, Report)> {
        let variables = self.passive(mechanism);
        let mut q = seed.to_vec();
        for iteration in 0..64 {
            cancelled(cancel)?;
            let residual = self.vector(mechanism, &q)?;
            let report = self.report(mechanism, &q)?;
            if variables.is_empty() {
                if report.max_normalized_residual <= 0.05 {
                    return Ok((q, report));
                }
                return Err(Error::domain(
                    "INFEASIBLE_MECHANISM",
                    "No passive coordinate can satisfy the requested constraints",
                ));
            }
            let jacobian = self.jacobian(mechanism, &q, &variables, residual.len())?;
            let svd = SVD::try_new(jacobian, true, true, 1e-12, 256).ok_or_else(|| {
                Error::domain(
                    "CONSTRAINT_NUMERIC_FAILURE",
                    "Residual Jacobian decomposition did not converge",
                )
            })?;
            let maximum = svd.singular_values.iter().copied().fold(0., f64::max);
            let threshold = (maximum * 1e-8).max(1e-10);
            if svd.rank(threshold) < variables.len() {
                return Err(Error::domain(
                    "SINGULAR_MECHANISM",
                    "Passive coordinates are underdetermined or the assembly branch is singular",
                ));
            }
            if report.max_normalized_residual <= 0.05 {
                let mut report = report;
                report.iterations = iteration;
                return Ok((q, report));
            }
            let mut step = svd.solve(&(-&residual), threshold).map_err(|_| {
                Error::domain(
                    "CONSTRAINT_NUMERIC_FAILURE",
                    "Constraint step could not be solved",
                )
            })?;
            let maximum = step.iter().map(|v| v.abs()).fold(1., f64::max);
            step /= maximum;
            let current_cost = residual.norm_squared();
            let mut accepted = None;
            for line_search in 0..16 {
                let factor = 0.5_f64.powi(line_search);
                let mut candidate = q.clone();
                for (column, &index) in variables.iter().enumerate() {
                    let joint = &mechanism.definition().joints[index];
                    let [min, max] = joint.limits.unwrap();
                    candidate[index] =
                        (q[index] + factor * step[column] * scale(&joint.kind)).clamp(min, max);
                }
                let cost = self.vector(mechanism, &candidate)?.norm_squared();
                if cost < current_cost * (1. - 1e-10) {
                    accepted = Some(candidate);
                    break;
                }
            }
            q = accepted.ok_or_else(|| {
                Error::domain(
                    "INFEASIBLE_MECHANISM",
                    "Constraint solve cannot improve within the joint limits on this branch",
                )
            })?;
        }
        Err(Error::domain(
            "INFEASIBLE_MECHANISM",
            "Constraint iteration budget exhausted; previous pose is preserved",
        ))
    }
}
fn cancelled(cancel: Option<&tokio::sync::watch::Receiver<bool>>) -> Result<()> {
    if cancel.is_some_and(|cancel| *cancel.borrow()) {
        Err(Error::domain(
            "CANCELLED",
            "Constraint solve cancelled; pose preserved",
        ))
    } else {
        Ok(())
    }
}
/// Maximum of the endpoint interpolation plus M*t*(1-t)/2.
fn interpolated_bound(a: f64, b: f64, second: f64) -> f64 {
    let k = second * 0.5;
    let mut maximum = a.max(b);
    if k > 0. {
        let t = ((b - a + k) / (2. * k)).clamp(0., 1.);
        maximum = maximum.max(a + (b - a + k) * t - k * t * t);
    }
    maximum
}
impl Definition {
    fn certify(&self, mechanism: &Mechanism, a: &[f64], b: &[f64]) -> Result<bool> {
        let ga = self.groups(mechanism, a)?;
        let gb = self.groups(mechanism, b)?;
        let eval_a = mechanism.evaluate_at(a, false)?;
        let eval_b = mechanism.evaluate_at(b, false)?;
        for (a_group, b_group) in ga.iter().zip(&gb) {
            let (speed, second, smooth) = match &a_group.kind {
                GroupKind::Coordinate { .. } => (0., 0., true),
                GroupKind::Position(a_ref, b_ref) | GroupKind::Distance(a_ref, b_ref) => {
                    let da = mechanism.differential(
                        a_ref.instance_id.as_deref(),
                        DVec3::from_array(a_ref.point_m),
                        a,
                        b,
                        false,
                    )?;
                    let db = mechanism.differential(
                        b_ref.instance_id.as_deref(),
                        DVec3::from_array(b_ref.point_m),
                        a,
                        b,
                        false,
                    )?;
                    let speed = da.speed + db.speed;
                    let mut second = da.acceleration + db.acceleration;
                    if matches!(a_group.kind, GroupKind::Distance(..)) {
                        let start = point(a_ref, &eval_a)?.distance(point(b_ref, &eval_a)?);
                        let end = point(a_ref, &eval_b)?.distance(point(b_ref, &eval_b)?);
                        let lower = start.min(end) - speed * 0.5;
                        if lower <= 0. {
                            return Ok(false);
                        }
                        second += speed * speed / lower;
                    }
                    (speed, second, da.smooth && db.smooth)
                }
                GroupKind::Axes(a_ref, b_ref) => {
                    let da = mechanism.differential(
                        a_ref.instance_id.as_deref(),
                        DVec3::from_array(a_ref.direction),
                        a,
                        b,
                        true,
                    )?;
                    let db = mechanism.differential(
                        b_ref.instance_id.as_deref(),
                        DVec3::from_array(b_ref.direction),
                        a,
                        b,
                        true,
                    )?;
                    (
                        da.speed + db.speed,
                        da.acceleration + db.acceleration,
                        da.smooth && db.smooth,
                    )
                }
            };
            let bound = if smooth {
                interpolated_bound(a_group.norm(), b_group.norm(), second)
            } else {
                a_group
                    .norm()
                    .max(b_group.norm())
                    .max((a_group.norm() + b_group.norm() + speed) * 0.5)
            };
            if !bound.is_finite() || bound > a_group.tolerance * 0.5 {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// Walk the assembly branch toward an endpoint with successive local solves.
    /// Used for interactive previews; does not certify the intervening path.
    /// If the exact target is unreachable on this branch, returns the farthest
    /// feasible pose instead of failing the whole interaction.
    pub fn continue_endpoint(
        &self,
        mechanism: &Mechanism,
        start: &[f64],
        driver: usize,
        target: f64,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Result<(Vec<f64>, Report)> {
        if self.is_passive(&mechanism.definition().joints[driver].id) {
            return Err(invalid(
                "Passive coordinates are solved by constraints; choose an independent driver",
            ));
        }
        let joint = &mechanism.definition().joints[driver];
        let step = scale(&joint.kind);
        let mut q = start.to_vec();
        let origin = q[driver];
        let delta = target - origin;
        if delta == 0. {
            let mut report = self.report(mechanism, &q)?;
            report.path_certified = false;
            return Ok((q, report));
        }
        let chunks = ((delta.abs() / step).ceil() as usize).max(1);
        let mut iterations = 0usize;
        let mut steps = 0usize;
        for i in 1..=chunks {
            cancelled(cancel)?;
            let waypoint = if i == chunks {
                target
            } else {
                origin + delta * (i as f64) / (chunks as f64)
            };
            match self.step_endpoint(mechanism, &q, driver, waypoint, cancel)? {
                Some((next, used)) => {
                    q = next;
                    iterations += used;
                    steps += 1;
                }
                None => break,
            }
        }
        let mut report = self.report(mechanism, &q)?;
        report.iterations = iterations;
        report.continuation_steps = steps;
        report.path_certified = false;
        Ok((q, report))
    }
    fn step_endpoint(
        &self,
        mechanism: &Mechanism,
        start: &[f64],
        driver: usize,
        target: f64,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Result<Option<(Vec<f64>, usize)>> {
        let mut seed = start.to_vec();
        seed[driver] = target;
        match self.solve(mechanism, &seed, cancel) {
            Ok((q, report)) => return Ok(Some((q, report.iterations))),
            Err(error)
                if error.code() != "INFEASIBLE_MECHANISM"
                    && error.code() != "SINGULAR_MECHANISM" =>
            {
                return Err(error);
            }
            Err(_) => {}
        }
        let joint = &mechanism.definition().joints[driver];
        let tol = scale(&joint.kind) * 1e-3;
        let mut lo = start.to_vec();
        let mut hi = target;
        let mut best = None;
        let mut iterations = 0usize;
        for _ in 0..24 {
            cancelled(cancel)?;
            let mid = (lo[driver] + hi) * 0.5;
            if (hi - lo[driver]).abs() <= tol {
                break;
            }
            let mut seed = lo.clone();
            seed[driver] = mid;
            match self.solve(mechanism, &seed, cancel) {
                Ok((q, report)) => {
                    iterations += report.iterations;
                    best = Some(q.clone());
                    lo = q;
                }
                Err(error)
                    if error.code() == "INFEASIBLE_MECHANISM"
                        || error.code() == "SINGULAR_MECHANISM" =>
                {
                    hi = mid;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(best.map(|q| (q, iterations)))
    }
    pub fn trajectory(
        &self,
        mechanism: &Mechanism,
        start: &[f64],
        driver: usize,
        target: f64,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Result<Trajectory> {
        if self.is_passive(&mechanism.definition().joints[driver].id) {
            return Err(invalid(
                "Passive coordinates are solved by constraints; choose an independent driver",
            ));
        }
        let delta = target - start[driver];
        let mut trajectory = Trajectory {
            nodes: vec![Node {
                fraction: 0.,
                coordinates: start.to_vec(),
            }],
            report: self.report(mechanism, start)?,
        };
        if delta == 0. {
            trajectory.nodes.push(Node {
                fraction: 1.,
                coordinates: start.to_vec(),
            });
            trajectory.report.path_certified = true;
            return Ok(trajectory);
        }
        // Certify in driver-bounded segments so long interactive commits stay
        // inside the recursion/node budget instead of failing the whole move.
        let joint = &mechanism.definition().joints[driver];
        let span = scale(&joint.kind);
        let segments = ((delta.abs() / span).ceil() as usize).max(1);
        let mut q = start.to_vec();
        let mut iterations = 0usize;
        for segment in 1..=segments {
            cancelled(cancel)?;
            let local_target = if segment == segments {
                target
            } else {
                start[driver] + delta * (segment as f64) / (segments as f64)
            };
            let local_fraction = (segment as f64) / (segments as f64);
            let mut piece = Trajectory {
                nodes: vec![Node {
                    fraction: trajectory.nodes.last().unwrap().fraction,
                    coordinates: q.clone(),
                }],
                report: Report::default(),
            };
            advance(
                self,
                mechanism,
                &mut piece,
                driver,
                (local_target, local_fraction),
                0,
                cancel,
            )?;
            iterations += piece.report.iterations;
            for node in piece.nodes.into_iter().skip(1) {
                if trajectory.nodes.len() >= 8192 {
                    return Err(Error::domain(
                        "CONSTRAINT_PATH_LIMIT",
                        "Continuation exceeded its bounded branch/path budget",
                    ));
                }
                q = node.coordinates.clone();
                trajectory.nodes.push(node);
            }
        }
        let mut report = self.report(mechanism, &trajectory.nodes.last().unwrap().coordinates)?;
        report.iterations = iterations;
        report.continuation_steps = trajectory.nodes.len() - 1;
        report.path_certified = true;
        trajectory.report = report;
        Ok(trajectory)
    }
}
fn advance(
    definition: &Definition,
    mechanism: &Mechanism,
    trajectory: &mut Trajectory,
    driver: usize,
    destination: (f64, f64),
    depth: usize,
    cancel: Option<&tokio::sync::watch::Receiver<bool>>,
) -> Result<()> {
    let (target, fraction) = destination;
    cancelled(cancel)?;
    if depth > 24 || trajectory.nodes.len() > 2048 {
        return Err(Error::domain(
            "CONSTRAINT_PATH_LIMIT",
            "Continuation exceeded its bounded branch/path budget",
        ));
    }
    let previous = trajectory.nodes.last().unwrap().clone();
    let joint = &mechanism.definition().joints[driver];
    let delta = target - previous.coordinates[driver];
    let mut seed = previous.coordinates.clone();
    seed[driver] = target;
    let solve = if delta.abs() > scale(&joint.kind) {
        None
    } else {
        Some(definition.solve(mechanism, &seed, cancel))
    };
    if let Some(Ok((coordinates, report))) = &solve {
        let near = definition.passive(mechanism).iter().all(|&i| {
            (coordinates[i] - previous.coordinates[i]).abs()
                <= scale(&mechanism.definition().joints[i].kind) * 2.5
        });
        if near && definition.certify(mechanism, &previous.coordinates, coordinates)? {
            trajectory.report.iterations += report.iterations;
            trajectory.nodes.push(Node {
                fraction,
                coordinates: coordinates.clone(),
            });
            return Ok(());
        }
    }
    if delta.abs() < scale(&joint.kind) * 1e-6 {
        return Err(solve.and_then(Result::err).unwrap_or_else(|| {
            Error::domain(
                "CONSTRAINT_PATH_LIMIT",
                "Could not certify a continuous path on the previous assembly branch",
            )
        }));
    }
    let middle = (previous.coordinates[driver] + target) * 0.5;
    let middle_fraction = (previous.fraction + fraction) * 0.5;
    advance(
        definition,
        mechanism,
        trajectory,
        driver,
        (middle, middle_fraction),
        depth + 1,
        cancel,
    )?;
    advance(
        definition,
        mechanism,
        trajectory,
        driver,
        (target, fraction),
        depth + 1,
        cancel,
    )
}

#[cfg(test)]
mod interactive_pose_tests {
    use serde_json::json;
    use std::time::{Duration, Instant};
    #[test]
    fn small_interactive_pose_steps_avoid_certified_path_work() {
        let mut def = crate::kinematics::decode(
            &serde_json::from_str::<serde_json::Value>(include_str!(
                "../../../fixtures/four_bar.mechanism.json"
            ))
            .unwrap(),
        )
        .unwrap();
        def.collision = None;
        let parts = json!([
            {"instance_id":"ground","position":[0,0,0]},
            {"instance_id":"crank","position":[0,0,0]},
            {"instance_id":"coupler","position":[0,0,0]},
            {"instance_id":"rocker","position":[0,0,0]},
            {"instance_id":"stop","position":[88,33,1]}
        ]);
        let mut mechanism = crate::kinematics::Mechanism::new(def, &parts).unwrap();
        mechanism
            .set_pose_cancel("crank", 0.9, "rad", None)
            .unwrap();
        let mut total = Duration::ZERO;
        for i in 0..40 {
            let target = 1.0 + (i as f64) * 0.05;
            let started = Instant::now();
            mechanism
                .set_pose_cancel("crank", target, "rad", None)
                .unwrap();
            total += started.elapsed();
            assert!(mechanism.last_trajectory().is_none());
            assert!(
                !mechanism
                    .evaluate()
                    .unwrap()
                    .constraints
                    .unwrap()
                    .path_certified
            );
        }
        assert!(
            total < Duration::from_millis(40),
            "interactive steps took {total:?}"
        );
    }
    #[test]
    fn quick_jumps_return_farthest_feasible_pose_instead_of_hard_failing() {
        let mut def = crate::kinematics::decode(
            &serde_json::from_str::<serde_json::Value>(include_str!(
                "../../../fixtures/four_bar.mechanism.json"
            ))
            .unwrap(),
        )
        .unwrap();
        def.collision = None;
        let parts = json!([
            {"instance_id":"ground","position":[0,0,0]},
            {"instance_id":"crank","position":[0,0,0]},
            {"instance_id":"coupler","position":[0,0,0]},
            {"instance_id":"rocker","position":[0,0,0]},
            {"instance_id":"stop","position":[88,33,1]}
        ]);
        let mut mechanism = crate::kinematics::Mechanism::new(def, &parts).unwrap();
        for target in [1.5_f64, 3.0, -2.5, 6.0, -3.0, 7.0, 0.0, 4.2] {
            let from = mechanism.coordinates()[0];
            let (applied, _) = mechanism
                .set_pose_cancel("crank", target, "rad", None)
                .unwrap();
            assert!(
                (mechanism.coordinates()[0] - applied).abs() < 1e-12,
                "{from} -> {target}"
            );
            assert!(applied.is_finite());
            // Progress should never reverse away from the request past the start.
            assert!(
                (applied - target).abs() <= (from - target).abs() + 1e-9,
                "{from} -> {target} applied {applied}"
            );
        }
    }
    #[test]
    fn long_certified_trajectories_segment_instead_of_hitting_path_budget() {
        let mut def = crate::kinematics::decode(
            &serde_json::from_str::<serde_json::Value>(include_str!(
                "../../../fixtures/four_bar.mechanism.json"
            ))
            .unwrap(),
        )
        .unwrap();
        def.collision = None;
        let parts = json!([
            {"instance_id":"ground","position":[0,0,0]},
            {"instance_id":"crank","position":[0,0,0]},
            {"instance_id":"coupler","position":[0,0,0]},
            {"instance_id":"rocker","position":[0,0,0]},
            {"instance_id":"stop","position":[88,33,1]}
        ]);
        let mechanism = crate::kinematics::Mechanism::new(def, &parts).unwrap();
        let constraints = mechanism.definition().constraints.as_ref().unwrap();
        let start = mechanism.coordinates().to_vec();
        for target in [-3.0_f64, 6.5, 7.0] {
            let trajectory = constraints
                .trajectory(&mechanism, &start, 0, target, None)
                .unwrap();
            assert!(trajectory.nodes.len() > 2, "{target}");
            assert!(trajectory.report.path_certified);
        }
        let (q, report) = constraints
            .continue_endpoint(&mechanism, &start, 0, 7.0, None)
            .unwrap();
        assert!((q[0] - 7.0).abs() < 1e-12);
        assert!(!report.path_certified);
    }
}

#[cfg(test)]
mod physical_report_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn physical_residual_report_uses_actual_poses_without_projecting_to_design() {
        let definition=crate::kinematics::decode(&json!({"constraints":{"passive_joints":[],"equations":[{"type":"coincident","id":"point","a":{"instance_id":null,"point_m":[0,0,0]},"b":{"instance_id":"body","point_m":[0,0,0]}},{"type":"aligned_axes","id":"axis","a":{"instance_id":null,"direction":[0,0,1]},"b":{"instance_id":"body","direction":[0,0,1]}}]}})).unwrap();
        let mechanism = Mechanism::new(
            definition,
            &json!([{"instance_id":"body","position":[0,0,0]}]),
        )
        .unwrap();
        let mut evaluation = mechanism.evaluate().unwrap();
        let pose = evaluation.poses.get_mut("body").unwrap();
        pose.position = [2., 0., 0.];
        pose.quaternion = DQuat::from_rotation_x(0.1).to_array();
        let constraints = mechanism.definition().constraints.as_ref().unwrap();
        let report = constraints
            .report_evaluation(&mechanism, &evaluation)
            .unwrap();
        assert!((report.position_error_m - 0.002).abs() < 1e-14);
        assert!((report.axis_error_rad - 0.1).abs() < 1e-14);
        assert!(
            (report.max_normalized_residual - 0.05_f64.sin() / (0.5e-5_f64).sin()).abs() < 1e-8
        );
        assert!(!report.path_certified);
        let design = constraints.report(&mechanism, &[]).unwrap();
        assert_eq!(design.position_error_m, 0.);
        assert_eq!(design.axis_error_rad, 0.);
        assert_eq!(evaluation.poses["body"].position, [2., 0., 0.]);
    }
}
