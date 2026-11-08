use cadpad_core::kinematics::{self, Mechanism};
use serde_json::{Value, json};
use std::f64::consts::FRAC_PI_2;
fn parts() -> Value {
    json!([{"instance_id":"rail","position":[10,20,30]},{"instance_id":"carriage","position":[0,0,0]},{"instance_id":"arm","position":[0,0,0]}])
}
fn definition() -> Value {
    json!({"schema_version":1,"joints":[
     {"id":"hinge","parent":"carriage","child":"arm","kind":{"type":"revolute","axis":[0,0,1]},"parent_frame":{"position_m":[0.01,0,0]},"child_frame":{"position_m":[-0.01,0,0]},"initial":0,"limits":[-3.2,3.2]},
     {"id":"slide","parent":"rail","child":"carriage","kind":{"type":"prismatic","axis":[1,0,0]},"parent_frame":{"position_m":[0,0,0.013]},"initial":0.02,"limits":[-0.08,0.08]}
    ]})
}
fn near(actual: &[f64], expected: &[f64]) {
    assert_eq!(actual.len(), expected.len());
    for (a, b) in actual.iter().zip(expected) {
        assert!((a - b).abs() < 1e-10, "{actual:?} != {expected:?}");
    }
}
#[test]
fn nested_frames_unit_conversion_rotation_and_downstream_motion_are_exact() {
    let mut mechanism =
        Mechanism::new(kinematics::decode(&definition()).unwrap(), &parts()).unwrap();
    let first = mechanism.evaluate().unwrap();
    near(&first.poses["carriage"].position, &[30., 20., 43.]);
    near(&first.poses["arm"].position, &[50., 20., 43.]);
    assert_eq!(first.joints[0].id, "slide");
    assert_eq!(first.joints[0].unit, "m");
    assert_eq!(first.joints[1].unit, "rad");
    mechanism.set("hinge", FRAC_PI_2, "rad").unwrap();
    let second = mechanism.evaluate().unwrap();
    near(&second.poses["arm"].position, &[40., 30., 43.]);
    near(
        &second.poses["arm"].quaternion,
        &[0., 0., (0.5_f64).sqrt(), (0.5_f64).sqrt()],
    );
    let (applied, clamped) = mechanism.set("slide", 1., "m").unwrap();
    assert!(clamped);
    assert_eq!(applied, 0.08);
    let third = mechanism.evaluate().unwrap();
    near(&third.poses["carriage"].position, &[90., 20., 43.]);
    near(&third.poses["arm"].position, &[100., 30., 43.]);
    assert!(mechanism.set("slide", 10., "mm").is_err());
    assert!(mechanism.set("slide", f64::NAN, "m").is_err());
    near(
        &mechanism.evaluate().unwrap().poses["arm"].position,
        &third.poses["arm"].position,
    );
}
#[test]
fn fixed_world_frames_and_joint_frame_rotation_use_xyzw_right_handed_axes() {
    let s = (0.5_f64).sqrt();
    let value = json!({"joints":[{"id":"slide","parent":null,"child":"carriage","kind":{"type":"prismatic","axis":[1,0,0]},"parent_frame":{"position_m":[0.1,0,0],"quaternion":[0,0,s,s]},"initial":0.02,"limits":[0,0.04]}, {"id":"weld","parent":"carriage","child":"arm","kind":{"type":"fixed"},"parent_frame":{"position_m":[0.01,0,0]}}]});
    let mut mechanism = Mechanism::new(kinematics::decode(&value).unwrap(), &parts()).unwrap();
    let state = mechanism.evaluate().unwrap();
    near(&state.poses["carriage"].position, &[100., 20., 0.]);
    near(&state.poses["arm"].position, &[100., 30., 0.]);
    near(&state.joints[0].axis_world, &[0., 1., 0.]);
    assert!(mechanism.set("weld", 0., "m").is_err());
}
#[test]
fn invalid_tree_axes_frames_limits_and_missing_instances_are_rejected() {
    let mut value = definition();
    value["joints"][1]["parent"] = json!("arm");
    assert!(kinematics::decode(&value).is_err());
    let mut value = definition();
    value["joints"][0]["child"] = json!("carriage");
    assert!(kinematics::decode(&value).is_err());
    let mut value = definition();
    value["joints"][0]["id"] = json!("slide");
    assert!(kinematics::decode(&value).is_err());
    for (field, data) in [("axis", json!([0, 0, 0])), ("axis", json!([2, 0, 0]))] {
        let mut value = definition();
        value["joints"][0]["kind"][field] = data;
        assert!(kinematics::decode(&value).is_err());
    }
    let mut value = definition();
    value["joints"][0]["parent_frame"]["quaternion"] = json!([0, 0, 0, 2]);
    assert!(kinematics::decode(&value).is_err());
    let mut value = definition();
    value["joints"][0]["limits"] = json!([1, -1]);
    assert!(kinematics::decode(&value).is_err());
    let mut value = definition();
    value["joints"][0]["initial"] = json!(4);
    assert!(kinematics::decode(&value).is_err());
    let mut value = definition();
    value["joints"][0]["child"] = json!("missing");
    assert!(Mechanism::new(kinematics::decode(&value).unwrap(), &parts()).is_err());
    assert!(kinematics::decode(&json!({"schema_version":2})).is_err());
    assert!(kinematics::decode(&json!({"unsupported":true})).is_err());
}

#[test]
fn unrepresentable_wire_pose_rolls_back_the_coordinate() {
    let mut value = definition();
    value["joints"][1]["limits"] = json!([0, 1e308]);
    let mut mechanism = Mechanism::new(kinematics::decode(&value).unwrap(), &parts()).unwrap();
    let before = mechanism.evaluate().unwrap();
    assert!(mechanism.set("slide", 1e308, "m").is_err());
    near(
        &mechanism.evaluate().unwrap().poses["carriage"].position,
        &before.poses["carriage"].position,
    );
}
