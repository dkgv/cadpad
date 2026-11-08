//! Resource layout shared by the packaged desktop and headless packaging probe.
//! Release startup never falls back to a repository interpreter or debug helper.
use crate::{
    Error, Result,
    worker::{Containment, WorkerConfig},
};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct PackagedRuntime {
    pub worker: PathBuf,
    pub guardian: PathBuf,
    pub bridge: PathBuf,
}
impl PackagedRuntime {
    pub fn open(resources: &Path) -> Result<Self> {
        let root = resources.canonicalize().map_err(|error| {
            Error::domain(
                "APP_UNAVAILABLE",
                format!("Packaged runtime resources are unavailable: {error}"),
            )
        })?;
        let executable = |relative: &str| -> Result<PathBuf> {
            let path = root.join(relative).canonicalize().map_err(|error| {
                Error::domain(
                    "APP_UNAVAILABLE",
                    format!("Missing packaged runtime {relative}: {error}"),
                )
            })?;
            if !path.starts_with(&root) || !path.is_file() {
                return Err(Error::domain(
                    "APP_UNAVAILABLE",
                    format!("Invalid packaged executable {relative}"),
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if path.metadata()?.permissions().mode() & 0o111 == 0 {
                    return Err(Error::domain(
                        "APP_UNAVAILABLE",
                        format!("Packaged runtime {relative} is not executable"),
                    ));
                }
            }
            Ok(path)
        };
        let runtime = Self {
            worker: executable("worker/cadpad-worker")?,
            guardian: executable("cadpad-worker-guard")?,
            bridge: executable("mcp/cadpad-mcp")?,
        };
        let internal = root
            .join("worker/_internal")
            .canonicalize()
            .map_err(|error| {
                Error::domain(
                    "APP_UNAVAILABLE",
                    format!("Missing bundled Python/native libraries: {error}"),
                )
            })?;
        if !internal.starts_with(root.join("worker")) || !internal.is_dir() {
            return Err(Error::domain(
                "APP_UNAVAILABLE",
                "Bundled Python/native libraries must stay within the worker resource directory",
            ));
        }
        Ok(runtime)
    }
    pub fn worker_config(&self) -> WorkerConfig {
        WorkerConfig {
            executable: self.worker.clone(),
            args: vec![],
            module_path: None,
            containment: Containment::Required,
            guardian_executable: self.guardian.clone(),
            limits: Default::default(),
        }
    }
}
