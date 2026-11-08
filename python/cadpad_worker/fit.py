"""Score reconstructed CadQuery solids against reference STL meshes.

Checks are layered:
  always  — bbox, volume
  default — probes, sections
  strict  — sampled directed Hausdorff (mesh↔CAD surface samples)

`on_fail` is "error" (fail the build) or "report" (include results only).
"""
from __future__ import annotations

import math
import os
import tempfile
from pathlib import Path

import cadquery as cq
from OCP.BRepBuilderAPI import BRepBuilderAPI_Copy, BRepBuilderAPI_MakeVertex
from OCP.BRepExtrema import BRepExtrema_DistShapeShape
from OCP.gp import gp_Pnt

from . import stl_mesh
from .stl import ANGULAR_DEFLECTION_RAD, LINEAR_DEFLECTION_MM


def _domain(code, message, details=None):
    from .model import DomainError
    return DomainError(code, message, details=details)


def _shape_bounds(shape):
    from .model import bounds
    return bounds(shape)


DEFAULT_TOLERANCES = dict(
    bbox_mm=0.5,
    volume_rel=0.02,
    probe_mm=0.3,
    section_mm=0.5,
    hausdorff_mm=0.5,
)
DEFAULT_CHECKS = ("bbox", "volume", "probes", "sections", "hausdorff")
STRICTNESS_CHECKS = {
    "always": ("bbox", "volume"),
    "default": ("bbox", "volume", "probes", "sections"),
    "strict": ("bbox", "volume", "probes", "sections", "hausdorff"),
}


def _finite3(point, label):
    if len(point) != 3 or not all(isinstance(value, (int, float)) and math.isfinite(value) for value in point):
        raise _domain("INVALID_REQUEST", f"{label} requires three finite millimetre coordinates")
    return [float(point[0]), float(point[1]), float(point[2])]


def normalize_spec(spec: dict) -> dict:
    if not isinstance(spec, dict):
        raise _domain("INVALID_REQUEST", "fit_part requires a specification object")
    part = spec.get("part")
    reference = spec.get("reference_stl")
    if not isinstance(part, str) or not part:
        raise _domain("INVALID_REQUEST", "fit_part requires a part name")
    if not isinstance(reference, str) or not reference:
        raise _domain("INVALID_REQUEST", "fit_part requires reference_stl under assets/")
    strictness = spec.get("strictness", "default")
    if strictness not in STRICTNESS_CHECKS:
        raise _domain("INVALID_REQUEST", "fit_part strictness must be always, default or strict")
    checks = spec.get("checks")
    if checks is None:
        checks = STRICTNESS_CHECKS[strictness]
    if not isinstance(checks, (list, tuple)) or not checks or any(not isinstance(item, str) for item in checks):
        raise _domain("INVALID_REQUEST", "fit_part checks must be a non-empty list of names")
    unknown = sorted(set(checks) - set(DEFAULT_CHECKS))
    if unknown:
        raise _domain("INVALID_REQUEST", f"Unknown fit checks: {', '.join(unknown)}")
    tolerances = dict(DEFAULT_TOLERANCES)
    provided = spec.get("tolerances") or {}
    if not isinstance(provided, dict):
        raise _domain("INVALID_REQUEST", "fit_part tolerances must be an object")
    for key, value in provided.items():
        if key not in DEFAULT_TOLERANCES:
            raise _domain("INVALID_REQUEST", f"Unknown fit tolerance {key}")
        if not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0:
            raise _domain("INVALID_REQUEST", f"Tolerance {key} must be a non-negative finite number")
        tolerances[key] = float(value)
    on_fail = spec.get("on_fail", "error")
    if on_fail not in ("error", "report"):
        raise _domain("INVALID_REQUEST", "fit_part on_fail must be error or report")
    probes = []
    for probe in spec.get("probes") or ():
        if not isinstance(probe, dict) or not isinstance(probe.get("id"), str) or not probe["id"]:
            raise _domain("INVALID_REQUEST", "Each fit probe needs a string id")
        expect = probe.get("expect", "on_surface")
        if expect not in ("inside", "outside", "on_surface"):
            raise _domain("INVALID_REQUEST", f"Probe {probe['id']} expect must be inside, outside or on_surface")
        probes.append(dict(
            id=probe["id"],
            point=_finite3(probe.get("point", ()), f"Probe {probe['id']} point"),
            expect=expect,
            max_distance_mm=float(probe["max_distance_mm"]) if "max_distance_mm" in probe else tolerances["probe_mm"],
        ))
    sections = []
    for section in spec.get("sections") or ():
        if not isinstance(section, dict) or not isinstance(section.get("id"), str) or not section["id"]:
            raise _domain("INVALID_REQUEST", "Each fit section needs a string id")
        sections.append(dict(
            id=section["id"],
            origin=_finite3(section.get("origin", (0, 0, 0)), f"Section {section['id']} origin"),
            normal=_finite3(section.get("normal", (0, 0, 1)), f"Section {section['id']} normal"),
            tolerance_mm=float(section["tolerance_mm"]) if "tolerance_mm" in section else tolerances["section_mm"],
        ))
    sample_budget = spec.get("sample_budget", 256)
    if not isinstance(sample_budget, int) or not 8 <= sample_budget <= 2048:
        raise _domain("INVALID_REQUEST", "fit_part sample_budget must be an int from 8 to 2048")
    return dict(
        id=spec.get("id") or part,
        part=part,
        reference_stl=reference,
        strictness=strictness,
        checks=tuple(checks),
        tolerances=tolerances,
        probes=probes,
        sections=sections,
        on_fail=on_fail,
        sample_budget=sample_budget,
    )


def _check(name, passed, *, measured=None, expected=None, tolerance=None, message=None, **extra):
    item = dict(name=name, passed=bool(passed))
    if measured is not None:
        item["measured"] = measured
    if expected is not None:
        item["expected"] = expected
    if tolerance is not None:
        item["tolerance"] = tolerance
    if message:
        item["message"] = message
    item.update(extra)
    return item


def _distance_point_to_shape(shape: cq.Shape, point) -> float:
    vertex = BRepBuilderAPI_MakeVertex(gp_Pnt(*point)).Vertex()
    extrema = BRepExtrema_DistShapeShape(shape.wrapped, vertex)
    extrema.Perform()
    return extrema.Value() if extrema.IsDone() else math.inf


def _cad_surface_samples(shape: cq.Shape, budget: int) -> list[tuple[float, float, float]]:
    from OCP.BRep import BRep_Tool
    from OCP.TopLoc import TopLoc_Location

    points = []
    faces = shape.Faces()
    if faces:
        per_face = max(1, budget // max(1, len(faces)))
        for face in faces:
            location = TopLoc_Location()
            triangulation = BRep_Tool.Triangulation_s(face.wrapped, location)
            if triangulation is None:
                continue
            transform = location.Transformation()
            count = triangulation.NbNodes()
            step = max(1, count // per_face)
            for index in range(1, count + 1, step):
                node = triangulation.Node(index).Transformed(transform)
                points.append((node.X(), node.Y(), node.Z()))
                if len(points) >= budget:
                    return points
    if points:
        return points
    copied = cq.Shape.cast(BRepBuilderAPI_Copy(shape.wrapped, False, False).Shape())
    fd, name = tempfile.mkstemp(suffix=".stl")
    os.close(fd)
    path = Path(name)
    try:
        if not copied.exportStl(
            str(path),
            tolerance=LINEAR_DEFLECTION_MM,
            angularTolerance=ANGULAR_DEFLECTION_RAD,
            ascii=False,
            relative=False,
        ):
            raise _domain("INVALID_GEOMETRY", "Could not tessellate CAD for fit sampling")
        return stl_mesh.sample_points(stl_mesh.load(path), budget)
    finally:
        path.unlink(missing_ok=True)


def _point_inside_solid(shape: cq.Shape, point) -> bool:
    from OCP.BRepClass3d import BRepClass3d_SolidClassifier
    from OCP.TopAbs import TopAbs_State

    solids = shape.Solids()
    if not solids:
        return False
    classifier = BRepClass3d_SolidClassifier(solids[0].wrapped)
    classifier.Perform(gp_Pnt(*point), 1e-6)
    return classifier.State() in (TopAbs_State.TopAbs_IN, TopAbs_State.TopAbs_ON)


def evaluate(spec: dict, parts: dict, source_root: Path) -> dict:
    spec = normalize_spec(spec)
    assets = (source_root / "assets").resolve()
    asset = (source_root / "assets" / spec["reference_stl"]).resolve(strict=False)
    result = dict(
        id=spec["id"],
        part=spec["part"],
        reference_stl=spec["reference_stl"],
        strictness=spec["strictness"],
        on_fail=spec["on_fail"],
        status="pending",
        passed=False,
        checks=[],
    )
    if not asset.is_file() or not asset.is_relative_to(assets):
        result["status"] = "missing_reference"
        result["checks"].append(_check("reference", False, message="reference_stl must resolve under assets/"))
        return result
    if spec["part"] not in parts:
        result["status"] = "awaiting_part"
        result["checks"].append(_check("part", False, message=f"Published part {spec['part']!r} is missing"))
        return result
    try:
        mesh = stl_mesh.load(asset)
    except ValueError as error:
        result["status"] = "invalid_reference"
        result["checks"].append(_check("reference", False, message=str(error)))
        return result

    shape = parts[spec["part"]].shape
    mesh_box = stl_mesh.bounds(mesh)
    cad_box = _shape_bounds(shape)
    mesh_volume = abs(stl_mesh.signed_volume(mesh))
    cad_volume = abs(shape.Volume()) if shape.Solids() else None
    checks = []

    if "bbox" in spec["checks"]:
        size_ok = all(
            abs(cad_box["size"][i] - mesh_box["size"][i]) <= spec["tolerances"]["bbox_mm"] for i in range(3)
        )
        corner_ok = all(
            abs(cad_box["min"][i] - mesh_box["min"][i]) <= spec["tolerances"]["bbox_mm"]
            and abs(cad_box["max"][i] - mesh_box["max"][i]) <= spec["tolerances"]["bbox_mm"]
            for i in range(3)
        )
        checks.append(_check(
            "bbox",
            size_ok and corner_ok,
            measured={"size": cad_box["size"], "min": cad_box["min"], "max": cad_box["max"]},
            expected={"size": mesh_box["size"], "min": mesh_box["min"], "max": mesh_box["max"]},
            tolerance={"bbox_mm": spec["tolerances"]["bbox_mm"]},
            message=None if size_ok and corner_ok else "Bounding box differs from the reference STL",
        ))

    if "volume" in spec["checks"]:
        if cad_volume is None:
            checks.append(_check("volume", False, message="Reconstruction must be a solid for volume fit"))
        elif mesh_volume <= 0:
            checks.append(_check("volume", False, message="Reference STL volume is non-positive; mesh may be open"))
        else:
            rel = abs(cad_volume - mesh_volume) / mesh_volume
            checks.append(_check(
                "volume",
                rel <= spec["tolerances"]["volume_rel"],
                measured={"volume_mm3": cad_volume, "relative_error": rel},
                expected={"volume_mm3": mesh_volume},
                tolerance={"volume_rel": spec["tolerances"]["volume_rel"]},
            ))

    if "probes" in spec["checks"]:
        if not spec["probes"]:
            checks.append(_check("probes", True, message="No probes declared"))
        else:
            probe_results = []
            all_ok = True
            for probe in spec["probes"]:
                distance = stl_mesh.distance_to_mesh(tuple(probe["point"]), mesh)
                inside = _point_inside_solid(shape, probe["point"]) if shape.Solids() else False
                if probe["expect"] == "on_surface":
                    cad_distance = _distance_point_to_shape(shape, probe["point"])
                    ok = distance <= probe["max_distance_mm"] and cad_distance <= probe["max_distance_mm"]
                elif probe["expect"] == "inside":
                    ok = inside
                else:
                    ok = not inside
                all_ok = all_ok and ok
                probe_results.append(dict(
                    id=probe["id"],
                    passed=ok,
                    expect=probe["expect"],
                    stl_distance_mm=distance,
                    inside_solid=inside,
                ))
            checks.append(_check("probes", all_ok, probes=probe_results))

    if "sections" in spec["checks"]:
        if spec["sections"]:
            section_specs = spec["sections"]
        else:
            center = mesh_box["center"]
            section_specs = [
                dict(id="xy", origin=center, normal=[0, 0, 1], tolerance_mm=spec["tolerances"]["section_mm"]),
                dict(id="xz", origin=center, normal=[0, 1, 0], tolerance_mm=spec["tolerances"]["section_mm"]),
                dict(id="yz", origin=center, normal=[1, 0, 0], tolerance_mm=spec["tolerances"]["section_mm"]),
            ]
        samples = _cad_surface_samples(shape, spec["sample_budget"])
        section_results = []
        all_ok = True
        for section in section_specs:
            ref_points = stl_mesh.section_points(mesh, section["origin"], section["normal"])
            if not ref_points:
                section_results.append(dict(id=section["id"], passed=True, message="No reference section points"))
                continue
            origin = section["origin"]
            normal = section["normal"]
            norm = math.hypot(*normal) or 1.0
            unit = [value / norm for value in normal]
            cad_points = [
                point for point in samples
                if abs(sum((point[i] - origin[i]) * unit[i] for i in range(3))) <= section["tolerance_mm"]
            ]
            misses = 0
            step = max(1, len(ref_points) // 64)
            for point in ref_points[::step]:
                near = cad_points and min(math.dist(point, other) for other in cad_points) <= section["tolerance_mm"]
                if not near and _distance_point_to_shape(shape, point) > section["tolerance_mm"]:
                    misses += 1
            ok = misses == 0
            all_ok = all_ok and ok
            section_results.append(dict(
                id=section["id"],
                passed=ok,
                reference_points=len(ref_points),
                misses=misses,
                tolerance_mm=section["tolerance_mm"],
            ))
        checks.append(_check("sections", all_ok, sections=section_results))

    if "hausdorff" in spec["checks"]:
        mesh_samples = stl_mesh.sample_points(mesh, spec["sample_budget"])
        cad_samples = _cad_surface_samples(shape, spec["sample_budget"])
        to_cad = [_distance_point_to_shape(shape, point) for point in mesh_samples]
        to_mesh = max((stl_mesh.distance_to_mesh(point, mesh) for point in cad_samples), default=0.0)
        directed = max(max(to_cad) if to_cad else 0.0, to_mesh)
        checks.append(_check(
            "hausdorff",
            directed <= spec["tolerances"]["hausdorff_mm"],
            measured={
                "max_distance_mm": directed,
                "reference_to_cad_mm": max(to_cad) if to_cad else 0.0,
                "cad_to_reference_mm": to_mesh,
                "samples": len(mesh_samples),
            },
            tolerance={"hausdorff_mm": spec["tolerances"]["hausdorff_mm"]},
        ))

    result["checks"] = checks
    result["passed"] = all(item["passed"] for item in checks) if checks else False
    result["status"] = "passed" if result["passed"] else "failed"
    result["reference"] = dict(
        triangle_count=mesh.triangle_count,
        bounds=mesh_box,
        volume_mm3=mesh_volume,
    )
    return result


def evaluate_all(specs: list, parts: dict, source_root: Path) -> list:
    return [evaluate(spec, parts, source_root) for spec in specs]


def enforce(results: list):
    blocking = [item for item in results if item.get("on_fail") == "error" and not item.get("passed")]
    if not blocking:
        return
    summary = ", ".join(f"{item['id']} ({item['status']})" for item in blocking)
    raise _domain(
        "FIT_FAILED",
        f"STL fit checks failed: {summary}",
        details=dict(fit_results=blocking),
    )
