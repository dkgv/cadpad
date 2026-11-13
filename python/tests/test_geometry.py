import json
import math
from pathlib import Path
import struct
import subprocess

import cadquery as cq
import pytest

from cadpad_worker.model import DomainError, Model

ROOT = Path(__file__).resolve().parents[2]


def build(tmp_path, source, parameters=None):
    source_root = tmp_path / "source"
    source_root.mkdir(exist_ok=True)
    (source_root / "model.py").write_text(source)
    model = Model(tmp_path / "cache")
    result = model.build(dict(source_root=str(source_root), parameters=parameters or {}), "r1")
    return model, result


def test_analytic_properties_topology_and_binary_artifacts(tmp_path):
    model, result = build(tmp_path, 'import cadquery as cq\nshow(cq.Workplane().box(80,40,6).faces(">Z").workplane().hole(8), name="plate")')
    part = result["parts"][0]
    assert part["bounds"]["size"] == pytest.approx([80, 40, 6])
    assert part["volume"] == pytest.approx(80 * 40 * 6 - math.pi * 4**2 * 6)
    refs = [dict(revision="r1", instance_id="plate", entity_id=f"face:{i}") for i in range(part["face_count"])]
    inspected = model.inspect(dict(entities=refs))
    cylinder = next(e for e in inspected["entities"] if e["surface_type"] == "CYLINDER")
    assert cylinder["radius"] == pytest.approx(4)
    buffers = part["mesh"]["buffers"]
    counts_data = (model.cache / buffers["triangles_per_face"]["handle"]).read_bytes()
    counts = struct.unpack(f"<{len(counts_data)//4}I", counts_data)
    assert len(counts) == part["face_count"]
    assert sum(counts) * 3 * 4 == buffers["triangles"]["bytes"]
    assert "path" not in json.dumps(buffers)
    with pytest.raises(DomainError, match="different revision"):
        model.inspect(dict(entities=[dict(refs[0], revision="old")]))


def test_pose_distance_and_step_roundtrip(tmp_path):
    model, result = build(tmp_path, 'import cadquery as cq\nshow(cq.Workplane().box(10,20,30),name="a")\nshow(cq.Workplane().box(10,20,30),name="b",position=(30,0,0))')
    refs = [dict(revision="r1", instance_id=name, entity_id="part:0") for name in ("a", "b")]
    assert model.inspect(dict(query="distance", entities=refs))["distance"] == pytest.approx(20)
    poses = {"b": dict(position=[50, 0, 0], quaternion=[0, 0, 0, 1])}
    assert model.inspect(dict(query="distance", entities=refs, poses=poses))["distance"] == pytest.approx(40)
    exported = model.export(dict(format="STEP", poses=poses, pose_version=7))
    imported = cq.importers.importStep(str(model.cache / exported["handle"])).val()
    assert imported.BoundingBox().xlen == pytest.approx(60)
    assert imported.Volume() == pytest.approx(12000)
    assert exported["pose_version"] == 7
    with pytest.raises(DomainError):
        model.export(dict(poses={"a": dict(quaternion=[1,1,1,1])}))


def test_parameters_annotations_and_multifile_entrypoint(tmp_path):
    source = '''import cadquery as cq

def build(context):
    width = context.parameter("width", 10, minimum=1, maximum=50)
    context.show(cq.Workplane().box(width, 5, 2), name="box")
    context.dimension("width_label", "box", (0,0,0), (width,0,0), parameter="width")
'''
    model, result = build(tmp_path, source, {"width": 25})
    assert result["parts"][0]["bounds"]["size"][0] == pytest.approx(25)
    assert result["annotations"][0]["value"] == 25
    with pytest.raises(DomainError, match="fresh candidate"):
        model.build({}, "r2")


def test_invalid_shapes_and_duplicate_publications(tmp_path):
    with pytest.raises(DomainError, match="unique"):
        build(tmp_path, 'import cadquery as cq\ns=cq.Workplane().box(1,1,1)\nshow(s,name="x")\nshow(s,name="x")')


def test_rail_fixture_and_semantic_features(tmp_path):
    model, result = build(tmp_path, (ROOT / "fixtures/rail_carriage.py").read_text())
    assert len(result["parts"]) == 2
    assert len(result["parts"][0]["features"]["mounting_holes"]) == 2
    assert result["annotations"][0]["value"] == 120


def test_private_protocol_framing_and_bounded_logs(tmp_path):
    source_root = tmp_path / "source"
    source_root.mkdir()
    (source_root / "model.py").write_text('import cadquery as cq\nprint("x"*100000)\nshow(cq.Workplane().box(1,2,3),name="box")')
    requests = [dict(jsonrpc="2.0", id=1, protocol_version=1, worker_generation="g", revision="r", method="build", params=dict(source_root=str(source_root))),
                dict(jsonrpc="2.0", id=2, protocol_version=1, worker_generation="g", revision="old", method="inspect", params={})]
    result = subprocess.run([str(ROOT / ".venv/bin/python"), "-m", "cadpad_worker", "--cache", str(tmp_path / "cache"), "--generation", "g"],
                            input="".join(json.dumps(r)+"\n" for r in requests), text=True, capture_output=True, timeout=60)
    assert result.returncode == 0, result.stderr
    first, second = map(json.loads, result.stdout.splitlines())
    assert first["result"]["parts"][0]["name"] == "box"
    assert len(first["logs"]) == 32768 and first["logs_truncated"]
    assert second["error"]["data"]["code"] == "STALE_ENTITY"


def test_nested_assembly_locations_and_existing_shape_placement(tmp_path):
    source = '''import cadquery as cq
leaf = cq.Assembly(name="leaf", loc=cq.Location(cq.Vector(0,20,0)))
leaf.add(cq.Workplane().box(10,10,10).translate((5,0,0)), name="box", loc=cq.Location(cq.Vector(10,0,0)))
assembly = cq.Assembly(name="root", loc=cq.Location(cq.Vector(100,0,0)))
assembly.add(leaf)
show(assembly,name="assembly",position=(0,0,30))
'''
    model, result = build(tmp_path, source)
    part = result["parts"][0]
    ref = dict(revision="r1", instance_id=part["instance_id"], entity_id="part:0")
    item = model.inspect(dict(entities=[ref]))["entities"][0]
    assert item["world_center"] == pytest.approx([115,20,30])
    exported = model.export(dict(format="STEP"))
    imported = cq.importers.importStep(str(model.cache / exported["handle"])).val()
    assert imported.Center().toTuple() == pytest.approx([115,20,30])


def test_surfaces_are_valid_publications_and_mass_uses_si_density(tmp_path):
    source = '''import cadquery as cq
show(cq.Workplane().box(10,10,10),name="solid",density=7800)
show(cq.Workplane().box(10,10,10).faces(">Z"),name="surface")
'''
    model, result = build(tmp_path, source)
    assert result["parts"][0]["mass_kg"] == pytest.approx(0.0078)
    assert result["parts"][1]["volume"] is None
    assert result["parts"][1]["shape_kind"] == "Face"


def test_authoritative_bounds_do_not_change_after_display_tessellation(tmp_path):
    model, result = build(tmp_path, 'import cadquery as cq\nshow(cq.Workplane().box(75,20,6),name="plate")')
    before = result["parts"][0]["bounds"]
    reference = dict(revision="r1", instance_id="plate", entity_id="part:0")
    for _ in range(2):
        model.mesh(model.parts["plate"].shape)
        after = model.inspect(dict(entities=[reference]))["entities"][0]["bounds"]
        assert before == after
        assert after["size"] == [75, 20, 6]


def build_saved(tmp_path, source, records, parameters=None):
    source_root = tmp_path / "source"
    source_root.mkdir(exist_ok=True)
    (source_root / "model.py").write_text(source)
    model = Model(tmp_path / "cache")
    result = model.build(dict(source_root=str(source_root), parameters=parameters or {}, annotations=records), "r1")
    return model, result


def test_saved_dimensions_regenerate_bounds_semantic_centers_and_cross_instance_values(tmp_path):
    source = '''import cadquery as cq
w=parameter("width",10)
a=cq.Workplane().box(w,4,2)
show(a,name="a",features={"top":a.faces(">Z")})
show(cq.Workplane().box(4,4,2),name="b",position=(30,0,0))
dimension("published_width","a",(-w/2,0,0),(w/2,0,0),parameter="width")
'''
    records = [dict(id="width", label="Width", definition=dict(kind="overall", instance_id="a", axis="x"), format=dict(unit="in",precision=4)),
               dict(id="spacing",label="Center distance",definition=dict(kind="distance",start=dict(kind="feature_center",instance_id="a",feature="top"),end=dict(kind="bounds",instance_id="b",fraction=[0.5,0.5,0.5]))),
               dict(id="published",label="Design width",definition=dict(kind="published",publication_id="published_width"))]
    _, result = build_saved(tmp_path, source, records, {"width":25.4})
    saved = {a["saved_id"]:a for a in result["annotations"] if "saved_id" in a}
    assert saved["width"]["value"] == pytest.approx(25.4)
    assert saved["width"]["format"] == dict(unit="in",precision=4)
    assert saved["width"]["unit"] == "mm"
    assert saved["spacing"]["start"] == pytest.approx([0,0,1])
    assert saved["spacing"]["end"] == pytest.approx([0,0,0])
    assert saved["spacing"]["end_part"] == "b"
    assert saved["spacing"]["value"] == pytest.approx(math.sqrt(901))
    assert saved["published"]["parameter"] == "width"
    _, changed = build_saved(tmp_path, source, records, {"width":50.8})
    assert changed["annotations"][1]["value"] == pytest.approx(50.8)
    assert changed["annotations"][3]["value"] == pytest.approx(50.8)


def test_saved_circular_dimensions_use_exact_brep_and_reject_ambiguous_or_changed_features(tmp_path):
    source = '''import cadquery as cq
part=cq.Workplane().box(20,20,4).faces(">Z").workplane().hole(8)
show(part,name="plate",features={"bore":part.faces("%CYLINDER"),"ambiguous":part.faces("%PLANE"),"circle":part.edges("%CIRCLE").first()})
'''
    records = [dict(id="diameter",label="Bore",definition=dict(kind="diameter",instance_id="plate",feature="bore")),
               dict(id="radius",label="Round edge",definition=dict(kind="radius",instance_id="plate",feature="circle")),
               dict(id="ambiguous",label="Invalid selection",definition=dict(kind="distance",start=dict(kind="feature_center",instance_id="plate",feature="ambiguous"),end=dict(kind="local_point",instance_id="plate",point=[0,0,0])))]
    _, result = build_saved(tmp_path, source, records)
    assert result["annotations"][0]["value"] == pytest.approx(8)
    assert math.dist(result["annotations"][0]["start"],result["annotations"][0]["end"]) == pytest.approx(8)
    assert result["annotations"][1]["value"] == pytest.approx(4)
    assert result["annotations"][2]["status"] == "broken"
    assert result["annotations"][2]["error"]["code"] == "AMBIGUOUS_ANCHOR"
    _, changed = build_saved(tmp_path, 'import cadquery as cq\nshow(cq.Workplane().box(20,20,4),name="plate")', records)
    for item in changed["annotations"]:
        assert item["status"] == "broken"
        assert "value" not in item and "start" not in item and "end" not in item
    _, repaired = build_saved(tmp_path, source, records)
    assert repaired["annotations"][0]["status"] == "resolved"


def test_missing_published_or_instance_anchors_never_keep_old_values(tmp_path):
    records = [dict(id="source",label="Gone",definition=dict(kind="published",publication_id="removed")),
               dict(id="part",label="Gone part",definition=dict(kind="overall",instance_id="missing",axis="x"))]
    _, result = build_saved(tmp_path, 'import cadquery as cq\nshow(cq.Workplane().box(1,2,3),name="box")', records)
    assert len(result["parts"]) == 1
    assert all(item["status"] == "broken" and "value" not in item for item in result["annotations"])


def test_collision_proxy_boolean_coverage_detects_unsampled_protrusions_and_preserves_channels(tmp_path):
    from cadpad_worker.collision import validate_proxies
    source = '''import cadquery as cq
base=cq.Workplane().box(200,20,4).translate((0,0,-5))
left=cq.Workplane().box(200,4,12).translate((0,8,3))
right=cq.Workplane().box(200,4,12).translate((0,-8,3))
show(base.union(left).union(right),name="rail")
show(cq.Workplane().box(20,8,4),name="carriage")
'''
    model, _ = build(tmp_path, source)
    def box(half, center):
        return dict(frame=dict(position_m=center), shape=dict(type="box",half_extents_m=half))
    collision = dict(bodies=dict(rail=[box([.1,.01,.002],[0,0,-.005]),box([.1,.002,.006],[0,.008,.003]),box([.1,.002,.006],[0,-.008,.003])],carriage=[box([.01,.004,.002],[0,0,0])]))
    proof = validate_proxies(model, collision)
    assert proof["method"] == "brep_boolean_coverage"
    assert proof["covered_instances"] == ["carriage", "rail"]
    collision["bodies"]["rail"][0]["shape"]["half_extents_m"][0] = .09999
    with pytest.raises(DomainError, match="residual BRep solids"):
        validate_proxies(model, collision)
    with pytest.raises(DomainError, match="Every published instance"):
        validate_proxies(model, dict(bodies={"rail":[]}))


def test_collision_proxy_bounds_and_rotated_primitives_cover_exact_solids(tmp_path):
    from cadpad_worker.collision import validate_proxies
    model, _ = build(tmp_path, 'import cadquery as cq\nshow(cq.Workplane().box(12,4,6).rotate((0,0,0),(0,0,1),30),name="body")')
    proof = validate_proxies(model, dict(bodies={"body":[dict(shape=dict(type="bounds"))]}))
    assert proof["bounds_padding_mm"] == 1e-6
    angle = math.pi/12
    proxy=dict(frame=dict(quaternion=[0,0,math.sin(angle),math.cos(angle)]),shape=dict(type="box",half_extents_m=[.006,.002,.003]))
    validate_proxies(model, dict(bodies={"body":[proxy]}))
    model.parts["body"].shape = cq.Workplane().sphere(5).val()
    validate_proxies(model, dict(bodies={"body":[dict(shape=dict(type="ball",radius_m=.005))]}))


def test_geometry_fingerprints_ignore_display_state_but_distinguish_equal_summary_shapes():
    from cadpad_worker.model import geometry_fingerprint
    def plate(x):
        return cq.Workplane().box(50, 30, 6).faces('>Z').workplane().pushPoints([(x, 0)]).hole(4).val()
    a, b = plate(-10), plate(10)
    before_checked = a.wrapped.Checked()
    before = geometry_fingerprint(a)
    assert a.wrapped.Checked() == before_checked
    a.tessellate(0.1)
    assert geometry_fingerprint(a) == before == geometry_fingerprint(plate(-10))
    assert a.Volume() == pytest.approx(b.Volume())
    from cadpad_worker.model import bounds
    assert bounds(a) == bounds(b)
    assert geometry_fingerprint(b) != before
    for shape in (cq.Workplane().rect(10, 20).val(), cq.Workplane().box(10, 20, 6).faces('>Z').val()):
        digest = geometry_fingerprint(shape)
        shape.tessellate(0.1)
        assert digest == geometry_fingerprint(shape)


def test_repeated_instances_share_tessellation_and_content_addressed_buffers(tmp_path, monkeypatch):
    import ocp_tessellate.tessellator as tessellator
    original = tessellator.tessellate
    calls = []
    def counted(*args, **kwargs):
        calls.append(1)
        return original(*args, **kwargs)
    monkeypatch.setattr(tessellator, 'tessellate', counted)
    model, result = build(tmp_path, '''import cadquery as cq
for i in range(100):
    show(cq.Workplane().box(10,20,6),name=f'copy_{i}',position=(i*30,0,0))
show(cq.Workplane().box(12,20,6),name='different',position=(0,40,0))
''')
    assert len(calls) == 2
    assert len({part['id'] for part in result['parts']}) == 101
    assert result['parts'][0]['mesh'] == result['parts'][99]['mesh']
    assert result['parts'][0]['geometry_fingerprint'] != result['parts'][100]['geometry_fingerprint']
    buffers = result['parts'][0]['mesh']['buffers']
    assert all(info['handle'] == info['sha256'] + '.bin' for info in buffers.values())
    assert len(list(model.cache.glob('*.bin'))) < len(buffers) * 3


def test_recovery_build_omits_meshes_and_remesh_repairs_bytes_without_reexecuting_source(tmp_path):
    import hashlib
    root = tmp_path / 'source'
    root.mkdir()
    (root / 'model.py').write_text("import cadquery as cq\nshow(cq.Workplane().box(10,20,6),name='part')")
    model = Model(tmp_path / 'cache')
    result = model.build(dict(source_root=str(root), generate_mesh=False), 'revision')
    assert 'mesh' not in result['parts'][0]
    assert not list(model.cache.glob('*.bin'))
    repaired = model.remesh()
    info = repaired['parts'][0]['mesh']['buffers']['vertices']
    path = model.cache / info['handle']
    correct = path.read_bytes()
    path.write_bytes(b'x' * len(correct))
    (root / 'model.py').write_text("raise RuntimeError('must not execute on remesh')")
    again = model.remesh()
    assert again == repaired
    assert path.read_bytes() == correct
    assert hashlib.sha256(correct).hexdigest() == info['sha256']
    assert not list(model.cache.glob('*.tmp'))


def test_edge_spans_exact_properties_semantic_labels_and_rotated_face_normals(tmp_path):
    model, result = build(tmp_path, '''import cadquery as cq
shape = cq.Workplane().box(20,10,4).faces(">Z").workplane().hole(4)
show(shape,name="plate",features={"top":shape.faces(">Z"),"hole":shape.edges("%CIRCLE")})
''')
    part = result["parts"][0]
    buffers = part["mesh"]["buffers"]
    counts_data = (model.cache / buffers["segments_per_edge"]["handle"]).read_bytes()
    counts = struct.unpack(f"<{len(counts_data)//4}I", counts_data)
    assert len(counts) == part["edge_count"]
    assert sum(counts) * 6 * 4 == buffers["edges"]["bytes"]
    refs = [dict(revision="r1", instance_id="plate", entity_id=f"edge:{i}") for i in range(part["edge_count"])]
    edges = model.inspect(dict(entities=refs))["entities"]
    circle = next(edge for edge in edges if edge["curve_type"] == "CIRCLE")
    assert circle["radius"] == pytest.approx(2)
    assert circle["length"] == pytest.approx(4 * math.pi)
    assert circle["semantic_labels"] == ["hole"]
    assert model.inspect(dict(entities=[circle["reference"]],poses={"plate":dict(position=[30,0,0],quaternion=[0,0,0,1])}))["entities"][0]["world_center"][0] == pytest.approx(30)
    ref = dict(revision="r1", instance_id="plate", entity_id=part["features"]["top"][0])
    face = model.inspect(dict(entities=[ref],poses={"plate":dict(position=[0,0,0],quaternion=[math.sqrt(0.5),0,0,math.sqrt(0.5)])}))["entities"][0]
    assert face["semantic_labels"] == ["top"]
    assert face["normal"] == pytest.approx([0,0,1])
    assert face["world_normal"] == pytest.approx([0,-1,0],abs=1e-12)
    with pytest.raises(DomainError, match="different revision"):
        model.inspect(dict(entities=[dict(circle["reference"],revision="old")]))


def test_automatic_diameters_deduplicate_rims_skip_fillets_and_regenerate_values(tmp_path):
    source = '''import cadquery as cq
size=parameter("diameter",6)
shape=cq.Workplane().box(30,20,8).edges("|Z").fillet(2).faces(">Z").workplane().hole(size)
show(shape,name="plate")
'''
    model, result = build(tmp_path, source)
    automatic = [item for item in result["annotations"] if item.get("automatic")]
    assert len(automatic) == 1  # One bore, no duplicate rims or partial fillet cylinders.
    item = automatic[0]
    assert item["kind"] == "diameter" and item["value"] == pytest.approx(6)
    assert math.dist(item["start"], item["end"]) == pytest.approx(6)
    assert item["definition"]["entity"]["entity_kind"] == "face"
    root = tmp_path / "again"; root.mkdir()
    _, rebuilt = build(root, source, {"diameter": 10})
    assert next(item for item in rebuilt["annotations"] if item.get("automatic"))["value"] == pytest.approx(10)
    root = tmp_path / "many"; root.mkdir()
    _, many = build(root, 'import cadquery as cq\ns=cq.Workplane().box(100,30,6).faces(">Z").workplane().pushPoints([(x,0) for x in range(-40,41,8)]).hole(2)\nshow(s,name="many")')
    assert len([item for item in many["annotations"] if item.get("automatic")]) == 8


def test_geometry_anchors_resolve_fresh_topology_reject_changes_and_report_ambiguity(tmp_path):
    from cadpad_worker.annotations import resolve
    source = 'import cadquery as cq\nshow(cq.Workplane().box(20,10,4).faces(">Z").workplane().hole(4),name="plate")'
    model, result = build(tmp_path, source)
    auto = next(item for item in result["annotations"] if item.get("automatic"))
    record = dict(id="bore", label="Bore", definition=auto["definition"])
    refs = [dict(revision="r1",instance_id="plate",entity_id=f"face:{i}") for i in range(result["parts"][0]["face_count"])]
    planar = next(item for item in model.inspect(dict(entities=refs))["entities"] if item.get("normal") == [0,0,1])
    distance = dict(id="center",label="To face",definition=dict(kind="distance",start=planar["anchor"],end=dict(kind="bounds",instance_id="plate",fraction=[0.5,0.5,0.5])))
    assert resolve(model,[record,distance])[0]["value"] == pytest.approx(4)
    assert resolve(model,[record,distance])[1]["value"] == pytest.approx(2)
    root=tmp_path/"fresh"; root.mkdir()
    fresh, _ = build(root, source)
    assert resolve(fresh,[record,distance])[0]["status"] == "resolved"
    assert resolve(fresh,[record,distance])[1]["status"] == "resolved"
    root=tmp_path/"changed"; root.mkdir()
    changed, _ = build(root, source.replace('hole(4)','hole(6)'))
    broken = resolve(changed,[record,distance])
    assert all(item["status"] == "broken" and "value" not in item and "start" not in item for item in broken)
    root=tmp_path/"duplicated"; root.mkdir()
    duplicate, _ = build(root, 'import cadquery as cq\ns=cq.Workplane().box(20,10,4).faces(">Z").workplane().hole(4).val()\nshow(cq.Compound.makeCompound([s,s.copy()]),name="plate")')
    ambiguous = resolve(duplicate,[record])
    assert ambiguous[0]["error"]["code"] == "AMBIGUOUS_ANCHOR"
    assert "value" not in ambiguous[0]


def test_pinned_geometric_circle_suppresses_automatic_duplicate_and_survives_remesh(tmp_path):
    from cadpad_worker.annotations import resolve
    model, result = build(tmp_path, 'import cadquery as cq\nshow(cq.Workplane().cylinder(10,5),name="shaft")')
    auto = next(item for item in result["annotations"] if item.get("automatic"))
    source_root=tmp_path/"source"
    fresh=Model(tmp_path/"pinned-cache")
    pinned=fresh.build(dict(source_root=str(source_root),annotations=[dict(id="shaft",label="Shaft diameter",definition=auto["definition"])]),"r2")
    assert len(pinned["annotations"]) == 1
    assert pinned["annotations"][0]["saved_id"] == "shaft"
    assert pinned["annotations"][0]["value"] == pytest.approx(10)
    assert fresh.remesh()["annotations"] == pinned["annotations"]
    record=dict(id="radius",label="Radius",definition={**auto["definition"],"measure":"radius"})
    assert resolve(fresh,[record])[0]["value"] == pytest.approx(5)


def test_imported_stl_keeps_triangles_without_a_facet_wireframe(tmp_path):
    import numpy as np
    from cadpad_worker import stl
    reference = tmp_path / "box.stl"
    stl.export(cq.Workplane().box(10, 20, 30).val(), reference)
    root = tmp_path / "source"
    (root / "assets").mkdir(parents=True)
    (root / "assets" / "box.stl").write_bytes(reference.read_bytes())
    (root / "model.py").write_text(
        'import cadquery as cq\n'
        'show(import_stl("box.stl"), name="mesh", kind="surface")\n'
        'show(cq.Workplane().box(10, 20, 30), name="solid")\n'
    )
    model = Model(tmp_path / "cache")
    result = model.build(dict(source_root=str(root)), "r1")
    mesh_part, solid_part = result["parts"]
    mesh_buffers, solid_buffers = mesh_part["mesh"]["buffers"], solid_part["mesh"]["buffers"]
    assert mesh_part["face_count"] == 12 and mesh_part["edge_count"] == 18
    assert mesh_buffers["triangles"]["bytes"] > 0
    assert mesh_buffers["edges"]["bytes"] == 0
    assert mesh_part["mesh"]["edge_ids"] == []
    segments = np.frombuffer((model.cache / mesh_buffers["segments_per_edge"]["handle"]).read_bytes(), dtype="<u4")
    assert int(segments.sum()) == 0
    solid_segments = np.frombuffer((model.cache / solid_buffers["segments_per_edge"]["handle"]).read_bytes(), dtype="<u4")
    assert int(solid_segments.sum()) > 0
    assert int(solid_segments.sum()) * 24 == solid_buffers["edges"]["bytes"]


def test_distinct_shapes_never_share_the_tessellator_cache_and_topology_spans_match(tmp_path):
    from ocp_tessellate.tessellator import cache
    cache.clear()
    model, result = build(tmp_path, '''import cadquery as cq
show(cq.Workplane().box(10,20,6),name="box")
show(cq.Workplane().sphere(25),name="sphere")
show(cq.Workplane().cylinder(16,20).edges().fillet(2),name="knob")
show(cq.Workplane().box(10,20,6),name="repeat",position=(40,0,0))
''')
    import numpy as np
    handles=[]
    for part in result["parts"]:
        mesh=part["mesh"];buffers=mesh["buffers"]
        assert mesh["format"] == "cadpad-mesh-v2"
        vertices=np.frombuffer((model.cache/buffers["vertices"]["handle"]).read_bytes(),dtype="<f4").reshape(-1,3)
        spans=np.frombuffer((model.cache/buffers["triangles_per_face"]["handle"]).read_bytes(),dtype="<u4")
        segments=np.frombuffer((model.cache/buffers["segments_per_edge"]["handle"]).read_bytes(),dtype="<u4")
        assert len(spans)==part["face_count"]
        assert len(segments)==len(mesh["edge_ids"])
        assert sorted(identifier for identifier in mesh["edge_ids"] if identifier is not None)==sorted(f"edge:{i}" for i in range(part["edge_count"]))
        assert int(spans.sum())*12==buffers["triangles"]["bytes"]
        assert int(segments.sum())*24==buffers["edges"]["bytes"]
        # Tessellated extrema may miss a curved exact extremum by the display
        # tolerance; these are local arrays, independent of instance placement.
        assert vertices.min(axis=0).tolist()==pytest.approx(part["bounds"]["min"],abs=0.11)
        assert vertices.max(axis=0).tolist()==pytest.approx(part["bounds"]["max"],abs=0.11)
        handles.append(buffers["vertices"]["handle"])
    assert len(set(handles[:3]))==3
    assert handles[0]==handles[3]


@pytest.mark.parametrize("expression", [
    "cq.Workplane().circle(5).val()",
    "cq.Edge.makeLine(cq.Vector(0,0,0), cq.Vector(10,0,0))",
    "cq.Compound.makeCompound([cq.Workplane().box(10,10,10).val(), cq.Workplane().circle(5).val().translate((30,0,0))])",
    "cq.Compound.makeCompound([cq.Workplane().sphere(5).val(), cq.Workplane().circle(5).val().translate((30,0,0))])",
])
def test_standalone_and_compound_loose_edges_keep_display_spans_and_exact_identity(tmp_path, expression):
    import numpy as np
    model, result = build(tmp_path, f"import cadquery as cq\nshow({expression},name='shape')")
    part = result["parts"][0]
    mesh = part["mesh"]
    buffers = mesh["buffers"]
    segments = np.frombuffer((model.cache / buffers["segments_per_edge"]["handle"]).read_bytes(), dtype="<u4")
    assert len(segments) == len(mesh["edge_ids"])
    assert int(segments.sum()) > 0
    assert int(segments.sum()) * 24 == buffers["edges"]["bytes"]
    assert sorted(identifier for identifier in mesh["edge_ids"] if identifier is not None) == sorted(f"edge:{i}" for i in range(part["edge_count"]))
    references = [dict(revision="r1", instance_id="shape", entity_id=f"edge:{i}") for i in range(part["edge_count"])]
    exact = model.inspect(dict(entities=references))
    assert len(exact["entities"]) == part["edge_count"]
    assert all(entity["length"] > 0 for entity in exact["entities"])


def test_performance_reference_has_100_shared_instances_and_30_pinned_dimensions(tmp_path):
    from unittest.mock import patch
    from ocp_tessellate.tessellator import tessellate
    source=tmp_path/"source";source.mkdir()
    fixture=ROOT/"fixtures/performance_reference"
    (source/"model.py").write_text((fixture/"model.py").read_text())
    model=Model(tmp_path/"cache")
    with patch("ocp_tessellate.tessellator.tessellate",wraps=tessellate) as calls:
        result=model.build(dict(source_root=str(source),annotations=json.loads((fixture/"annotations.json").read_text())),"r1")
    assert calls.call_count==1
    assert len(result["parts"])==100
    assert len({part["geometry_fingerprint"]["sha256"] for part in result["parts"]})==1
    assert len({part["mesh"]["buffers"]["vertices"]["handle"] for part in result["parts"]})==1
    triangles=sum(part["mesh"]["buffers"]["triangles"]["bytes"]//12 for part in result["parts"])
    assert 900000<=triangles<=1100000
    pinned=[item for item in result["annotations"] if item.get("saved_id")]
    assert len(pinned)==30 and all(item["status"]=="resolved" and item["value"]==200 for item in pinned)


def test_scaffold_entry_accepts_empty_model_with_existing_project_files(tmp_path):
    source = tmp_path / 'source'
    source.mkdir()
    (source / 'model.py').write_text('# Describe a part to get started.\n')
    (source / 'helper.py').write_text('value = 42\n')
    model = Model(tmp_path / 'cache')
    result = model.build(dict(source_root=str(source), parameters={}), 'r1')
    assert result['parts'] == []
