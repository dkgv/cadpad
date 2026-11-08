"""Exercise a bundled worker without consulting a Python executable on PATH."""
import argparse
import json
import hashlib
import platform
from datetime import datetime, timezone
from pathlib import Path
import subprocess
import tempfile
import os
import selectors
import math
import struct


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("executable", type=Path)
    options = parser.parse_args()
    executable = options.executable.resolve(strict=True)
    repository = Path(__file__).resolve().parents[1]
    with tempfile.TemporaryDirectory(prefix="cadpad-bundle-") as temporary:
        directory = Path(temporary)
        source = directory / "source"
        source.mkdir()
        (source / "model.py").write_text((repository / "fixtures/rail_carriage.py").read_text()+"\nangle('corner','rail',(10,0,0),(0,0,0),(0,10,0))\n")
        requests = []
        annotations = [dict(id="width", label="Rail length", definition=dict(kind="overall", instance_id="rail", axis="x")),
                       dict(id="broken", label="Missing part", definition=dict(kind="overall", instance_id="missing", axis="x")),
                       dict(id="angle", label="Corner", definition=dict(kind="published", publication_id="corner"),format=dict(unit="deg",precision=2))]
        for request_id, method, params in [(1, "build", dict(source_root=str(source), annotations=annotations, collision=json.loads((repository / "fixtures/rail_carriage.mechanism.json").read_text())["collision"])),
                                          (2, "export", dict(format="STEP")),
                                          (3, "inspect", dict(entities=[dict(revision="bundle-r1", instance_id="rail", entity_id="part:0")])),
                                          (4, "inspect", dict(query="interference",pairs=[["rail","carriage"]]))]:
            requests.append(dict(jsonrpc="2.0", id=request_id, protocol_version=1,
                                 worker_generation="bundle-test", revision="bundle-r1", method=method, params=params))
        result = subprocess.run([str(executable), "--cache", str(directory / "cache"), "--generation", "bundle-test"],
                                input="".join(json.dumps(request)+"\n" for request in requests),
                                capture_output=True, text=True, timeout=120,
                                env={"PATH": "/usr/bin:/bin", "TMPDIR": temporary, "LANG": "en_US.UTF-8"})
        if result.returncode:
            raise RuntimeError(result.stderr[-16384:])
        responses = [json.loads(line) for line in result.stdout.splitlines()]
        assert len(responses) == 4, result.stdout
        assert all("result" in response for response in responses), responses
        assert len(responses[0]["result"]["parts"]) == 2
        saved = {item["saved_id"]: item for item in responses[0]["result"]["annotations"] if "saved_id" in item}
        automatic = [item for item in responses[0]["result"]["annotations"] if item.get("automatic")]
        assert sorted(item["value"] for item in automatic) == [6,6,8]
        assert all(item["definition"]["kind"] == "geometry_circle" for item in automatic)
        assert saved["width"]["value"] == 200
        assert saved["angle"]["kind"] == "angle" and saved["angle"]["unit"] == "rad"
        assert saved["angle"]["value"] == math.pi/2 and saved["angle"]["vertex"] == [0,0,0]
        assert saved["broken"]["status"] == "broken" and "value" not in saved["broken"]
        assert responses[0]["result"]["collision_validation"]["method"] == "brep_boolean_coverage"
        assert responses[3]["result"]["pairs"][0]["interfering"] is False
        assert responses[3]["result"]["pairs"][0]["distance_mm"] > 0.019
        exported = directory / "cache" / responses[1]["result"]["handle"]
        assert exported.read_bytes().startswith(b"ISO-10303-21;")
        assert responses[0]["result"]["parts"][0]["bounds"] == responses[2]["result"]["entities"][0]["bounds"], "BRep dimensions changed after display tessellation"
        assert all(part["geometry_fingerprint"]["format"] == "occt-brep-v3-no-display-v1"
                   for part in responses[0]["result"]["parts"])
        for part in responses[0]["result"]["parts"]:
            mesh = part["mesh"]
            assert mesh["format"] == "cadpad-mesh-v2"
            buffers = mesh["buffers"]
            vertices = list(struct.iter_unpack("<fff", (directory / "cache" / buffers["vertices"]["handle"]).read_bytes()))
            for axis in range(3):
                assert abs(min(point[axis] for point in vertices)-part["bounds"]["min"][axis]) < 0.11
                assert abs(max(point[axis] for point in vertices)-part["bounds"]["max"][axis]) < 0.11
            for kind, buffer in (("face", "triangles_per_face"), ("edge", "segments_per_edge")):
                assert buffers[buffer]["bytes"] == len(mesh[kind+"_ids"])*4
                assert {value for value in mesh[kind+"_ids"] if value is not None} == {f"{kind}:{index}" for index in range(part[kind+"_count"])}
        assert responses[0]["result"]["parts"][0]["mesh"]["buffers"]["vertices"]["handle"] != responses[0]["result"]["parts"][1]["mesh"]["buffers"]["vertices"]["handle"]
        recovery_cache = directory / "recovery-cache"
        recovery_cache.mkdir()
        lease = (directory / "inherited-lease").open("w+")
        if os.name == "posix":
            import fcntl
            fcntl.flock(lease.fileno(), fcntl.LOCK_SH | fcntl.LOCK_NB)
        child = subprocess.Popen(
            [str(executable), "--cache", str(recovery_cache), "--generation", "recovery-test"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            pass_fds=(lease.fileno(),) if os.name == "posix" else (),
            env={"PATH": "/usr/bin:/bin", "TMPDIR": temporary, "LANG": "en_US.UTF-8"})
        def reply():
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout, selectors.EVENT_READ)
                if not selector.select(timeout=60):
                    raise TimeoutError("Bundled recovery did not reply within 60 seconds")
            return json.loads(child.stdout.readline())
        try:
            recovery = dict(jsonrpc="2.0", id=1, protocol_version=1,
                            worker_generation="recovery-test", revision="bundle-r1", method="build",
                            params={**requests[0]["params"], "generate_mesh": False,
                                    "parameters": responses[0]["result"]["parameters"],
                                    "expected_dependencies": responses[0]["result"]["dependencies"]})
            child.stdin.write(json.dumps(recovery) + "\n")
            child.stdin.flush()
            recovered = reply()
            assert "result" in recovered, recovered
            assert all("mesh" not in part for part in recovered["result"]["parts"])
            assert not list(recovery_cache.glob("*.bin")), "Native recovery allocated duplicate display buffers"
            assert [part["geometry_fingerprint"] for part in recovered["result"]["parts"]] == [
                part["geometry_fingerprint"] for part in responses[0]["result"]["parts"]]
            lease.close()
            if os.name == "posix":
                with (directory / "inherited-lease").open("r+") as probe:
                    try:
                        fcntl.flock(probe.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                    except BlockingIOError:
                        pass
                    else:
                        raise AssertionError("Bundled worker did not retain its inherited cache lease")
            child.stdin.write(json.dumps(dict(jsonrpc="2.0", id=2, protocol_version=1,
                                             worker_generation="recovery-test", revision="bundle-r1",
                                             method="remesh", params={})) + "\n")
            child.stdin.flush()
            remeshed = reply()
            assert "result" in remeshed, remeshed
            assert [part["mesh"] for part in remeshed["result"]["parts"]] == [
                part["mesh"] for part in responses[0]["result"]["parts"]]
            rail = recovered["result"]["parts"][0]
            refs = [dict(revision="bundle-r1", instance_id="rail", entity_id=f"face:{index}")
                    for index in range(rail["face_count"])]
            refs.append(dict(revision="bundle-r1", instance_id="rail", entity_id="edge:0"))
            child.stdin.write(json.dumps(dict(jsonrpc="2.0", id=3, protocol_version=1,
                                             worker_generation="recovery-test", revision="bundle-r1",
                                             method="inspect", params=dict(entities=refs, poses={"rail":dict(
                                                 position=[10,0,0], quaternion=[0,0,math.sqrt(0.5),math.sqrt(0.5)])}))) + "\n")
            child.stdin.flush()
            inspected = reply()
            assert "result" in inspected, inspected
            entities = inspected["result"]["entities"]
            assert any("mounting_holes" in entity["semantic_labels"] for entity in entities)
            cylinder = next(entity for entity in entities if entity.get("surface_type") == "CYLINDER")
            assert cylinder["anchor"]["kind"] == "geometry_center"
            assert cylinder["circular_definition"]["entity"] == cylinder["anchor"]["entity"]
            plane = next(entity for entity in entities if "normal" in entity)
            normal = plane["normal"]
            assert all(abs(actual-expected)<1e-10 for actual, expected in zip(
                plane["world_normal"], [-normal[1], normal[0], normal[2]]))
            assert entities[-1]["kind"] == "edge" and entities[-1]["length"] > 0
            child.stdin.close()
            assert child.wait(timeout=30) == 0
        finally:
            lease.close()
            if child.poll() is None:
                child.kill()
                child.wait()
        invalid = dict(jsonrpc="2.0", id=3, protocol_version=1, worker_generation="dependency-test",
                       revision="invalid-dependencies", method="build", params=dict(
                           source_root=str(source), expected_dependencies={"cadquery": "invalid"}))
        rejected = subprocess.run([str(executable), "--cache", str(directory / "invalid-cache"), "--generation", "dependency-test"],
                                  input=json.dumps(invalid)+"\n", capture_output=True, text=True, timeout=120,
                                  env={"PATH": "/usr/bin:/bin", "TMPDIR": temporary, "LANG": "en_US.UTF-8"})
        assert rejected.returncode == 0, rejected.stderr
        assert json.loads(rejected.stdout)["error"]["data"]["code"] == "UNSUPPORTED_OPERATION"
        print(json.dumps(dict(status="passed", executable=str(executable), parts=2,
                              checked_at=datetime.now(timezone.utc).isoformat(), platform=platform.platform(),
                              executable_sha256=hashlib.sha256(executable.read_bytes()).hexdigest(),
                              rejects_dependency_mismatch=True, stable_brep_bounds=True, saved_annotations=True, collision_proxy_coverage=True, exact_interference=True,
                              semantic_selection_properties=True, rotated_face_normals=True,
                              automatic_diameters=True, selected_geometry_anchors=True, published_and_saved_angles=True,
                              distinct_part_meshes=True, mesh_topology_mapping=True,
                              geometry_fingerprints=True, recovery_without_mesh_allocation=True,
                              stable_content_addressed_remesh=True, inherited_cache_lease=(os.name == "posix"),
                              export_bytes=exported.stat().st_size,
                              dependencies=responses[0]["result"]["dependencies"]), indent=2))


if __name__ == "__main__":
    main()
