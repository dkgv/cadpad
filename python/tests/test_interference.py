"""Optimized exact queries must agree with the original CAD-kernel operations."""
import sys
import math

import cadquery as cq
import pytest
from OCP.BRep import BRep_Builder
from OCP.TopAbs import TopAbs_VERTEX
from OCP.TopExp import TopExp_Explorer
from OCP.TopoDS import TopoDS

from cadpad_worker.model import DomainError, Model, Part


def model_with(tmp_path, shapes):
    model = Model(tmp_path / 'cache')
    model.revision = 'r1'
    model.parts = {name: Part(name, shape, (0, 0, 0), {}, None)
                   for name, shape in shapes.items()}
    return model


def query(model, pairs=None, poses=None):
    return model.inspect(dict(query='interference', pairs=pairs or [['a', 'b']], poses=poses or {}))


def original(a, b):
    overlap = a.intersect(b)
    assert overlap.isValid()
    solid = bool(overlap.Solids())
    return dict(distance_mm=a.distance(b), intersection_volume_mm3=overlap.Volume() if solid else 0.,
                interfering=solid)


@pytest.mark.parametrize('offset', [0, 5, 10, 10 + 1e-8, 10 + 1e-5, 30])
def test_exact_results_match_original_for_overlap_contact_and_separation(tmp_path, offset):
    a = cq.Workplane().box(10, 10, 10).val()
    b = cq.Workplane().box(10, 10, 10).val().translate((offset, 0, 0))
    result = query(model_with(tmp_path, dict(a=a, b=b)))['pairs'][0]
    expected = original(a, b)
    assert result['interfering'] == expected['interfering']
    assert result['distance_mm'] == pytest.approx(expected['distance_mm'], abs=1e-10)
    assert result['intersection_volume_mm3'] == pytest.approx(expected['intersection_volume_mm3'], abs=1e-8)


@pytest.mark.parametrize('kind', ['contained', 'hollow', 'compound', 'rotated', 'loose_tolerance'])
def test_complex_geometry_and_tolerances_match_original(tmp_path, kind):
    a = cq.Workplane().box(10, 10, 10).val()
    b = cq.Workplane().sphere(2).val()
    if kind == 'hollow':
        a = cq.Workplane().box(10, 10, 10).faces('>Z').workplane().hole(6).val()
    elif kind == 'compound':
        a = cq.Compound.makeCompound([a.translate((-15, 0, 0)), a.translate((15, 0, 0))])
        b = b.translate((15, 0, 0))
    elif kind == 'rotated':
        b = a.rotate((0, 0, 0), (1, 1, 1), 37).translate((8, 0, 0))
    elif kind == 'loose_tolerance':
        b = a.translate((10.001, 0, 0))
        explorer = TopExp_Explorer(b.wrapped, TopAbs_VERTEX)
        while explorer.More():
            BRep_Builder().UpdateVertex(TopoDS.Vertex_s(explorer.Current()), .01)
            explorer.Next()
    result = query(model_with(tmp_path, dict(a=a, b=b)))['pairs'][0]
    expected = original(a, b)
    assert result['interfering'] == expected['interfering']
    assert result['distance_mm'] == pytest.approx(expected['distance_mm'], abs=1e-10)
    assert result['intersection_volume_mm3'] == pytest.approx(expected['intersection_volume_mm3'], abs=1e-8)


def test_reuses_batch_placements_skips_booleans_and_caches_reversed_pairs(tmp_path):
    shape = cq.Workplane().box(10, 10, 10).val()
    model = model_with(tmp_path, {name: shape.translate((i * 30, 0, 0)) for i, name in enumerate('abc')})
    calls = dict(moved=0, intersect=0, distance=0, Solids=0)
    def profile(frame, event, argument):
        if (event == 'call' and frame.f_code.co_name in calls
                and isinstance(frame.f_locals.get('self'), cq.Shape)
                and frame.f_code.co_filename.endswith('cadquery/occ_impl/shapes.py')):
            calls[frame.f_code.co_name] += 1
    previous = sys.getprofile()
    sys.setprofile(profile)
    try:
        first = query(model, [['a', 'b'], ['a', 'c'], ['b', 'c']])
        # Returned dictionaries are isolated from the scalar cache.
        first['pairs'][0]['distance_mm'] = -999
        second = query(model, [['b', 'a'], ['c', 'a'], ['c', 'b']])
    finally:
        sys.setprofile(previous)
    assert calls == dict(moved=3, intersect=0, distance=3, Solids=3)
    assert second['pairs'][0]['instances'] == ['b', 'a']
    assert second['pairs'][0]['distance_mm'] == pytest.approx(20)


def test_cache_respects_pose_geometry_revision_and_capacity(tmp_path):
    shape = cq.Workplane().box(10, 10, 10).val()
    model = model_with(tmp_path, dict(a=shape, b=shape.translate((30, 0, 0))))
    assert query(model)['pairs'][0]['distance_mm'] == pytest.approx(20)
    pose = dict(position=[-25, 0, 0], quaternion=[0, 0, 0, 1])
    assert query(model, poses={'b': pose})['pairs'][0]['interfering']
    pose['position'][0] = 30
    assert query(model, poses={'b': pose})['pairs'][0]['distance_mm'] == pytest.approx(50)
    with pytest.raises(DomainError):
        query(model, poses={'b': dict(position=[0, 0, 0], quaternion=[0, 0, 0, 2])})
    model.parts['b'].shape = shape.translate((40, 0, 0))
    assert query(model)['pairs'][0]['distance_mm'] == pytest.approx(30)
    model.revision = 'r2'
    assert query(model)['revision'] == 'r2'
    for x in range(260):
        query(model, poses={'b': dict(position=[x, 0, 0])})
    assert len(model._interference_results) == 256
    assert query(model)['pairs'][0]['distance_mm'] == pytest.approx(30)


def test_non_solid_geometry_still_rejected(tmp_path):
    model = model_with(tmp_path, dict(a=cq.Workplane().rect(10, 10).val(),
                                     b=cq.Workplane().box(10, 10, 10).val()))
    with pytest.raises(DomainError, match='solid geometry'):
        query(model)


def test_rotation_pose_changes_are_measured_and_not_reused(tmp_path):
    shape = cq.Workplane().box(10, 4, 6).val()
    model = model_with(tmp_path, dict(a=shape, b=shape))
    for angle in [90, 180, 90]:
        radians = math.radians(angle)
        pose = dict(position=[6, 0, 0], quaternion=[0, 0, math.sin(radians / 2), math.cos(radians / 2)])
        result = query(model, poses={'b': pose})['pairs'][0]
        expected = original(shape, shape.moved(model.location(pose)))
        assert result['distance_mm'] == pytest.approx(expected['distance_mm'], abs=1e-10)
        assert result['intersection_volume_mm3'] == pytest.approx(expected['intersection_volume_mm3'])
        assert result['interfering'] == expected['interfering']
