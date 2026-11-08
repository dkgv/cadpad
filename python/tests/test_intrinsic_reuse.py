"""Exact metadata reuse must preserve occurrence semantics and mutable isolation."""
import cadquery as cq
import pytest
import sys
from cadpad_worker.model import Model, DomainError
from cadpad_worker import annotations


def build(root, source):
    source_root = root / "source"
    source_root.mkdir()
    (source_root / "model.py").write_text(source)
    model = Model(root / "cache")
    return model, model.build(dict(source_root=str(source_root), generate_mesh=False), "r1")


def test_equal_geometry_reuses_exact_integrals_but_not_features_density_pose_or_mutable_results(tmp_path, monkeypatch):
    calls = {"volume": 0, "area": 0}
    def profile(frame, event, argument):
        if event == "call":
            if frame.f_code is cq.Shape.Volume.__code__: calls["volume"] += 1
            if frame.f_code is cq.Shape.Area.__code__: calls["area"] += 1
    previous = sys.getprofile()
    sys.setprofile(profile)
    try:
        model, result = build(tmp_path, """
import cadquery as cq
first = cq.Workplane().box(10,20,6)
second = cq.Workplane().box(10,20,6)
show(first,name='first',features={'top':first.faces('>Z')},density=1000)
show(second,name='second',position=(100,0,0),features={'bottom':second.faces('<Z')},density=2700)
show(cq.Workplane().box(20,20,6),name='different')
""")
    finally:
        sys.setprofile(previous)
    a, b, other = result["parts"]
    assert a["geometry_fingerprint"] == b["geometry_fingerprint"] != other["geometry_fingerprint"]
    assert calls == {"volume": 2, "area": 2}
    assert a["volume"] == b["volume"]
    assert a["volume"] == pytest.approx(1200, rel=1e-12)
    assert a["area"] == b["area"] == 760
    assert a["face_count"] == b["face_count"] == 6
    assert a["edge_count"] == b["edge_count"] == 12
    assert a["mass_kg"] == pytest.approx(.0012)
    assert b["mass_kg"] == pytest.approx(.00324)
    assert a["position"] == [0,0,0] and b["position"] == [100,0,0]
    assert a["features"]["top"] != b["features"]["bottom"]
    assert a["dimensions"][0]["id"] == "first:x" and b["dimensions"][0]["id"] == "second:x"
    a["bounds"]["min"][0] = -999
    assert b["bounds"]["min"][0] == -5
    assert model.describe(model.parts["first"])["bounds"]["min"][0] == -5
    # Actual geometry, not a stale Part.fingerprint, governs cache use.
    model.parts["second"].shape = cq.Workplane().box(30,20,6).val()
    with pytest.raises(DomainError, match="Geometry changed"):
        model.describe(model.parts["second"])


def test_automatic_discovery_reuses_geometry_but_keeps_local_anchors_ids_and_suppression_per_instance(tmp_path, monkeypatch):
    discovered = []
    original = annotations._automatic_circle_candidates
    def counted(model, instance, part):
        discovered.append(instance)
        return original(model, instance, part)
    monkeypatch.setattr(annotations, "_automatic_circle_candidates", counted)
    model, result = build(tmp_path, """
import cadquery as cq
def plate(x):
    return cq.Workplane().box(40,20,6).faces('>Z').workplane().pushPoints([(x,0)]).hole(4)
show(plate(-10),name='first')
show(plate(-10),name='second',position=(100,0,0))
show(plate(10),name='changed')
""")
    assert discovered == ["first", "changed"]
    a, b, changed = result["parts"]
    assert a["bounds"] == changed["bounds"]
    assert a["volume"] == pytest.approx(changed["volume"])
    assert a["geometry_fingerprint"] == b["geometry_fingerprint"] != changed["geometry_fingerprint"]
    automatic = {item["part"]: item for item in result["annotations"] if item.get("automatic")}
    assert set(automatic) == {"first", "second", "changed"}
    assert len({item["id"] for item in automatic.values()}) == 3
    assert all(item["value"] == 4 for item in automatic.values())
    assert (automatic["first"]["start"][0]+automatic["first"]["end"][0])/2 == pytest.approx(-10)
    assert (automatic["second"]["start"][0]+automatic["second"]["end"][0])/2 == pytest.approx(-10)
    assert (automatic["changed"]["start"][0]+automatic["changed"]["end"][0])/2 == pytest.approx(10)
    start, end = automatic["first"]["start"][:], automatic["first"]["end"][:]
    reference = automatic["second"]["definition"]["entity"].copy()
    automatic["first"]["start"][0] = 999
    automatic["first"]["definition"]["entity"]["sha256"] = "0"*64
    assert automatic["second"]["start"] == start
    assert automatic["second"]["definition"]["entity"] == reference
    model.annotations[:] = [item for item in model.annotations if not item.get("automatic")]
    model.annotations.append(dict(id="manual",part="first",kind="diameter",start=start,end=end))
    repeated = annotations.automatic_circles(model)
    assert {item["part"] for item in repeated} == {"second", "changed"}


@pytest.mark.parametrize("kind,expression,area", [
    ("wire", "cq.Workplane().rect(10,10)", 0),
    ("surface", "cq.Workplane().box(10,10,1).faces('>Z')", 100),
])
def test_shared_non_solids_keep_null_volume_and_do_not_invent_mass(tmp_path, kind, expression, area):
    model, result = build(tmp_path, f"import cadquery as cq\nshape={expression}\nshow(shape,name='first',kind='{kind}',density=1000)\nshow(shape,name='second',kind='{kind}',density=2700)\n")
    a, b = result["parts"]
    assert a["geometry_fingerprint"] == b["geometry_fingerprint"]
    assert a["volume"] is b["volume"] is None
    assert a["area"] == b["area"] == pytest.approx(area)
    assert "mass_kg" not in a and "mass_kg" not in b


def test_intrinsic_reuse_keeps_exact_values_when_one_occurrence_has_coarse_display_mesh(tmp_path):
    import math
    model, result = build(tmp_path, """
import cadquery as cq
coarse = cq.Workplane().sphere(20)
coarse.val().mesh(5, .8)
show(coarse,name='coarse')
show(cq.Workplane().sphere(20),name='fresh')
""")
    a, b = result["parts"]
    assert a["geometry_fingerprint"] == b["geometry_fingerprint"]
    assert a["volume"] == b["volume"] == pytest.approx(4/3*math.pi*20**3, rel=1e-10)
    assert a["area"] == b["area"] == pytest.approx(4*math.pi*20**2, rel=1e-10)
    assert a["bounds"]["size"] == b["bounds"]["size"] == pytest.approx([40,40,40])


@pytest.mark.parametrize("custom", ["subclass", "instance"])
def test_custom_shape_methods_are_not_assumed_to_be_intrinsic(tmp_path, custom):
    declaration = "class Custom(cq.Solid):\n def Volume(self): return self.measurement\n" if custom == "subclass" else "Custom=cq.Solid\n"
    overrides = "" if custom == "subclass" else "a.Volume=lambda:100\nb.Volume=lambda:200\n"
    source = "import cadquery as cq\n"+declaration+"base=cq.Workplane().box(10,10,10).val()\na=Custom(base.wrapped);a.measurement=100\nb=Custom(base.wrapped);b.measurement=200\n"+overrides+"show(a,name='first')\nshow(b,name='second')\n"
    model, result = build(tmp_path, source)
    a, b = result["parts"]
    assert a["geometry_fingerprint"] == b["geometry_fingerprint"]
    assert a["volume"] == 100 and b["volume"] == 200
