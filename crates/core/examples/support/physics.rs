use cadpad_core::{
    Core, Error, JobState, Result, dynamics, kinematics, motion, worker::WorkerConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

const SOURCE: &str = include_str!("../../../../fixtures/physics_slider.py");
const MECHANISM: &str = include_str!("../../../../fixtures/physics_slider.mechanism.json");
const ACCEPTANCE: &str = include_str!("../../../../fixtures/physics_slider.acceptance.json");
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Acceptance {
    schema_version: u32,
    pub mass_kg: f64,
    pub gravity_m_s2: f64,
    pub stiffness_n_m: f64,
    pub damping_n_s_m: f64,
    pub max_force_n: f64,
    pub insufficient_force_n: f64,
    pub proxy_padding_mm: f64,
    pub max_proxy_clearance_comparison_error_mm: f64,
    pub ticks_per_phase: u32,
    pub max_transient_penetration_mm: f64,
    pub max_settled_penetration_mm: f64,
    pub max_settled_cad_gap_mm: f64,
    pub max_joint_limit_error_mm: f64,
    pub max_spring_equilibrium_error_mm: f64,
    pub max_settled_speed_m_s: f64,
    pub max_axis_drift_mm: f64,
    pub max_rotation_error_rad: f64,
    pub max_static_motor_force_error_n: f64,
    pub motor_impulse_slack_n_s: f64,
}
#[derive(Serialize)]
pub struct Phase {
    pub name: String,
    pub requested_target_m: f64,
    pub applied_target_m: f64,
    pub max_force_n: f64,
    pub settled_coordinate_m: f64,
    pub settled_speed_m_s: f64,
    pub max_limit_error_mm: f64,
    pub max_penetration_mm: f64,
    pub max_proxy_penetration_mm: f64,
    pub settled_proxy_gap_mm: f64,
    pub max_axis_drift_mm: f64,
    pub max_rotation_error_rad: f64,
    pub max_solver_motor_impulse_n_s: f64,
    pub last_solver_motor_impulse_n_s: f64,
    pub exact_cad_gap_mm: f64,
    pub exact_cad_distance_mm: f64,
    pub exact_cad_intersection_volume_mm3: f64,
    pub pose_version: u64,
}
#[derive(Serialize)]
pub struct Check {
    pub name: String,
    pub measured: f64,
    pub maximum: f64,
    pub unit: String,
    pub passed: bool,
}
#[derive(Serialize)]
pub struct Report {
    pub passed: bool,
    pub fixture_sha256: String,
    pub proxy_padding_mm: f64,
    pub acceptance: Acceptance,
    pub phases: Vec<Phase>,
    pub checks: Vec<Check>,
    pub dependencies: Value,
    pub solver_settings: dynamics::SolverSettings,
    pub timestep_s: f64,
    pub ticks: u64,
    pub expected_loaded_equilibrium_m: f64,
    pub measured_static_motor_force_magnitude_n: f64,
    pub native_build_ms: f64,
    pub control_tick_us_mean: f64,
    pub control_tick_us_p95: f64,
    pub control_tick_us_max: f64,
    pub optimized: bool,
    pub design_revision_unchanged: bool,
    pub stop_restores_design_pose: bool,
}
fn failure(message: &str) -> Error {
    Error::domain("PHYSICS_BENCHMARK_FAILED", message)
}
async fn control(
    core: &Core,
    ws: &str,
    revision: &str,
    version: u64,
    command: dynamics::Command,
) -> Result<motion::Snapshot> {
    core.control_simulation(
        "physics-reference",
        dynamics::Control {
            workspace_id: ws.into(),
            revision: revision.into(),
            expected_pose_version: version,
            command,
        },
    )
    .await
}
struct Trial {
    name: &'static str,
    target: f64,
    force: f64,
    padding_mm: f64,
}
async fn phase(
    core: &Core,
    ws: &str,
    rev: &str,
    state: &mut motion::Snapshot,
    trial: Trial,
    tol: &Acceptance,
    timings: &mut Vec<f64>,
) -> Result<Phase> {
    let Trial {
        name,
        target,
        force,
        padding_mm,
    } = trial;
    *state = control(
        core,
        ws,
        rev,
        state.pose_version,
        dynamics::Command::Motor {
            target: dynamics::Motor {
                joint_id: "vertical".into(),
                target,
                unit: "m".into(),
                stiffness: tol.stiffness_n_m,
                damping: tol.damping_n_s_m,
                max_force: force,
            },
        },
    )
    .await?;
    let mut limit: f64 = 0.;
    let mut penetration: f64 = 0.;
    let mut proxy_penetration: f64 = 0.;
    let mut drift: f64 = 0.;
    let mut rotation: f64 = 0.;
    let mut impulse: f64 = 0.;
    for _ in 0..tol.ticks_per_phase {
        let start = Instant::now();
        *state = control(
            core,
            ws,
            rev,
            state.pose_version,
            dynamics::Command::Advance { steps: 1 },
        )
        .await?;
        timings.push(start.elapsed().as_secs_f64() * 1e6);
        let simulation = state
            .simulation
            .as_ref()
            .ok_or_else(|| failure("Missing simulation state"))?;
        let p = &state.evaluation.poses["slider"];
        let joint = state
            .evaluation
            .joints
            .iter()
            .find(|joint| joint.id == "vertical")
            .ok_or_else(|| failure("Missing vertical joint"))?;
        limit = limit.max(((0.01 - joint.coordinate).max(joint.coordinate - 0.15)).max(0.) * 1000.);
        penetration = penetration.max((p.position[2] + support_z(p, 10.) - 120.).max(0.));
        proxy_penetration =
            proxy_penetration.max((p.position[2] + support_z(p, 10. + padding_mm) - 120.).max(0.));
        drift = drift.max(p.position[0].hypot(p.position[1]));
        rotation = rotation.max(
            2. * p.quaternion[0]
                .hypot(p.quaternion[1])
                .hypot(p.quaternion[2])
                .atan2(p.quaternion[3].abs()),
        );
        impulse = impulse.max(simulation.motor_impulses["vertical"].impulse.abs());
    }
    let exact=core.query(ws,rev,"inspect",json!({"pose_version":state.pose_version,"entities":[{"revision":rev,"instance_id":"slider","entity_id":"part:0"},{"revision":rev,"instance_id":"stop","entity_id":"part:0"}]})).await?;
    let pair=core.query(ws,rev,"inspect",json!({"pose_version":state.pose_version,"query":"interference","pairs":[["slider","stop"]]})).await?;
    let scalar = |value: &Value| {
        value
            .as_f64()
            .ok_or_else(|| failure("Missing exact numeric result"))
    };
    let gap = scalar(&exact["entities"][1]["world_bounds"]["min"][2])?
        - scalar(&exact["entities"][0]["world_bounds"]["max"][2])?;
    let simulation = state.simulation.as_ref().unwrap();
    let motor = &simulation.motor_impulses["vertical"];
    Ok(Phase {
        name: name.into(),
        requested_target_m: target,
        applied_target_m: motor.target_position,
        max_force_n: motor.max_force,
        settled_coordinate_m: state.evaluation.joints[0].coordinate,
        settled_speed_m_s: simulation.current.linear_velocity_m_s["slider"][2].abs(),
        max_limit_error_mm: limit,
        max_penetration_mm: penetration,
        max_proxy_penetration_mm: proxy_penetration,
        settled_proxy_gap_mm: 120.
            - (state.evaluation.poses["slider"].position[2]
                + support_z(&state.evaluation.poses["slider"], 10. + padding_mm)),
        max_axis_drift_mm: drift,
        max_rotation_error_rad: rotation,
        max_solver_motor_impulse_n_s: impulse,
        last_solver_motor_impulse_n_s: motor.impulse,
        exact_cad_gap_mm: gap,
        exact_cad_distance_mm: scalar(&pair["pairs"][0]["distance_mm"])?,
        exact_cad_intersection_volume_mm3: scalar(&pair["pairs"][0]["intersection_volume_mm3"])?,
        pose_version: state.pose_version,
    })
}
fn support_z(pose: &kinematics::Pose, half: f64) -> f64 {
    let [x, y, z, w] = pose.quaternion;
    half * ((2. * (x * z - y * w)).abs()
        + (2. * (y * z + x * w)).abs()
        + (1. - 2. * (x * x + y * y)).abs())
}
pub async fn run(config: WorkerConfig, padded: bool) -> Result<Report> {
    let tol: Acceptance = serde_json::from_str(ACCEPTANCE)?;
    let temp = tempfile::tempdir()?;
    let core = Core::open(temp.path().join("app"), config).await?;
    let ws = core.create_workspace("physics-reference").await?;
    let base = core.get_project(&ws).await?["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), SOURCE.into())]),
            "physics-reference",
        )
        .await?;
    let padding_mm = if padded { tol.proxy_padding_mm } else { 0. };
    let mut definition: Value = serde_json::from_str(MECHANISM)?;
    if padded {
        let half = [(10. + padding_mm) * 0.001; 3];
        definition["collision"]["bodies"]["slider"] =
            json!([{"shape":{"type":"box","half_extents_m":half}}]);
    }
    let mechanism_source = serde_json::to_string(&definition)?;
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            kinematics::decode(&definition)?,
            "physics-reference",
        )
        .await?;
    let start = Instant::now();
    let job = core
        .build(&ws, &base, &draft.id, "physics-reference")
        .await?;
    let job = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let job = core.get_job(&ws, &job.id).await?;
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                break Ok::<_, Error>(job);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| failure("Reference build timed out"))??;
    if job.state != JobState::Succeeded {
        return Err(Error::Domain {
            code: "PHYSICS_BENCHMARK_FAILED".into(),
            message: "Reference build failed".into(),
            details: job.diagnostics,
        });
    }
    let build_ms = start.elapsed().as_secs_f64() * 1000.;
    let rev = job.resulting_revision.unwrap();
    let before = core.get_project(&ws).await?;
    let definition = serde_json::from_value(
        json!({"gravity_m_s2":[0,0,tol.gravity_m_s2],"bodies":{"stop":{"type":"fixed","friction":0.5,"restitution":0},"slider":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":tol.mass_kg},"friction":0.5,"restitution":0}}}),
    )?;
    let mut state = control(&core, &ws, &rev, 0, dynamics::Command::Start { definition }).await?;
    let mut timings = vec![];
    let mut phases = vec![];
    for (name, target, force) in [
        ("loaded_spring", 0.08, tol.max_force_n),
        ("ceiling_contact", 0.3, tol.max_force_n),
        ("lower_joint_limit", -0.3, tol.max_force_n),
        ("insufficient_force", 0.08, tol.insufficient_force_n),
    ] {
        phases.push(
            phase(
                &core,
                &ws,
                &rev,
                &mut state,
                Trial {
                    name,
                    target,
                    force,
                    padding_mm,
                },
                &tol,
                &mut timings,
            )
            .await?,
        );
    }
    let ticks = state.simulation.as_ref().unwrap().current.tick;
    let expected = 0.08 + tol.mass_kg * tol.gravity_m_s2 / tol.stiffness_n_m;
    let solver_settings = state.simulation.as_ref().unwrap().solver_settings.clone();
    let measured_force =
        phases[0].last_solver_motor_impulse_n_s.abs() / solver_settings.nominal_solver_substep_s;
    let time_s = state.simulation.as_ref().unwrap().current.time_s;
    let mut checks = vec![];
    let mut check = |name: String, measured: f64, maximum: f64, unit: &str| {
        checks.push(Check {
            name,
            measured,
            maximum,
            unit: unit.into(),
            passed: measured.is_finite() && measured <= maximum,
        });
    };
    check(
        "tick count".into(),
        (ticks as f64 - f64::from(4 * tol.ticks_per_phase)).abs(),
        0.,
        "ticks",
    );
    check(
        "simulation clock".into(),
        (time_s - f64::from(4 * tol.ticks_per_phase) * dynamics::TIMESTEP_S).abs(),
        1e-12,
        "s",
    );
    check(
        "loaded spring equilibrium".into(),
        (phases[0].settled_coordinate_m - expected).abs() * 1000.,
        tol.max_spring_equilibrium_error_mm,
        "mm",
    );
    check(
        "static motor force vs weight".into(),
        (measured_force.abs() - tol.mass_kg * tol.gravity_m_s2.abs()).abs(),
        tol.max_static_motor_force_error_n,
        "N",
    );
    check(
        "settled proxy contact gap".into(),
        phases[1].settled_proxy_gap_mm.max(0.),
        tol.max_settled_cad_gap_mm,
        "mm",
    );
    check(
        "settled proxy penetration".into(),
        (-phases[1].settled_proxy_gap_mm).max(0.),
        tol.max_settled_penetration_mm,
        "mm",
    );
    check(
        "settled exact CAD penetration".into(),
        (-phases[1].exact_cad_gap_mm).max(0.),
        tol.max_settled_penetration_mm,
        "mm",
    );
    check(
        "settled exact intersection volume".into(),
        phases[1].exact_cad_intersection_volume_mm3,
        400. * tol.max_settled_penetration_mm,
        "mm3",
    );
    check(
        "proxy/CAD clearance difference".into(),
        (phases[1].exact_cad_gap_mm - phases[1].settled_proxy_gap_mm - padding_mm).abs(),
        tol.max_proxy_clearance_comparison_error_mm,
        "mm",
    );
    if padded {
        check(
            "padded proxy leaves CAD nonintersecting".into(),
            phases[1].exact_cad_intersection_volume_mm3,
            0.,
            "mm3",
        );
    }
    for (index, p) in phases.iter().enumerate() {
        check(
            format!("{} target clamp", p.name),
            (p.applied_target_m - [0.08, 0.15, 0.01, 0.08][index]).abs(),
            1e-12,
            "m",
        );
        check(
            format!("{} peak limit error", p.name),
            p.max_limit_error_mm,
            tol.max_joint_limit_error_mm,
            "mm",
        );
        check(
            format!("{} peak penetration", p.name),
            p.max_proxy_penetration_mm,
            tol.max_transient_penetration_mm,
            "mm",
        );
        check(
            format!("{} axis drift", p.name),
            p.max_axis_drift_mm,
            tol.max_axis_drift_mm,
            "mm",
        );
        check(
            format!("{} rotation drift", p.name),
            p.max_rotation_error_rad,
            tol.max_rotation_error_rad,
            "rad",
        );
        check(
            format!("{} settled speed", p.name),
            p.settled_speed_m_s,
            tol.max_settled_speed_m_s,
            "m/s",
        );
        check(
            format!("{} solver impulse budget", p.name),
            (p.max_solver_motor_impulse_n_s
                - p.max_force_n * solver_settings.nominal_solver_substep_s)
                .max(0.),
            tol.motor_impulse_slack_n_s,
            "N s",
        );
    }
    check(
        "lower limit reached".into(),
        (phases[2].settled_coordinate_m - 0.01).abs() * 1000.,
        tol.max_joint_limit_error_mm,
        "mm",
    );
    check(
        "insufficient motor cannot lift load".into(),
        (phases[3].settled_coordinate_m - 0.01).abs() * 1000.,
        tol.max_joint_limit_error_mm,
        "mm",
    );
    let after = core.get_project(&ws).await?;
    let unchanged = before["revision"] == after["revision"]
        && before["model"] == after["model"]
        && before["parameters"] == after["parameters"]
        && before["activity"] == after["activity"];
    let stopped = control(
        &core,
        &ws,
        &rev,
        state.pose_version,
        dynamics::Command::Stop,
    )
    .await?;
    let restored = serde_json::to_value(&stopped.evaluation.poses)? == before["motion"]["poses"];
    let sum = timings.iter().sum::<f64>();
    let count = timings.len();
    timings.sort_by(f64::total_cmp);
    let mut hasher = Sha256::new();
    for source in [SOURCE, mechanism_source.as_str(), ACCEPTANCE] {
        hasher.update(source.as_bytes());
    }
    Ok(Report {
        passed: checks.iter().all(|check| check.passed) && unchanged && restored,
        fixture_sha256: format!("{:x}", hasher.finalize()),
        proxy_padding_mm: padding_mm,
        acceptance: tol,
        phases,
        checks,
        dependencies: before["model"]["dependencies"].clone(),
        solver_settings,
        timestep_s: dynamics::TIMESTEP_S,
        ticks,
        expected_loaded_equilibrium_m: expected,
        measured_static_motor_force_magnitude_n: measured_force,
        native_build_ms: build_ms,
        control_tick_us_mean: sum / count as f64,
        control_tick_us_p95: timings[(count * 95).div_ceil(100).saturating_sub(1)],
        control_tick_us_max: *timings.last().unwrap(),
        optimized: !cfg!(debug_assertions),
        design_revision_unchanged: unchanged,
        stop_restores_design_pose: restored,
    })
}
