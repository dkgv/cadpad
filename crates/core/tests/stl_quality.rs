use base64::Engine;
use cadpad_core::{Core, JobState, artifacts::Chunk, tools::ToolContext, worker::WorkerConfig};
use glam::DVec3;
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

fn config() -> WorkerConfig {
    if let Some(resources) = std::env::var_os("CADPAD_TEST_PACKAGED_RESOURCES") {
        assert_eq!(std::env::var("PATH").as_deref(), Ok(""));
        return cadpad_core::runtime::PackagedRuntime::open(std::path::Path::new(&resources))
            .unwrap()
            .worker_config();
    }
    WorkerConfig::development(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap(),
    )
}
#[tokio::test]
async fn contained_curved_stl_preserves_pose_closed_surface_normals_and_declared_error() {
    let temporary = tempfile::tempdir().unwrap();
    let core = Core::open(temporary.path().join("app"), config())
        .await
        .unwrap();
    let ws = core.create_workspace("stl-test").await.unwrap();
    let base = core.get_project(&ws).await.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let draft = core
        .update_source(
            &ws,
            &base,
            None,
            BTreeMap::from([(
                "model.py".into(),
                include_str!("../../../fixtures/export_curved.py").into(),
            )]),
            "stl-test",
        )
        .await
        .unwrap();
    let q = 0.5_f64.sqrt();
    let definition = json!({"joints":[
        {"id":"sphere_mount","parent":null,"child":"sphere","parent_frame":{"position_m":[0.037,-0.019,0.011],"quaternion":[q,0,0,q]},"kind":{"type":"fixed"},"limits":null},
        {"id":"torus_mount","parent":null,"child":"torus","parent_frame":{"position_m":[0.7,0.01,-0.03],"quaternion":[q,0,0,q]},"kind":{"type":"fixed"},"limits":null}
    ]});
    let draft = core
        .stage_mechanism(
            &ws,
            &base,
            Some(&draft.id),
            cadpad_core::kinematics::decode(&definition).unwrap(),
            "stl-test",
        )
        .await
        .unwrap();
    let job = core.build(&ws, &base, &draft.id, "stl-test").await.unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let job = core.get_job(&ws, &job.id).await.unwrap();
            if !matches!(job.state, JobState::Queued | JobState::Running) {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        completed.state,
        JobState::Succeeded,
        "{:?}",
        completed.diagnostics
    );
    let revision = completed.resulting_revision.unwrap();
    let before = core.get_project(&ws).await.unwrap();
    let actor = ToolContext {
        actor: "stl-test".into(),
    };
    let exported = core
        .dispatch(
            &actor,
            "export",
            json!({"workspace_id":ws,"revision":revision,"pose_version":0,"format":"STL"}),
        )
        .await
        .unwrap();
    assert_eq!(exported["units"], "mm");
    let handle = exported["handle"].as_str().unwrap();
    let resource = core
        .export_resource(&actor.actor, &ws, &revision, handle, None)
        .await
        .unwrap();
    let base64 = base64::engine::general_purpose::STANDARD;
    let bytes = if let Some(blob) = resource["blob"].as_str() {
        base64.decode(blob).unwrap()
    } else {
        let manifest = &resource["manifest"];
        let size = manifest["bytes"].as_u64().unwrap();
        let mut bytes = Vec::new();
        while (bytes.len() as u64) < size {
            let offset = bytes.len() as u64;
            let part = core
                .export_resource(
                    &actor.actor,
                    &ws,
                    &revision,
                    handle,
                    Some(Chunk {
                        offset,
                        length: (size - offset).min(1024 * 1024),
                        content_id: manifest["content_id"].as_str().unwrap().into(),
                    }),
                )
                .await
                .unwrap();
            bytes.extend(base64.decode(part["blob"].as_str().unwrap()).unwrap());
        }
        bytes
    };
    assert_eq!(
        cadpad_core::portable::bytes_hash(&bytes),
        exported["sha256"]
    );
    let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
    assert_eq!(bytes.len(), 84 + 50 * count);
    let mut edges = BTreeMap::<([i64; 3], [i64; 3]), usize>::new();
    let mut volumes = [0_f64; 2];
    let mut errors = [0_f64; 2];
    for facet in bytes[84..].as_chunks::<50>().0 {
        let vector = |offset| {
            DVec3::new(
                f32::from_le_bytes(facet[offset..offset + 4].try_into().unwrap()) as f64,
                f32::from_le_bytes(facet[offset + 4..offset + 8].try_into().unwrap()) as f64,
                f32::from_le_bytes(facet[offset + 8..offset + 12].try_into().unwrap()) as f64,
            )
        };
        let normal = vector(0);
        let vertices = [vector(12), vector(24), vector(36)];
        assert!(normal.is_finite() && vertices.iter().all(|v| v.is_finite()));
        let cross = (vertices[1] - vertices[0]).cross(vertices[2] - vertices[0]);
        assert!(cross.length() > 0. && normal.dot(cross.normalize()) > 0.9999);
        assert!((normal.length() - 1.).abs() < 1e-6);
        let group = usize::from(vertices[0].x > 400.);
        let origin = if group == 0 {
            DVec3::new(37., -19., 11.)
        } else {
            DVec3::new(700., 10., -30.)
        };
        let points = vertices.map(|v| {
            let p = v - origin;
            DVec3::new(p.x, p.z, -p.y)
        });
        let center = (points[0] + points[1] + points[2]) / 3.;
        let error = if group == 0 {
            (center.length() - 200.).abs()
        } else {
            ((center.x.hypot(center.y) - 20.).hypot(center.z) - 3.).abs()
        };
        errors[group] = errors[group].max(error);
        volumes[group] += points[0].dot(points[1].cross(points[2])) / 6.;
        let keys = vertices.map(|v| v.to_array().map(|x| (x * 1e5).round() as i64));
        for i in 0..3 {
            *edges.entry((keys[i], keys[(i + 1) % 3])).or_default() += 1;
        }
    }
    assert!(
        edges
            .iter()
            .all(|((a, b), count)| a != b && *count == 1 && edges.get(&(*b, *a)) == Some(&1))
    );
    for (measured, expected) in volumes.into_iter().zip([
        4. / 3. * std::f64::consts::PI * 200_f64.powi(3),
        2. * std::f64::consts::PI.powi(2) * 20. * 9.,
    ]) {
        assert!(
            (measured / expected - 1.).abs() < 0.005,
            "{measured} vs {expected}"
        );
    }
    assert!(
        errors.iter().all(|e| *e <= 0.06),
        "centroid surface errors {errors:?} mm"
    );
    let after = core.get_project(&ws).await.unwrap();
    for field in ["revision", "motion", "model", "parameters"] {
        assert_eq!(before[field], after[field], "Export changed {field}");
    }
    eprintln!(
        "STL: {count} facets; sphere/torus centroid errors {errors:?} mm; closed outward mesh and posed volumes verified"
    );
}

#[tokio::test]
async fn contained_stl_rejects_float32_surface_loss_preserves_design_and_keeps_step_available() {
    for (size, offset, failure) in [
        (0.01, 1_000_000., Some("collapses or reverses")),
        (10., 100_000_000., Some("rounding exceeds")),
        (10., 1_000_000_000., Some("rounding exceeds")),
        (16., 100_000_000., None),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("app");
        let core = Core::open(root.clone(), config()).await.unwrap();
        let ws = core.create_workspace("stl-precision").await.unwrap();
        let base = core.get_project(&ws).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let source = format!(
            "import cadquery as cq\nshow(cq.Workplane().box({size},{size},{size}),name='part',position=({offset},0,0))\n"
        );
        let draft = core
            .update_source(
                &ws,
                &base,
                None,
                BTreeMap::from([("model.py".into(), source)]),
                "stl-precision",
            )
            .await
            .unwrap();
        let job = core
            .build(&ws, &base, &draft.id, "stl-precision")
            .await
            .unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let result = core.get_job(&ws, &job.id).await.unwrap();
                if !matches!(result.state, JobState::Queued | JobState::Running) {
                    break result;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            completed.state,
            JobState::Succeeded,
            "{:?}",
            completed.diagnostics
        );
        let revision = completed.resulting_revision.unwrap();
        let before = core.get_project(&ws).await.unwrap();
        let actor = ToolContext {
            actor: "stl-precision".into(),
        };
        let export = core
            .dispatch(
                &actor,
                "export",
                json!({"workspace_id":ws,"revision":revision,"pose_version":0,"format":"STL"}),
            )
            .await;
        if let Some(message) = failure {
            let error = export.unwrap_err();
            assert_eq!(error.code(), "INVALID_GEOMETRY");
            assert!(error.to_string().contains(message), "{error}");
            assert!(error.to_string().contains("STEP"));
            let directory = root.join("artifacts").join(&ws).join(&revision);
            assert!(!std::fs::read_dir(directory).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "stl")
            }));
        } else {
            let export = export.unwrap();
            let bytes = core
                .artifact(&ws, &revision, export["handle"].as_str().unwrap())
                .await
                .unwrap();
            assert_eq!(u32::from_le_bytes(bytes[80..84].try_into().unwrap()), 12);
            assert_eq!(bytes.len(), 684);
            let mut minimum = f64::INFINITY;
            let mut maximum = f64::NEG_INFINITY;
            for facet in bytes[84..].as_chunks::<50>().0 {
                for start in [12, 24, 36] {
                    let x = f32::from_le_bytes(facet[start..start + 4].try_into().unwrap()) as f64;
                    minimum = minimum.min(x);
                    maximum = maximum.max(x);
                }
            }
            assert_eq!(maximum - minimum, size);
            assert_eq!((maximum + minimum) / 2., offset);
        }
        let step = core
            .dispatch(
                &actor,
                "export",
                json!({"workspace_id":ws,"revision":revision,"pose_version":0,"format":"STEP"}),
            )
            .await
            .unwrap();
        assert!(
            core.artifact(&ws, &revision, step["handle"].as_str().unwrap())
                .await
                .unwrap()
                .starts_with(b"ISO-10303-21;")
        );
        let after = core.get_project(&ws).await.unwrap();
        for field in ["revision", "model", "motion", "parameters", "dirty"] {
            assert_eq!(before[field], after[field], "Export changed {field}");
        }
    }
}
