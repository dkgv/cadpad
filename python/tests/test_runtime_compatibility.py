"""Projects written before VTK removal remain usable on the headless runtime."""
import struct
import hashlib

import cadquery as cq
import pytest

from cadpad_worker.model import Model, DomainError

LEGACY = {"cadquery": "2.6.1", "cadquery-ocp": "7.8.1.1.post1", "ocp-tessellate": "3.0.17"}
CURRENT = {"cadquery": "2.6.1+cadpad.1", "cadquery-ocp-novtk": "7.8.1.1.post1", "ocp-tessellate": "3.0.17"}


def source(tmp_path):
    root = tmp_path / "source"
    root.mkdir()
    (root / "model.py").write_text("import cadquery as cq\nshow(cq.Workplane().box(1, 2, 3), name='box')\n")
    return root


@pytest.mark.parametrize("expected", [LEGACY, CURRENT])
def test_existing_project_builds_and_exports(tmp_path, expected):
    model = Model(tmp_path / "cache")
    result = model.build({"source_root": str(source(tmp_path)), "expected_dependencies": expected}, "compat")
    assert result["dependencies"] == CURRENT
    for format in ("STEP", "STL"):
        exported = model.export({"format": format})
        data = (model.cache / exported["handle"]).read_bytes()
        if format == "STEP":
            assert data.startswith(b"ISO-10303-21;")
        else:
            count, = struct.unpack_from("<I", data, 80)
            assert count > 0 and len(data) == 84 + count * 50


def test_named_step_import_records_asset_path_and_hash_on_published_occurrences(tmp_path):
    root = source(tmp_path)
    assets = root / "assets"
    assets.mkdir()
    step_path = assets / "housing.step"
    cq.exporters.export(cq.Workplane().box(2, 3, 4), str(step_path), exportType="STEP")
    (root / "model.py").write_text(
        "show(import_step_named('housing.step', name='Housing'), name='Housing')\n"
    )

    result = Model(tmp_path / "cache").build(
        {"source_root": str(root), "expected_dependencies": CURRENT}, "step-source"
    )

    assert result["parts"]
    assert all(
        part["source_assets"] == [{
            "path": "assets/housing.step",
            "sha256": hashlib.sha256(step_path.read_bytes()).hexdigest(),
        }]
        for part in result["parts"]
    )


@pytest.mark.parametrize("expected", [
    {**LEGACY, "cadquery": "2.5.0"},
    {**LEGACY, "cadquery-ocp": "7.8.1.0"},
    {**LEGACY, "ocp-tessellate": "3.0.16"},
    {**CURRENT, "cadquery": "2.6.1+cadpad.2"},
    {**LEGACY, "extra": "1.0"},
    {"cadquery": "2.6.1"},
])
def test_other_dependency_changes_still_rejected(tmp_path, expected):
    model = Model(tmp_path / "cache")
    with pytest.raises(DomainError, match="Project dependencies differ"):
        model.build({"source_root": str(source(tmp_path)), "expected_dependencies": expected}, "incompatible")
