//! Conservative, derived collision proxies; never authoritative CAD measurements.
use crate::{
    Error, Result,
    kinematics::{Evaluation, Frame, Mechanism, Pose as CadPose},
};
use glam::{DQuat, DVec3};
use parry3d_f64::{
    math::Pose,
    query::{self, NonlinearRigidMotion, ShapeCastOptions, ShapeCastStatus},
    shape::SharedShape,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};
fn clearance() -> f64 {
    0.00001
}
fn backoff() -> f64 {
    0.000001
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Shape {
    Box { half_extents_m: [f64; 3] },
    Ball { radius_m: f64 },
    Bounds,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Primitive {
    #[serde(default)]
    pub frame: Frame,
    pub shape: Shape,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub bodies: BTreeMap<String, Vec<Primitive>>,
    #[serde(default = "clearance")]
    pub clearance_m: f64,
    #[serde(default = "backoff")]
    pub backoff_m: f64,
    #[serde(default)]
    pub ignored_pairs: Vec<[String; 2]>,
}
fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_COLLISION_PROXY", message)
}
impl Definition {
    pub fn validate(&self) -> Result<()> {
        if self.bodies.is_empty()
            || self.bodies.len() > 64
            || self.bodies.values().map(Vec::len).sum::<usize>() > 128
        {
            return Err(invalid(
                "Collision blocking requires 1..64 bodies and at most 128 primitive pieces",
            ));
        }
        if !self.clearance_m.is_finite()
            || !(0. ..=1.).contains(&self.clearance_m)
            || !self.backoff_m.is_finite()
            || !(1e-8..=0.01).contains(&self.backoff_m)
        {
            return Err(invalid("Clearance must be 0..1 m and backoff 1e-8..0.01 m"));
        }
        for (name, pieces) in &self.bodies {
            if name.is_empty()
                || name.len() > 256
                || name.chars().any(char::is_control)
                || pieces.is_empty()
            {
                return Err(invalid("Each named body needs at least one proxy piece"));
            }
            for piece in pieces {
                piece.frame.transform()?;
                if piece.frame.position_m.iter().any(|v| v.abs() > 1e6) {
                    return Err(invalid(
                        "Proxy frame translations must be within 1e6 metres",
                    ));
                }
                match piece.shape {
                    Shape::Box { half_extents_m } => {
                        if half_extents_m
                            .iter()
                            .any(|v| !v.is_finite() || *v <= 0. || *v > 1e6)
                        {
                            return Err(invalid(
                                "Box half extents must be finite positive metres, at most 1e6",
                            ));
                        }
                    }
                    Shape::Ball { radius_m } => {
                        if !radius_m.is_finite() || radius_m <= 0. || radius_m > 1e6 {
                            return Err(invalid(
                                "Sphere radius must be finite positive metres, at most 1e6",
                            ));
                        }
                    }
                    Shape::Bounds => {
                        if piece.frame.position_m != [0.; 3]
                            || piece.frame.quaternion != [0., 0., 0., 1.]
                        {
                            return Err(invalid(
                                "Bounds proxies use the identity frame; their local placement is derived from BRep",
                            ));
                        }
                    }
                }
            }
        }
        if self.ignored_pairs.len() > 128 {
            return Err(invalid("At most 128 explicit ignored pairs are supported"));
        }
        let mut pairs = HashSet::new();
        for [a, b] in &self.ignored_pairs {
            if a == b
                || !self.bodies.contains_key(a)
                || !self.bodies.contains_key(b)
                || !pairs.insert(pair(a, b))
            {
                return Err(invalid(
                    "Ignored pairs must be unique distinct named collision bodies",
                ));
            }
        }
        Ok(())
    }
}
fn pair(a: &str, b: &str) -> (String, String) {
    if a < b {
        (a.into(), b.into())
    } else {
        (b.into(), a.into())
    }
}
fn pose(value: &CadPose) -> Pose {
    Pose {
        translation: DVec3::from_array(value.position) * 0.001,
        rotation: DQuat::from_array(value.quaternion),
    }
}
#[derive(Clone)]
struct Piece {
    body: String,
    local: Pose,
    shape: SharedShape,
    radius: f64,
}
#[derive(Clone)]
pub struct World {
    definition: Definition,
    pieces: Vec<Piece>,
    // Resolve body exclusions once, rather than allocating string pairs in hot loops.
    included: Arc<Vec<Vec<bool>>>,
    body_pairs: Arc<BTreeMap<(String, String), Vec<(usize, usize)>>>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Contact {
    pub instances: [String; 2],
    pub expanded_proxy_separation_m: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Policy {
    pub accuracy: String,
    pub clearance_m: f64,
    pub backoff_m: f64,
    pub coverage_method: String,
    pub ignored_pairs: Vec<[String; 2]>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    pub accuracy: String,
    pub clearance_m: f64,
    pub backoff_m: f64,
    pub coverage_method: String,
    pub ignored_pairs: Vec<[String; 2]>,
    pub contacts: Vec<Contact>,
    pub checked_pairs: Vec<[String; 2]>,
    pub minimum_expanded_proxy_separation_m: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Sweep {
    pub trajectory_enclosure_used: bool,
    pub linear_fallback_used: bool,
    pub curve_expansion_m: f64,
    pub query_count: usize,
    pub blocked: bool,
    pub fraction: f64,
    pub instances: Option<[String; 2]>,
    pub accuracy: String,
    pub clearance_m: f64,
    pub backoff_m: f64,
}
impl World {
    /// Verified conservative shapes in body-local SI coordinates. Simulation
    /// shares the proxy policy, including clearance expansion and exclusions.
    pub(crate) fn simulation_shapes(&self) -> Vec<(String, Pose, SharedShape)> {
        self.pieces
            .iter()
            .map(|p| (p.body.clone(), p.local, p.shape.clone()))
            .collect()
    }
    pub fn for_model(mechanism: &Mechanism, model: &Value) -> Result<Option<Self>> {
        if let Some(definition) = &mechanism.definition().collision {
            let world = Self::new(definition.clone(), model)?;
            world.validate_pose(&mechanism.evaluate()?)?;
            Ok(Some(world))
        } else {
            Ok(None)
        }
    }
    pub fn new(definition: Definition, model: &Value) -> Result<Self> {
        definition.validate()?;
        let parts = model["parts"]
            .as_array()
            .ok_or_else(|| invalid("Missing model parts"))?;
        if parts.len() != definition.bodies.len()
            || parts.iter().any(|part| {
                !definition
                    .bodies
                    .contains_key(part["instance_id"].as_str().unwrap_or(""))
            })
        {
            return Err(invalid(
                "Every published instance needs an explicit proxy; no body may be silently omitted",
            ));
        }
        if model["collision_validation"]["method"] != "brep_boolean_coverage"
            || model["collision_validation"]["covered_instances"]
                .as_array()
                .is_none_or(|ids| {
                    ids.len() != parts.len()
                        || parts.iter().any(|part| !ids.contains(&part["instance_id"]))
                })
        {
            return Err(Error::domain(
                "COLLISION_PROXY_UNVERIFIED",
                "Build must verify conservative proxy coverage against the BRep first",
            ));
        }
        let mut pieces = vec![];
        for part in parts {
            let body = part["instance_id"].as_str().unwrap();
            for primitive in &definition.bodies[body] {
                let frame = primitive.frame.transform()?;
                let mut local = Pose {
                    translation: frame.translation,
                    rotation: DQuat::from_affine3(&frame).normalize(),
                };
                let margin = definition.clearance_m * 0.5;
                let (shape, radius) = match primitive.shape {
                    Shape::Box { half_extents_m: h } => {
                        let h = DVec3::from_array(h) + DVec3::splat(margin);
                        (SharedShape::cuboid(h.x, h.y, h.z), h.length())
                    }
                    Shape::Ball { radius_m: r } => (SharedShape::ball(r + margin), r + margin),
                    Shape::Bounds => {
                        let min: [f64; 3] = serde_json::from_value(part["bounds"]["min"].clone())?;
                        let max: [f64; 3] = serde_json::from_value(part["bounds"]["max"].clone())?;
                        let min = DVec3::from_array(min) * 0.001;
                        let max = DVec3::from_array(max) * 0.001;
                        let half = (max - min) * 0.5 + DVec3::splat(1e-9 + margin);
                        if !half.is_finite() || half.min_element() <= 0. || half.max_element() > 1e6
                        {
                            return Err(invalid("Invalid BRep bounds for proxy"));
                        }
                        local.translation = (max + min) * 0.5;
                        (SharedShape::cuboid(half.x, half.y, half.z), half.length())
                    }
                };
                pieces.push(Piece {
                    body: body.into(),
                    local,
                    shape,
                    radius,
                });
            }
        }
        let ignored: HashSet<_> = definition
            .ignored_pairs
            .iter()
            .map(|[a, b]| pair(a, b))
            .collect();
        let included: Vec<Vec<bool>> = pieces
            .iter()
            .map(|a| {
                pieces
                    .iter()
                    .map(|b| a.body != b.body && !ignored.contains(&pair(&a.body, &b.body)))
                    .collect()
            })
            .collect();
        let mut body_pairs = BTreeMap::<_, Vec<_>>::new();
        for (i, a) in pieces.iter().enumerate() {
            for (j, b) in pieces.iter().enumerate().skip(i + 1) {
                if included[i][j] {
                    body_pairs
                        .entry(pair(&a.body, &b.body))
                        .or_default()
                        .push((i, j));
                }
            }
        }
        Ok(Self {
            definition,
            pieces,
            included: Arc::new(included),
            body_pairs: Arc::new(body_pairs),
        })
    }
    pub fn policy(&self) -> Policy {
        Policy {
            accuracy: "conservative_proxy".into(),
            clearance_m: self.definition.clearance_m,
            backoff_m: self.definition.backoff_m,
            coverage_method: "brep_boolean_coverage".into(),
            ignored_pairs: self.definition.ignored_pairs.clone(),
        }
    }
    pub fn definition(&self) -> &Definition {
        &self.definition
    }
    fn included(&self, a: usize, b: usize) -> bool {
        self.included[a][b]
    }
    pub fn report(&self, evaluation: &Evaluation) -> Result<Report> {
        self.report_impl(evaluation, None)
    }
    pub fn report_pairs(&self, evaluation: &Evaluation, pairs: &[[String; 2]]) -> Result<Report> {
        let wanted = pairs.iter().map(|[a, b]| pair(a, b)).collect();
        self.report_impl(evaluation, Some(&wanted))
    }
    fn report_impl(
        &self,
        evaluation: &Evaluation,
        wanted: Option<&HashSet<(String, String)>>,
    ) -> Result<Report> {
        let posed = self.posed(evaluation)?;
        let mut checked = std::collections::BTreeSet::new();
        let mut minimum = None::<f64>;
        let mut contacts = BTreeMap::<(String, String), f64>::new();
        // Reuse the best exact separation as an upper bound. AABB distances
        // are lower bounds, never reported values. Avoid sorting all pairs:
        // these primitive queries are cheap enough that sorting can dominate.
        // Explicit pair requests visit only the pieces belonging to those bodies.
        let selected: Vec<_> = if let Some(wanted) = wanted {
            wanted
                .iter()
                .filter_map(|key| self.body_pairs.get_key_value(key))
                .collect()
        } else {
            self.body_pairs.iter().collect()
        };
        for (key, pieces) in selected {
            checked.insert(key.clone());
            for &(i, j) in pieces {
                let lower = posed[i].1.distance(&posed[j].1);
                if minimum.is_some_and(|minimum| lower > minimum.max(1e-9)) {
                    continue;
                }
                let a = &self.pieces[i];
                let b = &self.pieces[j];
                let distance = separation(&posed[i].0, &a.shape, &posed[j].0, &b.shape)?;
                minimum = Some(minimum.map_or(distance, |old| old.min(distance)));
                if distance <= 1e-9 {
                    contacts
                        .entry(key.clone())
                        .and_modify(|v| *v = v.min(distance))
                        .or_insert(distance);
                }
            }
        }
        Ok(Report {
            accuracy: "conservative_proxy".into(),
            clearance_m: self.definition.clearance_m,
            backoff_m: self.definition.backoff_m,
            coverage_method: "brep_boolean_coverage".into(),
            ignored_pairs: self.definition.ignored_pairs.clone(),
            checked_pairs: checked.into_iter().map(|(a, b)| [a, b]).collect(),
            contacts: contacts
                .into_iter()
                .map(|((a, b), d)| Contact {
                    instances: [a, b],
                    expanded_proxy_separation_m: d,
                })
                .collect(),
            minimum_expanded_proxy_separation_m: minimum,
        })
    }
    fn posed(&self, evaluation: &Evaluation) -> Result<Vec<(Pose, Bounds)>> {
        for pose in evaluation.poses.values() {
            if pose
                .position
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 1e9)
                || pose.quaternion.iter().any(|v| !v.is_finite())
                || (pose.quaternion.iter().map(|v| v * v).sum::<f64>() - 1.).abs() > 1e-6
            {
                return Err(Error::domain(
                    "COLLISION_QUERY_FAILED",
                    "Proxy poses require finite normalized transforms within 1e6 metres",
                ));
            }
        }
        self.pieces
            .iter()
            .map(|piece| {
                let value = evaluation.poses.get(&piece.body).ok_or_else(|| {
                    Error::domain("COLLISION_QUERY_FAILED", "Missing collision body pose")
                })?;
                let position = pose(value) * piece.local;
                let aabb = piece.shape.compute_aabb(&position);
                let padding = DVec3::splat(
                    1e-9 + 16. * f64::EPSILON * aabb.mins.abs().max(aabb.maxs.abs()).max_element(),
                );
                Ok((
                    position,
                    Bounds {
                        min: aabb.mins - padding,
                        max: aabb.maxs + padding,
                    },
                ))
            })
            .collect()
    }
    pub fn validate_pose(&self, evaluation: &Evaluation) -> Result<()> {
        let posed = self.posed(evaluation)?;
        let bounds: Vec<_> = posed.iter().map(|(_, bounds)| *bounds).collect();
        for (i, j) in overlapping_pairs(&bounds) {
            if self.included(i, j)
                && separation(
                    &posed[i].0,
                    &self.pieces[i].shape,
                    &posed[j].0,
                    &self.pieces[j].shape,
                )? < -1e-9
            {
                return Err(Error::domain(
                    "COLLISION_INITIAL_OVERLAP",
                    "Collision proxies overlap or violate configured clearance; repair placement/proxies or explicitly ignore an intentional pair",
                ));
            }
        }
        Ok(())
    }
    pub fn sweep(&self, mechanism: &Mechanism, joint_id: &str, target: f64) -> Result<Sweep> {
        self.sweep_cancel(mechanism, joint_id, target, None)
    }
    pub fn sweep_cancel(
        &self,
        mechanism: &Mechanism,
        joint_id: &str,
        target: f64,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Result<Sweep> {
        if cancel.is_some_and(|cancel| *cancel.borrow()) {
            return Err(Error::domain(
                "CANCELLED",
                "Collision sweep cancelled; pose preserved",
            ));
        }
        if mechanism.definition().constraints.is_some() {
            let joint = mechanism
                .definition()
                .joints
                .iter()
                .find(|joint| joint.id == joint_id)
                .ok_or_else(|| invalid("Unknown driven joint"))?;
            let mut solved = mechanism.clone();
            solved.set_cancel(
                joint_id,
                target,
                crate::constraints::unit(&joint.kind),
                cancel,
            )?;
            return self.sweep_trajectory(mechanism, solved.last_trajectory().unwrap(), cancel);
        }
        let start = mechanism.evaluate()?;
        self.validate_pose(&start)?;
        let joint = start
            .joints
            .iter()
            .find(|j| j.id == joint_id)
            .ok_or_else(|| invalid("Unknown driven joint"))?;
        let delta = target - joint.coordinate;
        if !delta.is_finite() || joint.kind == "prismatic" && delta.abs() > 1e6 {
            return Err(Error::domain(
                "COLLISION_SWEEP_LIMIT",
                "Swept translation exceeds the numeric range; pose preserved",
            ));
        }
        let mut result = Sweep {
            trajectory_enclosure_used: false,
            linear_fallback_used: false,
            curve_expansion_m: 0.,
            query_count: 0,
            blocked: false,
            fraction: 1.,
            instances: None,
            accuracy: "conservative_proxy".into(),
            clearance_m: self.definition.clearance_m,
            backoff_m: self.definition.backoff_m,
        };
        if delta == 0. {
            return Ok(result);
        }
        let moving = mechanism.descendants(joint_id)?;
        let origin = DVec3::from_array(joint.origin_mm) * 0.001;
        if !origin.is_finite() || origin.abs().max_element() > 1e6 {
            return Err(Error::domain(
                "COLLISION_SWEEP_LIMIT",
                "Joint sweep frame exceeds the numeric range",
            ));
        }
        let max_offset = self
            .pieces
            .iter()
            .filter(|piece| moving.contains(&piece.body))
            .map(|piece| {
                let p = pose(&start.poses[&piece.body]) * piece.local;
                (p.translation - origin).length() + piece.radius
            })
            .fold(0.0_f64, f64::max);
        let spans = mechanism.sweep_spans(joint_id, target)?;
        if spans.iter().any(|span| {
            !span.pivot_m.is_finite()
                || span.pivot_m.abs().max_element() > 1e6
                || !span.path_point_m.is_finite()
                || span.path_point_m.abs().max_element() > 1e6
        }) {
            return Err(Error::domain(
                "COLLISION_SWEEP_LIMIT",
                "Path sweep frame exceeds the numeric range",
            ));
        }
        let max_speed = spans
            .iter()
            .map(|span| {
                (span.translation_m.length()
                    + span.rotation_vector.length() * (span.bend_radius_m + max_offset)
                    + span.omitted_roll * max_offset)
                    / (span.to - span.from)
            })
            .fold(0.0_f64, f64::max);
        let mut pending = std::collections::VecDeque::from(spans);
        let mut processed = 0;
        while let Some(span) = pending.pop_front() {
            processed += 1;
            if processed > 4096 {
                return Err(Error::domain(
                    "COLLISION_SWEEP_LIMIT",
                    "Curved sweep exceeded the bounded refinement budget; pose preserved",
                ));
            }
            let expansion = max_offset * span.omitted_roll.min(2.);
            result.curve_expansion_m = result.curve_expansion_m.max(expansion);
            let mut configuration = mechanism.clone();
            configuration.set(joint_id, joint.coordinate + delta * span.from, &joint.unit)?;
            let evaluation = configuration.evaluate()?;
            if cancel.is_some_and(|cancel| *cancel.borrow()) {
                return Err(Error::domain(
                    "CANCELLED",
                    "Collision sweep cancelled; pose preserved",
                ));
            }
            let posed = self
                .pieces
                .iter()
                .map(|piece| {
                    let position = pose(&evaluation.poses[&piece.body]) * piece.local;
                    let moving = moving.contains(&piece.body);
                    let motion = motion(position, moving, &span);
                    let shape = expanded(&piece.shape, if moving { expansion } else { 0. })?;
                    let bounds = swept_bounds(&shape, &motion);
                    Ok((position, motion, shape, bounds))
                })
                .collect::<Result<Vec<_>>>()?;
            let mut candidate = None::<(f64, [String; 2])>;
            let mut refine = false;
            let bounds: Vec<_> = posed.iter().map(|entry| entry.3).collect();
            'pairs: for (i, j) in overlapping_pairs(&bounds) {
                let a = &self.pieces[i];
                let b = &self.pieces[j];
                if !self.included(i, j) || moving.contains(&a.body) == moving.contains(&b.body) {
                    continue;
                }
                if cancel.is_some_and(|cancel| *cancel.borrow()) {
                    return Err(Error::domain(
                        "CANCELLED",
                        "Collision sweep cancelled; pose preserved",
                    ));
                }
                let (pa, ma, sa, _) = &posed[i];
                let (pb, mb, sb, _) = &posed[j];
                result.query_count += 1;
                if result.query_count > 16384 {
                    return Err(Error::domain(
                        "COLLISION_SWEEP_LIMIT",
                        "Collision sweep exceeds the bounded primitive-pair query budget",
                    ));
                }
                let mut hit = if span.rotation_vector.length_squared() < 1e-30 {
                    query::cast_shapes(
                        pa,
                        ma.linvel,
                        &**sa,
                        pb,
                        mb.linvel,
                        &**sb,
                        ShapeCastOptions {
                            max_time_of_impact: 1.,
                            target_distance: 0.,
                            stop_at_penetration: expansion > 0.,
                            compute_impact_geometry_on_penetration: true,
                        },
                    )
                } else {
                    query::cast_shapes_nonlinear(ma, &**sa, mb, &**sb, 0., 1., expansion > 0.)
                }
                .map_err(|_| {
                    Error::domain(
                        "COLLISION_QUERY_UNSUPPORTED",
                        "Unsupported collision pair; motion was not accepted",
                    )
                })?;
                let can_refine = span.to - span.from > 1e-10
                    && (span.rotation_vector.length() > 1e-5
                        || span.translation_m.length() > self.definition.backoff_m * 0.25);
                if hit.as_ref().is_some_and(|hit| {
                    matches!(
                        hit.status,
                        ShapeCastStatus::Failed | ShapeCastStatus::OutOfIterations
                    )
                }) && !can_refine
                    && expansion <= self.definition.backoff_m
                {
                    // A failed nonlinear TOI is not evidence of free motion.
                    // A fixed-orientation linear cast with a proven rotational
                    // displacement envelope can still certify this small span.
                    let (la, ea, error_a) = linear_envelope(sa, ma)?;
                    let (lb, eb, error_b) = linear_envelope(sb, mb)?;
                    result.curve_expansion_m = result
                        .curve_expansion_m
                        .max(expansion + error_a.max(error_b));
                    result.linear_fallback_used = true;
                    result.query_count += 1;
                    if result.query_count > 16384 {
                        return Err(Error::domain(
                            "COLLISION_SWEEP_LIMIT",
                            "Collision query budget exhausted",
                        ));
                    }
                    hit = query::cast_shapes(
                        &la.start,
                        la.linvel,
                        &*ea,
                        &lb.start,
                        lb.linvel,
                        &*eb,
                        ShapeCastOptions {
                            max_time_of_impact: 1.,
                            target_distance: 0.,
                            stop_at_penetration: true,
                            compute_impact_geometry_on_penetration: true,
                        },
                    )
                    .map_err(|_| {
                        Error::domain(
                            "COLLISION_QUERY_UNSUPPORTED",
                            "Unsupported linear-envelope query",
                        )
                    })?;
                }
                if let Some(hit) = hit {
                    if expansion > self.definition.backoff_m && (span.to - span.from) > 1e-12 {
                        refine = true;
                        break 'pairs;
                    }
                    if matches!(
                        hit.status,
                        ShapeCastStatus::Failed | ShapeCastStatus::OutOfIterations
                    ) || !hit.time_of_impact.is_finite()
                    {
                        if span.to - span.from > 1e-10
                            && (span.rotation_vector.length() > 1e-5
                                || span.translation_m.length() > self.definition.backoff_m * 0.25)
                        {
                            refine = true;
                            break 'pairs;
                        }
                        return Err(Error::Domain {
                            code: "COLLISION_QUERY_FAILED".into(),
                            message: "Swept query did not converge; current pose is preserved"
                                .into(),
                            details: serde_json::json!({"instances":[a.body,b.body],"from":span.from,"to":span.to,"status":format!("{:?}",hit.status)}),
                        });
                    }
                    let fraction =
                        span.from + hit.time_of_impact.clamp(0., 1.) * (span.to - span.from);
                    if candidate.as_ref().is_none_or(|(old, _)| fraction < *old) {
                        candidate = Some((fraction, [a.body.clone(), b.body.clone()]));
                    }
                }
            }
            if refine {
                let (first, second) = span.split();
                pending.push_front(second);
                pending.push_front(first);
                continue;
            }
            if let Some((fraction, instances)) = candidate {
                result.blocked = true;
                result.fraction =
                    (fraction - self.definition.backoff_m / max_speed.max(1e-15)).clamp(0., 1.);
                result.instances = Some(instances);
                break;
            }
        }
        let mut accepted = mechanism.clone();
        accepted.set(
            joint_id,
            joint.coordinate + delta * result.fraction,
            &joint.unit,
        )?;
        self.validate_pose(&accepted.evaluate()?)?;
        Ok(result)
    }
}
fn motion(start: Pose, moving: bool, span: &crate::paths::Span) -> NonlinearRigidMotion {
    if !moving {
        return NonlinearRigidMotion::constant_position(start);
    }
    NonlinearRigidMotion::new(
        start,
        start.inverse_transform_point(span.pivot_m),
        span.translation_m,
        span.rotation_vector,
    )
}
fn expanded(shape: &SharedShape, distance: f64) -> Result<SharedShape> {
    if distance == 0. {
        return Ok(shape.clone());
    }
    if let Some(ball) = shape.as_ball() {
        return Ok(SharedShape::ball(ball.radius + distance));
    }
    if let Some(cuboid) = shape.as_cuboid() {
        let h = cuboid.half_extents + DVec3::splat(distance);
        return Ok(SharedShape::cuboid(h.x, h.y, h.z));
    }
    Err(Error::domain(
        "COLLISION_QUERY_UNSUPPORTED",
        "Curved motion expansion requires a supported primitive",
    ))
}
/// Linear interpolation of a twice-differentiable center curve differs by at
/// most max|p''|/8. Rotation of a shape about its center moves any point by at
/// most radius*angle. Their sum encloses the entire screw trajectory.
fn linear_envelope(
    shape: &SharedShape,
    motion: &NonlinearRigidMotion,
) -> Result<(NonlinearRigidMotion, SharedShape, f64)> {
    let angle = motion.angvel.length();
    let pivot = motion.start * motion.local_center;
    let center_radius = (motion.start.translation - pivot).length();
    let error =
        center_radius * angle * angle / 8. + shape.compute_local_bounding_sphere().radius * angle;
    let linear = NonlinearRigidMotion::new(
        motion.start,
        DVec3::ZERO,
        motion.position_at_time(1.).translation - motion.start.translation,
        DVec3::ZERO,
    );
    Ok((linear, expanded(shape, error)?, error))
}
fn separation(a: &Pose, sa: &SharedShape, b: &Pose, sb: &SharedShape) -> Result<f64> {
    let contact = query::contact(a, &**sa, b, &**sb, 0.001).map_err(|_| {
        Error::domain(
            "COLLISION_QUERY_UNSUPPORTED",
            "Unsupported proxy contact query",
        )
    })?;
    let value = if let Some(contact) = contact {
        contact.dist
    } else {
        query::distance(a, &**sa, b, &**sb)
            .map_err(|_| {
                Error::domain(
                    "COLLISION_QUERY_UNSUPPORTED",
                    "Unsupported proxy distance query",
                )
            })?
            .distance
    };
    if !value.is_finite() {
        return Err(Error::domain(
            "COLLISION_QUERY_FAILED",
            "Proxy separation is not finite",
        ));
    }
    Ok(value)
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    min: DVec3,
    max: DVec3,
}
impl Bounds {
    fn empty() -> Self {
        Self {
            min: DVec3::splat(f64::INFINITY),
            max: DVec3::splat(f64::NEG_INFINITY),
        }
    }
    fn add(&mut self, point: DVec3) {
        self.min = self.min.min(point);
        self.max = self.max.max(point);
    }
    fn overlaps(&self, other: &Self) -> bool {
        self.min.cmple(other.max).all() && other.min.cmple(self.max).all()
    }
    fn distance(&self, other: &Self) -> f64 {
        (self.min - other.max)
            .max(other.min - self.max)
            .max(DVec3::ZERO)
            .length()
    }
}
/// Sweep and prune on the widest axis. Keep original pair order for stable
/// sweep refinement and equal-time collision selection. Dense scenes may still
/// have quadratic output; separated scenes avoid enumerating every pair.
fn overlapping_pairs(bounds: &[Bounds]) -> Vec<(usize, usize)> {
    let mut extent = Bounds::empty();
    for bounds in bounds {
        extent.add(bounds.min);
        extent.add(bounds.max);
    }
    let span = extent.max - extent.min;
    let axis = (0..3).max_by(|&a, &b| span[a].total_cmp(&span[b])).unwrap();
    let mut order: Vec<_> = (0..bounds.len()).collect();
    order.sort_by(|&a, &b| bounds[a].min[axis].total_cmp(&bounds[b].min[axis]));
    let mut pairs = vec![];
    for (offset, &i) in order.iter().enumerate() {
        for &j in &order[offset + 1..] {
            if bounds[j].min[axis] > bounds[i].max[axis] {
                break;
            }
            if bounds[i].overlaps(&bounds[j]) {
                pairs.push((i.min(j), i.max(j)));
            }
        }
    }
    pairs.sort_unstable();
    pairs
}
/// Bound each primitive corner (or ball center) over the complete screw motion.
/// Coordinate extrema solve the sinusoid-plus-linear derivative analytically.
fn swept_bounds(shape: &SharedShape, motion: &NonlinearRigidMotion) -> Bounds {
    let mut bounds = Bounds::empty();
    let (points, radius) = if let Some(cube) = shape.as_cuboid() {
        let h = cube.half_extents;
        let mut points = vec![];
        for x in [-h.x, h.x] {
            for y in [-h.y, h.y] {
                for z in [-h.z, h.z] {
                    points.push(DVec3::new(x, y, z));
                }
            }
        }
        (points, 0.)
    } else if let Some(ball) = shape.as_ball() {
        (vec![DVec3::ZERO], ball.radius)
    } else {
        // Existing primitives are boxes/balls. This fallback is deliberately
        // unbounded so future shapes cannot be silently culled incorrectly.
        return Bounds {
            min: DVec3::splat(f64::NEG_INFINITY),
            max: DVec3::splat(f64::INFINITY),
        };
    };
    let angle = motion.angvel.length();
    let pivot = motion.start * motion.local_center;
    for point in points {
        let start = motion.start * point;
        bounds.add(start);
        bounds.add(motion.position_at_time(1.) * point);
        if angle < 1e-15 {
            continue;
        }
        let axis = motion.angvel / angle;
        let offset = start - pivot;
        let base = pivot + axis * offset.dot(axis);
        let a = offset - axis * offset.dot(axis);
        let b = axis.cross(offset);
        for component in 0..3 {
            let r = a[component].hypot(b[component]);
            if r * angle < 1e-15 {
                continue;
            }
            let target = -motion.linvel[component] / (angle * r);
            if target.abs() > 1. {
                continue;
            }
            let phase = a[component].atan2(b[component]);
            let root = target.clamp(-1., 1.).acos();
            for sign in [-1., 1.] {
                let initial = sign * root - phase;
                let first = (-initial / std::f64::consts::TAU).ceil() as i32;
                let last = ((angle - initial) / std::f64::consts::TAU).floor() as i32;
                for turn in first..=last {
                    let theta = initial + std::f64::consts::TAU * turn as f64;
                    let t = (theta / angle).clamp(0., 1.);
                    let point = base + a * theta.cos() + b * theta.sin() + motion.linvel * t;
                    bounds.add(point);
                }
            }
        }
    }
    let padding =
        radius + 1e-9 + 16. * f64::EPSILON * bounds.min.abs().max(bounds.max.abs()).max_element();
    bounds.min -= DVec3::splat(padding);
    bounds.max += DVec3::splat(padding);
    bounds
}
impl World {
    /// Every segment follows the solver's certified joint-linear continuation.
    /// Passive links are included even when they are outside the driver's tree.
    pub(crate) fn sweep_trajectory(
        &self,
        mechanism: &Mechanism,
        trajectory: &crate::constraints::Trajectory,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Result<Sweep> {
        self.validate_pose(&mechanism.evaluate()?)?;
        let mut moving = HashSet::new();
        for pair in trajectory.nodes.windows(2) {
            for (index, (a, b)) in pair[0]
                .coordinates
                .iter()
                .zip(&pair[1].coordinates)
                .enumerate()
            {
                if a != b {
                    moving.extend(mechanism.descendants(&mechanism.definition().joints[index].id)?);
                }
            }
        }
        let mut max_speed = 0.0_f64;
        for nodes in trajectory.nodes.windows(2) {
            let width = nodes[1].fraction - nodes[0].fraction;
            for piece in &self.pieces {
                if moving.contains(&piece.body) {
                    let bounds = mechanism.differential(
                        Some(&piece.body),
                        piece.local.translation,
                        &nodes[0].coordinates,
                        &nodes[1].coordinates,
                        false,
                    )?;
                    max_speed =
                        max_speed.max((bounds.speed + piece.radius * bounds.angular_speed) / width);
                }
            }
        }
        let mut result = Sweep {
            trajectory_enclosure_used: true,
            linear_fallback_used: false,
            curve_expansion_m: 0.,
            query_count: 0,
            blocked: false,
            fraction: 1.,
            instances: None,
            accuracy: "conservative_proxy".into(),
            clearance_m: self.definition.clearance_m,
            backoff_m: self.definition.backoff_m,
        };
        let mut pending = std::collections::VecDeque::from(
            trajectory
                .nodes
                .windows(2)
                .map(|nodes| (nodes[0].fraction, nodes[1].fraction))
                .collect::<Vec<_>>(),
        );
        let mut processed = 0;
        while let Some((from, to)) = pending.pop_front() {
            if cancel.is_some_and(|cancel| *cancel.borrow()) {
                return Err(Error::domain(
                    "CANCELLED",
                    "Coupled sweep cancelled; pose preserved",
                ));
            }
            processed += 1;
            if processed > 4096 {
                return Err(Error::domain(
                    "COLLISION_SWEEP_LIMIT",
                    "Coupled sweep refinement budget exhausted; pose preserved",
                ));
            }
            let qa = trajectory.sample(from);
            let qb = trajectory.sample(to);
            let a = mechanism.evaluate_at(&qa, false)?;
            let b = mechanism.evaluate_at(&qb, false)?;
            let posed = self
                .pieces
                .iter()
                .map(|piece| {
                    let start = pose(&a.poses[&piece.body]) * piece.local;
                    let end = pose(&b.poses[&piece.body]) * piece.local;
                    let differential = mechanism.differential(
                        Some(&piece.body),
                        piece.local.translation,
                        &qa,
                        &qb,
                        false,
                    )?;
                    let error = if moving.contains(&piece.body) {
                        (if differential.smooth {
                            differential.acceleration / 8.
                        } else {
                            differential.speed * 0.5
                        }) + piece.radius * differential.angular_speed.min(2.)
                    } else {
                        0.
                    };
                    let motion = NonlinearRigidMotion::new(
                        start,
                        DVec3::ZERO,
                        end.translation - start.translation,
                        DVec3::ZERO,
                    );
                    let shape = expanded(&piece.shape, error)?;
                    let bounds = swept_bounds(&shape, &motion);
                    Ok((motion, shape, bounds, error))
                })
                .collect::<Result<Vec<_>>>()?;
            let mut candidate = None::<(f64, [String; 2])>;
            let mut refine = false;
            // Report the largest enclosure used by any included moving pair,
            // including pairs certified clear by the broad phase.
            for (i, entry) in posed.iter().enumerate() {
                if self.pieces.iter().enumerate().any(|(j, other)| {
                    self.included(i, j)
                        && (moving.contains(&self.pieces[i].body) || moving.contains(&other.body))
                }) {
                    result.curve_expansion_m = result.curve_expansion_m.max(entry.3);
                }
            }
            let bounds: Vec<_> = posed.iter().map(|entry| entry.2).collect();
            'pairs: for (i, j) in overlapping_pairs(&bounds) {
                let a = &self.pieces[i];
                let b = &self.pieces[j];
                if !self.included(i, j) || !moving.contains(&a.body) && !moving.contains(&b.body) {
                    continue;
                }
                let (ma, sa, _, ea) = &posed[i];
                let (mb, sb, _, eb) = &posed[j];
                let error = ea.max(*eb);
                result.curve_expansion_m = result.curve_expansion_m.max(error);
                result.query_count += 1;
                if result.query_count > 16384 {
                    return Err(Error::domain(
                        "COLLISION_SWEEP_LIMIT",
                        "Coupled sweep query budget exhausted; pose preserved",
                    ));
                }
                let hit = query::cast_shapes(
                    &ma.start,
                    ma.linvel,
                    &**sa,
                    &mb.start,
                    mb.linvel,
                    &**sb,
                    ShapeCastOptions {
                        max_time_of_impact: 1.,
                        target_distance: 0.,
                        stop_at_penetration: error > 0.,
                        compute_impact_geometry_on_penetration: true,
                    },
                )
                .map_err(|_| {
                    Error::domain(
                        "COLLISION_QUERY_UNSUPPORTED",
                        "Unsupported coupled proxy query",
                    )
                })?;
                if let Some(hit) = hit {
                    if error > self.definition.backoff_m && to - from > 1e-12 {
                        refine = true;
                        break 'pairs;
                    }
                    if matches!(
                        hit.status,
                        ShapeCastStatus::Failed | ShapeCastStatus::OutOfIterations
                    ) || !hit.time_of_impact.is_finite()
                    {
                        return Err(Error::domain(
                            "COLLISION_QUERY_FAILED",
                            "Coupled sweep did not converge; pose preserved",
                        ));
                    }
                    let fraction = from + (to - from) * hit.time_of_impact.clamp(0., 1.);
                    if candidate.as_ref().is_none_or(|(old, _)| fraction < *old) {
                        candidate = Some((fraction, [a.body.clone(), b.body.clone()]));
                    }
                }
            }
            if refine {
                let middle = (from + to) * 0.5;
                pending.push_front((middle, to));
                pending.push_front((from, middle));
                continue;
            }
            if let Some((fraction, pair)) = candidate {
                result.blocked = true;
                result.fraction = (fraction
                    - self.definition.backoff_m / (2. * max_speed).max(1e-15))
                .clamp(0., 1.);
                result.instances = Some(pair);
                break;
            }
        }
        let accepted = mechanism.at_coordinates(&trajectory.sample(result.fraction))?;
        self.validate_pose(&accepted.evaluate()?)?;
        Ok(result)
    }
}

#[cfg(test)]
mod swept_bounds_tests {
    use super::*;
    #[test]
    fn linear_fallback_envelope_contains_rotating_primitive_trajectories() {
        let cube = SharedShape::cuboid(0.004, 0.002, 0.001);
        let start = Pose {
            translation: DVec3::new(0.03, 0.01, -0.02),
            rotation: DQuat::from_rotation_y(0.4),
        };
        for rotation in [
            DVec3::new(1e-5, 0., 0.),
            DVec3::new(0.3, 0.7, -0.2),
            DVec3::Z * std::f64::consts::TAU,
        ] {
            let motion = NonlinearRigidMotion::new(
                start,
                DVec3::new(-0.05, 0.02, 0.),
                DVec3::new(0.03, -0.01, 0.02),
                rotation,
            );
            let (linear, envelope, _) = linear_envelope(&cube, &motion).unwrap();
            let half = envelope.as_cuboid().unwrap().half_extents;
            for step in 0..=100 {
                let t = step as f64 / 100.;
                let actual = motion.position_at_time(t);
                let approximate = linear.position_at_time(t);
                for x in [-0.004, 0.004] {
                    for y in [-0.002, 0.002] {
                        for z in [-0.001, 0.001] {
                            let point =
                                approximate.inverse_transform_point(actual * DVec3::new(x, y, z));
                            assert!(
                                point.abs().cmple(half + DVec3::splat(1e-14)).all(),
                                "{point:?} {half:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn bounds_include_intermediate_rotational_extrema_and_screw_trajectories() {
        let shape = SharedShape::ball(0.001);
        let angle = std::f64::consts::FRAC_PI_4;
        let start = Pose {
            translation: DVec3::new(0.05 * angle.cos(), -0.05 * angle.sin(), 0.),
            rotation: DQuat::IDENTITY,
        };
        let motion = NonlinearRigidMotion::new(
            start,
            start.inverse_transform_point(DVec3::ZERO),
            DVec3::ZERO,
            DVec3::Z * std::f64::consts::FRAC_PI_2,
        );
        let bounds = swept_bounds(&shape, &motion);
        assert!((bounds.max.x - 0.051).abs() < 1e-8);
        let cube = SharedShape::cuboid(0.002, 0.004, 0.001);
        for rotation in [
            DVec3::new(0.3, 0.7, -0.2),
            DVec3::new(-1.1, 0., 0.2),
            DVec3::ZERO,
        ] {
            let motion = NonlinearRigidMotion::new(
                start,
                DVec3::new(0.01, -0.02, 0.03),
                DVec3::new(0.02, -0.01, 0.015),
                rotation,
            );
            let bounds = swept_bounds(&cube, &motion);
            for i in 0..=100 {
                let p = motion.position_at_time(i as f64 / 100.);
                let actual = cube.compute_aabb(&p);
                assert!(
                    bounds.min.cmple(actual.mins).all() && bounds.max.cmpge(actual.maxs).all(),
                    "{bounds:?} {actual:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod optimization_tests {
    use super::*;
    use serde_json::json;

    fn scene(count: usize, spacing: f64) -> (World, Evaluation) {
        let names: Vec<_> = (0..count).map(|i| format!("body{i:02}")).collect();
        let definition = Definition {
            bodies: names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    (
                        name.clone(),
                        vec![Primitive {
                            frame: Frame::default(),
                            shape: if i % 2 == 0 {
                                Shape::Box {
                                    half_extents_m: [0.004, 0.002, 0.003],
                                }
                            } else {
                                Shape::Ball { radius_m: 0.003 }
                            },
                        }],
                    )
                })
                .collect(),
            clearance_m: 0.00002,
            backoff_m: 0.000001,
            ignored_pairs: vec![[names[0].clone(), names[1].clone()]],
        };
        let model = json!({
            "parts": names.iter().map(|name| json!({"instance_id":name})).collect::<Vec<_>>(),
            "collision_validation": {"method":"brep_boolean_coverage", "covered_instances":names},
        });
        let world = World::new(definition, &model).unwrap();
        let evaluation = Evaluation {
            poses: names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    (
                        name.clone(),
                        CadPose {
                            position: [i as f64 * spacing, (i as f64 * 0.7).sin(), 0.],
                            quaternion: DQuat::from_rotation_z(i as f64 * 0.23).to_array(),
                        },
                    )
                })
                .collect(),
            joints: vec![],
            constraints: None,
        };
        (world, evaluation)
    }

    fn exhaustive(
        world: &World,
        evaluation: &Evaluation,
        wanted: Option<&HashSet<(String, String)>>,
    ) -> (Option<f64>, BTreeMap<(String, String), f64>) {
        let mut minimum = None::<f64>;
        let mut contacts = BTreeMap::new();
        for (i, a) in world.pieces.iter().enumerate() {
            for (j, b) in world.pieces.iter().enumerate().skip(i + 1) {
                let key = pair(&a.body, &b.body);
                if !world.included(i, j) || wanted.is_some_and(|wanted| !wanted.contains(&key)) {
                    continue;
                }
                let pa = pose(&evaluation.poses[&a.body]) * a.local;
                let pb = pose(&evaluation.poses[&b.body]) * b.local;
                let distance = separation(&pa, &a.shape, &pb, &b.shape).unwrap();
                minimum = Some(minimum.map_or(distance, |old| old.min(distance)));
                if distance <= 1e-9 {
                    contacts
                        .entry(key)
                        .and_modify(|v: &mut f64| *v = v.min(distance))
                        .or_insert(distance);
                }
            }
        }
        (minimum, contacts)
    }

    #[test]
    fn pruned_reports_and_validation_match_exhaustive_geometry() {
        for spacing in [0., 1., 5., 8., 10., 100.] {
            let (mut world, evaluation) = scene(24, spacing);
            // Exercise multiple primitives per body as well as body exclusions.
            world
                .definition
                .bodies
                .get_mut("body05")
                .unwrap()
                .push(Primitive {
                    frame: Frame {
                        position_m: [0.003, 0., 0.],
                        ..Frame::default()
                    },
                    shape: Shape::Ball { radius_m: 0.001 },
                });
            let names: Vec<_> = world.definition.bodies.keys().cloned().collect();
            let model = json!({"parts":names.iter().map(|n| json!({"instance_id":n})).collect::<Vec<_>>(),
                "collision_validation":{"method":"brep_boolean_coverage","covered_instances":names}});
            let world = World::new(world.definition, &model).unwrap();
            let wanted = HashSet::from([
                pair("body02", "body05"),
                pair("body00", "body01"),
                pair("body08", "body14"),
            ]);
            for filter in [None, Some(&wanted)] {
                let (minimum, contacts) = exhaustive(&world, &evaluation, filter);
                let report = world.report_impl(&evaluation, filter).unwrap();
                assert!(
                    (minimum.unwrap() - report.minimum_expanded_proxy_separation_m.unwrap()).abs()
                        < 1e-12
                );
                let actual: BTreeMap<_, _> = report
                    .contacts
                    .into_iter()
                    .map(|c| {
                        (
                            (c.instances[0].clone(), c.instances[1].clone()),
                            c.expanded_proxy_separation_m,
                        )
                    })
                    .collect();
                assert_eq!(contacts.len(), actual.len());
                for (key, distance) in contacts {
                    assert!((actual[&key] - distance).abs() < 1e-12);
                }
                let expected_checked: Vec<_> = world
                    .body_pairs
                    .keys()
                    .filter(|p| filter.is_none_or(|f| f.contains(*p)))
                    .map(|(a, b)| [a.clone(), b.clone()])
                    .collect();
                assert_eq!(report.checked_pairs, expected_checked);
            }
            let (_, contacts) = exhaustive(&world, &evaluation, None);
            assert_eq!(
                world.validate_pose(&evaluation).is_ok(),
                !contacts.values().any(|d| *d < -1e-9)
            );
        }
    }

    #[test]
    fn broad_phase_matches_all_pairs_including_touching_and_unbounded_boxes() {
        let mut bounds: Vec<_> = (0..128)
            .map(|i| {
                let center = DVec3::new(
                    (i as f64 * 1.37).sin(),
                    (i as f64 * 2.19).cos(),
                    (i as f64 * 0.41).sin(),
                );
                let half = DVec3::splat((i % 7) as f64 * 0.04);
                Bounds {
                    min: center - half,
                    max: center + half,
                }
            })
            .collect();
        bounds.push(Bounds {
            min: DVec3::ZERO,
            max: DVec3::ONE,
        });
        bounds.push(Bounds {
            min: DVec3::ONE,
            max: DVec3::splat(2.),
        });
        bounds.push(Bounds {
            min: DVec3::splat(f64::NEG_INFINITY),
            max: DVec3::splat(f64::INFINITY),
        });
        let mut expected = vec![];
        for i in 0..bounds.len() {
            for j in i + 1..bounds.len() {
                if bounds[i].overlaps(&bounds[j]) {
                    expected.push((i, j));
                }
            }
        }
        assert_eq!(overlapping_pairs(&bounds), expected);
        assert!(overlapping_pairs(&[]).is_empty());
        let sparse: Vec<_> = (0..128)
            .map(|i| Bounds {
                min: DVec3::splat(i as f64 * 10.),
                max: DVec3::splat(i as f64 * 10. + 1.),
            })
            .collect();
        assert!(overlapping_pairs(&sparse).is_empty());
    }

    #[test]
    #[ignore = "manual performance comparison; cargo test -p cadpad-core --release collision_query_benchmark -- --ignored --nocapture"]
    fn collision_query_benchmark() {
        use std::{hint::black_box, time::Instant};
        let (world, evaluation) = scene(64, 20.);
        let rounds = 100;
        let started = Instant::now();
        for _ in 0..rounds {
            let (minimum, contacts) = exhaustive(&world, &evaluation, None);
            black_box(Report {
                accuracy: "conservative_proxy".into(),
                clearance_m: world.definition.clearance_m,
                backoff_m: world.definition.backoff_m,
                coverage_method: "brep_boolean_coverage".into(),
                ignored_pairs: world.definition.ignored_pairs.clone(),
                checked_pairs: world
                    .body_pairs
                    .keys()
                    .map(|(a, b)| [a.clone(), b.clone()])
                    .collect(),
                contacts: contacts
                    .into_iter()
                    .map(|((a, b), distance)| Contact {
                        instances: [a, b],
                        expanded_proxy_separation_m: distance,
                    })
                    .collect(),
                minimum_expanded_proxy_separation_m: minimum,
            });
        }
        let baseline = started.elapsed();
        let started = Instant::now();
        for _ in 0..rounds {
            black_box(world.report(&evaluation).unwrap());
        }
        let report = started.elapsed();
        let started = Instant::now();
        for _ in 0..rounds {
            black_box(world.validate_pose(&evaluation).unwrap());
        }
        let validation = started.elapsed();
        eprintln!(
            "64 bodies, {rounds} rounds: exhaustive={baseline:?}, report={report:?}, pose_validation={validation:?}"
        );
    }
}
