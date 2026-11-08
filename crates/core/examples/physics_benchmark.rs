#[path = "support/physics.rs"]
mod physics;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let config = if std::env::args().any(|arg| arg == "--bundled") {
        cadpad_core::worker::WorkerConfig::bundled(
            repo.join("target/worker-bundle/cadpad-worker/cadpad-worker"),
        )
    } else {
        cadpad_core::worker::WorkerConfig::development(&repo)
    };
    let report = physics::run(config, std::env::args().any(|arg| arg == "--padded-proxy")).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.passed {
        std::process::exit(1);
    }
    Ok(())
}
