"""Binary STL export on private topology with explicit millimetre meshing.

The deflection is an OCCT meshing setting, not a certified surface-error bound.
STL stores float32 coordinates; normals must describe those actual coordinates.
"""
import math
import os
from pathlib import Path
import struct
import uuid

import cadquery as cq
from OCP.BRepBuilderAPI import BRepBuilderAPI_Copy
from OCP.BRep import BRep_Tool
from OCP.TopLoc import TopLoc_Location

LINEAR_DEFLECTION_MM = 0.025
ANGULAR_DEFLECTION_RAD = 0.1
MAX_BYTES = 1024 * 1024 * 1024
# Additional vertex-rounding budget; independent of the mesher's deflection.
MAX_VERTEX_ROUNDING_MM = 0.025


def _normal(points):
    a, b, c = points
    u = tuple(b[i]-a[i] for i in range(3))
    v = tuple(c[i]-a[i] for i in range(3))
    return (u[1]*v[2]-u[2]*v[1], u[2]*v[0]-u[0]*v[2], u[0]*v[1]-u[1]*v[0])


def _validate_precision(shape):
    count = 0
    for face in shape.Faces():
        location = TopLoc_Location()
        mesh = BRep_Tool.Triangulation_s(face.wrapped, location)
        if mesh is None:
            continue
        count += mesh.NbTriangles()
        if 84 + count*50 > MAX_BYTES:
            raise ValueError("STL triangulation exceeds the export budget")
        transform = location.Transformation()
        for index in range(1, mesh.NbTriangles()+1):
            points = [mesh.Node(node).Transformed(transform) for node in mesh.Triangle(index).Get()]
            points = [(point.X(), point.Y(), point.Z()) for point in points]
            try:
                rounded = [struct.unpack("<3f", struct.pack("<3f", *point)) for point in points]
            except (OverflowError, struct.error) as error:
                raise ValueError("STL coordinates exceed float32 precision; use STEP") from error
            if any(not all(math.isfinite(value) for value in point) for point in rounded):
                raise ValueError("STL coordinates are not finite")
            if any(math.dist(a, b) > MAX_VERTEX_ROUNDING_MM for a, b in zip(points, rounded)):
                raise ValueError("STL float32 vertex rounding exceeds 0.025 mm; use STEP or explicitly move the design closer to the origin")
            original, converted = _normal(points), _normal(rounded)
            # A genuinely degenerate kernel seam may be removed later. A real
            # triangle lost or reversed by serialization must never be published.
            if math.hypot(*original) > 0 and sum(a*b for a, b in zip(original, converted)) <= 0:
                raise ValueError("STL float32 precision collapses or reverses a surface facet; use STEP or explicitly move the design closer to the origin")


def export(shape, path: Path):
    # Never reuse display triangulation or mutate the committed shape's mesh.
    copied = cq.Shape.cast(BRepBuilderAPI_Copy(shape.wrapped, False, False).Shape())
    try:
        if not copied.exportStl(str(path), tolerance=LINEAR_DEFLECTION_MM,
                                angularTolerance=ANGULAR_DEFLECTION_RAD,
                                ascii=False, relative=False):
            raise ValueError("The geometry kernel could not write STL")
    except Exception:
        path.unlink(missing_ok=True)
        raise
    temporary = path.with_name("." + uuid.uuid4().hex + ".stl")
    try:
        with path.open("rb") as source, temporary.open("xb") as target:
            header = source.read(84)
            if len(header) != 84:
                raise ValueError("The geometry kernel wrote an incomplete STL")
            count, = struct.unpack_from("<I", header, 80)
            if not count or 84 + count*50 > MAX_BYTES or path.stat().st_size != 84 + count*50:
                raise ValueError("STL is empty, oversized or has an invalid facet count")
            _validate_precision(copied)
            target.write(b"Cadpad binary STL; coordinates in millimetres".ljust(80, b"\0"))
            target.write(struct.pack("<I", 0))
            accepted = 0
            for _ in range(count):
                record = source.read(50)
                if len(record) != 50:
                    raise ValueError("STL changed during export")
                coordinates = struct.unpack_from("<9f", record, 12)
                if not all(math.isfinite(value) for value in coordinates):
                    raise ValueError("STL coordinates are not finite")
                a, b, c = coordinates[:3], coordinates[3:6], coordinates[6:9]
                n = _normal((a, b, c))
                norm = math.hypot(*n)
                if norm == 0:
                    # Poles/seams can collapse after float32 quantization.
                    # Precision checks above reject loss of nonzero CAD facets.
                    continue
                normal = tuple(value/norm for value in n)
                target.write(struct.pack("<12fH", *normal, *coordinates, 0))
                accepted += 1
            if not accepted:
                raise ValueError("All STL facets collapsed at float32 precision")
            target.seek(80)
            target.write(struct.pack("<I", accepted))
            target.flush()
            os.fsync(target.fileno())
        os.replace(temporary, path)
    except Exception:
        path.unlink(missing_ok=True)
        raise
    finally:
        temporary.unlink(missing_ok=True)
