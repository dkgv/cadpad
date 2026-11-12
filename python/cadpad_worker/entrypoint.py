"""Inspect entry candidates without importing or executing project code."""
import ast
import json


def index_sources(args):
    """Build a source catalog without importing project modules or constructing CAD."""
    from .part_index import detect
    detected = detect(args['sources'])
    files = []
    for entry, source in sorted(args['sources'].items()):
        plan = analyze(dict(entry=entry, source=source, include_source=False))
        try:
            tree = ast.parse(source)
            lines = {node.name: node.lineno for node in tree.body if isinstance(node, ast.FunctionDef)}
            description = (ast.get_docstring(tree) or '').split('\n')[0][:240]
        except SyntaxError:
            lines, description = {}, ''
        builders = [{key: option[key] for key in ('id', 'label', 'available', 'reason')}
                    | {'line': lines.get(option['id'].split(':')[0], 1),
                       'kind': 'geometry_candidate' if option['id'].split(':')[0] in detected.get(entry, {}) else 'function',
                       'inference': 'source'} for option in plan['options']]
        files.append(dict(path=entry, description=description, builders=builders,
                          recommended=plan['recommended'], diagnostic=plan['diagnostic']))
    return dict(files=files)


def adapter_source(entry, name, context=False, variable=False, producer=None, group=None):
    # User paths and symbol names are encoded as literals, never interpolated as code.
    parts = entry[:-3].split('/')
    package = '.'.join(parts[:-1]) if all(part.isidentifier() for part in parts[:-1]) else ''
    load = (f"    path = Path(__file__).parent / {json.dumps(entry)}\n"
            "    spec = importlib.util.spec_from_file_location('cadpad_source', path)\n"
            "    module = importlib.util.module_from_spec(spec)\n"
            f"    module.__package__ = {json.dumps(package)}\n"
            "    sys.modules[spec.name] = module\n"
            "    root = Path(__file__).parent\n"
            "    sys.path[:0] = [str(parent) for parent in path.parents if parent.is_relative_to(root)]\n"
            "    spec.loader.exec_module(module)\n")
    argument = f"getattr(module, {json.dumps(producer)})()" if producer else "context" if context else ""
    expression = f"getattr(module, {json.dumps(name)})" + ("" if variable else f"({argument})")
    if group:
        expression = '{' + ', '.join(f'{json.dumps(symbol)}: getattr(module, {json.dumps(symbol)})({"context" if context else ""})' for symbol, context in group) + '}'
    return '''# Generated Cadpad entry point. Original source files stay unchanged.
# Importing the module skips its `if __name__ == '__main__'` export script.
import importlib
import importlib.util
import sys
from pathlib import Path
import cadquery as cq


def _publish(context, value, name):
    if isinstance(value, cq.Workplane):
        shapes = [shape for shape in value.vals() if isinstance(shape, cq.Shape)]
        if len(shapes) == 1:
            return _publish(context, shapes[0], name)
        return sum(_publish(context, shape, f"{name}/item_{index}")
                   for index, shape in enumerate(shapes, 1))
    if isinstance(value, cq.Compound):
        # Direct topology children are selectable geometry, not inferred physical parts.
        return sum(_publish(context, child, f"{name}/item_{index}")
                   for index, child in enumerate(value, 1))
    if isinstance(value, (cq.Shape, cq.Assembly)):
        context.show(value, name=name)
        return 1
    if isinstance(value, dict):
        return sum(_publish(context, part, f"{name}_{key}") for key, part in value.items())
    if isinstance(value, (list, tuple)):
        if len(value) == 2 and isinstance(value[0], str):
            return _publish(context, value[1], f"{name}_{value[0]}")
        # Builders often return (metadata, shape) pairs. Non-geometry is skipped.
        return sum(_publish(context, part, f"{name}_{index}") for index, part in enumerate(value, 1))
    return 0


def build(context):
''' + load + f"    result = {expression}\n" + '''    count = _publish(context, result, 'model')
''' + ("    # Context builders may publish directly instead of returning geometry.\n" if context else "    if count == 0:\n        raise ValueError('The selected builder returned no CadQuery geometry. Choose another builder or edit this entry point.')\n")


def analyze(args):
    entry, source = args['entry'], args['source']
    try:
        tree = ast.parse(source, filename=entry)
    except SyntaxError as error:
        return dict(options=[], recommended=None, has_main=False, direct=False,
                    diagnostic=f"{entry}:{error.lineno}: {error.msg}")
    include_source = args.get('include_source', True)
    options = []
    has_main = any(isinstance(node, ast.If) and '__name__' in ast.unparse(node.test) and '__main__' in ast.unparse(node.test) for node in tree.body)
    direct = any(isinstance(node, ast.Expr) and isinstance(node.value, ast.Call) and isinstance(node.value.func, ast.Name) and node.value.func.id in ('show', 'show_object') for node in tree.body)
    for node in tree.body:
        if isinstance(node, ast.FunctionDef) and not node.name.startswith('_') and node.name not in ('main', 'preview', 'export'):
            positional = node.args.posonlyargs + node.args.args
            required = [arg.arg for arg in positional[:len(positional) - len(node.args.defaults)]]
            required += [arg.arg for arg, default in zip(node.args.kwonlyargs, node.args.kw_defaults) if default is None]
            context = len(required) == 1 and required[0] in ('context', 'ctx') and positional and positional[0].arg == required[0]
            eligible = not required or context
            options.append(dict(id=node.name, context=context, label=f"{node.name}({', '.join(arg.arg for arg in positional)})", available=eligible,
                                reason='' if eligible else f"Requires arguments: {', '.join(required)}",
                                source=adapter_source(entry, node.name, context=context) if eligible and include_source else None))
            if node.name == 'build' and context:
                direct = True
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            if any(isinstance(target, ast.Name) and target.id == 'result' for target in targets):
                direct = True
                options.append(dict(id='$result', label='result', available=True, reason='', source=adapter_source(entry, 'result', variable=True) if include_source else None))
    # Offer explicit compositions for the common parts -> assembled preview
    # pattern. Only local, callable-without-arguments producers qualify.
    producers = {option['id'] for option in options if option['available']
                 and option['id'] in ('parts', 'build_parts', 'make_parts')}
    compositions = []
    for node in tree.body:
        if not isinstance(node, ast.FunctionDef) or node.name not in ('assembled_preview', 'assembly', 'build_assembly'):
            continue
        positional = node.args.posonlyargs + node.args.args
        required = positional[:len(positional) - len(node.args.defaults)]
        required_keywords = any(default is None for default in node.args.kw_defaults)
        if len(required) != 1 or required_keywords:
            continue
        for producer in sorted(producers):
            compositions.append(dict(id=f'{node.name}:{producer}', label=f'{node.name}({producer}())',
                                     available=True, reason='', source=adapter_source(entry, node.name, producer=producer) if include_source else None))
    options = compositions + options
    if args.get('group_builders'):
        requested = args['group_builders']
        group = [(option['id'], option.get('context', False)) for option in options
                 if option['id'] in requested and option['available'] and option['id'].isidentifier()]
        if group and len(group) == len(set(requested)):
            options.append(dict(id='$group', label='File geometry', available=True, reason='',
                                source=adapter_source(entry, '', group=group)))
    if include_source:
        options = [option for option in options if option['id'] == args['builder']] if args.get('builder') else options[:64]
    preferred = next((option['id'] for option in options if option['available'] and option['id'] in ('build', 'build_model', 'make_model', 'create_model', '$result')), None)
    if preferred is None:
        preferred = compositions[0]['id'] if compositions else None
    if preferred is None:
        preferred = next((option['id'] for option in options if option['available'] and option['id'].startswith(('build_', 'make_', 'create_'))), None)
    return dict(options=options, recommended=preferred, has_main=has_main, direct=direct, diagnostic=None)
