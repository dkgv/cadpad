from cadpad_worker.entrypoint import analyze, index_sources


def test_assembly_adapter_uses_local_parts_and_skips_main(tmp_path):
    source = '''import cadquery as cq
def parts():
    return {'plate': cq.Workplane().box(2, 3, 4)}
def assembled_preview(p, hardware=True):
    return [('plate', p['plate'].translate((20, 0, 0)))]
if __name__ == '__main__':
    raise RuntimeError('export script ran')
'''
    path = tmp_path / 'alpha' / 'model.py'
    path.parent.mkdir()
    path.write_text(source)
    plan = analyze(dict(entry='alpha/model.py', source=source))
    assert plan['recommended'] == 'assembled_preview:parts'
    adapter = next(o['source'] for o in plan['options'] if o['id'] == plan['recommended'])
    namespace = {'__file__': str(tmp_path / 'cadpad_entry.py')}
    exec(adapter, namespace)
    shown = []
    class Context:
        def show(self, shape, *, name):
            shown.append((name, (shape.val() if hasattr(shape, 'val') else shape).Center().x))
    namespace['build'](Context())
    assert [name for name, _ in shown] == ['model_1_plate']
    assert abs(shown[0][1] - 20) < 1e-8


def test_composition_does_not_guess_required_arguments_or_execute_source():
    plan = analyze(dict(entry='model.py', source='''raise RuntimeError('do not run')
def parts(size): pass
def assembled_preview(p): pass
def build_assembly(p, *, required): pass
'''))
    assert plan['recommended'] is None
    assert not any(o['available'] for o in plan['options'])


def test_source_index_keeps_paths_signatures_and_parse_errors_without_importing():
    index = index_sources(dict(sources={
        'cad/plate.py': '"""Mounting plate."""\nraise RuntimeError("never execute")\ndef plate(width=10): pass\ndef hole(size): pass\n',
        'broken.py': 'def broken(',
        'empty.py': '# notes',
    }))
    files = {file['path']: file for file in index['files']}
    assert files['cad/plate.py']['description'] == 'Mounting plate.'
    plate, hole = files['cad/plate.py']['builders']
    assert plate['id'] == 'plate' and plate['available'] and plate['line'] == 3
    assert hole['id'] == 'hole' and not hole['available']
    assert files['broken.py']['diagnostic']
    assert files['empty.py']['builders'] == []
    assert 'source' not in plate


def test_catalog_can_select_builders_beyond_the_initial_preview_limit():
    source = '\n'.join(f'def part_{index}(): pass' for index in range(100))
    index = index_sources(dict(sources={'parts.py': source}))
    assert len(index['files'][0]['builders']) == 100
    plan = analyze(dict(entry='parts.py', source=source, builder='part_99'))
    assert [option['id'] for option in plan['options']] == ['part_99']
    assert 'part_99' in plan['options'][0]['source']


def test_parts_detected_through_imports_without_including_scalar_helpers():
    sources = {
        'cad/common.py': 'import cadquery as cq\ndef box(): return cq.Workplane().box(1,2,3)\n',
        'cad/brake.py': '''from .common import *
raise RuntimeError('indexing must never import this module')
def brake_body():
    part = box()
    return part.translate((0,0,2))
def mass():
    return box().val().Volume()
def check():
    def helper(): return cq.Workplane().box(1,1,1)
    return True
def assembly():
    return cq.Assembly().add(box())
''',
    }
    files = {file['path']: file for file in index_sources(dict(sources=sources))['files']}
    kinds = {builder['id']: builder['kind'] for builder in files['cad/brake.py']['builders']}
    assert kinds == {'brake_body': 'geometry_candidate', 'mass': 'function', 'check': 'function', 'assembly': 'geometry_candidate'}
    assert all(builder['inference'] == 'source' for builder in files['cad/brake.py']['builders'])


def test_group_adapter_publishes_all_detected_factories_and_rejects_required_arguments(tmp_path):
    source = '''import cadquery as cq
def wheel(): return cq.Workplane().circle(2).extrude(3)
def frame(): return cq.Workplane().box(4, 5, 6)
def helper(size): return cq.Workplane().box(size, size, size)
if __name__ == '__main__': raise RuntimeError('export routine ran')
'''
    (tmp_path / 'model.py').write_text(source)
    plan = analyze(dict(entry='model.py', source=source, builder='$group', group_builders=['wheel', 'frame']))
    assert len(plan['options']) == 1
    namespace = {'__file__': str(tmp_path / 'cadpad_view.py')}
    exec(plan['options'][0]['source'], namespace)
    shown = []
    class Context:
        def show(self, value, *, name): shown.append(name)
    namespace['build'](Context())
    assert shown == ['model_wheel', 'model_frame']
    invalid = analyze(dict(entry='model.py', source=source, builder='$group', group_builders=['wheel', 'helper']))
    assert invalid['options'] == []


def test_nested_model_can_import_helpers_from_project_ancestors(tmp_path, monkeypatch):
    import sys
    monkeypatch.setattr(sys, 'path', sys.path[:])
    alpha = tmp_path / 'alpha'
    entry = alpha / 'playground' / 'mechanism' / 'model.py'
    entry.parent.mkdir(parents=True)
    (alpha / 'ancestor_geometry_helpers.py').write_text('import cadquery as cq\ndef shape(): return cq.Workplane().box(1,2,3)\n')
    source = 'from ancestor_geometry_helpers import shape\ndef build(): return shape()\n'
    entry.write_text(source)
    plan = analyze(dict(entry='alpha/playground/mechanism/model.py', source=source, builder='build'))
    namespace = {'__file__': str(tmp_path / 'cadpad_view.py')}
    exec(plan['options'][0]['source'], namespace)
    shown = []
    class Context:
        def show(self, value, *, name): shown.append(name)
    namespace['build'](Context())
    assert shown == ['model']
    sys.modules.pop('ancestor_geometry_helpers', None)


def test_compound_children_become_separate_selectable_geometry(tmp_path):
    from cadpad_worker.model import Model
    source = '''import cadquery as cq
def build():
    first = cq.Workplane().box(2,3,4).val()
    second = first.translate((10,0,0))
    nested = cq.Compound.makeCompound([second])
    return cq.Compound.makeCompound([first,nested]).moved(cq.Location((20,0,0)))
'''
    (tmp_path / 'original.py').write_text(source)
    plan = analyze(dict(entry='original.py', source=source, builder='build'))
    (tmp_path / 'model.py').write_text(plan['options'][0]['source'])
    result = Model(tmp_path / 'cache').build(dict(source_root=str(tmp_path)), 'compound-test')
    assert [part['instance_id'] for part in result['parts']] == ['model/item_1', 'model/item_2/item_1']
    assert [round(part['bounds']['min'][0]) for part in result['parts']] == [19,29]
    assert all(abs(part['volume'] - 24) < 1e-8 for part in result['parts'])


def test_workplane_stack_is_published_without_losing_shapes(tmp_path):
    from cadpad_worker.model import Model
    source = '''import cadquery as cq
def build():
    first = cq.Workplane().box(2,2,2).val()
    second = first.translate((8,0,0))
    return cq.Workplane().newObject([first, second])
'''
    (tmp_path / 'original.py').write_text(source)
    plan = analyze(dict(entry='original.py', source=source, builder='build'))
    (tmp_path / 'model.py').write_text(plan['options'][0]['source'])
    result = Model(tmp_path / 'cache').build(dict(source_root=str(tmp_path)), 'stack-test')
    assert len(result['parts']) == 2
    assert [part['instance_id'] for part in result['parts']] == ['model/item_1', 'model/item_2']


def test_adapter_keeps_explicit_assembly_child_names_and_grouping(tmp_path):
    from cadpad_worker.model import Model
    source = '''import cadquery as cq
def build():
    shape = cq.Workplane().box(2,2,2).val()
    group = cq.Compound.makeCompound([shape, shape.translate((5,0,0))])
    return cq.Assembly(name='kit').add(shape, name='plate').add(group, name='named_group')
'''
    (tmp_path / 'original.py').write_text(source)
    plan = analyze(dict(entry='original.py', source=source, builder='build'))
    (tmp_path / 'model.py').write_text(plan['options'][0]['source'])
    result = Model(tmp_path / 'cache').build(dict(source_root=str(tmp_path)), 'named-assembly-test')
    assert [part['instance_id'] for part in result['parts']] == ['model/plate', 'model/named_group']
    assert all(abs(part['volume'] - expected) < 1e-8 for part, expected in zip(result['parts'], [8,16]))
