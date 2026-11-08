use cadpad_core::kinematics::{self, Mechanism};
use serde_json::{Value, json};
fn parts(names: &[&str]) -> Value {
    json!(
        names
            .iter()
            .map(|name| json!({"instance_id":name,"position":[0,0,0]}))
            .collect::<Vec<_>>()
    )
}
fn gear() -> Value {
    json!({"joints":[{"id":"a","parent":null,"child":"a","kind":{"type":"revolute","axis":[0,0,1]},"initial":0,"limits":[-4,4]},{"id":"b","parent":null,"child":"b","kind":{"type":"revolute","axis":[0,0,1]},"initial":0,"limits":[-8,8]}],"constraints":{"passive_joints":["b"],"equations":[{"type":"gear","id":"mesh","joint_a":"a","joint_b":"b","ratio":-2,"offset":0,"unit_a":"rad","unit_b":"rad"}]}})
}
#[test]
fn gear_coordinates_are_solved_with_units_and_passive_joints_are_not_independent_drivers() {
    let mut mechanism =
        Mechanism::new(kinematics::decode(&gear()).unwrap(), &parts(&["a", "b"])).unwrap();
    mechanism.set("a", 1.2, "rad").unwrap();
    let evaluated = mechanism.evaluate().unwrap();
    assert!((evaluated.joints[1].coordinate + 2.4).abs() < 1e-7);
    assert!(!evaluated.joints[1].drivable);
    assert!(evaluated.constraints.unwrap().path_certified);
    assert_eq!(
        mechanism.set("b", 0., "rad").unwrap_err().code(),
        "INVALID_CONSTRAINT"
    );
    let mut invalid = gear();
    invalid["constraints"]["equations"][0]["unit_b"] = json!("m");
    assert!(kinematics::decode(&invalid).is_err());
    let mut backlash = gear();
    backlash["constraints"]["equations"][0]["backlash"] = json!(-0.01);
    assert!(kinematics::decode(&backlash).is_err());
}
fn four_bar(a: f64, b: f64, c: f64, d: f64, theta: f64, upper: bool) -> Value {
    let p = glam::DVec2::new(a * theta.cos(), a * theta.sin());
    let base = glam::DVec2::new(d, 0.);
    let direction = base - p;
    let distance = direction.length();
    let unit = direction / distance;
    let x = (b * b - c * c + distance * distance) / (2. * distance);
    let y = (b * b - x * x).sqrt();
    let q = p + unit * x + glam::DVec2::new(-unit.y, unit.x) * y * if upper { 1. } else { -1. };
    let phi = (q.y - p.y).atan2(q.x - p.x) - theta;
    let rocker = q.y.atan2(q.x - d);
    json!({"joints":[{"id":"crank","parent":null,"child":"crank","kind":{"type":"revolute","axis":[0,0,1]},"initial":theta,"limits":[-3,7]},{"id":"coupler","parent":"crank","child":"coupler","parent_frame":{"position_m":[a,0,0]},"kind":{"type":"revolute","axis":[0,0,1]},"initial":phi,"limits":[-10,10]},{"id":"rocker","parent":null,"child":"rocker","parent_frame":{"position_m":[d,0,0]},"kind":{"type":"revolute","axis":[0,0,1]},"initial":rocker,"limits":[-4,4]}],"constraints":{"passive_joints":["coupler","rocker"],"equations":[{"type":"hinge_closure","id":"closing_pin","a":{"instance_id":"coupler","frame":{"position_m":[b,0,0]}},"b":{"instance_id":"rocker","frame":{"position_m":[c,0,0]}}}],"length_tolerance_m":0.000001,"angle_tolerance_rad":0.00001}})
}
#[test]
fn four_bar_continuation_preserves_both_initial_branches_and_declared_closure_tolerance() {
    for upper in [true, false] {
        let definition = four_bar(0.04, 0.09, 0.07, 0.1, 0.8, upper);
        let mut mechanism = Mechanism::new(
            kinematics::decode(&definition).unwrap(),
            &parts(&["crank", "coupler", "rocker"]),
        )
        .unwrap();
        for target in [1., 1.4, 2., 1.4, 0.8] {
            mechanism.set("crank", target, "rad").unwrap();
            let evaluation = mechanism.evaluate().unwrap();
            let report = evaluation.constraints.unwrap();
            assert!(report.position_error_m < 1e-6);
            assert!(report.axis_error_rad < 1e-5);
            assert!(report.path_certified);
            assert!(report.continuation_steps > 0);
            assert_eq!(evaluation.joints[2].coordinate > 0., upper);
        }
    }
}
#[test]
fn infeasible_target_and_a_singular_initial_branch_do_not_change_valid_coordinates() {
    let definition = four_bar(0.04, 0.06, 0.05, 0.1, 0.6, true);
    let mut mechanism = Mechanism::new(
        kinematics::decode(&definition).unwrap(),
        &parts(&["crank", "coupler", "rocker"]),
    )
    .unwrap();
    let before = mechanism.evaluate().unwrap();
    let error = mechanism.set("crank", 2.5, "rad").unwrap_err();
    assert!(matches!(
        error.code(),
        "INFEASIBLE_MECHANISM" | "SINGULAR_MECHANISM" | "CONSTRAINT_PATH_LIMIT"
    ));
    let after = mechanism.evaluate().unwrap();
    for (a, b) in before.joints.iter().zip(&after.joints) {
        assert_eq!(a.coordinate, b.coordinate);
    }
    let mut singular = gear();
    singular["constraints"]["passive_joints"] = json!(["a", "b"]);
    assert_eq!(
        Mechanism::new(kinematics::decode(&singular).unwrap(), &parts(&["a", "b"]))
            .err()
            .unwrap()
            .code(),
        "SINGULAR_MECHANISM"
    );
}

#[test]
fn continuation_interpolants_stay_closed_between_the_solved_nodes() {
    let definition = four_bar(0.04, 0.09, 0.07, 0.1, 0.8, true);
    let input = kinematics::decode(&definition).unwrap();
    let mechanism = Mechanism::new(input.clone(), &parts(&["crank", "coupler", "rocker"])).unwrap();
    let start = mechanism
        .evaluate()
        .unwrap()
        .joints
        .iter()
        .map(|joint| joint.coordinate)
        .collect::<Vec<_>>();
    let trajectory = input
        .constraints
        .as_ref()
        .unwrap()
        .trajectory(&mechanism, &start, 0, 2., None)
        .unwrap();
    assert!(trajectory.nodes.len() > 2);
    for sample in 0..=300 {
        let q = trajectory.sample(sample as f64 / 300.);
        let mut sample_definition = input.clone();
        sample_definition.constraints = None;
        for (joint, q) in sample_definition.joints.iter_mut().zip(q) {
            joint.initial = q;
        }
        let evaluation = Mechanism::new(sample_definition, &parts(&["crank", "coupler", "rocker"]))
            .unwrap()
            .evaluate()
            .unwrap();
        let world = |name: &str, point: glam::DVec3| {
            let pose = &evaluation.poses[name];
            glam::DQuat::from_array(pose.quaternion) * point
                + glam::DVec3::from_array(pose.position) * 0.001
        };
        let error = world("coupler", glam::DVec3::new(0.09, 0., 0.))
            .distance(world("rocker", glam::DVec3::new(0.07, 0., 0.)));
        assert!(error <= 0.500001e-6, "{error}");
    }
}
#[test]
fn collision_sweeps_the_passive_gear_link_and_not_only_the_driver_subtree() {
    use cadpad_core::collision::World;
    let mut definition = gear();
    definition["joints"][1]["parent_frame"] = json!({"position_m":[0.1,0,0]});
    definition["collision"] = json!({"bodies":{"a":[{"frame":{"position_m":[0.02,0,0]},"shape":{"type":"box","half_extents_m":[0.004,0.002,0.002]}}],"b":[{"frame":{"position_m":[0.05,0,0]},"shape":{"type":"box","half_extents_m":[0.004,0.002,0.002]}}],"stop":[{"shape":{"type":"ball","radius_m":0.003}}]},"clearance_m":0.00001,"backoff_m":0.000001});
    let model = json!({"parts":[{"instance_id":"a","position":[0,0,0]},{"instance_id":"b","position":[100,0,0]},{"instance_id":"stop","position":[100,-50,0]}],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["a","b","stop"]}});
    let definition = kinematics::decode(&definition).unwrap();
    let mechanism = Mechanism::new(definition.clone(), &model["parts"]).unwrap();
    let world = World::new(definition.collision.unwrap(), &model).unwrap();
    let sweep = world.sweep(&mechanism, "a", 1.5).unwrap();
    assert!(sweep.blocked, "{sweep:?}");
    assert!(sweep.trajectory_enclosure_used);
    assert!(sweep.fraction > 0.35 && sweep.fraction < 0.6, "{sweep:?}");
    let pair = sweep.instances.unwrap();
    assert!(pair.contains(&"b".into()) && pair.contains(&"stop".into()));
}

#[test]
fn physical_distance_and_directed_axis_equations_use_their_declared_units() {
    let distance = json!({"joints":[{"id":"x","parent":null,"child":"a","kind":{"type":"prismatic","axis":[1,0,0]},"initial":0,"limits":[-0.02,0.08]},{"id":"y","parent":null,"child":"b","parent_frame":{"position_m":[0.1,0,0]},"kind":{"type":"prismatic","axis":[0,1,0]},"initial":(0.15_f64.powi(2)-0.1_f64.powi(2)).sqrt(),"limits":[0,0.2]}],"constraints":{"passive_joints":["y"],"equations":[{"type":"distance","id":"link_length","a":{"instance_id":"a","point_m":[0,0,0]},"b":{"instance_id":"b","point_m":[0,0,0]},"distance_m":0.15}]}});
    let mut mechanism =
        Mechanism::new(kinematics::decode(&distance).unwrap(), &parts(&["a", "b"])).unwrap();
    mechanism.set("x", 0.02, "m").unwrap();
    assert!(
        mechanism
            .evaluate()
            .unwrap()
            .constraints
            .unwrap()
            .position_error_m
            < 1e-6
    );
    let mut axes = gear();
    axes["constraints"]["equations"] = json!([{"type":"aligned_axes","id":"same_direction","a":{"instance_id":"a","direction":[1,0,0]},"b":{"instance_id":"b","direction":[1,0,0]}}]);
    let mut mechanism =
        Mechanism::new(kinematics::decode(&axes).unwrap(), &parts(&["a", "b"])).unwrap();
    mechanism.set("a", 0.5, "rad").unwrap();
    let state = mechanism.evaluate().unwrap();
    assert!((state.joints[1].coordinate - 0.5).abs() < 1e-6);
    assert!(state.constraints.unwrap().axis_error_rad < 1e-5);
}
