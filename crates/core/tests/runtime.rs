#![cfg(unix)]
use cadpad_core::runtime::PackagedRuntime;
use std::{
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};
fn layout(root: &Path) {
    std::fs::create_dir_all(root.join("worker/_internal")).unwrap();
    std::fs::create_dir_all(root.join("mcp")).unwrap();
    for entry in [
        "worker/cadpad-worker",
        "cadpad-worker-guard",
        "mcp/cadpad-mcp",
    ] {
        std::fs::write(root.join(entry), b"fixture executable").unwrap();
        std::fs::set_permissions(root.join(entry), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}
#[test]
fn packaged_paths_are_explicit_and_never_resolve_development_fallbacks() {
    let root = tempfile::tempdir().unwrap();
    layout(root.path());
    let runtime = PackagedRuntime::open(root.path()).unwrap();
    let config = runtime.worker_config();
    assert_eq!(
        config.executable,
        root.path()
            .canonicalize()
            .unwrap()
            .join("worker/cadpad-worker")
    );
    assert_eq!(config.guardian_executable, runtime.guardian);
    assert!(config.module_path.is_none());
    assert!(config.args.is_empty());
    std::fs::remove_file(root.path().join("cadpad-worker-guard")).unwrap();
    assert_eq!(
        PackagedRuntime::open(root.path()).unwrap_err().code(),
        "APP_UNAVAILABLE"
    );
}
#[test]
fn incomplete_nonexecutable_or_external_runtime_entries_fail_before_execution() {
    let root = tempfile::tempdir().unwrap();
    layout(root.path());
    std::fs::set_permissions(
        root.path().join("mcp/cadpad-mcp"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(PackagedRuntime::open(root.path()).is_err());
    std::fs::remove_file(root.path().join("mcp/cadpad-mcp")).unwrap();
    symlink("/bin/ls", root.path().join("mcp/cadpad-mcp")).unwrap();
    assert!(PackagedRuntime::open(root.path()).is_err());
    std::fs::remove_file(root.path().join("mcp/cadpad-mcp")).unwrap();
    layout(root.path());
    std::fs::remove_dir(root.path().join("worker/_internal")).unwrap();
    assert!(PackagedRuntime::open(root.path()).is_err());
    let external = tempfile::tempdir().unwrap();
    symlink(external.path(), root.path().join("worker/_internal")).unwrap();
    assert!(PackagedRuntime::open(root.path()).is_err());
}
