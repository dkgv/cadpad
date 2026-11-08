#[path = "support/pendulum.rs"]
mod pendulum;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let mut args = std::env::args_os().skip(1);
    let mut resources = None;
    let mut scale = 1.;
    let mut amplitude = 0.05;
    while let Some(arg) = args.next() {
        if arg == "--resources" {
            resources = Some(std::path::PathBuf::from(
                args.next().ok_or("Missing resources directory")?,
            ));
        } else if arg == "--scale" {
            scale = args
                .next()
                .ok_or("Missing scale")?
                .to_str()
                .ok_or("Invalid scale")?
                .parse()?;
        } else if arg == "--amplitude" {
            amplitude = args
                .next()
                .ok_or("Missing amplitude")?
                .to_str()
                .ok_or("Invalid amplitude")?
                .parse()?;
        } else {
            return Err("Unknown pendulum benchmark argument".into());
        }
    }
    let config = if let Some(resources) = resources {
        cadpad_core::runtime::PackagedRuntime::open(&resources)?.worker_config()
    } else {
        cadpad_core::worker::WorkerConfig::development(&repo)
    };
    let report = pendulum::run(config, scale, amplitude).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report["passed"] != true {
        std::process::exit(1);
    }
    Ok(())
}
