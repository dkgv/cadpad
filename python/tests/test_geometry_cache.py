"""Native BRep reuse is explicit, input-scoped, independent and disposable."""
import copy
import hashlib
import shutil

import pytest
from cadpad_worker.model import Model, DomainError

SOURCE = """
import cadquery as cq
def make():
    print('BUILDER_EXECUTED')
    return cq.Workplane().box(40,20,6).faces('>Z').workplane().hole(4).val()
a=cached_shape('first',make)
b=cached_shape('second',make)
a.move(cq.Location((25,0,0)))
show(a,name='first',features={'top':cq.Workplane(obj=a).faces('>Z')},density=1000)
show(b,name='second',position=(100,0,0),density=2700)
print('SOURCE_EXECUTED')
"""


def build(root, offered=None, previous=None, digest='a'*64, source=SOURCE):
    directory = root/'source'
    directory.mkdir(parents=True)
    (directory/'model.py').write_text(source)
    model = Model(root/'cache')
    if previous:
        for file in previous.cache.glob('*.brep'):
            shutil.copyfile(file, model.cache/file.name)
    result=model.build(dict(source_root=str(directory),input_digest=digest,geometry_cache=offered), 'r1')
    return model,result


def test_cached_native_shapes_preserve_full_publication_and_independent_mutation(tmp_path,capsys):
    first,result=build(tmp_path/'first')
    assert capsys.readouterr().out.count('BUILDER_EXECUTED')==2
    manifest=first._geometry_cache.manifest()
    assert manifest is not None and set(manifest['entries'])=={'first','second'}
    assert manifest['entries']['first']==manifest['entries']['second']
    second,restored=build(tmp_path/'second',manifest,first)
    output=capsys.readouterr().out
    assert 'SOURCE_EXECUTED' in output and 'BUILDER_EXECUTED' not in output
    assert result==restored
    assert restored['parts'][0]['bounds']['min'][0]==5
    assert restored['parts'][1]['bounds']['min'][0]==-20
    # Candidate-owned files never share inodes with the previous cache.
    handle=manifest['entries']['first']['handle']
    assert (first.cache/handle).stat().st_ino!=(second.cache/handle).stat().st_ino


@pytest.mark.parametrize('damage',['input','policy','missing','corrupt','geometry','type','oversized','symlink','malformed_brep'])
def test_invalid_or_ineligible_cache_is_a_fresh_builder_miss(tmp_path,capsys,damage):
    first,result=build(tmp_path/'first')
    capsys.readouterr()
    offered=copy.deepcopy(first._geometry_cache.manifest())
    entry=offered['entries']['first']
    if damage=='input':offered['input_digest']='b'*64
    elif damage=='policy':offered['policy']='0'*64
    elif damage=='missing':(first.cache/entry['handle']).unlink()
    elif damage=='corrupt':(first.cache/entry['handle']).write_bytes(b'corrupt')
    elif damage=='geometry':entry['geometry_sha256']='0'*64
    elif damage=='type':entry['shape_type']='Wire'
    elif damage=='oversized':entry['bytes']=16*1024*1024+1
    elif damage=='symlink':
        target=first.cache/'outside';(first.cache/entry['handle']).rename(target)
        (first.cache/entry['handle']).symlink_to(target)
    elif damage=='malformed_brep':
        data=b'not a BRep';digest=hashlib.sha256(data).hexdigest()
        entry.update(handle=digest+'.brep',sha256=digest,bytes=len(data))
        (first.cache/entry['handle']).write_bytes(data)
    # Symlink entries are intentionally offered directly to the same directory,
    # rather than copied by the unit helper; native copy validation is separate.
    if damage=='symlink':
        first._geometry_cache.offered=offered['entries'];first._geometry_cache.entries={};first._geometry_cache.names=set()
        from cadpad_worker.geometry_cache import take
        from cadpad_worker.model import geometry_fingerprint
        assert take(first.cache,entry,geometry_fingerprint) is None
        repaired=Model(first.cache)
        restored=repaired.build(dict(source_root=str(tmp_path/'first'/'source'),input_digest='a'*64,geometry_cache=offered),'r1')
        assert restored==result
        assert repaired._geometry_cache.manifest() is None
        assert (first.cache/entry['handle']).is_symlink()
        return
    second,restored=build(tmp_path/'second',offered,first)
    assert 'BUILDER_EXECUTED' in capsys.readouterr().out
    assert restored==result
    assert second._geometry_cache.manifest() is not None


def test_helper_falls_back_without_a_native_input_key_and_rejects_duplicate_names(tmp_path,capsys):
    model,_=build(tmp_path/'plain',digest=None)
    assert capsys.readouterr().out.count('BUILDER_EXECUTED')==2
    assert model._geometry_cache.manifest() is None
    with pytest.raises(DomainError,match='unique name'):
        build(tmp_path/'duplicate',source=SOURCE+"\ncached_shape('first',make)\n")


def test_custom_shape_behavior_is_not_serialized_as_stock_geometry(tmp_path):
    source="""
import cadquery as cq
class Custom(cq.Solid):
    def Volume(self): return 123
shape=cached_shape('custom',lambda:Custom(cq.Workplane().box(10,10,10).val().wrapped))
show(shape,name='custom')
"""
    model,result=build(tmp_path,source=source)
    assert result['parts'][0]['volume']==123
    assert model._geometry_cache.manifest() is None


@pytest.mark.parametrize('expression',[
    'cq.Workplane().rect(10,20).val()',
    "cq.Workplane().box(10,20,2).faces('>Z').val()",
    'cq.Workplane().sphere(20).val()',
])
def test_non_solid_and_curved_shape_publications_survive_native_roundtrip(tmp_path,capsys,expression):
    source=f"import cadquery as cq\ndef make():\n print('BUILDER_EXECUTED')\n return {expression}\nshow(cached_shape('shape',make),name='shape')\n"
    first,result=build(tmp_path/'first',source=source)
    capsys.readouterr()
    second,restored=build(tmp_path/'second',first._geometry_cache.manifest(),first,source=source)
    assert 'BUILDER_EXECUTED' not in capsys.readouterr().out
    assert restored==result


@pytest.mark.parametrize('failure',['size_budget','write_failure'])
def test_disposable_cache_failure_preserves_the_same_canonical_publication(tmp_path,monkeypatch,failure):
    first,result=build(tmp_path/'first')
    if failure=='size_budget':
        from cadpad_worker import geometry_cache
        monkeypatch.setattr(geometry_cache,'MAX_ENTRY_BYTES',1)
    else:
        import os
        original=os.replace
        def replace(source,target):
            if str(target).endswith('.brep'):raise OSError('fixture cache write failure')
            return original(source,target)
        monkeypatch.setattr(os,'replace',replace)
    second,restored=build(tmp_path/'second')
    assert restored==result
    assert second._geometry_cache.manifest() is None
