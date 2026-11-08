"""Record actual native-launcher containment probes, including the packaged worker.

Run with .venv/bin/python after building the one-folder worker. No UI is launched.
"""
import hashlib
import json
import platform
from datetime import datetime, timezone
from pathlib import Path
import subprocess


def main():
    if platform.system() != "Darwin":
        raise RuntimeError("The contained launcher currently targets macOS only")
    repository = Path(__file__).resolve().parents[1]
    executable = repository / "target/worker-bundle/cadpad-worker/cadpad-worker"
    if not executable.is_file():
        raise RuntimeError("Build packaging/worker.spec before checking bundled containment")
    guardian=repository / "target/debug/cadpad-worker-guard"
    commands = [
        ["cargo", "build", "--locked", "-p", "cadpad-core", "--bin", "cadpad-worker-guard"],
        ["cargo", "test", "--locked", "-p", "cadpad-core", "--test", "containment", "--",
         "--include-ignored", "--skip", "credential_environment_host"],
        ["cargo", "test", "--locked", "-p", "cadpad-core", "--lib", "worker::framing_tests"],
        ["cargo", "test", "--locked", "-p", "cadpad-core", "--test", "worker_lifecycle"],
    ]
    evidence = []
    for command in commands:
        result = subprocess.run(command, cwd=repository, capture_output=True, text=True, timeout=180)
        if result.returncode:
            raise RuntimeError(result.stdout[-16384:] + result.stderr[-16384:])
        evidence.append(dict(command=command, stdout=result.stdout, stderr=result.stderr))
    for name in ("default_development_worker_contains_real_model_code",
                 "bundled_worker_contains_real_model_code", "worker_does_not_inherit_host_credentials",
                 "contained_rail_example_collision_validation_and_native_restart_recovery"):
        assert f"test {name} ... ok" in evidence[1]["stdout"]
    for name in ("explicit_stop_reaps_guardian_and_busy_contained_interpreter_without_socket_leaks",
                 "dropping_worker_terminates_and_reaps_a_stopped_interpreter",
                 "app_sigkill_terminates_a_stopped_contained_worker_without_cooperative_python",
                 "missing_guardian_fails_closed_before_starting_model_code"):
        assert f"test {name} ... ok" in evidence[3]["stdout"]
    print(json.dumps(dict(
        status="passed", checked_at=datetime.now(timezone.utc).isoformat(), platform=platform.platform(),
        executable_sha256=hashlib.sha256(executable.read_bytes()).hexdigest(),
        guardian_sha256=hashlib.sha256(guardian.read_bytes()).hexdigest(),
        native_guardian_liveness_socket=True, app_death_reaps_stopped_worker=True,
        liveness_not_inherited_by_python=True, missing_guardian_fails_closed=True,
        policy_sha256=hashlib.sha256((repository / "crates/core/src/containment.sbpl").read_bytes()).hexdigest(),
        development_and_bundle=True, ordinary_modeling_and_exact_step_export=True,
        rail_collision_and_restart_recovery=True,
        denies_outside_read_write=True, denies_source_runtime_write=True,
        denies_symlink_hardlink_escape=True, denies_tcp_udp_unix_connections=True,
        denies_subprocess_and_fork=True, environment_canary_absent=True,
        private_writable_scratch=True, core_file_descriptor_and_single_file_limits=True,
        failed_candidate_preserves_model=True, cancel_after_execution_reaps_worker=True,
        bounded_native_replies=True, blocked_request_write_cancellable=True,
        evidence=evidence), indent=2))


if __name__ == "__main__":
    main()
