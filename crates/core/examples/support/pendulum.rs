use cadpad_core::{
    Core, Error, JobState, Result, dynamics, kinematics, motion, worker::WorkerConfig,
};
use glam::{DQuat, DVec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};

const SOURCE: &str = include_str!("../../../../fixtures/physics_pendulum.py");
const MECHANISM: &str = include_str!("../../../../fixtures/physics_pendulum.mechanism.json");
const ACCEPTANCE: &str = include_str!("../../../../fixtures/physics_pendulum.acceptance.json");

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Acceptance {
    schema_version: u32,
    scales: Vec<f64>,
    amplitudes_rad: Vec<f64>,
    base_dimensions_m: [f64; 3],
    base_mass_kg: f64,
    gravity_m_s2: f64,
    periods: u32,
    minimum_measured_periods: usize,
    max_period_relative_error: f64,
    max_energy_relative_drift: f64,
    max_pivot_drift_mm: f64,
    max_axis_tilt_rad: f64,
    max_exact_bounds_error_mm: f64,
    max_joint_limit_error_rad: f64,
}

#[derive(Serialize)]
struct Check {
    name: String,
    measured: f64,
    maximum: f64,
    unit: String,
    passed: bool,
}
fn check(checks: &mut Vec<Check>, name: &str, measured: f64, maximum: f64, unit: &str) {
    checks.push(Check {
        name: name.into(),
        measured,
        maximum,
        unit: unit.into(),
        passed: measured.is_finite() && measured <= maximum,
    });
}

// Independent analytic oracle: T = 2π sqrt(I_pivot/(mgd)) / AGM(1, cos(A/2)).
// MIT 8.01SC chapter 24 supplies the physical-pendulum equation; NIST DLMF
// 19.8.5 supplies K(k) = π/(2 AGM(1, sqrt(1-k²))). No solver state is used.
pub fn period(inertia: f64, mass: f64, gravity: f64, offset: f64, amplitude: f64) -> f64 {
    let (mut a, mut b) = (1., (amplitude / 2.).cos());
    for _ in 0..16 {
        (a, b) = ((a + b) / 2., (a * b).sqrt());
    }
    std::f64::consts::TAU * (inertia / (mass * gravity * offset)).sqrt() / a
}

async fn control(
    core: &Core,
    ws: &str,
    rev: &str,
    version: u64,
    command: dynamics::Command,
) -> Result<motion::Snapshot> {
    core.control_simulation(
        "pendulum-reference",
        dynamics::Control {
            workspace_id: ws.into(),
            revision: rev.into(),
            expected_pose_version: version,
            command,
        },
    )
    .await
}

pub async fn run(config: WorkerConfig, scale: f64, amplitude: f64) -> Result<Value> {
    let tol: Acceptance = serde_json::from_str(ACCEPTANCE)?;
    if !tol.scales.contains(&scale) || !tol.amplitudes_rad.contains(&amplitude) {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Scale and amplitude must match predeclared pendulum cases",
        ));
    }
    let source = SOURCE.replace("SCALE = 1.0", &format!("SCALE = {scale:?}"));
    let mut definition: Value = serde_json::from_str(MECHANISM)?;
    let mass = tol.base_mass_kg * scale.powi(3);
    let dimensions = tol.base_dimensions_m.map(|length| length * scale);
    let offset = dimensions[2] / 2.;
    definition["joints"][0]["child_frame"]["position_m"] = json!([0, 0, offset]);
    definition["joints"][0]["initial"] = json!(amplitude);
    let mechanism_source = serde_json::to_string(&definition)?;
    // Uniform rectangular solid, not a point bob: include both center inertia
    // and the parallel-axis contribution when predicting gravitational response.
    let center_inertia = [
        mass * (dimensions[1].powi(2) + dimensions[2].powi(2)) / 12.,
        mass * (dimensions[0].powi(2) + dimensions[2].powi(2)) / 12.,
        mass * (dimensions[0].powi(2) + dimensions[1].powi(2)) / 12.,
    ];
    let pivot_inertia = center_inertia[1] + mass * offset.powi(2);
    let expected_period = period(pivot_inertia, mass, tol.gravity_m_s2, offset, amplitude);
    let initial_energy = mass * tol.gravity_m_s2 * offset * (1. - amplitude.cos());
    let ticks = (f64::from(tol.periods) * expected_period / dynamics::TIMESTEP_S).ceil() as u32;
    let temp = tempfile::tempdir()?;
    let core = Core::open(temp.path().join("app"), config).await?;
    let ws = core.create_workspace("pendulum-reference").await?;
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
            "pendulum-reference",
        )
        .await?;
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            kinematics::decode(&definition)?,
            "pendulum-reference",
        )
        .await?;
    let job = core
        .build(&ws, &base, &draft.id, "pendulum-reference")
        .await?;
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
    .map_err(|_| Error::domain("PHYSICS_BENCHMARK_FAILED", "Pendulum build timed out"))??;
    if job.state != JobState::Succeeded {
        return Err(Error::Domain {
            code: "PHYSICS_BENCHMARK_FAILED".into(),
            message: "Pendulum fixture build failed".into(),
            details: job.diagnostics,
        });
    }
    let rev = job.resulting_revision.unwrap();
    let before = core.get_project(&ws).await?;
    let saved_source = core.read_source(&ws, &rev, "model.py").await?;
    let initial = core.get_motion(&ws, &rev).await?;
    let mut state = control(&core, &ws, &rev, initial.pose_version, dynamics::Command::Start {
        definition: serde_json::from_value(json!({
            "gravity_m_s2":[0,0,-tol.gravity_m_s2],
            "bodies":{"pendulum":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":mass},"friction":0,"restitution":0}}
        }))?,
    }).await?;
    let mut checks = Vec::new();
    let mut crossings = Vec::new();
    let mut previous_angle = amplitude;
    let mut pivot_drift: f64 = 0.;
    let mut axis_tilt: f64 = 0.;
    let mut energy_drift: f64 = 0.;
    let mut limit_error: f64 = 0.;
    let mut finite = true;
    for tick in 1..=ticks {
        state = control(
            &core,
            &ws,
            &rev,
            state.pose_version,
            dynamics::Command::Advance { steps: 1 },
        )
        .await?;
        let angle = state.evaluation.joints[0].coordinate;
        if previous_angle > 0. && angle <= 0. {
            let fraction = previous_angle / (previous_angle - angle);
            crossings.push((f64::from(tick - 1) + fraction) * dynamics::TIMESTEP_S);
        }
        previous_angle = angle;
        let pose = &state.evaluation.poses["pendulum"];
        let rotation = DQuat::from_array(pose.quaternion);
        let center = DVec3::from_array(pose.position) * 0.001;
        let pivot = center + rotation * DVec3::new(0., 0., offset);
        pivot_drift = pivot_drift.max(pivot.length() * 1000.);
        let axis = rotation * DVec3::Y;
        axis_tilt = axis_tilt.max(axis.cross(DVec3::Y).length().atan2(axis.y));
        limit_error = limit_error.max((angle.abs() - 0.8).max(0.));
        let frame = &state.simulation.as_ref().unwrap().current;
        let velocity = DVec3::from_array(frame.linear_velocity_m_s["pendulum"]);
        let angular =
            rotation.inverse() * DVec3::from_array(frame.angular_velocity_rad_s["pendulum"]);
        let kinetic = 0.5 * mass * velocity.length_squared()
            + 0.5 * DVec3::from_array(center_inertia).dot(angular * angular);
        // Use measured COM height and linear velocity, not an assumed pinned
        // trajectory. This also exposes constraint error in the energy balance.
        let potential = mass * tol.gravity_m_s2 * (center.z + offset);
        let energy = kinetic + potential;
        energy_drift = energy_drift.max((energy / initial_energy - 1.).abs());
        finite &= angle.is_finite()
            && center.is_finite()
            && rotation.is_finite()
            && velocity.is_finite()
            && angular.is_finite()
            && energy.is_finite();
    }
    let periods: Vec<_> = crossings.windows(2).map(|pair| pair[1] - pair[0]).collect();
    check(
        &mut checks,
        "missing full periods",
        tol.minimum_measured_periods.saturating_sub(periods.len()) as f64,
        0.,
        "count",
    );
    let max_period_error = periods
        .iter()
        .map(|value| (value / expected_period - 1.).abs())
        .fold(0., f64::max);
    check(
        &mut checks,
        "maximum full-period relative error",
        max_period_error,
        tol.max_period_relative_error,
        "relative",
    );
    check(
        &mut checks,
        "maximum energy drift",
        energy_drift,
        tol.max_energy_relative_drift,
        "relative",
    );
    check(
        &mut checks,
        "maximum pivot drift",
        pivot_drift,
        tol.max_pivot_drift_mm,
        "mm",
    );
    check(
        &mut checks,
        "maximum axis tilt",
        axis_tilt,
        tol.max_axis_tilt_rad,
        "rad",
    );
    check(
        &mut checks,
        "transient joint limits",
        limit_error,
        tol.max_joint_limit_error_rad,
        "rad",
    );
    check(
        &mut checks,
        "nonfinite state samples",
        if finite { 0. } else { 1. },
        0.,
        "count",
    );
    let exact = core.query(&ws, &rev, "inspect", json!({"pose_version":state.pose_version,"entities":[{"revision":rev,"instance_id":"pendulum","entity_id":"part:0"}]})).await?;
    if exact["pose_version"] != state.pose_version {
        return Err(Error::domain(
            "PHYSICS_BENCHMARK_FAILED",
            "Exact query pose tag mismatch",
        ));
    }
    let pose = &state.evaluation.poses["pendulum"];
    let angle = state.evaluation.joints[0].coordinate;
    let intrinsic = dimensions.map(|value| value * 1000.);
    let (s, c) = angle.sin_cos();
    let world_size = [
        intrinsic[0] * c.abs() + intrinsic[2] * s.abs(),
        intrinsic[1],
        intrinsic[0] * s.abs() + intrinsic[2] * c.abs(),
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
                - world_size[index])
                .abs(),
        );
        let world_min = exact["entities"][0]["world_bounds"]["min"][index]
            .as_f64()
            .unwrap();
        let world_max = exact["entities"][0]["world_bounds"]["max"][index]
            .as_f64()
            .unwrap();
        exact_error = exact_error.max(((world_min + world_max) / 2. - pose.position[index]).abs());
    }
    check(
        &mut checks,
        "intrinsic and posed exact bounds",
        exact_error,
        tol.max_exact_bounds_error_mm,
        "mm",
    );
    let simulation = serde_json::to_value(state.simulation.as_ref().unwrap())?;
    let endpoint = json!({"angle_rad":angle,"position_mm":pose.position,"pose_version":state.pose_version,"exact_bounds_max_error_mm":exact_error});
    state = control(
        &core,
        &ws,
        &rev,
        state.pose_version,
        dynamics::Command::Stop,
    )
    .await?;
    let restored = serde_json::to_value(&state.evaluation.poses)? == before["motion"]["poses"];
    let after = core.get_project(&ws).await?;
    let unchanged = ["revision", "model", "parameters", "activity"]
        .iter()
        .all(|key| before[key] == after[key])
        && core.read_source(&ws, &rev, "model.py").await? == saved_source;
    let mut hash = Sha256::new();
    for data in [&source, &mechanism_source, ACCEPTANCE] {
        hash.update(data.as_bytes());
    }
    Ok(json!({
        "passed":checks.iter().all(|check|check.passed)&&restored&&unchanged,
        "scale":scale,"amplitude_rad":amplitude,"dimensions_m":dimensions,"mass_kg":mass,
        "center_inertia_kg_m2":center_inertia,"pivot_inertia_kg_m2":pivot_inertia,
        "pivot_to_center_m":offset,"expected_period_s":expected_period,"measured_periods_s":periods,
        "initial_energy_j":initial_energy,"ticks":ticks,"time_s":f64::from(ticks)*dynamics::TIMESTEP_S,
        "endpoint":endpoint,"checks":checks,"acceptance":tol,"fixture_sha256":format!("{:x}",hash.finalize()),
        "solver_settings":simulation["solver_settings"],"dependencies":before["model"]["dependencies"],
        "design_revision_and_source_unchanged":unchanged,"stop_restores_design_pose":restored,
        "optimized":!cfg!(debug_assertions),"ui_launched":false
    }))
}
