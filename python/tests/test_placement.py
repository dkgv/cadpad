"""`place()` assembly helper: feature-derived placement and its metadata."""
import pytest

from cadpad_worker.model import DomainError, Model


def build(tmp_path, source, parameters=None):
    source_root = tmp_path / "source"
    source_root.mkdir(exist_ok=True)
    (source_root / "model.py").write_text(source)
    model = Model(tmp_path / "cache")
    result = model.build(dict(source_root=str(source_root), parameters=parameters or {}), "r1")
    return model, result


def part(result, name):
    return next(p for p in result["parts"] if p["instance_id"] == name)


def test_place_on_cq_face_seats_and_centres(tmp_path):
    _, result = build(tmp_path, '''import cadquery as cq
box = cq.Workplane().box(40, 40, 4)
show(box, name="box")
lid = cq.Workplane().box(20, 20, 2)
show(place(lid, on=box.faces(">Z")), name="lid")
''')
    lid = part(result, "lid")
    # Box top is z=2; the lid's lower face seats exactly on it, centred.
    assert lid["bounds"]["min"][2] == pytest.approx(2.0, abs=1e-6)
    assert lid["bounds"]["max"][2] == pytest.approx(4.0, abs=1e-6)
    assert lid["bounds"]["min"][:2] == pytest.approx([-10.0, -10.0], abs=1e-6)
    record = result["placements"]["lid"]
    assert record["method"] == "place"
    assert record["on"] == {"kind": "face"}
    assert record["align"] == "center"
    assert record["offset_mm"] == 0.0
    assert result["placements"]["box"]["method"] == "source"


def test_place_align_min_max_and_offset(tmp_path):
    _, result = build(tmp_path, '''import cadquery as cq
box = cq.Workplane().box(40, 40, 4)
show(box, name="box")
lid = cq.Workplane().box(20, 20, 2)
show(place(lid, on=box.faces(">Z"), align="min", offset=0.5), name="lid")
''')
    lid = part(result, "lid")
    # align="min": the lid's min edges meet the face's min edges (x=y=-20).
    assert lid["bounds"]["min"][:2] == pytest.approx([-20.0, -20.0], abs=1e-6)
    assert lid["bounds"]["min"][2] == pytest.approx(2.5, abs=1e-6)


def test_place_on_published_feature_uses_part_position(tmp_path):
    _, result = build(tmp_path, '''import cadquery as cq
box = cq.Workplane().box(40, 40, 4)
show(box, name="box", position=(10, 0, 5), features={"top": box.faces(">Z")})
lid = cq.Workplane().box(20, 20, 2)
show(place(lid, on="box#top"), name="lid")
''')
    lid = part(result, "lid")
    # Host position shifts the target: top face is z=7, centred at x=10.
    assert lid["bounds"]["min"][2] == pytest.approx(7.0, abs=1e-6)
    assert lid["bounds"]["min"][0] == pytest.approx(0.0, abs=1e-6)
    record = result["placements"]["lid"]
    assert record["on"] == {"kind": "face", "part": "box", "feature": "top"}


def test_place_on_whole_published_part_uses_bbox_top(tmp_path):
    _, result = build(tmp_path, '''import cadquery as cq
show(cq.Workplane().box(40, 40, 4), name="box")
lid = cq.Workplane().box(20, 20, 2)
show(place(lid, on="box"), name="lid")
''')
    lid = part(result, "lid")
    assert lid["bounds"]["min"][2] == pytest.approx(2.0, abs=1e-6)
    assert result["placements"]["lid"]["on"] == {"kind": "part", "part": "box"}


def test_place_on_edge_centres_part_on_the_line(tmp_path):
    _, result = build(tmp_path, '''import cadquery as cq
box = cq.Workplane().box(40, 40, 4)
show(box, name="box")
wheel = cq.Workplane().box(4, 4, 8)
show(place(wheel, on=box.edges("|Z").vals()[0]), name="wheel")
''')
    wheel = part(result, "wheel")
    edge = part(result, "box")
    # The vertical box edge runs z=-2..2 at a corner; the wheel centres on it.
    assert wheel["bounds"]["size"] == pytest.approx([4.0, 4.0, 8.0])
    centre = [(wheel["bounds"]["min"][i] + wheel["bounds"]["max"][i]) / 2 for i in range(3)]
    # Edge midpoint is a box corner at (±20, ±20, 0); z centre is 0 either way.
    assert centre[2] == pytest.approx(0.0, abs=1e-6)
    assert {round(abs(centre[0]), 6), round(abs(centre[1]), 6)} == {20.0}
    assert result["placements"]["wheel"]["on"]["kind"] == "edge"


def test_place_with_mate_seats_a_vertical_plate_on_a_wall(tmp_path):
    _, result = build(tmp_path, '''import cadquery as cq
box = cq.Workplane().box(40, 40, 4)
show(box, name="box", features={"wall": box.faces(">Y")})
plate = cq.Workplane().box(10, 30, 2)
show(place(plate, on="box#wall", mate=plate.faces(">Z")), name="plate")
''')
    plate = part(result, "plate")
    # The wall is the y=20 face centred at z=0; the plate's top face mates it,
    # so the 2 mm thickness stands off along +Y and the 30 mm side is vertical.
    assert plate["bounds"]["min"][1] == pytest.approx(20.0, abs=1e-6)
    assert plate["bounds"]["size"][1] == pytest.approx(2.0, abs=1e-6)
    assert plate["bounds"]["size"][2] == pytest.approx(30.0, abs=1e-6)
    assert plate["bounds"]["min"][2] == pytest.approx(-15.0, abs=1e-6)
    assert result["placements"]["plate"]["mate"] is True


def test_place_origin_and_show_origin_provenance(tmp_path):
    (tmp_path / "source").mkdir(exist_ok=True)
    (tmp_path / "source" / "lid.py").write_text("# lid builder\n")
    _, result = build(tmp_path, '''import cadquery as cq
box = cq.Workplane().box(40, 40, 4)
show(box, name="box")
lid = cq.Workplane().box(20, 20, 2)
show(place(lid, on=box.faces(">Z"), origin="lid.py#lid"), name="lid")
show(cq.Workplane().box(1, 1, 1), name="tab", origin="lid.py#tab", position=(50, 0, 0))
''')
    assert result["placements"]["lid"]["origin"] == {"entry": "lid.py", "builder": "lid"}
    assert result["placements"]["tab"]["origin"] == {"entry": "lid.py", "builder": "tab"}
    assert result["placements"]["tab"]["method"] == "position"


def test_placements_map_omitted_without_place(tmp_path):
    _, result = build(tmp_path, 'import cadquery as cq\nshow(cq.Workplane().box(1,2,3),name="box",position=(5,0,0))')
    assert "placements" not in result


def test_place_errors_are_clear(tmp_path):
    for name in "abcd":
        (tmp_path / name).mkdir()
    with pytest.raises(DomainError, match="not published"):
        build(tmp_path / "a", '''import cadquery as cq
show(place(cq.Workplane().box(1,1,1), on="ghost#top"), name="x")
''')
    with pytest.raises(DomainError, match="align"):
        build(tmp_path / "b", '''import cadquery as cq
show(cq.Workplane().box(4,4,4), name="box")
show(place(cq.Workplane().box(1,1,1), on=cq.Workplane().box(4,4,4).faces(">Z"), align="sideways"), name="x")
''')
    with pytest.raises(DomainError, match="mate"):
        build(tmp_path / "c", '''import cadquery as cq
show(cq.Workplane().box(4,4,4), name="box")
show(place(cq.Workplane().box(1,1,1), on=cq.Workplane().box(4,4,4).faces(">Z"), mate=cq.Workplane().box(1,1,1)), name="x")
''')
    with pytest.raises(DomainError, match="does not exist"):
        build(tmp_path / "d", '''import cadquery as cq
show(cq.Workplane().box(4,4,4), name="box")
show(cq.Workplane().box(1,1,1), name="x", origin="missing.py#x")
''')
