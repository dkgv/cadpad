//! Real Rapier math with synthetic coverage metadata; native CAD validation is separate.
use cadpad_core::{
    dynamics::{Definition, Load, Motor, Simulation},
    kinematics::{self, Mechanism},
};
use glam::{DQuat, DVec3};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn body(equation: Value, position: [f64; 3]) -> (Simulation, Mechanism) {
    let model = json!({"parts":[{"instance_id":"body","position":position,"bounds":{"min":[-5,-5,-5],"max":[5,5,5]},"volume":1000}],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["body"]}});
    let definition = json!({"constraints":{"passive_joints":[],"equations":[equation]},"collision":{"bodies":{"body":[{"shape":{"type":"bounds"}}]},"clearance_m":0,"backoff_m":1e-6,"ignored_pairs":[]}});
    let mechanism =
        Mechanism::new(kinematics::decode(&definition).unwrap(), &model["parts"]).unwrap();
    let physics:Definition=serde_json::from_value(json!({"gravity_m_s2":[0,0,0],"bodies":{"body":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":1},"friction":0,"restitution":0}}})).unwrap();
    (
        Simulation::new(physics, &mechanism, &model).unwrap(),
        mechanism,
    )
}
fn loads(simulation: &mut Simulation, force: [f64; 3], torque: [f64; 3]) {
    simulation
        .set_loads(BTreeMap::from([(
            "body".into(),
            Load {
                force_n: force,
                torque_nm: torque,
            },
        )]))
        .unwrap();
}

#[test]
fn coincident_point_keeps_the_anchor_fixed_without_locking_rotation() {
    let (mut sim, mechanism) = body(
        json!({"type":"coincident","id":"pin","a":{"instance_id":null,"point_m":[0,0,0]},"b":{"instance_id":"body","point_m":[0,0,0]}}),
        [0.; 3],
    );
    loads(&mut sim, [1., 2., 3.], [0., 0., 2e-5]);
    for _ in 0..120 {
        sim.step(1, None).unwrap();
        let evaluation = sim.evaluation(&mechanism).unwrap();
        assert!(DVec3::from_array(evaluation.poses["body"].position).length() < 0.001);
        assert!(!evaluation.constraints.unwrap().path_certified);
    }
    assert!(sim.snapshot().unwrap().current.angular_velocity_rad_s["body"][2] > 1.);
}

#[test]
fn fixed_distance_resists_tension_and_compression_while_allowing_tangential_motion() {
    for radial in [-1., 1.] {
        let (mut sim, mechanism) = body(
            json!({"type":"distance","id":"rod","a":{"instance_id":null,"point_m":[0,0,0]},"b":{"instance_id":"body","point_m":[0,0,0]},"distance_m":0.1}),
            [0., 0., -100.],
        );
        loads(&mut sim, [0.05, 0., radial], [0.; 3]);
        for _ in 0..120 {
            sim.step(1, None).unwrap();
            let evaluation = sim.evaluation(&mechanism).unwrap();
            let center = DVec3::from_array(evaluation.poses["body"].position) * 0.001;
            let error = (center.length() - 0.1).abs();
            assert!(error < 1e-6, "radial={radial}, error={error}");
            let report = evaluation.constraints.unwrap();
            assert!((report.position_error_m - error).abs() < 1e-12);
            assert!(!report.path_certified);
        }
        assert!(sim.snapshot().unwrap().current.poses["body"].position[0] > 1.);
    }
}

#[test]
fn aligned_axes_keeps_direction_but_allows_translation_and_twist() {
    let (mut sim, mechanism) = body(
        json!({"type":"aligned_axes","id":"axis","a":{"instance_id":null,"direction":[0,1,0]},"b":{"instance_id":"body","direction":[0,1,0]}}),
        [0.; 3],
    );
    loads(&mut sim, [0.1, 0., 0.], [0.001, 2e-5, 0.001]);
    for _ in 0..60 {
        sim.step(1, None).unwrap();
        let evaluation = sim.evaluation(&mechanism).unwrap();
        let axis = DQuat::from_array(evaluation.poses["body"].quaternion) * DVec3::Y;
        assert!(axis.distance(DVec3::Y) < 1e-5);
        assert!(evaluation.constraints.unwrap().axis_error_rad < 1e-5);
    }
    let frame = sim.snapshot().unwrap().current;
    assert!(frame.poses["body"].position[0] > 10.);
    assert!(frame.angular_velocity_rad_s["body"][1] > 0.1);
}

#[test]
fn closing_hinge_uses_local_frame_z_axis_and_offset_anchors() {
    let q = [
        -std::f64::consts::FRAC_1_SQRT_2,
        0.,
        0.,
        std::f64::consts::FRAC_1_SQRT_2,
    ];
    let (mut sim, mechanism) = body(
        json!({"type":"hinge_closure","id":"offset_pin","a":{"instance_id":null,"frame":{"position_m":[0.02,0,0],"quaternion":q}},"b":{"instance_id":"body","frame":{"position_m":[0,0,0.05],"quaternion":q}}}),
        [20., 0., -50.],
    );
    loads(&mut sim, [0.; 3], [0.001, 0.001, 0.001]);
    for _ in 0..60 {
        sim.step(1, None).unwrap();
        let evaluation = sim.evaluation(&mechanism).unwrap();
        let pose = &evaluation.poses["body"];
        let rotation = DQuat::from_array(pose.quaternion);
        let anchor = DVec3::from_array(pose.position) * 0.001 + rotation * DVec3::new(0., 0., 0.05);
        assert!(anchor.distance(DVec3::new(0.02, 0., 0.)) < 1e-6);
        assert!((rotation * DVec3::Y).distance(DVec3::Y) < 1e-5);
        assert!(!evaluation.constraints.unwrap().path_certified);
    }
    assert!(sim.snapshot().unwrap().current.angular_velocity_rad_s["body"][1] > 0.1);
}

#[test]
fn joint_coordinates_keep_their_identity_when_definition_order_is_not_topological() {
    let model = json!({"parts":[{"instance_id":"a","position":[0,0,0],"bounds":{"min":[-1,-1,-1],"max":[1,1,1]},"volume":8},{"instance_id":"b","position":[30,0,0],"bounds":{"min":[-1,-1,-1],"max":[1,1,1]},"volume":8}],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["a","b"]}});
    let definition = json!({"joints":[{"id":"child","parent":"a","child":"b","parent_frame":{"position_m":[0.03,0,0]},"kind":{"type":"prismatic","axis":[0,1,0]},"initial":0.02,"limits":[0,0.1]},{"id":"parent","parent":null,"child":"a","kind":{"type":"prismatic","axis":[1,0,0]},"initial":0.01,"limits":[0,0.1]}],"collision":{"bodies":{"a":[{"shape":{"type":"bounds"}}],"b":[{"shape":{"type":"bounds"}}]},"clearance_m":0,"backoff_m":1e-6}});
    let mechanism =
        Mechanism::new(kinematics::decode(&definition).unwrap(), &model["parts"]).unwrap();
    let dynamic = json!({"type":"dynamic","mass":{"type":"kilograms","mass_kg":1},"friction":0,"restitution":0});
    let mut sim = Simulation::new(
        serde_json::from_value(json!({"gravity_m_s2":[0,0,0],"bodies":{"a":dynamic,"b":dynamic}}))
            .unwrap(),
        &mechanism,
        &model,
    )
    .unwrap();
    sim.step(1, None).unwrap();
    let evaluation = sim.evaluation(&mechanism).unwrap();
    for (id, expected) in [("parent", 0.01), ("child", 0.02)] {
        assert!(
            (evaluation
                .joints
                .iter()
                .find(|j| j.id == id)
                .unwrap()
                .coordinate
                - expected)
                .abs()
                < 1e-10
        );
    }
}

#[test]
fn closed_linkage_passive_coordinates_cannot_be_motor_targets() {
    let definition: Value = serde_json::from_str(include_str!(
        "../../../fixtures/physics_linkage.mechanism.json"
    ))
    .unwrap();
    let mut parts = vec![
        json!({"instance_id":"ground","position":[0,0,0],"bounds":{"min":[-10,-8,-10],"max":[110,8,-6]},"volume":7680}),
    ];
    for (name, length) in [("crank", 40), ("coupler", 90), ("rocker", 70)] {
        parts.push(json!({"instance_id":name,"position":[0,0,0],"bounds":{"min":[0,-3,-2],"max":[length,3,2]},"volume":length*24}));
    }
    let model = json!({"parts":parts,"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["ground","crank","coupler","rocker"]}});
    let mechanism =
        Mechanism::new(kinematics::decode(&definition).unwrap(), &model["parts"]).unwrap();
    let dynamic = json!({"type":"dynamic","mass":{"type":"density","density_kg_m3":7800},"friction":0,"restitution":0});
    let mut sim=Simulation::new(serde_json::from_value(json!({"gravity_m_s2":[0,0,0],"bodies":{"ground":{"type":"fixed","friction":0,"restitution":0},"crank":dynamic,"coupler":dynamic,"rocker":dynamic}})).unwrap(),&mechanism,&model).unwrap();
    let motor = |joint_id: &str| Motor {
        joint_id: joint_id.into(),
        target: 1.05,
        unit: "rad".into(),
        stiffness: 0.3,
        damping: 0.03,
        max_force: 0.03,
    };
    assert_eq!(
        sim.motor(&motor("coupler")).unwrap_err().code(),
        "INVALID_SIMULATION"
    );
    assert_eq!(
        sim.motor(&motor("rocker")).unwrap_err().code(),
        "INVALID_SIMULATION"
    );
    sim.motor(&motor("crank")).unwrap();
    for _ in 0..120 {
        sim.step(1, None).unwrap();
        let report = sim.evaluation(&mechanism).unwrap().constraints.unwrap();
        assert!(report.position_error_m < 2e-5, "{report:?}");
        assert!(!report.path_certified);
    }
}

#[test]
fn zero_distance_is_rejected_before_a_singular_radial_joint_can_be_created() {
    let error=kinematics::decode(&json!({"constraints":{"passive_joints":[],"equations":[{"type":"distance","id":"zero","a":{"instance_id":null,"point_m":[0,0,0]},"b":{"instance_id":"body","point_m":[0,0,0]},"distance_m":0}]}})).expect_err("Zero distance must fail; use coincidence instead");
    assert_eq!(error.code(), "INVALID_CONSTRAINT");
}

#[test]
fn redundant_same_body_and_fixed_only_closures_are_explicit_errors() {
    let model = json!({"parts":[{"instance_id":"body","position":[0,0,0],"bounds":{"min":[-5,-5,-5],"max":[5,5,5]},"volume":1000}],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["body"]}});
    for (a, body_type) in [(Some("body"), "dynamic"), (None, "fixed")] {
        let definition = json!({"constraints":{"passive_joints":[],"equations":[{"type":"coincident","id":"pin","a":{"instance_id":a,"point_m":[0,0,0]},"b":{"instance_id":"body","point_m":[0,0,0]}}]},"collision":{"bodies":{"body":[{"shape":{"type":"bounds"}}]},"clearance_m":0,"backoff_m":1e-6}});
        let mechanism =
            Mechanism::new(kinematics::decode(&definition).unwrap(), &model["parts"]).unwrap();
        let mut body = json!({"type":body_type,"friction":0,"restitution":0});
        if body_type == "dynamic" {
            body["mass"] = json!({"type":"kilograms","mass_kg":1});
        }
        let physics =
            serde_json::from_value(json!({"gravity_m_s2":[0,0,0],"bodies":{"body":body}})).unwrap();
        let error = Simulation::new(physics, &mechanism, &model)
            .err()
            .expect("Invalid closure must fail");
        assert_eq!(error.code(), "INVALID_SIMULATION");
    }
}

fn gear_pair(
    kind_b: Value,
    ratio: f64,
    unit_b: &str,
    limits_b: [f64; 2],
) -> (Simulation, Mechanism) {
    gear_pair_with_driver_offset(kind_b, ratio, unit_b, limits_b, [0., 0., 0.])
}
fn gear_pair_with_driver_offset(
    kind_b: Value,
    ratio: f64,
    unit_b: &str,
    limits_b: [f64; 2],
    driver_child_frame_m: [f64; 3],
) -> (Simulation, Mechanism) {
    gear_pair_with_options(kind_b, ratio, unit_b, limits_b, driver_child_frame_m, 0.)
}
fn gear_pair_with_options(
    kind_b: Value,
    ratio: f64,
    unit_b: &str,
    limits_b: [f64; 2],
    driver_child_frame_m: [f64; 3],
    backlash: f64,
) -> (Simulation, Mechanism) {
    let part = |id, x| json!({"instance_id":id,"position":[x,0,0],"bounds":{"min":[-5,-5,-5],"max":[5,5,5]},"volume":1000});
    // Child-frame offset places the driver's center of mass away from the hinge so
    // the free-axis inertia must include the parallel-axis bearing term.
    let driver = if driver_child_frame_m == [0., 0., 0.] {
        json!({"id":"a","parent":null,"child":"a","kind":{"type":"revolute","axis":[0,0,1]},"initial":0,"limits":[-3,3]})
    } else {
        json!({"id":"a","parent":null,"child":"a","child_frame":{"position_m":driver_child_frame_m},"kind":{"type":"revolute","axis":[0,0,1]},"initial":0,"limits":[-3,3]})
    };
    let model = json!({"parts":[part("a", 0.), part("b", 40.)],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["a","b"]}});
    let mut equation = json!({"type":"gear","id":"mesh","joint_a":"a","joint_b":"b","ratio":ratio,"offset":0,"unit_a":"rad","unit_b":unit_b});
    if backlash > 0. {
        equation["backlash"] = json!(backlash);
    }
    let definition = json!({"joints":[driver,{"id":"b","parent":null,"child":"b","parent_frame":{"position_m":[0.04,0,0]},"kind":kind_b,"initial":0,"limits":limits_b}],"constraints":{"passive_joints":["b"],"equations":[equation]},"collision":{"bodies":{"a":[{"shape":{"type":"bounds"}}],"b":[{"shape":{"type":"bounds"}}]},"clearance_m":0,"backoff_m":1e-6}});
    let mechanism =
        Mechanism::new(kinematics::decode(&definition).unwrap(), &model["parts"]).unwrap();
    let dynamic = json!({"type":"dynamic","mass":{"type":"kilograms","mass_kg":1},"friction":0,"restitution":0});
    let physics = serde_json::from_value(
        json!({"gravity_m_s2":[0,0,0],"bodies":{"a":dynamic.clone(),"b":dynamic}}),
    )
    .unwrap();
    (
        Simulation::new(physics, &mechanism, &model).unwrap(),
        mechanism,
    )
}
fn coordinate(simulation: &Simulation, mechanism: &Mechanism, id: &str) -> f64 {
    simulation
        .evaluation(mechanism)
        .unwrap()
        .joints
        .into_iter()
        .find(|joint| joint.id == id)
        .unwrap()
        .coordinate
}
#[test]
fn gear_ratio_uses_reflected_inertia_for_a_centered_hinge_and_a_rack() {
    let half = 0.005 + 1e-9;
    let spin_inertia = (half * half + half * half) / 3.;
    let torque = 1e-4;
    let ticks = 60u32;
    let dt = 1. / 120.;
    for (kind, ratio, unit, limits, reflected) in [
        (
            json!({"type":"revolute","axis":[0,0,1]}),
            -2.,
            "rad",
            [-3., 3.],
            spin_inertia,
        ),
        (
            json!({"type":"prismatic","axis":[1,0,0]}),
            0.01,
            "m",
            [-0.05, 0.05],
            1.,
        ),
    ] {
        let (mut simulation, mechanism) = gear_pair(kind, ratio, unit, limits);
        assert_eq!(
            simulation
                .motor(&Motor {
                    joint_id: "b".into(),
                    target: 0.1,
                    unit: unit.into(),
                    stiffness: 0.1,
                    damping: 0.01,
                    max_force: 0.01,
                })
                .unwrap_err()
                .code(),
            "INVALID_SIMULATION"
        );
        simulation
            .set_loads(BTreeMap::from([(
                "a".into(),
                Load {
                    force_n: [0.; 3],
                    torque_nm: [0., 0., torque],
                },
            )]))
            .unwrap();
        simulation.step(ticks, None).unwrap();
        let alpha = torque / (spin_inertia + ratio * ratio * reflected);
        let snapshot = simulation.snapshot().unwrap();
        // Rapier divides each tick by its solver-substep count before integrating.
        let substeps = snapshot.solver_settings.solver_substeps as f64;
        let step = dt / substeps;
        let count = f64::from(ticks) * substeps;
        let expected_a = alpha * step * step * count * (count + 1.) / 2.;
        let expected_rate = alpha * step * count;
        let qa = coordinate(&simulation, &mechanism, "a");
        let qb = coordinate(&simulation, &mechanism, "b");
        let frame = snapshot.current;
        let rate_b = if unit == "rad" {
            frame.angular_velocity_rad_s["b"][2]
        } else {
            frame.linear_velocity_m_s["b"][0]
        };
        assert!(
            (qa - expected_a).abs() < 1e-7,
            "qa {qa} expected {expected_a}"
        );
        assert!((qb - ratio * expected_a).abs() < 1e-5, "qb {qb}");
        assert!(
            (frame.angular_velocity_rad_s["a"][2] - expected_rate).abs() < 1e-5,
            "wa {}",
            frame.angular_velocity_rad_s["a"][2]
        );
        assert!((rate_b - ratio * expected_rate).abs() < 1e-5, "wb {rate_b}");
        assert!((qb - ratio * qa).abs() < 1e-8);
        assert!(
            !simulation
                .evaluation(&mechanism)
                .unwrap()
                .constraints
                .unwrap()
                .path_certified
        );
    }
}
#[test]
fn gear_ratio_uses_parallel_axis_inertia_for_an_offset_hinge() {
    let half = 0.005 + 1e-9;
    let spin_inertia = (half * half + half * half) / 3.;
    let offset = 0.05;
    let driver_inertia = spin_inertia + offset * offset;
    let ratio = -2.;
    let torque = 1e-4;
    let ticks = 60u32;
    let dt = 1. / 120.;
    let (mut simulation, mechanism) = gear_pair_with_driver_offset(
        json!({"type":"revolute","axis":[0,0,1]}),
        ratio,
        "rad",
        [-3., 3.],
        [offset, 0., 0.],
    );
    simulation
        .set_loads(BTreeMap::from([(
            "a".into(),
            Load {
                force_n: [0.; 3],
                torque_nm: [0., 0., torque],
            },
        )]))
        .unwrap();
    simulation.step(ticks, None).unwrap();
    let alpha = torque / (driver_inertia + ratio * ratio * spin_inertia);
    let snapshot = simulation.snapshot().unwrap();
    let substeps = snapshot.solver_settings.solver_substeps as f64;
    let step = dt / substeps;
    let count = f64::from(ticks) * substeps;
    let expected_a = alpha * step * step * count * (count + 1.) / 2.;
    let expected_rate = alpha * step * count;
    let qa = coordinate(&simulation, &mechanism, "a");
    let qb = coordinate(&simulation, &mechanism, "b");
    let frame = snapshot.current;
    assert!(
        (qa - expected_a).abs() < 1e-6,
        "qa {qa} expected {expected_a}"
    );
    assert!((qb - ratio * expected_a).abs() < 1e-5, "qb {qb}");
    assert!(
        (frame.angular_velocity_rad_s["a"][2] - expected_rate).abs() < 1e-5,
        "wa {}",
        frame.angular_velocity_rad_s["a"][2]
    );
    assert!((frame.angular_velocity_rad_s["b"][2] - ratio * expected_rate).abs() < 1e-5);
    assert!((qb - ratio * qa).abs() < 1e-8);
    // The driver center of mass must orbit the hinge, not spin in place.
    let position = DVec3::from_array(frame.poses["a"].position) * 0.001;
    let radius = position.truncate().length();
    assert!(
        (radius - offset).abs() < 1e-5,
        "driver should stay {offset} m from the hinge, got {radius}"
    );
}

#[test]
fn gear_tooth_contact_stays_open_through_backlash_then_drives_the_flank() {
    let backlash = 0.2;
    let (mut simulation, mechanism) = gear_pair_with_options(
        json!({"type":"revolute","axis":[0,0,1]}),
        -1.,
        "rad",
        [-3., 3.],
        [0., 0., 0.],
        backlash,
    );
    simulation
        .set_loads(BTreeMap::from([(
            "a".into(),
            Load {
                force_n: [0.; 3],
                torque_nm: [0., 0., 5e-4],
            },
        )]))
        .unwrap();
    // While the residual is inside ±backlash/2 the mesh is open: only the driver moves.
    simulation.step(5, None).unwrap();
    let qa_open = coordinate(&simulation, &mechanism, "a");
    let qb_open = coordinate(&simulation, &mechanism, "b");
    let residual_open = qb_open + qa_open;
    assert!(
        qa_open.abs() > 0.01,
        "driver should advance through the backlash, got {qa_open}"
    );
    assert!(
        qb_open.abs() < 1e-6,
        "driven gear should stay idle in the open mesh, got {qb_open}"
    );
    assert!(
        residual_open.abs() < backlash * 0.5,
        "residual {residual_open} should still be inside the backlash"
    );
    // After the flank engages, the driven gear follows with residual ≈ ±backlash/2.
    simulation.step(60, None).unwrap();
    let qa = coordinate(&simulation, &mechanism, "a");
    let qb = coordinate(&simulation, &mechanism, "b");
    let residual = qb + qa;
    assert!(
        qb.abs() > 0.02,
        "driven gear should move after tooth contact, got {qb}"
    );
    assert!(
        (residual.abs() - backlash * 0.5).abs() < 1e-4,
        "engaged residual {residual} should sit on the backlash flank"
    );
    assert!(qa.abs() > qa_open.abs());
}

#[test]
fn gear_keeps_the_ratio_and_speed_after_the_driving_torque_stops() {
    let (mut simulation, mechanism) = gear_pair(
        json!({"type":"revolute","axis":[0,0,1]}),
        -2.,
        "rad",
        [-3., 3.],
    );
    simulation
        .set_loads(BTreeMap::from([(
            "a".into(),
            Load {
                force_n: [0.; 3],
                torque_nm: [0., 0., 1e-4],
            },
        )]))
        .unwrap();
    simulation.step(30, None).unwrap();
    simulation.set_loads(BTreeMap::new()).unwrap();
    let speed = simulation
        .snapshot()
        .unwrap()
        .current
        .angular_velocity_rad_s["a"][2];
    let start = coordinate(&simulation, &mechanism, "a");
    simulation.step(90, None).unwrap();
    let frame = simulation.snapshot().unwrap().current;
    let qa = coordinate(&simulation, &mechanism, "a");
    let qb = coordinate(&simulation, &mechanism, "b");
    assert!((frame.angular_velocity_rad_s["a"][2] - speed).abs() < 1e-6);
    assert!((frame.angular_velocity_rad_s["b"][2] + 2. * speed).abs() < 1e-6);
    assert!((qb + 2. * qa).abs() < 1e-8);
    assert!((qa - (start + speed * 90. / 120.)).abs() < 1e-5);
}
fn path_carriage(
    path: Value,
    initial: f64,
    limits: [f64; 2],
    gravity: [f64; 3],
) -> (Simulation, Mechanism) {
    path_carriage_with_wall(path, initial, limits, gravity, None)
}
fn path_carriage_with_wall(
    path: Value,
    initial: f64,
    limits: [f64; 2],
    gravity: [f64; 3],
    wall_x_m: Option<f64>,
) -> (Simulation, Mechanism) {
    let mut parts = vec![
        json!({"instance_id":"carriage","position":[0,0,0],"bounds":{"min":[-5,-5,-5],"max":[5,5,5]},"volume":1000}),
    ];
    let mut covered = vec![json!("carriage")];
    let mut collision_bodies = json!({"carriage":[{"shape":{"type":"bounds"}}]});
    let mut physics_bodies = json!({"carriage":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":1},"friction":0,"restitution":0}});
    if let Some(x) = wall_x_m {
        // Thin fixed stop across the track. Carriage half-extent is 5 mm.
        parts.push(json!({"instance_id":"wall","position":[x * 1000., 0., 0.],"bounds":{"min":[-1,-20,-20],"max":[1,20,20]},"volume":1}));
        covered.push(json!("wall"));
        collision_bodies["wall"] = json!([{"shape":{"type":"bounds"}}]);
        physics_bodies["wall"] = json!({"type":"fixed","friction":0,"restitution":0});
    }
    let model = json!({"parts": parts, "collision_validation":{"method":"brep_boolean_coverage","covered_instances": covered}});
    let definition = json!({"joints":[{"id":"track","parent":null,"child":"carriage","kind":{"type":"path","path":path},"initial":initial,"limits":limits}],"collision":{"bodies": collision_bodies, "clearance_m":0,"backoff_m":1e-6}});
    let mechanism =
        Mechanism::new(kinematics::decode(&definition).unwrap(), &model["parts"]).unwrap();
    let physics =
        serde_json::from_value(json!({"gravity_m_s2": gravity, "bodies": physics_bodies})).unwrap();
    (
        Simulation::new(physics, &mechanism, &model).unwrap(),
        mechanism,
    )
}
#[test]
fn straight_path_accelerates_with_the_applied_force() {
    let (mut simulation, mechanism) = path_carriage(
        json!({"start_m":[0,0,0],"up":[0,0,1],"segments":[{"type":"line","end_m":[1,0,0]}]}),
        0.05,
        [0., 1.],
        [0.; 3],
    );
    simulation
        .set_loads(BTreeMap::from([(
            "carriage".into(),
            Load {
                force_n: [1., 0., 0.],
                torque_nm: [0.; 3],
            },
        )]))
        .unwrap();
    let ticks = 60u32;
    simulation.step(ticks, None).unwrap();
    let snapshot = simulation.snapshot().unwrap();
    let substeps = snapshot.solver_settings.solver_substeps as f64;
    let step = (1. / 120.) / substeps;
    let count = f64::from(ticks) * substeps;
    let expected = 0.05 + step * step * count * (count + 1.) / 2.;
    let coordinate = coordinate(&simulation, &mechanism, "track");
    let speed = snapshot.current.linear_velocity_m_s["carriage"][0];
    assert!(
        (coordinate - expected).abs() < 1e-6,
        "{coordinate} != {expected}"
    );
    assert!((speed - f64::from(ticks) / 120.).abs() < 1e-6, "{speed}");
}
#[test]
fn circular_path_swings_as_a_pendulum_including_frame_inertia() {
    let radius = 0.2;
    let bottom = radius;
    let initial = bottom + radius * 0.2;
    let start = [-radius * 1.0_f64.sin(), -radius * 1.0_f64.cos(), 0.];
    let (mut simulation, mechanism) = path_carriage(
        json!({"start_m":start,"up":[0,0,1],"segments":[{"type":"arc","center_m":[0,0,0],"axis":[0,0,1],"angle_rad":2.0}]}),
        initial,
        [0., radius * 2.],
        [0., -9.81, 0.],
    );
    let ticks = 90u32;
    simulation.step(ticks, None).unwrap();
    let half = 0.005 + 1e-9;
    let inertia = (half * half + half * half) / 3.;
    let metric = 1. + inertia / (radius * radius);
    let substeps = simulation
        .snapshot()
        .unwrap()
        .solver_settings
        .solver_substeps;
    let step = (1. / 120.) / substeps as f64;
    let mut s = initial;
    let mut speed = 0.;
    for _ in 0..ticks as usize * substeps {
        let theta = (s - bottom) / radius;
        speed += -9.81 * theta.sin() / metric * step;
        s += speed * step;
    }
    let actual = coordinate(&simulation, &mechanism, "track");
    let velocity =
        DVec3::from_array(simulation.snapshot().unwrap().current.linear_velocity_m_s["carriage"]);
    assert!((actual - s).abs() < 1e-5, "{actual} != {s}");
    assert!(
        (velocity.length() - speed.abs()).abs() < 1e-5,
        "{velocity:?}"
    );
    let theta = (actual - bottom) / radius;
    let center = DVec3::new(radius * theta.sin(), -radius * theta.cos(), 0.);
    let position =
        DVec3::from_array(simulation.snapshot().unwrap().current.poses["carriage"].position)
            * 0.001;
    assert!(
        position.distance(center) < 1e-5,
        "{position:?} != {center:?}"
    );
}
#[test]
fn straight_path_contact_stops_the_carriage_at_a_fixed_wall() {
    // Wall face at 0.2 − 0.001 m; carriage half-extent 0.005 m → first contact near 0.194 m.
    let wall_x = 0.2;
    let contact = wall_x - 0.001 - 0.005;
    let (mut simulation, mechanism) = path_carriage_with_wall(
        json!({"start_m":[0,0,0],"up":[0,0,1],"segments":[{"type":"line","end_m":[1,0,0]}]}),
        0.05,
        [0., 1.],
        [0.; 3],
        Some(wall_x),
    );
    simulation
        .set_loads(BTreeMap::from([(
            "carriage".into(),
            Load {
                force_n: [1., 0., 0.],
                torque_nm: [0.; 3],
            },
        )]))
        .unwrap();
    simulation.step(120, None).unwrap();
    let coordinate = coordinate(&simulation, &mechanism, "track");
    let speed = simulation.snapshot().unwrap().current.linear_velocity_m_s["carriage"][0];
    assert!(
        coordinate < contact + 0.002,
        "carriage should not pass the wall: {coordinate} >= {contact}"
    );
    assert!(
        coordinate > contact - 0.01,
        "carriage should reach the wall: {coordinate} << {contact}"
    );
    assert!(
        speed < 0.05,
        "contact should remove forward speed, got {speed}"
    );
}
