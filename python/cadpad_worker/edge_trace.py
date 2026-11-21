"""Bounded native edge queries and source attribution by OCCT topology identity."""
import inspect
import math

import cadquery as cq

from .errors import DomainError

_OPERATIONS = frozenset("""
    box rect circle ellipse polygon polyline lineTo line hLine vLine spline threePointArc
    sagittaArc radiusArc cylinder sphere cone torus wedge hole cboreHole cskHole
    extrude twistExtrude revolve loft sweep cut cutThruAll cutBlind cutEach
    union fuse intersect combine fillet chamfer shell offset2D clean mirror
    translate rotate rotateAboutCenter moved located split makeBox makeCylinder
    makeSphere makeCone makeTorus makeWedge makeLine makeCircle makeEllipse
    makeSpline makeThreePointArc makePolygon makeLoft makeOffsetShape
""".split())
_MODULES = frozenset(("cadquery.cq", "cadquery.occ_impl.shapes"))
_SHAPES = (cq.Shape, cq.Vertex, cq.Edge, cq.Wire, cq.Face, cq.Shell, cq.Solid, cq.Compound)


def _shapes(value):
    if type(value) in _SHAPES:
        yield value
    elif type(value) is cq.Workplane:
        yield from (item for item in value.objects if type(item) in _SHAPES)


class OperationTrace:
    """Remember operations only when their returned edge survives with the same identity."""

    def __init__(self, root):
        self.prefix = str(root) + "/"
        self.edges = {}
        self.remaining = 50_000
        self.calls = 512

    def profile(self, frame, event, result):
        if event != "return" or result is None or not self.calls or not self.remaining:
            return
        code = frame.f_code
        if code.co_name not in _OPERATIONS or frame.f_globals.get("__name__") not in _MODULES:
            return
        caller = frame.f_back
        if caller is None:
            return
        filename = caller.f_code.co_filename
        if not filename.startswith(self.prefix) or not filename.endswith(".py"):
            return
        self.calls -= 1
        try:
            # Include the receiver and direct operands. Keep unchanged identities
            # attributed to their earlier operation.
            inputs = set()
            count = code.co_argcount + code.co_kwonlyargcount
            count += bool(code.co_flags & inspect.CO_VARARGS)
            count += bool(code.co_flags & inspect.CO_VARKEYWORDS)
            locals_ = frame.f_locals
            for name in code.co_varnames[:count]:
                value = locals_.get(name)
                if type(value) is dict:
                    operands = value.values()
                elif type(value) in (list, tuple):
                    operands = value
                else:
                    operands = (value,)
                for operand in operands:
                    for shape in _shapes(operand):
                        edges = shape.Edges()
                        self.remaining = max(0, self.remaining - len(edges))
                        if not self.remaining:
                            return
                        inputs.update(edges)
            site = dict(file=filename[len(self.prefix):], line=caller.f_lineno, operation=code.co_name)
            for shape in _shapes(result):
                edges = shape.Edges()
                self.remaining = max(0, self.remaining - len(edges))
                if not self.remaining:
                    return
                for edge in edges:
                    if edge not in inputs:
                        # An earlier operation already observed this identity.
                        self.edges.setdefault(edge, site)
        except Exception:
            # Optional attribution must never fail the design build.
            self.calls = 0


def trace_native_edges(model, args):
    refs = args.get("entities", [])
    if not 1 <= len(refs) <= 16:
        raise DomainError("INVALID_REQUEST", "Trace 1..16 native CAD edges")
    selections = []
    adjacency = {}
    for ref in refs:
        part, edge, kind = model.entity(ref)
        if kind != "edge":
            raise DomainError("INVALID_REQUEST", "Select native CAD edges to trace")
        selections.append((ref, part, edge))
        adjacency.setdefault(part.name, {}).setdefault(edge, [0, []])
    for name, edges in adjacency.items():
        for index, face in enumerate(model.parts[name].shape.Faces()):
            for edge in face.Edges():
                if edge in edges:
                    record = edges[edge]
                    record[0] += 1
                    if len(record[1]) < 16:
                        record[1].append(dict(face_id=f"face:{index}", surface_type=face.geomType()))
    result = []
    for ref, part, edge in selections:
        count, faces = adjacency[part.name][edge]
        result.append(dict(reference=ref, curve_type=edge.geomType(), length_mm=edge.Length(),
                           incident_face_count=count, incident_faces=faces,
                           operation=model._operation_trace.edges.get(edge), publication=part.source))
    return dict(revision=model.revision, accuracy="exact", units="mm", edges=result)


def match_mesh_segments(model, args):
    from .model import bounds

    tolerance = args["tolerance_mm"]
    segments = args["segments"]
    if (not 1 <= len(segments) <= 16 or not math.isfinite(tolerance) or
            not 0.000001 <= tolerance <= 5):
        raise DomainError("INVALID_REQUEST", "Match 1..16 segments with tolerance between 0.000001 and 5 mm")
    part = model.parts.get(args["instance_id"])
    if part is None:
        raise DomainError("STALE_ENTITY", "Unknown target CAD instance")
    pose = args["poses"].get(part.name, dict(position=part.position, quaternion=[0, 0, 0, 1]))
    location = model.location(pose)
    edges = part.shape.Edges()
    if len(edges) > 20_000:
        raise DomainError("INVALID_REQUEST", "Target exceeds 20,000 CAD edges")
    placed = []
    for index, edge in enumerate(edges):
        world_edge = edge.moved(location)
        box = bounds(world_edge)
        placed.append((index, world_edge, box["min"], box["max"]))
    result = []
    for start, end in segments:
        samples = [cq.Vertex.makeVertex(*(a + t * (b - a) for a, b in zip(start, end)))
                   for t in (0, 0.25, 0.5, 0.75, 1)]
        low = list(map(min, zip(start, end)))
        high = list(map(max, zip(start, end)))
        matches = []
        for index, edge, minimum, maximum in placed:
            if any(lo < lower - tolerance or hi > upper + tolerance
                   for lo, hi, lower, upper in zip(low, high, minimum, maximum)):
                continue
            error = 0.0
            for sample in samples:
                error = max(error, sample.distance(edge))
                if error > tolerance:
                    break
            else:
                matches.append((error, index, edge.geomType()))
        matches.sort()
        result.append(dict(candidate_count=len(matches), candidates=[
            dict(reference=dict(revision=model.revision, instance_id=part.name, entity_id=f"edge:{index}"),
                 max_sample_distance_mm=error, curve_type=kind,
                 operation=model._operation_trace.edges.get(edges[index]))
            for error, index, kind in matches[:5]]))
    return dict(target_revision=model.revision, target_pose_version=args["pose_version"],
                tolerance_mm=tolerance, matches=result)
