"""Conservative static detection of CadQuery factories; never imports project code."""
import ast

_CONSTRUCTORS = {'Workplane', 'Assembly', 'Shape', 'Solid', 'Compound', 'Wire', 'Face', 'Edge', 'Shell'}
_GEOMETRY_METHODS = {
    'val', 'vals', 'solids', 'faces', 'edges', 'wires', 'shells', 'vertices',
    'translate', 'rotate', 'rotateAboutCenter', 'mirror', 'moved', 'located', 'move',
    'union', 'cut', 'intersect', 'combine', 'clean', 'fuse', 'fillet', 'chamfer',
    'box', 'circle', 'rect', 'polyline', 'polygon', 'spline', 'close', 'extrude',
    'revolve', 'loft', 'sweep', 'workplane', 'center', 'pushPoints', 'hole',
    'cutThruAll', 'cutBlind', 'cutEach', 'eachpoint', 'add', 'toPending', 'shell',
}


def detect(sources):
    modules, paths = {}, {}
    for path, source in sources.items():
        module = path[:-3].replace('/', '.')
        if module.endswith('.__init__'):
            module = module[:-9]
        try:
            tree = ast.parse(source)
        except SyntaxError:
            continue
        modules[module] = (path, tree)
        paths[path] = module

    def resolve_module(name):
        if name in modules or name == 'cadquery':
            return name
        matches = [module for module in modules if module.endswith('.' + name)]
        return matches[0] if len(matches) == 1 else name

    envs, imports, functions = {}, {}, {}
    for module, (path, tree) in modules.items():
        env, pending = {}, []
        for node in tree.body:
            if isinstance(node, ast.FunctionDef):
                env[node.name] = f'{module}:{node.name}'
                functions[f'{module}:{node.name}'] = (module, node)
            elif isinstance(node, ast.Import):
                for alias in node.names:
                    env[alias.asname or alias.name.split('.')[0]] = resolve_module(alias.name if alias.asname else alias.name.split('.')[0])
            elif isinstance(node, ast.ImportFrom):
                base = module.split('.') if path.endswith('/__init__.py') else module.split('.')[:-1]
                prefix = base[:len(base) - node.level + 1] if node.level else []
                target = resolve_module('.'.join(prefix + ([node.module] if node.module else [])))
                pending.extend((target, alias.name, alias.asname or alias.name) for alias in node.names)
        envs[module], imports[module] = env, pending
    for _ in range(len(modules) + 1):
        changed = False
        for module, pending in imports.items():
            env = envs[module]
            for target, name, alias in pending:
                values = envs.get(target, {})
                additions = {k: v for k, v in values.items() if not k.startswith('_')} if name == '*' else {alias: values.get(name, target + '.' + name)}
                for key, value in additions.items():
                    if key not in env:
                        env[key] = value
                        changed = True
        if not changed:
            break

    geometry, assemblies = set(), set()

    def symbol(node, env):
        if isinstance(node, ast.Name):
            return env.get(node.id, node.id)
        if isinstance(node, ast.Attribute):
            parent = symbol(node.value, env)
            return envs.get(parent, {}).get(node.attr, parent + '.' + node.attr)
        return ''

    def shape(node, env, variables):
        if node is None:
            return False
        if isinstance(node, ast.Name):
            return node.id in variables
        if isinstance(node, ast.Call):
            target = symbol(node.func, env)
            if target in geometry:
                return True
            if target.startswith('cadquery.') and target.split('.')[1] in _CONSTRUCTORS:
                return True
            if isinstance(node.func, ast.Attribute) and node.func.attr in _GEOMETRY_METHODS:
                return shape(node.func.value, env, variables)
            return False
        if isinstance(node, (ast.List, ast.Tuple, ast.Set)):
            return any(shape(item, env, variables) for item in node.elts)
        if isinstance(node, ast.Dict):
            return any(shape(item, env, variables) for item in node.values)
        if isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp)):
            return shape(node.elt, env, variables)
        if isinstance(node, ast.DictComp):
            return shape(node.value, env, variables)
        if isinstance(node, ast.IfExp):
            return shape(node.body, env, variables) or shape(node.orelse, env, variables)
        return False

    def body_nodes(function):
        pending = list(function.body)
        while pending:
            node = pending.pop()
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                continue
            yield node
            pending.extend(ast.iter_child_nodes(node))

    for _ in range(len(functions) + 1):
        added = False
        for key, (module, function) in functions.items():
            if key in geometry:
                continue
            env, variables = envs[module], set()
            nodes = list(body_nodes(function))
            for _ in range(4):
                for node in nodes:
                    if isinstance(node, (ast.Assign, ast.AnnAssign)) and shape(node.value, env, variables):
                        targets = node.targets if isinstance(node, ast.Assign) else [node.target]
                        variables.update(target.id for target in targets if isinstance(target, ast.Name))
                    if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr in ('append', 'extend') and any(shape(arg, env, variables) for arg in node.args):
                        if isinstance(node.func.value, ast.Name):
                            variables.add(node.func.value.id)
            returns_shape = any(isinstance(node, ast.Return) and shape(node.value, env, variables) for node in nodes)
            publishes = any(isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and isinstance(node.func.value, ast.Name) and node.func.value.id in ('context', 'ctx') and node.func.attr == 'show' for node in nodes)
            if returns_shape or publishes:
                geometry.add(key)
                added = True
                if function.name in ('assembled_preview', 'assembly', 'build_assembly') or any(isinstance(node, ast.Call) and symbol(node.func, env).startswith('cadquery.Assembly') for node in nodes):
                    assemblies.add(key)
        if not added:
            break
    return {path: {name: ('assembly' if f'{module}:{name}' in assemblies else 'part')
                   for name, target in envs[module].items() if target == f'{module}:{name}' and target in geometry}
            for path, module in paths.items()}
