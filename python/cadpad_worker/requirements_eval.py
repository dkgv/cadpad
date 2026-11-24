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
            {"id": "m5", "primitive": "valid_solid_count", "a": ["arm"]}
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
PRIMITIVES = ("interference", "min_clearance", "extent", "outside_volume", "valid_solid_count")
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


def _run_job(task):
    kind = task[0]
    if kind == "common":
        return _common_volume(task[1], task[2])
    if kind == "cut":
        return _cut_volume(task[1], task[2])
    if kind == "distance":
        return _distance(task[1], task[2])
    if kind == "validity":
        return _validity(task[1])
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
    __slots__ = ("name", "shape", "position", "hash")

    def __init__(self, name, shape, position, digest):
        self.name, self.shape, self.position, self.hash = name, shape, tuple(position), digest


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
        entries.append(dict(name=name, handle=handle, sha256=hashlib.sha256(data).hexdigest(),
                            position=list(part.position), geometry_sha256=geometry_fingerprint(restored)))
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
            parts[entry["name"]] = RPart(entry["name"], shape, entry["position"], entry["geometry_sha256"])
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
                parts[name] = RPart(name, part.shape, part.position, part.fingerprint)
            self.base = parts
        return self.base


# ---------------------------------------------------------------- placement

def _axis_vector(axis):
    named = dict(x=[1.0, 0.0, 0.0], y=[0.0, 1.0, 0.0], z=[0.0, 0.0, 1.0])
    vector = named.get(axis) if isinstance(axis, str) else _vec3(axis, "axis")
    if vector is None or math.sqrt(sum(v * v for v in vector)) < 1e-12:
        raise _invalid("axis must be x, y, z or a nonzero vector")
    return vector


def _parse_moves(moves):
    if not isinstance(moves, list) or len(moves) > 256:
        raise _invalid("moves must be a list")
    parsed = []
    for move in moves:
        if not isinstance(move, dict) or move.get("op") not in ("rotate", "translate"):
            raise _invalid("move op must be rotate or translate")
        names = set(_names(move.get("parts"), "move parts"))
        if move["op"] == "rotate":
            parsed.append(dict(op="rotate", parts=names, axis=_axis_vector(move.get("axis")),
                               origin=_vec3(move.get("origin", [0, 0, 0]), "origin"),
                               angle=_number(move.get("angle_deg"), "angle_deg")))
        else:
            parsed.append(dict(op="translate", parts=names, vector=_vec3(move.get("vector"), "vector")))
    return parsed


def _move_location(move):
    from OCP.gp import gp_Ax1, gp_Dir, gp_Pnt, gp_Trsf, gp_Vec
    trsf = gp_Trsf()
    if move["op"] == "rotate":
        trsf.SetRotation(gp_Ax1(gp_Pnt(*move["origin"]), gp_Dir(*move["axis"])), math.radians(move["angle"]))
    else:
        trsf.SetTranslation(gp_Vec(*move["vector"]))
    return cq.Location(trsf)


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
        self.moves_locations = [_move_location(m) for m in moves]
        self.placed = {}

    def get(self, name):
        if name not in self.placed:
            if name not in self.parts:
                raise DomainError("UNKNOWN_PART", f"Unknown part {name}")
            part = self.parts[name]
            from .model import Model
            location = Model.location(dict(position=list(part.position)))
            for move, move_location in zip(self.moves, self.moves_locations):
                if name in move["parts"]:
                    location = move_location * location
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
            if primitive not in PRIMITIVES:
                raise _invalid(f"Unknown primitive {primitive!r}")
            spec = dict(id=m["id"], primitive=primitive, a=_names(m.get("a"), "a"))
            if primitive in ("interference", "min_clearance"):
                spec["b"] = _names(m.get("b"), "b")
            elif primitive == "outside_volume":
                spec["env"] = _names(m.get("env"), "env")
            elif primitive == "extent":
                if m.get("axis") not in ("x", "y", "z"):
                    raise _invalid("extent axis must be x, y or z")
                spec["axis"] = m["axis"]
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


def _plan_measurement(spec, scene, planner, label):
    """-> dict(items=[...], pruned=int, total=int, broad_s=float); items hold job refs."""
    start = pc()
    primitive, items, pruned, total = spec["primitive"], [], 0, 0
    if primitive in ("interference", "min_clearance"):
        pairs = _pairs(spec["a"], spec["b"])
        total = len(pairs)
        for x, y in pairs:
            if primitive == "interference":
                px, py = scene.solids(x), scene.solids(y)
                if _disjoint(px, py):
                    pruned += 1
                    continue
                key = "|".join(sorted((px.key, py.key)))
                job, source = planner.job("common", key, ("common", px.shape, py.shape), _cost(px, py), label + (x, y))
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
        if job is None:  # pruned outside_volume member: whole volume
            entry.update(status="ok", value=item["value"], pruned=True)
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
    elif primitive == "min_clearance":
        best = min(values, key=lambda t: (t[0], t[1]["a"], t[1]["b"]), default=None)
        if best is None:
            out.update(status="error", reason="min_clearance needs at least one distinct pair")
        else:
            out["value"], out["witness"] = best[0], [best[1]["a"], best[1]["b"]]
    elif primitive == "outside_volume":
        out["value"] = float(sum(v for v, _ in values))
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
