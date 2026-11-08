"""Parse binary/ASCII STL meshes for reconstruction fit checks.

Coordinates are millimetres. This is a reference-mesh utility, not a CAD solid
importer; display import uses OCCT separately.
"""
from __future__ import annotations

import math
import struct
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Mesh:
    vertices: tuple[tuple[float, float, float], ...]
    triangles: tuple[tuple[int, int, int], ...]

    @property
    def triangle_count(self) -> int:
        return len(self.triangles)


def _finite3(point) -> tuple[float, float, float]:
    if len(point) != 3 or not all(isinstance(value, (int, float)) and math.isfinite(value) for value in point):
        raise ValueError("STL coordinates must be three finite floats")
    return float(point[0]), float(point[1]), float(point[2])


def load(path: Path) -> Mesh:
    data = path.read_bytes()
    if len(data) < 84:
        raise ValueError("STL is too small")
    # Binary STL: 80-byte header + uint32 count + 50-byte facets. Prefer binary
    # when the declared size matches; otherwise try ASCII.
    count, = struct.unpack_from("<I", data, 80)
    if 84 + count * 50 == len(data) and count > 0:
        return _load_binary(data, count)
    text = data.decode("utf-8", errors="replace")
    if "facet" in text.lower():
        return _load_ascii(text)
    raise ValueError("STL is neither a valid binary nor ASCII mesh")


def _load_binary(data: bytes, count: int) -> Mesh:
    vertices: list[tuple[float, float, float]] = []
    index = {}
    triangles = []
    offset = 84
    for _ in range(count):
        record = data[offset:offset + 50]
        offset += 50
        coordinates = struct.unpack_from("<9f", record, 12)
        if not all(math.isfinite(value) for value in coordinates):
            raise ValueError("STL coordinates are not finite")
        points = [coordinates[i:i + 3] for i in range(0, 9, 3)]
        ids = []
        for point in points:
            key = point
            if key not in index:
                index[key] = len(vertices)
                vertices.append(point)
            ids.append(index[key])
        if ids[0] == ids[1] or ids[1] == ids[2] or ids[0] == ids[2]:
            continue
        triangles.append((ids[0], ids[1], ids[2]))
    if not triangles:
        raise ValueError("STL has no usable triangles")
    return Mesh(tuple(vertices), tuple(triangles))


def _load_ascii(text: str) -> Mesh:
    vertices: list[tuple[float, float, float]] = []
    index = {}
    triangles = []
    points = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line.lower().startswith("vertex"):
            if points and len(points) == 3:
                ids = []
                for point in points:
                    if point not in index:
                        index[point] = len(vertices)
                        vertices.append(point)
                    ids.append(index[point])
                if len(set(ids)) == 3:
                    triangles.append((ids[0], ids[1], ids[2]))
                points = []
            continue
        parts = line.split()
        if len(parts) < 4:
            raise ValueError("ASCII STL vertex is malformed")
        point = _finite3((float(parts[1]), float(parts[2]), float(parts[3])))
        points.append(point)
    if points and len(points) == 3:
        ids = []
        for point in points:
            if point not in index:
                index[point] = len(vertices)
                vertices.append(point)
            ids.append(index[point])
        if len(set(ids)) == 3:
            triangles.append((ids[0], ids[1], ids[2]))
    if not triangles:
        raise ValueError("ASCII STL has no usable triangles")
    return Mesh(tuple(vertices), tuple(triangles))


def bounds(mesh: Mesh) -> dict:
    xs = [vertex[0] for vertex in mesh.vertices]
    ys = [vertex[1] for vertex in mesh.vertices]
    zs = [vertex[2] for vertex in mesh.vertices]
    minimum = [min(xs), min(ys), min(zs)]
    maximum = [max(xs), max(ys), max(zs)]
    return {
        "min": minimum,
        "max": maximum,
        "size": [maximum[i] - minimum[i] for i in range(3)],
        "center": [(minimum[i] + maximum[i]) / 2 for i in range(3)],
    }


def signed_volume(mesh: Mesh) -> float:
    # Divergence theorem on a closed triangle mesh. Open meshes still return a
    # value; callers should treat it as approximate for fit scoring.
    total = 0.0
    for a, b, c in mesh.triangles:
        ax, ay, az = mesh.vertices[a]
        bx, by, bz = mesh.vertices[b]
        cx, cy, cz = mesh.vertices[c]
        total += ax * (by * cz - bz * cy) - ay * (bx * cz - bz * cx) + az * (bx * cy - by * cx)
    return total / 6.0


def sample_points(mesh: Mesh, budget: int = 512) -> list[tuple[float, float, float]]:
    if budget < 1:
        raise ValueError("Sample budget must be positive")
    points = []
    step = max(1, len(mesh.triangles) // budget)
    for index in range(0, len(mesh.triangles), step):
        a, b, c = (mesh.vertices[i] for i in mesh.triangles[index])
        # Triangle centroid is stable and cheap.
        points.append(((a[0] + b[0] + c[0]) / 3, (a[1] + b[1] + c[1]) / 3, (a[2] + b[2] + c[2]) / 3))
        if len(points) >= budget:
            break
    # Always include bounds corners for thin features.
    box = bounds(mesh)
    for x in (box["min"][0], box["max"][0]):
        for y in (box["min"][1], box["max"][1]):
            for z in (box["min"][2], box["max"][2]):
                points.append((x, y, z))
    return points


def point_triangle_distance(point, a, b, c) -> float:
    # Ericson: Real-Time Collision Detection, §5.1.5.
    ax, ay, az = a
    bx, by, bz = b
    cx, cy, cz = c
    px, py, pz = point
    ab = (bx - ax, by - ay, bz - az)
    ac = (cx - ax, cy - ay, cz - az)
    ap = (px - ax, py - ay, pz - az)
    d1 = ab[0] * ap[0] + ab[1] * ap[1] + ab[2] * ap[2]
    d2 = ac[0] * ap[0] + ac[1] * ap[1] + ac[2] * ap[2]
    if d1 <= 0 and d2 <= 0:
        return math.dist(point, a)
    bp = (px - bx, py - by, pz - bz)
    d3 = ab[0] * bp[0] + ab[1] * bp[1] + ab[2] * bp[2]
    d4 = ac[0] * bp[0] + ac[1] * bp[1] + ac[2] * bp[2]
    if d3 >= 0 and d4 <= d3:
        return math.dist(point, b)
    vc = d1 * d4 - d3 * d2
    if vc <= 0 and d1 >= 0 and d3 <= 0:
        v = d1 / (d1 - d3)
        return math.dist(point, (ax + ab[0] * v, ay + ab[1] * v, az + ab[2] * v))
    cp = (px - cx, py - cy, pz - cz)
    d5 = ab[0] * cp[0] + ab[1] * cp[1] + ab[2] * cp[2]
    d6 = ac[0] * cp[0] + ac[1] * cp[1] + ac[2] * cp[2]
    if d6 >= 0 and d5 <= d6:
        return math.dist(point, c)
    vb = d5 * d2 - d1 * d6
    if vb <= 0 and d2 >= 0 and d6 <= 0:
        w = d2 / (d2 - d6)
        return math.dist(point, (ax + ac[0] * w, ay + ac[1] * w, az + ac[2] * w))
    va = d3 * d6 - d5 * d4
    if va <= 0 and (d4 - d3) >= 0 and (d5 - d6) >= 0:
        w = (d4 - d3) / ((d4 - d3) + (d5 - d6))
        return math.dist(point, (bx + (cx - bx) * w, by + (cy - by) * w, bz + (cz - bz) * w))
    denom = 1.0 / (va + vb + vc)
    v = vb * denom
    w = vc * denom
    return math.dist(point, (ax + ab[0] * v + ac[0] * w, ay + ab[1] * v + ac[1] * w, az + ab[2] * v + ac[2] * w))


def distance_to_mesh(point, mesh: Mesh) -> float:
    best = math.inf
    for a, b, c in mesh.triangles:
        best = min(best, point_triangle_distance(point, mesh.vertices[a], mesh.vertices[b], mesh.vertices[c]))
    return best


def section_points(mesh: Mesh, origin, normal, *, band_mm: float = 0.25) -> list[tuple[float, float, float]]:
    origin = _finite3(origin)
    normal = _finite3(normal)
    norm = math.hypot(*normal)
    if norm == 0:
        raise ValueError("Section normal must be non-zero")
    normal = (normal[0] / norm, normal[1] / norm, normal[2] / norm)
    points = []
    for a_i, b_i, c_i in mesh.triangles:
        tri = (mesh.vertices[a_i], mesh.vertices[b_i], mesh.vertices[c_i])
        for start, end in ((tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])):
            da = sum((start[i] - origin[i]) * normal[i] for i in range(3))
            db = sum((end[i] - origin[i]) * normal[i] for i in range(3))
            if abs(da) <= band_mm:
                points.append(start)
            if da == 0 or db == 0 or da * db > 0:
                continue
            t = da / (da - db)
            points.append(tuple(start[i] + (end[i] - start[i]) * t for i in range(3)))
    return points
