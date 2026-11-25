import cadquery as cq
import pytest

from cadpad_worker.errors import DomainError
from cadpad_worker.model import Model
from cadpad_worker.step_import import _unique_name


def build(root, source):
    source_dir = root / "source"
    source_dir.mkdir(exist_ok=True)
    (source_dir / "model.py").write_text(source)
    return Model(root / "cache").build(dict(source_root=str(source_dir), generate_mesh=False), "r1")


def test_fastener_metadata_is_validated_and_published(tmp_path):
    result = build(tmp_path, '''import cadquery as cq
bolt = cq.Workplane().cylinder(20, 4)
show(bolt, name="bolt", fastener=dict(kind="bolt", size="M8", length=20, mates=["plate"]))
show(cq.Workplane().box(30, 30, 4), name="plate")
''')
    part = next(part for part in result["parts"] if part["name"] == "bolt")
    assert part["fastener"] == dict(kind="bolt", size="M8", length=20.0, mates=["plate"])


@pytest.mark.parametrize("metadata", [
    'dict(kind="", size="M8")',
    'dict(kind="bolt", size=" ")',
    'dict(kind="bolt", size="M8", length=0)',
    'dict(kind="bolt", size="M8", length=float("nan"))',
    'dict(kind="bolt", size="M8", mates="plate")',
    'dict(kind="bolt", size="M8", mates=["plate", " plate "])',
    'dict(kind="bolt", size="M8", unknown=1)',
])
def test_invalid_fastener_metadata_is_rejected(tmp_path, metadata):
    with pytest.raises(DomainError, match="fastener"):
        build(tmp_path, f'import cadquery as cq\nshow(cq.Workplane().box(1,1,1), name="bolt", fastener={metadata})\n')


def test_named_step_import_preserves_component_names_and_instances(tmp_path):
    assets = tmp_path / "source" / "assets"
    assets.mkdir(parents=True)
    assembly = cq.Assembly(name="device")
    assembly.add(cq.Workplane().box(4, 4, 4), name="board")
    assembly.add(cq.Workplane().box(1, 1, 1), name="J1", loc=cq.Location((8, 0, 0)))
    assembly.add(cq.Workplane().box(1, 1, 1), name="J1_b", loc=cq.Location((12, 0, 0)))
    subassembly = cq.Assembly(name="connectors")
    subassembly.add(cq.Workplane().box(2, 2, 2), name="J2", loc=cq.Location((20, 0, 0)))
    assembly.add(subassembly, name="connectors", loc=cq.Location((0, 10, 0)))
    assembly.save(str(assets / "device.step"), exportType="STEP")
    (tmp_path / "source" / "model.py").write_text('show(import_step_named("device.step", name="device"), name="device")\n')
    result = Model(tmp_path / "cache").build(dict(source_root=str(tmp_path / "source"), generate_mesh=False), "r1")
    names = {part["name"] for part in result["parts"]}
    assert {"device/board", "device/J1", "device/J1_b"} <= names
    assert any(name.endswith("/J2") and len(name.split("/")) == 3 for name in names)
    by_name = {part["name"]: part for part in result["parts"]}
    assert by_name["device/J1"]["bounds"] != by_name["device/J1_b"]["bounds"]
    nested = next(part for name, part in by_name.items() if name.endswith("/J2"))
    assert nested["bounds"]["min"][0] >= 19 and nested["bounds"]["min"][1] >= 9


def test_missing_and_repeated_step_names_receive_stable_unique_fallbacks():
    used = set()
    assert [_unique_name(name, used) for name in ("", "J1", "J1", "")] == ["part", "J1", "J1_2", "part_2"]
