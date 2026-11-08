"""Inspect entry candidates without importing or executing project code."""
import ast
import json


def adapter_source(entry, name, context=False, variable=False):
    # User paths and symbol names are encoded as literals, never interpolated as code.
    parts = entry[:-3].split('/')
    package = '.'.join(parts[:-1]) if all(part.isidentifier() for part in parts[:-1]) else ''
    load = (f"    path = Path(__file__).parent / {json.dumps(entry)}\n"
            "    spec = importlib.util.spec_from_file_location('cadpad_source', path)\n"
            "    module = importlib.util.module_from_spec(spec)\n"
            f"    module.__package__ = {json.dumps(package)}\n"
            "    sys.modules[spec.name] = module\n"
            "    sys.path.insert(0, str(path.parent))\n"
            "    spec.loader.exec_module(module)\n")
    expression = f"getattr(module, {json.dumps(name)})" + ("" if variable else "(context)" if context else "()")
    return '''# Generated Cadpad entry point. Original source files stay unchanged.
# Importing the module skips its `if __name__ == '__main__'` export script.
import importlib
import importlib.util
import sys
from pathlib import Path
import cadquery as cq


def _publish(context, value, name):
    if isinstance(value, (cq.Workplane, cq.Shape, cq.Assembly)):
        context.show(value, name=name)
        return 1
    if isinstance(value, dict):
        return sum(_publish(context, part, f"{name}_{key}") for key, part in value.items())
    if isinstance(value, (list, tuple)):
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
            options.append(dict(id=node.name, label=f"{node.name}({', '.join(arg.arg for arg in positional)})", available=eligible,
                                reason='' if eligible else f"Requires arguments: {', '.join(required)}",
                                source=adapter_source(entry, node.name, context=context) if eligible else None))
            if node.name == 'build' and context:
                direct = True
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            if any(isinstance(target, ast.Name) and target.id == 'result' for target in targets):
                direct = True
                options.append(dict(id='$result', label='result', available=True, reason='', source=adapter_source(entry, 'result', variable=True)))
    options = options[:64]
    preferred = next((option['id'] for option in options if option['available'] and option['id'] in ('build', 'build_model', 'make_model', 'create_model', '$result')), None)
    if preferred is None:
        preferred = next((option['id'] for option in options if option['available'] and option['id'].startswith(('build_', 'make_', 'create_'))), None)
    return dict(options=options, recommended=preferred, has_main=has_main, direct=direct, diagnostic=None)
