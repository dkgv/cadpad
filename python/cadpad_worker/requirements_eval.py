"""Requirements evaluator: execute a plan chunk and return raw measurements.

The evaluator is deliberately dumb (spec section 2). It applies states to the
built subject, runs measurement primitives and reports numbers. It never
compares against a threshold, never decides met / not met, never reports a
partial result for a cancelled chunk.

Request (JSON-RPC method ``evaluate_plan_chunk``; the worker must hold the
built subject, so ``revision`` must equal the revision of its ``build``)::

    params = {
      "chunk_id": "c1",                       # optional, echoed
      "limits": {                             # all optional
        "pair_time_limit_s": 20,              # per boolean/distance job
        "chunk_time_limit_s": 120,            # whole chunk, else cancelled
        "workers": 4                          # forked processes; 0 = in-process
      },
      "cache": {"results": true, "rebuild": true},   # both default true
      "states": [
        {
          "id": "s0",
          "params": {"extension": 12.5},      # optional: one rebuild per distinct set
          "moves": [                          # optional, applied in order, no rebuild
            {"op": "rotate", "parts": ["arm"], "axis": "z",      # "x"|"y"|"z"|[x,y,z]
             "origin": [0, 0, 0], "angle_deg": 35.0},
            {"op": "translate", "parts": ["arm"], "vector": [0, 0, 5]}
          ],
          "measurements": [
            {"id": "m1", "primitive": "interference",   "a": ["arm"], "b": ["base", "post"]},
            {"id": "m2", "primitive": "min_clearance",   "a": ["arm"], "b": ["base"]},
            {"id": "m3", "primitive": "extent",          "a": ["arm"], "axis": "x"},
            {"id": "m4", "primitive": "outside_volume",  "a": ["arm"], "env": ["housing"]},
            {"id": "m5", "primitive": "valid_solid_count", "a": ["arm"]},
            {"id": "m6", "primitive": "contact_area", "a": ["arm"], "b": ["base"]}
          ]
        }
      ]
    }

``a`` / ``b`` / ``env`` are part sets: a list of published part names (a bare
string is a one-element set). A state with neither ``params`` nor ``moves`` is
the rest state. ``moves`` rotate/translate in world coordinates, after the
part's published ``position``. ``param`` values override the build parameters
by name; values equal to the rest build's values reuse the rest model.

Set semantics (all deterministic, member pairs in sorted order):

* ``interference``: sum over unordered member pairs (x in a, y in b, x != y) of
  the volume (mm3) of ``common(x, y)``. ``witness`` = pair with the largest
  volume (first in sorted pair order on ties; null if the total is 0).
* ``contact_area``: sum over the same sorted pair set of the exact area (mm2)
  of common face regions when the pair has no positive-volume intersection.
  Clearance and volume overlap contribute zero. ``witness`` is the pair with
  the largest contact area (first in sorted pair order on ties; null if total
  contact area is zero).
* ``min_clearance``: minimum exact distance (mm) over member pairs; 0 when
  touching or overlapping. ``witness`` = argmin pair.
* ``extent``: world bounding box of the union of ``a`` along the axis;
  ``value`` = size, plus ``min`` / ``max``.
* ``outside_volume``: per member of ``a``, volume (mm3) of ``x`` minus the union
  of ``env``; summed.
* ``valid_solid_count``: number of ``isValid()`` solids over members of ``a``;
  ``solids`` is the total number of solids.

Response ``result``::

    {
      "format": "requirements-eval-v1",
      "chunk_id": "c1",
      "status": "complete" | "cancelled",
      "reason": null | "chunk_time_limit",       # when cancelled
      "engine": {"format": ..., "cadquery": ..., "ocp": ..., "occ_parallel": false},
      "results": null | {                        # null when cancelled: no partial results
        "states": [
          {"id": "s0", "status": "ok" | "error",
           "error": {"code": ..., "message": ...},          # only when error
           "measurements": [
             {"id": "m1", "primitive": "interference",
              "status": "ok" | "inconclusive" | "error",
              "value": 0.0 | null,               # null unless ok
              "witness": ["arm", "base"] | null,
              "pairs_total": 3, "pairs_pruned": 2,   # bbox broad phase, no boolean run
              "items": [{"a": "arm", "b": "base", "status": "ok", "value": 0.0}],
              "reason": null | "pair_time_limit" | "<error text>",
              "min": .., "max": ..}               # extent only
           ]}
        ]},
      "results_sha256": null | "<sha256 of canonical JSON of results>",
      "timings": {"total_s", "phases": {...}, "states": [...], "measurements": [...],
                  "jobs": [{"state","measurement","kind","a","b","seconds","status","source"}],
                  "workers": n},
      "cache": {"result_hits", "result_misses", "dedup", "rebuild_memory_hits",
                "rebuild_disk_hits", "rebuilds"}
    }

``results`` contains no timings, cache provenance or process counts, so it is
byte-identical for serial, pooled, cold and warm runs of the same plan;
compare ``results_sha256``. ``inconclusive`` (a job passed the per-pair limit;
its child process was killed) is the one status that depends on wall time.
Inconclusive outcomes are never cached.

Mechanisms (spec section 6.4): rigid states re-place shapes without rebuild;
``param`` states rebuild only the subject, once per distinct effective
parameter set, cached in memory and (when the build carried a valid
``input_digest``) on disk; a bounding-box broad phase precedes every boolean;
job results are cached by (geometry hash, exact transform, primitive) in
memory and persisted under ``<cache>/requirements``; jobs run in forked
processes (never threads), ``BRepAlgoAPI_*`` with ``SetRunParallel(False)``,
reduced in plan order.
"""
from __future__ import annotations

import hashlib
import importlib.metadata
import json
import math
import multiprocessing as mp
import os
import shutil
import signal
import sys
import time
import uuid
from collections import OrderedDict, deque
from multiprocessing import connection as mpconn
from pathlib import Path

import cadquery as cq

from .errors import DomainError

FORMAT = "requirements-eval-v1"
DEFAULT_PAIR_LIMIT_S = 20.0
DEFAULT_CHUNK_LIMIT_S = 120.0
MAX_STATES = 4096
MAX_MEASUREMENTS = 50000
MAX_RESULT_ENTRIES = 200000
MAX_REBUILD_DIRS = 64
MAX_REBUILD_MEMORY = 16
MAX_JOB_TIMINGS = 500
PRIMITIVES = (
    "interference", "contact_area", "min_clearance", "distance", "extent", "outside_volume", "valid_solid_count",
    "diameter", "radius", "length", "area", "volume", "gap", "clearance_along",
    "overlap_depth", "axis_offset", "axis_angle", "profile_centre", "profile_size",
    "diameter_difference", "thread_protrusion",
)
# Primitives whose ``a``/``b`` may name bound features: a part name or
# ``{"part": name, "feature": {"kind": ..., ...}}``.
_ENTITY_PRIMITIVES = (
    "diameter", "radius", "length", "area", "volume", "gap", "clearance_along",
    "overlap_depth", "axis_offset", "axis_angle", "profile_centre", "profile_size",
    "diameter_difference", "thread_protrusion",
)
_ENTITY_WITH_B = (
    "gap", "clearance_along", "overlap_depth", "axis_offset", "axis_angle",
    "profile_centre", "profile_size", "diameter_difference",
)
# Clearance along a direction where nothing is ahead is unbounded; reported as
# this finite sentinel so results stay JSON-serialisable and cacheable.
_INF_CLEARANCE = 1.0e9
MAX_SAMPLE_POINTS = 20000
pc = time.perf_counter


class ChunkCancelled(Exception):
    pass


# ---------------------------------------------------------------- utilities

def _invalid(message):
    return DomainError("INVALID_REQUEST", message)


def _engine():
    def version(*names):
        for name in names:
            try:
                return importlib.metadata.version(name)
            except importlib.metadata.PackageNotFoundError:
                pass
        return None
    return dict(format=FORMAT, cadquery=version("cadquery"),
                ocp=version("cadquery-ocp-novtk", "cadquery-ocp"), occ_parallel=False)


def _engine_id():
    engine = dict(_engine(), python=sys.version)
    return hashlib.sha256(json.dumps(engine, sort_keys=True).encode()).hexdigest()[:16]


def _canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)


def _hex(value):
    return float(value + 0.0).hex()


def _trsf_key(location):
    t = location.wrapped.Transformation()
    return ",".join(_hex(t.Value(r, c)) for r in (1, 2, 3) for c in (1, 2, 3, 4))


def _number(value, what):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise _invalid(f"{what} must be a finite number")
    return float(value)


def _vec3(value, what):
    if not isinstance(value, (list, tuple)) or len(value) != 3:
        raise _invalid(f"{what} must be three numbers")
    return [_number(v, what) for v in value]


def _names(value, what):
    if isinstance(value, str):
        value = [value]
    if (not isinstance(value, list) or not value or len(value) > 512
            or any(not isinstance(v, str) or not v for v in value)):
        raise _invalid(f"{what} must be a nonempty list of part names")
    return sorted(set(value))


def _feature_label(desc):
    kind = desc.get("kind") if isinstance(desc, dict) else None
    if kind in ("explicit", "history"):
        return str(desc.get("name") or kind)
    if kind == "geometric":
        return f"{desc.get('entity', 'entity')}:{str(desc.get('hash', ''))[:12]}"
    return "feature"


def _entity_refs(value, what):
    """``a``/``b`` of feature-aware primitives: part names or feature objects."""
    if isinstance(value, str):
        value = [value]
    if not isinstance(value, list) or not value or len(value) > 512:
        raise _invalid(f"{what} must be a nonempty list of parts or features")
    out, seen = [], set()
    for item in value:
        if isinstance(item, str):
            ref = dict(part=item, feature=None, label=item)
        elif isinstance(item, dict):
            part, feature = item.get("part"), item.get("feature")
            if not isinstance(part, str) or not part:
                raise _invalid(f"{what} entries need a part name")
            if feature is not None and not isinstance(feature, dict):
                raise _invalid(f"{what} feature must be an object")
            label = part if feature is None else f"{part}#{_feature_label(feature)}"
            ref = dict(part=part, feature=feature, label=label)
        else:
            raise _invalid(f"{what} entries must be part names or feature objects")
        key = (ref["part"], _canonical(ref["feature"]) if ref["feature"] else "")
        if key not in seen:
            seen.add(key)
            out.append(ref)
    out.sort(key=lambda r: (r["part"], _canonical(r["feature"]) if r["feature"] else ""))
    return out


# ---------------------------------------------------------------- jobs (run in children)

def _common_volume(a, b):
    from OCP.BRepAlgoAPI import BRepAlgoAPI_Common
    from OCP.TopTools import TopTools_ListOfShape
    op = BRepAlgoAPI_Common()
    left, right = TopTools_ListOfShape(), TopTools_ListOfShape()
    left.Append(a.wrapped)
    right.Append(b.wrapped)
    op.SetArguments(left)
    op.SetTools(right)
    # cadquery's Shape.intersect hard-codes parallel on; phase 0 found it slower
    # on typical pairs, so the evaluator always runs the boolean serially.
    op.SetRunParallel(False)
    op.Build()
    return _solid_volume(op)


def _contact_area(a, b):
    """Exact area of coincident faces, excluding positive-volume overlap."""
    from OCP.BRepAlgoAPI import BRepAlgoAPI_Common
    from OCP.TopTools import TopTools_ListOfShape

    def common(left_shape, right_shape):
        op = BRepAlgoAPI_Common()
        left, right = TopTools_ListOfShape(), TopTools_ListOfShape()
        left.Append(left_shape.wrapped)
        right.Append(right_shape.wrapped)
        op.SetArguments(left)
        op.SetTools(right)
        op.SetRunParallel(False)
        op.Build()
        if not op.IsDone():
            raise DomainError("INVALID_GEOMETRY", "BRep contact-area boolean failed")
        return cq.Shape.cast(op.Shape())

    common_solids = common(a, b)
    # A solid in the common means the bodies overlap volumetrically; its
    # boundary faces are not a bearing contact and must not count as contact.
    if any(solid.Volume() > 0.0 for solid in common_solids.Solids()):
        return 0.0

    def face_key(face):
        box = face.BoundingBox()
        return (box.xmin, box.ymin, box.zmin, box.xmax, box.ymax, box.zmax, face.Area())

    def bounds_overlap(left, right):
        a_box, b_box = left.BoundingBox(), right.BoundingBox()
        return not (a_box.xmin > b_box.xmax or b_box.xmin > a_box.xmax
                    or a_box.ymin > b_box.ymax or b_box.ymin > a_box.ymax
                    or a_box.zmin > b_box.zmax or b_box.zmin > a_box.zmax)

    faces_a = sorted(a.Faces(), key=face_key)
    faces_b = sorted(b.Faces(), key=face_key)
    areas = []
    for face_a in faces_a:
        for face_b in faces_b:
            if not bounds_overlap(face_a, face_b):
                continue
            shared = common(face_a, face_b)
            areas.extend(float(face.Area()) for face in shared.Faces())
    areas.sort()
    if any(not math.isfinite(area) for area in areas):
        raise DomainError("INVALID_GEOMETRY", "BRep contact area is not finite")
    return math.fsum(areas)


def _cut_volume(a, tools):
    from OCP.BRepAlgoAPI import BRepAlgoAPI_Cut
    from OCP.TopTools import TopTools_ListOfShape
    op = BRepAlgoAPI_Cut()
    left, right = TopTools_ListOfShape(), TopTools_ListOfShape()
    left.Append(a.wrapped)
    for tool in tools:
        right.Append(tool.wrapped)
    op.SetArguments(left)
    op.SetTools(right)
    op.SetRunParallel(False)
    op.Build()
    return _solid_volume(op)


def _solid_volume(op):
    if not op.IsDone():
        raise DomainError("INVALID_GEOMETRY", "BRep boolean failed")
    result = cq.Shape.cast(op.Shape())
    volume = float(sum(solid.Volume() for solid in result.Solids()))
    if not math.isfinite(volume):
        raise DomainError("INVALID_GEOMETRY", "BRep boolean produced a non-finite volume")
    return volume


def _distance(a, b):
    from OCP.BRepExtrema import BRepExtrema_DistShapeShape
    calc = BRepExtrema_DistShapeShape(a.wrapped, b.wrapped)
    calc.SetMultiThread(False)
    if not calc.IsDone():
        raise DomainError("INVALID_GEOMETRY", "Distance computation failed")
    value = float(calc.Value())
    if not math.isfinite(value):
        raise DomainError("INVALID_GEOMETRY", "Distance is not finite")
    return value


def _validity(shape):
    solids = shape.Solids()
    return [sum(1 for solid in solids if solid.isValid()), len(solids)]


# ------------------------------------------------------- feature measurements

def _common_shape(a, b):
    from OCP.BRepAlgoAPI import BRepAlgoAPI_Common
    from OCP.TopTools import TopTools_ListOfShape
    op = BRepAlgoAPI_Common()
    left, right = TopTools_ListOfShape(), TopTools_ListOfShape()
    left.Append(a.wrapped)
    right.Append(b.wrapped)
    op.SetArguments(left)
    op.SetTools(right)
    op.SetRunParallel(False)
    op.Build()
    if not op.IsDone():
        raise DomainError("INVALID_GEOMETRY", "BRep boolean failed")
    return cq.Shape.cast(op.Shape())


def _overlap_extent(a, b, direction):
    """Extent of the volumetric intersection of a and b projected on direction."""
    common = _common_shape(a, b)
    volume = float(sum(solid.Volume() for solid in common.Solids()))
    if volume <= 0.0:
        return 0.0
    points = _shape_samples(common, _default_step([common]))
    low, high = _project_interval(points, direction)
    return high - low


def _vec_add(a, b):
    return [a[i] + b[i] for i in range(3)]


def _vec_sub(a, b):
    return [a[i] - b[i] for i in range(3)]


def _vec_scale(a, s):
    return [a[i] * s for i in range(3)]


def _vec_dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def _vec_cross(a, b):
    return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]


def _vec_norm(a):
    return math.sqrt(_vec_dot(a, a))


def _unit(vector):
    n = _vec_norm(vector)
    if n < 1e-12 or not math.isfinite(n):
        raise DomainError("INVALID_REQUEST", "direction must be a nonzero finite vector")
    return [v / n for v in vector]


def _shape_samples(shape, step):
    """Deterministic samples of a shape's boundary: vertices, edge and face grids."""
    from OCP.BRepAdaptor import BRepAdaptor_Curve, BRepAdaptor_Surface
    from OCP.BRepClass import BRepClass_FaceClassifier
    from OCP.BRepTools import BRepTools
    from OCP.GCPnts import GCPnts_UniformAbscissa
    from OCP.Precision import Precision
    from OCP.TopAbs import TopAbs_IN, TopAbs_ON
    from OCP.gp import gp_Pnt

    points = []
    for vertex in shape.Vertices():
        c = vertex.Center().toTuple()
        points.append((float(c[0]), float(c[1]), float(c[2])))
    for edge in shape.Edges():
        try:
            length = float(edge.Length())
        except Exception:  # noqa: BLE001 - degenerate edges contribute vertices only
            continue
        count = int(min(65, max(2, math.ceil(length / step)))) if step > 0 else 9
        try:
            curve = BRepAdaptor_Curve(edge.wrapped)
            disc = GCPnts_UniformAbscissa(curve, count)
            if disc.IsDone():
                for i in range(1, disc.NbPoints() + 1):
                    p = curve.Value(disc.Parameter(i))
                    points.append((p.X(), p.Y(), p.Z()))
        except Exception:  # noqa: BLE001 - unbounded curves skip discretisation
            continue
    for face in shape.Faces():
        box = face.BoundingBox()
        extent = max(box.xmax - box.xmin, box.ymax - box.ymin, box.zmax - box.zmin, step)
        count = int(min(33, max(2, math.ceil(extent / step)))) if step > 0 else 5
        try:
            umin, umax, vmin, vmax = BRepTools.UVBounds_s(face.wrapped)
        except Exception:  # noqa: BLE001
            continue
        if not all(math.isfinite(v) for v in (umin, umax, vmin, vmax)) or umax <= umin or vmax <= vmin:
            continue
        try:
            surface = BRepAdaptor_Surface(face.wrapped)
            tol = Precision.Confusion_s()
            for i in range(count + 1):
                u = umin + (umax - umin) * i / count
                for j in range(count + 1):
                    v = vmin + (vmax - vmin) * j / count
                    p = surface.Value(u, v)
                    if not isinstance(p, gp_Pnt):
                        continue
                    state = BRepClass_FaceClassifier(face.wrapped, p, tol).State()
                    if state in (TopAbs_IN, TopAbs_ON):
                        points.append((p.X(), p.Y(), p.Z()))
        except Exception:  # noqa: BLE001 - faces that resist sampling still give vertices/edges
            continue
    return points


def _default_step(shapes):
    """Sampling step derived from the bounding box when the claim gives none."""
    low = [math.inf] * 3
    high = [-math.inf] * 3
    for shape in shapes:
        box = shape.BoundingBox()
        for i, (a, b) in enumerate(((box.xmin, box.xmax), (box.ymin, box.ymax), (box.zmin, box.zmax))):
            low[i] = min(low[i], a)
            high[i] = max(high[i], b)
    extent = max(high[i] - low[i] for i in range(3)) if all(math.isfinite(v) for v in low + high) else 10.0
    return max(extent / 24.0, 0.05)


def _entity_cloud(entities, step):
    """Merged deterministic samples of a set of entities, capped in size."""
    points = []
    current = step
    for _ in range(5):
        points = []
        for ent in entities:
            points.extend(_shape_samples(ent.shape, current))
        if len(points) <= MAX_SAMPLE_POINTS:
            return points
        current *= 2.0
    return points[:MAX_SAMPLE_POINTS]


def _project_interval(points, direction):
    d = _unit(direction)
    values = [p[0] * d[0] + p[1] * d[1] + p[2] * d[2] for p in points]
    if not values:
        raise DomainError("INVALID_GEOMETRY", "entity has no measurable extent")
    return min(values), max(values)


def _max_cloud_distance(a_points, b_points):
    """Largest sampled point-to-point distance between two clouds."""
    import numpy as np
    if not a_points or not b_points:
        return 0.0
    a = np.asarray(a_points, dtype=float)
    b = np.asarray(b_points, dtype=float)
    best = 0.0
    for start in range(0, len(a), 256):
        chunk = a[start:start + 256]
        dist = np.sqrt(((chunk[:, None, :] - b[None, :, :]) ** 2).sum(axis=2))
        best = max(best, float(dist.max()))
    return best


def _compound(shapes):
    if len(shapes) == 1:
        return shapes[0]
    return cq.Compound.makeCompound(list(shapes))


def _entity_cost(*entities):
    product = 1.0
    for ent in entities:
        box = ent.shape.BoundingBox()
        size = math.sqrt((box.xmax - box.xmin) ** 2 + (box.ymax - box.ymin) ** 2
                         + (box.zmax - box.zmin) ** 2)
        product *= max(size, 1e-6)
    return product


def _run_job(task):
    kind = task[0]
    if kind == "common":
        return _common_volume(task[1], task[2])
    if kind == "contact_area":
        return _contact_area(task[1], task[2])
    if kind == "cut":
        return _cut_volume(task[1], task[2])
    if kind == "distance":
        return _distance(task[1], task[2])
    if kind == "validity":
        return _validity(task[1])
    if kind == "overlap":
        return _overlap_extent(task[1], task[2], task[3])
    raise DomainError("INVALID_REQUEST", f"Unknown job {kind}")


# ---------------------------------------------------------------- fork pool with kill-on-timeout

_ACTIVE = None  # (fn, tasks) inherited by forked children


class Outcome:
    __slots__ = ("status", "value", "seconds")

    def __init__(self, status, value, seconds):
        self.status, self.value, self.seconds = status, value, seconds


_FORK_PROBE = None


def _fork_permitted():
    """Whether this process may fork (the production sandbox denies it).

    Probed once with a throwaway child; without fork the evaluator runs its
    tasks in-process, serially, and judges task limits after the fact.
    """
    global _FORK_PROBE
    if _FORK_PROBE is None:
        try:
            pid = os.fork()
        except OSError:
            _FORK_PROBE = False
        else:
            if pid == 0:
                os._exit(0)
            os.waitpid(pid, 0)
            _FORK_PROBE = True
    return _FORK_PROBE


def _fork_available(workers):
    return (workers > 0 and "fork" in mp.get_all_start_methods() and hasattr(signal, "setitimer")
            and _fork_permitted())


def _single_threaded_occt():
    """Make OCCT run on the calling thread only.

    The parent has usually already used OCCT's default thread pool (parallel
    booleans while building the model). A forked child inherits the pool object
    but none of its threads, so the first parallel operation would wait forever
    for workers that do not exist. Children never parallelise internally; the
    process pool is their parallelism.
    """
    try:
        from OCP.OSD import OSD_ThreadPool
        # Re-initialising to one thread (the caller) replaces the dead pool;
        # only this works. Limiting launched threads or disabling OCCT threads
        # still waits on the missing workers.
        OSD_ThreadPool.DefaultPool_s().Init(1)
    except Exception:  # noqa: BLE001 - best effort; older kernels lack the pool API
        pass


def _child_loop(conn, alarm_s):
    _single_threaded_occt()
    signal.signal(signal.SIGALRM, signal.SIG_DFL)
    fn, tasks = _ACTIVE
    while True:
        try:
            index = conn.recv()
        except (EOFError, OSError):
            return
        if index is None:
            return
        # Safety net against orphans (a parent killed mid-job): the kernel's
        # default SIGALRM action ends us even inside native OCC code.
        signal.setitimer(signal.ITIMER_REAL, alarm_s)
        start = pc()
        try:
            message = ("ok", fn(tasks[index]))
        except BaseException as error:  # noqa: BLE001 - reported, never raised across the pipe
            message = ("error", dict(code=getattr(error, "code", "EVAL_FAILED"),
                                     message=f"{type(error).__name__}: {error}"[:500]))
        elapsed = pc() - start
        signal.setitimer(signal.ITIMER_REAL, 0)
        try:
            conn.send((index, message[0], message[1], elapsed))
        except (OSError, ValueError):
            return


class _Slot:
    def __init__(self, ctx, alarm_s):
        self.conn, child = ctx.Pipe()
        self.proc = ctx.Process(target=_child_loop, args=(child, alarm_s), daemon=True)
        self.proc.start()
        child.close()
        self.task = None
        self.started = 0.0

    def kill(self):
        try:
            self.proc.kill()
            self.proc.join(5)
        finally:
            self.conn.close()


def run_tasks(fn, tasks, *, workers, task_limit, deadline, order=None):
    """Run ``fn(task)`` for every task; returns Outcome list indexed like ``tasks``.

    Outcomes: ``ok`` (value), ``error`` (dict code/message), ``timeout`` (task
    passed ``task_limit``; its process was killed). Raises ChunkCancelled when
    ``deadline`` (perf_counter time) passes, after killing every child.
    """
    global _ACTIVE
    outcomes = [None] * len(tasks)
    order = list(range(len(tasks))) if order is None else list(order)
    if not tasks:
        return outcomes
    if not _fork_available(workers):
        for index in order:
            if pc() > deadline:
                raise ChunkCancelled()
            start = pc()
            try:
                outcome = Outcome("ok", fn(tasks[index]), 0.0)
            except Exception as error:  # noqa: BLE001
                outcome = Outcome("error", dict(code=getattr(error, "code", "EVAL_FAILED"),
                                                message=f"{type(error).__name__}: {error}"[:500]), 0.0)
            outcome.seconds = pc() - start
            # No way to interrupt native code in-process: the limit is judged afterwards.
            if task_limit is not None and outcome.seconds > task_limit:
                outcome = Outcome("timeout", None, outcome.seconds)
            outcomes[index] = outcome
        return outcomes
    ctx = mp.get_context("fork")
    alarm_s = (task_limit if task_limit is not None else max(1.0, deadline - pc())) + 10.0
    _ACTIVE = (fn, tasks)
    pending = deque(order)
    slots = [None] * min(workers, len(tasks))
    try:
        while pending or any(slot is not None and slot.task is not None for slot in slots):
            now = pc()
            if now > deadline:
                raise ChunkCancelled()
            for i, slot in enumerate(slots):
                if pending and (slot is None or slot.task is None):
                    if slot is None:
                        slot = slots[i] = _Slot(ctx, alarm_s)
                    slot.task = pending.popleft()
                    slot.started = pc()
                    slot.conn.send(slot.task)
            busy = [slot for slot in slots if slot is not None and slot.task is not None]
            wake = deadline
            if task_limit is not None:
                wake = min([deadline] + [slot.started + task_limit for slot in busy])
            ready = mpconn.wait([slot.conn for slot in busy], timeout=max(0.0, min(wake - pc(), 1.0)))
            for slot in busy:
                index = slot.task
                if slot.conn in ready:
                    try:
                        _, status, value, seconds = slot.conn.recv()
                        outcomes[index] = Outcome(status, value, seconds)
                        slot.task = None
                    except (EOFError, OSError):
                        slot.proc.join(1)
                        outcomes[index] = Outcome("error", dict(
                            code="EVAL_FAILED",
                            message=f"worker process died (exit code {slot.proc.exitcode})"), pc() - slot.started)
                        slot.kill()
                        slots[slots.index(slot)] = None
                elif task_limit is not None and pc() - slot.started >= task_limit:
                    outcomes[index] = Outcome("timeout", None, pc() - slot.started)
                    slot.kill()
                    slots[slots.index(slot)] = None
    except BaseException:
        for slot in slots:
            if slot is not None:
                slot.kill()
        raise
    finally:
        _ACTIVE = None
    for slot in slots:
        if slot is not None:
            try:
                slot.conn.send(None)
            except (OSError, ValueError):
                pass
            slot.proc.join(2)
            if slot.proc.is_alive():
                slot.proc.kill()
                slot.proc.join(5)
            slot.conn.close()
    return outcomes


# ---------------------------------------------------------------- result cache

class ResultCache:
    """(geometry hash, exact transform, primitive) -> raw value; persisted as JSON."""

    def __init__(self, directory):
        self.directory = directory
        self.entries = {}
        self.new = {}
        self.loaded = False

    @property
    def path(self):
        return self.directory / "requirements" / f"results-{_engine_id()}.json"

    def load(self):
        if self.loaded:
            return
        self.loaded = True
        try:
            data = json.loads(self.path.read_text())
            if data.get("format") == FORMAT and data.get("engine") == _engine_id() and isinstance(data.get("entries"), dict):
                self.entries.update(data["entries"])
        except (OSError, ValueError, AttributeError):
            pass

    def get(self, key):
        return self.entries.get(key)

    def put(self, key, value):
        self.entries[key] = value
        self.new[key] = value

    def persist(self):
        if not self.new:
            return
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            merged = {}
            try:
                data = json.loads(self.path.read_text())
                if data.get("format") == FORMAT and data.get("engine") == _engine_id():
                    merged.update(data["entries"])
            except (OSError, ValueError, AttributeError, KeyError):
                pass
            merged.update(self.new)
            while len(merged) > MAX_RESULT_ENTRIES:
                merged.pop(next(iter(merged)))
            temporary = self.path.parent / f".{uuid.uuid4().hex}.tmp"
            try:
                temporary.write_text(json.dumps(dict(format=FORMAT, engine=_engine_id(), entries=merged),
                                                allow_nan=False))
                os.replace(temporary, self.path)
            finally:
                temporary.unlink(missing_ok=True)
            self.new = {}
        except OSError:
            pass  # a read-only cache directory only costs speed


# ---------------------------------------------------------------- rebuild cache

class RPart:
    __slots__ = ("name", "shape", "position", "hash", "features", "fastener")

    def __init__(self, name, shape, position, digest, features=None, fastener=None):
        self.name, self.shape, self.position, self.hash = name, shape, tuple(position), digest
        # {"explicit": {name: {"faces": [fp], "edges": [fp]}}, "history": resolver|None}
        self.features = features or {}
        # Published fastener metadata (kind/size/length/mates); state-invariant.
        self.fastener = fastener


def _purge_project_modules(source_root):
    root = Path(source_root).resolve()
    for name, module in list(sys.modules.items()):
        path = getattr(module, "__file__", None)
        if not path or name == "__main__":
            continue
        try:
            if Path(path).resolve().is_relative_to(root):
                del sys.modules[name]
        except (OSError, ValueError):
            continue
    import importlib
    importlib.invalidate_caches()


def _explicit_feature_index(part):
    """Published feature name -> entity fingerprints, for exact re-resolution."""
    from .model import geometry_fingerprint
    index = {}
    if not getattr(part, "features", None):
        return index
    faces, edges = part.shape.Faces(), part.shape.Edges()
    for name, selected in part.features.items():
        selected = selected.vals() if hasattr(selected, "vals") else [selected]
        entry = {"faces": [], "edges": []}
        for item in selected:
            for kind, entities in (("faces", faces), ("edges", edges)):
                if any(item.isSame(entity) for entity in entities):
                    entry[kind].append(
                        geometry_fingerprint(next(e for e in entities if item.isSame(e))))
        index[name] = {k: sorted(v) for k, v in entry.items() if v}
    return index


def _history_feature_members(model, part_name, feature_name):
    """History feature name -> [(kind, shape)] on the traced base build."""
    try:
        model._trace_history_names()
    except Exception:
        return None
    names = model._history_names or {}
    part = model.parts.get(part_name)
    if part is None:
        return []
    members = []
    for kind, by_index in names.get(part_name, {}).items():
        items = part.shape.Faces() if kind == "face" else part.shape.Edges()
        for entity_index, entity_names in by_index.items():
            if feature_name in entity_names and entity_index < len(items):
                members.append((kind, items[entity_index]))
    return members


def _feature_entities(rpart, desc):
    """Resolve a feature descriptor to [(kind, part-local shape)] on this state's part."""
    from .model import DomainError, geometry_fingerprint
    kind = desc.get("kind")
    if kind == "explicit":
        name = desc.get("name")
        entry = (rpart.features.get("explicit") or {}).get(name)
        if entry is None:
            raise DomainError("BIND_FEATURE", f"{rpart.name} has no published feature {name!r}")
        found = []
        for entity_kind, key in (("face", "faces"), ("edge", "edges")):
            wanted = set(entry.get(key, []))
            if not wanted:
                continue
            entities = rpart.shape.Faces() if entity_kind == "face" else rpart.shape.Edges()
            for entity in entities:
                if geometry_fingerprint(entity) in wanted:
                    found.append((entity_kind, entity))
        if not found:
            raise DomainError("BIND_FEATURE", f"Feature {name!r} of {rpart.name} no longer matches")
        return found
    if kind == "geometric":
        entity_kind, expected = desc.get("entity"), desc.get("hash")
        if entity_kind not in ("face", "edge") or not isinstance(expected, str):
            raise DomainError("INVALID_REQUEST", "Geometric feature needs face/edge and exact hash")
        entities = rpart.shape.Faces() if entity_kind == "face" else rpart.shape.Edges()
        found = [e for e in entities if geometry_fingerprint(e) == expected]
        if len(found) != 1:
            raise DomainError("BIND_FEATURE", f"Geometric feature of {rpart.name} resolves to {len(found)} entities")
        return [(entity_kind, found[0])]
    if kind == "history":
        resolver = rpart.features.get("history")
        if resolver is None:
            raise DomainError("UNSUPPORTED_OPERATION",
                              "history features need the traced base build, not a rebuilt state")
        members = resolver(desc.get("name"))
        if members is None:
            raise DomainError("HISTORY_TRACE_UNAVAILABLE", "History tracing is unavailable for this build")
        if not members:
            raise DomainError("BIND_FEATURE", f"{rpart.name} has no history feature {desc.get('name')!r}")
        return members
    raise DomainError("INVALID_REQUEST", "Feature reference kind must be explicit, history or geometric")


def _entity_axis(shape):
    """(point, direction) of a face/edge axis in the entity's own coordinates."""
    from OCP.BRepAdaptor import BRepAdaptor_Curve, BRepAdaptor_Surface
    from OCP.gp import gp_Ax1
    kind = shape.ShapeType()
    surface = BRepAdaptor_Surface(shape.wrapped) if kind == "Face" else None
    if kind == "Face" and shape.geomType() == "CYLINDER":
        cyl = surface.Cylinder()
        point, axis = cyl.Location(), cyl.Axis()
    elif kind == "Face" and shape.geomType() == "PLANE":
        plane = surface.Plane()
        point, axis = plane.Location(), plane.Axis()
    elif kind == "Edge" and shape.geomType() == "CIRCLE":
        circle = BRepAdaptor_Curve(shape.wrapped).Circle()
        point, axis = circle.Location(), circle.Axis()
    elif kind == "Edge" and shape.geomType() == "LINE":
        line = BRepAdaptor_Curve(shape.wrapped).Line()
        point, axis = line.Location(), gp_Ax1(line.Location(), line.Direction())
    else:
        raise DomainError("UNSUPPORTED_OPERATION",
                          "feature axis needs a cylinder face, circle edge, plane face or line edge")
    direction = axis.Direction()
    p = point.Coord() if hasattr(point, "Coord") else (point.X(), point.Y(), point.Z())
    return ([p[0], p[1], p[2]], [direction.X(), direction.Y(), direction.Z()])


def _rebuild_task(task):
    """Runs in a child: rebuild the subject with new parameters, write BREP files."""
    from .geometry_cache import encode, decode
    from .model import Model, geometry_fingerprint
    args, revision, cache_dir, target = task
    _purge_project_modules(args["source_root"])
    model = Model(Path(cache_dir))
    model.build(args, revision)
    partial = Path(str(target) + f".partial-{os.getpid()}-{uuid.uuid4().hex[:8]}")
    partial.mkdir(parents=True)
    entries = []
    for index, (name, part) in enumerate(model.parts.items()):
        data = encode(part.shape)
        restored = decode(data)  # publish the canonical representation on hit and miss alike
        handle = f"p{index}.brep"
        (partial / handle).write_bytes(data)
        features = {"explicit": _explicit_feature_index(part), "history": None}
        entries.append(dict(name=name, handle=handle, sha256=hashlib.sha256(data).hexdigest(),
                            position=list(part.position), geometry_sha256=geometry_fingerprint(restored),
                            features=features, fastener=part.fastener))
    (partial / "manifest.json").write_text(json.dumps(dict(format=FORMAT, engine=_engine_id(), parts=entries)))
    try:
        os.rename(partial, target)
    except OSError:
        shutil.rmtree(partial, ignore_errors=True)  # concurrent writer won; its copy is identical
        if not (Path(target) / "manifest.json").exists():
            raise
    return str(target)


def _load_rebuilt(directory):
    from .geometry_cache import decode
    try:
        directory = Path(directory)
        manifest = json.loads((directory / "manifest.json").read_text())
        if manifest.get("format") != FORMAT or manifest.get("engine") != _engine_id():
            return None
        parts = {}
        for entry in manifest["parts"]:
            data = (directory / entry["handle"]).read_bytes()
            if hashlib.sha256(data).hexdigest() != entry["sha256"]:
                return None
            shape = decode(data)
            if shape.isNull():
                return None
            parts[entry["name"]] = RPart(entry["name"], shape, entry["position"], entry["geometry_sha256"],
                                         entry.get("features"), entry.get("fastener"))
        return parts
    except (OSError, ValueError, KeyError, TypeError, RuntimeError):
        return None


class EvalState:
    """Per-Model caches (one immutable revision per worker process)."""

    def __init__(self, model):
        self.results = ResultCache(model.cache)
        self.rebuilds = OrderedDict()   # key -> {name: RPart}
        self.base = None

    def base_parts(self, model):
        from .model import geometry_fingerprint
        if self.base is None:
            parts = {}
            for name, part in model.parts.items():
                if part.fingerprint is None:
                    part.fingerprint = geometry_fingerprint(part.shape)
                parts[name] = RPart(name, part.shape, part.position, part.fingerprint,
                                    {"explicit": _explicit_feature_index(part),
                                     "history": (lambda feature_name, _m=model, _p=name:
                                                 _history_feature_members(_m, _p, feature_name))},
                                    part.fastener)
            self.base = parts
        return self.base


# ---------------------------------------------------------------- placement

def _axis_vector(axis):
    named = dict(x=[1.0, 0.0, 0.0], y=[0.0, 1.0, 0.0], z=[0.0, 0.0, 1.0])
    vector = named.get(axis) if isinstance(axis, str) else _vec3(axis, "axis")
    if vector is None or math.sqrt(sum(v * v for v in vector)) < 1e-12:
        raise _invalid("axis must be x, y, z or a nonzero vector")
    return vector


def _axis_spec(axis):
    """axis JSON: 'x'/'y'/'z', a vector, or {"feature": desc, "on": part}."""
    if isinstance(axis, dict) and "feature" in axis:
        return axis
    return _axis_vector(axis)


def _parse_moves(moves):
    if not isinstance(moves, list) or len(moves) > 256:
        raise _invalid("moves must be a list")
    parsed = []
    for move in moves:
        if not isinstance(move, dict) or move.get("op") not in ("rotate", "translate"):
            raise _invalid("move op must be rotate or translate")
        names = set(_names(move.get("parts"), "move parts"))
        if move["op"] == "rotate":
            parsed.append(dict(op="rotate", parts=names, axis=_axis_spec(move.get("axis")),
                               origin=_vec3(move.get("origin", [0, 0, 0]), "origin"),
                               angle=_number(move.get("angle_deg"), "angle_deg")))
        else:
            vector = move.get("vector")
            if isinstance(vector, dict) and "feature" in vector:
                parsed.append(dict(op="translate", parts=names, vector=dict(
                    feature=vector["feature"], on=vector.get("on"),
                    distance=_number(vector.get("distance"), "vector.distance"))))
            else:
                parsed.append(dict(op="translate", parts=names, vector=_vec3(vector, "vector")))
    return parsed


def _move_location(move):
    from OCP.gp import gp_Ax1, gp_Dir, gp_Pnt, gp_Trsf, gp_Vec
    trsf = gp_Trsf()
    if move["op"] == "rotate":
        trsf.SetRotation(gp_Ax1(gp_Pnt(*move["origin"]), gp_Dir(*move["axis"])), math.radians(move["angle"]))
    else:
        trsf.SetTranslation(gp_Vec(*move["vector"]))
    return cq.Location(trsf)


def _scene_axis(scene, part_name, desc):
    """World axis of a feature descriptor on one part of the scene."""
    part = scene.parts.get(part_name)
    if part is None:
        raise DomainError("UNKNOWN_PART", f"Unknown part {part_name}")
    from OCP.gp import gp_XYZ
    axes = set()
    for kind, shape in _feature_entities(part, desc):
        point, direction = _entity_axis(shape)
        trsf = shape.wrapped.Location().Transformation()

        def world(x, y, z):
            coord = gp_XYZ(x, y, z)
            trsf.Transforms(coord)
            return coord.Coord()

        wp = world(*point)
        tip = world(point[0] + direction[0], point[1] + direction[1], point[2] + direction[2])
        world_point = wp
        world_dir = tuple(tip[i] - wp[i] for i in range(3))
        axes.add((tuple(round(v, 9) for v in world_dir),
                  tuple(round(v, 9) for v in world_point)))
    if not axes:
        raise DomainError("UNSUPPORTED_OPERATION", "feature has no measurable axis")
    if len({a for a, _ in axes}) > 1 or len({p for _, p in axes}) > 1:
        raise DomainError("UNSUPPORTED_OPERATION", "feature members give different axes")
    direction, point = next(iter(axes))
    return {"point": list(point), "direction": list(direction)}


class Placed:
    __slots__ = ("name", "shape", "key", "_box", "hash")

    def __init__(self, name, shape, digest, trsf):
        self.name, self.shape, self.hash = name, shape, digest
        self.key = f"{digest}:{trsf}"
        self._box = None

    def box(self):
        """World bounds padded by the stored tolerance, as in interference inspection."""
        if self._box is None:
            from OCP.Precision import Precision
            from OCP.ShapeAnalysis import ShapeAnalysis_ShapeTolerance
            from .model import bounds
            value = bounds(self.shape)
            # Stored tolerances and solid-ness depend on geometry only; sweeps re-place the same parts.
            tolerance = _TOLERANCE.get(self.hash)
            if tolerance is None:
                tolerance = _TOLERANCE[self.hash] = max(
                    Precision.Confusion_s(), ShapeAnalysis_ShapeTolerance().Tolerance(self.shape.wrapped, 1))
            scale = max(abs(v) for side in ("min", "max") for v in value[side])
            margin = 2 * tolerance + 16 * sys.float_info.epsilon * scale
            self._box = dict(raw=value, min=[v - margin for v in value["min"]], max=[v + margin for v in value["max"]])
        return self._box


_TOLERANCE = {}
_HAS_SOLIDS = {}


def _disjoint(a, b):
    ba, bb = a.box(), b.box()
    return any(ba["min"][i] > bb["max"][i] or bb["min"][i] > ba["max"][i] for i in range(3))


class Scene:
    """Parts of one state at their world placement, built lazily."""

    def __init__(self, parts, moves):
        self.parts, self.moves = parts, moves
        self._locations = {}
        self.placed = {}

    def _move_location(self, move, part_name):
        key = (id(move), part_name)
        if key not in self._locations:
            resolved = dict(move)
            if isinstance(move.get("axis"), dict):
                spec = move["axis"]
                feature_axis = _scene_axis(self, spec.get("on") or part_name, spec["feature"])
                resolved["axis"] = feature_axis["direction"]
                resolved["origin"] = feature_axis["point"]
            if isinstance(move.get("vector"), dict):
                spec = move["vector"]
                feature_axis = _scene_axis(self, spec.get("on") or part_name, spec["feature"])
                direction = feature_axis["direction"]
                distance = abs(move["vector"]["distance"])
                sign = 1.0 if move["vector"]["distance"] >= 0 else -1.0
                resolved["vector"] = [v * distance * sign for v in direction]
            self._locations[key] = _move_location(resolved)
        return self._locations[key]

    def _location(self, name):
        """World placement of a part: published position composed with moves."""
        if name not in self.parts:
            raise DomainError("UNKNOWN_PART", f"Unknown part {name}")
        part = self.parts[name]
        from .model import Model
        location = Model.location(dict(position=list(part.position)))
        for move in self.moves:
            if name in move["parts"]:
                location = self._move_location(move, name) * location
        return location

    def get(self, name):
        if name not in self.placed:
            location = self._location(name)
            part = self.parts[name]
            self.placed[name] = Placed(name, part.shape.moved(location), part.hash, _trsf_key(location))
        return self.placed[name]

    def solids(self, name):
        placed = self.get(name)
        if placed.hash not in _HAS_SOLIDS:
            _HAS_SOLIDS[placed.hash] = bool(placed.shape.Solids())
        if not _HAS_SOLIDS[placed.hash]:
            raise DomainError("UNSUPPORTED_OPERATION", f"{name}: volumetric measurement requires solid geometry")
        return placed


# ---------------------------------------------------------------- planning

def _parse_request(args):
    states = args.get("states")
    if not isinstance(states, list) or not 1 <= len(states) <= MAX_STATES:
        raise _invalid(f"states must be a list of 1..{MAX_STATES}")
    limits = args.get("limits", {})
    if not isinstance(limits, dict):
        raise _invalid("limits must be an object")
    pair = _number(limits.get("pair_time_limit_s", DEFAULT_PAIR_LIMIT_S), "pair_time_limit_s")
    chunk = _number(limits.get("chunk_time_limit_s", DEFAULT_CHUNK_LIMIT_S), "chunk_time_limit_s")
    if not 0 < pair <= 3600 or not 0 < chunk <= 3600:
        raise _invalid("time limits must be in (0, 3600] seconds")
    workers = limits.get("workers", max(1, min(4, (os.cpu_count() or 2) - 1)))
    if type(workers) is not int or not 0 <= workers <= 32:
        raise _invalid("workers must be an integer in 0..32")
    cache = args.get("cache", {})
    if not isinstance(cache, dict):
        raise _invalid("cache must be an object")
    parsed, total, seen = [], 0, set()
    for state in states:
        if not isinstance(state, dict) or not isinstance(state.get("id"), str) or state["id"] in seen:
            raise _invalid("every state needs a unique string id")
        seen.add(state["id"])
        params = state.get("params", {})
        if not isinstance(params, dict):
            raise _invalid("params must be an object")
        params = {str(k): _number(v, f"param {k}") for k, v in params.items()}
        measurements, ids = [], set()
        for m in state.get("measurements", []):
            if not isinstance(m, dict) or not isinstance(m.get("id"), str) or m["id"] in ids:
                raise _invalid("every measurement needs a unique string id within its state")
            ids.add(m["id"])
            primitive = m.get("primitive")
            if primitive == "distance":
                primitive = "min_clearance"  # spec alias
            if primitive not in PRIMITIVES:
                raise _invalid(f"Unknown primitive {primitive!r}")
            spec = dict(id=m["id"], primitive=primitive)
            if primitive in ("interference", "contact_area", "min_clearance"):
                spec["a"] = _names(m.get("a"), "a")
                spec["b"] = _names(m.get("b"), "b")
            elif primitive == "outside_volume":
                spec["a"] = _names(m.get("a"), "a")
                spec["env"] = _names(m.get("env"), "env")
            elif primitive == "valid_solid_count":
                spec["a"] = _names(m.get("a"), "a")
            elif primitive == "extent":
                spec["a"] = _names(m.get("a"), "a")
                if m.get("axis") not in ("x", "y", "z"):
                    raise _invalid("extent axis must be x, y or z")
                spec["axis"] = m["axis"]
            elif primitive in _ENTITY_PRIMITIVES:
                spec["a"] = _entity_refs(m.get("a"), "a")
                if primitive in _ENTITY_WITH_B:
                    spec["b"] = _entity_refs(m.get("b"), "b")
                if primitive == "gap":
                    if m.get("spacing") is not None:
                        spacing = _number(m["spacing"], "spacing")
                        if spacing <= 0:
                            raise _invalid("spacing must be positive")
                        spec["spacing"] = spacing
                elif primitive == "clearance_along":
                    dirs = m.get("dirs")
                    if not isinstance(dirs, list) or not dirs or len(dirs) > 64:
                        raise _invalid("dirs must be a nonempty list of axes")
                    spec["dirs"] = [_axis_spec(d) for d in dirs]
                elif primitive == "overlap_depth":
                    spec["along"] = _axis_spec(m.get("along"))
                elif primitive in ("profile_centre", "profile_size"):
                    spec["axis"] = _axis_spec(m.get("axis"))
            measurements.append(spec)
        total += len(measurements)
        parsed.append(dict(id=state["id"], params=params, moves=_parse_moves(state.get("moves", [])),
                           measurements=measurements))
    if total > MAX_MEASUREMENTS:
        raise _invalid(f"at most {MAX_MEASUREMENTS} measurements per chunk")
    return dict(states=parsed, pair_limit=pair, chunk_limit=chunk, workers=workers,
                use_results=cache.get("results", True) is not False,
                use_rebuild=cache.get("rebuild", True) is not False,
                chunk_id=args.get("chunk_id"))


class Job:
    __slots__ = ("kind", "key", "task", "cost", "first", "outcome")

    def __init__(self, kind, key, task, cost, first):
        self.kind, self.key, self.task, self.cost, self.first = kind, key, task, cost, first
        self.outcome = None


def _cost(*placed):
    product = 1.0
    for p in placed:
        product *= max(p.box()["raw"]["size"][0] ** 2 + p.box()["raw"]["size"][1] ** 2 + p.box()["raw"]["size"][2] ** 2, 1e-12) ** 0.5
    return product


class Planner:
    """Turns measurements into items; items needing a job reference a registry entry."""

    def __init__(self, cache, use_results):
        self.cache, self.use_results = cache, use_results
        self.jobs, self.by_key = [], {}
        self.hits = self.dedup = self.misses = 0

    def job(self, kind, key, task, cost, label):
        full = f"{FORMAT}|{kind}|{key}"
        if full in self.by_key:
            self.dedup += 1
            return self.by_key[full], "dedup"
        job = Job(kind, full, task, cost, label)
        self.by_key[full] = job
        cached = self.cache.get(full) if self.use_results else None
        if cached is not None:
            job.outcome = Outcome("ok", cached, 0.0)
            self.hits += 1
            return job, "cache"
        self.jobs.append(job)
        self.misses += 1
        return job, "computed"


def _pairs(a, b):
    return sorted({tuple(sorted((x, y))) for x in a for y in b if x != y})


# ------------------------------------------------------- feature-aware entities

class GEntity:
    """One measurable thing in a state: a whole placed part or a feature face/edge."""

    __slots__ = ("label", "kind", "shape", "key", "part")

    def __init__(self, label, kind, shape, key, part=None):
        self.label, self.kind, self.shape, self.key = label, kind, shape, key
        self.part = part


def _scene_entities(scene, refs):
    """Resolve parsed entity refs to placed world geometry."""
    from .model import geometry_fingerprint
    out = []
    for ref in refs:
        rpart = scene.parts.get(ref["part"])
        if rpart is None:
            raise DomainError("UNKNOWN_PART", f"Unknown part {ref['part']}")
        if ref["feature"] is None:
            placed = scene.get(ref["part"])
            out.append(GEntity(ref["label"], "part", placed.shape, placed.key, ref["part"]))
            continue
        location = scene._location(ref["part"])
        trsf = _trsf_key(location)
        for kind, local in _feature_entities(rpart, ref["feature"]):
            world = cq.Shape.cast(local.wrapped.Moved(location.wrapped))
            key = f"{ref['part']}|{geometry_fingerprint(local)}|{trsf}"
            out.append(GEntity(ref["label"], kind, world, key, ref["part"]))
    return out


def _resolve_axis(scene, spec):
    """World direction of an axis spec: 'x'/'y'/'z', vector, feature object or
    entity object ``{"part": name, "feature": desc|null}``."""
    if isinstance(spec, dict):
        if "part" in spec:
            ref = {"part": spec["part"], "feature": spec.get("feature"),
                   "label": str(spec["part"])}
            entities = _scene_entities(scene, [ref])
            axes = [_entity_axis_single(ent) for ent in entities]
            if not axes:
                raise DomainError("INVALID_REQUEST", "axis entity resolved to nothing")
            point, direction = axes[0]
            d0 = _unit(direction)
            for other_point, other_direction in axes[1:]:
                d1 = _unit(other_direction)
                if abs(_vec_dot(d0, d1)) < 1.0 - 1e-9:
                    raise DomainError("UNSUPPORTED_OPERATION", "axis entities give different axes")
                offset = _vec_cross(_vec_sub(other_point, point), d0)
                scale = max(1.0, _vec_norm(point), _vec_norm(other_point))
                if _vec_norm(offset) > _AXIS_TOL * scale:
                    raise DomainError("UNSUPPORTED_OPERATION", "axis entities give offset axes")
            return d0
        part_name = spec.get("on")
        if not isinstance(part_name, str) or not part_name or not isinstance(spec.get("feature"), dict):
            raise DomainError("INVALID_REQUEST", "feature axis needs \"on\" and \"feature\"")
        axis = _scene_axis(scene, part_name, spec.get("feature"))
        return _unit(axis["direction"])
    return _unit(_axis_vector(spec))


_AXIS_TOL = 1e-6


def _face_radius(face):
    from OCP.BRepAdaptor import BRepAdaptor_Surface
    geom = face.geomType()
    if geom not in ("CYLINDER", "SPHERE"):
        return None
    adaptor = BRepAdaptor_Surface(face.wrapped)
    try:
        radius = float(adaptor.Cylinder().Radius() if geom == "CYLINDER" else adaptor.Sphere().Radius())
    except Exception:  # noqa: BLE001
        return None
    return radius if math.isfinite(radius) and radius > 0 else None


def _edge_radius(edge):
    if edge.geomType() != "CIRCLE":
        return None
    from OCP.BRepAdaptor import BRepAdaptor_Curve
    try:
        radius = float(BRepAdaptor_Curve(edge.wrapped).Circle().Radius())
    except Exception:  # noqa: BLE001
        return None
    return radius if math.isfinite(radius) and radius > 0 else None


def _radius_candidates(ent):
    if ent.kind == "face":
        return [v for v in (_face_radius(ent.shape),) if v is not None]
    if ent.kind == "edge":
        return [v for v in (_edge_radius(ent.shape),) if v is not None]
    values = [_face_radius(f) for f in ent.shape.Faces()]
    values += [_edge_radius(e) for e in ent.shape.Edges()]
    return [v for v in values if v is not None]


def _entity_measure(primitive, ent):
    """diameter/radius/length/area/volume of one entity (exact BRep queries)."""
    shape = ent.shape
    if primitive in ("diameter", "radius"):
        values = _radius_candidates(ent)
        if not values:
            raise DomainError("UNSUPPORTED_OPERATION",
                              f"{ent.label}: no cylinder, circle or sphere to measure")
        if max(values) - min(values) > 1e-9 * max(1.0, max(values)):
            raise DomainError("UNSUPPORTED_OPERATION",
                              f"{ent.label}: members give different radii")
        radius = values[0]
        return 2.0 * radius if primitive == "diameter" else radius
    if primitive == "length":
        if ent.kind == "edge":
            return float(shape.Length())
        box = shape.BoundingBox()
        return max(box.xmax - box.xmin, box.ymax - box.ymin, box.zmax - box.zmin)
    if primitive == "area":
        if ent.kind == "edge":
            raise DomainError("UNSUPPORTED_OPERATION", f"{ent.label}: an edge has no area")
        return float(shape.Area())
    if primitive == "volume":
        if ent.kind != "part":
            raise DomainError("UNSUPPORTED_OPERATION", f"{ent.label}: volume needs solid parts")
        solids = shape.Solids()
        if not solids:
            raise DomainError("UNSUPPORTED_OPERATION", f"{ent.label}: no solid geometry")
        return float(sum(s.Volume() for s in solids))
    raise DomainError("INVALID_REQUEST", f"Unknown measure {primitive}")


def _entity_axis_single(ent):
    """(point, unit direction) when the entity has exactly one consistent axis.

    Cylinder faces, circle edges and plane faces define the axis strictly
    (their axis passes through a canonical point). Line edges only contribute
    when nothing stricter exists: a cylinder's seam is a line parallel to, but
    offset from, the true axis, so lines are fallback candidates.
    """
    sources = [ent.shape] if ent.kind != "part" else list(ent.shape.Faces()) + list(ent.shape.Edges())
    strict, loose = [], []
    for source in sources:
        try:
            point, direction = _entity_axis(source)
        except DomainError:
            continue
        is_line = source.geomType() == "LINE"
        (loose if is_line else strict).append((_axis_point(source, point), direction))
    for pool in (strict, loose):
        if not pool:
            continue
        point, direction = pool[0]
        d0 = _unit(direction)
        for other_point, other_direction in pool[1:]:
            d1 = _unit(other_direction)
            if abs(_vec_dot(d0, d1)) < 1.0 - 1e-9:
                raise DomainError("UNSUPPORTED_OPERATION", f"{ent.label}: members give different axes")
            offset = _vec_cross(_vec_sub(other_point, point), d0)
            scale = max(1.0, _vec_norm(point), _vec_norm(other_point))
            if _vec_norm(offset) > _AXIS_TOL * scale:
                raise DomainError("UNSUPPORTED_OPERATION", f"{ent.label}: members give offset axes")
        return point, d0
    raise DomainError("UNSUPPORTED_OPERATION",
                      f"{ent.label}: no measurable axis (need a cylinder face, circle or "
                      "line edge, or plane face)")


def _axis_point(source, point):
    """Canonical point of an axis: a plane's Location() is arbitrary, so planes
    use their bounding-box centre (on-axis for symmetric features)."""
    if source.geomType() == "PLANE":
        box = source.BoundingBox()
        return [(box.xmin + box.xmax) * 0.5, (box.ymin + box.ymax) * 0.5,
                (box.zmin + box.zmax) * 0.5]
    return point


def _axis_distance(p1, d1, p2, d2):
    """Shortest distance between two (point, direction) axes."""
    normal = _vec_cross(d1, d2)
    nn = _vec_norm(normal)
    w = _vec_sub(p2, p1)
    if nn > 1e-12:
        return abs(_vec_dot(w, normal)) / nn
    return _vec_norm(_vec_cross(w, d1))


def _profile(shape, direction):
    """Bounding profile perpendicular to direction: (cx, cy, sx, sy) in the plane."""
    from OCP.gp import gp_Quaternion, gp_Trsf, gp_Vec
    d = _unit(direction)
    if _vec_dot(d, [0.0, 0.0, 1.0]) < -0.999999999:
        quat = gp_Quaternion(gp_Vec(1.0, 0.0, 0.0), math.pi)
    else:
        quat = gp_Quaternion(gp_Vec(*d), gp_Vec(0.0, 0.0, 1.0))
    trsf = gp_Trsf()
    trsf.SetRotation(quat)
    rotated = cq.Shape.cast(shape.wrapped.Moved(cq.Location(trsf).wrapped))
    box = rotated.BoundingBox()
    return ((box.xmin + box.xmax) * 0.5, (box.ymin + box.ymax) * 0.5,
            box.xmax - box.xmin, box.ymax - box.ymin)


def _fastener_axis(ent):
    """(point, unit direction) of a fastener's thread axis.

    Only cylindrical faces and circular edges vote: hex heads and washer
    faces are planes whose normals would look like conflicting axes.
    """
    sources = [f for f in ent.shape.Faces() if f.geomType() == "CYLINDER"]
    sources += [e for e in ent.shape.Edges() if e.geomType() == "CIRCLE"]
    if not sources:
        raise DomainError("UNSUPPORTED_OPERATION",
                          f"{ent.label}: fastener has no cylindrical face or circular edge")
    point, direction = _entity_axis(sources[0])
    d0 = _unit(direction)
    for source in sources[1:]:
        p1, d1 = _entity_axis(source)
        if abs(_vec_dot(d0, _unit(d1))) < 1.0 - 1e-9:
            raise DomainError("UNSUPPORTED_OPERATION", f"{ent.label}: members give different axes")
        offset = _vec_cross(_vec_sub(p1, point), d0)
        scale = max(1.0, _vec_norm(point), _vec_norm(p1))
        if _vec_norm(offset) > _AXIS_TOL * scale:
            raise DomainError("UNSUPPORTED_OPERATION", f"{ent.label}: members give offset axes")
    return point, d0


def _thread_protrusion(ent, scene):
    """Threaded-end overhang of a fastener past the far side of its mates.

    Uses the published ``fastener`` metadata (``mates``) plus geometry: the
    axis comes from the fastener's own cylinders, the head end is the end
    with the larger sampled profile, and protrusion is the fastener's extent
    beyond the mates' interval on the opposite end — 0 when the fastener does
    not reach through the stack.
    """
    rpart = scene.parts.get(ent.part) if ent.part else None
    meta = getattr(rpart, "fastener", None)
    mates = list((meta or {}).get("mates") or [])
    if not mates:
        raise DomainError("UNSUPPORTED_OPERATION",
                          f"{ent.label}: thread_protrusion needs fastener metadata with mates")
    point, d = _fastener_axis(ent)
    step = _default_step([ent.shape])
    samples = _shape_samples(ent.shape, step)
    if not samples:
        raise DomainError("INVALID_GEOMETRY", f"{ent.label}: fastener has no measurable extent")
    ts, rs = [], []
    for p in samples:
        rel = _vec_sub(p, point)
        t = _vec_dot(rel, d)
        ts.append(t)
        rs.append(_vec_norm(_vec_sub(rel, _vec_scale(d, t))))
    t_lo, t_hi = min(ts), max(ts)
    span = t_hi - t_lo
    if span <= 1e-9:
        raise DomainError("INVALID_GEOMETRY", f"{ent.label}: fastener has no axial extent")
    band = max(0.25 * span, step)
    lo_r = [r for t, r in zip(ts, rs) if t <= t_lo + band]
    hi_r = [r for t, r in zip(ts, rs) if t >= t_hi - band]
    lo_mean = math.fsum(lo_r) / len(lo_r) if lo_r else 0.0
    hi_mean = math.fsum(hi_r) / len(hi_r) if hi_r else 0.0
    # The head end carries the larger profile; the threaded end is the other
    # one. Symmetric fasteners (nuts, spacers) have no head: report the larger
    # overhang so the clamping side is not mistaken for a missing thread.
    scale = max(lo_mean, hi_mean, 1e-9)
    if abs(lo_mean - hi_mean) <= 0.05 * scale:
        head_at_lo = None
    else:
        head_at_lo = lo_mean > hi_mean
    # The mates' interval along the fastener axis; bounding boxes are tight.
    m_lo, m_hi = math.inf, -math.inf
    for name in mates:
        placed = scene.get(name)
        box = placed.shape.BoundingBox()
        for x in (box.xmin, box.xmax):
            for y in (box.ymin, box.ymax):
                for z in (box.zmin, box.zmax):
                    t = _vec_dot(_vec_sub((x, y, z), point), d)
                    m_lo, m_hi = min(m_lo, t), max(m_hi, t)
    over_lo = m_lo - t_lo   # fastener extent below the mates' near face
    over_hi = t_hi - m_hi   # fastener extent past the mates' far face
    if head_at_lo is None:
        protrusion = max(over_lo, over_hi)
    elif head_at_lo:
        protrusion = over_hi
    else:
        protrusion = over_lo
    return max(0.0, protrusion)


def _shadow_clearance(a_points, b_points, direction):
    """Clearance of A from B along direction, ignoring perpendicular offsets.

    Shadow semantics: the distance A travels along the direction before the
    projections onto it overlap B's. Perpendicular offsets are ignored, so the
    result never overstates the real clearance. Unbounded when B lies wholly
    behind A along the direction.
    """
    a_low, a_high = _project_interval(a_points, direction)
    b_low, b_high = _project_interval(b_points, direction)
    if b_high < a_low:
        return _INF_CLEARANCE
    if b_low <= a_high:
        return 0.0
    return b_low - a_high


def _dir_key(direction):
    return ",".join(_hex(v) for v in direction)


def _plan_entities(spec, scene, planner, label):
    """Plan a feature-aware measurement -> same shape as _plan_measurement."""
    start = pc()
    primitive = spec["primitive"]
    a_ents = _scene_entities(scene, spec["a"])
    b_ents = _scene_entities(scene, spec["b"]) if "b" in spec else []
    items, extra = [], {}
    if primitive in ("diameter", "radius", "length", "area", "volume"):
        for ent in a_ents:
            items.append(dict(a=ent.label, value=_entity_measure(primitive, ent)))
    elif primitive == "diameter_difference":
        for x in a_ents:
            for y in b_ents:
                value = (_entity_measure("diameter", x)
                         - _entity_measure("diameter", y))
                items.append(dict(a=x.label, b=y.label, value=value))
    elif primitive == "thread_protrusion":
        for ent in a_ents:
            items.append(dict(a=ent.label, value=_thread_protrusion(ent, scene)))
    elif primitive == "gap":
        for x in a_ents:
            for y in b_ents:
                key = "|".join(sorted((x.key, y.key)))
                job, source = planner.job("distance", key, ("distance", x.shape, y.shape),
                                          _entity_cost(x, y), label + (x.label, y.label))
                items.append(dict(a=x.label, b=y.label, job=job, source=source))
        step = spec.get("spacing") or _default_step([e.shape for e in a_ents + b_ents])
        a_points = _entity_cloud(a_ents, step)
        b_points = _entity_cloud(b_ents, step)
        extra["gap_max"] = _max_cloud_distance(a_points, b_points)
    elif primitive == "clearance_along":
        directions = [_resolve_axis(scene, d) for d in spec["dirs"]]
        clouds = {}
        for ent in a_ents + b_ents:
            clouds[id(ent)] = _entity_cloud([ent], _default_step([ent.shape]))
        for x in a_ents:
            for y in b_ents:
                for d in directions:
                    value = _shadow_clearance(clouds[id(x)], clouds[id(y)], d)
                    items.append(dict(a=x.label, b=y.label, value=value))
    elif primitive == "overlap_depth":
        direction = _resolve_axis(scene, spec["along"])
        for x in a_ents:
            for y in b_ents:
                key = f"overlap|{_dir_key(direction)}|" + "|".join(sorted((x.key, y.key)))
                task = ("overlap", _compound([x.shape]), _compound([y.shape]), list(direction))
                job, source = planner.job("overlap", key, task, _entity_cost(x, y),
                                          label + (x.label, y.label))
                items.append(dict(a=x.label, b=y.label, job=job, source=source))
    elif primitive in ("axis_offset", "axis_angle"):
        for x in a_ents:
            for y in b_ents:
                px, dx = _entity_axis_single(x)
                py, dy = _entity_axis_single(y)
                if primitive == "axis_offset":
                    value = _axis_distance(px, dx, py, dy)
                else:
                    value = math.acos(min(1.0, abs(_vec_dot(dx, dy))))
                items.append(dict(a=x.label, b=y.label, value=value))
    elif primitive in ("profile_centre", "profile_size"):
        axis = _resolve_axis(scene, spec["axis"])
        for x in a_ents:
            for y in b_ents:
                cx, cy, sx, sy = _profile(x.shape, axis)
                dx_, dy_, ex, ey = _profile(y.shape, axis)
                if primitive == "profile_centre":
                    value = math.hypot(dx_ - cx, dy_ - cy)
                else:
                    value = max(ex, ey) - max(sx, sy)
                items.append(dict(a=x.label, b=y.label, value=value))
    else:
        raise DomainError("INVALID_REQUEST", f"Unknown primitive {primitive}")
    return dict(items=items, pruned=0, total=len(items), broad_s=pc() - start, **extra)


def _plan_measurement(spec, scene, planner, label):
    """-> dict(items=[...], pruned=int, total=int, broad_s=float); items hold job refs."""
    start = pc()
    primitive, items, pruned, total = spec["primitive"], [], 0, 0
    if primitive in ("interference", "contact_area", "min_clearance"):
        pairs = _pairs(spec["a"], spec["b"])
        total = len(pairs)
        for x, y in pairs:
            if primitive in ("interference", "contact_area"):
                px, py = scene.solids(x), scene.solids(y)
                if _disjoint(px, py):
                    pruned += 1
                    continue
                key = "|".join(sorted((px.key, py.key)))
                kind = "common" if primitive == "interference" else "contact_area"
                job, source = planner.job(kind, key, (kind, px.shape, py.shape), _cost(px, py), label + (x, y))
            else:
                px, py = scene.get(x), scene.get(y)
                key = "|".join(sorted((px.key, py.key)))
                job, source = planner.job("distance", key, ("distance", px.shape, py.shape), _cost(px, py), label + (x, y))
            items.append(dict(a=x, b=y, job=job, source=source))
    elif primitive == "outside_volume":
        env = [scene.solids(n) for n in spec["env"]]
        total = len(spec["a"])
        for x in spec["a"]:
            px = scene.solids(x)
            near = [e for e in env if e.name != x and not _disjoint(px, e)]
            if not near:
                pruned += 1
                items.append(dict(a=x, value=float(sum(s.Volume() for s in px.shape.Solids())), pruned=True))
                continue
            key = px.key + "|" + "|".join(sorted(e.key for e in near))
            job, source = planner.job("cut", key, ("cut", px.shape, [e.shape for e in near]),
                                      _cost(px, *near), label + (x,))
            items.append(dict(a=x, job=job, source=source))
    elif primitive == "valid_solid_count":
        total = len(spec["a"])
        for x in spec["a"]:
            px = scene.get(x)
            # Validity is invariant under rigid placement: key on geometry only.
            job, source = planner.job("validity", px.hash, ("validity", px.shape), _cost(px), label + (x,))
            items.append(dict(a=x, job=job, source=source))
    elif primitive in _ENTITY_PRIMITIVES:
        return _plan_entities(spec, scene, planner, label)
    else:  # extent: bounds only, no job
        total = len(spec["a"])
        lows, highs = [], []
        index = "xyz".index(spec["axis"])
        for x in spec["a"]:
            raw = scene.get(x).box()["raw"]
            lows.append(raw["min"][index])
            highs.append(raw["max"][index])
        items.append(dict(extent=(min(lows), max(highs))))
    return dict(items=items, pruned=pruned, total=total, broad_s=pc() - start)


def _item_witness(item):
    return [v for v in (item.get("a"), item.get("b")) if v]


def _reduce(spec, planned):
    """Deterministic reduction of one measurement, in sorted plan order."""
    primitive = spec["primitive"]
    out = dict(id=spec["id"], primitive=primitive, status="ok", value=None, witness=None, reason=None,
               pairs_total=planned["total"], pairs_pruned=planned["pruned"], items=[])
    if primitive == "extent":
        low, high = planned["items"][0]["extent"]
        out.update(value=high - low, min=low, max=high, axis=spec["axis"])
        return out
    status, reason = "ok", None
    values = []
    for item in planned["items"]:
        job = item.get("job")
        entry = {k: v for k, v in item.items() if k in ("a", "b")}
        if job is None:  # computed at plan time or a pruned outside_volume member
            entry.update(status="ok", value=item["value"])
            if item.get("pruned"):
                entry["pruned"] = True
            values.append((item["value"], item))
        elif job.outcome.status == "ok":
            value = job.outcome.value
            if primitive == "valid_solid_count":
                entry.update(status="ok", value=value[0], solids=value[1])
                values.append((value, item))
            else:
                entry.update(status="ok", value=value)
                values.append((value, item))
        elif job.outcome.status == "timeout":
            entry.update(status="inconclusive", value=None)
            if status != "error":
                status, reason = "inconclusive", "pair_time_limit"
        else:
            entry.update(status="error", value=None, error=job.outcome.value)
            status, reason = "error", job.outcome.value["message"]
        out["items"].append(entry)
    out.update(status=status, reason=reason)
    if status != "ok":
        return out
    if primitive == "interference":
        out["value"] = float(sum(v for v, _ in values)) if values else 0.0
        best = max(values, key=lambda t: t[0], default=None)  # first of equals: sorted pair order
        if best is not None and best[0] > 0:
            out["witness"] = [best[1]["a"], best[1]["b"]]
    elif primitive == "contact_area":
        out["value"] = math.fsum(v for v, _ in values)
        best = max(values, key=lambda t: t[0], default=None)  # first of equals: sorted pair order
        if best is not None and best[0] > 0:
            out["witness"] = [best[1]["a"], best[1]["b"]]
    elif primitive == "min_clearance":
        best = min(values, key=lambda t: (t[0], t[1].get("a",""), t[1].get("b","")), default=None)
        if best is None:
            out.update(status="error", reason="min_clearance needs at least one distinct pair")
        else:
            out["value"], out["witness"] = best[0], [best[1]["a"], best[1]["b"]]
    elif primitive == "outside_volume":
        out["value"] = float(sum(v for v, _ in values))
    elif primitive == "gap":
        best = min(values, key=lambda t: (t[0], t[1].get("a",""), t[1].get("b","")), default=None)
        if best is None:
            out.update(status="error", reason="gap needs at least one entity pair")
        else:
            out["value"], out["witness"] = best[0], _item_witness(best[1])
            if planned.get("gap_max") is not None:
                out["max"] = planned["gap_max"]
    elif primitive == "clearance_along":
        best = min(values, key=lambda t: (t[0], t[1].get("a",""), t[1].get("b","")), default=None)
        if best is None:
            out.update(status="error", reason="clearance_along needs at least one entity pair")
        else:
            out["value"], out["witness"] = best[0], _item_witness(best[1])
    elif primitive in ("profile_size",):
        best = min(values, key=lambda t: (t[0], t[1].get("a",""), t[1].get("b","")), default=None)
        if best is None:
            out.update(status="error", reason="profile_size needs at least one entity pair")
        else:
            out["value"], out["witness"] = best[0], _item_witness(best[1])
    elif primitive in ("overlap_depth", "axis_offset", "axis_angle", "length", "profile_centre"):
        best = max(values, key=lambda t: (t[0], t[1].get("a",""), t[1].get("b","")), default=None)
        if best is None:
            out.update(status="error", reason=f"{primitive} needs at least one entity")
        else:
            out["value"], out["witness"] = best[0], _item_witness(best[1])
    elif primitive == "diameter_difference":
        # A set reports the pair with the largest absolute difference.
        best = max(values, key=lambda t: (abs(t[0]), t[1].get("a",""), t[1].get("b","")), default=None)
        if best is None:
            out.update(status="error", reason="diameter_difference needs at least one entity pair")
        else:
            out["value"], out["witness"] = best[0], _item_witness(best[1])
    elif primitive == "thread_protrusion":
        # The worst-engaging fastener decides.
        best = min(values, key=lambda t: (t[0], t[1].get("a","")), default=None)
        if best is None:
            out.update(status="error", reason="thread_protrusion needs at least one entity")
        else:
            out["value"], out["witness"] = best[0], [best[1]["a"]]
    elif primitive in ("area", "volume"):
        out["value"] = float(sum(v for v, _ in values))
    elif primitive in ("diameter", "radius"):
        if not values:
            out.update(status="error", reason=f"{primitive} needs at least one entity")
        else:
            low, high = min(v for v, _ in values), max(v for v, _ in values)
            if high - low > 1e-7 * max(1.0, abs(high)):
                out.update(status="error",
                           reason=f"{primitive}: entities disagree on the radius")
            else:
                out["value"] = values[0][0]
                out["witness"] = [values[0][1]["a"]]
    else:
        out["value"] = int(sum(v[0] for v, _ in values))
        out["solids"] = int(sum(v[1] for v, _ in values))
    return out


# ---------------------------------------------------------------- entry point

def _effective(model, overrides):
    base = {name: p["value"] for name, p in model.parameters.items()}
    for name in overrides:
        if name not in base:
            raise DomainError("INVALID_PARAMETER", f"Unknown parameter {name}")
    return base, {**base, **overrides}


def _rebuild_key(identity, values):
    return hashlib.sha256(_canonical(dict(engine=_engine_id(), input=identity,
                                          params={k: _hex(v) for k, v in sorted(values.items())})).encode()).hexdigest()


def _prepare_scenes(model, ev, request, deadline, timings, counters):
    """-> {state index: (Scene | None, error | None)}"""
    states = request["states"]
    base_parts = ev.base_parts(model)
    args = getattr(model, "_build_args", None)
    digest = (args or {}).get("input_digest")
    from .geometry_cache import valid_hash
    persistent = valid_hash(digest)
    identity = digest if persistent else f"revision:{model.revision}"
    plan, needed, errors = {}, OrderedDict(), {}
    for i, state in enumerate(states):
        try:
            if state["params"]:
                base, effective = _effective(model, state["params"])
                if effective == base:
                    plan[i] = None
                else:
                    key = _rebuild_key(identity, effective)
                    plan[i] = key
                    needed.setdefault(key, effective)
            else:
                plan[i] = None
        except DomainError as error:
            errors[i] = dict(code=error.code, message=str(error))
    if needed and args is None:
        raise DomainError("UNSUPPORTED_OPERATION", "param states need the build arguments of this worker's subject")
    root = model.cache / "requirements"
    resolved, rebuild_seconds, source = {}, {}, {}
    scratch = None
    todo = []
    for key, effective in needed.items():
        if request["use_rebuild"] and key in ev.rebuilds:
            ev.rebuilds.move_to_end(key)
            resolved[key], source[key] = ev.rebuilds[key], "memory"
            counters["rebuild_memory_hits"] += 1
            continue
        if request["use_rebuild"] and persistent:
            loaded = _load_rebuilt(root / "rebuild" / key)
            if loaded is not None:
                resolved[key], source[key] = loaded, "disk"
                counters["rebuild_disk_hits"] += 1
                continue
        todo.append(key)
    if todo:
        if request["use_rebuild"] and persistent:
            directory = root / "rebuild"
        else:
            scratch = directory = root / f"tmp-{uuid.uuid4().hex}"
        directory.mkdir(parents=True, exist_ok=True)
        tasks = []
        for key in todo:
            rebuild_args = dict(args, parameters={**args.get("parameters", {}), **needed[key]},
                                generate_mesh=False, annotations=[], collision=None,
                                geometry_cache=None, input_digest=None, reuse_meshes={})
            tasks.append((rebuild_args, model.revision, str(model.cache), str(directory / key)))
        start = pc()
        try:
            outcomes = run_tasks(_rebuild_task, tasks, workers=request["workers"], task_limit=None, deadline=deadline)
        finally:
            timings["rebuild_wall_s"] = pc() - start
        for key, outcome in zip(todo, outcomes):
            counters["rebuilds"] += 1
            rebuild_seconds[key] = outcome.seconds
            source[key] = "built"
            if outcome.status != "ok":
                detail = outcome.value if isinstance(outcome.value, dict) else dict(code="BUILD_FAILED", message=str(outcome.value))
                resolved[key] = ("error", detail)
                continue
            loaded = _load_rebuilt(directory / key)
            resolved[key] = loaded if loaded is not None else ("error", dict(code="BUILD_FAILED", message="Rebuilt geometry could not be read back"))
        if scratch is not None:
            shutil.rmtree(scratch, ignore_errors=True)
        elif request["use_rebuild"]:
            _trim_rebuild_dirs(directory)
    if request["use_rebuild"]:
        for key in todo:
            if isinstance(resolved[key], dict):
                ev.rebuilds[key] = resolved[key]
        while len(ev.rebuilds) > MAX_REBUILD_MEMORY:
            ev.rebuilds.popitem(last=False)
    scenes = {}
    for i, state in enumerate(states):
        start = pc()
        if i in errors:
            scenes[i] = (None, errors[i], dict(rebuild="none"))
            continue
        key = plan[i]
        if key is None:
            parts, how = base_parts, "none"
        else:
            parts, how = resolved[key], source[key]
            if isinstance(parts, tuple):
                scenes[i] = (None, parts[1], dict(rebuild=how, rebuild_s=rebuild_seconds.get(key)))
                continue
        try:
            scene = Scene(parts, state["moves"])
            unknown = [n for m in state["moves"] for n in sorted(m["parts"]) if n not in parts]
            if unknown:
                raise DomainError("UNKNOWN_PART", f"Unknown part {unknown[0]} in move")
            scenes[i] = (scene, None, dict(rebuild=how, rebuild_s=rebuild_seconds.get(key), place_s=pc() - start))
        except DomainError as error:
            scenes[i] = (None, dict(code=error.code, message=str(error)), dict(rebuild=how))
    return scenes


def _trim_rebuild_dirs(directory):
    try:
        entries = sorted((p for p in directory.iterdir() if p.is_dir() and ".partial-" not in p.name),
                         key=lambda p: p.stat().st_mtime)
        for old in entries[:max(0, len(entries) - MAX_REBUILD_DIRS)]:
            shutil.rmtree(old, ignore_errors=True)
    except OSError:
        pass


def evaluate_plan_chunk(model, args):
    t_start = pc()
    request = _parse_request(args)
    deadline = t_start + request["chunk_limit"]
    ev = getattr(model, "_requirements", None)
    if ev is None:
        ev = model._requirements = EvalState(model)
    if request["use_results"]:
        ev.results.load()
    counters = dict(result_hits=0, result_misses=0, dedup=0, rebuild_memory_hits=0, rebuild_disk_hits=0, rebuilds=0)
    timings = dict(phases={}, states=[], measurements=[], jobs=[], workers=request["workers"])
    response = dict(format=FORMAT, chunk_id=request["chunk_id"], status="complete", reason=None,
                    engine=_engine(), results=None, results_sha256=None, timings=timings, cache=counters)
    try:
        t = pc()
        scenes = _prepare_scenes(model, ev, request, deadline, timings["phases"], counters)
        timings["phases"]["prepare_s"] = pc() - t
        planner = Planner(ev.results, request["use_results"])
        t = pc()
        planned = []
        for i, state in enumerate(request["states"]):
            scene, error, info = scenes[i]
            row = []
            for spec in state["measurements"]:
                if scene is None:
                    row.append(dict(error=error))
                    continue
                try:
                    row.append(_plan_measurement(spec, scene, planner, (state["id"], spec["id"])))
                except DomainError as exc:
                    row.append(dict(error=dict(code=exc.code, message=str(exc))))
                if pc() > deadline:
                    raise ChunkCancelled()
            planned.append(row)
        timings["phases"]["broad_s"] = pc() - t
        counters.update(result_hits=planner.hits, result_misses=planner.misses, dedup=planner.dedup)
        # Heaviest first shortens the straggler tail; order never affects values.
        order = sorted(range(len(planner.jobs)), key=lambda i: (-planner.jobs[i].cost, planner.jobs[i].key))
        t = pc()
        outcomes = run_tasks(_run_job, [job.task for job in planner.jobs], workers=request["workers"],
                             task_limit=request["pair_limit"], deadline=deadline, order=order)
        timings["phases"]["jobs_wall_s"] = pc() - t
        for job, outcome in zip(planner.jobs, outcomes):
            job.outcome = outcome
            if outcome.status == "ok" and request["use_results"]:
                ev.results.put(job.key, outcome.value)
        if request["use_results"]:
            ev.results.persist()
        states_out = []
        for i, state in enumerate(request["states"]):
            scene, error, info = scenes[i]
            entry = dict(id=state["id"], status="ok" if scene is not None else "error", measurements=[])
            if scene is None:
                entry["error"] = error
            for spec, plan in zip(state["measurements"], planned[i]):
                if "error" in plan:
                    entry["measurements"].append(dict(id=spec["id"], primitive=spec["primitive"], status="error",
                                                      value=None, witness=None, reason=plan["error"]["message"],
                                                      error=plan["error"], pairs_total=0, pairs_pruned=0, items=[]))
                    continue
                entry["measurements"].append(_reduce(spec, plan))
                first = [j for it in plan["items"] if (j := it.get("job")) is not None]
                timings["measurements"].append(dict(
                    state=state["id"], id=spec["id"], broad_s=plan["broad_s"], pruned=plan["pruned"],
                    jobs=len(first), job_cpu_s=sum(j.outcome.seconds for j in first if j.first[:2] == (state["id"], spec["id"])),
                    cache_hits=sum(1 for it in plan["items"] if it.get("source") == "cache")))
            states_out.append(entry)
            timings["states"].append(dict(id=state["id"], **{k: v for k, v in info.items() if v is not None}))
        results = dict(states=states_out)
        response.update(results=results, results_sha256=hashlib.sha256(_canonical(results).encode()).hexdigest())
        jobs = [dict(state=j.first[0], measurement=j.first[1], kind=j.kind, a=j.first[2], b=j.first[3] if len(j.first) > 3 else None,
                     seconds=j.outcome.seconds, status=j.outcome.status, source="computed") for j in planner.jobs]
        jobs.sort(key=lambda j: (-j["seconds"], j["state"], j["measurement"], j["a"], j["b"] or ""))
        timings["jobs"] = jobs[:MAX_JOB_TIMINGS]
    except ChunkCancelled:
        # Cancelled chunks claim nothing: no values, no cache writes beyond finished results.
        response.update(status="cancelled", reason="chunk_time_limit", results=None, results_sha256=None)
    timings["total_s"] = pc() - t_start
    return response
