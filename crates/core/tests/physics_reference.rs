#![cfg(target_os = "macos")]
#[path = "../examples/support/physics.rs"]
mod physics;
#[tokio::test]
async fn motorized_slider_meets_predeclared_gravity_contact_and_joint_limit_tolerances() {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let config = cadpad_core::worker::WorkerConfig::development(&repo);
    for padded in [false, true] {
        let report = physics::run(config.clone(), padded).await.unwrap();
        assert!(
            report.passed,
            "{}",
            serde_json::to_string_pretty(&report).unwrap()
        );
    }
}
