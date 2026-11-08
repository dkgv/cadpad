//! Fixed-step f64 dynamics over verified conservative collision proxies.
//! Numerical simulation poses never constitute dimensional or load verification.
use crate::{
    Error, Result, collision,
    kinematics::{self, JointKind, Mechanism},
};
use glam::{DQuat, DVec3};
use rapier3d_f64::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const TIMESTEP_S: f64 = 1. / 120.;
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mass {
    Kilograms { mass_kg: f64 },
    Density { density_kg_m3: f64 },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Body {
    Fixed {
        friction: f64,
        restitution: f64,
    },
    Dynamic {
        mass: Mass,
        friction: f64,
        restitution: f64,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub gravity_m_s2: [f64; 3],
    pub bodies: BTreeMap<String, Body>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Motor {
    pub joint_id: String,
    pub target: f64,
    pub unit: String,
    pub stiffness: f64,
    pub damping: f64,
    /// Force in N for prismatic joints; torque in N m for revolute joints.
    pub max_force: f64,
}
/// Persistent world-space load. Force acts at the body's center of mass;
/// torque is an independent couple, not an application-point offset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Load {
    pub force_n: [f64; 3],
    pub torque_nm: [f64; 3],
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Frame {
    pub tick: u64,
    pub time_s: f64,
    pub poses: BTreeMap<String, kinematics::Pose>,
    pub linear_velocity_m_s: BTreeMap<String, [f64; 3]>,
    pub angular_velocity_rad_s: BTreeMap<String, [f64; 3]>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MotorImpulse {
    /// Last internal solver-substep impulse; CCD can shorten the nominal substep.
    pub impulse: f64,
    pub impulse_unit: String,
    pub max_force: f64,
    pub force_unit: String,
    pub target_position: f64,
    pub coordinate_unit: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SolverSettings {
    pub solver_substeps: usize,
    pub internal_pgs_iterations: usize,
    pub internal_stabilization_iterations: usize,
    pub nominal_solver_substep_s: f64,
    pub contact_frequency_hz: f64,
    pub static_contact_frequency_hz: f64,
    pub contact_damping_ratio: f64,
    pub static_contact_damping_ratio: f64,
    pub contact_recycle_distance_m: f64,
    pub warmstart_joints: bool,
    pub max_ccd_substeps: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Snapshot {
    pub external_loads: BTreeMap<String, Load>,
    pub solver_settings: SolverSettings,
    pub motor_impulses: BTreeMap<String, MotorImpulse>,
    pub timestep_s: f64,
    pub accuracy: String,
    pub mass_kg: BTreeMap<String, f64>,
    pub previous: Frame,
    pub current: Frame,
}
#[derive(Clone)]
struct Drive {
    handle: ImpulseJointHandle,
    axis: JointAxis,
    limits: [f64; 2],
    unit: &'static str,
    active: bool,
}
/// One revolute or prismatic coordinate used by a gear equation.
#[derive(Clone)]
struct GearDof {
    child: RigidBodyHandle,
    parent: RigidBodyHandle,
    parent_frame: rapier3d_f64::math::Pose,
    child_frame: rapier3d_f64::math::Pose,
    /// Unit axis in the parent-frame coordinates of the joint definition.
    axis: DVec3,
    revolute: bool,
}
/// `coordinate_b - ratio * coordinate_a - offset = 0`, with each coordinate in its joint unit.
#[derive(Clone)]
struct Gear {
    a: GearDof,
    b: GearDof,
    ratio: f64,
    offset: f64,
    /// Total free play in the residual. Tooth contact engages outside ±backlash/2.
    backlash: f64,
}
#[derive(Clone)]
struct PathMotor {
    target: f64,
    stiffness: f64,
    damping: f64,
    max_force: f64,
}
/// Arc-length coordinate of a prescribed path. Rapier does not have this joint,
/// so the reduced mass and frame inertia are integrated explicitly.
#[derive(Clone)]
struct PathTrack {
    id: String,
    child: RigidBodyHandle,
    parent: RigidBodyHandle,
    parent_frame: rapier3d_f64::math::Pose,
    child_frame: rapier3d_f64::math::Pose,
    path: crate::paths::Path,
    limits: [f64; 2],
    coordinate: f64,
    speed: f64,
    drivable: bool,
    motor: Option<PathMotor>,
}
struct DofState {
    coordinate: f64,
    velocity: f64,
    axis: DVec3,
    anchor: DVec3,
}
pub struct Simulation {
    world: PhysicsWorld,
    handles: BTreeMap<String, RigidBodyHandle>,
    drives: BTreeMap<String, Drive>,
    gears: Vec<Gear>,
    paths: Vec<PathTrack>,
    tick: u64,
    previous: Frame,
    mass_kg: BTreeMap<String, f64>,
    external_loads: BTreeMap<String, Load>,
}
// Pipeline and CCD scratch buffers are recreated, while all integration state,
// contacts and warmstart impulses are retained for transactional core stepping.
impl Clone for Simulation {
    fn clone(&self) -> Self {
        let w = &self.world;
        let world = PhysicsWorld {
            gravity: w.gravity,
            integration_parameters: w.integration_parameters,
            islands: w.islands.clone(),
            broad_phase: w.broad_phase.clone(),
            narrow_phase: w.narrow_phase.clone(),
            bodies: w.bodies.clone(),
            colliders: w.colliders.clone(),
            impulse_joints: w.impulse_joints.clone(),
            multibody_joints: w.multibody_joints.clone(),
            soft_bodies: w.soft_bodies.clone(),
            ..PhysicsWorld::default()
        };
        Self {
            world,
            handles: self.handles.clone(),
            drives: self.drives.clone(),
            gears: self.gears.clone(),
            paths: self.paths.clone(),
            tick: self.tick,
            previous: self.previous.clone(),
            mass_kg: self.mass_kg.clone(),
            external_loads: self.external_loads.clone(),
        }
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_SIMULATION", message)
}
fn frame(value: &kinematics::Frame) -> Result<rapier3d_f64::math::Pose> {
    let transform = value.transform()?;
    Ok(rapier3d_f64::math::Pose {
        translation: transform.translation,
        rotation: DQuat::from_affine3(&transform).normalize(),
    })
}
fn pose(value: &kinematics::Pose) -> rapier3d_f64::math::Pose {
    rapier3d_f64::math::Pose {
        translation: DVec3::from_array(value.position) * 0.001,
        rotation: DQuat::from_array(value.quaternion),
    }
}
fn gear_dof(
    mechanism: &Mechanism,
    id: &str,
    handles: &BTreeMap<String, RigidBodyHandle>,
    ground: RigidBodyHandle,
) -> Result<GearDof> {
    let joint = mechanism
        .definition()
        .joints
        .iter()
        .find(|joint| joint.id == id)
        .ok_or_else(|| invalid(format!("Unknown gear coordinate {id}")))?;
    let (axis, revolute) = match &joint.kind {
        JointKind::Revolute { axis } => (DVec3::from_array(*axis), true),
        JointKind::Prismatic { axis } => (DVec3::from_array(*axis), false),
        JointKind::Fixed | JointKind::Path { .. } => {
            return Err(invalid(
                "Gear couplings require revolute or prismatic coordinates",
            ));
        }
    };
    if axis.length_squared() < 1e-24 || !axis.is_finite() {
        return Err(invalid(format!(
            "Gear coordinate {id} has an undefined axis"
        )));
    }
    let parent = match &joint.parent {
        Some(name) => *handles
            .get(name)
            .ok_or_else(|| invalid(format!("Unknown gear body {name}")))?,
        None => ground,
    };
    let child = *handles
        .get(&joint.child)
        .ok_or_else(|| invalid(format!("Unknown gear body {}", joint.child)))?;
    Ok(GearDof {
        child,
        parent,
        parent_frame: frame(&joint.parent_frame)?,
        child_frame: frame(&joint.child_frame)?,
        axis: axis.normalize(),
        revolute,
    })
}
/// Inverse inertia of one body along a free joint axis.
///
/// Revolute axes use the parallel-axis term about the hinge so an offset center
/// of mass contributes bearing force. Prismatic axes keep the translational mass
/// along the free direction; the prismatic joint carries any moment from offset.
fn body_inverse_inertia(body: &RigidBody, revolute: bool, axis: DVec3, anchor: DVec3) -> f64 {
    if !body.is_dynamic() {
        return 0.;
    }
    let props = body.mass_properties();
    if revolute {
        let r = body.center_of_mass() - anchor;
        let r_perp = r - axis * r.dot(axis);
        let inertia = axis.dot(props.effective_angular_inertia() * axis)
            + body.mass() * r_perp.length_squared();
        if inertia < 1e-16 {
            0.
        } else {
            1. / inertia
        }
    } else {
        let inv = props.effective_inv_mass;
        axis.x * axis.x * inv.x + axis.y * axis.y * inv.y + axis.z * axis.z * inv.z
    }
}
fn dof_anchor_axis(world: &PhysicsWorld, dof: &GearDof) -> (DVec3, DVec3) {
    let parent = world.bodies[dof.parent].position();
    let anchor = parent * dof.parent_frame;
    (
        anchor.translation,
        (anchor.rotation * dof.axis).normalize_or_zero(),
    )
}
fn dof_inverse_inertia(world: &PhysicsWorld, dof: &GearDof) -> f64 {
    let (anchor, axis) = dof_anchor_axis(world, dof);
    body_inverse_inertia(&world.bodies[dof.child], dof.revolute, axis, anchor)
        + body_inverse_inertia(&world.bodies[dof.parent], dof.revolute, axis, anchor)
}
pub(crate) fn unavailable(mechanism: &Mechanism) -> Option<(&'static str, &'static str)> {
    if mechanism.definition().joints.iter().any(|joint| {
        matches!(joint.kind, JointKind::Revolute { .. })
            && joint
                .limits
                .is_some_and(|[lo, hi]| lo < -std::f64::consts::PI || hi > std::f64::consts::PI)
    }) {
        return Some((
            "INVALID_SIMULATION",
            "Simulation needs hinge limits between −π and π radians",
        ));
    }
    None
}
impl Simulation {
    /// Signed normal contact impulse applied to receiver by pusher, sampled
    /// after the last solver substep. This is not an outer-tick average force
    /// or an integral over all solver substeps (tangential friction excluded).
    pub(crate) fn contact_impulse(&self, pusher: &str, receiver: &str) -> Result<[f64; 3]> {
        let pusher = self
            .handles
            .get(pusher)
            .ok_or_else(|| invalid("Unknown pusher"))?;
        let receiver = self
            .handles
            .get(receiver)
            .ok_or_else(|| invalid("Unknown receiver"))?;
        let mut impulse = DVec3::ZERO;
        for pair in self.world.narrow_phase.contact_pairs() {
            let a = self.world.colliders[pair.collider1].parent();
            let b = self.world.colliders[pair.collider2].parent();
            // Pinned Rapier solver uses force_dir1 = -manifold.normal:
            // total_impulse() follows the impulse applied to collider2.
            if a == Some(*receiver) && b == Some(*pusher) {
                impulse -= pair.total_impulse();
            } else if b == Some(*receiver) && a == Some(*pusher) {
                impulse += pair.total_impulse();
            }
        }
        if !impulse.is_finite() {
            return Err(Error::domain(
                "SIMULATION_NUMERIC_FAILURE",
                "Nonfinite contact impulse",
            ));
        }
        Ok(impulse.to_array())
    }

    pub fn new(definition: Definition, mechanism: &Mechanism, model: &Value) -> Result<Self> {
        if let Some((code, message)) = unavailable(mechanism) {
            return Err(Error::domain(code, message));
        }
        if definition
            .gravity_m_s2
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 1000.)
        {
            return Err(invalid(
                "Gravity must be finite and within 1000 m/s² per axis",
            ));
        }
        let proxies = collision::World::for_model(mechanism, model)?.ok_or_else(|| {
            invalid("Simulation requires BRep-verified collision proxies for every body")
        })?;
        let evaluation = mechanism.evaluate()?;
        if definition.bodies.len() != evaluation.poses.len()
            || evaluation
                .poses
                .keys()
                .any(|name| !definition.bodies.contains_key(name))
        {
            return Err(invalid(
                "Every instance needs an explicit fixed/dynamic body definition",
            ));
        }
        let mut world = PhysicsWorld {
            gravity: DVec3::from_array(definition.gravity_m_s2),
            integration_parameters: IntegrationParameters {
                dt: TIMESTEP_S,
                normalized_allowed_linear_error: 1e-6,
                normalized_prediction_distance: 0.001,
                max_ccd_substeps: 4,
                num_solver_iterations: 24,
                // Closed graphs need more velocity sweeps than independent
                // trees: a motor reaction must propagate around the loop.
                num_internal_pgs_iterations: if mechanism.definition().constraints.is_some() {
                    64
                } else {
                    4
                },
                warmstart_joints: true,
                contact_softness: SpringCoefficients::new(120., 1.),
                static_contact_softness: SpringCoefficients::new(240., 1.),
                normalized_contact_recycle_distance: 1e-6,
                ..IntegrationParameters::default()
            },
            ..PhysicsWorld::default()
        };
        let mut handles = BTreeMap::new();
        let mut mass_kg = BTreeMap::new();
        let shapes = proxies.simulation_shapes();
        for (name, body) in &definition.bodies {
            let (builder, friction, restitution, mass) = match body {
                Body::Fixed {
                    friction,
                    restitution,
                } => (RigidBodyBuilder::fixed(), *friction, *restitution, None),
                Body::Dynamic {
                    mass,
                    friction,
                    restitution,
                } => {
                    let mass = match mass {
                        Mass::Kilograms { mass_kg } => *mass_kg,
                        Mass::Density { density_kg_m3 } => {
                            if !density_kg_m3.is_finite() || !(1e-6..=1e8).contains(density_kg_m3) {
                                return Err(invalid("Density must be 1e-6..1e8 kg/m³"));
                            }
                            let volume = model["parts"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .find(|part| part["instance_id"] == *name)
                                .unwrap()["volume"]
                                .as_f64()
                                .ok_or_else(|| invalid("Density needs exact solid volume"))?;
                            volume * 1e-9 * density_kg_m3
                        }
                    };
                    if !mass.is_finite() || !(1e-12..=1e9).contains(&mass) {
                        return Err(invalid("Dynamic mass must be 1e-12..1e9 kg"));
                    }
                    (
                        // Simulation advances only on explicit ticks. Do not let
                        // the engine's game-scale sleep threshold freeze slow
                        // mechanisms or discard their remaining kinetic energy.
                        // Pause/Stop, rather than body sleep, controls execution.
                        RigidBodyBuilder::dynamic()
                            .ccd_enabled(true)
                            .can_sleep(false),
                        *friction,
                        *restitution,
                        Some(mass),
                    )
                }
            };
            if !friction.is_finite()
                || !(0. ..=10.).contains(&friction)
                || !restitution.is_finite()
                || !(0. ..=1.).contains(&restitution)
            {
                return Err(invalid("Friction must be 0..10 and restitution 0..1"));
            }
            let handle = world
                .bodies
                .insert(builder.pose(pose(&evaluation.poses[name])));
            let pieces = shapes
                .iter()
                .filter(|(body, ..)| body == name)
                .map(|(_, frame, shape)| (*frame, shape.clone()))
                .collect();
            let mut collider = ColliderBuilder::compound(pieces)
                .friction(friction)
                .restitution(restitution);
            if let Some(mass) = mass {
                collider = collider.mass(mass);
                mass_kg.insert(name.clone(), mass);
            }
            world
                .colliders
                .insert_with_parent(collider, handle, &mut world.bodies);
            handles.insert(name.clone(), handle);
        }
        let ground = world.bodies.insert(RigidBodyBuilder::fixed());
        let collision = mechanism.definition().collision.as_ref().unwrap();
        let mut drives = BTreeMap::new();
        let mut gears = Vec::new();
        let mut paths = Vec::new();
        for joint in &mechanism.definition().joints {
            if !matches!(definition.bodies[&joint.child], Body::Dynamic { .. }) {
                return Err(invalid("A joint child must be dynamic in Simulate mode"));
            }
            let parent = joint.parent.as_ref().map_or(ground, |name| handles[name]);
            let child = handles[&joint.child];
            if let JointKind::Path { path } = &joint.kind {
                let coordinate = evaluation
                    .joints
                    .iter()
                    .find(|state| state.id == joint.id)
                    .map(|state| state.coordinate)
                    .unwrap_or(joint.initial);
                paths.push(PathTrack {
                    id: joint.id.clone(),
                    child,
                    parent,
                    parent_frame: frame(&joint.parent_frame)?,
                    child_frame: frame(&joint.child_frame)?,
                    path: crate::paths::Path::compile(path)?,
                    limits: joint.limits.unwrap(),
                    coordinate,
                    speed: 0.,
                    drivable: !mechanism
                        .definition()
                        .constraints
                        .as_ref()
                        .is_some_and(|constraints| constraints.is_passive(&joint.id)),
                    motor: None,
                });
                continue;
            }
            let (mask, axis, units) = match &joint.kind {
                JointKind::Fixed => (JointAxesMask::LOCKED_FIXED_AXES, None, "fixed"),
                JointKind::Prismatic { axis } => (
                    JointAxesMask::LOCKED_PRISMATIC_AXES,
                    Some((DVec3::from_array(*axis), JointAxis::LinX)),
                    "m",
                ),
                JointKind::Revolute { axis } => (
                    JointAxesMask::LOCKED_REVOLUTE_AXES,
                    Some((DVec3::from_array(*axis), JointAxis::AngX)),
                    "rad",
                ),
                JointKind::Path { .. } => unreachable!(),
            };
            let mut a = frame(&joint.parent_frame)?;
            let mut b = frame(&joint.child_frame)?;
            if let Some((direction, _)) = axis {
                let rotation = DQuat::from_rotation_arc(DVec3::X, direction.normalize());
                a.rotation *= rotation;
                b.rotation *= rotation;
            }
            let ignored = joint.parent.as_ref().is_some_and(|parent| {
                collision
                    .ignored_pairs
                    .iter()
                    .any(|p| p.contains(parent) && p.contains(&joint.child))
            });
            let mut data = GenericJointBuilder::new(mask)
                .local_frame1(a)
                .local_frame2(b)
                .contacts_enabled(!ignored)
                .build();
            if let Some((_, axis)) = axis {
                data.set_limits(axis, joint.limits.unwrap());
            }
            let handle = world.impulse_joints.insert(parent, child, data, true);
            if let Some((_, axis)) = axis
                && !mechanism
                    .definition()
                    .constraints
                    .as_ref()
                    .is_some_and(|constraints| constraints.is_passive(&joint.id))
            {
                drives.insert(
                    joint.id.clone(),
                    Drive {
                        handle,
                        axis,
                        limits: joint.limits.unwrap(),
                        unit: units,
                        active: false,
                    },
                );
            }
        }
        if let Some(constraints) = &mechanism.definition().constraints {
            use crate::constraints::Equation;
            for equation in &constraints.equations {
                if let Equation::Gear {
                    joint_a,
                    joint_b,
                    ratio,
                    offset,
                    backlash,
                    ..
                } = equation
                {
                    let gear = Gear {
                        a: gear_dof(mechanism, joint_a, &handles, ground)?,
                        b: gear_dof(mechanism, joint_b, &handles, ground)?,
                        ratio: *ratio,
                        offset: *offset,
                        backlash: *backlash,
                    };
                    let inv_a = dof_inverse_inertia(&world, &gear.a);
                    let inv_b = dof_inverse_inertia(&world, &gear.b);
                    let stiffness = gear.ratio * gear.ratio * inv_a + inv_b;
                    if !stiffness.is_finite() || stiffness < 1e-16 {
                        return Err(invalid(format!(
                            "Gear {joint_a}/{joint_b} needs a finite free-axis inertia"
                        )));
                    }
                    gears.push(gear);
                    continue;
                }
                let (id, a_id, b_id, mut a, mut b, mask, distance) = match equation {
                    Equation::HingeClosure { id, a, b } => {
                        let rotate = DQuat::from_rotation_arc(DVec3::X, DVec3::Z);
                        let mut fa = frame(&a.frame)?;
                        fa.rotation *= rotate;
                        let mut fb = frame(&b.frame)?;
                        fb.rotation *= rotate;
                        (
                            id,
                            &a.instance_id,
                            &b.instance_id,
                            fa,
                            fb,
                            JointAxesMask::LOCKED_REVOLUTE_AXES,
                            None,
                        )
                    }
                    Equation::Coincident { id, a, b } => {
                        let mut fa = rapier3d_f64::math::Pose::IDENTITY;
                        fa.translation = DVec3::from_array(a.point_m);
                        let mut fb = rapier3d_f64::math::Pose::IDENTITY;
                        fb.translation = DVec3::from_array(b.point_m);
                        (
                            id,
                            &a.instance_id,
                            &b.instance_id,
                            fa,
                            fb,
                            JointAxesMask::LIN_AXES,
                            None,
                        )
                    }
                    Equation::Distance {
                        id,
                        a,
                        b,
                        distance_m,
                    } => {
                        let mut fa = rapier3d_f64::math::Pose::IDENTITY;
                        fa.translation = DVec3::from_array(a.point_m);
                        let mut fb = rapier3d_f64::math::Pose::IDENTITY;
                        fb.translation = DVec3::from_array(b.point_m);
                        (
                            id,
                            &a.instance_id,
                            &b.instance_id,
                            fa,
                            fb,
                            JointAxesMask::empty(),
                            Some(*distance_m),
                        )
                    }
                    Equation::AlignedAxes { id, a, b } => {
                        let mut fa = rapier3d_f64::math::Pose::IDENTITY;
                        fa.rotation =
                            DQuat::from_rotation_arc(DVec3::X, DVec3::from_array(a.direction));
                        let mut fb = rapier3d_f64::math::Pose::IDENTITY;
                        fb.rotation =
                            DQuat::from_rotation_arc(DVec3::X, DVec3::from_array(b.direction));
                        (
                            id,
                            &a.instance_id,
                            &b.instance_id,
                            fa,
                            fb,
                            JointAxesMask::ANG_Y | JointAxesMask::ANG_Z,
                            None,
                        )
                    }
                    Equation::Gear { .. } => unreachable!("gears are inserted above"),
                };
                let handle = |id: &Option<String>| -> Result<RigidBodyHandle> {
                    match id {
                        Some(id) => handles
                            .get(id)
                            .copied()
                            .ok_or_else(|| invalid("Unknown closing-joint body")),
                        None => Ok(ground),
                    }
                };
                let body_a = handle(a_id)?;
                let body_b = handle(b_id)?;
                if body_a == body_b
                    || (!world.bodies[body_a].is_dynamic() && !world.bodies[body_b].is_dynamic())
                {
                    return Err(invalid(format!(
                        "Constraint {id} must connect distinct bodies with at least one dynamic body"
                    )));
                }
                // Avoid relying on arbitrary quaternion signs from caller frames.
                a.rotation = a.rotation.normalize();
                b.rotation = b.rotation.normalize();
                let ignored = match (a_id, b_id) {
                    (Some(a), Some(b)) => collision
                        .ignored_pairs
                        .iter()
                        .any(|pair| pair.contains(a) && pair.contains(b)),
                    _ => false,
                };
                let mut joint = GenericJointBuilder::new(mask)
                    .local_frame1(a)
                    .local_frame2(b)
                    .contacts_enabled(!ignored)
                    .build();
                if let Some(distance) = distance {
                    joint.coupled_axes = JointAxesMask::LIN_AXES;
                    // A coupled radial limit behaves as a rope under
                    // compression in the pinned engine. A bilateral radial
                    // position row uses exactly the same ERP/CFM as the other
                    // locked joints: k=ω², d=2ζω, acceleration based. Its default
                    // unbounded impulse is a constraint reaction, not a user
                    // motor; do not register it as a drive.
                    let omega = joint.softness.angular_frequency();
                    let damping = 2. * joint.softness.damping_ratio * omega;
                    joint
                        .set_motor_model(JointAxis::LinX, MotorModel::AccelerationBased)
                        .set_motor_position(JointAxis::LinX, distance, omega * omega, damping);
                }
                world.impulse_joints.insert(body_a, body_b, joint, true);
            }
        }
        // Explicit collision exclusions also apply to unconnected body pairs.
        for [a, b] in &collision.ignored_pairs {
            world.impulse_joints.insert(
                handles[a],
                handles[b],
                GenericJointBuilder::new(JointAxesMask::empty()).contacts_enabled(false),
                true,
            );
        }
        let empty = Frame {
            tick: 0,
            time_s: 0.,
            poses: BTreeMap::new(),
            linear_velocity_m_s: BTreeMap::new(),
            angular_velocity_rad_s: BTreeMap::new(),
        };
        let mut simulation = Self {
            world,
            handles,
            drives,
            gears,
            paths,
            tick: 0,
            previous: empty,
            mass_kg,
            external_loads: BTreeMap::new(),
        };
        simulation.previous = simulation.frame()?;
        Ok(simulation)
    }
    /// Motor inputs are SI and force based; this never teleports a body.
    pub fn motor(&mut self, target: &Motor) -> Result<f64> {
        if [
            target.target,
            target.stiffness,
            target.damping,
            target.max_force,
        ]
        .iter()
        .any(|v| !v.is_finite())
            || !(0. ..=1e9).contains(&target.stiffness)
            || !(0. ..=1e9).contains(&target.damping)
            || !(1e-12..=1e9).contains(&target.max_force)
        {
            return Err(invalid(
                "Motor needs correct units, nonnegative stiffness/damping and positive bounded maximum force/torque",
            ));
        }
        if let Some(drive) = self.drives.get_mut(&target.joint_id) {
            if target.unit != drive.unit {
                return Err(invalid(
                    "Motor needs correct units, nonnegative stiffness/damping and positive bounded maximum force/torque",
                ));
            }
            drive.active = true;
            let applied = target.target.clamp(drive.limits[0], drive.limits[1]);
            let joint = self
                .world
                .impulse_joints
                .get_mut(drive.handle, true)
                .unwrap();
            joint
                .data
                .set_motor_model(drive.axis, MotorModel::ForceBased)
                .set_motor_position(drive.axis, applied, target.stiffness, target.damping)
                .set_motor_max_force(drive.axis, target.max_force);
            return Ok(applied);
        }
        if let Some(track) = self
            .paths
            .iter_mut()
            .find(|track| track.id == target.joint_id && track.drivable)
        {
            if target.unit != "m" {
                return Err(invalid(
                    "Motor needs correct units, nonnegative stiffness/damping and positive bounded maximum force/torque",
                ));
            }
            let applied = target.target.clamp(track.limits[0], track.limits[1]);
            track.motor = Some(PathMotor {
                target: applied,
                stiffness: target.stiffness,
                damping: target.damping,
                max_force: target.max_force,
            });
            return Ok(applied);
        }
        Err(invalid("Unknown, fixed or passive motor joint"))
    }
    /// Replace persistent loads without changing poses or velocities.
    pub fn set_loads(&mut self, loads: BTreeMap<String, Load>) -> Result<()> {
        // Validate the whole replacement before clearing an existing load.
        for (name, load) in &loads {
            let body = self
                .handles
                .get(name)
                .map(|handle| &self.world.bodies[*handle]);
            if !body.is_some_and(|body| body.is_dynamic()) {
                return Err(invalid("External loads require a known dynamic body"));
            }
            if load
                .force_n
                .iter()
                .chain(&load.torque_nm)
                .any(|value| !value.is_finite() || value.abs() > 1e9)
            {
                return Err(invalid(
                    "Load components must be finite and at most 1e9 N or N m in world coordinates",
                ));
            }
        }
        for (name, handle) in &self.handles {
            if self.external_loads.get(name) == loads.get(name) {
                continue;
            }
            let body = &mut self.world.bodies[*handle];
            body.reset_forces(true);
            body.reset_torques(true);
            if let Some(load) = loads.get(name) {
                body.add_force(DVec3::from_array(load.force_n), true);
                body.add_torque(DVec3::from_array(load.torque_nm), true);
            }
        }
        self.external_loads = loads;
        Ok(())
    }
    /// Call on a private candidate; errors/cancellation discard that candidate.
    pub fn step(
        &mut self,
        steps: u32,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Result<Snapshot> {
        self.step_capture(steps, cancel, |_| Ok(()))
    }
    pub(crate) fn step_capture(
        &mut self,
        steps: u32,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
        mut capture: impl FnMut(&Self) -> Result<()>,
    ) -> Result<Snapshot> {
        if !(1..=120).contains(&steps) {
            return Err(invalid("Advance 1..120 fixed ticks per request"));
        }
        if self.tick > 9_007_199_254_740_991 - u64::from(steps) {
            return Err(invalid("Simulation tick budget exhausted"));
        }
        for _ in 0..steps {
            if cancel.is_some_and(|c| *c.borrow()) {
                return Err(Error::domain("CANCELLED", "Simulation advance cancelled"));
            }
            self.previous = self.frame()?;
            // Rapier's velocity-based sleep threshold can otherwise stop a
            // small body under a persistent load before it accelerates out of
            // that threshold. A nonzero applied load keeps it participating.
            for (name, load) in &self.external_loads {
                if load
                    .force_n
                    .iter()
                    .chain(&load.torque_nm)
                    .any(|value| *value != 0.)
                {
                    self.world.bodies[self.handles[name]].wake_up(true);
                }
            }
            // Keep path bodies on the track with their reduced-coordinate velocity
            // so contact during this tick sees the constrained pose, not a free drift.
            self.pose_paths_for_step()?;
            self.world.step();
            // Rapier has no gear or path joint. Project gears, then replace any
            // free motion of a path body with its arc-length integral, folding in
            // contact impulses accumulated while the body was held on the track.
            self.enforce_gears()?;
            self.enforce_paths()?;
            self.tick += 1;
            if !self.world.quarantine().is_empty() {
                return Err(Error::domain(
                    "SIMULATION_NUMERIC_FAILURE",
                    "Rapier quarantined nonfinite state; previous committed pose is preserved",
                ));
            }
            self.frame()?;
            capture(self)?;
        }
        self.snapshot()
    }
    /// Tooth-contact projection of `q_b - ratio * q_a - offset`.
    ///
    /// Inside the backlash deadband the mesh is open and no impulse is applied.
    /// Outside it, the residual is measured from the nearer tooth flank. The
    /// impulse uses free-axis inverse inertia about the joint axis (including the
    /// parallel-axis term for an offset center of mass), which is the reduced
    /// form of equal-and-opposite pitch-point forces along the line of action.
    /// Position is moved by the same inertia weighting.
    fn enforce_gears(&mut self) -> Result<()> {
        if self.gears.is_empty() {
            return Ok(());
        }
        for _ in 0..4 {
            for index in 0..self.gears.len() {
                self.enforce_gear(index)?;
            }
        }
        Ok(())
    }
    fn enforce_gear(&mut self, index: usize) -> Result<()> {
        let gear = self.gears[index].clone();
        let a = self.dof_state(&gear.a);
        let b = self.dof_state(&gear.b);
        let raw = b.coordinate - gear.ratio * a.coordinate - gear.offset;
        let half = gear.backlash * 0.5;
        let error = if raw > half {
            raw - half
        } else if raw < -half {
            raw + half
        } else {
            0.
        };
        let error_rate = b.velocity - gear.ratio * a.velocity;
        // Open mesh inside the backlash: no tooth force.
        if gear.backlash > 0. && error == 0. {
            return Ok(());
        }
        if error == 0. && error_rate.abs() <= 1e-16 {
            return Ok(());
        }
        let inv_a = self.body_pair_inertia(&gear.a, a.axis, a.anchor);
        let inv_b = self.body_pair_inertia(&gear.b, b.axis, b.anchor);
        let metric = gear.ratio * gear.ratio * inv_a + inv_b;
        if !metric.is_finite() || metric < 1e-16 {
            return Err(Error::domain(
                "SIMULATION_NUMERIC_FAILURE",
                "Gear constraint lost its free-axis inertia",
            ));
        }
        let velocity_impulse = -error_rate / metric;
        let position_scale = -error / metric;
        if ![velocity_impulse, position_scale]
            .iter()
            .all(|value| value.is_finite() && value.abs() <= 1e6)
        {
            return Err(Error::domain(
                "SIMULATION_NUMERIC_FAILURE",
                "Gear constraint impulse is nonfinite or unbounded",
            ));
        }
        self.apply_dof_impulse(&gear.a, a.axis, a.anchor, -gear.ratio * velocity_impulse);
        self.apply_dof_impulse(&gear.b, b.axis, b.anchor, velocity_impulse);
        let correction_a = inv_a * gear.ratio * error / metric;
        let correction_b = -inv_b * error / metric;
        self.project_dof(&gear.a, a.axis, a.anchor, correction_a);
        self.project_dof(&gear.b, b.axis, b.anchor, correction_b);
        Ok(())
    }
    fn dof_state(&self, dof: &GearDof) -> DofState {
        let parent = self.world.bodies[dof.parent].position();
        let child = self.world.bodies[dof.child].position();
        let anchor_pose = parent * dof.parent_frame;
        let child_pose = child * dof.child_frame;
        let axis = (anchor_pose.rotation * dof.axis).normalize();
        let relative = anchor_pose.inverse() * child_pose;
        let coordinate = if dof.revolute {
            let rotation = relative.rotation;
            let angle = 2. * rotation.xyz().dot(dof.axis).atan2(rotation.w);
            (angle + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI
        } else {
            relative.translation.dot(dof.axis)
        };
        let parent_body = &self.world.bodies[dof.parent];
        let child_body = &self.world.bodies[dof.child];
        let velocity = if dof.revolute {
            (child_body.angvel() - parent_body.angvel()).dot(axis)
        } else {
            let parent_point = parent_body.linvel()
                + parent_body
                    .angvel()
                    .cross(anchor_pose.translation - parent_body.center_of_mass());
            let child_point = child_body.linvel()
                + child_body
                    .angvel()
                    .cross(child_pose.translation - child_body.center_of_mass());
            (child_point - parent_point).dot(axis)
        };
        DofState {
            coordinate,
            velocity,
            axis,
            anchor: anchor_pose.translation,
        }
    }
    fn body_pair_inertia(&self, dof: &GearDof, axis: DVec3, anchor: DVec3) -> f64 {
        body_inverse_inertia(&self.world.bodies[dof.child], dof.revolute, axis, anchor)
            + body_inverse_inertia(&self.world.bodies[dof.parent], dof.revolute, axis, anchor)
    }
    fn apply_dof_impulse(&mut self, dof: &GearDof, axis: DVec3, anchor: DVec3, impulse: f64) {
        if impulse == 0. {
            return;
        }
        if dof.revolute {
            // Apply a generalized hinge impulse: change spin about the joint axis
            // using I_com + m d², and add the matching center-of-mass velocity so
            // the body rotates about the bearing rather than spinning in place.
            for (handle, sign) in [(dof.child, 1.), (dof.parent, -1.)] {
                let body = &mut self.world.bodies[handle];
                if !body.is_dynamic() {
                    continue;
                }
                let r = body.center_of_mass() - anchor;
                let r_perp = r - axis * r.dot(axis);
                let inertia = axis.dot(body.mass_properties().effective_angular_inertia() * axis)
                    + body.mass() * r_perp.length_squared();
                if inertia < 1e-16 {
                    continue;
                }
                let delta_omega = axis * (sign * impulse / inertia);
                let omega = body.angvel() + delta_omega;
                let linvel = body.linvel() + delta_omega.cross(r);
                body.set_angvel(omega, true);
                body.set_linvel(linvel, true);
            }
        } else {
            self.world.bodies[dof.child].apply_impulse(axis * impulse, true);
            self.world.bodies[dof.parent].apply_impulse(-axis * impulse, true);
        }
    }
    fn project_dof(&mut self, dof: &GearDof, axis: DVec3, anchor: DVec3, delta: f64) {
        if delta.abs() <= 1e-16 {
            return;
        }
        let body = &mut self.world.bodies[dof.child];
        let pose = *body.position();
        let updated = if dof.revolute {
            let rotation = DQuat::from_axis_angle(axis, delta);
            rapier3d_f64::math::Pose {
                translation: anchor + rotation * (pose.translation - anchor),
                rotation: (rotation * pose.rotation).normalize(),
            }
        } else {
            rapier3d_f64::math::Pose {
                translation: pose.translation + axis * delta,
                rotation: pose.rotation,
            }
        };
        body.set_position(updated, true);
    }
    /// Pose every path body on its track with the reduced-coordinate velocity so
    /// the following Rapier tick resolves contact against the constrained pose.
    fn pose_paths_for_step(&mut self) -> Result<()> {
        for index in 0..self.paths.len() {
            let s = self.paths[index].coordinate;
            let speed = self.paths[index].speed;
            let pose = self.path_pose(index, s);
            let (velocity_per_m, omega_per_m) = self.path_basis(index, s);
            let body = &mut self.world.bodies[self.paths[index].child];
            body.set_position(pose, true);
            body.set_linvel(velocity_per_m * speed, true);
            body.set_angvel(omega_per_m * speed, true);
            if !pose.translation.is_finite()
                || !pose.rotation.is_finite()
                || !(velocity_per_m * speed).is_finite()
                || !(omega_per_m * speed).is_finite()
            {
                return Err(Error::domain(
                    "SIMULATION_NUMERIC_FAILURE",
                    "Path pose for contact is nonfinite",
                ));
            }
        }
        Ok(())
    }
    /// Project contact impulses from the preceding Rapier tick onto the path
    /// coordinate. Linear impulse contributes through the tangent; the moment of
    /// that impulse about the center of mass contributes through the path spin.
    fn path_contact_impulse(&self, index: usize) -> Result<f64> {
        let child = self.paths[index].child;
        let s = self.paths[index].coordinate;
        let (velocity_per_m, omega_per_m) = self.path_basis(index, s);
        let com = self.world.bodies[child].center_of_mass();
        let mut generalized = 0.;
        for pair in self.world.narrow_phase.contact_pairs() {
            let a = self.world.colliders[pair.collider1].parent();
            let b = self.world.colliders[pair.collider2].parent();
            // Pinned Rapier solver uses force_dir1 = -manifold.normal:
            // total_impulse() follows the impulse applied to collider2.
            let sign = if a == Some(child) && b != Some(child) {
                -1.
            } else if b == Some(child) && a != Some(child) {
                1.
            } else {
                continue;
            };
            for manifold in pair.solver_manifolds() {
                let normal = manifold.data.normal;
                for contact in &manifold.points {
                    let impulse = contact.data.impulse;
                    if impulse == 0. || !impulse.is_finite() {
                        continue;
                    }
                    let linear = normal * (sign * impulse);
                    let point = if a == Some(child) {
                        self.world.colliders[pair.collider1].position() * contact.local_p1
                    } else {
                        self.world.colliders[pair.collider2].position() * contact.local_p2
                    };
                    let torque = (point - com).cross(linear);
                    generalized += linear.dot(velocity_per_m) + torque.dot(omega_per_m);
                }
            }
        }
        if !generalized.is_finite() {
            return Err(Error::domain(
                "SIMULATION_NUMERIC_FAILURE",
                "Path contact impulse is nonfinite",
            ));
        }
        Ok(generalized)
    }
    /// Reduced arc-length dynamics. The constraint force does no work, so only
    /// gravity, external loads, the path motor and projected contact impulses
    /// change the coordinate. Free motion from the Rapier tick is discarded; the
    /// contact it resolved against the on-track pose is kept.
    fn enforce_paths(&mut self) -> Result<()> {
        if self.paths.is_empty() {
            return Ok(());
        }
        let contacts = (0..self.paths.len())
            .map(|index| self.path_contact_impulse(index))
            .collect::<Result<Vec<_>>>()?;
        for (index, impulse) in contacts.into_iter().enumerate() {
            if impulse == 0. {
                continue;
            }
            let metric = self.path_metric(index)?;
            self.paths[index].speed += impulse / metric;
            if !self.paths[index].speed.is_finite() {
                return Err(Error::domain(
                    "SIMULATION_NUMERIC_FAILURE",
                    "Path contact left a nonfinite speed",
                ));
            }
        }
        let substeps = self
            .world
            .integration_parameters
            .num_solver_iterations
            .max(1);
        let step = TIMESTEP_S / substeps as f64;
        for _ in 0..substeps {
            for index in 0..self.paths.len() {
                self.integrate_path(index, step)?;
            }
        }
        Ok(())
    }
    fn path_metric(&self, index: usize) -> Result<f64> {
        let s = self.paths[index].coordinate;
        let (velocity_per_m, omega_per_m) = self.path_basis(index, s);
        let body = &self.world.bodies[self.paths[index].child];
        let mass = body.mass();
        let inertia = body.mass_properties().effective_angular_inertia();
        let metric =
            mass * velocity_per_m.length_squared() + omega_per_m.dot(inertia * omega_per_m);
        if !metric.is_finite() || metric < 1e-16 {
            return Err(Error::domain(
                "SIMULATION_NUMERIC_FAILURE",
                "Path coordinate lost its finite inertia",
            ));
        }
        Ok(metric)
    }
    fn integrate_path(&mut self, index: usize, step: f64) -> Result<()> {
        let s = self.paths[index].coordinate;
        let speed = self.paths[index].speed;
        let pose = self.path_pose(index, s);
        self.world.bodies[self.paths[index].child].set_position(pose, true);
        let (velocity_per_m, omega_per_m) = self.path_basis(index, s);
        let metric = self.path_metric(index)?;
        let body = &self.world.bodies[self.paths[index].child];
        let force = body.mass() * self.world.gravity * body.gravity_scale() + body.user_force();
        let mut generalized = force.dot(velocity_per_m) + body.user_torque().dot(omega_per_m);
        if let Some(motor) = &self.paths[index].motor {
            let spring = motor.stiffness * (motor.target - s) - motor.damping * speed;
            generalized += spring.clamp(-motor.max_force, motor.max_force);
        }
        if !generalized.is_finite() {
            return Err(Error::domain(
                "SIMULATION_NUMERIC_FAILURE",
                "Path force is nonfinite",
            ));
        }
        let mut speed = speed + generalized / metric * step;
        let mut coordinate = s + speed * step;
        let [min, max] = self.paths[index].limits;
        if coordinate <= min {
            coordinate = min;
            speed = speed.max(0.);
        } else if coordinate >= max {
            coordinate = max;
            speed = speed.min(0.);
        }
        self.paths[index].coordinate = coordinate;
        self.paths[index].speed = speed;
        let pose = self.path_pose(index, coordinate);
        let (velocity_per_m, omega_per_m) = self.path_basis(index, coordinate);
        let body = &mut self.world.bodies[self.paths[index].child];
        body.set_position(pose, true);
        body.set_linvel(velocity_per_m * speed, true);
        body.set_angvel(omega_per_m * speed, true);
        Ok(())
    }
    fn path_pose(&self, index: usize, s: f64) -> rapier3d_f64::math::Pose {
        let track = &self.paths[index];
        let anchor = self.world.bodies[track.parent].position() * track.parent_frame;
        let (_, rotation, translation) = track.path.pose(s).to_scale_rotation_translation();
        let motion = rapier3d_f64::math::Pose {
            translation,
            rotation: rotation.normalize(),
        };
        anchor * motion * track.child_frame.inverse()
    }
    fn path_basis(&self, index: usize, s: f64) -> (DVec3, DVec3) {
        let track = &self.paths[index];
        let anchor = self.world.bodies[track.parent].position() * track.parent_frame;
        let (tangent, omega) = track.path.rate(s);
        let tangent = anchor.rotation * tangent;
        let omega = anchor.rotation * omega;
        let pose = self.path_pose(index, s);
        let offset = pose.rotation * self.world.bodies[track.child].local_center_of_mass();
        (tangent + omega.cross(offset), omega)
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        let motor_impulses = self
            .drives
            .iter()
            .filter(|(_, drive)| drive.active)
            .map(|(name, drive)| {
                let motor = &self
                    .world
                    .impulse_joints
                    .get(drive.handle)
                    .unwrap()
                    .data
                    .motors[drive.axis as usize];
                if !motor.impulse.is_finite() {
                    return Err(Error::domain(
                        "SIMULATION_NUMERIC_FAILURE",
                        "Nonfinite motor impulse",
                    ));
                }
                Ok((
                    name.clone(),
                    MotorImpulse {
                        impulse: motor.impulse,
                        impulse_unit: if drive.unit == "m" { "N s" } else { "N m s" }.into(),
                        max_force: motor.max_force,
                        force_unit: if drive.unit == "m" { "N" } else { "N m" }.into(),
                        target_position: motor.target_pos,
                        coordinate_unit: drive.unit.into(),
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let p = &self.world.integration_parameters;
        let solver_settings = SolverSettings {
            solver_substeps: p.num_solver_iterations,
            internal_pgs_iterations: p.num_internal_pgs_iterations,
            internal_stabilization_iterations: p.num_internal_stabilization_iterations,
            nominal_solver_substep_s: p.dt / p.num_solver_iterations as f64,
            contact_frequency_hz: p.contact_softness.natural_frequency,
            static_contact_frequency_hz: p.static_contact_softness.natural_frequency,
            contact_damping_ratio: p.contact_softness.damping_ratio,
            static_contact_damping_ratio: p.static_contact_softness.damping_ratio,
            contact_recycle_distance_m: p.normalized_contact_recycle_distance * p.length_unit,
            warmstart_joints: p.warmstart_joints,
            max_ccd_substeps: p.max_ccd_substeps,
        };
        Ok(Snapshot {
            external_loads: self.external_loads.clone(),
            solver_settings,
            motor_impulses,
            timestep_s: TIMESTEP_S,
            accuracy: "approximate dynamics; mass from explicit input/exact volume, inertia from conservative proxy".into(),
            mass_kg: self.mass_kg.clone(),
            previous: self.previous.clone(),
            current: self.frame()?,
        })
    }
    fn frame(&self) -> Result<Frame> {
        let mut result = Frame {
            tick: self.tick,
            time_s: self.tick as f64 * TIMESTEP_S,
            poses: BTreeMap::new(),
            linear_velocity_m_s: BTreeMap::new(),
            angular_velocity_rad_s: BTreeMap::new(),
        };
        for (name, &handle) in &self.handles {
            let body = &self.world.bodies[handle];
            let p = body.position();
            if !p.translation.is_finite()
                || p.translation.abs().max_element() > 1e6
                || !p.rotation.is_finite()
                || !body.linvel().is_finite()
                || !body.angvel().is_finite()
            {
                return Err(Error::domain(
                    "SIMULATION_NUMERIC_FAILURE",
                    "Simulation state is nonfinite or outside the 1e6 m domain",
                ));
            }
            result.poses.insert(
                name.clone(),
                kinematics::Pose {
                    position: (p.translation * 1000.).to_array(),
                    quaternion: p.rotation.normalize().to_array(),
                },
            );
            result
                .linear_velocity_m_s
                .insert(name.clone(), body.linvel().to_array());
            result
                .angular_velocity_rad_s
                .insert(name.clone(), body.angvel().to_array());
        }
        Ok(result)
    }
}

impl Simulation {
    pub fn evaluation(&self, mechanism: &Mechanism) -> Result<kinematics::Evaluation> {
        let mut evaluation = mechanism.evaluate()?;
        evaluation.poses = self.frame()?.poses;
        for state in &mut evaluation.joints {
            let joint = mechanism
                .definition()
                .joints
                .iter()
                .find(|joint| joint.id == state.id)
                .unwrap();
            let parent = joint
                .parent
                .as_ref()
                .map_or(rapier3d_f64::math::Pose::IDENTITY, |name| {
                    pose(&evaluation.poses[name])
                });
            let a = parent * frame(&joint.parent_frame)?;
            let b = pose(&evaluation.poses[&joint.child]) * frame(&joint.child_frame)?;
            let relative = a.inverse() * b;
            state.origin_mm = (a.translation * 1000.).to_array();
            match &joint.kind {
                JointKind::Prismatic { axis } => {
                    let axis = DVec3::from_array(*axis).normalize();
                    state.coordinate = relative.translation.dot(axis);
                    state.axis_world = (a.rotation * axis).to_array();
                }
                JointKind::Revolute { axis } => {
                    let axis = DVec3::from_array(*axis).normalize();
                    let q = relative.rotation;
                    let angle = 2. * q.xyz().dot(axis).atan2(q.w);
                    state.coordinate = (angle + std::f64::consts::PI)
                        .rem_euclid(std::f64::consts::TAU)
                        - std::f64::consts::PI;
                    state.axis_world = (a.rotation * axis).to_array();
                }
                JointKind::Path { .. } => {
                    let track = self
                        .paths
                        .iter()
                        .find(|track| track.id == state.id)
                        .expect("path joint has a track");
                    let (tangent, _) = track.path.rate(track.coordinate);
                    state.coordinate = track.coordinate;
                    state.axis_world = (a.rotation * tangent).normalize().to_array();
                }
                JointKind::Fixed => {}
            }
        }
        if let Some(constraints) = &mechanism.definition().constraints {
            evaluation.constraints = Some(constraints.report_evaluation(mechanism, &evaluation)?);
        }
        Ok(evaluation)
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Start {
        definition: Definition,
    },
    Advance {
        steps: u32,
    },
    Motor {
        target: Motor,
    },
    /// Replace all persistent external loads. An empty map clears them all.
    Loads {
        loads: BTreeMap<String, Load>,
    },
    Stop,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub workspace_id: String,
    pub revision: String,
    pub expected_pose_version: u64,
    pub command: Command,
}
impl crate::Core {
    pub async fn control_simulation(
        &self,
        actor: &str,
        args: Control,
    ) -> Result<crate::motion::Snapshot> {
        self.ensure_actor_active(actor)?;
        let (mechanism, current, model, recorder) = {
            let mut workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get_mut(&args.workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != args.revision {
                return Err(Error::domain(
                    "STALE_ENTITY",
                    "Simulation revision is no longer current",
                ));
            }
            let state = crate::motion::state(ws)?;
            if state.version != args.expected_pose_version {
                return Err(Error::domain(
                    "POSE_CONFLICT",
                    "Simulation requires the current pose version",
                ));
            }
            if state.playback.is_some() {
                return Err(Error::domain(
                    "MODE_CONFLICT",
                    "Leave playback before controlling simulation",
                ));
            }
            if state.recorder.is_some()
                && matches!(args.command, Command::Start { .. } | Command::Stop)
            {
                return Err(Error::domain(
                    "MODE_CONFLICT",
                    "Finish or cancel recording before switching modes",
                ));
            }
            let recorder = state.recorder.clone();
            let mechanism = state.mechanism.clone();
            let current = state.simulation.clone();
            (
                mechanism,
                current,
                ws.stored.revisions[&args.revision].model.clone(),
                recorder,
            )
        };
        let cancel = self.actor_signal(actor);
        let (candidate, recorder) = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut recorder = recorder;
            if *cancel.borrow() {
                return Err(Error::domain("CANCELLED", "Simulation control cancelled"));
            }
            match args.command {
                Command::Start { definition } => {
                    if current.is_some() {
                        return Err(Error::domain(
                            "MODE_CONFLICT",
                            "Simulation is already active",
                        ));
                    }
                    Ok((
                        Some(std::sync::Arc::new(Simulation::new(
                            definition, &mechanism, &model,
                        )?)),
                        recorder,
                    ))
                }
                Command::Stop => {
                    if current.is_none() {
                        return Err(Error::domain("MODE_CONFLICT", "Simulation is not active"));
                    }
                    Ok((None, recorder))
                }
                command => {
                    let mut candidate = current
                        .ok_or_else(|| Error::domain("MODE_CONFLICT", "Start Simulate mode first"))?
                        .as_ref()
                        .clone();
                    match command {
                        Command::Advance { steps } => {
                            candidate.step_capture(steps, Some(&cancel), |simulation| {
                                if let Some(recorder) = &mut recorder {
                                    recorder.capture(
                                        simulation.evaluation(&mechanism)?,
                                        args.expected_pose_version + 1,
                                        Some(simulation.tick as f64 * TIMESTEP_S),
                                    );
                                }
                                Ok(())
                            })?;
                        }
                        Command::Motor { target } => {
                            candidate.motor(&target)?;
                        }
                        Command::Loads { loads } => {
                            candidate.set_loads(loads)?;
                        }
                        _ => unreachable!(),
                    }
                    Ok((Some(std::sync::Arc::new(candidate)), recorder))
                }
            }
        })
        .await
        .map_err(|_| {
            Error::domain(
                "SIMULATION_NUMERIC_FAILURE",
                "Simulation task failed; committed pose preserved",
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
                "Revision changed during simulation control",
            ));
        }
        let state = crate::motion::state(ws)?;
        if state.version != args.expected_pose_version {
            return Err(Error::domain(
                "POSE_CONFLICT",
                "Pose changed during simulation control; candidate discarded",
            ));
        }
        let version = state
            .version
            .checked_add(1)
            .filter(|v| *v <= 9_007_199_254_740_991)
            .ok_or_else(|| Error::domain("POSE_CONFLICT", "Pose version exhausted"))?;
        state.simulation = candidate;
        state.recorder = recorder;
        state.version = version;
        let mut snapshot = crate::motion::snapshot(ws)?;
        snapshot.actor = Some(actor.into());
        let _ = self.motion_events.send(snapshot.clone());
        Ok(snapshot)
    }
}
