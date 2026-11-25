"""P0.9-P0.11 spike: history names for faces and edges.

A name is   <file>:<function>/<op>#<index>[.<occurrence>]/<role>

  function    enclosing function (qualname) of the call site, "<module>" at top level
  op#index    rank of the call site among same-named operation calls in that function,
              by source position (static, from the AST, not by execution order)
  occurrence  k-th execution of that site in the build (0 is omitted): loops and helpers
  role        entity's role in that operation's result: top/bottom/left/right/front/back,
              wall, cone, sphere, start/end, side<src>, fillet<src>, ...; edges are
              edge[<face>|<face>] (incident faces) or e<i> (profile wires)

Names are followed through later operations with OCC history (Modified / Generated /
IsDeleted, BRepTools_History for UnifySameDomain) read from the builder objects that
CadQuery keeps as locals of its wrappers.  Where history is not reachable the name is
dropped and a tombstone says why (fail loudly, never guess).

An entity may carry several names (faces merged by clean()) and a name may resolve to
several entities (a split face, an array of holes): resolution always returns a set.
"""
import ast
import re
import sys

import cadquery as cq

OPS = frozenset("""
    box rect circle ellipse polygon polyline lineTo line hLine vLine spline threePointArc
    sagittaArc radiusArc cylinder sphere cone torus wedge hole cboreHole cskHole slot2D
    close wire extrude twistExtrude revolve loft sweep cut cutThruAll cutBlind cutEach
    union fuse intersect combine fillet chamfer shell offset2D clean mirror
    translate rotate rotateAboutCenter moved located split makeBox makeCylinder
    makeSphere makeCone makeTorus makeWedge makeLine makeCircle makeEllipse
    makeSpline makeThreePointArc makePolygon makeLoft makeOffsetShape
""".split())
REMOVAL_OPS = frozenset("hole cboreHole cskHole cut cutThruAll cutBlind cutEach".split())
CQ_MODULES = frozenset(("cadquery.cq", "cadquery.occ_impl.shapes"))
SHAPES = (cq.Shape, cq.Vertex, cq.Edge, cq.Wire, cq.Face, cq.Shell, cq.Solid, cq.Compound)
# Internal CadQuery wrappers whose locals hold a builder with history.
INTERNAL_HISTORY = frozenset("cut fuse intersect split fillet chamfer shell clean extrudeLinear revolve "
                             "sweep loft thicken offset2D assembleEdges makeFromWires extrudeLinearWithRotation".split())
# Wrappers that copy/relocate a shape without a reachable builder: positional mapping.
INTERNAL_TRANSFORM = frozenset("_apply_transform mirror transformShape transformGeometry located moved".split())
SIDE_WORD = dict(extrude="side", twistExtrude="side", revolve="face", loft="side", sweep="side",
                 fillet="fillet", chamfer="chamfer", shell="shell", offset2D="offset")


def positional_name(name):
    """Flag identities that depend on source/call order or topology order."""
    separator = name.rfind(":")
    suffix = name[separator + 1:] if separator >= 0 else ""
    operation = suffix.split("/", 1)[1] if "/" in suffix else ""
    ordinal = re.search(r"#(\d+)(?:\.(\d+))?", operation)
    role = name.rsplit("/", 1)[-1]
    return bool((ordinal and (ordinal.group(1) != "0" or ordinal.group(2) is not None))
                or "@" in name or re.fullmatch(r"e\d+", role))


def shapes_in(value):
    if type(value) in SHAPES:
        yield value
    elif type(value) is cq.Workplane:
        yield from (o for o in value.objects if type(o) in SHAPES)
    elif type(value) in (list, tuple):
        for v in value:
            yield from shapes_in(v)
    elif type(value) is dict:
        for v in value.values():
            yield from shapes_in(v)


def entities(shape):
    """Faces then edges (TopoDS identity via CadQuery __eq__/__hash__)."""
    return shape.Faces() + shape.Edges()


class FileIndex:
    """Static call-site index of a source file: position -> (scope, op name, ordinal)."""

    def __init__(self, path):
        with open(path, encoding="utf-8") as source:
            tree = ast.parse(source.read(), filename=path)
        per_scope = {}
        any_scope = {}

        def visit(node, scope):
            for child in ast.iter_child_nodes(node):
                if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                    visit(child, scope + [child.name])
                    continue
                if isinstance(child, ast.Call):
                    f = child.func
                    name = f.attr if isinstance(f, ast.Attribute) else f.id if isinstance(f, ast.Name) else None
                    if name:
                        any_scope.setdefault(".".join(scope) or "<module>", []).append((name, child))
                    if name in OPS:
                        per_scope.setdefault(".".join(scope) or "<module>", []).append((name, child))
                visit(child, scope)

        visit(tree, [])
        self.sites = {}
        for scope, calls in per_scope.items():
            counts = {}
            for name, node in sorted(calls, key=lambda c: (c[1].end_lineno, c[1].end_col_offset)):
                ordinal = counts.get(name, 0)
                counts[name] = ordinal + 1
                self.sites[(node.lineno, node.end_lineno, node.col_offset, node.end_col_offset)] = (scope, name, ordinal)
        # every call, ranked per (scope, callee name): used for the caller path in 'path' mode
        self.calls = {}
        for scope, calls in any_scope.items():
            counts = {}
            for name, node in sorted(calls, key=lambda c: (c[1].end_lineno, c[1].end_col_offset)):
                rank = counts.get(name, 0)
                counts[name] = rank + 1
                self.calls[(node.lineno, node.end_lineno, node.col_offset, node.end_col_offset)] = (scope, name, rank)


def axis_word(n, tol=1e-6):
    for axis, (pos, neg) in zip((0, 1, 2), (("right", "left"), ("back", "front"), ("top", "bottom"))):
        others = [abs(n.toTuple()[i]) for i in range(3) if i != axis]
        if max(others) < tol and abs(abs(n.toTuple()[axis]) - 1) < tol:
            return pos if n.toTuple()[axis] > 0 else neg
    return None


def geometric_role(face, removal):
    kind = face.geomType()
    if kind == "PLANE":
        word = axis_word(face.normalAt())
        if word is None:
            return "facet"
        if removal:  # faces of a cavity face into the void
            return {"top": "floor", "bottom": "ceiling"}.get(word, "wall-" + word)
        return word
    return {"CYLINDER": "wall", "CONE": "cone", "SPHERE": "sphere", "TORUS": "torus"}.get(kind, kind.lower())


class Info:
    __slots__ = ("names", "gen", "hint", "origin", "foreign")

    def __init__(self):
        self.names = set()
        self.gen = set()      # names of the entities this one was Generated from
        self.hint = set()     # builder roles (start/end)
        self.origin = []      # source entities (history), to detect foreign operands
        self.foreign = False


class Namer:
    def __init__(self, root):
        self.root = str(root)
        self.prefix = self.root.rstrip("/") + "/"
        self.info = {}
        self.tombstones = {}       # name -> reason
        self.unreliable = []       # (op, reason) events
        self.files = {}
        self.site_counts = {}
        self.dyn_ordinals = {}
        self.codes = {}
        self.positions = {}
        self.calls = 0
        self.ops = []              # (opkey, n_new_faces, n_new_edges)
        self.dead = False
        self.positional_names = set()
        self.history_names = set()
        # Keep trace_cad_edge response shape compatible on opt-in traced builds.
        self.edges = {}

    # ------------------------------------------------------------------ hook
    def profile(self, frame, event, result):
        if event != "return" or self.dead:
            return
        code = frame.f_code
        kind = self.codes.get(code)
        if kind is None:
            mod = frame.f_globals.get("__name__")
            kind = 0
            if mod in CQ_MODULES:
                low = mod == "cadquery.occ_impl.shapes"
                kind = (1 if code.co_name in OPS else 0) | (2 if low and code.co_name in INTERNAL_HISTORY else 0) \
                    | (4 if low and code.co_name in INTERNAL_TRANSFORM else 0)
            self.codes[code] = kind
        if not kind or result is None:
            return
        try:
            if kind & 2:
                self.internal_history(frame, result)
            if kind & 4:
                self.internal_transform(frame, result)
            if kind & 1:
                caller = frame.f_back
                if caller is not None and caller.f_code.co_filename.startswith(self.prefix):
                    self.user_op(frame, caller, result)
        except Exception as exc:  # the spike must be loud about its own failures
            self.dead = True
            self.unreliable.append(("tracer", repr(exc)))
            raise

    # ------------------------------------------------------------ operands
    @staticmethod
    def operand_shapes(frame):
        code = frame.f_code
        count = code.co_argcount + code.co_kwonlyargcount + bool(code.co_flags & 4) + bool(code.co_flags & 8)
        out = []
        for name in code.co_varnames[:count]:
            out.extend(shapes_in(frame.f_locals.get(name)))
        return out

    def get(self, entity):
        rec = self.info.get(entity)
        if rec is None:
            rec = self.info[entity] = Info()
        return rec

    # ------------------------------------------------------- history events
    def internal_history(self, frame, result):
        builders = []
        for v in frame.f_locals.values():
            if hasattr(v, "Modified") and hasattr(v, "Generated") and hasattr(v, "IsDeleted"):
                builders.append(("occ", v))
            elif type(v).__name__ == "ShapeUpgrade_UnifySameDomain":
                builders.append(("hist", v.History()))
        ins = []
        for s in self.operand_shapes(frame):
            ins.extend(entities(s))
        outs_shapes = list(shapes_in(result))
        outs = set()
        for s in outs_shapes:
            outs.update(entities(s))
        name = frame.f_code.co_name
        lost_reason = None
        if not builders:
            lost_reason = f"{name}: no builder history reachable"
            self.unreliable.append((name, lost_reason))
        # first/last shape hints (extrude, revolve)
        for _, b in builders:
            for attr, hint in (("FirstShape", "start"), ("LastShape", "end")):
                if hasattr(b, attr):
                    try:
                        for f in cq.Shape.cast(getattr(b, attr)()).Faces():
                            if f in outs:
                                self.get(f).hint.add(hint)
                    except Exception:
                        pass
        for ent in ins:
            if ent in outs:
                continue
            mods, deleted = [], False
            for style, b in builders:
                if style == "occ":
                    mods += [(s, False) for s in b.Modified(ent.wrapped)]
                    mods += [(s, True) for s in b.Generated(ent.wrapped)]
                    deleted |= b.IsDeleted(ent.wrapped)
                else:
                    mods += [(s, False) for s in b.Modified(ent.wrapped)]
                    mods += [(s, True) for s in b.Generated(ent.wrapped)]
                    deleted |= b.IsRemoved(ent.wrapped)
            src = self.info.get(ent)
            src_names = src.names if src else set()
            found = False
            for s, generated in mods:
                d = cq.Shape.cast(s)
                if d not in outs:
                    continue
                found = True
                rec = self.get(d)
                rec.origin.append(ent)
                if src is not None:
                    rec.hint |= src.hint
                    rec.foreign |= src.foreign
                if generated:
                    rec.gen |= src_names
                else:
                    rec.names |= src_names
                    rec.gen |= (src.gen if src else set())
            if not found and src_names:
                reason = lost_reason or (f"{name}: deleted by operation" if deleted else f"{name}: history gap")
                for n in src_names:
                    self.tombstones[n] = reason
            elif found:
                self.history_names.update(src_names)

    def internal_transform(self, frame, result):
        """Copy/relocation with no reachable builder: map by index, verified."""
        ins = self.operand_shapes(frame)
        outs = list(shapes_in(result))
        if len(ins) != 1 or len(outs) != 1:
            for s in ins:
                for e in entities(s):
                    if e in self.info and self.info[e].names:
                        for n in self.info[e].names:
                            self.tombstones[n] = f"{frame.f_code.co_name}: multi-copy transform"
            return
        s, r = ins[0], outs[0]
        if s.wrapped.IsSame(r.wrapped):
            return
        fs, fr = s.Faces(), r.Faces()
        es, er = s.Edges(), r.Edges()
        ok = len(fs) == len(fr) and len(es) == len(er) and all(a.geomType() == b.geomType() for a, b in zip(fs, fr))
        if ok and fs:
            ratios = {round(b.Area() / a.Area(), 6) for a, b in zip(fs, fr) if a.Area() > 1e-9}
            ok = len(ratios) <= 1
        if not ok:
            for e in es + fs:
                if e in self.info:
                    for n in self.info[e].names:
                        self.tombstones[n] = f"{frame.f_code.co_name}: positional mapping failed verification"
            return
        for a, b in list(zip(fs, fr)) + list(zip(es, er)):
            src = self.info.get(a)
            if src is not None:
                rec = self.get(b)
                rec.names |= src.names
                rec.gen |= src.gen
                rec.hint |= src.hint
                rec.foreign |= src.foreign
                rec.origin.append(a)
                self.positional_names.update(rec.names)

    # --------------------------------------------------------- user-level op
    def site_key(self, caller):
        code = caller.f_code
        path = code.co_filename
        index = self.files.get(path)
        if index is None:
            index = self.files[path] = FileIndex(path)
        pos = self.positions.get(code)
        if pos is None:
            pos = self.positions[code] = list(code.co_positions())
        p = pos[caller.f_lasti // 2]
        site = index.sites.get((p[0], p[1], p[2], p[3]))
        return path[len(self.prefix):], site, p

    def user_op(self, frame, caller, result):
        opname = frame.f_code.co_name
        rel, site, p = self.site_key(caller)
        aliased = site is None
        if aliased:  # aliased call: dynamic ordinal, flagged
            scope = caller.f_code.co_qualname
            ordinal = self.dyn_ordinals.setdefault((rel, scope, opname, p), len([1 for k in self.dyn_ordinals if k[:3] == (rel, scope, opname)]))
            site = (scope, opname, ordinal)
            self.unreliable.append((opname, "aliased call site, dynamic ordinal"))
        scope, _, ordinal = site
        sid = (rel, scope, opname, ordinal)
        suffix = ""
        frame_up, depth, parts = caller.f_back, 0, []
        while frame_up is not None and depth < 4 and frame_up.f_code.co_filename.startswith(self.prefix):
            if frame_up.f_code.co_name != "<module>":
                r2, _, p2 = self.site_key(frame_up)
                call = self.files[frame_up.f_code.co_filename].calls.get((p2[0], p2[1], p2[2], p2[3]))
                if call:
                    parts.append(f"{call[0]}>{call[1]}#{call[2]}")
                depth += 1
            frame_up = frame_up.f_back
        suffix = "".join(f"@{x}" for x in parts)
        key = (sid, suffix)
        occ = self.site_counts.get(key, 0)
        self.site_counts[key] = occ + 1
        opkey = f"{rel}:{scope}/{opname}#{ordinal}" + (f".{occ}" if occ else "") + suffix

        inputs = set()
        for s in self.operand_shapes(frame):
            inputs.update(entities(s))
        self_obj = frame.f_locals.get("self")
        if type(self_obj) is cq.Workplane:
            try:
                inputs.update(entities(self_obj.findSolid()))
            except Exception:
                pass
        removal = opname in REMOVAL_OPS
        result_shapes = list(shapes_in(result))
        new_faces, new_edges = 0, 0
        faces = []
        for s in result_shapes:
            faces.extend(s.Faces())
        for f in faces:
            rec = self.get(f)
            if rec.names or f in inputs:
                continue
            if rec.foreign or any(o in inputs and not self.info.get(o, Info()).names for o in rec.origin):
                rec.foreign = True
                continue
            roles = set()
            if rec.gen:
                word = SIDE_WORD.get(opname, "wall" if removal else "gen")
                roles |= {f"{word}<{g.replace(f'{rel}:{scope}/', '')}>" for g in rec.gen}
            elif rec.hint:
                roles |= {h for h in rec.hint}
            else:
                roles.add(geometric_role(f, removal))
            names = {f"{opkey}/{r}" for r in roles}
            rec.names |= names
            if aliased:
                self.positional_names.update(names)
            new_faces += 1
        # edges
        edge_faces = {}
        for f in faces:
            for e in f.Edges():
                edge_faces.setdefault(e, []).append(f)
        for s in result_shapes:
            for index, e in enumerate(s.Edges()):
                rec = self.get(e)
                if rec.names or e in inputs:
                    continue
                if rec.foreign or any(o in inputs and not self.info.get(o, Info()).names for o in rec.origin):
                    rec.foreign = True
                    continue
                inc = edge_faces.get(e)
                if inc:
                    strip = f"{rel}:{scope}/"
                    parts = sorted({min(self.info[f].names).replace(strip, "") for f in inc if self.info.get(f) and self.info[f].names})
                    if not parts:
                        continue
                    name = f"{opkey}/edge[{'|'.join(parts)}]"
                    rec.names.add(name)
                    if aliased:
                        self.positional_names.add(name)
                else:
                    name = f"{opkey}/e{index}"
                    rec.names.add(name)
                    self.positional_names.add(name)
                new_edges += 1
        self.ops.append((opkey, new_faces, new_edges))

    # ------------------------------------------------------------ resolution
    def snapshot(self, final):
        """name -> (kind, [entities]) over the final shape(s)."""
        table = {}
        for s in shapes_in(final):
            for e in s.Faces():
                for n in self.info.get(e, Info()).names:
                    table.setdefault(n, ("face", []))[1].append(e)
            for e in s.Edges():
                for n in self.info.get(e, Info()).names:
                    table.setdefault(n, ("edge", []))[1].append(e)
        return table
