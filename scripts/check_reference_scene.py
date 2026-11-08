"""Geometry/transport preparation only; does not measure a rendered viewport."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import time
from datetime import datetime, timezone


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bundled", action="store_true")
    parser.add_argument("--grid-side", type=int, default=10)
    parser.add_argument("--artifact-directory", type=Path)
    options = parser.parse_args()
    repository = Path(__file__).resolve().parents[1]
    fixture = repository / "fixtures/performance_reference"
    annotations = json.loads((fixture / "annotations.json").read_text())
    command = [str(repository / "target/worker-bundle/cadpad-worker/cadpad-worker")] if options.bundled else [str(repository / ".venv/bin/python"), "-m", "cadpad_worker"]
    with tempfile.TemporaryDirectory(prefix="cadpad-reference-") as temporary:
        root = Path(temporary).resolve(); source = root / "source"; source.mkdir(); cache = root / "cache"
        shutil.copyfile(fixture / "model.py", source / "model.py")
        request = dict(jsonrpc="2.0", id=1, protocol_version=1, worker_generation="reference", revision="reference-r1", method="build",
                       params=dict(source_root=str(source), parameters={"grid_side":options.grid_side}, annotations=annotations))
        start = time.perf_counter()
        child = subprocess.run(command + ["--cache",str(cache),"--generation","reference"], input=json.dumps(request)+"\n", capture_output=True, text=True, timeout=180,
                               env={"PATH":"/usr/bin:/bin","PYTHONPATH":str(repository/"python"),"PYTHONNOUSERSITE":"1","PYTHONDONTWRITEBYTECODE":"1","TMPDIR":temporary})
        elapsed = time.perf_counter()-start
        assert child.returncode==0, child.stderr[-16384:]
        response = json.loads(child.stdout); assert "result" in response, response
        model = response["result"]; parts = model["parts"]; unique = {}; repeated_bytes = 0
        for part in parts:
            assert part["mesh"]["format"]=="cadpad-mesh-v2"
            for descriptor in part["mesh"]["buffers"].values():
                handle=descriptor["handle"]; repeated_bytes+=descriptor["bytes"]
                if handle in unique: assert unique[handle]==descriptor
                else:
                    data=(cache/handle).read_bytes();assert len(data)==descriptor["bytes"] and hashlib.sha256(data).hexdigest()==descriptor["sha256"]
                    unique[handle]=descriptor
        triangles=sum(part["mesh"]["buffers"]["triangles"]["bytes"]//12 for part in parts)
        pinned=[item for item in model["annotations"] if item.get("saved_id")]
        assert len(parts)==options.grid_side**2
        assert len(pinned)==30 and all(item["status"]=="resolved" and item["value"]==200 for item in pinned)
        assert len({part["geometry_fingerprint"]["sha256"] for part in parts})==1
        assert abs(triangles/len(parts)-10000)<1000
        if options.artifact_directory is not None:
            # Never overwrite an existing directory; benchmark callers own its lifetime.
            options.artifact_directory.mkdir(parents=True, exist_ok=False)
            (options.artifact_directory/"scene.json").write_text(json.dumps(model))
            for handle in unique:
                shutil.copyfile(cache/handle,options.artifact_directory/handle)
        print(json.dumps(dict(status="passed",checked_at=datetime.now(timezone.utc).isoformat(),platform=platform.platform(),mode="bundled" if options.bundled else "development",
                              scope="trusted direct worker geometry preparation over stdio; no native containment, desktop transport, UI or GPU timing",grid_side=options.grid_side,instances=len(parts),scene_triangle_count=triangles,
                              unique_geometry_count=1,pinned_dimension_count=30,dimension_value_mm=200,unique_buffer_count=len(unique),unique_buffer_bytes=sum(value["bytes"] for value in unique.values()),
                              buffer_bytes_if_copied_per_instance=repeated_bytes,spawn_build_reply_seconds=elapsed,rpc_metadata_bytes=len(child.stdout.encode()),
                              worker_model_sha256=hashlib.sha256((repository/"python/cadpad_worker/model.py").read_bytes()).hexdigest(),
                              executable_sha256=hashlib.sha256(Path(command[0]).resolve().read_bytes()).hexdigest(),
                              source_sha256=hashlib.sha256((fixture/"model.py").read_bytes()).hexdigest(),annotations_sha256=hashlib.sha256((fixture/"annotations.json").read_bytes()).hexdigest(),dependencies=model["dependencies"]),indent=2))


if __name__ == "__main__": main()
