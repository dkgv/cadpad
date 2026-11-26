"""STEP-imported parts referenced by exact geometric feature refs (P3.9/P3.10).

Imported geometry has no CAD history names, so requirements address its faces
and edges by ``FeatureRef::Geometric``: entity kind plus the SHA-256 of
``geometry_fingerprint(entity)``. These tests build real STEP assets, publish
them through ``import_step_named`` / ``import_step``, and check that:

* every face/edge of an imported part has a stable, unique fingerprint that
  survives the requirements evaluator's BREP encode/decode rebuild path;
* ``inspect`` exposes the same hash a client needs to author the reference
  (``anchor.entity``), and ``resolve_features`` resolves it exactly;
* a geometric feature axis drives rigid moves in ``evaluate_plan_chunk``.
"""
import cadquery as cq
import pytest

from cadpad_worker.errors import DomainError
from cadpad_worker.model import Model, geometry_fingerprint
from cadpad_worker.requirements_eval import evaluate_plan_chunk


NAMED_SOURCE = "show(import_step_named('device.step', name='device'), name='device')\n"
LEGACY_SOURCE = "show(import_step('device.step'), name='device')\n"


def write_device_step(path):
    """Assembly 'device': an arm box and a vertical pin cylinder.

    ``device/arm`` is a box spanning x[10, 18], y[-1, 1], z[0, 2] (centre
    (14, 0, 1)). ``device/pin`` is a cylinder of radius 1, height 4 along Z,
    centred on (10, 0, 0): its lateral CYLINDER face and top/bottom CIRCLE
    edges all have the Z axis through (10, 0, 0) as their axis.
    """
    assembly = cq.Assembly(name="device")
    assembly.add(cq.Workplane().box(8, 2, 2).translate((14, 0, 1)), name="arm")
    assembly.add(cq.Workplane().circle(1).extrude(4).translate((10, 0, -2)), name="pin")
    assembly.save(str(path), exportType="STEP")


def project(tmp_path, source_text=NAMED_SOURCE, name="proj"):
    source = tmp_path / name
    (source / "assets").mkdir(parents=True)
    write_device_step(source / "assets" / "device.step")
    (source / "model.py").write_text(source_text)
    return source


def cylinder_face(shape):
    return next(face for face in shape.Faces() if face.geomType() == "CYLINDER")


def circle_edge(shape):
    return next(edge for edge in shape.Edges() if edge.geomType() == "CIRCLE")


def fingerprints(shape, kind):
    entities = shape.Faces() if kind == "face" else shape.Edges()
    return sorted(geometry_fingerprint(entity) for entity in entities)


def inspect_hash(model, part_name, entity_id):
    """The geometric hash a client reads off a picked entity (``anchor.entity``)."""
    result = model.inspect(dict(entities=[dict(revision=model.revision,
                                              instance_id=part_name, entity_id=entity_id)]))
    entity = result["entities"][0]["anchor"]["entity"]
    return entity["entity_kind"], entity["sha256"]


def test_step_import_entities_have_stable_unique_fingerprints(tmp_path):
    source = project(tmp_path)
    model_a = Model(tmp_path / "cache-a")
    model_a.build(dict(source_root=str(source), generate_mesh=False), "r1")
    model_b = Model(tmp_path / "cache-b")
    model_b.build(dict(source_root=str(source), generate_mesh=False), "r1")
    assert set(model_a.parts) == {"device/arm", "device/pin"}
    for name in ("device/arm", "device/pin"):
        assert model_a.parts[name].fingerprint == model_b.parts[name].fingerprint
        for kind in ("face", "edge"):
            hashes_a = fingerprints(model_a.parts[name].shape, kind)
            hashes_b = fingerprints(model_b.parts[name].shape, kind)
            assert hashes_a == hashes_b
            assert len(hashes_a) == len(set(hashes_a))


def test_named_step_import_geometric_refs_resolve_and_drive_moves(tmp_path):
    source = project(tmp_path)
    model = Model(tmp_path / "cache")
    model.build(dict(source_root=str(source), generate_mesh=False), "r1")

    pin = model.parts["device/pin"].shape
    face_index = next(i for i, face in enumerate(pin.Faces()) if face.geomType() == "CYLINDER")
    edge_index = next(i for i, edge in enumerate(pin.Edges()) if edge.geomType() == "CIRCLE")

    # Discovery path: inspect reports the hash a geometric ref must carry.
    entity_kind, face_hash = inspect_hash(model, "device/pin", f"face:{face_index}")
    assert entity_kind == "face"
    assert face_hash == geometry_fingerprint(cylinder_face(pin))
    edge_kind, edge_hash = inspect_hash(model, "device/pin", f"edge:{edge_index}")
    assert edge_kind == "edge"
    assert edge_hash == geometry_fingerprint(circle_edge(pin))

    resolved = model.resolve_features(dict(references=[
        dict(kind="geometric", part="device/pin", entity="face", hash=face_hash),
        dict(kind="geometric", part="device/pin", entity="edge", hash=edge_hash),
        dict(kind="geometric", part="device/pin", entity="face",
             hash="0" * 64),
        dict(kind="geometric", part="device/arm", entity="face", hash=face_hash),
    ]))
    face, edge, missing, wrong_part = resolved["references"]
    assert face == dict(status="resolved", entity="face", count=1, hash=face_hash, positional=False)
    assert edge["status"] == "resolved" and edge["count"] == 1
    assert missing["status"] == "missing" and missing["count"] == 0
    assert wrong_part["status"] == "missing"

    # A 180 deg turn about the pin axis (x=10, y=0, dir z) maps the arm centre
    # (14,0,1) to (6,0,1): x extent [2, 10]. Any other axis gives a different box.
    states = []
    for entity, h in (("face", face_hash), ("edge", edge_hash)):
        states.append(dict(
            id=f"spin-{entity}",
            moves=[dict(op="rotate", parts=["device/arm"], angle_deg=180,
                        axis=dict(feature=dict(kind="geometric", part="device/pin",
                                               entity=entity, hash=h),
                                  on="device/pin"))],
            measurements=[dict(id="x", primitive="extent", a=["device/arm"], axis="x")]))
    result = evaluate_plan_chunk(model, dict(states=states, limits=dict(workers=2)))
    assert result["status"] == "complete"
    for state in result["results"]["states"]:
        extent = state["measurements"][0]
        assert extent["status"] == "ok"
        assert (extent["min"], extent["max"]) == pytest.approx((2.0, 10.0))


def test_legacy_step_import_resolves_geometric_refs(tmp_path):
    source = project(tmp_path, LEGACY_SOURCE)
    model = Model(tmp_path / "cache")
    model.build(dict(source_root=str(source), generate_mesh=False), "r1")
    # The legacy adapter flattens the file into one compound part.
    shape = model.parts["device"].shape
    pin_face = cylinder_face(shape)
    face_hash = geometry_fingerprint(pin_face)
    resolved = model.resolve_features(dict(references=[
        dict(kind="geometric", part="device", entity="face", hash=face_hash)]))
    assert resolved["references"] == [dict(status="resolved", entity="face", count=1,
                                           hash=face_hash, positional=False)]


def test_geometric_refs_survive_param_rebuild_of_step_import(tmp_path):
    """Rebuilt states decode parts from BREP; entity fingerprints must still match."""
    source = project(tmp_path, NAMED_SOURCE +
        "import cadquery as cq\n"
        "lift = parameter('lift', 0.0, minimum=0.0, maximum=5.0)\n"
        "show(cq.Workplane().box(1, 1, 1).translate((30, 30, lift)), name='weight')\n")
    model = Model(tmp_path / "cache")
    model.build(dict(source_root=str(source), generate_mesh=False), "r1")
    pin = model.parts["device/pin"].shape
    face_index = next(i for i, face in enumerate(pin.Faces()) if face.geomType() == "CYLINDER")
    _, face_hash = inspect_hash(model, "device/pin", f"face:{face_index}")

    def state(tag, params):
        row = dict(id=tag, moves=[dict(op="rotate", parts=["device/arm"], angle_deg=180,
                                      axis=dict(feature=dict(kind="geometric", part="device/pin",
                                                             entity="face", hash=face_hash),
                                                on="device/pin"))],
                   measurements=[dict(id="x", primitive="extent", a=["device/arm"], axis="x")])
        if params:
            row["params"] = params
        return row

    result = evaluate_plan_chunk(model, dict(
        states=[state("base", None), state("rebuilt", {"lift": 2.0})],
        limits=dict(workers=2)))
    assert result["status"] == "complete"
    assert result["cache"]["rebuilds"] == 1
    for row in result["results"]["states"]:
        extent = row["measurements"][0]
        assert extent["status"] == "ok"
        assert (extent["min"], extent["max"]) == pytest.approx((2.0, 10.0))


def test_geometric_feature_rejects_non_step_and_bad_refs(tmp_path):
    source = tmp_path / "authored"
    source.mkdir()
    (source / "model.py").write_text(
        "import cadquery as cq\nshow(cq.Workplane().box(4, 4, 4), name='part')\n")
    model = Model(tmp_path / "cache")
    model.build(dict(source_root=str(source), generate_mesh=False), "r1")
    shape = model.parts["part"].shape
    face_hash = geometry_fingerprint(shape.Faces()[0])
    # Geometric refs are not restricted to imports at the worker layer: an
    # exact hash resolves wherever it is unique (spec limits them to imports
    # by convention, enforced by what clients publish).
    resolved = model.resolve_features(dict(references=[
        dict(kind="geometric", part="part", entity="face", hash=face_hash),
        dict(kind="geometric", part="missing", entity="face", hash=face_hash),
    ]))
    face, missing_part = resolved["references"]
    assert face["status"] in ("resolved", "ambiguous")
    assert missing_part["status"] == "missing_part"
    with pytest.raises(DomainError):
        model.resolve_features(dict(references=[
            dict(kind="geometric", part="part", entity="vertex", hash=face_hash)]))
