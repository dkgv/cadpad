"""STL reference fit checks for reconstruction."""
import math
from pathlib import Path

import cadquery as cq
import pytest

from cadpad_worker.model import DomainError, Model
from cadpad_worker import stl


def _project(tmp_path, source, assets=None):
    root = tmp_path / "source"
    root.mkdir()
    (root / "model.py").write_text(source)
    if assets:
        for name, path in assets.items():
            target = root / "assets" / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(Path(path).read_bytes() if not isinstance(path, bytes) else path)
    return root


def _box_stl(tmp_path, size=(10, 20, 30), name="box.stl"):
    path = tmp_path / name
    stl.export(cq.Workplane().box(*size).val(), path)
    return path


def test_matching_reconstruction_passes_strict_fit(tmp_path):
    reference = _box_stl(tmp_path)
    source = f'''
import cadquery as cq
show(cq.Workplane().box(10, 20, 30), name="box", kind="solid")
fit_part("box", reference_stl="box.stl", strictness="strict", on_fail="error",
         tolerances=dict(bbox_mm=0.5, volume_rel=0.02, hausdorff_mm=0.5, section_mm=0.5))
'''
    root = _project(tmp_path, source, {"box.stl": reference})
    result = Model(tmp_path / "cache").build(dict(source_root=str(root)), "fit-ok")
    assert result["fit_results"][0]["status"] == "passed"
    assert result["fit_results"][0]["passed"] is True
    assert {item["name"] for item in result["fit_results"][0]["checks"]} >= {
        "bbox", "volume", "probes", "sections", "hausdorff"
    }


def test_mismatched_solid_fails_build_with_fit_details(tmp_path):
    reference = _box_stl(tmp_path)
    source = '''
import cadquery as cq
show(cq.Workplane().box(8, 20, 30), name="box", kind="solid")
fit_part("box", reference_stl="box.stl", strictness="always", on_fail="error")
'''
    root = _project(tmp_path, source, {"box.stl": reference})
    with pytest.raises(DomainError) as raised:
        Model(tmp_path / "cache").build(dict(source_root=str(root)), "fit-bad")
    assert raised.value.code == "FIT_FAILED"
    assert raised.value.details["fit_results"][0]["status"] == "failed"
    assert any(not item["passed"] for item in raised.value.details["fit_results"][0]["checks"])


def test_report_mode_keeps_build_when_part_is_missing(tmp_path):
    reference = _box_stl(tmp_path)
    source = '''
import cadquery as cq
show(import_stl("box.stl"), name="stl_ref", kind="surface")
fit_part("part_pending", reference_stl="box.stl", strictness="strict", on_fail="report")
'''
    root = _project(tmp_path, source, {"box.stl": reference})
    result = Model(tmp_path / "cache").build(dict(source_root=str(root)), "fit-report")
    assert len(result["parts"]) == 1
    assert result["parts"][0]["name"] == "stl_ref"
    assert result["fit_results"][0]["status"] == "awaiting_part"
    assert result["fit_results"][0]["passed"] is False


def test_probe_and_volume_catch_feature_errors(tmp_path):
    reference = _box_stl(tmp_path, size=(20, 20, 6))
    source = '''
import cadquery as cq
show(cq.Workplane().box(20, 20, 6).faces(">Z").workplane().hole(8), name="plate", kind="solid")
fit_part("plate", reference_stl="plate.stl", checks=("volume", "probes"), on_fail="error",
         probes=[dict(id="center", point=(0, 0, 3), expect="inside")])
'''
    root = _project(tmp_path, source, {"plate.stl": reference})
    with pytest.raises(DomainError) as raised:
        Model(tmp_path / "cache").build(dict(source_root=str(root)), "fit-hole")
    assert raised.value.code == "FIT_FAILED"
    volume = next(item for item in raised.value.details["fit_results"][0]["checks"] if item["name"] == "volume")
    assert volume["passed"] is False


def test_stl_mesh_volume_matches_box(tmp_path):
    from cadpad_worker import stl_mesh
    path = _box_stl(tmp_path, size=(10, 4, 2))
    mesh = stl_mesh.load(path)
    assert abs(stl_mesh.signed_volume(mesh)) == pytest.approx(80, rel=1e-3)
    assert stl_mesh.bounds(mesh)["size"] == pytest.approx([10, 4, 2], abs=1e-3)
