from __future__ import annotations

import copy
import io
import hashlib
import importlib.metadata
import json
import math
import os
import runpy
import sys
import tempfile
import uuid
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace

import cadquery as cq

from .errors import DomainError


def finite(value):
    if isinstance(value, (int, float)) and not math.isfinite(value):
        raise DomainError("INVALID_GEOMETRY", "Geometry contains non-finite values")
    if isinstance(value, dict):
        for item in value.values():
            finite(item)
    elif isinstance(value, (list, tuple)):
        for item in value:
            finite(item)
    return value


def validate_fastener(value):
    """Validate explicit fastener metadata without inferring standard dimensions."""
    if value is None:
        return None
    if not isinstance(value, dict):
        raise DomainError("INVALID_PARAMETER", "fastener must be a mapping")
    unknown = set(value) - {"kind", "size", "length", "mates"}
    if unknown:
        raise DomainError("INVALID_PARAMETER", f"Unknown fastener fields: {sorted(unknown)}")
    for key in ("kind", "size"):
        item = value.get(key)
        if not isinstance(item, str) or not item.strip():
            raise DomainError("INVALID_PARAMETER", f"fastener.{key} must be a non-empty string")
    result = {"kind": value["kind"].strip(), "size": value["size"].strip()}
    if "length" in value:
        length = value["length"]
        if isinstance(length, bool) or not isinstance(length, (int, float)) or not math.isfinite(length) or length <= 0:
            raise DomainError("INVALID_PARAMETER", "fastener.length must be a positive finite number in mm")
        result["length"] = float(length)
    if "mates" in value:
        mates = value["mates"]
        if not isinstance(mates, (list, tuple)) or any(not isinstance(m, str) or not m.strip() for m in mates):
            raise DomainError("INVALID_PARAMETER", "fastener.mates must be a list of non-empty published part names")
        mates = [m.strip() for m in mates]
        if len(set(mates)) != len(mates):
            raise DomainError("INVALID_PARAMETER", "fastener.mates must not contain duplicate part names")
        result["mates"] = mates
    return result


def vector(v):
    return [v.x, v.y, v.z]


def bounds(shape):
    from OCP.BRepBndLib import BRepBndLib
    from OCP.Bnd import Bnd_Box
    box = Bnd_Box()
    # Cached display triangulation must never change authoritative dimensions.
    BRepBndLib.AddOptimal_s(shape.wrapped, box, False, False)
    xmin, ymin, zmin, xmax, ymax, zmax = box.Get()
    return {"min": [xmin, ymin, zmin], "max": [xmax, ymax, zmax],
            "size": [xmax-xmin, ymax-ymin, zmax-zmin]}


FINGERPRINT_FORMAT = "occt-brep-v3-no-display-v1"


def geometry_fingerprint(shape):
    from OCP.BRepBuilderAPI import BRepBuilderAPI_Copy
    from OCP.BRepTools import BRepTools
    from OCP.TopExp import TopExp
    from OCP.TopTools import TopTools_IndexedMapOfShape, TopTools_FormatVersion
    # Copy topology without cached mesh data. Geometry and its tolerances remain
    # represented; transient topology ownership/validation flags are normalized.
    copied = BRepBuilderAPI_Copy(shape.wrapped, False, False).Shape()
    topology = TopTools_IndexedMapOfShape()
    TopExp.MapShapes_s(copied, topology)
    for index in range(1, topology.Extent() + 1):
        item = topology.FindKey(index)
        item.Checked(False)
        item.Modified(False)
        item.Free(False)
    stream = io.BytesIO()
    BRepTools.Write_s(copied, stream, False, False,
                      TopTools_FormatVersion.TopTools_FormatVersion_VERSION_3)
    return hashlib.sha256(stream.getvalue()).hexdigest()


# Caching assumes the stock kernel-backed Shape methods. Custom subclasses and
# per-instance/class method overrides retain their original fresh behavior.
_INTRINSIC_METHODS = ("Volume", "Area", "Solids", "ShapeType", "Faces", "Edges", "geomType", "Length", "Center")
_INTRINSIC_PRODUCERS = {
    shape_type: {name: getattr(getattr(shape_type, name), "__func__", getattr(shape_type, name))
                 for name in _INTRINSIC_METHODS if hasattr(shape_type, name)}
    for shape_type in (cq.Shape, cq.Vertex, cq.Edge, cq.Wire, cq.Face, cq.Shell, cq.Solid, cq.Compound)
}


def intrinsic_reuse_allowed(shape):
    methods = _INTRINSIC_PRODUCERS.get(type(shape))
    return methods is not None and all(
        getattr(getattr(shape, name), "__func__", getattr(shape, name)) is producer
        for name, producer in methods.items())


def circular_producers_unchanged():
    return all(all(getattr(getattr(kind, name), "__func__", getattr(kind, name)) is producer
                   for name, producer in _INTRINSIC_PRODUCERS[kind].items())
               for kind in (cq.Face, cq.Edge))


MAX_CONSTANTS = 256
MAX_CONSTANT_VALUES = 16


def module_constants(namespace, adapted=None):
    """Numeric module-level constants of the entry file, for `model` provenance.

    Only plain finite int/float values and short tuples/lists of them count;
    underscore names, helpers injected by the worker and callables never do.
    Values are the raw numbers in the source (their unit is the author's).
    """
    def number(value):
        return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)
    # Project models run through a generated adapter that loads the user's
    # file as `cadpad_source`; its constants are the model's, the adapter's
    # own namespace holds none. The model file wins on a name clash.
    merged = dict(namespace)
    if adapted:
        merged.update(adapted)
    found = []
    for name, value in merged.items():
        if name.startswith("_") or not name.isidentifier():
            continue
        if number(value):
            values = [float(value)]
        elif (isinstance(value, (tuple, list)) and 0 < len(value) <= MAX_CONSTANT_VALUES
              and all(number(item) for item in value)):
            values = [float(item) for item in value]
        else:
            continue
        found.append(dict(name=name, values=values))
    return sorted(found, key=lambda item: item["name"])[:MAX_CONSTANTS]


@dataclass
class Part:
    name: str
    shape: cq.Shape
    position: tuple
    features: dict
    density: float | None
    fingerprint: str | None = None
    source: dict | None = None
    # Enclosing project function of the publishing show() call; None at module level.
    function: str | None = None
    fastener: dict | None = None
    # How the part was positioned (place()/position/assembly/source); see
    # cadpad_worker.placement. Emitted in the build result's `placements` map.
    placement: dict | None = None
    # Source STEP assets used to make this published part. Path + digest lets
    # requirement relinking distinguish an imported-file replacement from an
    # ordinary geometry edit.
    source_assets: list[dict] | None = None


class Model:
    """One immutable revision per process; candidates never replace committed shapes."""

    def __init__(self, cache: Path):
        self.cache = cache.resolve()
        self.cache.mkdir(parents=True, exist_ok=True)
        self.parts = {}
        self.parameters = {}
        self.annotations = []
        self.revision = None
        self.built = False
        self._meshes = {}
        self._intrinsic_properties = {}
        self._publication_result = None
        self._geometry_anchors = {}
        self._display_policy = None
        self._mesh_reuse = {}
        self._geometry_cache = None
        self._history_names = None
        self._history_namer = None
        self._history_features = None
        # Scalar results only; bounded across poses in this immutable revision.
        self._interference_results = OrderedDict()
        self._source_assets = {}

    def artifact(self, data: bytes, suffix: str):
        digest = hashlib.sha256(data).hexdigest()
        handle = digest + suffix if suffix in (".bin", ".brep") else uuid.uuid4().hex + suffix
        target = self.cache / handle
        if target.is_symlink():
            raise DomainError("INVALID_ARTIFACT", "Artifact symlinks are not followed")
        if target.exists() and target.stat().st_size == len(data):
            hasher = hashlib.sha256()
            with target.open("rb") as stream:
                for block in iter(lambda: stream.read(65536), b""):
                    hasher.update(block)
            if hasher.hexdigest() == digest:
                return {"handle": handle, "bytes": len(data), "sha256": digest}
        temporary = self.cache / ("." + uuid.uuid4().hex + ".tmp")
        try:
            with temporary.open("xb") as stream:
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, target)
        finally:
            temporary.unlink(missing_ok=True)
        return {"handle": handle, "bytes": len(data), "sha256": digest}

    def build(self, args, revision):
        if self.built:
            raise DomainError("UNSUPPORTED_OPERATION", "Build requires a fresh candidate worker")
        self.built = True
        self.revision = revision
        # Parameter-sweep evaluation rebuilds the subject from these arguments.
        self._build_args = args
        source_root = Path(args["source_root"]).resolve(strict=True)
        entry = args.get("entry_point", "model.py")
        entry_path = (source_root / entry).resolve(strict=True)
        if not entry_path.is_relative_to(source_root):
            raise DomainError("BUILD_FAILED", "Entry point leaves workspace")
        dependencies = {name: importlib.metadata.version(name) for name in ("cadquery", "cadquery-ocp-novtk", "ocp-tessellate")}
        expected = args.get("expected_dependencies", {})
        # The headless package changes distribution names, not this pinned
        # modeling API. Accept only the exact pre-VTK-removal runtime; retain
        # actual installed identities in responses and cache policies.
        legacy_runtime = {"cadquery": "2.6.1", "cadquery-ocp": "7.8.1.1.post1",
                          "ocp-tessellate": "3.0.17"}
        headless_runtime = {"cadquery": "2.6.1+cadpad.1", "cadquery-ocp-novtk": "7.8.1.1.post1",
                            "ocp-tessellate": "3.0.17"}
        compatible_legacy = expected == legacy_runtime and dependencies == headless_runtime
        if expected and expected != dependencies and not compatible_legacy:
            raise DomainError("UNSUPPORTED_OPERATION", f"Project dependencies differ from worker: expected {expected}, installed {dependencies}")
        values = args.get("parameters", {})
        from .geometry_cache import Cache
        self._geometry_cache = Cache(self, args.get("input_digest"), args.get("geometry_cache"), geometry_fingerprint)

        def parameter(name, default, *, unit="mm", minimum=None, maximum=None, description=""):
            if name in self.parameters:
                raise DomainError("INVALID_PARAMETER", f"Duplicate parameter {name}")
            value = values.get(name, default)
            if isinstance(value, dict):
                if value.get("unit", unit) != unit:
                    raise DomainError("INVALID_PARAMETER", f"{name} unit differs from published {unit}")
                value = value["value"]
            if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
                raise DomainError("INVALID_PARAMETER", f"{name} must be a finite number")
            if (minimum is not None and value < minimum) or (maximum is not None and value > maximum):
                raise DomainError("INVALID_PARAMETER", f"{name} outside [{minimum}, {maximum}]")
            self.parameters[name] = dict(value=value, unit=unit, minimum=minimum, maximum=maximum, description=description)
            return value

        def call_site():
            # The innermost project-source frame is the component file that published the part.
            frame = sys._getframe(1)
            while frame is not None:
                try:
                    path = Path(frame.f_code.co_filename).resolve()
                except (OSError, ValueError):
                    path = None
                if path is not None and path.is_relative_to(source_root) and path.suffix == ".py":
                    # Comprehensions and lambdas belong to the function around them.
                    outer = frame
                    while outer is not None and outer.f_code.co_name.startswith("<") and outer.f_code.co_name != "<module>":
                        outer = outer.f_back
                    name = outer.f_code.co_name if outer is not None else "<module>"
                    return dict(file=path.relative_to(source_root).as_posix(), line=frame.f_lineno,
                                function=None if name.startswith("<") else name)
                frame = frame.f_back
            return None

        def record_step_source(path):
            site = call_site()
            if site is None:
                return
            key = (site["file"], site.get("function"))
            item = {
                "path": path.relative_to(source_root).as_posix(),
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
            assets = self._source_assets.setdefault(key, {})
            assets[item["path"]] = item

        def _origin_ref(value):
            """`origin="entry.py#builder"` provenance for chip→part mapping."""
            if value is None:
                return None
            if not isinstance(value, str):
                raise DomainError("INVALID_PARAMETER", "origin must be 'entry.py#builder'")
            entry, sep, builder = value.rpartition("#")
            if not sep or not entry.endswith(".py") or not builder or ":" in builder or builder.startswith("$"):
                raise DomainError("INVALID_PARAMETER", "origin must be 'entry.py#builder'")
            if not (source_root / entry).is_file():
                raise DomainError("INVALID_PARAMETER", f"origin entry does not exist: {entry}")
            return {"entry": entry, "builder": builder}

        def _placement_record(marks, position, origin):
            """Resolve how a published shape was positioned."""
            present = [mark for mark in marks if mark]
            if present and len(present) != len(marks):
                raise DomainError("INVALID_GEOMETRY",
                                  "Published shape mixes placed and unplaced geometry; show the placed part separately")
            distinct = {json.dumps(mark, sort_keys=True) for mark in present}
            if len(distinct) > 1:
                raise DomainError("INVALID_GEOMETRY", "Conflicting place() records on one published shape")
            record = json.loads(distinct.pop()) if distinct else None
            if record is None:
                record = {"method": "position" if tuple(position) != (0.0, 0.0, 0.0) else "source"}
            if not isinstance(record, dict) or not isinstance(record.get("method"), str):
                raise DomainError("INVALID_GEOMETRY", "Invalid placement record")
            record = dict(record)
            origin_ref = _origin_ref(origin) if origin is not None else record.get("origin")
            if origin_ref is not None:
                record["origin"] = origin_ref
            return record

        def show(obj, *, name, position=(0, 0, 0), features=None, density=None, kind=None,
                 fastener=None, origin=None, _source=None, _placement=None):
            source = _source or call_site()
            if not isinstance(name, str) or not name or name in self.parts:
                raise DomainError("INVALID_GEOMETRY", f"Published name must be unique: {name!r}")
            if isinstance(obj, cq.Assembly):
                if fastener is not None:
                    raise DomainError("INVALID_PARAMETER", "Fastener metadata must describe a published shape, not an assembly")
                def publish_assembly(assembly, prefix, parent_location):
                    location = parent_location * assembly.loc
                    if assembly.obj is not None:
                        shape = assembly.obj.val() if isinstance(assembly.obj, cq.Workplane) else assembly.obj
                        mark = getattr(shape, "_cadpad_placement", None)
                        placed = shape.moved(location)
                        show(placed, name=prefix, position=position, _source=source,
                             _placement={"method": "assembly",
                                         **({"placed": mark} if mark else {})})
                    for child in assembly.children:
                        publish_assembly(child, f"{prefix}/{child.name}", location)
                publish_assembly(obj, name, cq.Location())
                return
            marks = []
            if isinstance(obj, cq.Workplane):
                shapes = [s for s in obj.vals() if isinstance(s, cq.Shape)]
                marks = [getattr(s, "_cadpad_placement", None) for s in shapes]
                shape = cq.Compound.makeCompound(shapes) if len(shapes) > 1 else shapes[0] if shapes else None
            else:
                shape = obj
                marks = [getattr(shape, "_cadpad_placement", None)] if isinstance(shape, cq.Shape) else []
            if not isinstance(shape, cq.Shape) or shape.isNull() or not shape.isValid():
                raise DomainError("INVALID_GEOMETRY", f"Invalid published shape: {name}")
            if kind is not None and (kind not in ("solid", "surface", "wire") or
                    kind == "solid" and not shape.Solids() or
                    kind == "surface" and not shape.Faces() or
                    kind == "wire" and not shape.Wires()):
                raise DomainError("INVALID_GEOMETRY", f"{name} does not match intended {kind} geometry")
            if len(position) != 3:
                raise DomainError("INVALID_GEOMETRY", "position requires three millimetre coordinates")
            finite(position)
            if density is not None and (not math.isfinite(density) or density <= 0):
                raise DomainError("INVALID_PARAMETER", "Density must be positive kg/m³")
            fastener = validate_fastener(fastener)
            function = source.get("function") if source else None
            source_assets = (list(self._source_assets.get((source["file"], function), {}).values())
                             if source else [])
            source = dict(file=source["file"], line=source["line"]) if source else None
            if _placement is not None:
                marks = [_placement]
            self.parts[name] = Part(name, shape, tuple(position), features or {}, density,
                                    source=source, function=function, fastener=fastener,
                                    placement=_placement_record(marks, position, origin),
                                    source_assets=copy.deepcopy(source_assets) or None)

        def _selection_shapes(value):
            """cq entities inside a feature selection or Workplane."""
            if isinstance(value, cq.Workplane):
                items = value.vals()
            elif isinstance(value, (list, tuple)):
                items = value
            else:
                items = [value]
            return [item for item in items if isinstance(item, cq.Shape)]

        def _world(entity, part):
            """Published-part-local entity in assembly coordinates."""
            return entity.moved(cq.Location(cq.Vector(*part.position)))

        def _placement_target(on):
            from . import placement
            if on is None:
                raise DomainError("INVALID_PARAMETER", "place() needs on= (a face, edge, part or 'part#feature')")
            if isinstance(on, str):
                name, sep, feature = on.rpartition("#")
                if not sep:
                    name, feature = on, None
                host = self.parts.get(name)
                if host is None:
                    raise DomainError("INVALID_PARAMETER", f"place() target part is not published yet: {name!r}")
                if feature is None:
                    entity = _world(host.shape, host)
                    kind = "part"
                else:
                    if feature not in host.features:
                        raise DomainError("INVALID_PARAMETER",
                                          f"place() target {name!r} has no feature {feature!r}")
                    entities = _selection_shapes(host.features[feature])
                    if len(entities) != 1:
                        raise DomainError("INVALID_PARAMETER",
                                          f"place() target {name}#{feature} selects {len(entities)} entities; "
                                          "publish a single face or edge for placement")
                    entity = _world(entities[0], host)
                    kind = ("face" if isinstance(entity, cq.Face)
                            else "edge" if isinstance(entity, cq.Edge) else "part")
                description = {"kind": kind, "part": name}
                if feature is not None:
                    description["feature"] = feature
                return placement.target_from_entity(entity, description)
            if isinstance(on, cq.Assembly):
                raise DomainError("INVALID_PARAMETER", "place() on= does not accept an assembly")
            if isinstance(on, cq.Workplane):
                entities = _selection_shapes(on)
                if len(entities) != 1:
                    raise DomainError("INVALID_PARAMETER",
                                      f"place() on= selects {len(entities)} entities; "
                                      "select exactly one face, edge or part")
                on = entities[0]
            return placement.target_from_entity(on)

        def _placement_mate(mate):
            if mate is None:
                return None
            if isinstance(mate, cq.Face):
                return mate
            if isinstance(mate, cq.Workplane):
                faces = [item for item in mate.vals() if isinstance(item, cq.Face)]
                if len(faces) == 1:
                    return faces[0]
            raise DomainError("INVALID_PARAMETER",
                              "place() mate= must be a single cadquery face of the part being placed")

        def place(part, on=None, align="center", mate=None, offset=0.0, origin=None):
            """Position `part` on a target feature and return the placed shape.

            `on` is a planar `cq.Face`/`cq.Workplane` face selection, a `cq.Edge`
            (centres the part on the edge line, e.g. a bore over an axle), a
            whole `cq.Shape` (bounding-box top), or `"part#feature"` /
            `"part"` naming an already-published part and one of its
            `features=` selections. `align` is "center" (default), "min" or
            "max" inside the target's extent; `offset` lifts the part along
            the target normal in mm; `mate=` seats a specific face of the
            part onto the target with a rigid transform. Show the result
            directly: `show(place(lid, on="box#top"), name="lid")`.
            """
            from . import placement
            if isinstance(part, cq.Assembly):
                raise DomainError("INVALID_PARAMETER", "place() does not accept an assembly")
            if isinstance(part, cq.Workplane):
                shapes = [s for s in part.vals() if isinstance(s, cq.Shape)]
                shape = cq.Compound.makeCompound(shapes) if len(shapes) > 1 else shapes[0] if shapes else None
            else:
                shape = part
            if not isinstance(shape, cq.Shape) or shape.isNull() or not shape.isValid():
                raise DomainError("INVALID_GEOMETRY", "place() needs valid cadquery geometry")
            target = _placement_target(on)
            mate_face = _placement_mate(mate)
            location, record = placement.compute(shape, target, mate_face,
                                                 align=align, offset=offset)
            origin_ref = _origin_ref(origin)
            if origin_ref is not None:
                record["origin"] = origin_ref
            placed = shape.moved(location)
            placed._cadpad_placement = record
            return placed

        def dimension(name, part, start, end, *, label=None, parameter=None):
            if not isinstance(name, str) or not name or name.startswith(("saved:", "overall:")):
                raise DomainError("AMBIGUOUS_ANCHOR", "Dimension name is empty or uses a reserved namespace")
            if any(a["id"] == name for a in self.annotations):
                raise DomainError("AMBIGUOUS_ANCHOR", f"Duplicate dimension {name}")
            if len(start) != 3 or len(end) != 3:
                raise DomainError("AMBIGUOUS_ANCHOR", "Dimension anchors require three coordinates")
            finite([start, end])
            self.annotations.append(dict(id=name, kind="distance", part=part, start=list(start),
                                         end=list(end), label=label or name, parameter=parameter, unit="mm"))

        def angle(name, part, start, vertex, end, *, label=None, parameter=None):
            dimension(name, part, start, end, label=label, parameter=parameter)
            if len(vertex) != 3:
                raise DomainError("AMBIGUOUS_ANCHOR", "Angle vertex requires three coordinates")
            finite(vertex)
            self.annotations[-1].update(kind="angle",vertex=list(vertex),vertex_part=part,unit="rad",format={"unit":"deg","precision":2})

        def import_step(asset):
            path = (source_root / "assets" / asset).resolve(strict=True)
            if not path.is_relative_to(source_root / "assets"):
                raise DomainError("BUILD_FAILED", "Imported assets must stay inside assets/")
            record_step_source(path)
            return cq.importers.importStep(str(path))

        def import_step_named(asset, name="imported"):
            path = (source_root / "assets" / asset).resolve(strict=True)
            if not path.is_relative_to(source_root / "assets"):
                raise DomainError("BUILD_FAILED", "Imported assets must stay inside assets/")
            record_step_source(path)
            from .step_import import import_named_step
            return import_named_step(path, name)

        def import_stl(asset):
            path = (source_root / "assets" / asset).resolve(strict=True)
            if not path.is_relative_to(source_root / "assets"):
                raise DomainError("BUILD_FAILED", "Imported assets must stay inside assets/")
            from OCP.StlAPI import StlAPI_Reader
            from OCP.TopoDS import TopoDS_Shape
            shape = TopoDS_Shape()
            if not StlAPI_Reader().Read(shape, str(path)) or shape.IsNull():
                raise DomainError("INVALID_GEOMETRY", f"Could not read STL asset {asset}")
            return cq.Shape.cast(shape)

        fit_specs = []

        def fit_part(part, *, reference_stl, id=None, strictness="default", checks=None,
                     tolerances=None, probes=None, sections=None, on_fail="error",
                     sample_budget=256):
            fit_specs.append(dict(
                id=id, part=part, reference_stl=reference_stl, strictness=strictness,
                checks=checks, tolerances=tolerances, probes=probes, sections=sections,
                on_fail=on_fail, sample_budget=sample_budget,
            ))

        def show_object(obj, name=None, options=None, **kwargs):
            # CQ-editor scripts can run without rewriting their publication calls.
            label = name or f"part_{len(self.parts) + 1}"
            show(obj, name=label)

        helpers = dict(
            show=show, show_object=show_object, parameter=parameter, dimension=dimension, angle=angle,
            import_step=import_step, import_step_named=import_step_named,
            import_stl=import_stl, fit_part=fit_part, place=place,
            cached_shape=self._geometry_cache.shape,
        )
        context = SimpleNamespace(parameters=values, **helpers)
        history_names = args.get("history_names", False)
        if not isinstance(history_names, bool):
            raise DomainError("INVALID_REQUEST", "history_names must be a boolean opt-in")
        if history_names:
            from .history_names import Namer
            self._history_namer = Namer(source_root)
        from .edge_trace import OperationTrace
        self._operation_trace = OperationTrace(source_root)
        profile = self._operation_trace.profile
        if self._history_namer is not None:
            def profile(frame, event, result):
                self._operation_trace.profile(frame, event, result)
                self._history_namer.profile(frame, event, result)
        old_profile = sys.getprofile()
        if history_names and old_profile is not None:
            raise DomainError("HISTORY_TRACE_UNAVAILABLE", "History naming requires an exclusive worker trace hook")
        old_path, old_cwd = sys.path[:], Path.cwd()
        try:
            sys.path.insert(0, str(source_root))
            if entry_path.parent != source_root:
                sys.path.insert(0, str(entry_path.parent))
            os.chdir(source_root)
            if old_profile is None:
                sys.setprofile(profile)
            namespace = runpy.run_path(str(entry_path), init_globals=dict(helpers), run_name="__main__")
            if callable(namespace.get("build")):
                namespace["build"](context)
            if not self.parts and isinstance(namespace.get("result"), (cq.Workplane, cq.Shape, cq.Assembly)):
                show(namespace["result"], name="result")
        finally:
            if old_profile is None and sys.getprofile() is profile:
                sys.setprofile(None)
            os.chdir(old_cwd)
            sys.path[:] = old_path
        if history_names:
            self._history_names = {}
            for part in self.parts.values():
                table = self._history_namer.snapshot(part.shape)
                index = {"face": {}, "edge": {}}
                collections = {"face": part.shape.Faces(), "edge": part.shape.Edges()}
                entity_ids = {kind: {entity: entity_index for entity_index, entity in enumerate(items)}
                              for kind, items in collections.items()}
                for name, (kind, entities) in table.items():
                    for entity in entities:
                        entity_index = entity_ids[kind].get(entity)
                        if entity_index is not None:
                            index[kind].setdefault(entity_index, set()).add(name)
                self._history_names[part.name] = index
        starter_entry = entry_path.read_text().strip() in ("# Describe a part to get started.", "# Cadpad project browser. Select a part in Explorer.")
        if not self.parts and not (args.get("allow_empty", False) or starter_entry):
            raise DomainError("INVALID_GEOMETRY", "Model did not publish geometry. Use show_object(result), show(result, name=...), or assign the model to result.")
        for annotation in self.annotations:
            if annotation["part"] not in self.parts or (annotation["parameter"] is not None and annotation["parameter"] not in self.parameters):
                raise DomainError("AMBIGUOUS_ANCHOR", f"Unresolved dimension {annotation['id']}")
            if annotation["kind"] == "angle":
                from .annotations import angle_value
                annotation["value"] = angle_value(annotation["start"], annotation["vertex"], annotation["end"])
                if annotation["parameter"] is not None and self.parameters[annotation["parameter"]]["unit"] not in ("deg", "rad"):
                    raise DomainError("INVALID_PARAMETER", "Angle parameter binding requires a deg/rad parameter")
            else:
                annotation["value"] = math.dist(annotation["start"], annotation["end"])
                if annotation["parameter"] is not None and self.parameters[annotation["parameter"]]["unit"] != "mm":
                    raise DomainError("INVALID_PARAMETER", "Length parameter binding requires an mm parameter")
        from .annotations import resolve, automatic_circles
        self.annotations.extend(resolve(self, args.get("annotations", [])))
        self.annotations.extend(automatic_circles(self))
        from .collision import validate_proxies
        collision_validation = validate_proxies(self, args.get("collision"))
        from .fit import evaluate_all, enforce
        fit_results = evaluate_all(fit_specs, self.parts, source_root)
        enforce(fit_results)
        for part in self.parts.values():
            if part.fastener is not None:
                missing = sorted(set(part.fastener.get("mates", ())) - self.parts.keys())
                if missing:
                    raise DomainError("INVALID_PARAMETER", f"Fastener {part.name} references unpublished mates: {missing}")
        generate_mesh = args.get("generate_mesh", True)
        if not isinstance(generate_mesh, bool):
            raise DomainError("INVALID_REQUEST", "generate_mesh must be boolean")
        from .display_cache import policy, offers
        self._display_policy = policy(self, geometry_fingerprint, FINGERPRINT_FORMAT)
        self._mesh_reuse = offers(args)
        # Snapshot shared occurrences before any of them can be tessellated.
        if len(self.parts) > 1:
            for part in self.parts.values():
                part.fingerprint = geometry_fingerprint(part.shape)
        result = dict(collision_validation=collision_validation, constants=module_constants(namespace, getattr(sys.modules.get('cadpad_source'), '__dict__', None)), parts=[self.describe(part, mesh=generate_mesh) for part in self.parts.values()],
                      parameters=self.parameters, annotations=self.annotations,
                      units="mm", revision=revision,
                      dependencies=dependencies)
        # Omit when unused so recovery contracts stay identical for ordinary projects.
        if fit_results:
            result["fit_results"] = fit_results
        # Placement evidence for requirements (spec §3.2/§6.4): only emitted
        # once `place()` is used, so rebuilding older sources stays identical.
        placements = {part.name: part.placement for part in self.parts.values() if part.placement}
        if any(record.get("method") == "place" for record in placements.values()):
            result["placements"] = placements
        self._publication_result = finite(result)
        self._mesh_reuse = {}
        return self._publication_result

    def remesh(self, args=None):
        if self._publication_result is None:
            raise DomainError("UNSUPPORTED_OPERATION", "Build valid geometry before remeshing")
        self._meshes.clear()
        from .display_cache import policy, offers
        self._display_policy = policy(self, geometry_fingerprint, FINGERPRINT_FORMAT)
        self._mesh_reuse = offers(args or {})
        result = copy.deepcopy(self._publication_result)
        for item in result["parts"]:
            part = self.parts[item["instance_id"]]
            if geometry_fingerprint(part.shape) != part.fingerprint:
                raise DomainError("RECOVERY_GEOMETRY_CHANGED", "Geometry changed before cache repair")
            item["mesh"] = self.mesh(part.shape, part.fingerprint)
        self._mesh_reuse = {}
        return finite(result)

    def describe(self, part, mesh=False):
        shape = part.shape
        fingerprint = geometry_fingerprint(shape)
        if part.fingerprint is not None and fingerprint != part.fingerprint:
            raise DomainError("RECOVERY_GEOMETRY_CHANGED", "Geometry changed during publication")
        part.fingerprint = fingerprint
        # Exact intrinsic values depend on BRep, not an occurrence's name,
        # transform, density or semantic feature selections. Never share mutable
        # result dictionaries with the cache or another occurrence.
        reusable = intrinsic_reuse_allowed(shape)
        intrinsic = self._intrinsic_properties.get(fingerprint) if reusable else None
        if intrinsic is None:
            intrinsic = finite(dict(
                bounds=bounds(shape), volume=shape.Volume() if shape.Solids() else None,
                area=shape.Area(), shape_kind=shape.ShapeType(),
                face_count=len(shape.Faces()), edge_count=len(shape.Edges())))
            if reusable:
                self._intrinsic_properties[fingerprint] = intrinsic
        intrinsic = copy.deepcopy(intrinsic)
        feature_entities = {}
        feature_kinds = {}
        if part.features:
            faces, edges = shape.Faces(), shape.Edges()
            for name, selected in part.features.items():
                selected = selected.vals() if isinstance(selected, cq.Workplane) else [selected]
                feature_entities[name] = [f"{kind}:{i}" for kind, entities in (("face", faces), ("edge", edges))
                                          for i, entity in enumerate(entities) if any(entity.isSame(s) for s in selected)]
                ids = feature_entities[name]
                n_faces = sum(1 for item in ids if item.startswith("face:"))
                n_edges = len(ids) - n_faces
                # The hash identifies the selection within this exact geometry.
                digest = hashlib.sha256(json.dumps([part.fingerprint, name, sorted(ids)]).encode()).hexdigest()
                feature_kinds[name] = dict(entity="mixed" if n_faces and n_edges else "edge" if n_edges else "face",
                                           count=len(ids), faces=n_faces, edges=n_edges, hash=digest)
        result = dict(id=hashlib.sha256(part.name.encode()).hexdigest()[:24], instance_id=part.name,
                      name=part.name, position=list(part.position), **intrinsic,
                      features=feature_entities, feature_kinds=feature_kinds, accuracy="exact", units="mm",
                      geometry_fingerprint=dict(format=FINGERPRINT_FORMAT, sha256=part.fingerprint))
        if part.source is not None:
            result["source"] = dict(part.source)
        if part.source_assets:
            result["source_assets"] = sorted(part.source_assets, key=lambda item: item["path"])
        if part.function is not None:
            result["builder_function"] = part.function
        if part.fastener is not None:
            result["fastener"] = copy.deepcopy(part.fastener)
        if part.density is not None and result["volume"] is not None:
            result["mass_kg"] = result["volume"] * 1e-9 * part.density
        result["dimensions"] = [dict(id=f"{part.name}:{axis}", kind="overall", axis=axis, value=value, unit="mm")
                                for axis, value in zip("xyz", result["bounds"]["size"])]
        if mesh:
            result["mesh"] = self.mesh(shape, part.fingerprint)
        return result

    def mesh(self, shape, fingerprint=None):
        fingerprint = fingerprint or geometry_fingerprint(shape)
        if fingerprint in self._meshes:
            return self._meshes[fingerprint]
        from .display_cache import take
        reused = take(self.cache, fingerprint, self._display_policy, self._mesh_reuse)
        if reused is not None:
            self._meshes[fingerprint] = reused
            return reused
        # Retain tessellator face/edge spans for revision-scoped picking.
        # Buffers are little-endian f32/u32 artifacts.
        from ocp_tessellate.ocp_utils import get_faces, get_edges
        from ocp_tessellate.tessellator import discretize_edges, tessellate
        from OCP.BRepAdaptor import BRepAdaptor_Surface
        from OCP.BRepGProp import BRepGProp
        from OCP.BRepTools import BRepTools
        from OCP.GeomAbs import GeomAbs_Plane
        from OCP.GProp import GProp_GProps
        from OCP.TopAbs import TopAbs_EDGE
        from OCP.TopExp import TopExp_Explorer
        from OCP.TopTools import TopTools_IndexedMapOfShape
        from OCP.BRep import BRep_Tool
        import numpy as np
        displayed_faces = list(get_faces(shape.wrapped))
        # STL import is a shell of one triangular face per facet, not a solid.
        # Those edges are the triangle boundaries. Publishing them makes the
        # viewer draw a screen-space line over every facet.
        triangle_mesh = not shape.Solids() and len(displayed_faces) > 1
        if triangle_mesh:
            for face in displayed_faces:
                count = 0
                explorer = TopExp_Explorer(face, TopAbs_EDGE)
                while explorer.More():
                    count += 1
                    if count > 3:
                        break
                    explorer.Next()
                if count != 3:
                    triangle_mesh = False
                    break
        displayed_edges = [] if triangle_mesh else list(get_edges(shape.wrapped))
        def tessellate_at(deflection):
            return tessellate(
                shape.wrapped, cache_key=f"cadpad-mesh-v2:{FINGERPRINT_FORMAT}:{fingerprint}:quality={deflection}",
                deviation=deflection, quality=deflection, angular_tolerance=0.2,
                compute_faces=True, compute_edges=bool(displayed_edges),
            )

        def missing_planar_area(mesh, deflection):
            counts = mesh["triangles_per_face"]
            if len(counts) != len(displayed_faces):
                return float("inf")
            vertices = np.asarray(mesh["vertices"], dtype=np.float64).reshape(-1, 3)
            triangles = np.asarray(mesh["triangles"]).reshape(-1, 3)
            area_props, length_props = GProp_GProps(), GProp_GProps()
            missing = 0.0
            start = 0
            for face, count in zip(displayed_faces, counts):
                end = start + int(count)
                if BRepAdaptor_Surface(face).GetType() == GeomAbs_Plane:
                    a, b, c = vertices[triangles[start:end]].transpose(1, 0, 2)
                    area = np.linalg.norm(np.cross(b - a, c - a), axis=1).sum() / 2
                    BRepGProp.SurfaceProperties_s(face, area_props)
                    exact = area_props.Mass()
                    deficit = exact - area
                    if deficit > exact * 1e-5:
                        # Curved boundaries are approximated by chords. Their
                        # expected area error scales with perimeter and deflection.
                        BRepGProp.LinearProperties_s(face, length_props)
                        if deficit > length_props.Mass() * deflection:
                            missing += deficit
                start = end
            return missing

        tess = tessellate_at(0.1)
        if not triangle_mesh and displayed_faces:
            missing = missing_planar_area(tess, 0.1)
            if missing > 0:
                # OCCT can leave gaps in planar faces at coarse deflection even
                # when meshing reports success. Regenerate faces and edges together.
                BRepTools.Clean_s(shape.wrapped)
                refined = tessellate_at(0.001)
                if missing_planar_area(refined, 0.001) < missing:
                    tess = refined
        # The face tessellator omits standalone edges, including loose wires in
        # compounds that also contain faces. Discretize the original edge list
        # through the publisher's edge API so spans keep exact topology order.
        if len(displayed_edges) != len(tess["segments_per_edge"]):
            tess = dict(tess, **discretize_edges(displayed_edges, deflection=0.1))
        if not displayed_edges:
            # No topological edges to draw. Drop the tessellator's fallback,
            # which otherwise emits every triangle edge.
            tess = dict(tess, edges=[], segments_per_edge=[], edge_types=[])
        mappings = {}
        for kind, get_published, displayed, counts in (
                ("face", shape.Faces, displayed_faces, tess["triangles_per_face"]),
                ("edge", shape.Edges, displayed_edges, tess["segments_per_edge"])):
            if len(displayed) != len(counts):
                raise DomainError("INVALID_GEOMETRY", "Tessellator topology does not match the published shape")
            if not displayed:
                mappings[kind] = []
                continue
            indices = TopTools_IndexedMapOfShape()
            for entity in get_published():
                indices.Add(entity.wrapped)
            identifiers = []
            for entity in displayed:
                index = indices.FindIndex(entity)
                if index:
                    identifiers.append(f"{kind}:{index-1}")
                elif kind == "edge" and BRep_Tool.Degenerated_s(entity):
                    identifiers.append(None)  # Sphere poles have no CadQuery edge identity.
                else:
                    raise DomainError("INVALID_GEOMETRY", "Display topology has no published entity")
            mappings[kind] = identifiers
        buffers = {}
        for key, dtype in (("vertices", "f"), ("normals", "f"), ("triangles", "I"),
                           ("edges", "f"), ("triangles_per_face", "I"), ("segments_per_edge", "I"), ("obj_vertices", "f"), ("face_types", "I"), ("edge_types", "I")):
            value = tess.get(key)
            if value is not None:
                array = np.asarray(value, dtype="<f4" if dtype == "f" else "<u4")
                if dtype == "f" and not np.isfinite(array).all():
                    raise DomainError("INVALID_GEOMETRY", "Geometry contains non-finite values")
                buffers[key] = self.artifact(array.tobytes(), ".bin")
        result = dict(buffers=buffers, face_ids=mappings["face"],
                      edge_ids=mappings["edge"],
                      format="cadpad-mesh-v2", tolerance_mm=0.1,
                      cache_policy=self._display_policy)
        self._meshes[fingerprint] = result
        return result

    def entity(self, ref):
        if ref.get("revision") != self.revision:
            raise DomainError("STALE_ENTITY", "Selection belongs to a different revision")
        try:
            part = self.parts[ref["instance_id"]]
            kind, index = ref.get("entity_id", "part:0").split(":")
            if kind not in ("face", "edge", "part"):
                raise ValueError()
            shape = part.shape if kind == "part" else (part.shape.Faces() if kind == "face" else part.shape.Edges())[int(index)]
            if int(index) < 0 or (kind == "part" and index != "0"):
                raise ValueError()
            return part, shape, kind
        except (KeyError, ValueError, IndexError):
            raise DomainError("STALE_ENTITY", "Unknown geometry entity") from None

    def inspect(self, args):
        if args.get("history_names") is True and self._history_names is None:
            self._trace_history_names()
        entities = args.get("entities", [])
        poses = args.get("poses", {})
        def world(ref):
            part, shape, _ = self.entity(ref)
            pose = poses.get(part.name, dict(position=part.position, quaternion=[0, 0, 0, 1]))
            return shape.moved(self.location(pose))
        if args.get("query") == "interference":
            from OCP.Precision import Precision
            from OCP.ShapeAnalysis import ShapeAnalysis_ShapeTolerance
            pairs = args.get("pairs", [])
            if not 1 <= len(pairs) <= 64:
                raise DomainError("INVALID_REQUEST", "Exact interference requires 1..64 instance pairs")
            results = []
            prepared = {}
            solid_shapes = set()
            def prepare(name):
                if name not in prepared:
                    part, shape, _ = self.entity(dict(revision=self.revision, instance_id=name, entity_id="part:0"))
                    pose = poses.get(name, dict(position=part.position, quaternion=[0, 0, 0, 1]))
                    location = self.location(pose)
                    key = (shape, shape.wrapped.Orientation(), tuple(pose.get("position", [0, 0, 0])),
                           tuple(pose.get("quaternion", [0, 0, 0, 1])))
                    prepared[name] = (key, shape, location)
                return prepared[name]
            transformed = {}
            def placed(name):
                if name not in transformed:
                    _, shape, location = prepared[name]
                    if shape not in solid_shapes:
                        if not shape.Solids():
                            raise DomainError("UNSUPPORTED_OPERATION", "Volumetric interference requires solid geometry")
                        solid_shapes.add(shape)
                    transformed[name] = shape.moved(location)
                return transformed[name]
            tolerances = {}
            def tolerance(name):
                shape = prepared[name][1]
                if shape not in tolerances:
                    tolerances[shape] = max(Precision.Confusion_s(),
                        ShapeAnalysis_ShapeTolerance().Tolerance(shape.wrapped, 1))
                return tolerances[shape]
            boxes = {}
            def box(name):
                if name not in boxes:
                    value = bounds(placed(name))
                    scale = max(abs(v) for side in ("min", "max") for v in value[side])
                    margin = 2 * tolerance(name) + 16 * sys.float_info.epsilon * scale
                    boxes[name] = {"min": [v - margin for v in value["min"]],
                                   "max": [v + margin for v in value["max"]]}
                return boxes[name]
            for first, second in pairs:
                # Canonical order reuses reversed queries without changing reply order.
                left, right = sorted((first, second))
                ka, _, _ = prepare(left)
                kb, _, _ = prepare(right)
                key = (self.revision, ka, kb)
                measured = self._interference_results.get(key)
                if measured is None:
                    a, b = placed(left), placed(right)
                    volume, interfering = 0.0, False
                    # Expand by stored CAD tolerances, including imports whose
                    # tolerances exceed Confusion(). Bounds can prove separation
                    # even for compounds; whole-shape distance cannot (it may be
                    # positive when a compound contains another solid).
                    ba, bb = box(left), box(right)
                    separated = any(ba["min"][i] > bb["max"][i] or
                                    bb["min"][i] > ba["max"][i] for i in range(3))
                    if not separated:
                        overlap = a.intersect(b)
                        if not overlap.isValid():
                            raise DomainError("INVALID_GEOMETRY", "BRep interference boolean did not produce valid geometry")
                        interfering = bool(overlap.Solids())
                        volume = overlap.Volume() if interfering else 0.0
                    # Preserve the original kernel operation order for nearby
                    # pairs. In overlap-heavy scenes this is measurably faster
                    # than computing distance before the intersection.
                    distance = a.distance(b)
                    measured = (finite(distance), finite(volume), interfering)
                    self._interference_results[key] = measured
                    if len(self._interference_results) > 256:
                        self._interference_results.popitem(last=False)
                self._interference_results.move_to_end(key)
                distance, volume, interfering = measured
                results.append(dict(instances=[first, second], distance_mm=distance,
                                    intersection_volume_mm3=volume, interfering=interfering))
            result = dict(pairs=results, kernel_tolerance_mm=Precision.Confusion_s())
        elif args.get("query") == "distance":
            if len(entities) != 2:
                raise DomainError("STALE_ENTITY", "Distance needs two entities")
            result = dict(distance=world(entities[0]).distance(world(entities[1])))
        else:
            result = {"entities": []}
            for ref in entities:
                part, shape, kind = self.entity(ref)
                item = dict(reference=ref, bounds=bounds(shape), center=vector(shape.Center()), kind=kind,
                            coordinate_space="part_local", world_bounds=bounds(world(ref)), world_center=vector(world(ref).Center()))
                if kind in ("face", "edge"):
                    from .annotations import geometry_reference
                    reference = geometry_reference(shape)
                    item["anchor"] = dict(kind="geometry_center", instance_id=part.name, entity=reference)
                    if (kind == "edge" and shape.geomType() == "CIRCLE") or (kind == "face" and shape.geomType() == "CYLINDER"):
                        item["circular_definition"] = dict(kind="geometry_circle", instance_id=part.name, entity=reference, measure="diameter")
                item["semantic_labels"] = [name for name, selected in part.features.items()
                                           if any(shape.isSame(entity) for entity in
                                                  (selected.vals() if isinstance(selected, cq.Workplane) else [selected]))]
                if self._history_names is not None and kind in ("face", "edge"):
                    from .history_names import positional_name
                    names = sorted(self._history_names.get(part.name, {}).get(kind, {}).get(
                        int(ref["entity_id"].split(":", 1)[1]), set()))
                    exact_hash = geometry_fingerprint(shape)
                    features = self._history_feature_index().get(part.name, {})
                    item["history_references"] = []
                    for name in names:
                        feature = features[name]
                        positional = (positional_name(name)
                                      or name in self._history_namer.positional_names)
                        item["history_references"].append(dict(
                            name=name, entity_kind=kind, exact_hash=exact_hash,
                            count=feature["count"], set_hash=feature["set_hash"],
                            positional=positional,
                            basis=("positional_fallback" if positional else
                                   "occt_history" if name in self._history_namer.history_names
                                   else "source_role")))
                    item["history_references_status"] = "available" if names else "unavailable"
                    if not names:
                        item["history_references_reason"] = "no_reliable_name"
                if kind == "part":
                    item.update(self.describe(part))
                elif kind == "face":
                    item.update(surface_type=shape.geomType(), area=shape.Area())
                    if shape.geomType() == "PLANE":
                        item["normal"] = vector(shape.normalAt())
                        item["world_normal"] = vector(world(ref).normalAt())
                    if shape.geomType() in ("CYLINDER", "SPHERE", "TORUS"):
                        from OCP.BRepAdaptor import BRepAdaptor_Surface
                        surface = BRepAdaptor_Surface(shape.wrapped)
                        if shape.geomType() == "CYLINDER":
                            item["radius"] = surface.Cylinder().Radius()
                        elif shape.geomType() == "SPHERE":
                            item["radius"] = surface.Sphere().Radius()
                elif kind == "edge":
                    item.update(curve_type=shape.geomType(), length=shape.Length())
                    if shape.geomType() == "CIRCLE":
                        item["radius"] = shape.radius()
                result["entities"].append(item)
        result = dict(**result, revision=self.revision, pose_version=args.get("pose_version", 0), accuracy="exact", units="mm")
        if args.get("query") not in ("distance", "interference"):
            result["history_names"] = {"status": "traced" if self._history_names is not None else "unavailable",
                                        **({"reason": "not_traced"} if self._history_names is None else {})}
        return finite(result)

    def resolve_features(self, args):
        """Resolve named history or exact geometric references on this build."""
        references = args.get("references", [])
        if not isinstance(references, list) or len(references) > 64:
            raise DomainError("INVALID_REQUEST", "references must be an array of at most 64 items")
        if any(not isinstance(ref, dict) for ref in references):
            raise DomainError("INVALID_REQUEST", "Each feature reference must be an object")
        if any(ref.get("kind") == "history" for ref in references) and self._history_names is None:
            self._trace_history_names()
        history = self._history_feature_index() if self._history_names is not None else {}
        traced_parts = sorted({ref.get("part") for ref in references if ref.get("kind") == "history"})
        resolved = []
        for ref in references:
            part_name = ref.get("part")
            part = self.parts.get(part_name)
            if part is None:
                resolved.append(dict(status="missing_part"))
                continue
            if ref.get("kind") == "history":
                name = ref.get("name")
                feature = history.get(part_name, {}).get(name)
                if feature is None:
                    resolved.append(dict(status="unavailable", reason="no_reliable_name"))
                    continue
                resolved.append(dict(status="resolved", entity=feature["kind"], count=feature["count"],
                                     hash=feature["set_hash"], positional=feature["positional"]))
            elif ref.get("kind") == "geometric":
                entity_kind, expected_hash = ref.get("entity"), ref.get("hash")
                if entity_kind not in ("face", "edge") or not isinstance(expected_hash, str):
                    raise DomainError("INVALID_REQUEST", "Geometric feature needs face/edge and exact hash")
                shapes = part.shape.Faces() if entity_kind == "face" else part.shape.Edges()
                matches = [shape for shape in shapes if geometry_fingerprint(shape) == expected_hash]
                status = "resolved" if len(matches) == 1 else "ambiguous" if matches else "missing"
                resolved.append(dict(status=status, entity=entity_kind, count=len(matches), hash=expected_hash,
                                     positional=False))
            else:
                raise DomainError("INVALID_REQUEST", "Feature reference kind must be history or geometric")
        history_index = {
            part: [dict(name=name, entity=value["kind"], count=value["count"],
                        hash=value["set_hash"], positional=value["positional"])
                   for name, value in sorted(history.get(part, {}).items())]
            for part in traced_parts
        }
        return finite(dict(references=resolved, history_index=history_index, revision=self.revision))

    def _trace_history_names(self):
        """Replay this build with tracing and accept names only for identical BReps."""
        if self._build_args is None or self._publication_result is None:
            raise DomainError("HISTORY_TRACE_UNAVAILABLE", "Build inputs are not available for history tracing")
        published = {
            part["instance_id"]: tuple(part.get("position", ()))
            for part in self._publication_result.get("parts", [])
        }
        expected = {}
        for part in self.parts.values():
            live_hash = geometry_fingerprint(part.shape)
            position = tuple(part.position)
            if (part.fingerprint is None or live_hash != part.fingerprint
                    or published.get(part.name) != position):
                raise DomainError(
                    "HISTORY_TRACE_MISMATCH",
                    f"Part {part.name} geometry or placement changed after build; no history names were attached",
                )
            expected[part.name] = (live_hash, position)
        trace_args = copy.deepcopy(self._build_args)
        trace_args.update(history_names=True, generate_mesh=False, geometry_cache=None, input_digest=None)
        try:
            with tempfile.TemporaryDirectory(prefix="history-trace-", dir=self.cache) as temporary:
                candidate = Model(Path(temporary) / "cache")
                result = candidate.build(trace_args, self.revision)
                actual = {
                    part["instance_id"]: (
                        part.get("geometry_fingerprint", {}).get("sha256"),
                        tuple(part.get("position", ())),
                    )
                    for part in result.get("parts", [])
                }
                if not expected or actual != expected:
                    raise DomainError(
                        "HISTORY_TRACE_MISMATCH",
                        "History tracing replay did not reproduce the exact committed geometry; no names were attached",
                    )
                self._history_names = candidate._history_names
                self._history_namer = candidate._history_namer
                self._history_features = None
        except DomainError:
            raise
        except Exception as error:
            raise DomainError("HISTORY_TRACE_UNAVAILABLE", f"Could not trace this exact build: {error}") from None

    def _history_feature_index(self):
        if self._history_features is None:
            from .history_names import positional_name
            index = {}
            for part in self.parts.values():
                groups = {}
                collections = {"face": part.shape.Faces(), "edge": part.shape.Edges()}
                for kind, items in collections.items():
                    by_index = self._history_names.get(part.name, {}).get(kind, {})
                    for entity_index, names in by_index.items():
                        shape = items[entity_index]
                        entity_hash = geometry_fingerprint(shape)
                        for name in names:
                            group = groups.setdefault(name, {"kind": kind, "hashes": [], "positional": False})
                            if group["kind"] != kind:
                                raise DomainError("HISTORY_TRACE_UNAVAILABLE", "One history name maps to mixed entity kinds")
                            group["hashes"].append(entity_hash)
                            group["positional"] |= (positional_name(name)
                                or name in self._history_namer.positional_names)
                index[part.name] = {}
                for name, group in groups.items():
                    hashes = sorted(group["hashes"])
                    canonical = json.dumps([group["kind"], hashes], separators=(",", ":")).encode()
                    index[part.name][name] = {
                        "kind": group["kind"],
                        "count": len(hashes),
                        "set_hash": hashlib.sha256(canonical).hexdigest(),
                        "positional": group["positional"],
                    }
            self._history_features = index
        return self._history_features

    @staticmethod
    def location(pose):
        from OCP.gp import gp_Trsf, gp_Quaternion, gp_Vec
        position = pose.get("position", [0, 0, 0])
        quaternion = pose.get("quaternion", [0, 0, 0, 1])
        finite([position, quaternion])
        if len(position) != 3 or len(quaternion) != 4 or abs(sum(x*x for x in quaternion)-1) > 1e-6:
            raise DomainError("INVALID_PARAMETER", "Pose requires mm position and normalized xyzw quaternion")
        transform = gp_Trsf()
        transform.SetRotation(gp_Quaternion(*quaternion))
        transform.SetTranslationPart(gp_Vec(*position))
        return cq.Location(transform)

    def export(self, args):
        if not self.parts:
            raise DomainError("UNSUPPORTED_OPERATION", "Empty scratchpad has no geometry to export")
        format = args.get("format", "STEP").upper()
        if format not in ("STEP", "STL"):
            raise DomainError("UNSUPPORTED_OPERATION", f"Cannot export {format}")
        poses = args.get("poses", {})
        shapes = [part.shape.moved(self.location(poses.get(name, dict(position=part.position))))
                  for name, part in self.parts.items()]
        compound = cq.Compound.makeCompound(shapes)
        handle = uuid.uuid4().hex + "." + format.lower()
        path = self.cache / handle
        if format == "STL":
            from .stl import export as export_stl
            try:
                export_stl(compound, path)
            except ValueError as error:
                raise DomainError("INVALID_GEOMETRY", str(error)) from error
        else:
            cq.exporters.export(compound, str(path), exportType=format)
        data = path.read_bytes()
        return dict(handle=handle, bytes=len(data), sha256=hashlib.sha256(data).hexdigest(),
                    format=format, units="mm", revision=self.revision, pose_version=args.get("pose_version", 0))
