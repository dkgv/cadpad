#![cfg(target_os = "macos")]
#[path = "../examples/support/pendulum.rs"]
mod pendulum;

#[tokio::test]
async fn offset_pendulum_matches_gravity_parallel_axis_period_energy_and_exact_pose() {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    for scale in [1., 2.] {
        for amplitude in [0.05, 0.4] {
            let report = pendulum::run(
                cadpad_core::worker::WorkerConfig::development(&repo),
                scale,
                amplitude,
            )
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
}
