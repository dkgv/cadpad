#![cfg(target_os = "macos")]
#[path = "../examples/support/stack.rs"]
mod stack;
#[tokio::test]
async fn stacked_blocks_obey_predeclared_settle_hold_slide_and_long_run_tolerances() {
    let repository = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    for scale in [1., 2.] {
        let report = stack::run(
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
