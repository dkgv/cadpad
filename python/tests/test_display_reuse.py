import copy
import shutil
import sys

import pytest

from cadpad_worker.model import DomainError, Model
from ocp_tessellate.tessellator import tessellate


def build(root, length=10, name="round", generate_mesh=True):
    source=root/"source"
    source.mkdir(parents=True)
    (source/"model.py").write_text(
        "import cadquery as cq\n"
        "length=parameter('length',10,unit='mm',minimum=1,maximum=100)\n"
        "show(cq.Workplane().box(length,10,10),name='box')\n"
        f"show(cq.Workplane().sphere(5),name={name!r},position=(30,0,0))\n"
        "from pathlib import Path\n"
        "marker=Path(__file__).with_name('executed')\n"
        "marker.write_text('source ran')\n")
    model=Model(root/"cache")
    result=model.build(dict(source_root=str(source),parameters={"length":length},generate_mesh=generate_mesh),str(root.name))
    assert (source/"executed").read_text()=="source ran"
    return model,result


def offers(old,publication,new):
    result={}
    for part in publication["parts"]:
        mesh=part["mesh"]
        for info in mesh["buffers"].values():
            shutil.copyfile(old.cache/info["handle"],new.cache/info["handle"])
        result[part["geometry_fingerprint"]["sha256"]]=copy.deepcopy(mesh)
    return result


def remesh_count(model,offered):
    count=0
    cells=dict(zip(tessellate.__code__.co_freevars,tessellate.__closure__ or ()))
    def profile(frame,event,arg):
        nonlocal count
        if event=="call" and frame.f_code is tessellate.__code__ and all(frame.f_locals.get(name) is cell.cell_contents for name,cell in cells.items()):
            count+=1
    previous=sys.getprofile()
    try:
        sys.setprofile(profile)
        result=model.remesh({"reuse_meshes":offered})
    finally:
        sys.setprofile(previous)
    return result,count


def test_changed_geometry_tessellates_and_renamed_equivalent_shape_reuses_verified_topology(tmp_path):
    old,publication=build(tmp_path/"old")
    new,geometry=build(tmp_path/"new",length=20,name="renamed",generate_mesh=False)
    assert old._display_policy==new._display_policy
    offered=offers(old,publication,new)
    displayed,calls=remesh_count(new,offered)
    assert calls==1
    assert displayed["parts"][1]["mesh"]["face_ids"]==publication["parts"][1]["mesh"]["face_ids"]
    assert displayed["parts"][0]["bounds"]["size"][0]==20
    assert new.inspect({"entities":[{"revision":"new","instance_id":"renamed","entity_id":"face:0"}]})["entities"][0]["radius"]==5
    with pytest.raises(DomainError,match="different revision"):
        new.inspect({"entities":[{"revision":"old","instance_id":"round","entity_id":"face:0"}]})
    buffer=publication["parts"][1]["mesh"]["buffers"]["vertices"]["handle"]
    before=(old.cache/buffer).read_bytes()
    (new.cache/buffer).write_bytes(b"candidate changed")
    assert (old.cache/buffer).read_bytes()==before
    assert all("mesh" not in part for part in geometry["parts"])


@pytest.mark.parametrize("failure",["missing","corrupt","policy","manifest","oversized"])
def test_unusable_reuse_is_a_miss_and_regenerates_current_geometry(tmp_path,failure):
    old,publication=build(tmp_path/"old")
    new,_=build(tmp_path/"new",generate_mesh=False)
    offered=offers(old,publication,new)
    part=publication["parts"][0]
    key=part["geometry_fingerprint"]["sha256"]
    info=part["mesh"]["buffers"]["vertices"]
    if failure=="missing":
        (new.cache/info["handle"]).unlink()
    elif failure=="corrupt":
        data=bytearray((new.cache/info["handle"]).read_bytes());data[0]^=1
        (new.cache/info["handle"]).write_bytes(data)
    elif failure=="policy":
        offered[key]["cache_policy"]="0"*64
    elif failure=="manifest":
        offered[key]["buffers"]["vertices"]["handle"]="../escape.bin"
    else:
        offered[key]["buffers"]["vertices"]["bytes"]=17*1024*1024
    displayed,calls=remesh_count(new,offered)
    assert calls==1


def test_explicit_remesh_does_not_reuse_a_previous_offer(tmp_path):
    old,publication=build(tmp_path/"old")
    new,_=build(tmp_path/"new",generate_mesh=False)
    _,calls=remesh_count(new,offers(old,publication,new))
    assert calls==0
    fresh,calls=remesh_count(new,{})
    assert calls==2


def test_deep_or_cyclic_producer_wrappers_disable_reuse(tmp_path):
    from cadpad_worker.display_cache import policy
    from cadpad_worker.model import geometry_fingerprint, FINGERPRINT_FORMAT
    model, _ = build(tmp_path/"wrapped", generate_mesh=False)
    original = model.mesh
    def wrapped(*args, **kwargs):
        return original(*args, **kwargs)
    wrapped.__wrapped__ = wrapped
    model.mesh = wrapped
    assert policy(model, geometry_fingerprint, FINGERPRINT_FORMAT) is None


def test_offer_limits_use_the_same_compact_manifest_size_as_core():
    from cadpad_worker.display_cache import offers as accepted, MAX_MANIFEST_BYTES
    value = {str(index): {} for index in range(128)}
    assert accepted({"reuse_meshes": value}) == value
    assert accepted({"reuse_meshes": {**value, "extra": {}}}) == {}
    # Compact JSON is exactly the limit; whitespace must not reject a valid offer.
    value = {"key": "x" * (MAX_MANIFEST_BYTES - len('{"key":""}'))}
    assert accepted({"reuse_meshes": value}) == value
    value["key"] += "x"
    assert accepted({"reuse_meshes": value}) == {}
