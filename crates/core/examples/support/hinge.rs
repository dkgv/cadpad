use cadpad_core::{
    Core, Error, JobState, Result, dynamics, kinematics, motion, worker::WorkerConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};

const SOURCE: &str = include_str!("../../../../fixtures/physics_hinge.py");
const MECHANISM: &str = include_str!("../../../../fixtures/physics_hinge.mechanism.json");
const ACCEPTANCE: &str = include_str!("../../../../fixtures/physics_hinge.acceptance.json");

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Acceptance {
    schema_version: u32,
    scales: Vec<f64>,
    base_mass_kg: f64,
    base_dimensions_m: [f64; 3],
    base_acceleration_torque_nm: f64,
    acceleration_ticks: u32,
    settling_ticks: u32,
    base_stiffness_nm_rad: f64,
    base_damping_nm_s_rad: f64,
    base_motor_cap_nm: f64,
    base_load_nm: f64,
    base_insufficient_cap_nm: f64,
    max_acceleration_angle_relative_error: f64,
    max_acceleration_speed_relative_error: f64,
    max_work_energy_relative_error: f64,
    max_settled_angle_error_rad: f64,
    max_settled_speed_rad_s: f64,
    max_joint_limit_error_rad: f64,
    max_pivot_drift_mm: f64,
    max_axis_tilt_rad: f64,
    max_exact_bounds_error_mm: f64,
    max_static_torque_relative_error: f64,
    motor_impulse_slack_nm_s: f64,
}
#[derive(Serialize)]
struct Check {
    name: String,
    measured: f64,
    maximum: f64,
    unit: String,
    passed: bool,
}
fn check(
    checks: &mut Vec<Check>,
    name: impl Into<String>,
    measured: f64,
    maximum: f64,
    unit: &str,
) {
    checks.push(Check {
        name: name.into(),
        measured,
        maximum,
        unit: unit.into(),
        passed: measured.is_finite() && measured <= maximum,
    });
}
async fn control(
    core: &Core,
    ws: &str,
    rev: &str,
    version: u64,
    command: dynamics::Command,
) -> Result<motion::Snapshot> {
    core.control_simulation(
        "hinge-reference",
        dynamics::Control {
            workspace_id: ws.into(),
            revision: rev.into(),
            expected_pose_version: version,
            command,
        },
    )
    .await
}

pub async fn run(config: WorkerConfig, scale: f64) -> Result<Value> {
    let tol: Acceptance = serde_json::from_str(ACCEPTANCE)?;
    if !tol.scales.contains(&scale) {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Scale must match a predeclared hinge reference case",
        ));
    }
    let source = SOURCE.replace("SCALE = 1.0", &format!("SCALE = {scale:?}"));
    let mass = tol.base_mass_kg * scale.powi(3);
    let torque_scale = scale.powi(5);
    let dimensions = tol.base_dimensions_m.map(|length| length * scale);
    let inertia = mass * (dimensions[0].powi(2) + dimensions[1].powi(2)) / 12.;
    let temp = tempfile::tempdir()?;
    let core = Core::open(temp.path().join("app"), config).await?;
    let ws = core.create_workspace("hinge-reference").await?;
    let base = core.get_project(&ws).await?["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), source.clone())]),
            "hinge-reference",
        )
        .await?;
    let definition: Value = serde_json::from_str(MECHANISM)?;
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            kinematics::decode(&definition)?,
            "hinge-reference",
        )
        .await?;
    let job = core.build(&ws, &base, &draft.id, "hinge-reference").await?;
    let job = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let current = core.get_job(&ws, &job.id).await?;
            if !matches!(current.state, JobState::Queued | JobState::Running) {
                return Ok::<_, Error>(current);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| Error::domain("PHYSICS_BENCHMARK_FAILED", "Hinge build timed out"))??;
    if job.state != JobState::Succeeded {
        return Err(Error::Domain {
            code: "PHYSICS_BENCHMARK_FAILED".into(),
            message: "Hinge fixture build failed".into(),
            details: job.diagnostics,
        });
    }
    let rev = job.resulting_revision.unwrap();
    let before = core.get_project(&ws).await?;
    let mut state = core.get_motion(&ws, &rev).await?;
    let mut checks = Vec::new();
    let mut phases = Vec::new();
    let mut solver = None;
    let mut restored = true;
    for (name, target, load_sign, cap) in [
        ("constant_torque", None, 1., 0.),
        ("loaded_equilibrium", Some(0.3), -1., tol.base_motor_cap_nm),
        ("upper_limit", Some(2.), 1., tol.base_motor_cap_nm),
        ("lower_limit", Some(-2.), -1., tol.base_motor_cap_nm),
        (
            "insufficient_motor",
            Some(0.3),
            -1.,
            tol.base_insufficient_cap_nm,
        ),
    ] {
        let definition = serde_json::from_value(
            json!({"gravity_m_s2":[0,0,0],"bodies":{"rotor":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":mass},"friction":0,"restitution":0}}}),
        )?;
        state = control(
            &core,
            &ws,
            &rev,
            state.pose_version,
            dynamics::Command::Start { definition },
        )
        .await?;
        let load = if target.is_none() {
            tol.base_acceleration_torque_nm
        } else {
            tol.base_load_nm
        } * torque_scale
            * load_sign;
        state = control(
            &core,
            &ws,
            &rev,
            state.pose_version,
            dynamics::Command::Loads {
                loads: BTreeMap::from([(
                    "rotor".into(),
                    dynamics::Load {
                        force_n: [0.; 3],
                        torque_nm: [0., 0., load],
                    },
                )]),
            },
        )
        .await?;
        if let Some(target) = target {
            state = control(
                &core,
                &ws,
                &rev,
                state.pose_version,
                dynamics::Command::Motor {
                    target: dynamics::Motor {
                        joint_id: "hinge".into(),
                        target,
                        unit: "rad".into(),
                        stiffness: tol.base_stiffness_nm_rad * torque_scale,
                        damping: tol.base_damping_nm_s_rad * torque_scale,
                        max_force: cap * torque_scale,
                    },
                },
            )
            .await?;
        }
        let ticks = if target.is_none() {
            tol.acceleration_ticks
        } else {
            tol.settling_ticks
        };
        let mut limit_error: f64 = 0.;
        let mut drift: f64 = 0.;
        let mut tilt: f64 = 0.;
        let mut impulse: f64 = 0.;
        for _ in 0..ticks {
            state = control(
                &core,
                &ws,
                &rev,
                state.pose_version,
                dynamics::Command::Advance { steps: 1 },
            )
            .await?;
            let coordinate = state.evaluation.joints[0].coordinate;
            limit_error = limit_error.max((coordinate.abs() - 0.5).max(0.));
            let pose = &state.evaluation.poses["rotor"];
            drift = drift.max(glam::DVec3::from_array(pose.position).length());
            let axis = glam::DQuat::from_array(pose.quaternion) * glam::DVec3::Z;
            tilt = tilt.max(axis.cross(glam::DVec3::Z).length().atan2(axis.z));
            if target.is_some() {
                impulse = impulse.max(
                    state.simulation.as_ref().unwrap().motor_impulses["hinge"]
                        .impulse
                        .abs(),
                );
            }
        }
        let simulation = state.simulation.as_ref().unwrap();
        solver = Some(simulation.solver_settings.clone());
        let angle = state.evaluation.joints[0].coordinate;
        let speed = simulation.current.angular_velocity_rad_s["rotor"][2];
        let time = f64::from(ticks) * dynamics::TIMESTEP_S;
        check(
            &mut checks,
            format!("{name}: pivot drift"),
            drift,
            tol.max_pivot_drift_mm,
            "mm",
        );
        check(
            &mut checks,
            format!("{name}: axis tilt"),
            tilt,
            tol.max_axis_tilt_rad,
            "rad",
        );
        check(
            &mut checks,
            format!("{name}: transient limits"),
            limit_error,
            tol.max_joint_limit_error_rad,
            "rad",
        );
        let mut measured_torque = None;
        match target {
            None => {
                let expected_speed = load / inertia * time;
                let expected_angle = load / inertia * time * time / 2.;
                check(
                    &mut checks,
                    "constant torque: angular speed",
                    (speed - expected_speed).abs() / expected_speed.abs(),
                    tol.max_acceleration_speed_relative_error,
                    "relative",
                );
                check(
                    &mut checks,
                    "constant torque: angular position",
                    (angle - expected_angle).abs() / expected_angle.abs(),
                    tol.max_acceleration_angle_relative_error,
                    "relative",
                );
                let work = load * angle;
                let kinetic = inertia * speed * speed / 2.;
                check(
                    &mut checks,
                    "constant torque: work versus kinetic energy",
                    (work - kinetic).abs() / work.abs(),
                    tol.max_work_energy_relative_error,
                    "relative",
                );
            }
            Some(target) => {
                let motor = &simulation.motor_impulses["hinge"];
                if motor.force_unit != "N m"
                    || motor.impulse_unit != "N m s"
                    || motor.coordinate_unit != "rad"
                {
                    return Err(Error::domain(
                        "PHYSICS_BENCHMARK_FAILED",
                        "Hinge motor units differ from the SI contract",
                    ));
                }
                check(
                    &mut checks,
                    format!("{name}: motor impulse cap"),
                    (impulse
                        - cap * torque_scale * simulation.solver_settings.nominal_solver_substep_s)
                        .max(0.),
                    tol.motor_impulse_slack_nm_s * torque_scale,
                    "N m s",
                );
                check(
                    &mut checks,
                    format!("{name}: target clamping"),
                    (motor.target_position - target.clamp(-0.5, 0.5)).abs(),
                    0.,
                    "rad",
                );
                check(
                    &mut checks,
                    format!("{name}: settled speed"),
                    speed.abs(),
                    tol.max_settled_speed_rad_s,
                    "rad/s",
                );
                let expected = match name {
                    "loaded_equilibrium" => 0.3 + load / (tol.base_stiffness_nm_rad * torque_scale),
                    "upper_limit" => 0.5,
                    _ => -0.5,
                };
                check(
                    &mut checks,
                    format!("{name}: settled coordinate"),
                    (angle - expected).abs(),
                    tol.max_settled_angle_error_rad,
                    "rad",
                );
                if name == "loaded_equilibrium" {
                    let torque =
                        motor.impulse.abs() / simulation.solver_settings.nominal_solver_substep_s;
                    check(
                        &mut checks,
                        "loaded equilibrium: torque versus applied load",
                        (torque - load.abs()).abs() / load.abs(),
                        tol.max_static_torque_relative_error,
                        "relative",
                    );
                    measured_torque = Some(torque);
                }
            }
        }
        let exact = core.query(&ws,&rev,"inspect",json!({"pose_version":state.pose_version,"entities":[{"revision":rev,"instance_id":"rotor","entity_id":"part:0"}]})).await?;
        if exact["pose_version"] != state.pose_version {
            return Err(Error::domain(
                "PHYSICS_BENCHMARK_FAILED",
                "Exact query pose tag mismatch",
            ));
        }
        let intrinsic = dimensions.map(|length| length * 1000.);
        let (s, c) = angle.sin_cos();
        let world = [
            intrinsic[0] * c.abs() + intrinsic[1] * s.abs(),
            intrinsic[0] * s.abs() + intrinsic[1] * c.abs(),
            intrinsic[2],
        ];
        let mut exact_error: f64 = 0.;
        for index in 0..3 {
            exact_error = exact_error.max(
                (exact["entities"][0]["bounds"]["size"][index]
                    .as_f64()
                    .unwrap()
                    - intrinsic[index])
                    .abs(),
            );
            exact_error = exact_error.max(
                (exact["entities"][0]["world_bounds"]["size"][index]
                    .as_f64()
                    .unwrap()
                    - world[index])
                    .abs(),
            );
        }
        check(
            &mut checks,
            format!("{name}: intrinsic and posed exact bounds"),
            exact_error,
            tol.max_exact_bounds_error_mm,
            "mm",
        );
        phases.push(json!({"name":name,"ticks":ticks,"time_s":time,"load_nm":load,"requested_target_rad":target,"cap_nm":cap*torque_scale,"coordinate_rad":angle,"angular_speed_rad_s":speed,"max_limit_error_rad":limit_error,"max_pivot_drift_mm":drift,"max_axis_tilt_rad":tilt,"max_motor_impulse_nm_s":impulse,"settled_motor_torque_magnitude_nm":measured_torque,"pose_version":state.pose_version,"exact_bounds_max_error_mm":exact_error}));
        state = control(
            &core,
            &ws,
            &rev,
            state.pose_version,
            dynamics::Command::Stop,
        )
        .await?;
        restored &= serde_json::to_value(&state.evaluation.poses)? == before["motion"]["poses"];
    }
    let after = core.get_project(&ws).await?;
    let unchanged = ["revision", "model", "parameters", "activity"]
        .iter()
        .all(|key| before[key] == after[key]);
    let mut hash = Sha256::new();
    for data in [&source, MECHANISM, ACCEPTANCE] {
        hash.update(data.as_bytes());
    }
    Ok(
        json!({"passed":checks.iter().all(|check|check.passed)&&restored&&unchanged,"scale":scale,"mass_kg":mass,"dimensions_m":dimensions,"analytic_inertia_kg_m2":inertia,"acceptance":tol,"fixture_sha256":format!("{:x}",hash.finalize()),"phases":phases,"checks":checks,"solver_settings":solver,"dependencies":before["model"]["dependencies"],"design_revision_unchanged":unchanged,"stop_restores_design_pose":restored,"optimized":!cfg!(debug_assertions),"ui_launched":false}),
    )
}
