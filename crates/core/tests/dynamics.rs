use cadpad_core::{
    dynamics::{Definition, Load, Motor, Simulation, TIMESTEP_S},
    kinematics::{self, Mechanism},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn scene() -> (Value, Value) {
    // Synthetic coverage metadata tests engine math; native tests verify CAD coverage.
    let model = json!({"parts":[{"instance_id":"ground","position":[0,-100,0],"bounds":{"min":[-100,-5,-100],"max":[100,5,100]},"volume":200000},{"instance_id":"body","position":[0,0,0],"bounds":{"min":[-5,-5,-5],"max":[5,5,5]},"volume":1000}],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["ground","body"]}});
    let definition = json!({"collision":{"bodies":{"ground":[{"shape":{"type":"bounds"}}],"body":[{"shape":{"type":"bounds"}}]},"clearance_m":0,"backoff_m":1e-6}});
    (model, definition)
}
fn inputs() -> Definition {
    serde_json::from_value(json!({"gravity_m_s2":[0,-9.81,0],"bodies":{"ground":{"type":"fixed","friction":0.5,"restitution":0},"body":{"type":"dynamic","mass":{"type":"density","density_kg_m3":1000},"friction":0.5,"restitution":0}}})).unwrap()
}
fn simulation(model: &Value, definition: &Value) -> (Simulation, Mechanism) {
    let mechanism =
        Mechanism::new(kinematics::decode(definition).unwrap(), &model["parts"]).unwrap();
    (
        Simulation::new(inputs(), &mechanism, model).unwrap(),
        mechanism,
    )
}
#[test]
fn fixed_step_free_fall_and_exact_volume_density_mass_are_si_and_chunk_independent() {
    let (model, definition) = scene();
    let (mut a, _) = simulation(&model, &definition);
    assert!((a.snapshot().unwrap().mass_kg["body"] - 0.001).abs() < 1e-14);
    let mut b = a.clone();
    let result = a.step(10, None).unwrap();
    for _ in 0..10 {
        b.step(1, None).unwrap();
    }
    let p = result.current.poses["body"].position[1] * 0.001;
    let exact = -0.5 * 9.81 * (10. * TIMESTEP_S).powi(2);
    assert!((p - exact).abs() < 0.004, "{p} vs {exact}");
    assert_eq!(
        result.current.poses["body"].position,
        b.snapshot().unwrap().current.poses["body"].position
    );
    assert_eq!(result.previous.tick, 9);
    assert_eq!(result.current.tick, 10);
    assert!((result.current.linear_velocity_m_s["body"][1] + 9.81 * 10. * TIMESTEP_S).abs() < 1e-9);
}
#[test]
fn contacts_settle_on_verified_proxy_and_do_not_prove_exact_cad_fit() {
    let (model, definition) = scene();
    let (mut simulation, _) = simulation(&model, &definition);
    for _ in 0..5 {
        simulation.step(120, None).unwrap();
    }
    let snapshot = simulation.snapshot().unwrap();
    let y = snapshot.current.poses["body"].position[1];
    assert!((y + 90.).abs() < 0.2, "settled center {y} mm");
    assert!(snapshot.current.linear_velocity_m_s["body"][1].abs() < 0.01);
    assert!(snapshot.accuracy.contains("approximate"));
}
#[test]
fn bounded_slider_motor_is_force_based_and_reaches_limits_without_teleporting() {
    let (model, mut definition) = scene();
    definition["joints"] = json!([{"id":"slide","parent":null,"child":"body","kind":{"type":"prismatic","axis":[1,0,0]},"initial":0,"limits":[0,0.05]}]);
    let mechanism =
        Mechanism::new(kinematics::decode(&definition).unwrap(), &model["parts"]).unwrap();
    let mut inputs = inputs();
    inputs.gravity_m_s2 = [0.; 3];
    let mut simulation = Simulation::new(inputs, &mechanism, &model).unwrap();
    let motor = Motor {
        joint_id: "slide".into(),
        target: 0.2,
        unit: "m".into(),
        stiffness: 100.,
        damping: 0.6,
        max_force: 0.001,
    };
    assert_eq!(simulation.motor(&motor).unwrap(), 0.05);
    assert_eq!(
        simulation.snapshot().unwrap().current.poses["body"].position,
        [0., 0., 0.]
    );
    let early = simulation.step(1, None).unwrap();
    assert!(
        early.current.poses["body"].position[0] < 0.2,
        "bounded force jumped {early:?}"
    );
    for _ in 0..12 {
        simulation.step(120, None).unwrap();
    }
    let evaluation = simulation.evaluation(&mechanism).unwrap();
    assert!(
        (evaluation.joints[0].coordinate - 0.05).abs() < 0.0001,
        "{:?}",
        evaluation.joints[0]
    );
    assert!(evaluation.poses["body"].position[1].abs() < 1e-6);
}
#[test]
fn arbitrary_hinge_axis_and_rotated_local_frames_keep_native_joint_coordinates() {
    let (model, mut definition) = scene();
    definition["joints"] = json!([{"id":"hinge","parent":null,"child":"body","parent_frame":{"quaternion":[0,0,0.7071067811865475,0.7071067811865475]},"kind":{"type":"revolute","axis":[0,1,0]},"initial":0.4,"limits":[-1,1]}]);
    let mechanism =
        Mechanism::new(kinematics::decode(&definition).unwrap(), &model["parts"]).unwrap();
    let mut inputs = inputs();
    inputs.gravity_m_s2 = [0.; 3];
    let mut simulation = Simulation::new(inputs, &mechanism, &model).unwrap();
    assert!((simulation.evaluation(&mechanism).unwrap().joints[0].coordinate - 0.4).abs() < 1e-12);
    simulation
        .motor(&Motor {
            joint_id: "hinge".into(),
            target: -0.3,
            unit: "rad".into(),
            stiffness: 0.01,
            damping: 0.001,
            max_force: 0.001,
        })
        .unwrap();
    simulation.step(120, None).unwrap();
    let state = simulation.evaluation(&mechanism).unwrap();
    assert!(
        (state.joints[0].coordinate + 0.3).abs() < 0.005,
        "{:?}",
        state.joints[0]
    );
    assert!((state.joints[0].axis_world[0] + 1.).abs() < 1e-12);
}
#[test]
fn invalid_inputs_and_cancellation_reject_without_a_tick_or_teleport() {
    let (model, definition) = scene();
    let (mut simulation, mechanism) = simulation(&model, &definition);
    assert!(simulation.step(121, None).is_err());
    let (_, cancel) = tokio::sync::watch::channel(true);
    assert_eq!(
        simulation.step(1, Some(&cancel)).unwrap_err().code(),
        "CANCELLED"
    );
    assert_eq!(simulation.snapshot().unwrap().current.tick, 0);
    let mut invalid = inputs();
    invalid.gravity_m_s2[0] = f64::NAN;
    assert!(Simulation::new(invalid, &mechanism, &model).is_err());
    let mut unverified = model.clone();
    unverified["collision_validation"] = json!({});
    assert!(Simulation::new(inputs(), &mechanism, &unverified).is_err());
}

#[test]
fn persistent_world_loads_match_force_and_torque_response_without_accumulation() {
    let (model, definition) = scene();
    let mechanism =
        Mechanism::new(kinematics::decode(&definition).unwrap(), &model["parts"]).unwrap();
    let mut input = inputs();
    input.gravity_m_s2 = [0.; 3];
    let mut simulation = Simulation::new(input, &mechanism, &model).unwrap();
    // Rotate the free body 90° about Y, then return angular velocity to zero.
    // Model placements bake CAD rotations into local geometry, so a synthetic
    // part quaternion would not establish a rotated simulation body.
    let inertia = 0.001 * 0.01_f64.powi(2) / 6.;
    let torque = std::f64::consts::FRAC_PI_2 * inertia;
    for direction in [1., -1.] {
        simulation
            .set_loads(BTreeMap::from([(
                "body".into(),
                Load {
                    force_n: [0.; 3],
                    torque_nm: [0., direction * torque, 0.],
                },
            )]))
            .unwrap();
        simulation.step(120, None).unwrap();
    }
    let baseline = simulation.snapshot().unwrap().current;
    // This preparation establishes a nontrivial orientation, not a separate
    // high-precision angular-position benchmark. Subsequent checks measure the
    // additional load response against this measured baseline.
    assert!(
        (0.70..0.71).contains(&baseline.poses["body"].quaternion[1]),
        "{baseline:?}"
    );
    assert!(
        baseline.angular_velocity_rad_s["body"][1].abs() < 2e-6,
        "{baseline:?}"
    );
    let loads = BTreeMap::from([(
        "body".into(),
        Load {
            force_n: [0.002, 0., 0.],
            torque_nm: [0., 0., 1e-7],
        },
    )]);
    simulation.set_loads(loads.clone()).unwrap();
    assert_eq!(simulation.snapshot().unwrap().current.tick, 240);
    let first = simulation.step(10, None).unwrap();
    let elapsed = 10. * TIMESTEP_S;
    assert!((first.current.linear_velocity_m_s["body"][0] - 2. * elapsed).abs() < 1e-9);
    assert!(first.current.linear_velocity_m_s["body"][1].abs() < 1e-12);
    assert!(first.current.linear_velocity_m_s["body"][2].abs() < 1e-12);
    // Cube proxy I_z = mass * side² / 6 = 0.001 * 0.01² / 6 kg m².
    assert!((first.current.angular_velocity_rad_s["body"][2] - 6. * elapsed).abs() < 1e-6);
    assert!(
        (first.current.angular_velocity_rad_s["body"][0]
            - baseline.angular_velocity_rad_s["body"][0])
            .abs()
            < 1e-6
    );
    assert!(
        (first.current.angular_velocity_rad_s["body"][1]
            - baseline.angular_velocity_rad_s["body"][1])
            .abs()
            < 1e-6
    );
    assert!((first.current.poses["body"].position[0] * 0.001 - elapsed.powi(2)).abs() < 0.00005);
    simulation.set_loads(loads.clone()).unwrap();
    let second = simulation.step(10, None).unwrap();
    assert!((second.current.linear_velocity_m_s["body"][0] - 4. * elapsed).abs() < 1e-9);
    let mut still_loaded = simulation.clone();
    simulation.set_loads(BTreeMap::new()).unwrap();
    let cleared = simulation.step(10, None).unwrap();
    assert!(cleared.external_loads.is_empty());
    assert!(
        (cleared.current.linear_velocity_m_s["body"][0]
            - second.current.linear_velocity_m_s["body"][0])
            .abs()
            < 1e-9
    );
    assert!(
        (cleared.current.angular_velocity_rad_s["body"][2]
            - second.current.angular_velocity_rad_s["body"][2])
            .abs()
            < 1e-6
    );
    let retained = still_loaded.step(10, None).unwrap();
    assert_eq!(retained.external_loads, loads);
    assert!((retained.current.linear_velocity_m_s["body"][0] - 6. * elapsed).abs() < 1e-9);
}

#[test]
fn invalid_load_replacement_preserves_all_previous_loads_and_pose() {
    let (model, definition) = scene();
    let (mut simulation, _) = simulation(&model, &definition);
    let valid = Load {
        force_n: [0.002, 0., 0.],
        torque_nm: [0.; 3],
    };
    let loads = BTreeMap::from([("body".into(), valid.clone())]);
    simulation.set_loads(loads.clone()).unwrap();
    for name in ["ground", "missing"] {
        let invalid = BTreeMap::from([
            (
                "body".into(),
                Load {
                    force_n: [0.; 3],
                    torque_nm: [0.; 3],
                },
            ),
            (name.into(), valid.clone()),
        ]);
        assert_eq!(
            simulation.set_loads(invalid).unwrap_err().code(),
            "INVALID_SIMULATION"
        );
        assert_eq!(simulation.snapshot().unwrap().external_loads, loads);
    }
    for number in [f64::NAN, f64::INFINITY, 1e9 + 1.] {
        let invalid = BTreeMap::from([(
            "body".into(),
            Load {
                force_n: [number, 0., 0.],
                torque_nm: [0.; 3],
            },
        )]);
        assert!(simulation.set_loads(invalid).is_err());
        assert_eq!(simulation.snapshot().unwrap().external_loads, loads);
    }
    assert_eq!(simulation.snapshot().unwrap().current.tick, 0);
}
