import json
from pathlib import Path
import subprocess
import sys

import pytest


def request(method, params, revision="r1", **overrides):
    return dict(jsonrpc="2.0", id=method, protocol_version=1,
                worker_generation="test", revision=revision, method=method,
                params=params) | overrides


def worker(tmp_path, requests, *, forbid_geometry=False):
    command = [sys.executable]
    if forbid_geometry:
        # Run the real protocol with native imports unavailable. A timing
        # threshold would be flaky and wouldn't prove analysis stays AST-only.
        command += ["-c", """
import runpy, sys
class NoGeometry:
    def find_spec(self, fullname, path=None, target=None):
        if fullname.split('.')[0] in ('cadquery', 'OCP', 'vtk', 'vtkmodules'):
            raise AssertionError('Source analysis imported ' + fullname)
sys.meta_path.insert(0, NoGeometry())
runpy.run_module('cadpad_worker', run_name='__main__')
"""]
    else:
        command += ["-m", "cadpad_worker"]
    result = subprocess.run(
        [*command, "--cache", str(tmp_path / "cache"), "--generation", "test"],
        input="".join(json.dumps(value) + "\n" for value in requests),
        capture_output=True, text=True, timeout=30,
        cwd=Path(__file__).resolve().parents[2], check=True,
    )
    return [json.loads(line) for line in result.stdout.splitlines()]


def test_source_analysis_and_protocol_errors_need_no_native_geometry(tmp_path):
    source = "import cadquery as cq\nraise RuntimeError('never execute')\ndef build(): pass"
    responses = worker(tmp_path, [
        request("analyze_entry", dict(entry="model.py", source=source)),
        request("index_sources", dict(sources={"model.py": source})),
        request("inspect", dict(entities=[])),
        request("analyze_entry", {}, worker_generation="wrong"),
        request("analyze_entry", {}, protocol_version=0),
    ], forbid_geometry=True)
    assert responses[0]["result"]["recommended"] == "build"
    assert responses[1]["result"]["files"][0]["path"] == "model.py"
    assert [r["error"]["data"]["code"] for r in responses[2:]] == [
        "STALE_ENTITY", "WORKER_UNAVAILABLE", "INVALID_REQUEST",
    ]


def test_analysis_then_build_and_inspection_share_the_same_worker(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    text = "import cadquery as cq\nshow(cq.Workplane().box(2,3,4), name='box')"
    (source / "model.py").write_text(text)
    responses = worker(tmp_path, [
        request("analyze_entry", dict(entry="model.py", source=text), revision="analysis"),
        request("build", dict(source_root=str(source))),
        request("index_sources", dict(sources={"model.py": text}), revision="analysis"),
        request("inspect", dict(entities=[dict(
            revision="r1", instance_id="box", entity_id="part:0")])),
        request("inspect", dict(entities=[]), revision="stale"),
    ])
    assert all("result" in r for r in responses[:4]), responses
    assert responses[1]["result"]["parts"][0]["instance_id"] == "box"
    assert responses[3]["result"]["entities"][0]["volume"] == pytest.approx(24)
    assert responses[4]["error"]["data"]["code"] == "STALE_ENTITY"
