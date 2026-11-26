"""Feature-derived part placement for assembly models (requirements spec §3.2).

`place(part, on=..., align=..., mate=..., offset=...)` positions one part
relative to a face, edge or whole part of another, so generated assembly
source does not hard-code coordinates. The result is an ordinary
`cq.Shape` with world coordinates baked in; `show()` records how it was
placed so the requirements link stage can verify seated/contact claims and
warn about raw-transform placement (`W_PLACEMENT_UNVERIFIED`).

Frames are right-handed triples `(u, v, n)` with `u x v = n`. All math is
done in a target frame whose origin is the target point and whose axes are
`(u, v, n)`; extents are measured exactly by rotating a copy of the shape
and taking its optimal bounding box, so curved bottoms seat correctly.
"""
from __future__ import annotations

import math

import cadquery as cq

from .errors import DomainError

ALIGNMENTS = ("center", "min", "max")


def _invalid(message):
    return DomainError("INVALID_PARAMETER", message)


def _dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def _cross(a, b):
    return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]


def _scale(a, k):
    return [a[0] * k, a[1] * k, a[2] * k]


def _add(a, b):
    return [a[0] + b[0], a[1] + b[1], a[2] + b[2]]


def _sub(a, b):
    return [a[0] - b[0], a[1] - b[1], a[2] - b[2]]


def _norm(a):
    length = math.sqrt(_dot(a, a))
    if length < 1e-12:
        raise _invalid("Placement frame axis is degenerate")
    return _scale(a, 1.0 / length)


def _mat_vec(rows, v):
    return [_dot(row, v) for row in rows]


def _tight_bounds(shape):
    """Optimal bounding box; identical policy to `model.bounds`."""
    from OCP.BRepBndLib import BRepBndLib
    from OCP.Bnd import Bnd_Box

    box = Bnd_Box()
    BRepBndLib.AddOptimal_s(shape.wrapped, box, False, False)
    xmin, ymin, zmin, xmax, ymax, zmax = box.Get()
    return {
        "min": [xmin, ymin, zmin],
        "max": [xmax, ymax, zmax],
        "size": [xmax - xmin, ymax - ymin, zmax - zmin],
    }


def _center(bounds):
    return _scale(_add(bounds["min"], bounds["max"]), 0.5)


def _frame_axes(normal):
    """Deterministic in-plane axes `(u, v)` with `u x v = n`.

    `u` prefers the world +X direction (then +Y, +Z) projected into the
    plane, so `align="min"`/`"max"` match the intuitive low/high edges for
    axis-aligned targets; `v = n x u`.
    """
    n = _norm(normal)
    u = None
    for seed in ((1.0, 0.0, 0.0), (0.0, 1.0, 0.0), (0.0, 0.0, 1.0)):
        projected = _sub(list(seed), _scale(n, _dot(seed, n)))
        if _dot(projected, projected) > 1e-18:
            u = _norm(projected)
            break
    if u is None:
        raise _invalid("Placement frame axis is degenerate")
    v = _cross(n, u)  # u x v = n  <=>  v = n x u
    return u, v, n


def _trsf(rotation_rows, translation):
    from OCP.gp import gp_Trsf

    # gp_Trsf.SetValues is row-major; the last column is the translation.
    trsf = gp_Trsf()
    trsf.SetValues(
        rotation_rows[0][0], rotation_rows[0][1], rotation_rows[0][2], translation[0],
        rotation_rows[1][0], rotation_rows[1][1], rotation_rows[1][2], translation[1],
        rotation_rows[2][0], rotation_rows[2][1], rotation_rows[2][2], translation[2],
    )
    return trsf


def _measure(shape, origin, axes):
    """Tight bounds of `shape` in the frame `(axes[0], axes[1], axes[2])` at `origin`.

    Returns `{axis_index: (min, max)}` for the three frame coordinates.
    """
    u, v, n = axes
    rows = [u, v, n]
    rotated_origin = _mat_vec(rows, origin)
    translation = [-rotated_origin[0], -rotated_origin[1], -rotated_origin[2]]
    moved = shape.moved(cq.Location(_trsf(rows, translation)))
    bounds = _tight_bounds(moved)
    # The rotated copy's world axes are the frame axes.
    return [(bounds["min"][i], bounds["max"][i]) for i in range(3)]


def _shift(align, part_interval, target_interval):
    """Translation along one axis so the part interval lands per `align`."""
    pmin, pmax = part_interval
    tmin, tmax = target_interval
    if align == "center":
        return (tmin + tmax - pmin - pmax) / 2.0
    if align == "min":
        return tmin - pmin
    return tmax - pmax


class Target:
    """A resolved placement target: a plane (face / part top) or a line (edge)."""

    def __init__(self, kind, origin, axes, extents, description):
        self.kind = kind            # "face" | "edge" | "part"
        self.origin = origin        # point on the plane / on the edge
        self.axes = axes            # (u, v, n) right-handed; edge: (p, q, tangent)
        self.extents = extents      # per-axis (min, max) of the target in frame coords
        self.description = description


def face_target(face, description):
    """Frame for a planar-ish face. Non-planar faces use their bbox centre/normal."""
    bounds = _tight_bounds(face)
    origin = _center(bounds)
    try:
        normal = list(face.normalAt())
    except Exception:
        normal = [0.0, 0.0, 1.0]
    if math.sqrt(sum(c * c for c in normal)) < 1e-9:
        normal = [0.0, 0.0, 1.0]
    u, v, n = _frame_axes(normal)
    # In-plane extent of the face in its own frame.
    extents = _measure(face, origin, (u, v, n))
    return Target("face", origin, (u, v, n), extents, description)


def edge_target(edge, description):
    """Frame for an edge: the part is centred on the edge line (axle/bore case)."""
    bounds = _tight_bounds(edge)
    origin = _center(bounds)
    try:
        tangent = list(edge.tangentAt(0.5))
    except Exception:
        tangent = _sub(bounds["max"], bounds["min"])
    t = _norm(tangent)
    seed = min(((1.0, 0.0, 0.0), (0.0, 1.0, 0.0), (0.0, 0.0, 1.0)),
               key=lambda axis: abs(_dot(axis, t)))
    p = _norm(_cross(t, seed))
    q = _cross(t, p)  # p x q = t
    extents = _measure(edge, origin, (p, q, t))
    return Target("edge", origin, (p, q, t), extents, description)


def part_target(shape, description):
    """Frame for a whole part: its bounding-box top face (+Z normal)."""
    bounds = _tight_bounds(shape)
    origin = [(bounds["min"][0] + bounds["max"][0]) / 2.0,
              (bounds["min"][1] + bounds["max"][1]) / 2.0,
              bounds["max"][2]]
    u, v, n = (1.0, 0.0, 0.0), (0.0, 1.0, 0.0), (0.0, 0.0, 1.0)
    extents = _measure(shape, origin, (u, v, n))
    # Only the in-plane extent is meaningful for a box-top target.
    return Target("part", origin, (u, v, n), extents, description)


def _describe(kind, part=None, feature=None):
    description = {"kind": kind}
    if part is not None:
        description["part"] = part
    if feature is not None:
        description["feature"] = feature
    return description


def target_from_entity(entity, description=None):
    """Build a Target from a cq.Face, cq.Edge or whole cq.Shape."""
    if isinstance(entity, cq.Face):
        return face_target(entity, description or _describe("face"))
    if isinstance(entity, cq.Edge):
        return edge_target(entity, description or _describe("edge"))
    if isinstance(entity, cq.Shape):
        return part_target(entity, description or _describe("part"))
    raise _invalid("place() 'on' must be a cadquery face, edge or shape")


def mate_frame(face):
    """Right-handed frame `(u, v, n)` of a mating face (outward normal)."""
    bounds = _tight_bounds(face)
    center = _center(bounds)
    try:
        normal = _norm(list(face.normalAt()))
    except Exception:
        raise _invalid("place() mate= must be a face with a well-defined normal")
    seed = min(((1.0, 0.0, 0.0), (0.0, 1.0, 0.0), (0.0, 0.0, 1.0)),
               key=lambda axis: abs(_dot(axis, normal)))
    u = _norm(_sub(list(seed), _scale(normal, _dot(seed, normal))))
    v = _cross(normal, u)  # u x v = n
    return center, u, v, normal


def compute(shape, target, mate_face=None, align="center", offset=0.0):
    """Plan the placement of `shape` onto `target`.

    Returns `(location, record)` where `location` is a `cq.Location` that
    moves `shape` into its placed position and `record` is the JSON-safe
    placement description stored in the build result.
    """
    if align not in ALIGNMENTS:
        raise _invalid(f"place() align must be one of {ALIGNMENTS}")
    if isinstance(offset, bool) or not isinstance(offset, (int, float)) or not math.isfinite(offset):
        raise _invalid("place() offset must be a finite number of millimetres")
    offset = float(offset)
    if target.kind == "edge" and mate_face is not None:
        raise _invalid("place() mate= needs a face or part target, not an edge")
    if target.kind == "edge" and offset != 0.0:
        raise _invalid("place() offset needs a face or part target; edges have no normal")

    u, v, n = target.axes
    origin = target.origin

    if mate_face is None:
        if target.kind == "edge":
            p, q, t = u, v, n
            measured = _measure(shape, origin, (p, q, t))
            # Centred on the edge in the plane perpendicular to it; `align`
            # applies along the edge's own extent.
            translation = _add(
                _add(_scale(p, _shift("center", measured[0], (0.0, 0.0))),
                     _scale(q, _shift("center", measured[1], (0.0, 0.0)))),
                _scale(t, _shift(align, measured[2], target.extents[2])),
            )
        else:
            measured = _measure(shape, origin, (u, v, n))
            su = _shift(align, measured[0], target.extents[0])
            sv = _shift(align, measured[1], target.extents[1])
            # Seat the part's lowest extent along n on the target plane.
            sn = offset - measured[2][0]
            translation = _add(_add(_scale(u, su), _scale(v, sv)), _scale(n, sn))
        record = {
            "method": "place",
            "on": target.description,
            "align": align,
            "offset_mm": offset,
        }
        location = cq.Location(_trsf([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], translation))
        return location, record

    # mate mode: rigid transform mapping the mate face onto the target plane.
    if target.kind != "face" and target.kind != "part":
        raise _invalid("place() mate= needs a face or part target")
    mate_center, mu, mv, mn = mate_frame(mate_face)
    # Seat frame: mate outward normal maps to -n so the surfaces oppose.
    seat_u, seat_v, seat_n = u, _scale(v, -1.0), _scale(n, -1.0)
    source_cols = [mu, mv, mn]
    target_cols = [seat_u, seat_v, seat_n]
    # R = F_target @ F_source^T (both frames are right-handed).
    rotation = [
        [sum(target_cols[k][i] * source_cols[k][j] for k in range(3)) for j in range(3)]
        for i in range(3)
    ]
    # Sanity: the rotation must map the mate normal onto -n.
    mapped = _mat_vec(rotation, mn)
    if _dot(_sub(mapped, seat_n), _sub(mapped, seat_n)) > 1e-12:
        raise _invalid("place() could not align the mate face with the target")

    mate_center_rotated = _mat_vec(rotation, mate_center)
    # Height of the mate plane after rotation; seat it `offset` above the target plane.
    plane_height = _dot(origin, n) + offset
    t_normal = plane_height - _dot(mate_center_rotated, n)
    translation = _scale(n, t_normal)

    # In-plane alignment of the mating face's extent inside the target extent.
    rotation_only = cq.Location(_trsf(rotation, [0.0, 0.0, 0.0]))
    rotated_mate = mate_face.moved(rotation_only)
    mate_extents = _measure(rotated_mate, origin, (u, v, n))
    # `translation` is purely along n so far, so the rotated mate's frame
    # intervals compare directly with the target's.
    su = _shift(align, mate_extents[0], target.extents[0])
    sv = _shift(align, mate_extents[1], target.extents[1])
    translation = _add(translation, _add(_scale(u, su), _scale(v, sv)))

    record = {
        "method": "place",
        "on": target.description,
        "align": align,
        "offset_mm": offset,
        "mate": True,
    }
    location = cq.Location(_trsf(rotation, translation))
    return location, record
