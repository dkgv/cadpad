"""Measure the predeclared linkage reference with contained development/bundled workers."""
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


def main():
    repository = Path(__file__).resolve().parents[1]
    if Path(sys.prefix).resolve() != (repository / ".venv").resolve():
        raise RuntimeError("Use the pinned project .venv/bin/python")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--resources", type=Path, help="Existing .app/Contents/Resources; never rebuilt or modified")
    options = parser.parse_args()
    sources = ["Cargo.lock", "uv.lock", "crates/core/src/dynamics.rs", "crates/core/src/constraints.rs",
               "crates/core/src/kinematics.rs", "crates/core/src/motion.rs", "crates/core/src/playback.rs",
               "fixtures/physics_linkage.py", "fixtures/physics_linkage.mechanism.json",
               "fixtures/physics_linkage.acceptance.json", "crates/core/examples/support/linkage.rs",
               "crates/core/examples/linkage_benchmark.rs", "crates/core/tests/linkage_reference.rs",
               "scripts/check_linkage.py"]
    source_fingerprints = {name: hashlib.sha256((repository / name).read_bytes()).hexdigest() for name in sources}
    subprocess.run(["cargo", "build", "--locked", "--release", "-p", "cadpad-core", "--bins", "--example", "linkage_benchmark"], cwd=repository, check=True)
    benchmark = repository / "target/release/examples/linkage_benchmark"
    acceptance = json.loads((repository / "fixtures/physics_linkage.acceptance.json").read_text())
    cases = []
    modes = [("development", [])]
    if options.resources is not None:
        resources = options.resources.resolve(strict=True)
        modes.append(("bundled", ["--resources", str(resources)]))
    for mode, arguments in modes:
        for scale in acceptance["scales"]:
            command = [str(benchmark), "--scale", str(scale), *arguments]
            with tempfile.TemporaryDirectory(prefix="cadpad-linkage-reference-") as temporary:
                result = subprocess.run(command, cwd=repository, env=dict(PATH="", HOME=temporary, TMPDIR=temporary, LANG="en_US.UTF-8"), capture_output=True, text=True, timeout=600)
            try:
                report = json.loads(result.stdout)
            except json.JSONDecodeError:
                report = dict(passed=False, error=result.stderr[-16384:])
            cases.append(dict(mode=mode, scale=scale, command=command, exit_code=result.returncode, report=report))
    files = [*sources, "target/release/examples/linkage_benchmark", "target/release/cadpad-worker-guard"]
    if options.resources is not None:
        files.extend([resources / "worker/cadpad-worker", resources / "cadpad-worker-guard"])
    fingerprints = {}
    for name in files:
        with (repository / name).open("rb") as stream:
            fingerprints[str(name)] = hashlib.file_digest(stream, "sha256").hexdigest()
    if any(fingerprints[name] != digest for name, digest in source_fingerprints.items()):
        raise RuntimeError("Linkage reference sources changed during build or measurement")
    hardware = {}
    if platform.system() == "Darwin":
        for name in ("hw.model", "hw.memsize", "machdep.cpu.brand_string"):
            result = subprocess.run(["/usr/sbin/sysctl", "-n", name], capture_output=True, text=True, check=True)
            hardware[name] = result.stdout.strip()
    locked = tomllib.loads((repository / "Cargo.lock").read_text())
    engine = [dict(name=package["name"], version=package["version"]) for package in locked["package"] if package["name"] in ("rapier3d-f64", "parry3d-f64", "glam", "nalgebra")]
    passed = bool(cases) and all(case["exit_code"] == 0 and case["report"]["passed"] for case in cases)
    print(json.dumps(dict(status="passed" if passed else "failed", checked_at=datetime.now(timezone.utc).isoformat(), platform=platform.platform(), architecture=platform.machine(), hardware=hardware, engine_dependencies=engine, fingerprints=fingerprints, empty_path=True, contained_workers=True, ui_verified=False, engineering_validation=False, cases=cases), indent=2))
    raise SystemExit(0 if passed else 1)


if __name__ == "__main__":
    main()
