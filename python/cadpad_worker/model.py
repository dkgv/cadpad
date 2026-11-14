from __future__ import annotations

import copy
import io
import hashlib
import importlib.metadata
import json
import math
import os
import runpy
import struct
import sys
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


@dataclass
class Part:
    name: str
    shape: cq.Shape
    position: tuple
    features: dict
    density: float | None
    fingerprint: str | None = None
    source: dict | None = None


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
        # Scalar results only; bounded across poses in this immutable revision.
        self._interference_results = OrderedDict()

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
        source_root = Path(args["source_root"]).resolve(strict=True)
        entry = args.get("entry_point", "model.py")
        entry_path = (source_root / entry).resolve(strict=True)
        if not entry_path.is_relative_to(source_root):
            raise DomainError("BUILD_FAILED", "Entry point leaves workspace")
        dependencies = {name: importlib.metadata.version(name) for name in ("cadquery", "cadquery-ocp", "ocp-tessellate")}
        expected = args.get("expected_dependencies", {})
        if expected and expected != dependencies:
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
                    return dict(file=path.relative_to(source_root).as_posix(), line=frame.f_lineno)
                frame = frame.f_back
            return None

        def show(obj, *, name, position=(0, 0, 0), features=None, density=None, kind=None, _source=None):
            source = _source or call_site()
            if not isinstance(name, str) or not name or name in self.parts:
                raise DomainError("INVALID_GEOMETRY", f"Published name must be unique: {name!r}")
            if isinstance(obj, cq.Assembly):
                def publish_assembly(assembly, prefix, parent_location):
                    location = parent_location * assembly.loc
                    if assembly.obj is not None:
                        shape = assembly.obj.val() if isinstance(assembly.obj, cq.Workplane) else assembly.obj
                        show(shape.moved(location), name=prefix, position=position, _source=source)
                    for child in assembly.children:
                        publish_assembly(child, f"{prefix}/{child.name}", location)
                publish_assembly(obj, name, cq.Location())
                return
            if isinstance(obj, cq.Workplane):
                shapes = [s for s in obj.vals() if isinstance(s, cq.Shape)]
                shape = cq.Compound.makeCompound(shapes) if len(shapes) > 1 else shapes[0] if shapes else None
            else:
                shape = obj
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
            self.parts[name] = Part(name, shape, tuple(position), features or {}, density, source=source)

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
            return cq.importers.importStep(str(path))

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
            import_step=import_step, import_stl=import_stl, fit_part=fit_part,
            cached_shape=self._geometry_cache.shape,
        )
        context = SimpleNamespace(parameters=values, **helpers)
        old_path, old_cwd = sys.path[:], Path.cwd()
        try:
            sys.path.insert(0, str(source_root))
            if entry_path.parent != source_root:
                sys.path.insert(0, str(entry_path.parent))
            os.chdir(source_root)
            namespace = runpy.run_path(str(entry_path), init_globals=dict(helpers), run_name="__main__")
            if callable(namespace.get("build")):
                namespace["build"](context)
            if not self.parts and isinstance(namespace.get("result"), (cq.Workplane, cq.Shape, cq.Assembly)):
                show(namespace["result"], name="result")
        finally:
            os.chdir(old_cwd)
            sys.path[:] = old_path
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
        generate_mesh = args.get("generate_mesh", True)
        if not isinstance(generate_mesh, bool):
            raise DomainError("INVALID_REQUEST", "generate_mesh must be boolean")
        from .display_cache import policy, offers
        self._display_policy = policy(self, geometry_fingerprint, FINGERPRINT_FORMAT)
        self._mesh_reuse = offers(args)
        # Fingerprint every shape before tessellating any shared occurrence.
        for part in self.parts.values():
            part.fingerprint = geometry_fingerprint(part.shape)
        result = dict(collision_validation=collision_validation, parts=[self.describe(part, mesh=generate_mesh) for part in self.parts.values()],
                      parameters=self.parameters, annotations=self.annotations,
                      units="mm", revision=revision,
                      dependencies=dependencies)
        # Omit when unused so recovery contracts stay identical for ordinary projects.
        if fit_results:
            result["fit_results"] = fit_results
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
        if part.features:
            faces, edges = shape.Faces(), shape.Edges()
            for name, selected in part.features.items():
                selected = selected.vals() if isinstance(selected, cq.Workplane) else [selected]
                feature_entities[name] = [f"{kind}:{i}" for kind, entities in (("face", faces), ("edge", edges))
                                          for i, entity in enumerate(entities) if any(entity.isSame(s) for s in selected)]
        result = dict(id=hashlib.sha256(part.name.encode()).hexdigest()[:24], instance_id=part.name,
                      name=part.name, position=list(part.position), **intrinsic,
                      features=feature_entities, accuracy="exact", units="mm",
                      geometry_fingerprint=dict(format=FINGERPRINT_FORMAT, sha256=part.fingerprint))
        if part.source is not None:
            result["source"] = dict(part.source)
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
        from OCP.TopAbs import TopAbs_EDGE
        from OCP.TopExp import TopExp_Explorer
        from OCP.TopTools import TopTools_IndexedMapOfShape
        from OCP.BRep import BRep_Tool
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
        tess = tessellate(shape.wrapped, cache_key=f"cadpad-mesh-v2:{FINGERPRINT_FORMAT}:{fingerprint}:quality=0.1", deviation=0.1,
                          quality=0.1, angular_tolerance=0.2, compute_faces=True, compute_edges=bool(displayed_edges))
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
        for kind, published, displayed, counts in (
                ("face", shape.Faces(), displayed_faces, tess["triangles_per_face"]),
                ("edge", shape.Edges(), displayed_edges, tess["segments_per_edge"])):
            if len(displayed) != len(counts):
                raise DomainError("INVALID_GEOMETRY", "Tessellator topology does not match the published shape")
            indices = TopTools_IndexedMapOfShape()
            for entity in published:
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
                array = value.reshape(-1).tolist() if hasattr(value, "reshape") else list(value)
                finite(array)
                buffers[key] = self.artifact(struct.pack(f"<{len(array)}{dtype}", *array), ".bin")
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
        return finite(dict(**result, revision=self.revision, pose_version=args.get("pose_version", 0), accuracy="exact", units="mm"))

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
