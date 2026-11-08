use cadpad_core::{
    Core, Error, JobState, Result, dynamics, kinematics, motion, worker::WorkerConfig,
};
use glam::{DQuat, DVec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};

const SOURCE: &str = include_str!("../../../../fixtures/physics_friction.py");
const MECHANISM: &str = include_str!("../../../../fixtures/physics_friction.mechanism.json");
const ACCEPTANCE: &str = include_str!("../../../../fixtures/physics_friction.acceptance.json");
const ACTOR: &str = "friction-reference";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Acceptance {
    schema_version: u32,
    scales: Vec<f64>,
    base_mass_kg: f64,
    base_block_half_extents_mm: [f64; 3],
    gravity_m_s2: f64,
    friction: f64,
    base_hold_force_n: f64,
    base_slide_force_n: f64,
    settle_ticks: u32,
    hold_ticks: u32,
    slide_ticks: u32,
    stop_ticks: u32,
    frictionless_ticks: u32,
    long_run_ticks: u32,
    max_hold_displacement_mm: f64,
    max_settled_speed_m_s: f64,
    max_slide_velocity_error_m_s: f64,
    max_slide_position_error_mm: f64,
    max_stopping_distance_error_mm: f64,
    max_translational_work_error_j: f64,
    max_frictionless_velocity_error_m_s: f64,
    max_long_run_drift_mm: f64,
    max_transient_penetration_mm: f64,
    max_settled_penetration_mm: f64,
    max_settled_gap_mm: f64,
    max_lateral_drift_mm: f64,
    max_rotation_rad: f64,
}
#[derive(Serialize)]
pub struct Check {
    name: String,
    measured: f64,
    maximum: f64,
    unit: String,
    passed: bool,
}
#[derive(Serialize)]
pub struct Phase {
    name: String,
    friction: f64,
    applied_force_n: f64,
    start_x_mm: f64,
    start_velocity_x_m_s: f64,
    final_x_mm: f64,
    velocity_x_m_s: f64,
    speed_m_s: f64,
    max_penetration_mm: f64,
    max_lateral_drift_mm: f64,
    max_rotation_rad: f64,
    exact_gap_mm: f64,
    exact_distance_mm: f64,
    exact_intersection_volume_mm3: f64,
    pose_version: u64,
}
#[derive(Serialize)]
pub struct Report {
    pub passed: bool,
    scale: f64,
    mass_kg: f64,
    hold_force_n: f64,
    slide_force_n: f64,
    acceptance: Acceptance,
    fixture_sha256: String,
    static_friction_threshold_n: f64,
    expected_slide_acceleration_m_s2: f64,
    expected_slide_velocity_m_s: f64,
    expected_stop_distance_mm: f64,
    phases: Vec<Phase>,
    checks: Vec<Check>,
    dependencies: Value,
    timestep_s: f64,
    solver_settings: dynamics::SolverSettings,
    optimized: bool,
    design_revision_unchanged: bool,
    stop_restores_design_pose: bool,
    containment_required: bool,
    worker_executable: std::path::PathBuf,
    guardian_executable: std::path::PathBuf,
}
struct Trial {
    name: &'static str,
    friction: f64,
    force: f64,
    ticks: u32,
}
struct Case {
    mass_kg: f64,
    hold_force_n: f64,
    slide_force_n: f64,
    half_extents_mm: [f64; 3],
    length_scale: f64,
    force_scale: f64,
}
fn failure(message: impl Into<String>) -> Error {
    Error::domain("FRICTION_BENCHMARK_FAILED", message)
}
async fn control(
    core: &Core,
    ws: &str,
    revision: &str,
    version: u64,
    command: dynamics::Command,
) -> Result<motion::Snapshot> {
    core.control_simulation(
        ACTOR,
        dynamics::Control {
            workspace_id: ws.into(),
            revision: revision.into(),
            expected_pose_version: version,
            command,
        },
    )
    .await
}
fn definition(friction: f64, mass_kg: f64, gravity: f64) -> Result<dynamics::Definition> {
    Ok(serde_json::from_value(
        json!({"gravity_m_s2":[0,0,-gravity],"bodies":{
            "floor":{"type":"fixed","friction":friction,"restitution":0},
            "block":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":mass_kg},"friction":friction,"restitution":0}
        }}),
    )?)
}
fn loads(force: f64) -> dynamics::Command {
    dynamics::Command::Loads {
        loads: if force == 0. {
            BTreeMap::new()
        } else {
            BTreeMap::from([(
                "block".into(),
                dynamics::Load {
                    force_n: [force, 0., 0.],
                    torque_nm: [0.; 3],
                },
            )])
        },
    }
}
async fn phase(
    core: &Core,
    ws: &str,
    rev: &str,
    state: &mut motion::Snapshot,
    trial: Trial,
    half_extents_mm: [f64; 3],
) -> Result<Phase> {
    let Trial {
        name,
        friction,
        force,
        ticks,
    } = trial;
    *state = control(core, ws, rev, state.pose_version, loads(force)).await?;
    let start_x_mm = state.evaluation.poses["block"].position[0];
    let start_velocity_x_m_s = state
        .simulation
        .as_ref()
        .unwrap()
        .current
        .linear_velocity_m_s["block"][0];
    let (mut penetration, mut lateral, mut rotation) = (0_f64, 0_f64, 0_f64);
    for _ in 0..ticks {
        *state = control(
            core,
            ws,
            rev,
            state.pose_version,
            dynamics::Command::Advance { steps: 1 },
        )
        .await?;
        let p = &state.evaluation.poses["block"];
        let q = DQuat::from_array(p.quaternion);
        let support = (q * DVec3::X).z.abs() * half_extents_mm[0]
            + (q * DVec3::Y).z.abs() * half_extents_mm[1]
            + (q * DVec3::Z).z.abs() * half_extents_mm[2];
        penetration = penetration.max((support - p.position[2]).max(0.));
        lateral = lateral.max(p.position[1].abs());
        rotation = rotation.max(2. * q.xyz().length().atan2(q.w.abs()));
    }
    let exact=core.query(ws,rev,"inspect",json!({"pose_version":state.pose_version,"entities":[{"revision":rev,"instance_id":"block","entity_id":"part:0"},{"revision":rev,"instance_id":"floor","entity_id":"part:0"}]})).await?;
    let pair=core.query(ws,rev,"inspect",json!({"pose_version":state.pose_version,"query":"interference","pairs":[["block","floor"]]})).await?;
    let scalar = |value: &Value| {
        value
            .as_f64()
            .ok_or_else(|| failure("Missing exact CAD scalar"))
    };
    let velocity = state
        .simulation
        .as_ref()
        .unwrap()
        .current
        .linear_velocity_m_s["block"];
    Ok(Phase {
        name: name.into(),
        friction,
        applied_force_n: force,
        start_x_mm,
        start_velocity_x_m_s,
        final_x_mm: state.evaluation.poses["block"].position[0],
        velocity_x_m_s: velocity[0],
        speed_m_s: DVec3::from_array(velocity).length(),
        max_penetration_mm: penetration,
        max_lateral_drift_mm: lateral,
        max_rotation_rad: rotation,
        exact_gap_mm: scalar(&exact["entities"][0]["world_bounds"]["min"][2])?
            - scalar(&exact["entities"][1]["world_bounds"]["max"][2])?,
        exact_distance_mm: scalar(&pair["pairs"][0]["distance_mm"])?,
        exact_intersection_volume_mm3: scalar(&pair["pairs"][0]["intersection_volume_mm3"])?,
        pose_version: state.pose_version,
    })
}
pub async fn run(config: WorkerConfig, scale: f64) -> Result<Report> {
    if config.containment != cadpad_core::worker::Containment::Required {
        return Err(failure("Reference requires the default contained worker"));
    }
    let worker_executable = config.executable.clone();
    let guardian_executable = config.guardian_executable.clone();
    let tol: Acceptance = serde_json::from_str(ACCEPTANCE)?;
    if tol.schema_version != 1 {
        return Err(failure("Unsupported acceptance inputs"));
    }
    if !tol.scales.contains(&scale) {
        return Err(failure(format!("Unsupported friction scale {scale}")));
    }
    let case = Case {
        mass_kg: tol.base_mass_kg * scale.powi(3),
        hold_force_n: tol.base_hold_force_n * scale.powi(3),
        slide_force_n: tol.base_slide_force_n * scale.powi(3),
        half_extents_mm: tol.base_block_half_extents_mm.map(|value| value * scale),
        length_scale: scale,
        force_scale: scale.powi(3),
    };
    let source = SOURCE.replace("SCALE = 1.0", &format!("SCALE = {scale:?}"));
    let temporary = tempfile::tempdir()?;
    let core = Core::open(temporary.path().join("app"), config).await?;
    let ws = core.create_workspace(ACTOR).await?;
    let base = core.get_project(&ws).await?["revision"]
        .as_str()
        .unwrap()
        .to_string();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([("model.py".into(), source)]),
            ACTOR,
        )
        .await?;
    let mechanism: Value = serde_json::from_str(MECHANISM)?;
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            kinematics::decode(&mechanism)?,
            ACTOR,
        )
        .await?;
    let job = core.build(&ws, &base, &draft.id, ACTOR).await?;
    let completed = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let job = core.get_job(&ws, &job.id).await?;
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                return Ok::<_, Error>(job);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| failure("Native build timed out"))??;
    if completed.state != JobState::Succeeded {
        return Err(failure(format!(
            "Native fixture failed: {:?}",
            completed.diagnostics
        )));
    }
    let rev = completed.resulting_revision.unwrap();
    let before = core.get_project(&ws).await?;
    let original = core.get_motion(&ws, &rev).await?;
    let mut state = control(
        &core,
        &ws,
        &rev,
        original.pose_version,
        dynamics::Command::Start {
            definition: definition(tol.friction, case.mass_kg, tol.gravity_m_s2)?,
        },
    )
    .await?;
    let solver_settings = state.simulation.as_ref().unwrap().solver_settings.clone();
    let half = case.half_extents_mm;
    let mut phases = vec![
        phase(
            &core,
            &ws,
            &rev,
            &mut state,
            Trial {
                name: "settle",
                friction: tol.friction,
                force: 0.,
                ticks: tol.settle_ticks,
            },
            half,
        )
        .await?,
    ];
    phases.push(
        phase(
            &core,
            &ws,
            &rev,
            &mut state,
            Trial {
                name: "hold",
                friction: tol.friction,
                force: case.hold_force_n,
                ticks: tol.hold_ticks,
            },
            half,
        )
        .await?,
    );
    phases.push(
        phase(
            &core,
            &ws,
            &rev,
            &mut state,
            Trial {
                name: "slide",
                friction: tol.friction,
                force: case.slide_force_n,
                ticks: tol.slide_ticks,
            },
            half,
        )
        .await?,
    );
    phases.push(
        phase(
            &core,
            &ws,
            &rev,
            &mut state,
            Trial {
                name: "stop",
                friction: tol.friction,
                force: 0.,
                ticks: tol.stop_ticks,
            },
            half,
        )
        .await?,
    );
    phases.push(
        phase(
            &core,
            &ws,
            &rev,
            &mut state,
            Trial {
                name: "long_run",
                friction: tol.friction,
                force: 0.,
                ticks: tol.long_run_ticks,
            },
            half,
        )
        .await?,
    );
    let stopped = control(
        &core,
        &ws,
        &rev,
        state.pose_version,
        dynamics::Command::Stop,
    )
    .await?;
    let restored = serde_json::to_value(&stopped.evaluation.poses)?
        == serde_json::to_value(&original.evaluation.poses)?;
    state = control(
        &core,
        &ws,
        &rev,
        stopped.pose_version,
        dynamics::Command::Start {
            definition: definition(0., case.mass_kg, tol.gravity_m_s2)?,
        },
    )
    .await?;
    phases.push(
        phase(
            &core,
            &ws,
            &rev,
            &mut state,
            Trial {
                name: "frictionless_settle",
                friction: 0.,
                force: 0.,
                ticks: tol.settle_ticks,
            },
            half,
        )
        .await?,
    );
    phases.push(
        phase(
            &core,
            &ws,
            &rev,
            &mut state,
            Trial {
                name: "frictionless_slide",
                friction: 0.,
                force: case.hold_force_n,
                ticks: tol.frictionless_ticks,
            },
            half,
        )
        .await?,
    );
    let stopped = control(
        &core,
        &ws,
        &rev,
        state.pose_version,
        dynamics::Command::Stop,
    )
    .await?;
    let restored = restored
        && serde_json::to_value(&stopped.evaluation.poses)?
            == serde_json::to_value(&original.evaluation.poses)?;
    let after = core.get_project(&ws).await?;
    let unchanged = before["revision"] == after["revision"]
        && before["activity"] == after["activity"]
        && before["parameters"] == after["parameters"];
    let slide = &phases[2];
    let stop = &phases[3];
    let long_run = &phases[4];
    let frictionless_slide = &phases[6];
    let time = f64::from(tol.slide_ticks) * dynamics::TIMESTEP_S;
    let acceleration = case.slide_force_n / case.mass_kg - tol.friction * tol.gravity_m_s2;
    let expected_velocity = acceleration * time;
    let expected_stop =
        slide.velocity_x_m_s.powi(2) / (2. * tol.friction * tol.gravity_m_s2) * 1000.;
    let length = case.length_scale;
    let energy = case.force_scale;
    let mut checks = vec![];
    let mut check = |name: String, measured: f64, maximum: f64, unit: &str| {
        checks.push(Check {
            name,
            measured,
            maximum,
            unit: unit.into(),
            passed: measured.is_finite() && measured <= maximum,
        })
    };
    check(
        "hold displacement".into(),
        (phases[1].final_x_mm - phases[1].start_x_mm).abs(),
        tol.max_hold_displacement_mm * length,
        "mm",
    );
    check(
        "hold speed".into(),
        phases[1].speed_m_s,
        tol.max_settled_speed_m_s,
        "m/s",
    );
    check(
        "slide velocity error".into(),
        (slide.velocity_x_m_s - expected_velocity).abs(),
        tol.max_slide_velocity_error_m_s,
        "m/s",
    );
    check(
        "slide position error".into(),
        (slide.final_x_mm - slide.start_x_mm - 0.5 * acceleration * time * time * 1000.).abs(),
        tol.max_slide_position_error_mm * length,
        "mm",
    );
    check(
        "stop distance error".into(),
        (stop.final_x_mm - stop.start_x_mm - expected_stop).abs(),
        tol.max_stopping_distance_error_mm * length,
        "mm",
    );
    check(
        "stop speed".into(),
        stop.speed_m_s,
        tol.max_settled_speed_m_s,
        "m/s",
    );
    let net_work = (case.slide_force_n - tol.friction * case.mass_kg * tol.gravity_m_s2)
        * (slide.final_x_mm - slide.start_x_mm)
        * 0.001;
    check(
        "translational work error".into(),
        (0.5 * case.mass_kg * slide.velocity_x_m_s.powi(2) - net_work).abs(),
        tol.max_translational_work_error_j * energy,
        "J",
    );
    check(
        "long-run drift".into(),
        (long_run.final_x_mm - long_run.start_x_mm).abs(),
        tol.max_long_run_drift_mm * length,
        "mm",
    );
    check(
        "long-run speed".into(),
        long_run.speed_m_s,
        tol.max_settled_speed_m_s,
        "m/s",
    );
    check(
        "frictionless velocity error".into(),
        (frictionless_slide.velocity_x_m_s
            - frictionless_slide.start_velocity_x_m_s
            - case.hold_force_n / case.mass_kg
                * f64::from(tol.frictionless_ticks)
                * dynamics::TIMESTEP_S)
            .abs(),
        tol.max_frictionless_velocity_error_m_s,
        "m/s",
    );
    for p in &phases {
        check(
            format!("{} transient penetration", p.name),
            p.max_penetration_mm,
            tol.max_transient_penetration_mm * length,
            "mm",
        );
        check(
            format!("{} settled penetration", p.name),
            (-p.exact_gap_mm).max(0.),
            tol.max_settled_penetration_mm * length,
            "mm",
        );
        check(
            format!("{} settled gap", p.name),
            p.exact_gap_mm.max(0.),
            tol.max_settled_gap_mm * length,
            "mm",
        );
        check(
            format!("{} lateral drift", p.name),
            p.max_lateral_drift_mm,
            tol.max_lateral_drift_mm * length,
            "mm",
        );
        check(
            format!("{} rotation", p.name),
            p.max_rotation_rad,
            tol.max_rotation_rad,
            "rad",
        );
    }
    Ok(Report {
        passed: unchanged && restored && checks.iter().all(|c| c.passed),
        scale,
        mass_kg: case.mass_kg,
        hold_force_n: case.hold_force_n,
        slide_force_n: case.slide_force_n,
        fixture_sha256: format!(
            "{:x}",
            Sha256::digest([SOURCE, MECHANISM, ACCEPTANCE].concat().as_bytes())
        ),
        static_friction_threshold_n: tol.friction * case.mass_kg * tol.gravity_m_s2,
        expected_slide_acceleration_m_s2: acceleration,
        expected_slide_velocity_m_s: expected_velocity,
        expected_stop_distance_mm: expected_stop,
        acceptance: tol,
        phases,
        checks,
        dependencies: before["dependencies"].clone(),
        timestep_s: dynamics::TIMESTEP_S,
        solver_settings,
        optimized: !cfg!(debug_assertions),
        design_revision_unchanged: unchanged,
        stop_restores_design_pose: restored,
        containment_required: true,
        worker_executable,
        guardian_executable,
    })
}
