#![cfg(target_os = "macos")]
#[path = "../examples/support/friction.rs"]
mod friction;
#[tokio::test]
async fn applied_loads_obey_predeclared_friction_contact_stopping_and_long_run_tolerances() {
    let repository = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    for scale in [1., 2.] {
        let report = friction::run(
            cadpad_core::worker::WorkerConfig::development(&repository),
            scale,
        )
        .await
        .unwrap();
        assert!(
            report.passed,
            "scale {scale}: {}",
            serde_json::to_string_pretty(&report).unwrap()
        );
    }
}
