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
