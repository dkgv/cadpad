use cadpad_core::{
    Core, Error, JobState, Result, dynamics, kinematics, motion, worker::WorkerConfig,
};
use glam::{DQuat, DVec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};

const SOURCE: &str = include_str!("../../../../fixtures/physics_stack.py");
const MECHANISM: &str = include_str!("../../../../fixtures/physics_stack.mechanism.json");
const ACCEPTANCE: &str = include_str!("../../../../fixtures/physics_stack.acceptance.json");
const ACTOR: &str = "stack-reference";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Acceptance {
    schema_version: u32,
    scales: Vec<f64>,
    base_mass_kg: f64,
    base_half_extents_mm: [f64; 3],
    gravity_m_s2: f64,
    friction: f64,
    base_hold_force_n: f64,
    base_slide_force_n: f64,
    settle_ticks: u32,
    hold_ticks: u32,
    slide_ticks: u32,
    stop_ticks: u32,
    long_run_ticks: u32,
    max_settled_speed_m_s: f64,
    max_hold_relative_displacement_mm: f64,
    max_hold_lower_displacement_mm: f64,
    max_slide_relative_velocity_error_m_s: f64,
    max_lower_slide_speed_m_s: f64,
    max_settled_height_error_mm: f64,
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
pub struct BodySample {
    x_mm: f64,
    z_mm: f64,
    velocity_x_m_s: f64,
    speed_m_s: f64,
}
#[derive(Serialize)]
pub struct Phase {
    name: String,
    applied_force_n: f64,
    start_upper: BodySample,
    start_lower: BodySample,
    final_upper: BodySample,
    final_lower: BodySample,
    max_penetration_mm: f64,
    max_lateral_drift_mm: f64,
    max_rotation_rad: f64,
    lower_floor_gap_mm: f64,
    upper_lower_gap_mm: f64,
    lower_floor_distance_mm: f64,
    upper_lower_distance_mm: f64,
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
    interface_friction_threshold_n: f64,
    expected_slide_relative_acceleration_m_s2: f64,
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
    force: f64,
    ticks: u32,
}
struct Case {
    mass_kg: f64,
    hold_force_n: f64,
    slide_force_n: f64,
    half_extents_mm: [f64; 3],
    length_scale: f64,
}
fn failure(message: impl Into<String>) -> Error {
    Error::domain("STACK_BENCHMARK_FAILED", message)
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
    Ok(serde_json::from_value(json!({
        "gravity_m_s2": [0, 0, -gravity],
        "bodies": {
            "floor": {"type": "fixed", "friction": friction, "restitution": 0},
            "lower": {
                "type": "dynamic",
                "mass": {"type": "kilograms", "mass_kg": mass_kg},
                "friction": friction,
                "restitution": 0
            },
            "upper": {
                "type": "dynamic",
                "mass": {"type": "kilograms", "mass_kg": mass_kg},
                "friction": friction,
                "restitution": 0
            }
        }
    }))?)
}
fn loads(force: f64) -> dynamics::Command {
    dynamics::Command::Loads {
        loads: if force == 0. {
            BTreeMap::new()
        } else {
            BTreeMap::from([(
                "upper".into(),
                dynamics::Load {
                    force_n: [force, 0., 0.],
                    torque_nm: [0.; 3],
                },
            )])
        },
    }
}
fn sample(state: &motion::Snapshot, name: &str) -> BodySample {
    let pose = &state.evaluation.poses[name];
    let velocity = state
        .simulation
        .as_ref()
        .unwrap()
        .current
        .linear_velocity_m_s[name];
    BodySample {
        x_mm: pose.position[0],
        z_mm: pose.position[2],
        velocity_x_m_s: velocity[0],
        speed_m_s: DVec3::from_array(velocity).length(),
    }
}
fn support_z(pose: &kinematics::Pose, half: [f64; 3]) -> f64 {
    let q = DQuat::from_array(pose.quaternion);
    (q * DVec3::X).z.abs() * half[0]
        + (q * DVec3::Y).z.abs() * half[1]
        + (q * DVec3::Z).z.abs() * half[2]
}
fn rotation(pose: &kinematics::Pose) -> f64 {
    let q = DQuat::from_array(pose.quaternion);
    2. * q.xyz().length().atan2(q.w.abs())
}
async fn phase(
    core: &Core,
    ws: &str,
    rev: &str,
    state: &mut motion::Snapshot,
    trial: Trial,
    half: [f64; 3],
) -> Result<Phase> {
    let Trial { name, force, ticks } = trial;
    *state = control(core, ws, rev, state.pose_version, loads(force)).await?;
    let start_upper = sample(state, "upper");
    let start_lower = sample(state, "lower");
    let (mut penetration, mut lateral, mut rot) = (0_f64, 0_f64, 0_f64);
    for _ in 0..ticks {
        *state = control(
            core,
            ws,
            rev,
            state.pose_version,
            dynamics::Command::Advance { steps: 1 },
        )
        .await?;
        let lower = &state.evaluation.poses["lower"];
        let upper = &state.evaluation.poses["upper"];
        let lower_support = support_z(lower, half);
        let upper_support = support_z(upper, half);
        penetration = penetration.max((lower_support - lower.position[2]).max(0.));
        let lateral_offset = (upper.position[0] - lower.position[0]).abs();
        if lateral_offset < half[0] {
            penetration = penetration.max(
                (upper_support + lower_support - (upper.position[2] - lower.position[2])).max(0.),
            );
        }
        lateral = lateral
            .max(lower.position[1].abs())
            .max(upper.position[1].abs());
        rot = rot.max(rotation(lower)).max(rotation(upper));
    }
    let exact = core
        .query(
            ws,
            rev,
            "inspect",
            json!({
                "pose_version": state.pose_version,
                "entities": [
                    {"revision": rev, "instance_id": "lower", "entity_id": "part:0"},
                    {"revision": rev, "instance_id": "floor", "entity_id": "part:0"},
                    {"revision": rev, "instance_id": "upper", "entity_id": "part:0"}
                ]
            }),
        )
        .await?;
    let pairs = core
        .query(
            ws,
            rev,
            "inspect",
            json!({
                "pose_version": state.pose_version,
                "query": "interference",
                "pairs": [["lower", "floor"], ["upper", "lower"]]
            }),
        )
        .await?;
    let scalar = |value: &Value| {
        value
            .as_f64()
            .ok_or_else(|| failure("Missing exact CAD scalar"))
    };
    Ok(Phase {
        name: name.into(),
        applied_force_n: force,
        start_upper,
        start_lower,
        final_upper: sample(state, "upper"),
        final_lower: sample(state, "lower"),
        max_penetration_mm: penetration,
        max_lateral_drift_mm: lateral,
        max_rotation_rad: rot,
        lower_floor_gap_mm: scalar(&exact["entities"][0]["world_bounds"]["min"][2])?
            - scalar(&exact["entities"][1]["world_bounds"]["max"][2])?,
        upper_lower_gap_mm: scalar(&exact["entities"][2]["world_bounds"]["min"][2])?
            - scalar(&exact["entities"][0]["world_bounds"]["max"][2])?,
        lower_floor_distance_mm: scalar(&pairs["pairs"][0]["distance_mm"])?,
        upper_lower_distance_mm: scalar(&pairs["pairs"][1]["distance_mm"])?,
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
        return Err(failure(format!("Unsupported stack scale {scale}")));
    }
    let case = Case {
        mass_kg: tol.base_mass_kg * scale.powi(3),
        hold_force_n: tol.base_hold_force_n * scale.powi(3),
        slide_force_n: tol.base_slide_force_n * scale.powi(3),
        half_extents_mm: tol.base_half_extents_mm.map(|value| value * scale),
        length_scale: scale,
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
    let mut phases = Vec::new();
    for trial in [
        Trial {
            name: "settle",
            force: 0.,
            ticks: tol.settle_ticks,
        },
        Trial {
            name: "hold",
            force: case.hold_force_n,
            ticks: tol.hold_ticks,
        },
        Trial {
            name: "slide",
            force: case.slide_force_n,
            ticks: tol.slide_ticks,
        },
        Trial {
            name: "stop",
            force: 0.,
            ticks: tol.stop_ticks,
        },
        Trial {
            name: "long_run",
            force: 0.,
            ticks: tol.long_run_ticks,
        },
    ] {
        phases.push(phase(&core, &ws, &rev, &mut state, trial, half).await?);
    }
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
    let after = core.get_project(&ws).await?;
    let unchanged = before["revision"] == after["revision"]
        && before["activity"] == after["activity"]
        && before["parameters"] == after["parameters"];
    let settle = &phases[0];
    let hold = &phases[1];
    let slide = &phases[2];
    let stop = &phases[3];
    let long_run = &phases[4];
    let length = case.length_scale;
    let expected_z_lower = case.half_extents_mm[2];
    let expected_z_upper = 3. * case.half_extents_mm[2];
    let interface = tol.friction * case.mass_kg * tol.gravity_m_s2;
    let relative_acceleration = case.slide_force_n / case.mass_kg - tol.friction * tol.gravity_m_s2;
    let slide_time = f64::from(tol.slide_ticks) * dynamics::TIMESTEP_S;
    let expected_relative_velocity = relative_acceleration * slide_time;
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
        "settle lower height error".into(),
        (settle.final_lower.z_mm - expected_z_lower).abs(),
        tol.max_settled_height_error_mm * length,
        "mm",
    );
    check(
        "settle upper height error".into(),
        (settle.final_upper.z_mm - expected_z_upper).abs(),
        tol.max_settled_height_error_mm * length,
        "mm",
    );
    check(
        "settle lower speed".into(),
        settle.final_lower.speed_m_s,
        tol.max_settled_speed_m_s,
        "m/s",
    );
    check(
        "settle upper speed".into(),
        settle.final_upper.speed_m_s,
        tol.max_settled_speed_m_s,
        "m/s",
    );
    check(
        "hold relative displacement".into(),
        ((hold.final_upper.x_mm - hold.final_lower.x_mm)
            - (hold.start_upper.x_mm - hold.start_lower.x_mm))
            .abs(),
        tol.max_hold_relative_displacement_mm * length,
        "mm",
    );
    check(
        "hold lower displacement".into(),
        (hold.final_lower.x_mm - hold.start_lower.x_mm).abs(),
        tol.max_hold_lower_displacement_mm * length,
        "mm",
    );
    check(
        "hold upper speed".into(),
        hold.final_upper.speed_m_s,
        tol.max_settled_speed_m_s,
        "m/s",
    );
    check(
        "slide relative velocity error".into(),
        ((slide.final_upper.velocity_x_m_s - slide.final_lower.velocity_x_m_s)
            - expected_relative_velocity)
            .abs(),
        tol.max_slide_relative_velocity_error_m_s,
        "m/s",
    );
    check(
        "slide lower speed".into(),
        slide.final_lower.speed_m_s,
        tol.max_lower_slide_speed_m_s,
        "m/s",
    );
    check(
        "stop speed".into(),
        stop.final_upper.speed_m_s.max(stop.final_lower.speed_m_s),
        tol.max_settled_speed_m_s,
        "m/s",
    );
    check(
        "long-run lower drift".into(),
        (long_run.final_lower.x_mm - long_run.start_lower.x_mm).abs(),
        tol.max_long_run_drift_mm * length,
        "mm",
    );
    check(
        "long-run upper relative drift".into(),
        ((long_run.final_upper.x_mm - long_run.final_lower.x_mm)
            - (long_run.start_upper.x_mm - long_run.start_lower.x_mm))
            .abs(),
        tol.max_long_run_drift_mm * length,
        "mm",
    );
    check(
        "long-run speed".into(),
        long_run
            .final_upper
            .speed_m_s
            .max(long_run.final_lower.speed_m_s),
        tol.max_settled_speed_m_s,
        "m/s",
    );
    for p in &phases {
        check(
            format!("{} transient penetration", p.name),
            p.max_penetration_mm,
            tol.max_transient_penetration_mm * length,
            "mm",
        );
        let lower_floor_penetration = if p.lower_floor_distance_mm <= 1e-9 {
            (-p.lower_floor_gap_mm).max(0.)
        } else {
            0.
        };
        let upper_lower_penetration = if p.upper_lower_distance_mm <= 1e-9 {
            (-p.upper_lower_gap_mm).max(0.)
        } else {
            0.
        };
        check(
            format!("{} lower-floor settled penetration", p.name),
            lower_floor_penetration,
            tol.max_settled_penetration_mm * length,
            "mm",
        );
        check(
            format!("{} upper-lower settled penetration", p.name),
            upper_lower_penetration,
            tol.max_settled_penetration_mm * length,
            "mm",
        );
        check(
            format!("{} lower-floor settled gap", p.name),
            if p.lower_floor_distance_mm <= 1e-9 {
                p.lower_floor_gap_mm.max(0.)
            } else {
                0.
            },
            tol.max_settled_gap_mm * length,
            "mm",
        );
        check(
            format!("{} upper-lower settled gap", p.name),
            if p.upper_lower_distance_mm <= 1e-9 {
                p.upper_lower_gap_mm.max(0.)
            } else {
                0.
            },
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
        acceptance: tol,
        fixture_sha256: format!(
            "{:x}",
            Sha256::digest([SOURCE, MECHANISM, ACCEPTANCE].concat().as_bytes())
        ),
        interface_friction_threshold_n: interface,
        expected_slide_relative_acceleration_m_s2: relative_acceleration,
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
