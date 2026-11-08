import math
from pathlib import Path

import pytest

from cadpad_worker.annotations import angle_value, resolve
from cadpad_worker.model import Model, DomainError


def point(instance, coordinates):
    return dict(kind="local_point", instance_id=instance, point=coordinates)


def record(**changes):
    result = dict(id="included", label="Included angle", format=dict(unit="deg", precision=3),
                  definition=dict(kind="angle", start=point("left", [10, 0, 0]),
                                  vertex=point("left", [0, 0, 0]), end=point("right", [0, 0, 0])))
    result.update(changes)
    return result


def model_at(root: Path, text=None, records=None):
    source = root / "source"
    source.mkdir()
    (source / "model.py").write_text(text or "import cadquery as cq\nshow(cq.Workplane().box(2,2,2),name='left')\nshow(cq.Workplane().box(2,2,2),name='right',position=(0,20,0))")
    model = Model(root / "cache")
    result = model.build(dict(source_root=str(source), annotations=records or [], generate_mesh=False), "angles")
    return model, result


def test_angle_exact_world_anchors_and_display_independent_units(tmp_path):
    model, result = model_at(tmp_path, records=[record()])
    item = result["annotations"][0]
    assert item["status"] == "resolved"
    assert item["value"] == pytest.approx(math.pi / 2)
    assert item["unit"] == "rad" and item["format"]["unit"] == "deg"
    assert item["vertex"] == [0, 0, 0] and item["vertex_part"] == "left"
    radians = resolve(model, [record(format=dict(unit="rad", precision=6))])[0]
    assert radians["value"] == item["value"]
    assert radians["format"]["unit"] == "rad"


def test_broken_angle_discards_every_derived_anchor_and_value(tmp_path):
    model, _ = model_at(tmp_path)
    collapsed = record()
    collapsed["definition"]["start"] = point("left", [0, 0, 0])
    missing = record()
    missing["definition"]["vertex"]["instance_id"] = "missing"
    wrong_units = record(format=dict(unit="mm", precision=2))
    for item in resolve(model, [collapsed, missing, wrong_units]):
        assert item["status"] == "broken"
        assert item["error"]["code"] == "BROKEN_ANCHOR"
        assert not set(item).intersection(("start", "end", "vertex", "part", "end_part", "vertex_part", "value"))


def test_published_angle_context_and_snippet_bind_only_angular_parameters(tmp_path):
    text = """import cadquery as cq
def build(context):
    context.parameter('included', 90, unit='deg')
    context.show(cq.Workplane().box(2,2,2), name='left')
    context.angle('corner', 'left', (10,0,0), (0,0,0), (0,10,0), parameter='included')
"""
    model, result = model_at(tmp_path, text)
    assert result["annotations"][0]["value"] == math.pi / 2
    pinned = resolve(model, [dict(id="pin",label="Corner",definition=dict(kind="published",publication_id="corner"),format=dict(unit="rad",precision=3))])[0]
    assert pinned["vertex"] == [0, 0, 0] and pinned["value"] == math.pi / 2
    assert pinned["parameter"] == "included"
    snippet = tmp_path / "snippet"
    snippet.mkdir()
    _, result = model_at(snippet, "import cadquery as cq\nshow(cq.Workplane().box(2,2,2),name='left')\nangle('corner','left',(1,0,0),(0,0,0),(0,1,0))")
    assert result["annotations"][0]["value"] == math.pi / 2
    wrong = tmp_path / "wrong"
    wrong.mkdir()
    with pytest.raises(DomainError, match="requires a deg/rad"):
        model_at(wrong, text.replace("unit='deg'", "unit='mm'"))


def test_angle_numerics_distinguish_parallel_antiparallel_and_tiny_angles():
    assert angle_value([1,0,0], [0,0,0], [2,0,0]) == 0
    assert angle_value([1,0,0], [0,0,0], [-2,0,0]) == math.pi
    assert angle_value([1,0,0], [0,0,0], [1,1e-10,0]) == pytest.approx(1e-10, rel=1e-12, abs=0)
    with pytest.raises(DomainError):
        angle_value([0,0,0], [0,0,0], [1,0,0])
