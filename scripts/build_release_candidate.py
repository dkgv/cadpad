"""Build and probe a local, ad-hoc-signed macOS candidate without opening UI.

Always invoke with the pinned .venv/bin/python. Bundles are created in a unique
staging directory, leaving any existing installed or running bundle untouched.
This stages runtime resources;
Developer ID signing, notarization and clean-machine/UI acceptance are separate gates.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile

REPOSITORY = Path(__file__).resolve().parents[1]
SOURCE_INPUTS = (
    "Cargo.lock", "uv.lock", "package-lock.json", "packaging/worker.spec",
    "scripts/build_release_candidate.py", "scripts/worker_notices.py",
    "scripts/frontend_notices.mjs", "scripts/rust_notices.py",
    "apps/desktop/index.html", "apps/desktop/src/main.tsx", "apps/desktop/src/startup.tsx",
    "apps/desktop/public/startup.js", "apps/desktop/src-tauri/src/frontend_assets.rs",
    "apps/desktop/tests/startup.test.mjs", "scripts/check_frontend_startup.mjs",
    "apps/desktop/vite.config.mjs", "apps/desktop/src/app.tsx", "apps/desktop/src/style.css",
    "apps/desktop/src/motion-test-definition.tsx", "apps/desktop/src/motion-tests.tsx", "apps/desktop/src-tauri/src/main.rs",
    "apps/desktop/src-tauri/src/agents.rs", "crates/core/src/motion_tests.rs",
    "crates/core/src/motion_test_runs.rs", "crates/core/src/actors.rs",
    "crates/core/tests/motion_tests.rs", "fixtures/lever_shoe.py",
    "python/cadpad_worker/annotations.py", "python/tests/test_intrinsic_reuse.py",
    "python/cadpad_worker/model.py", "python/cadpad_worker/stl.py",
    "python/tests/test_stl_quality.py", "crates/core/tests/stl_quality.rs",
    "fixtures/export_curved.py",
    "apps/desktop/src-tauri/src/menus.rs", "apps/desktop/src-tauri/tauri.conf.json",
    "crates/core/src/runtime.rs", "crates/core/src/lib.rs", "crates/core/src/construction.rs",
    "crates/core/src/recovery.rs", "crates/core/src/worker.rs", "crates/core/src/worker_limits.rs",
    "crates/core/src/worker_cache.rs", "crates/core/src/native_admission.rs",
    "crates/core/src/cache.rs", "crates/core/src/artifacts.rs", "crates/core/tests/cache_buffers.rs",
    "crates/core/src/project_result.rs",
    "crates/core/src/dynamics.rs", "crates/core/src/paths.rs", "crates/core/src/kinematics.rs", "crates/core/src/collision.rs",
    "crates/core/src/constraints.rs", "packages/viewer/src/motion.ts",
    "crates/core/tests/closure_dynamics.rs", "crates/core/tests/linkage_reference.rs",
    "crates/core/examples/support/linkage.rs", "crates/core/examples/linkage_benchmark.rs",
    "fixtures/physics_linkage.py", "fixtures/physics_linkage.mechanism.json",
    "fixtures/physics_linkage.acceptance.json", "scripts/check_linkage.py",
    "crates/core/src/portable.rs", "crates/core/src/portable_recordings.rs",
    "crates/core/src/playback.rs", "crates/core/src/projects.rs", "crates/core/src/motion.rs",
    "crates/core/src/tools.rs", "crates/core/tests/fixtures/tool-schema-v1.json", "crates/core/tests/playback.rs",
    "crates/core/src/events.rs", "crates/agent/src/service.rs", "crates/agent/src/runner.rs",
    "crates/agent/tests/loop.rs", "apps/desktop/tests/workspace.test.mjs",
    "apps/desktop/tests/repair-view.test.mjs",
    "crates/core/examples/support/pendulum.rs", "crates/core/examples/pendulum_benchmark.rs",
    "fixtures/physics_pendulum.py", "fixtures/physics_pendulum.mechanism.json",
    "fixtures/physics_pendulum.acceptance.json",
    "crates/core/tests/worker_cache.rs",
    "crates/core/src/bin/cadpad-worker-guard.rs", "crates/core/src/bin/support/guardian_anchor.rs",
    "crates/mcp/examples/packaged_runtime_check.rs", "crates/core/examples/packaged_limits_check.rs",
    "packages/viewer/src/index.ts", "packages/viewer/src/entities.ts", "packages/viewer/src/buffers.ts",
    "packages/viewer/src/dimension-display.ts", "packages/viewer/tests/dimension-display.test.ts",
    "packages/viewer/src/picking.ts", "packages/viewer/src/render-buffers.ts",
    "fixtures/rail_carriage.py", "crates/core/src/display_reuse.rs",
    "python/cadpad_worker/display_cache.py", "python/cadpad_worker/protocol.py",
    "python/tests/test_display_reuse.py", "crates/core/tests/display_reuse.rs",
    "fixtures/display_reuse.py",
    "crates/core/src/geometry_cache.rs", "crates/core/tests/geometry_cache.rs",
    "python/cadpad_worker/geometry_cache.py", "python/tests/test_geometry_cache.py",
    "fixtures/cached_shape.py", "crates/mcp/src/mcp.rs",
)
MAGIC = {b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca", b"\xca\xfe\xba\xbf"}
IDLE_CACHE_CASES = (
    "idle_registry_eviction_is_lru_and_exact_recovery_preserves_source_motion_meshes_and_exports",
    "an_entered_exact_query_is_pinned_during_pressure_and_becomes_evictable_after_completion",
    "native_admission_bounds_active_registries_and_preserves_warm_repair_and_cancellation",
    "native_admission_lease_survives_owner_sigkill_without_entering_generated_code",
)
CURRENT_BUFFER_CASES = (
    "current_buffer_cleanup_retains_source_recordings_exports_readers_and_exact_geometry",
    "current_buffer_cleanup_waits_for_native_remesh_publication_without_blocking_context_reads",
    "display_budget_evicts_cold_publications_preserves_pinned_readers_and_recovers_explicitly",
)
PORTABLE_RECORDING_CASES = (
    "portable_recordings_move_between_clean_app_roots_and_rebind_only_verified_open_builds",
    "portable_recordings_reject_corrupt_missing_duplicate_oversized_and_escaped_archives_without_execution",
    "portable_recordings_do_not_rebind_to_changed_source_or_mismatched_geometry",
)
BUILTIN_AGENT_CASES = (
    "builtin_loop_repairs_inspects_captures_refines_exports_and_keeps_credentials_private",
    "stop_waits_for_native_termination_and_preserves_the_committed_model",
    "repeated_build_failures_stop_at_the_budget_and_history_recovers_without_secrets",
    "workspace_binding_and_tool_budgets_are_enforced_before_execution",
    "turn_deadline_revokes_an_inflight_tool_instead_of_waiting_for_its_own_timeout",
)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def inventory(root):
    """Capture bytes, executable modes and symlinks; no external link may ship."""
    entries = {}
    root = root.resolve(strict=True)
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            if not path.resolve(strict=True).is_relative_to(root):
                raise RuntimeError(f"Runtime symlink leaves resource directory: {relative}")
            entries[relative] = dict(kind="symlink", target=os.readlink(path))
        elif path.is_file():
            entries[relative] = dict(kind="file", bytes=path.stat().st_size, sha256=sha256(path), executable=bool(path.stat().st_mode & 0o111))
        elif not path.is_dir():
            raise RuntimeError(f"Special runtime resource: {relative}")
    return entries


def clean_build_environment():
    # This command only produces a local candidate. Do not pick up signing or
    # notarization credentials from an unrelated developer shell configuration.
    return {key: value for key, value in os.environ.items()
            if not key.startswith(("APPLE_", "TAURI_SIGNING_", "TAURI_PRIVATE_KEY"))}


def restore_runtime_links(expected_root, resources, expected):
    """Tauri's generic resource copy dereferences PyInstaller dylib aliases.

    This candidate has no final bundle signature yet. Restore only validated aliases,
    before final inventory verification and any later release signing step.
    """
    restored = 0
    for relative, entry in expected.items():
        if entry["kind"] != "symlink":
            continue
        copied = resources / relative
        if copied.is_symlink():
            continue
        target = (expected_root / relative).resolve(strict=True)
        if not target.is_file() or not copied.is_file() or sha256(copied) != sha256(target):
            raise RuntimeError(f"Cannot safely restore copied runtime alias: {relative}")
        copied.unlink()
        copied.symlink_to(entry["target"])
        restored += 1
    return restored


def run(command, stage, label, env=None, timeout=None):
    print(f"{label}…", flush=True)
    log = stage / f"{label.replace(' ', '-')}.log"
    with log.open("w") as output:
        result = subprocess.run(command, cwd=REPOSITORY, env=env, stdout=output, stderr=subprocess.STDOUT, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"{label} failed ({result.returncode}); see {log}\n{log.read_text()[-8000:]}")
    return log


def integration_executable(log, target, kind="test"):
    executables = set()
    for line in log.read_text().splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue  # Cargo progress on stderr shares the build log.
        if (message.get("reason") == "compiler-artifact"
                and message["target"]["name"] == target
                and kind in message["target"]["kind"] and message["profile"].get("test") and message.get("executable")):
            executables.add(Path(message["executable"]))
    if len(executables) != 1:
        raise RuntimeError(f"Expected exactly one compiled {target} test executable")
    executable = executables.pop().resolve(strict=True)
    if not executable.is_relative_to(REPOSITORY / "target") or not executable.is_file():
        raise RuntimeError("Unexpected integration-test output path")
    return executable


def native_minimum(root):
    native = []
    for path in root.rglob("*"):
        if path.is_file() and not path.is_symlink():
            with path.open("rb") as stream:
                if stream.read(4) in MAGIC:
                    native.append(path)
    if not native:
        raise RuntimeError("Staged runtime contains no Mach-O code")
    minimum = (14, 0)  # Declared frontend baseline; OS acceptance still required.
    for offset in range(0, len(native), 64):
        result = subprocess.run(["/usr/bin/otool", "-l", *map(str, native[offset:offset+64])], check=True, capture_output=True, text=True)
        versions = re.findall(r"\bminos\s+(\d+(?:\.\d+)*)", result.stdout)
        versions += re.findall(r"cmd LC_VERSION_MIN_MACOSX\s+cmdsize \d+\s+version (\d+(?:\.\d+)*)", result.stdout)
        for version in versions:
            minimum = max(minimum, tuple(map(int, version.split("."))))
    return ".".join(map(str, minimum)), len(native)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, default=REPOSITORY / "docs/release-staging-experiment.json")
    options = parser.parse_args()
    if platform.system() != "Darwin":
        raise RuntimeError("This candidate builder targets macOS only")
    if Path(sys.prefix).resolve() != (REPOSITORY / ".venv").resolve():
        raise RuntimeError("Use the pinned project .venv/bin/python")
    source_fingerprints = {name: sha256(REPOSITORY / name) for name in SOURCE_INPUTS}
    stage = Path(tempfile.mkdtemp(prefix="release-staging-", dir=REPOSITORY / "target"))
    print(f"Staging files and logs: {stage}", flush=True)
    env = clean_build_environment()
    # All paths below refer to host binaries in this cache. An inherited target
    # directory/triple must not cause a successful build to probe stale outputs.
    env["CARGO_TARGET_DIR"] = str(REPOSITORY / "target")
    env.pop("CARGO_BUILD_TARGET", None)
    run([sys.executable, "-m", "PyInstaller", "--clean", "--noconfirm", "--workpath", str(stage / "pyinstaller"), "--distpath", str(stage / "python"), "packaging/worker.spec"], stage, "Build pinned worker", env)
    run(["cargo", "build", "--locked", "--release", "-p", "cadpad-core", "-p", "cadpad-mcp", "--bins", "--example", "packaged_runtime_check", "--example", "packaged_limits_check", "--example", "pendulum_benchmark", "--example", "linkage_benchmark"], stage, "Build native runtime", env)
    cache_build_log = run(["cargo", "test", "--locked", "--release", "-p", "cadpad-core", "--test", "worker_cache", "--test", "cache_buffers", "--test", "playback", "--test", "motion_tests", "--test", "stl_quality", "--test", "display_reuse", "--test", "geometry_cache", "--no-run", "--message-format=json"], stage, "Build packaged integration checkers", env)
    cache_checker = integration_executable(cache_build_log, "worker_cache")
    playback_checker = integration_executable(cache_build_log, "playback")
    buffer_checker = integration_executable(cache_build_log, "cache_buffers")
    motion_test_checker = integration_executable(cache_build_log, "motion_tests")
    stl_checker = integration_executable(cache_build_log, "stl_quality")
    display_reuse_checker = integration_executable(cache_build_log, "display_reuse")
    geometry_cache_checker = integration_executable(cache_build_log, "geometry_cache")
    agent_build_log = run(["cargo", "test", "--locked", "--release", "-p", "cadpad-agent", "--test", "loop", "--no-run", "--message-format=json"], stage, "Build packaged agent checker", env)
    agent_checker = integration_executable(agent_build_log, "loop")
    expected_root = stage / "resources"
    expected_root.mkdir()
    shutil.move(str(stage / "python/cadpad-worker"), expected_root / "worker")
    (expected_root / "mcp").mkdir()
    for source, destination in [("cadpad-mcp", "mcp/cadpad-mcp"), ("cadpad-worker-guard", "cadpad-worker-guard")]:
        shutil.copy2(REPOSITORY / "target/release" / source, expected_root / destination)
    run([sys.executable, "scripts/rust_notices.py", "--output", str(expected_root / "third-party/rust")], stage, "Collect Cargo and toolchain notices", env)
    expected = inventory(expected_root)
    minimum, native_count = native_minimum(expected_root)
    manifest = stage / "runtime-inventory.json"
    manifest.write_text(json.dumps(expected, indent=2) + "\n")
    overlay = stage / "tauri-resources.json"
    overlay.write_text(json.dumps(dict(bundle=dict(active=True, targets=["app"], resources={str(expected_root) + "/": ""}, macOS=dict(signingIdentity=None, minimumSystemVersion=minimum, hardenedRuntime=True))), indent=2))
    # Reuse compilation artifacts, but never replace a bundle the user may have
    # open. The bundle-only command reads the copied executable in this stage.
    run(["npm", "run", "tauri", "-w", "@cadpad/desktop", "--", "build", "--ci", "--no-bundle", "--config", str(overlay)], stage, "Build release app", env)
    bundle_target = stage / "bundle-target"
    (bundle_target / "release").mkdir(parents=True)
    shutil.copy2(REPOSITORY / "target/release/cadpad-desktop", bundle_target / "release/cadpad-desktop")
    bundle_env = {**env, "CARGO_TARGET_DIR": str(bundle_target)}
    run(["npm", "run", "tauri", "-w", "@cadpad/desktop", "--", "bundle", "--ci", "--bundles", "app", "--config", str(overlay)], stage, "Bundle isolated app", bundle_env)
    app = bundle_target / "release/bundle/macos/Cadpad.app"
    resources = app / "Contents/Resources"
    restored_links = restore_runtime_links(expected_root, resources, expected)
    actual = inventory(resources)
    for path, entry in expected.items():
        if actual.get(path) != entry:
            raise RuntimeError(f"Bundling changed or omitted runtime resource: {path}")
    with (app / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    if info["CFBundleIdentifier"] != "app.cadpad.desktop" or info["LSMinimumSystemVersion"] != minimum:
        raise RuntimeError("Unexpected release bundle identity or deployment target")
    main_executable = app / "Contents/MacOS" / info["CFBundleExecutable"]
    if not main_executable.is_file() or not main_executable.stat().st_mode & stat.S_IXUSR:
        raise RuntimeError("Packaged desktop executable is absent or nonexecutable")
    run(["/usr/bin/codesign", "--force", "--sign", "-", "--options", "runtime", str(app)], stage, "Sign local candidate", env)
    run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)], stage, "Verify local signature", env)
    signed_resources = inventory(resources)
    if any(signed_resources.get(path) != entry for path, entry in expected.items()):
        raise RuntimeError("Final local signing changed the staged runtime resources")
    probe_env = dict(PATH="", HOME=str(stage), TMPDIR=str(stage), LANG="en_US.UTF-8")
    frontend_build_log = run(["cargo", "test", "--locked", "--release", "-p", "cadpad-desktop", "--bin", "cadpad-desktop", "--features", "tauri/custom-protocol", "--no-run", "--message-format=json"], stage, "Build embedded frontend checker", env)
    frontend_checker = integration_executable(frontend_build_log, "cadpad-desktop", "bin")
    frontend_dump = stage / "frontend-check"
    frontend_dump.mkdir()
    frontend_native_log = run([str(frontend_checker), "frontend_assets::release_assets_and_real_startup_reply_are_complete", "--ignored", "--exact", "--nocapture"], stage, "Extract embedded frontend", {**probe_env, "CADPAD_FRONTEND_CHECK_DIR": str(frontend_dump)})
    if "test result: ok. 1 passed; 0 failed" not in frontend_native_log.read_text():
        raise RuntimeError("Embedded frontend asset check did not pass")
    frontend_entry_log = run([shutil.which("node"), "scripts/check_frontend_startup.mjs", str(frontend_dump)], stage, "Probe production frontend entry", env)
    frontend_result = json.loads(frontend_entry_log.read_text().splitlines()[-1])
    if frontend_result.get("status") != "passed":
        raise RuntimeError("Production frontend entry did not render the workspace in the CPU DOM check")
    frontend_startup_probe = dict(status="passed", test_binary=str(frontend_checker), test_binary_sha256=sha256(frontend_checker), native_log=str(frontend_native_log), native_log_sha256=sha256(frontend_native_log), log=str(frontend_entry_log), log_sha256=sha256(frontend_entry_log), result=frontend_result, ui_launched=False, actual_webview_verified=False)
    probe_log = run([str(REPOSITORY / "target/release/examples/packaged_runtime_check"), str(resources)], stage, "Probe packaged runtime", probe_env, timeout=600)
    probe = json.loads(probe_log.read_text())
    if (probe["status"] != "passed" or not probe["empty_path"]
            or not probe.get("successful_rebuild_after_failures")
            or not probe.get("job_admission", {}).get("passed")
            or {case["case"] for case in probe.get("failure_recovery", [])}
            != {"execution_error", "invalid_intended_solid", "native_exit", "cancel_entered_native_loop"}):
        raise RuntimeError("Packaged runtime probe did not pass with empty PATH")
    budget_log = run([str(REPOSITORY / "target/release/examples/packaged_limits_check"), str(resources)], stage, "Probe packaged budgets", probe_env, timeout=600)
    budgets = json.loads(budget_log.read_text())
    if (budgets["status"] != "passed" or not budgets["empty_path"]
            or {item["resource"] for item in budgets.get("probes", [])}
            != {"cpu_time_ms", "memory_bytes", "operation_time_ms"}):
        raise RuntimeError("Packaged resource-budget probes did not pass")
    cache_env = {**probe_env, "CADPAD_TEST_PACKAGED_RESOURCES": str(resources)}
    cache_log = run([str(cache_checker), "--test-threads=1", "--nocapture"], stage, "Probe packaged idle registries", cache_env, timeout=600)
    cache_output = cache_log.read_text()
    if any(f"test {name} ... ok" not in cache_output for name in IDLE_CACHE_CASES):
        raise RuntimeError("Packaged idle-registry checks did not all run")
    idle_cache = dict(status="passed", tests=list(IDLE_CACHE_CASES), test_binary=str(cache_checker),
                      test_binary_sha256=sha256(cache_checker), log=str(cache_log), log_sha256=sha256(cache_log),
                      empty_path=True, required_containment=True, test_threads=1)
    buffer_log = run([str(buffer_checker), "--test-threads=1", "--nocapture"], stage, "Probe current mesh buffers", cache_env, timeout=600)
    buffer_output = buffer_log.read_text()
    if any(f"test {name} ... ok" not in buffer_output for name in CURRENT_BUFFER_CASES):
        raise RuntimeError("Packaged current-buffer cleanup checks did not all run")
    current_buffer_probe = dict(status="passed", tests=list(CURRENT_BUFFER_CASES),
                                test_binary=str(buffer_checker), test_binary_sha256=sha256(buffer_checker),
                                log=str(buffer_log), log_sha256=sha256(buffer_log), empty_path=True,
                                required_containment=True, test_threads=1, desktop_execution=False)
    pendulum_checker = REPOSITORY / "target/release/examples/pendulum_benchmark"
    acceptance = json.loads((REPOSITORY / "fixtures/physics_pendulum.acceptance.json").read_text())
    pendulum_cases = []
    for scale in acceptance["scales"]:
        for amplitude in acceptance["amplitudes_rad"]:
            log = run([str(pendulum_checker), "--resources", str(resources), "--scale", str(scale), "--amplitude", str(amplitude)],
                      stage, f"Probe pendulum {scale} {amplitude}", probe_env, timeout=600)
            case = json.loads(log.read_text())
            if not case.get("passed") or case["scale"] != scale or case["amplitude_rad"] != amplitude:
                raise RuntimeError("Packaged pendulum reference did not pass the declared case")
            pendulum_cases.append(case)
    pendulum_probe = dict(status="passed", cases=pendulum_cases, checker=str(pendulum_checker),
                          checker_sha256=sha256(pendulum_checker), empty_path=True, required_containment=True,
                          desktop_execution=False)
    linkage_checker = REPOSITORY / "target/release/examples/linkage_benchmark"
    linkage_acceptance = json.loads((REPOSITORY / "fixtures/physics_linkage.acceptance.json").read_text())
    linkage_cases = []
    for scale in linkage_acceptance["scales"]:
        log = run([str(linkage_checker), "--resources", str(resources), "--scale", str(scale)],
                  stage, f"Probe linkage {scale}", probe_env, timeout=600)
        case = json.loads(log.read_text())
        if not case.get("passed") or case["scale"] != scale:
            raise RuntimeError("Packaged closed-linkage reference did not pass the declared case")
        linkage_cases.append(case)
    linkage_probe = dict(status="passed", cases=linkage_cases, checker=str(linkage_checker),
                         checker_sha256=sha256(linkage_checker), empty_path=True,
                         required_containment=True, desktop_execution=False)
    playback_log = run([str(playback_checker), "portable_recordings", "--test-threads=1", "--nocapture"],
                       stage, "Probe portable recordings", cache_env, timeout=600)
    playback_output = playback_log.read_text()
    if any(f"test {name} ... ok" not in playback_output for name in PORTABLE_RECORDING_CASES):
        raise RuntimeError("Packaged portable-recording checks did not all run")
    portable_recordings_probe = dict(status="passed", tests=list(PORTABLE_RECORDING_CASES),
                                    test_binary=str(playback_checker), test_binary_sha256=sha256(playback_checker),
                                    log=str(playback_log), log_sha256=sha256(playback_log), empty_path=True,
                                    required_containment=True, test_threads=1, desktop_execution=False)
    agent_log = run([str(agent_checker), "--test-threads=1", "--nocapture"], stage, "Probe packaged builtin agent", cache_env, timeout=600)
    agent_output = agent_log.read_text()
    if any(f"test {name} ... ok" not in agent_output for name in BUILTIN_AGENT_CASES):
        raise RuntimeError("Packaged built-in agent checks did not all run")
    builtin_agent_probe = dict(status="passed", tests=list(BUILTIN_AGENT_CASES),
                              test_binary=str(agent_checker), test_binary_sha256=sha256(agent_checker),
                              log=str(agent_log), log_sha256=sha256(agent_log), empty_path=True,
                              required_containment=True, test_threads=1, desktop_execution=False,
                              provider="deterministic policies; loopback HTTP/SSE for the modeling loop",
                              credentials="in-memory fixture", capture="fixture PNG", real_provider_account=False,
                              real_keychain=False, ui_verified=False)
    motion_test_log = run([str(motion_test_checker), "--test-threads=1", "--nocapture"], stage,
                          "Probe saved motion tests", cache_env, timeout=600)
    if "test result: ok. 5 passed; 0 failed" not in motion_test_log.read_text():
        raise RuntimeError("Packaged motion-test checks did not all pass")
    motion_test_probe = dict(status="passed", test_binary=str(motion_test_checker),
                             test_binary_sha256=sha256(motion_test_checker), tests=5,
                             log=str(motion_test_log), log_sha256=sha256(motion_test_log),
                             empty_path=True, required_containment=True, desktop_execution=False,
                             natural_language_provider_verified=False)
    stl_log = run([str(stl_checker), "--test-threads=1", "--nocapture"], stage,
                  "Probe curved STL quality", cache_env, timeout=600)
    if "test result: ok. 2 passed; 0 failed" not in stl_log.read_text():
        raise RuntimeError("Packaged curved STL quality check did not pass")
    stl_probe = dict(status="passed", test_binary=str(stl_checker),
                    test_binary_sha256=sha256(stl_checker), tests=2,
                    log=str(stl_log), log_sha256=sha256(stl_log), empty_path=True,
                    required_containment=True, desktop_execution=False,
                    declared_centroid_error_mm=0.06, relative_volume_error=0.005,
                    vertex_rounding_budget_mm=0.025)
    reuse_log = run([str(display_reuse_checker), "--test-threads=1", "--nocapture"], stage,
                    "Probe cross revision display reuse", cache_env, timeout=600)
    if "test result: ok. 3 passed; 0 failed" not in reuse_log.read_text():
        raise RuntimeError("Packaged cross-revision display reuse checks did not pass")
    display_reuse_probe = dict(status="passed", test_binary=str(display_reuse_checker),
                              test_binary_sha256=sha256(display_reuse_checker), tests=3,
                              log=str(reuse_log), log_sha256=sha256(reuse_log), empty_path=True,
                              required_containment=True, desktop_execution=False)
    geometry_log = run([str(geometry_cache_checker), "--test-threads=1", "--nocapture"], stage,
                       "Probe deterministic native geometry cache", cache_env, timeout=600)
    if "test result: ok. 2 passed; 0 failed" not in geometry_log.read_text():
        raise RuntimeError("Packaged native geometry cache checks did not pass")
    geometry_cache_probe = dict(status="passed", test_binary=str(geometry_cache_checker),
                               test_binary_sha256=sha256(geometry_cache_checker), tests=2,
                               log=str(geometry_log), log_sha256=sha256(geometry_log), empty_path=True,
                               required_containment=True, desktop_execution=False)
    run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)], stage, "Verify exercised candidate", env)
    if source_fingerprints != {name: sha256(REPOSITORY / name) for name in SOURCE_INPUTS}:
        raise RuntimeError("Candidate source inputs changed during build or probes")
    report = dict(status="passed", checked_at=datetime.now(timezone.utc).isoformat(), platform=platform.platform(), architecture=platform.machine(),
                  app=str(app), app_executable_sha256=sha256(main_executable), stage=str(stage), declared_minimum_macos=minimum,
                  native_code_files=native_count, runtime_entries=len(expected), runtime_bytes=sum(v.get("bytes",0) for v in expected.values()),
                  resource_inventory=str(manifest), resource_inventory_sha256=sha256(manifest), runtime_copy_exact=True, restored_runtime_symlinks=restored_links,
                  local_adhoc_candidate=True, ad_hoc_bundle_signature_verified=True, developer_id_signed=False,
                  signing_credentials_removed=True, isolated_bundle=True, ui_launched=False, clean_machine_test=False, notarized=False,
                  frontend_startup_probe=frontend_startup_probe,
                  probe=probe, budget_probe=budgets, idle_cache_probe=idle_cache, pendulum_probe=pendulum_probe,
                  linkage_probe=linkage_probe,
                  current_buffer_probe=current_buffer_probe,
                  portable_recordings_probe=portable_recordings_probe,
                  builtin_agent_probe=builtin_agent_probe,
                  motion_test_probe=motion_test_probe,
                  stl_probe=stl_probe, display_reuse_probe=display_reuse_probe, geometry_cache_probe=geometry_cache_probe,
                  post_probe_signature_verified=True, listed_source_inputs_unchanged=True,
                  source_fingerprints=source_fingerprints)
    report_path = options.report.resolve()
    report_path.parent.mkdir(parents=True, exist_ok=True)
    temporary = report_path.with_suffix(".tmp")
    temporary.write_text(json.dumps(report, indent=2) + "\n")
    temporary.replace(report_path)
    print(f"Local candidate verified; report: {report_path}", flush=True)


if __name__ == "__main__":
    main()
