//! Platform enforcement belongs to the native launcher, before Python executes.
use crate::{
    Error, Result,
    worker::{Containment, WorkerConfig},
};
use std::path::Path;
use tokio::process::Command;

pub(crate) fn command(
    config: &WorkerConfig,
    cache: &Path,
    source: Option<&Path>,
) -> Result<Command> {
    if config.containment == Containment::UncontainedForTrustedTests {
        return Ok(Command::new(&config.executable));
    }
    #[cfg(target_os = "macos")]
    {
        macos(config, cache, source)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (cache, source);
        Err(Error::domain(
            "CONTAINMENT_UNAVAILABLE",
            "Contained CAD execution is currently supported on macOS only",
        ))
    }
}
#[cfg(target_os = "macos")]
fn parameter(command: &mut Command, key: &str, path: &Path) -> Result<()> {
    let value = path.to_str().ok_or_else(|| {
        Error::domain(
            "CONTAINMENT_UNAVAILABLE",
            "Sandbox paths must be valid UTF-8",
        )
    })?;
    // Values are separate -D arguments; they are never evaluated as profile text.
    command.arg("-D").arg(format!("{key}={value}"));
    Ok(())
}
#[cfg(target_os = "macos")]
fn macos(config: &WorkerConfig, cache: &Path, source: Option<&Path>) -> Result<Command> {
    let executable = config.executable.canonicalize()?;
    let cache = cache.canonicalize()?;
    let mut runtime = vec![];
    if let Some(module) = &config.module_path {
        runtime.push(module.canonicalize()?);
        runtime.push(
            config
                .executable
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| {
                    Error::domain(
                        "CONTAINMENT_UNAVAILABLE",
                        "Missing virtual environment root",
                    )
                })?
                .canonicalize()?,
        );
        runtime.push(
            executable
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| {
                    Error::domain("CONTAINMENT_UNAVAILABLE", "Missing Python runtime root")
                })?
                .to_path_buf(),
        );
    } else {
        runtime.push(
            executable
                .parent()
                .ok_or_else(|| {
                    Error::domain("CONTAINMENT_UNAVAILABLE", "Missing bundled runtime root")
                })?
                .to_path_buf(),
        );
    }
    let source = source.map(Path::canonicalize).transpose()?;
    if runtime
        .iter()
        .chain(source.iter())
        .any(|root| root.parent().is_none() || cache.starts_with(root) || root.starts_with(&cache))
    {
        return Err(Error::domain(
            "CONTAINMENT_UNAVAILABLE",
            "Runtime, immutable inputs and writable cache must have separate roots",
        ));
    }
    runtime.sort();
    runtime.dedup();
    let mut profile = String::from(include_str!("containment.sbpl"));
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.env_clear();
    parameter(&mut command, "EXECUTABLE", &executable)?;
    parameter(&mut command, "CACHE", &cache)?;
    for (index, root) in runtime.iter().enumerate() {
        let name = format!("RUNTIME_{index}");
        parameter(&mut command, &name, root)?;
        profile.push_str(&format!(
            "\n(allow file-read* file-map-executable (subpath (param \"{name}\")))\n"
        ));
    }
    if let Some(source) = source {
        parameter(&mut command, "SOURCE", &source)?;
        profile.push_str("\n(allow file-read* (subpath (param \"SOURCE\")))\n");
    }
    let home = cache.join(".sandbox-home");
    let temporary = cache.join(".sandbox-tmp");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&temporary)?;
    command
        .arg("-p")
        .arg(profile)
        .arg(&config.executable)
        .current_dir(&cache)
        .env("HOME", &home)
        .env("TMPDIR", &temporary)
        .env("XDG_CACHE_HOME", home.join(".cache"));
    Ok(command)
}
