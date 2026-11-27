"""Each published part records the component file and line of its show() call."""
from cadpad_worker.model import Model


def build(root, files):
    directory = root / 'source'
    for name, text in files.items():
        path = directory / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return Model(root / 'cache').build(dict(source_root=str(directory)), 'r1')


def sources(result):
    return {part['instance_id']: part.get('source') for part in result['parts']}


def test_component_modules_are_recorded_as_part_sources(tmp_path):
    result = build(tmp_path, {
        'model.py': 'import cadquery as cq\nfrom parts import plate\n\n'
                    'def build(context):\n'
                    '    plate.build(context)\n'
                    '    context.show(cq.Workplane().box(5,5,5), name="cube", position=(40,0,0))\n',
        'parts/plate.py': 'import cadquery as cq\n\n'
                          'def build(context):\n'
                          '    for i in range(2):\n'
                          '        context.show(cq.Workplane().box(20,10,2), name=f"plate_{i}", position=(0,0,5*i))\n',
    })
    assert sources(result) == {
        'plate_0': {'file': 'parts/plate.py', 'line': 5},
        'plate_1': {'file': 'parts/plate.py', 'line': 5},
        'cube': {'file': 'model.py', 'line': 6},
    }


def test_assembly_children_share_the_publishing_call(tmp_path):
    result = build(tmp_path, {
        'model.py': 'import cadquery as cq\n'
                    'assembly = cq.Assembly(name="root")\n'
                    'assembly.add(cq.Workplane().box(4,4,4), name="a")\n'
                    'assembly.add(cq.Workplane().box(4,4,4), name="b", loc=cq.Location((10,0,0)))\n'
                    'show(assembly, name="kit")\n',
    })
    assert {'kit/a', 'kit/b'} <= set(sources(result))
    assert all(source == {'file': 'model.py', 'line': 5} for source in sources(result).values())


def test_parts_report_builder_function_features_and_constants(tmp_path):
    result = build(tmp_path, {
        'model.py': 'import cadquery as cq\n'
                    'WIDTH = 30\nRATIOS = (0.5, 2)\nNAME = "x"\n_HIDDEN = 4\n'
                    'def make_door():\n'
                    '    body = cq.Workplane().box(WIDTH, 10, 2)\n'
                    '    show(body, name="door", features={"top": body.faces(">Z"), "rim": body.edges(">Z")})\n'
                    'make_door()\n'
                    'show(cq.Workplane().box(5, 5, 5), name="cube")\n',
    })
    parts = {part['instance_id']: part for part in result['parts']}
    assert parts['door']['builder_function'] == 'make_door'
    assert 'builder_function' not in parts['cube']
    assert parts['door']['source'] == {'file': 'model.py', 'line': 8}
    kinds = parts['door']['feature_kinds']
    assert (kinds['top']['entity'], kinds['top']['count']) == ('face', 1)
    assert (kinds['rim']['entity'], kinds['rim']['count']) == ('edge', 4)
    assert len(kinds['top']['hash']) == 64 and kinds['top']['hash'] != kinds['rim']['hash']
    assert all(len(part['geometry_fingerprint']['sha256']) == 64 for part in parts.values())
    constants = {item['name']: item['values'] for item in result['constants']}
    assert constants == {'WIDTH': [30.0], 'RATIOS': [0.5, 2.0]}


def test_generated_entry_points_credit_the_builder_not_the_adapter(tmp_path):
    from cadpad_worker.entrypoint import adapter_source
    directory = tmp_path / 'source'
    (directory / 'parts').mkdir(parents=True)
    (directory / 'parts' / 'mount.py').write_text(
        'import cadquery as cq\n\n\n'
        'def mount():\n'
        '    return cq.Workplane().box(10, 10, 10).union(cq.Workplane().box(2, 2, 2).translate((20, 0, 0)))\n'
        '    \n'
        'def pair():\n'
        '    return [cq.Workplane().box(1, 1, 1), cq.Workplane().box(2, 2, 2)]\n')
    (directory / 'cadpad_view_0.py').write_text(adapter_source('parts/mount.py', 'pair'))
    result = Model(tmp_path / 'cache').build(dict(source_root=str(directory), entry_point='cadpad_view_0.py'), 'r1')
    assert all(source == {'file': 'parts/mount.py', 'line': 7} for source in sources(result).values())
    assert len(result['parts']) == 2


def test_traced_faces_report_the_operation_that_created_them(tmp_path):
    directory = tmp_path / 'source'
    directory.mkdir()
    (directory / 'model.py').write_text(
        'import cadquery as cq\n\n'
        'def tab(x):\n'
        '    return cq.Workplane().box(2, 2, 2).translate((x, 0, 3))\n\n'
        'def frame():\n'
        '    body = cq.Workplane().box(20, 20, 4)\n'
        '    body = body.union(tab(5))\n'
        '    return body\n\n'
        'show(frame(), name="frame")\n')
    model = Model(tmp_path / 'cache')
    model.build(dict(source_root=str(directory)), 'r1')
    faces = len(model.parts['frame'].shape.Faces())
    reply = model.inspect(dict(history_names=True, entities=[
        dict(revision='r1', instance_id='frame', entity_id=f'face:{index}') for index in range(faces)]))
    calls = [reference['source'] for item in reply['entities']
             for reference in item.get('history_references', []) if 'source' in reference]
    assert [dict(file='model.py', line=4, function='tab'), dict(file='model.py', line=8, function='frame')] in \
        [call[:2] for call in calls]
    assert any(call[0] == dict(file='model.py', line=7, function='frame') for call in calls)
