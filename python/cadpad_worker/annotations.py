"""Resolve portable annotations from fresh BRep and semantic publications.

No topology index is persisted. A feature must resolve uniquely; a missing or
ambiguous anchor produces a broken annotation with no numeric value or endpoints.
"""
import math
import hashlib

import cadquery as cq

from .model import DomainError, bounds, vector, geometry_fingerprint, FINGERPRINT_FORMAT


def feature(model, instance, name):
    if instance not in model.parts:
        raise DomainError("BROKEN_ANCHOR", f"Missing instance {instance}")
    if name not in model.parts[instance].features:
        raise DomainError("BROKEN_ANCHOR", f"Missing feature {instance}/{name}")
    selected = model.parts[instance].features[name]
    selected = selected.vals() if isinstance(selected, cq.Workplane) else [selected]
    selected = [item for item in selected if isinstance(item, cq.Shape)]
    if len(selected) != 1:
        raise DomainError("AMBIGUOUS_ANCHOR", f"Feature {instance}/{name} must identify exactly one entity")
    # A semantic name must refer to geometry actually published in this part.
    shape = model.parts[instance].shape
    entities = [shape, *shape.Faces(), *shape.Edges(), *shape.Vertices()]
    if not any(item.isSame(selected[0]) for item in entities):
        raise DomainError("BROKEN_ANCHOR", f"Feature {instance}/{name} is outside the published part")
    return selected[0]


def geometry_reference(shape):
    return dict(entity_kind="face" if shape.ShapeType() == "Face" else "edge",
                format=FINGERPRINT_FORMAT, sha256=geometry_fingerprint(shape))


def geometric_feature(model, instance, reference):
    if instance not in model.parts:
        raise DomainError("BROKEN_ANCHOR", f"Missing instance {instance}")
    if reference.get("format") != FINGERPRINT_FORMAT or reference.get("entity_kind") not in ("face", "edge"):
        raise DomainError("BROKEN_ANCHOR", "Unsupported geometry anchor format")
    key = (instance, reference["entity_kind"])
    if key not in model._geometry_anchors:
        shape = model.parts[instance].shape
        entities = shape.Faces() if reference["entity_kind"] == "face" else shape.Edges()
        signatures = {}
        for entity in entities:
            signatures.setdefault(geometry_fingerprint(entity), []).append(entity)
        model._geometry_anchors[key] = signatures
    matches = model._geometry_anchors[key].get(reference.get("sha256"), [])
    if not matches:
        raise DomainError("BROKEN_ANCHOR", "Selected geometry no longer matches; repair the measurement")
    if len(matches) != 1:
        raise DomainError("AMBIGUOUS_ANCHOR", "Multiple entities match the saved geometry; use a unique semantic feature")
    return matches[0]


def anchor(model, definition):
    instance = definition["instance_id"]
    if instance not in model.parts:
        raise DomainError("BROKEN_ANCHOR", f"Missing instance {instance}")
    kind = definition["kind"]
    if kind == "local_point":
        point = definition["point"]
    elif kind == "bounds":
        box = bounds(model.parts[instance].shape)
        point = [low + fraction * size for low, fraction, size in
                 zip(box["min"], definition["fraction"], box["size"])]
    elif kind == "feature_center":
        point = vector(feature(model, instance, definition["feature"]).Center())
    elif kind == "geometry_center":
        point = vector(geometric_feature(model, instance, definition["entity"]).Center())
    else:
        raise DomainError("BROKEN_ANCHOR", "Unknown anchor kind")
    if len(point) != 3 or any(not math.isfinite(v) for v in point):
        raise DomainError("BROKEN_ANCHOR", "Anchor requires three finite coordinates")
    return instance, list(point)


def circle_geometry(shape):
    from OCP.BRepAdaptor import BRepAdaptor_Curve, BRepAdaptor_Surface
    if shape.ShapeType() == "Edge" and shape.geomType() == "CIRCLE":
        circle = BRepAdaptor_Curve(shape.wrapped).Circle()
        center, direction, radius = circle.Location(), circle.XAxis().Direction(), circle.Radius()
        point = [center.X(), center.Y(), center.Z()]
    elif shape.ShapeType() == "Face" and shape.geomType() == "CYLINDER":
        cylinder = BRepAdaptor_Surface(shape.wrapped).Cylinder()
        center, direction, radius = cylinder.Location(), cylinder.XAxis().Direction(), cylinder.Radius()
        axis = cylinder.Axis().Direction()
        origin = [center.X(), center.Y(), center.Z()]
        axis = [axis.X(), axis.Y(), axis.Z()]
        distance = sum((p - o) * a for p, o, a in zip(vector(shape.Center()), origin, axis))
        point = [o + distance * a for o, a in zip(origin, axis)]
    else:
        raise DomainError("BROKEN_ANCHOR", "Radius/diameter needs a circular edge or cylindrical face")
    offset = [direction.X() * radius, direction.Y() * radius, direction.Z() * radius]
    return point, offset, radius


def circular(model, definition):
    if definition["kind"] == "geometry_circle":
        shape = geometric_feature(model, definition["instance_id"], definition["entity"])
        measure = definition["measure"]
    else:
        shape = feature(model, definition["instance_id"], definition["feature"])
        measure = definition["kind"]
    point, offset, radius = circle_geometry(shape)
    start = [p - delta for p, delta in zip(point, offset)] if measure == "diameter" else point
    end = [p + delta for p, delta in zip(point, offset)]
    return start, end, radius * (2 if measure == "diameter" else 1)


def angle_value(start, vertex, end):
    """Unsigned angle [0, pi] from exact points; zero-length rays are undefined."""
    a = [p-v for p,v in zip(start,vertex)]
    b = [p-v for p,v in zip(end,vertex)]
    la, lb = math.hypot(*a), math.hypot(*b)
    if min(la,lb) <= 1e-9 or not math.isfinite(la+lb):
        raise DomainError("BROKEN_ANCHOR", "An angle requires two nonzero finite rays")
    a, b = [v/la for v in a], [v/lb for v in b]
    cross = [a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]
    return math.atan2(math.hypot(*cross), sum(x*y for x,y in zip(a,b)))


def world_point(model, instance, point):
    return [a+b for a,b in zip(point,model.parts[instance].position)]


def resolve(model, records):
    published = {item["id"]: item for item in model.annotations}
    output = []
    for record in records:
        item = dict(id=f"saved:{record['id']}", saved_id=record["id"], label=record["label"], kind=record["definition"]["kind"],
                    definition=record["definition"], format=record.get("format", {"unit": "mm", "precision": 2}),
                    label_offset=record.get("label_offset", [0, 0]), parameter=record.get("parameter"),
                    unit="mm", status="resolved", accuracy="exact")
        try:
            definition = record["definition"]
            kind = definition["kind"]
            if kind == "published":
                source = published.get(definition["publication_id"])
                if source is None:
                    raise DomainError("BROKEN_ANCHOR", "Published dimension no longer exists")
                item.update({key: source[key] for key in ("part", "start", "end", "value", "kind", "unit")})
                if source["kind"] == "angle":
                    item.update(vertex=source["vertex"], vertex_part=source["part"])
                item["parameter"] = record.get("parameter") or source.get("parameter")
                item["end_part"] = source["part"]
            elif kind == "distance":
                part, start = anchor(model, definition["start"])
                end_part, end = anchor(model, definition["end"])
                world_start = [a + b for a, b in zip(start, model.parts[part].position)]
                world_end = [a + b for a, b in zip(end, model.parts[end_part].position)]
                item.update(part=part, end_part=end_part, start=start, end=end, value=math.dist(world_start, world_end), kind=kind)
            elif kind == "angle":
                part, start = anchor(model, definition["start"])
                vertex_part, vertex = anchor(model, definition["vertex"])
                end_part, end = anchor(model, definition["end"])
                value = angle_value(world_point(model,part,start),world_point(model,vertex_part,vertex),world_point(model,end_part,end))
                item.update(part=part,vertex_part=vertex_part,end_part=end_part,start=start,vertex=vertex,end=end,value=value,unit="rad")
            elif kind == "overall":
                part = definition["instance_id"]
                if part not in model.parts:
                    raise DomainError("BROKEN_ANCHOR", f"Missing instance {part}")
                box = bounds(model.parts[part].shape)
                axis = "xyz".index(definition["axis"])
                start, end = box["min"][:], box["min"][:]
                end[axis] = box["max"][axis]
                item.update(part=part, end_part=part, start=start, end=end, value=box["size"][axis], kind=kind)
            elif kind in ("radius", "diameter", "geometry_circle"):
                part = definition["instance_id"]
                start, end, value = circular(model, definition)
                kind = definition["measure"] if kind == "geometry_circle" else kind
                item.update(part=part, end_part=part, start=start, end=end, value=value, kind=kind)
            else:
                raise DomainError("BROKEN_ANCHOR", "Unknown saved dimension kind")
            angular = item["kind"] == "angle"
            if (item["format"]["unit"] in ("deg", "rad")) != angular:
                raise DomainError("BROKEN_ANCHOR", "Angle display requires deg/rad; length display requires mm/cm/m/in")
            if item["parameter"] is not None:
                parameter = model.parameters.get(item["parameter"])
                allowed = ("deg", "rad") if angular else ("mm",)
                if parameter is None or parameter["unit"] not in allowed:
                    raise DomainError("BROKEN_ANCHOR", "Dimension parameter binding must reference a published parameter with matching length/angle units")
        except DomainError as error:
            # Discard every value/anchor on failure; never publish stale numbers.
            for key in ("value", "start", "end", "part", "end_part", "vertex", "vertex_part"):
                item.pop(key, None)
            item.update(status="broken", error=dict(code=error.code, message=str(error)))
        output.append(item)
    return output


def same_circular_axis(point, direction, radius, previous):
    other, axis, other_radius = previous
    if abs(radius-other_radius) >= 1e-7 or abs(abs(sum(a*b for a,b in zip(direction,axis)))-1) >= 1e-7:
        return False
    delta = [a-b for a,b in zip(point,other)]
    cross = [delta[1]*axis[2]-delta[2]*axis[1], delta[2]*axis[0]-delta[0]*axis[2], delta[0]*axis[1]-delta[1]*axis[0]]
    return math.hypot(*cross) < 1e-6


def _automatic_circle_candidates(model, instance, part):
    """Intrinsic, unambiguous circular anchors; no occurrence presentation."""
    from OCP.BRepAdaptor import BRepAdaptor_Surface
    seen, candidates = [], []
    for shape in [*part.shape.Faces(), *part.shape.Edges()]:
        kind = shape.ShapeType()
        if kind == "Face" and shape.geomType() == "CYLINDER":
            surface = BRepAdaptor_Surface(shape.wrapped)
            if abs(surface.LastUParameter() - surface.FirstUParameter() - 2 * math.pi) > 1e-6:
                continue
            axis = surface.Cylinder().Axis().Direction()
        elif kind == "Edge" and shape.geomType() == "CIRCLE":
            from OCP.BRepAdaptor import BRepAdaptor_Curve
            curve = BRepAdaptor_Curve(shape.wrapped).Circle()
            if abs(shape.Length() - 2 * math.pi * curve.Radius()) > 1e-6:
                continue
            axis = curve.Axis().Direction()
        else:
            continue
        point, offset, radius = circle_geometry(shape)
        direction = [axis.X(), axis.Y(), axis.Z()]
        if any(same_circular_axis(point, direction, radius, previous) for previous in seen):
            continue
        reference = geometry_reference(shape)
        try:
            geometric_feature(model, instance, reference)
        except DomainError:
            continue
        seen.append((point, direction, radius))
        candidates.append(dict(reference=reference, start=[a-b for a,b in zip(point,offset)],
                               end=[a+b for a,b in zip(point,offset)], value=2*radius))
        if len(seen) == 8:
            break
    return candidates


def automatic_circles(model):
    """Reuse intrinsic discovery, keeping suppression and identities per instance."""
    from .model import intrinsic_reuse_allowed, circular_producers_unchanged
    output, templates = [], {}
    stock_children = circular_producers_unchanged()
    for instance, part in model.parts.items():
        # Read the actual geometry now: do not trust shape identity or a cached
        # fingerprint left by an earlier call after someone changed the shape.
        fingerprint = geometry_fingerprint(part.shape)
        reusable = stock_children and intrinsic_reuse_allowed(part.shape)
        candidates = templates.get(fingerprint) if reusable else None
        if candidates is None:
            candidates = _automatic_circle_candidates(model, instance, part)
            if reusable:
                templates[fingerprint] = candidates
        for candidate in candidates:
            start, end = candidate["start"][:], candidate["end"][:]
            if any(item.get("part")==instance and item.get("kind")=="diameter"
                   and "start" in item and math.dist(item["start"],start)<1e-7 and math.dist(item["end"],end)<1e-7
                   for item in model.annotations):
                continue
            reference = candidate["reference"].copy()
            identity = hashlib.sha256((instance+reference["entity_kind"]+reference["sha256"]).encode()).hexdigest()[:24]
            output.append(dict(id=f"automatic:{identity}", kind="diameter", label="Diameter", part=instance,
                               end_part=instance, start=start, end=end, value=candidate["value"], parameter=None,
                               unit="mm", accuracy="exact", automatic=True,
                               definition=dict(kind="geometry_circle", instance_id=instance, entity=reference, measure="diameter")))
    return output
