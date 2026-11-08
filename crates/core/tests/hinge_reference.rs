#![cfg(target_os = "macos")]
#[path = "../examples/support/hinge.rs"]
mod hinge;

#[tokio::test]
async fn contained_hinge_matches_declared_inertia_torque_limit_and_exact_pose_tolerances() {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    for scale in [1., 2.] {
        let report = hinge::run(cadpad_core::worker::WorkerConfig::development(&repo), scale)
            .await
            .unwrap();
        assert_eq!(
            report["passed"],
            true,
            "{}",
            serde_json::to_string_pretty(&report).unwrap()
        );
    }
}
