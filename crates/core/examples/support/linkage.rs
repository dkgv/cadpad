//! Native BRep-backed, ideal-pin four-bar reference. No rendering or bearing model.
use cadpad_core::{
    Core, Error, JobState, Result, dynamics, kinematics, motion, worker::WorkerConfig,
};
use glam::{DQuat, DVec3};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};
const SOURCE: &str = include_str!("../../../../fixtures/physics_linkage.py");
const MECHANISM: &str = include_str!("../../../../fixtures/physics_linkage.mechanism.json");
const ACCEPTANCE: &str = include_str!("../../../../fixtures/physics_linkage.acceptance.json");
fn fail(message: &str) -> Error {
    Error::domain("PHYSICS_BENCHMARK_FAILED", message)
}
fn check(checks: &mut Vec<Value>, name: String, measured: f64, maximum: f64, unit: &str) {
    checks.push(json!({"name":name,"measured":measured,"maximum":maximum,"unit":unit,"passed":measured.is_finite()&&measured<=maximum}));
}
async fn control(
    core: &Core,
    ws: &str,
    rev: &str,
    version: u64,
    command: dynamics::Command,
) -> Result<motion::Snapshot> {
    core.control_simulation(
        "linkage-reference",
        dynamics::Control {
            workspace_id: ws.into(),
            revision: rev.into(),
            expected_pose_version: version,
            command,
        },
    )
    .await
}
fn scale_lengths(value: &mut Value, scale: f64) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "position_m" {
                    for n in value.as_array_mut().unwrap() {
                        *n = json!(n.as_f64().unwrap() * scale);
                    }
                } else if ["clearance_m", "backoff_m", "length_tolerance_m"].contains(&key.as_str())
                {
                    *value = json!(value.as_f64().unwrap() * scale);
                } else {
                    scale_lengths(value, scale);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                scale_lengths(value, scale);
            }
        }
        _ => (),
    }
}
// Upper circle intersection, independent of the kinematic and physical solvers.
fn geometry(theta: f64, scale: f64) -> [DVec3; 4] {
    let b = DVec3::new(theta.cos(), theta.sin(), 0.) * (0.04 * scale);
    let d = DVec3::new(0.1 * scale, 0., 0.);
    let delta = d - b;
    let r = delta.length();
    let e = delta / r;
    let t = ((0.09 * scale).powi(2) - (0.07 * scale).powi(2) + r * r) / (2. * r);
    let h = ((0.09 * scale).powi(2) - t * t).sqrt();
    let c = b + e * t + DVec3::new(-e.y, e.x, 0.) * h;
    [DVec3::ZERO, b, c, d]
}
fn potential(theta: f64, scale: f64, masses: &BTreeMap<String, f64>, gravity: f64) -> f64 {
    let [a, b, c, d] = geometry(theta, scale);
    gravity
        * (masses["crank"] * (a.y + b.y)
            + masses["coupler"] * (b.y + c.y)
            + masses["rocker"] * (c.y + d.y))
        / 2.
}
fn anchor(pose: &kinematics::Pose, local: DVec3) -> DVec3 {
    DVec3::from_array(pose.position) * 0.001 + DQuat::from_array(pose.quaternion) * local
}
pub async fn run(config: WorkerConfig, scale: f64) -> Result<Value> {
    let tol: Value = serde_json::from_str(ACCEPTANCE)?;
    if !tol["scales"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_f64() == Some(scale))
    {
        return Err(fail("Scale is not a predeclared reference case"));
    }
    let n = |key: &str| tol[key].as_f64().unwrap();
    let source = SOURCE.replace("SCALE = 1.0", &format!("SCALE = {scale:?}"));
    let mut definition: Value = serde_json::from_str(MECHANISM)?;
    scale_lengths(&mut definition, scale);
    let temp = tempfile::tempdir()?;
    let core = Core::open(temp.path().join("app"), config).await?;
    let ws = core.create_workspace("linkage-reference").await?;
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
            "linkage-reference",
        )
        .await?;
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            kinematics::decode(&definition)?,
            "linkage-reference",
        )
        .await?;
    let job = core
        .build(&ws, &base, &draft.id, "linkage-reference")
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
    .map_err(|_| fail("Linkage build timed out"))??;
    if job.state != JobState::Succeeded {
        return Err(Error::Domain {
            code: "PHYSICS_BENCHMARK_FAILED".into(),
            message: "Linkage fixture build failed".into(),
            details: job.diagnostics,
        });
    }
    let rev = job.resulting_revision.unwrap();
    let before = core.get_project(&ws).await?;
    let mut state = core.get_motion(&ws, &rev).await?;
    let mut checks = Vec::new();
    let mut phases = Vec::new();
    let mut restored = true;
    let mut invalid_motor_atomic = true;
    let mut solver = None;
    let torque_scale = scale.powi(4);
    let stiffness = n("base_stiffness_nm_rad") * torque_scale;
    let damping = n("base_damping_nm_s_rad") * scale.powf(4.5);
    let cap = n("base_motor_cap_nm") * torque_scale;
    for (name, gravity) in [("unloaded", 0.), ("gravity", n("gravity_m_s2"))] {
        let dynamic = json!({"type":"dynamic","mass":{"type":"density","density_kg_m3":n("density_kg_m3")},"friction":0,"restitution":0});
        let physics = serde_json::from_value(
            json!({"gravity_m_s2":[0,-gravity,0],"bodies":{"ground":{"type":"fixed","friction":0,"restitution":0},"crank":dynamic,"coupler":dynamic,"rocker":dynamic}}),
        )?;
        state = control(
            &core,
            &ws,
            &rev,
            state.pose_version,
            dynamics::Command::Start {
                definition: physics,
            },
        )
        .await?;
        let motor = |id: &str| dynamics::Motor {
            joint_id: id.into(),
            target: n("target_rad"),
            unit: "rad".into(),
            stiffness,
            damping,
            max_force: cap,
        };
        for id in ["coupler", "rocker"] {
            let old = serde_json::to_value(core.get_motion(&ws, &rev).await?)?;
            let result = control(
                &core,
                &ws,
                &rev,
                state.pose_version,
                dynamics::Command::Motor { target: motor(id) },
            )
            .await;
            invalid_motor_atomic &= matches!(result,Err(ref e) if e.code()=="INVALID_SIMULATION")
                && serde_json::to_value(core.get_motion(&ws, &rev).await?)? == old;
        }
        state = control(
            &core,
            &ws,
            &rev,
            state.pose_version,
            dynamics::Command::Motor {
                target: motor("crank"),
            },
        )
        .await?;
        let ticks = (n("base_settling_ticks") * scale.sqrt()).ceil() as u32;
        let mut pin_error: f64 = 0.;
        let mut axis_error: f64 = 0.;
        let mut limit_error: f64 = 0.;
        let mut max_impulse: f64 = 0.;
        let mut residual_error: f64 = 0.;
        let mut uncertified = true;
        let mut tail = Vec::new();
        for tick in 0..ticks {
            state = control(
                &core,
                &ws,
                &rev,
                state.pose_version,
                dynamics::Command::Advance { steps: 1 },
            )
            .await?;
            let poses = &state.evaluation.poses;
            let crank = anchor(&poses["crank"], DVec3::ZERO);
            let crank_end = anchor(&poses["crank"], DVec3::new(0.04 * scale, 0., 0.));
            let coupler = anchor(&poses["coupler"], DVec3::ZERO);
            let c1 = anchor(&poses["coupler"], DVec3::new(0.09 * scale, 0., 0.));
            let c2 = anchor(&poses["rocker"], DVec3::new(0.07 * scale, 0., 0.));
            let rocker = anchor(&poses["rocker"], DVec3::ZERO);
            for error in [
                crank.length(),
                crank_end.distance(coupler),
                c1.distance(c2),
                rocker.distance(DVec3::new(0.1 * scale, 0., 0.)),
            ] {
                pin_error = pin_error.max(error * 1000.);
            }
            for id in ["crank", "coupler", "rocker"] {
                let z = DQuat::from_array(poses[id].quaternion) * DVec3::Z;
                axis_error = axis_error.max(z.cross(DVec3::Z).length().atan2(z.z));
            }
            for joint in &state.evaluation.joints {
                let limits = definition["joints"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|j| j["id"] == joint.id)
                    .unwrap()["limits"]
                    .as_array()
                    .unwrap();
                limit_error = limit_error.max(
                    (limits[0].as_f64().unwrap() - joint.coordinate)
                        .max(joint.coordinate - limits[1].as_f64().unwrap())
                        .max(0.),
                );
            }
            let report = state
                .evaluation
                .constraints
                .as_ref()
                .ok_or_else(|| fail("Missing physical residual report"))?;
            residual_error =
                residual_error.max((report.position_error_m - c1.distance(c2)).abs() * 1000.);
            uncertified &= !report.path_certified;
            max_impulse = max_impulse.max(
                state.simulation.as_ref().unwrap().motor_impulses["crank"]
                    .impulse
                    .abs(),
            );
            if tick + 12 >= ticks {
                tail.push(json!({"tick":tick+1,"joints":state.evaluation.joints,"angular_velocity_rad_s":state.simulation.as_ref().unwrap().current.angular_velocity_rad_s}));
            }
        }
        let simulation = state.simulation.as_ref().unwrap();
        solver = Some(simulation.solver_settings.clone());
        let theta = state
            .evaluation
            .joints
            .iter()
            .find(|j| j.id == "crank")
            .unwrap()
            .coordinate;
        let speed = simulation
            .current
            .angular_velocity_rad_s
            .iter()
            .filter(|(id, _)| id.as_str() != "ground")
            .map(|(_, v)| DVec3::from_array(*v).length())
            .fold(0., f64::max);
        let [a, b, c, d] = geometry(theta, scale);
        let expected = [
            ("crank", a, b, 40.),
            ("coupler", b, c, 90.),
            ("rocker", d, c, 70.),
        ];
        let mut analytic_error: f64 = 0.;
        let mut exact_error: f64 = 0.;
        let exact=core.query(&ws,&rev,"inspect",json!({"pose_version":state.pose_version,"entities":[{"revision":rev,"instance_id":"crank","entity_id":"part:0"},{"revision":rev,"instance_id":"coupler","entity_id":"part:0"},{"revision":rev,"instance_id":"rocker","entity_id":"part:0"}]})).await?;
        if exact["pose_version"] != state.pose_version {
            return Err(fail("Exact query pose tag mismatch"));
        }
        for (index, (id, start, end, length)) in expected.into_iter().enumerate() {
            let pose = &state.evaluation.poses[id];
            let rotation = DQuat::from_array(pose.quaternion);
            analytic_error = analytic_error
                .max(anchor(pose, DVec3::ZERO).distance(start) * 1000.)
                .max(
                    anchor(pose, DVec3::new(length * scale * 0.001, 0., 0.)).distance(end) * 1000.,
                );
            let local = [length * scale, 6. * scale, 4. * scale];
            let center = anchor(pose, DVec3::new(local[0] * 0.0005, 0., 0.)) * 1000.;
            // Rotated-box AABB from the actual physical quaternion, not a projected planar angle.
            let size = (rotation * DVec3::X).abs() * local[0]
                + (rotation * DVec3::Y).abs() * local[1]
                + (rotation * DVec3::Z).abs() * local[2];
            for axis in 0..3 {
                exact_error = exact_error.max(
                    (exact["entities"][index]["bounds"]["size"][axis]
                        .as_f64()
                        .unwrap()
                        - local[axis])
                        .abs(),
                );
                exact_error = exact_error.max(
                    (exact["entities"][index]["world_bounds"]["size"][axis]
                        .as_f64()
                        .unwrap()
                        - size[axis])
                        .abs(),
                );
                for (key, sign) in [("min", -1.), ("max", 1.)] {
                    exact_error = exact_error.max(
                        (exact["entities"][index]["world_bounds"][key][axis]
                            .as_f64()
                            .unwrap()
                            - (center[axis] + sign * size[axis] * 0.5))
                            .abs(),
                    );
                }
            }
        }
        for (label, value, max, unit) in [
            ("all pin drift", pin_error, n("max_pin_error_mm"), "mm"),
            ("axis tilt", axis_error, n("max_axis_error_rad"), "rad"),
            (
                "transient limits",
                limit_error,
                n("max_joint_limit_error_rad"),
                "rad",
            ),
            (
                "settled speed",
                speed,
                n("max_settled_speed_rad_s"),
                "rad/s",
            ),
            (
                "independent four-bar geometry",
                analytic_error,
                n("max_analytic_pose_error_mm"),
                "mm",
            ),
            (
                "exact posed and intrinsic bounds",
                exact_error,
                n("max_exact_bounds_error_mm"),
                "mm",
            ),
            (
                "actual closure residual report",
                residual_error,
                1e-10,
                "mm",
            ),
            (
                "motor impulse cap",
                (max_impulse - cap * simulation.solver_settings.nominal_solver_substep_s).max(0.),
                n("motor_impulse_slack_nm_s"),
                "N m s",
            ),
        ] {
            check(&mut checks, format!("{name}: {label}"), value, max, unit);
        }
        // Joint-row impulse acts on body 1; child motor torque has the opposite sign.
        let torque = -simulation.motor_impulses["crank"].impulse
            / simulation.solver_settings.nominal_solver_substep_s;
        let h = 1e-5;
        let derivative = (potential(theta + h, scale, &simulation.mass_kg, gravity)
            - potential(theta - h, scale, &simulation.mass_kg, gravity))
            / (2. * h);
        if gravity == 0. {
            check(
                &mut checks,
                format!("{name}: target"),
                (theta - n("target_rad")).abs(),
                n("max_unloaded_target_error_rad"),
                "rad",
            );
        } else {
            check(
                &mut checks,
                format!("{name}: motor versus potential derivative"),
                (torque - derivative).abs() / derivative.abs(),
                n("max_gravity_torque_relative_error"),
                "relative",
            );
            check(
                &mut checks,
                format!("{name}: spring versus potential derivative"),
                (stiffness * (n("target_rad") - theta) - derivative).abs() / derivative.abs(),
                n("max_gravity_torque_relative_error"),
                "relative",
            );
        }
        phases.push(json!({"name":name,"ticks":ticks,"gravity_m_s2":gravity,"crank_rad":theta,"settled_speed_rad_s":speed,"max_pin_error_mm":pin_error,"max_axis_error_rad":axis_error,"max_joint_limit_error_rad":limit_error,"max_motor_impulse_nm_s":max_impulse,"settled_motor_torque_nm":torque,"analytic_gravity_torque_nm":derivative,"analytic_pose_error_mm":analytic_error,"exact_bounds_error_mm":exact_error,"mass_kg":simulation.mass_kg,"path_never_certified":uncertified,"settling_tail":tail}));
        if !uncertified {
            return Err(fail(
                "Simulation incorrectly certifies a geometric swept path",
            ));
        }
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
        json!({"passed":checks.iter().all(|c|c["passed"]==true)&&restored&&unchanged&&invalid_motor_atomic,"scale":scale,"acceptance":tol,"fixture_sha256":format!("{:x}",hash.finalize()),"phases":phases,"checks":checks,"solver_settings":solver,"passive_motor_rejection_atomic":invalid_motor_atomic,"design_unchanged":unchanged,"stop_restores_nonzero_pose":restored,"optimized":!cfg!(debug_assertions),"ui_launched":false,"scope":"Ideal pinned uniform bars; adjacent overlap contacts excluded; not bearing or load-capacity validation"}),
    )
}
