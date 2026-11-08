use cadpad_core::paths::{Definition, Path, Segment};
use glam::{DQuat, DVec3};
fn mixed() -> Definition {
    Definition {
        start_m: [0.; 3],
        up: [0., 0., 1.],
        closed: false,
        segments: vec![
            Segment::Line {
                end_m: [0.1, 0., 0.],
            },
            Segment::Arc {
                center_m: [0.1, 0.05, 0.],
                axis: [0., 0., 1.],
                angle_rad: std::f64::consts::FRAC_PI_2,
            },
            Segment::Line {
                end_m: [0.15, 0.15, 0.],
            },
        ],
    }
}
fn near(a: DVec3, b: DVec3) {
    assert!(a.distance(b) < 1e-10, "{a:?} != {b:?}");
}
#[test]
fn exact_arc_length_and_transported_frames_stay_stable_across_straight_sections() {
    let path = Path::compile(&mixed()).unwrap();
    assert!((path.length - (0.2 + std::f64::consts::FRAC_PI_2 * 0.05)).abs() < 1e-14);
    near(path.pose(0.04).translation, DVec3::new(0.04, 0., 0.));
    near(path.pose(0.04).transform_vector3(DVec3::Z), DVec3::Z);
    let mid = path.pose(0.1 + std::f64::consts::FRAC_PI_4 * 0.05);
    near(
        mid.translation,
        DVec3::new(
            0.1 + 0.05 * std::f64::consts::FRAC_1_SQRT_2,
            0.05 - 0.05 * std::f64::consts::FRAC_1_SQRT_2,
            0.,
        ),
    );
    near(
        mid.transform_vector3(DVec3::X),
        DVec3::new(
            std::f64::consts::FRAC_1_SQRT_2,
            std::f64::consts::FRAC_1_SQRT_2,
            0.,
        ),
    );
    let last = path.pose(path.length);
    near(last.translation, DVec3::new(0.15, 0.15, 0.));
    near(last.transform_vector3(DVec3::X), DVec3::Y);
    near(last.transform_vector3(DVec3::Z), DVec3::Z);
}
#[test]
fn analytic_spans_match_forward_and_reverse_path_motion_without_chord_substitution() {
    let path = Path::compile(&mixed()).unwrap();
    for (start, target) in [(0., path.length), (path.length, 0.)] {
        let spans = path.spans(start, target).unwrap();
        assert!(spans.len() > 2);
        let mut previous = 0.;
        for span in spans {
            assert!((span.from - previous).abs() < 1e-12);
            previous = span.to;
            let pose = path.pose(start + (target - start) * span.from);
            let point = pose.translation;
            let expected = path.pose(start + (target - start) * span.to);
            let rotation = DQuat::from_scaled_axis(span.rotation_vector);
            near(
                rotation * (point - span.pivot_m) + span.pivot_m + span.translation_m,
                expected.translation,
            );
            near(
                rotation * pose.transform_vector3(DVec3::X),
                expected.transform_vector3(DVec3::X),
            );
            assert_eq!(span.omitted_roll, 0.);
        }
        assert!((previous - 1.).abs() < 1e-12);
    }
}
/// A spatial filleted polygon has smooth tangents but nontrivial transport
/// holonomy. It exercises the closed-frame correction beyond a planar circle.
pub fn spatial_closed() -> Definition {
    let vertices = [
        DVec3::ZERO,
        DVec3::new(0.11, 0., 0.01),
        DVec3::new(0.13, 0.09, 0.05),
        DVec3::new(0.04, 0.14, -0.02),
        DVec3::new(-0.03, 0.06, 0.03),
    ];
    let radius = 0.005;
    let mut corners = vec![];
    for i in 0..vertices.len() {
        let vertex = vertices[i];
        let incoming = (vertex - vertices[(i + vertices.len() - 1) % vertices.len()]).normalize();
        let outgoing = (vertices[(i + 1) % vertices.len()] - vertex).normalize();
        let angle = incoming.dot(outgoing).acos();
        let distance = radius * (angle * 0.5).tan();
        let before = vertex - incoming * distance;
        let after = vertex + outgoing * distance;
        let axis = incoming.cross(outgoing).normalize();
        let center = before + axis.cross(incoming) * radius;
        corners.push((before, after, center, axis, angle));
    }
    let mut segments = vec![];
    for i in 1..=corners.len() {
        let (before, _, center, axis, angle) = corners[i % corners.len()];
        segments.push(Segment::Line {
            end_m: before.to_array(),
        });
        segments.push(Segment::Arc {
            center_m: center.to_array(),
            axis: axis.to_array(),
            angle_rad: angle,
        });
    }
    Definition {
        start_m: corners[0].1.to_array(),
        up: [0., 0., 1.],
        segments,
        closed: true,
    }
}
#[test]
fn closed_spatial_track_has_continuous_orientation_and_a_bounded_roll_sweep() {
    let path = Path::compile(&spatial_closed()).unwrap();
    let first = path.pose(0.);
    let last = path.pose(path.length);
    near(first.translation, last.translation);
    for axis in [DVec3::X, DVec3::Y, DVec3::Z] {
        near(first.transform_vector3(axis), last.transform_vector3(axis));
    }
    let spans = path.spans(0., path.length).unwrap();
    assert!(spans.iter().any(|span| span.omitted_roll > 1e-8));
    let near_seam = path.pose(path.length - 1e-8);
    assert!(
        near_seam
            .transform_vector3(DVec3::Z)
            .distance(first.transform_vector3(DVec3::Z))
            < 1e-4
    );
}
#[test]
fn sharp_corners_invalid_arcs_up_vectors_and_open_seams_are_rejected() {
    let mut value = mixed();
    value.up = [1., 0., 0.];
    assert!(Path::compile(&value).is_err());
    let mut value = mixed();
    value.segments[0] = Segment::Line {
        end_m: [0., 0.1, 0.],
    };
    assert!(Path::compile(&value).is_err());
    let mut value = mixed();
    value.closed = true;
    assert!(Path::compile(&value).is_err());
    let mut value = mixed();
    value.segments[1] = Segment::Arc {
        center_m: [0.1, 0.05, 0.01],
        axis: [0., 0., 1.],
        angle_rad: 1.,
    };
    assert!(Path::compile(&value).is_err());
    let mut value = mixed();
    value.segments[1] = Segment::Arc {
        center_m: [0.1, 0.05, 0.],
        axis: [0., 0., 2.],
        angle_rad: 1.,
    };
    assert!(Path::compile(&value).is_err());
    let mut value = mixed();
    value.segments.clear();
    assert!(Path::compile(&value).is_err());
}

#[test]
fn spatial_closed_roll_is_swept_conservatively_instead_of_interpolating_identical_endpoint_poses() {
    use cadpad_core::{collision::World, kinematics};
    use serde_json::json;
    let definition = spatial_closed();
    let path = Path::compile(&definition).unwrap();
    let stop = path
        .pose(path.length * 0.5)
        .transform_point3(DVec3::new(0.015, 0., 0.))
        * 1000.;
    let collision = json!({"bodies":{"carriage":[{"frame":{"position_m":[0.015,0,0]},"shape":{"type":"box","half_extents_m":[0.002,0.001,0.001]}}],"stop":[{"shape":{"type":"ball","radius_m":0.001}}]},"clearance_m":0.00001,"backoff_m":0.000001});
    let model = json!({"parts":[{"instance_id":"carriage","position":[0,0,0]},{"instance_id":"stop","position":stop.to_array()}],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["carriage","stop"]}});
    let definition=kinematics::decode(&json!({"joints":[{"id":"track","parent":null,"child":"carriage","kind":{"type":"path","path":definition},"initial":0,"limits":[0,path.length]}],"collision":collision})).unwrap();
    let mechanism = kinematics::Mechanism::new(definition.clone(), &model["parts"]).unwrap();
    let world = World::new(definition.collision.unwrap(), &model).unwrap();
    let hit = world.sweep(&mechanism, "track", path.length).unwrap();
    assert!(hit.blocked, "{hit:?}");
    assert!(hit.fraction > 0.25 && hit.fraction < 0.6, "{hit:?}");
    assert!(hit.curve_expansion_m > 0.);
    assert!(hit.query_count > 0);
    let mut accepted = mechanism.clone();
    accepted
        .set("track", path.length * hit.fraction, "m")
        .unwrap();
    world.validate_pose(&accepted.evaluate().unwrap()).unwrap();
}
