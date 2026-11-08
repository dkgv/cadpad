"""Analytic mesh checks; no browser, rendering or physical-printing claim."""
from collections import Counter
import math
import struct

import pytest

from cadpad_worker.model import Model, geometry_fingerprint
import cadquery as cq
from cadpad_worker import stl


def subtract(a, b):
    return tuple(x-y for x, y in zip(a, b))


def cross(a, b):
    return (a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0])


def dot(a, b):
    return sum(x*y for x, y in zip(a, b))


def length(a):
    return math.sqrt(dot(a, a))


def triangles(data):
    count, = struct.unpack_from("<I", data, 80)
    assert 0 < count < 500_000
    assert len(data) == 84 + 50*count
    for offset in range(84, len(data), 50):
        values = struct.unpack_from("<12fH", data, offset)
        assert all(math.isfinite(value) for value in values)
        yield values[:3], (values[3:6], values[6:9], values[9:12])


@pytest.mark.parametrize("kind,radius,minor", [
    ("sphere", 0.5, 0), ("sphere", 20, 0), ("sphere", 200, 0), ("torus", 20, 3), ("hollow", 20, 15),
])
def test_curved_stl_is_closed_outward_posed_and_has_absolute_mesh_accuracy(tmp_path, kind, radius, minor):
    source = tmp_path / "source"
    source.mkdir()
    shape = f"cq.Workplane().sphere({radius})" if kind == "sphere" else f"cq.Solid.makeTorus({radius},{minor})" if kind == "torus" else f"cq.Workplane().sphere({radius}).cut(cq.Workplane().sphere({minor}))"
    (source / "model.py").write_text(f"import cadquery as cq\nshow({shape}, name='part')")
    model = Model(tmp_path / "cache")
    publication = model.build({"source_root": str(source)}, "stl-reference")
    part = model.parts["part"]
    before = geometry_fingerprint(part.shape)
    # Deliberately coarse cached triangulation must not set export quality.
    part.shape.mesh(5, 0.8)
    translation = (37., -19., 11.)
    exported = model.export({"format": "STL", "poses": {"part": {
        "position": translation, "quaternion": [math.sqrt(0.5), 0, 0, math.sqrt(0.5)],
    }}, "pose_version": 7})
    assert exported["pose_version"] == 7 and exported["units"] == "mm"
    edges = Counter()
    volume = 0.
    maximum_error = 0.
    for normal, vertices in triangles((model.cache / exported["handle"]).read_bytes()):
        geometric_normal = cross(subtract(vertices[1], vertices[0]), subtract(vertices[2], vertices[0]))
        assert length(geometric_normal) > 1e-15
        assert dot(normal, geometric_normal) / length(geometric_normal) > 0.9999
        assert length(normal) == pytest.approx(1., abs=1e-6)
        local = []
        for vertex in vertices:
            p = subtract(vertex, translation)
            # Inverse of the declared 90-degree X rotation.
            local.append((p[0], p[2], -p[1]))
        center = tuple(sum(p[i] for p in local)/3 for i in range(3))
        error = abs(length(center)-radius) if kind == "sphere" else abs(math.hypot(math.hypot(center[0], center[1])-radius, center[2])-minor) if kind == "torus" else min(abs(length(center)-radius), abs(length(center)-minor))
        maximum_error = max(maximum_error, error)
        volume += dot(local[0], cross(local[1], local[2]))/6
        welded = [tuple(round(x, 6) for x in vertex) for vertex in vertices]
        for a, b in zip(welded, welded[1:]+welded[:1]):
            edges[(a, b)] += 1
    assert all(count == 1 and edges[(b, a)] == 1 for (a, b), count in edges.items())
    expected_volume = 4/3*math.pi*radius**3 if kind == "sphere" else 2*math.pi**2*radius*minor**2 if kind == "torus" else 4/3*math.pi*(radius**3-minor**3)
    assert volume == pytest.approx(expected_volume, rel=0.005)
    assert maximum_error <= 0.06, f"{kind}, radius {radius}: {maximum_error} mm chord error"
    assert geometry_fingerprint(part.shape) == before
    assert model.remesh()["parts"][0]["geometry_fingerprint"] == publication["parts"][0]["geometry_fingerprint"]


@pytest.mark.parametrize("payload", [
    b"short",
    b"\0"*80 + struct.pack("<I", 0),
    b"\0"*80 + struct.pack("<I", 1),
    b"\0"*80 + struct.pack("<I", 1) + struct.pack("<12fH", *([0.]*12), 0),
    b"\0"*80 + struct.pack("<I", 1) + struct.pack("<12fH", 0, 0, 1, math.nan, 0, 0, 1, 0, 0, 0, 1, 0, 0),
])
def test_incomplete_empty_collapsed_or_nonfinite_kernel_mesh_is_not_published(tmp_path, monkeypatch, payload):
    def broken_writer(self, filename, **kwargs):
        from pathlib import Path
        Path(filename).write_bytes(payload)
        return True
    monkeypatch.setattr(cq.Shape, "exportStl", broken_writer)
    path = tmp_path / "export.stl"
    with pytest.raises(ValueError):
        stl.export(cq.Workplane().box(1, 1, 1).val(), path)
    assert list(tmp_path.iterdir()) == []


def test_kernel_write_failure_leaves_no_partial_artifact(tmp_path, monkeypatch):
    def failed_writer(self, filename, **kwargs):
        from pathlib import Path
        Path(filename).write_bytes(b"partial")
        return False
    monkeypatch.setattr(cq.Shape, "exportStl", failed_writer)
    with pytest.raises(ValueError, match="could not write"):
        stl.export(cq.Workplane().box(1, 1, 1).val(), tmp_path / "export.stl")
    assert list(tmp_path.iterdir()) == []


@pytest.mark.parametrize("dimensions,translation,rotation", [
    ((1000, .02, .03), (0, 0, 0), 0),
    ((1000, .02, .03), (2000, -3000, 100), 17),
    ((32, 16, 8), (100_000_000, 0, 0), 0),
])
@pytest.mark.parametrize("reimported", [False, True])
def test_thin_and_exactly_representable_distant_solids_remain_closed_and_posed(tmp_path, dimensions, translation, rotation, reimported):
    shape = cq.Workplane().box(*dimensions).val().rotate((0, 0, 0), (0, 0, 1), rotation).translate(translation)
    if reimported:
        step = tmp_path / "input.step"
        cq.exporters.export(shape, str(step), exportType="STEP")
        shape = cq.importers.importStep(str(step)).val()
    original = geometry_fingerprint(shape)
    path = tmp_path / "thin.stl"
    stl.export(shape, path)
    edges = Counter()
    volume = 0.
    for normal, vertices in triangles(path.read_bytes()):
        n = cross(subtract(vertices[1], vertices[0]), subtract(vertices[2], vertices[0]))
        assert length(n) > 0
        assert dot(normal, n)/length(n) > .9999
        local = [subtract(point, translation) for point in vertices]
        volume += dot(local[0], cross(local[1], local[2]))/6
        for a, b in zip(vertices, vertices[1:]+vertices[:1]):
            edges[(a, b)] += 1
    assert all(count == 1 and edges[(b, a)] == 1 for (a, b), count in edges.items())
    assert volume == pytest.approx(math.prod(dimensions), rel=.02)
    assert geometry_fingerprint(shape) == original


@pytest.mark.parametrize("size,offset,reason", [
    (.01, 1_000_000, "collapses or reverses"),
    (10, 100_000_000, "rounding exceeds"),
    (10, 1_000_000_000, "rounding exceeds"),
])
def test_float32_surface_loss_and_excess_rounding_fail_without_partial_files(tmp_path, size, offset, reason):
    shape = cq.Workplane().box(size, size, size).translate((offset, 0, 0)).val()
    fingerprint = geometry_fingerprint(shape)
    path = tmp_path / "unrepresentable.stl"
    with pytest.raises(ValueError, match=reason):
        stl.export(shape, path)
    assert not list(tmp_path.iterdir())
    assert geometry_fingerprint(shape) == fingerprint
