//! Exact arc-length lines/arcs and rotation-minimizing frames. Closed tracks
//! distribute transport holonomy as roll, so the seam has one continuous frame.
use crate::{Error, Result};
use glam::{DAffine3, DMat3, DQuat, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_PATH", message)
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Segment {
    Line {
        end_m: [f64; 3],
    },
    Arc {
        center_m: [f64; 3],
        axis: [f64; 3],
        angle_rad: f64,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub start_m: [f64; 3],
    pub up: [f64; 3],
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub closed: bool,
}
#[derive(Clone)]
enum Curve {
    Line {
        tangent: DVec3,
    },
    Arc {
        center: DVec3,
        axis: DVec3,
        angle: f64,
        radius: f64,
    },
}
#[derive(Clone)]
struct Piece {
    start: DVec3,
    curve: Curve,
    length: f64,
    offset: f64,
    frame: DQuat,
}
#[derive(Clone)]
pub struct Path {
    pieces: Vec<Piece>,
    pub length: f64,
    pub closed: bool,
    roll: f64,
    samples: Vec<Sample>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Sample {
    pub coordinate_m: f64,
    pub position_mm: [f64; 3],
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct State {
    pub length_m: f64,
    pub closed: bool,
    pub frame_rule: String,
    pub samples: Vec<Sample>,
}
/// Exact base screw motion plus a bounded omitted roll on curved spans.
#[derive(Clone, Debug)]
pub struct Span {
    pub from: f64,
    pub to: f64,
    pub translation_m: DVec3,
    pub rotation_vector: DVec3,
    pub pivot_m: DVec3,
    pub path_point_m: DVec3,
    pub bend_radius_m: f64,
    pub omitted_roll: f64,
}
impl Span {
    pub fn split(&self) -> (Self, Self) {
        let mut first = self.clone();
        let mut second = self.clone();
        let mid = (self.from + self.to) * 0.5;
        first.to = mid;
        second.from = mid;
        for span in [&mut first, &mut second] {
            span.translation_m *= 0.5;
            span.rotation_vector *= 0.5;
            span.omitted_roll *= 0.5;
        }
        let rotation = DQuat::from_scaled_axis(first.rotation_vector);
        second.path_point_m =
            rotation * (self.path_point_m - self.pivot_m) + self.pivot_m + first.translation_m;
        // Linear screw pivots travel with their translation. Arc pivots are fixed.
        if self.bend_radius_m == 0. {
            second.pivot_m += first.translation_m;
        }
        (first, second)
    }
    pub fn world(&self, frame: DAffine3) -> Self {
        Self {
            from: self.from,
            to: self.to,
            translation_m: frame.transform_vector3(self.translation_m),
            rotation_vector: frame.transform_vector3(self.rotation_vector),
            pivot_m: frame.transform_point3(self.pivot_m),
            path_point_m: frame.transform_point3(self.path_point_m),
            bend_radius_m: self.bend_radius_m,
            omitted_roll: self.omitted_roll,
        }
    }
}
fn point(value: [f64; 3]) -> Result<DVec3> {
    let point = DVec3::from_array(value);
    if !point.is_finite() || point.abs().max_element() > 1e6 {
        return Err(invalid("Path points must be finite metres within 1e6 m"));
    }
    Ok(point)
}
impl Path {
    pub fn compile(definition: &Definition) -> Result<Self> {
        if definition.segments.is_empty() || definition.segments.len() > 256 {
            return Err(invalid("A path needs 1..256 line/arc segments"));
        }
        let start = point(definition.start_m)?;
        let mut cursor = start;
        let mut pieces = vec![];
        let mut offset = 0.;
        let mut previous_tangent = None::<DVec3>;
        let mut frame = DQuat::IDENTITY;
        let up = DVec3::from_array(definition.up);
        if !up.is_finite() || (up.length_squared() - 1.).abs() > 1e-9 {
            return Err(invalid("Path up must be a finite unit direction"));
        }
        for segment in &definition.segments {
            let (curve, length, tangent, end, end_tangent, bend) = match segment {
                Segment::Line { end_m } => {
                    let end = point(*end_m)?;
                    let direction = end - cursor;
                    let length = direction.length();
                    if length < 1e-9 {
                        return Err(invalid("Path lines must have positive length above 1e-9 m"));
                    }
                    let tangent = direction / length;
                    (
                        Curve::Line { tangent },
                        length,
                        tangent,
                        end,
                        tangent,
                        DQuat::IDENTITY,
                    )
                }
                Segment::Arc {
                    center_m,
                    axis,
                    angle_rad,
                } => {
                    let center = point(*center_m)?;
                    let axis = DVec3::from_array(*axis);
                    if !axis.is_finite()
                        || (axis.length_squared() - 1.).abs() > 1e-9
                        || !angle_rad.is_finite()
                        || angle_rad.abs() < 1e-9
                        || angle_rad.abs() > std::f64::consts::TAU * 8.
                    {
                        return Err(invalid(
                            "Arcs need a unit axis and nonzero angle of at most eight turns",
                        ));
                    }
                    let axis = axis.normalize();
                    let radial = cursor - center;
                    let radius = radial.length();
                    if radius < 1e-9 || radial.dot(axis).abs() > 1e-9 * radius {
                        return Err(invalid(
                            "Arc start must lie in its axis-normal plane at a positive radius",
                        ));
                    }
                    let tangent = axis.cross(radial).normalize() * angle_rad.signum();
                    let bend = DQuat::from_axis_angle(axis, *angle_rad);
                    let end = center + bend * radial;
                    (
                        Curve::Arc {
                            center,
                            axis,
                            angle: *angle_rad,
                            radius,
                        },
                        radius * angle_rad.abs(),
                        tangent,
                        end,
                        bend * tangent,
                        bend,
                    )
                }
            };
            if let Some(previous) = previous_tangent {
                if previous.distance(tangent) > 1e-9 {
                    return Err(invalid(
                        "Path segments need continuous tangents; replace sharp corners with explicit arcs",
                    ));
                }
            } else {
                let normal = up - tangent * up.dot(tangent);
                if normal.length_squared() < 1e-12 {
                    return Err(invalid("Path up cannot be parallel to its initial tangent"));
                }
                let z = normal.normalize();
                let y = z.cross(tangent).normalize();
                frame =
                    DQuat::from_mat3(&DMat3::from_cols(tangent, y, tangent.cross(y))).normalize();
            }
            pieces.push(Piece {
                start: cursor,
                curve,
                length,
                offset,
                frame,
            });
            cursor = end;
            offset += length;
            frame = (bend * frame).normalize();
            previous_tangent = Some(end_tangent);
        }
        if !offset.is_finite() || offset > 1e6 {
            return Err(invalid("Path length must be finite and at most 1e6 m"));
        }
        let mut roll = 0.;
        if definition.closed {
            let first = &pieces[0];
            let initial_tangent = first.frame * DVec3::X;
            if cursor.distance(start) > 1e-9
                || previous_tangent.unwrap().distance(initial_tangent) > 1e-9
            {
                return Err(invalid(
                    "Closed paths must return to the initial point and tangent",
                ));
            }
            let end_up = frame * DVec3::Z;
            let start_up = first.frame * DVec3::Z;
            roll = initial_tangent
                .dot(end_up.cross(start_up))
                .atan2(end_up.dot(start_up));
        }
        let mut path = Self {
            pieces,
            length: offset,
            closed: definition.closed,
            roll,
            samples: vec![],
        };
        // Interaction samples are explicitly approximate. Exact poses and sweep
        // boundaries always use analytic segment geometry, never this polyline.
        let mut coordinates = (0..=128)
            .map(|i| offset * i as f64 / 128.)
            .collect::<Vec<_>>();
        coordinates.extend(path.pieces.iter().map(|piece| piece.offset));
        coordinates.push(offset);
        coordinates.sort_by(f64::total_cmp);
        coordinates.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
        path.samples = coordinates
            .into_iter()
            .map(|coordinate| Sample {
                coordinate_m: coordinate,
                position_mm: (path.pose(coordinate).translation * 1000.).to_array(),
            })
            .collect();
        Ok(path)
    }
    fn index(&self, s: f64) -> usize {
        self.pieces
            .partition_point(|piece| piece.offset + piece.length <= s)
            .min(self.pieces.len() - 1)
    }
    pub fn pose(&self, s: f64) -> DAffine3 {
        let s = s.clamp(0., self.length);
        let piece = &self.pieces[self.index(s)];
        let distance = (s - piece.offset).clamp(0., piece.length);
        let (position, transport) = match piece.curve {
            Curve::Line { tangent } => (piece.start + tangent * distance, piece.frame),
            Curve::Arc {
                center,
                axis,
                angle,
                radius,
            } => {
                let rotation = DQuat::from_axis_angle(axis, angle.signum() * distance / radius);
                (
                    center + rotation * (piece.start - center),
                    rotation * piece.frame,
                )
            }
        };
        DAffine3::from_rotation_translation(
            (transport * DQuat::from_rotation_x(self.roll * s / self.length)).normalize(),
            position,
        )
    }
    /// Unit tangent and angular rate per metre, in path coordinates.
    /// Open tracks are analytic. Closed-seam roll uses a centered difference.
    pub fn rate(&self, s: f64) -> (DVec3, DVec3) {
        let s = s.clamp(0., self.length);
        if self.roll != 0. {
            return self.numeric_rate(s);
        }
        let pose = self.pose(s);
        let (_, rotation, _) = pose.to_scale_rotation_translation();
        let tangent = rotation * DVec3::X;
        let piece = &self.pieces[self.index(s)];
        let omega = match piece.curve {
            Curve::Line { .. } => DVec3::ZERO,
            Curve::Arc {
                axis,
                angle,
                radius,
                ..
            } => axis * (angle.signum() / radius),
        };
        (tangent, omega)
    }
    fn numeric_rate(&self, s: f64) -> (DVec3, DVec3) {
        let eps = 1e-5;
        let s0 = (s - eps).clamp(0., self.length);
        let s1 = (s + eps).clamp(0., self.length);
        let ds = (s1 - s0).max(1e-12);
        let a = self.pose(s0);
        let b = self.pose(s1);
        let (_, ra, ta) = a.to_scale_rotation_translation();
        let (_, rb, tb) = b.to_scale_rotation_translation();
        let delta = (rb * ra.inverse()).normalize();
        let imag = delta.xyz();
        let angle = 2. * imag.length().atan2(delta.w);
        let omega = if imag.length_squared() < 1e-24 {
            DVec3::ZERO
        } else {
            imag.normalize() * (angle / ds)
        };
        ((tb - ta) / ds, omega)
    }
    pub fn state(&self, frame: DAffine3) -> State {
        State {
            length_m: self.length,
            closed: self.closed,
            frame_rule: "rotation_minimizing_with_closed_seam_roll".into(),
            samples: self
                .samples
                .iter()
                .map(|sample| Sample {
                    coordinate_m: sample.coordinate_m,
                    position_mm: (frame
                        .transform_point3(DVec3::from_array(sample.position_mm) * 0.001)
                        * 1000.)
                        .to_array(),
                })
                .collect(),
        }
    }
    pub fn spans(&self, start: f64, target: f64) -> Result<Vec<Span>> {
        if start == target {
            return Ok(vec![]);
        }
        let delta = target - start;
        let low = start.min(target);
        let high = start.max(target);
        let mut cuts = vec![start, target];
        for piece in &self.pieces {
            if piece.offset > low && piece.offset < high {
                cuts.push(piece.offset);
            }
        }
        cuts.sort_by(f64::total_cmp);
        if delta < 0. {
            cuts.reverse();
        }
        let mut spans = vec![];
        for pair in cuts.windows(2) {
            let a = pair[0];
            let b = pair[1];
            let piece = &self.pieces[self.index((a + b) * 0.5)];
            let steps = match piece.curve {
                Curve::Arc { radius, .. } => ((b - a).abs() / radius / std::f64::consts::FRAC_PI_8)
                    .ceil()
                    .max(1.) as usize,
                _ => 1,
            };
            if spans.len() + steps > 2048 {
                return Err(Error::domain(
                    "COLLISION_SWEEP_LIMIT",
                    "Path target exceeds the bounded analytic sweep span budget",
                ));
            }
            for step in 0..steps {
                let from = a + (b - a) * step as f64 / steps as f64;
                let to = a + (b - a) * (step + 1) as f64 / steps as f64;
                let ds = to - from;
                let path_point = self.pose(from).translation;
                let (translation, rotation, pivot, radius, omitted) = match piece.curve {
                    Curve::Line { tangent } => (
                        tangent * ds,
                        (piece.frame * DVec3::X) * (self.roll * ds / self.length),
                        path_point,
                        0.,
                        0.,
                    ),
                    Curve::Arc {
                        center,
                        axis,
                        angle,
                        radius,
                    } => (
                        DVec3::ZERO,
                        axis * (angle.signum() * ds / radius),
                        center,
                        radius,
                        (self.roll * ds / self.length).abs(),
                    ),
                };
                spans.push(Span {
                    from: (from - start) / delta,
                    to: (to - start) / delta,
                    translation_m: translation,
                    rotation_vector: rotation,
                    pivot_m: pivot,
                    path_point_m: path_point,
                    bend_radius_m: radius,
                    omitted_roll: omitted,
                });
            }
        }
        Ok(spans)
    }
}

impl Path {
    pub(crate) fn differential(&self, a: f64, b: f64) -> (f64, f64, f64, f64, bool) {
        let low = a.min(b);
        let high = a.max(b);
        let ds = (b - a).abs();
        let mut curvature = 0.0_f64;
        let mut smooth = true;
        for piece in &self.pieces {
            if piece.offset > low && piece.offset < high {
                smooth = false;
            }
            if piece.offset <= high
                && piece.offset + piece.length >= low
                && let Curve::Arc { radius, .. } = piece.curve
            {
                curvature = curvature.max(1. / radius);
            }
        }
        let roll = self.roll.abs() / self.length;
        (
            self.pose(a).translation.length() + ds,
            ds * (curvature + roll),
            ds * ds * curvature * roll,
            ds * ds * curvature,
            smooth,
        )
    }
}
