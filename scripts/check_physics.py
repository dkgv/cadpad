"""Measure the preset SI slider reference through optimized, contained core calls."""
import json
import hashlib
import platform
from pathlib import Path
import subprocess
from datetime import datetime, timezone


def main():
    repository = Path(__file__).resolve().parents[1]
    hardware = {}
    if platform.system() == "Darwin":
        for name in ("hw.model", "hw.memsize", "machdep.cpu.brand_string"):
            result = subprocess.run(["/usr/sbin/sysctl", "-n", name], capture_output=True, text=True, timeout=10)
            if result.returncode == 0:
                hardware[name] = result.stdout.strip()
    cases = []
    command = ["cargo", "run", "--locked", "--release", "-p", "cadpad-core", "--example", "physics_benchmark"]
    for mode, args in (("development", []), ("development_padded", ["--", "--padded-proxy"]), ("bundled", ["--", "--bundled"]), ("bundled_padded", ["--", "--bundled", "--padded-proxy"])):
        result = subprocess.run(command + args, cwd=repository, capture_output=True, text=True, timeout=600)
        try:
            report = json.loads(result.stdout)
        except json.JSONDecodeError:
            report = dict(passed=False, error=result.stderr[-16384:])
        cases.append(dict(mode=mode, command=command + args, exit_code=result.returncode, report=report))
    passed = all(case["exit_code"] == 0 and case["report"]["passed"] for case in cases)
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1"], cwd=repository))
    engine = [dict(name=package["name"],version=package["version"]) for package in metadata["packages"]
              if package["name"] in ("cadpad-core","rapier3d-f64","parry3d-f64","glam","nalgebra")]
    fingerprints = {name:hashlib.sha256((repository/name).read_bytes()).hexdigest()
                    for name in ("Cargo.lock","uv.lock","target/release/examples/physics_benchmark",
                                 "target/worker-bundle/cadpad-worker/cadpad-worker")}
    print(json.dumps(dict(status="passed" if passed else "failed", checked_at=datetime.now(timezone.utc).isoformat(),
                          platform=platform.platform(), hardware=hardware, engine_dependencies=engine,
                          fingerprints=fingerprints, cases=cases), indent=2))
    raise SystemExit(0 if passed else 1)


if __name__ == "__main__":
    main()
