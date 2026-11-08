"""Run the preset native friction reference without opening or changing the app."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def runtime_inventory(root):
    root = root.resolve(strict=True)
    entries = {}
    for file in sorted(root.rglob("*")):
        relative = file.relative_to(root).as_posix()
        if file.is_symlink():
            if not file.resolve(strict=True).is_relative_to(root):
                raise RuntimeError("Packaged runtime symlink leaves resource root")
            entries[relative] = dict(kind="symlink", target=os.readlink(file))
        elif file.is_file():
            entries[relative] = dict(kind="file", bytes=file.stat().st_size, sha256=digest(file))
        elif not file.is_dir():
            raise RuntimeError("Special file in packaged runtime")
    return entries


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--resources", type=Path, help="Also test an existing packaged resource root, read-only")
    parser.add_argument("--output", type=Path, required=True, help="New experiment report file")
    args = parser.parse_args()
    repository = Path(__file__).resolve().parents[1]
    if Path(sys.prefix).resolve() != (repository / ".venv").resolve():
        raise RuntimeError("Run with the pinned .venv/bin/python")
    if args.output.exists():
        raise FileExistsError("Experiment output already exists; choose a new report file")
    stage = Path(tempfile.mkdtemp(prefix="friction-evidence-", dir=repository / "target"))
    print(f"Evidence: {stage}", file=sys.stderr, flush=True)
    with (stage / "build.log").open("w") as log:
        subprocess.run(["cargo", "build", "--locked", "--release", "-p", "cadpad-core", "--bin", "cadpad-worker-guard", "--example", "friction_benchmark"], cwd=repository, stdout=log, stderr=subprocess.STDOUT, check=True)
    binary = repository / "target/release/examples/friction_benchmark"
    modes = [("development", [])]
    if args.resources:
        modes.append(("packaged", ["--resources", str(args.resources.resolve(strict=True))]))
    scales = [1, 2]
    cases = []
    for mode, arguments in modes:
        for scale in scales:
            label = f"{mode}-scale-{scale}"
            command = [str(binary), *arguments, "--scale", str(scale)]
            result = subprocess.run(command, cwd=repository, capture_output=True, text=True, timeout=600)
            (stage / f"{label}.stdout.json").write_text(result.stdout)
            (stage / f"{label}.stderr.log").write_text(result.stderr)
            try:
                report = json.loads(result.stdout)
            except json.JSONDecodeError:
                report = dict(passed=False, error=result.stderr[-16384:])
            fingerprints = {}
            for field in ("worker_executable", "guardian_executable"):
                if field in report:
                    fingerprints[field] = digest(report[field])
            cases.append(dict(mode=mode, scale=scale, exit_code=result.returncode, report=report, executable_fingerprints=fingerprints))
    runtime = None
    if args.resources:
        entries = runtime_inventory(args.resources)
        manifest = stage / "packaged-runtime-inventory.json"
        manifest.write_text(json.dumps(entries, indent=2) + "\n")
        runtime = dict(root=str(args.resources.resolve()), entries=len(entries), inventory=str(manifest), inventory_sha256=digest(manifest))
    version = subprocess.check_output(["rustc", "-vV"], cwd=repository, text=True)
    host = next(line.split(": ", 1)[1] for line in version.splitlines() if line.startswith("host:"))
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--offline", "--format-version", "1", "--filter-platform", host], cwd=repository, text=True))
    engine = [dict(name=package["name"], version=package["version"]) for package in metadata["packages"] if package["name"] in {"rapier3d-f64", "parry3d-f64", "glam", "nalgebra"}]
    hardware = {}
    if platform.system() == "Darwin":
        for name in ("hw.model", "hw.memsize", "machdep.cpu.brand_string"):
            result = subprocess.run(["/usr/sbin/sysctl", "-n", name], capture_output=True, text=True, timeout=10)
            if result.returncode == 0:
                hardware[name] = result.stdout.strip()
    passed = all(case["exit_code"] == 0 and case["report"]["passed"] for case in cases)
    report = dict(status="passed" if passed else "failed", checked_at=datetime.now(timezone.utc).isoformat(), platform=platform.platform(),
                  hardware=hardware, rustc_version=version, engine_dependencies=engine, evidence=str(stage), cases=cases,
                  packaged_runtime=runtime, ui_launched=False, desktop_bundle_modified=False,
                  source_fingerprints={name:digest(repository / name) for name in ["Cargo.lock", "uv.lock", "fixtures/physics_friction.py", "fixtures/physics_friction.mechanism.json", "fixtures/physics_friction.acceptance.json", "crates/core/src/dynamics.rs", "crates/core/examples/support/friction.rs", "crates/core/examples/friction_benchmark.rs", "scripts/check_friction.py"]},
                  benchmark_executable_sha256=digest(binary))
    with args.output.open("x") as stream:
        json.dump(report, stream, indent=2)
        stream.write("\n")
    print(json.dumps(dict(status=report["status"], output=str(args.output), modes=[mode for mode, _ in modes])))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
