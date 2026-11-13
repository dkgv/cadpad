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
/// `home` is the interpreter bindir from pyvenv.cfg. `executable` is the real binary a trampoline re-execs.
#[cfg(target_os = "macos")]
fn parse_pyvenv(text: &str) -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
    let mut home = None;
    let mut executable = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || !Path::new(value).is_absolute() {
            continue;
        }
        match key.trim() {
            "home" => home = Some(std::path::PathBuf::from(value)),
            "executable" => executable = Some(std::path::PathBuf::from(value)),
            _ => {}
        }
    }
    (home, executable)
}
/// Seatbelt matches the path passed to execve. uv's venv `python` is a symlink
/// or a small trampoline, while the profile previously named only the resolved binary.
#[cfg(target_os = "macos")]
fn interpreter_grants(invoked: &Path) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
    let mut exec = vec![invoked.to_path_buf()];
    let mut runtime = Vec::new();
    if let Some(text) = invoked
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("pyvenv.cfg"))
        .filter(|path| path.is_file())
        .and_then(|path| std::fs::read_to_string(path).ok())
    {
        let (home, executable) = parse_pyvenv(&text);
        if let Some(home) = home {
            if let Some(prefix) = home.parent() {
                runtime.push(prefix.to_path_buf());
                // python.org builds are a stub in bin/ that posix_spawns this app.
                // Seatbelt reports that denial as "Undefined error: 0".
                push_framework_app(&mut exec, prefix);
            }
            for name in ["python3.12", "python3", "python"] {
                let candidate = home.join(name);
                if candidate.is_file() {
                    exec.push(candidate);
                }
            }
        }
        if let Some(executable) = executable.filter(|path| path.is_file()) {
            if let Some(prefix) = executable.parent().and_then(Path::parent) {
                runtime.push(prefix.to_path_buf());
                push_framework_app(&mut exec, prefix);
            }
            exec.push(executable);
        }
    }
    let resolved = exec
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect::<Vec<_>>();
    exec.extend(resolved);
    runtime.retain(|prefix| prefix.is_absolute() && prefix.components().count() >= 4);
    (dedup_paths(exec), dedup_paths(runtime))
}
#[cfg(target_os = "macos")]
fn push_framework_app(exec: &mut Vec<std::path::PathBuf>, prefix: &Path) {
    for relative in [
        "Resources/Python.app/Contents/MacOS/Python",
        "Python.app/Contents/MacOS/Python",
    ] {
        let candidate = prefix.join(relative);
        if candidate.is_file() {
            exec.push(candidate);
        }
    }
}
#[cfg(target_os = "macos")]
fn dedup_paths(paths: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
    let mut unique = Vec::new();
    for path in paths {
        if !unique.iter().any(|existing| existing == &path) {
            unique.push(path);
        }
    }
    unique
}
#[cfg(target_os = "macos")]
fn macos(config: &WorkerConfig, cache: &Path, source: Option<&Path>) -> Result<Command> {
    let invoked = if config.executable.is_absolute() {
        config.executable.clone()
    } else {
        std::env::current_dir()?.join(&config.executable)
    };
    let executable = invoked.canonicalize()?;
    let (exec_targets, venv_runtime) = interpreter_grants(&invoked);
    let cache = cache.canonicalize()?;
    let mut runtime = venv_runtime;
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
    let mut extra_exec = 0;
    for target in &exec_targets {
        if target == &executable {
            continue;
        }
        extra_exec += 1;
        let name = format!("EXECUTABLE_{extra_exec}");
        parameter(&mut command, &name, target)?;
        profile.push_str(&format!(
            "\n(allow process-exec (literal (param \"{name}\")))\n"
        ));
    }
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn pyvenv_keeps_absolute_interpreter_paths() {
        let (home, executable) = parse_pyvenv(
            "home = /opt/hosted/cpython/bin\nexecutable = /opt/hosted/cpython/bin/python3.12\nrelative = bin/python\n",
        );
        assert_eq!(
            home.unwrap(),
            std::path::PathBuf::from("/opt/hosted/cpython/bin")
        );
        assert_eq!(
            executable.unwrap(),
            std::path::PathBuf::from("/opt/hosted/cpython/bin/python3.12")
        );
    }

    #[test]
    fn framework_stub_may_spawn_python_app() {
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("Python.framework/Versions/3.12");
        let bin = prefix.join("bin");
        let app = prefix.join("Resources/Python.app/Contents/MacOS");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(bin.join("python3.12"), b"").unwrap();
        std::fs::write(app.join("Python"), b"").unwrap();
        let venv = temp.path().join("venv");
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        std::fs::write(venv.join("bin/python"), b"").unwrap();
        std::fs::write(
            venv.join("pyvenv.cfg"),
            format!("home = {}\n", bin.display()),
        )
        .unwrap();
        let (exec, runtime) = interpreter_grants(&venv.join("bin/python"));
        assert!(exec.iter().any(|path| path == &app.join("Python")));
        assert!(runtime.iter().any(|path| path == &prefix));
    }

    #[test]
    fn sandbox_allows_the_invoked_venv_symlink() {
        let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let config = WorkerConfig::development(&repo);
        let cache = tempfile::tempdir().unwrap();
        let command = command(&config, cache.path(), None).unwrap();
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let invoked = config.executable.to_string_lossy().into_owned();
        assert!(
            args.iter()
                .any(|arg| arg.strip_prefix("EXECUTABLE").is_some_and(|rest| {
                    rest.trim_start_matches('_')
                        .trim_start_matches(|ch: char| ch.is_ascii_digit())
                        == format!("={invoked}")
                })),
            "sandbox profile does not exec {invoked}: {args:?}"
        );
    }
}
