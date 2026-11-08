"""Measure fixed pendulum cases using contained development and optional bundled workers."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import tomllib


def fingerprint(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    repository = Path(__file__).resolve().parents[1]
    if Path(sys.prefix).resolve() != (repository / ".venv").resolve():
        raise RuntimeError("Use the pinned project .venv/bin/python")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--resources", type=Path, help="Existing .app/Contents/Resources; no development fallback")
    options = parser.parse_args()
    inputs = [
        "Cargo.lock", "uv.lock", "crates/core/src/dynamics.rs",
        "crates/core/src/kinematics.rs", "crates/core/src/collision.rs",
        "fixtures/physics_pendulum.py", "fixtures/physics_pendulum.mechanism.json",
        "fixtures/physics_pendulum.acceptance.json",
        "crates/core/examples/support/pendulum.rs",
        "crates/core/examples/pendulum_benchmark.rs",
        "crates/core/tests/pendulum_reference.rs", "scripts/check_pendulum.py",
    ]
    fingerprints = {name: fingerprint(repository / name) for name in inputs}
    subprocess.run([
        "cargo", "build", "--locked", "--release", "-p", "cadpad-core",
        "--bins", "--example", "pendulum_benchmark",
    ], cwd=repository, check=True)
    benchmark = repository / "target/release/examples/pendulum_benchmark"
    acceptance = json.loads((repository / "fixtures/physics_pendulum.acceptance.json").read_text())
    binaries = [benchmark, repository / "target/release/cadpad-worker-guard"]
    modes = [("development", [])]
    if options.resources is not None:
        resources = options.resources.resolve(strict=True)
        modes.append(("bundled", ["--resources", str(resources)]))
        binaries.extend([resources / "worker/cadpad-worker", resources / "cadpad-worker-guard"])
    fingerprints.update({str(path): fingerprint(path) for path in binaries})
    cases = []
    for mode, arguments in modes:
        for scale in acceptance["scales"]:
            for amplitude in acceptance["amplitudes_rad"]:
                command = [str(benchmark), "--scale", str(scale), "--amplitude", str(amplitude), *arguments]
                with tempfile.TemporaryDirectory(prefix="cadpad-pendulum-reference-") as temporary:
                    result = subprocess.run(
                        command, cwd=repository,
                        env=dict(PATH="", HOME=temporary, TMPDIR=temporary, LANG="en_US.UTF-8"),
                        capture_output=True, text=True, timeout=600,
                    )
                try:
                    report = json.loads(result.stdout)
                except json.JSONDecodeError:
                    report = dict(passed=False, error=result.stderr[-16384:])
                cases.append(dict(mode=mode, scale=scale, amplitude_rad=amplitude,
                                  command=command, exit_code=result.returncode, report=report))
    unchanged = all(fingerprint(repository / name) == value for name, value in fingerprints.items())
    hardware = {}
    if platform.system() == "Darwin":
        for name in ("hw.model", "hw.memsize", "machdep.cpu.brand_string"):
            result = subprocess.run(["/usr/sbin/sysctl", "-n", name], capture_output=True, text=True, check=True)
            hardware[name] = result.stdout.strip()
    locked = tomllib.loads((repository / "Cargo.lock").read_text())
    engine = [dict(name=package["name"], version=package["version"])
              for package in locked["package"]
              if package["name"] in ("rapier3d-f64", "parry3d-f64", "glam", "nalgebra")]
    passed = unchanged and bool(cases) and all(case["exit_code"] == 0 and case["report"]["passed"] for case in cases)
    print(json.dumps(dict(
        status="passed" if passed else "failed", checked_at=datetime.now(timezone.utc).isoformat(),
        platform=platform.platform(), architecture=platform.machine(), hardware=hardware,
        engine_dependencies=engine, fingerprints=fingerprints, inputs_and_binaries_unchanged=unchanged,
        empty_path=True, contained_workers=True, ui_verified=False, engineering_validation=False,
        cases=cases,
    ), indent=2))
    raise SystemExit(0 if passed else 1)


if __name__ == "__main__":
    main()
