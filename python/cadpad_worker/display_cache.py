"""One-shot, candidate-local reuse of verified display buffers, never CAD source."""
import copy
import hashlib
import importlib.metadata
import json
import os
import stat
import sys
from types import CodeType

MAX_BYTES = 64 * 1024 * 1024
MAX_MANIFEST_BYTES = 512 * 1024
MAX_BUFFER_BYTES = 16 * 1024 * 1024
BUFFER_NAMES = {"vertices", "normals", "triangles", "edges", "triangles_per_face",
                "segments_per_edge", "obj_vertices", "face_types", "edge_types"}


def _constant(value):
    if isinstance(value, CodeType):
        return dict(code=value.co_code.hex(), constants=_constant(value.co_consts),
                    names=value.co_names, args=[value.co_argcount, value.co_posonlyargcount,
                                               value.co_kwonlyargcount], flags=value.co_flags,
                    free=value.co_freevars, cells=value.co_cellvars)
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, bytes):
        return {"bytes": value.hex()}
    if isinstance(value, (tuple, list)):
        return [_constant(item) for item in value]
    if isinstance(value, (set, frozenset)):
        return {"set": sorted((_constant(item) for item in value), key=lambda x: json.dumps(x, sort_keys=True))}
    if isinstance(value, dict) and all(isinstance(key, str) for key in value):
        return {key: _constant(item) for key, item in value.items()}
    raise TypeError("Unversioned display producer")


def policy(model, fingerprint_function, fingerprint_format):
    from ocp_tessellate.tessellator import tessellate, discretize_edges
    from ocp_tessellate.ocp_utils import get_faces, get_edges
    try:
        producers = [model.mesh, model.artifact, fingerprint_function, tessellate,
                     discretize_edges, get_faces, get_edges, take, policy, _constant, _valid_hash, offers]
        codes = []
        for producer in producers:
            for _ in range(4):
                codes.append([_constant(producer.__code__), _constant(getattr(producer, "__defaults__", None)),
                              _constant(getattr(producer, "__kwdefaults__", None))])
                producer = getattr(producer, "__wrapped__", None)
                if producer is None:
                    break
            if producer is not None:
                return None
        value = dict(version=1, python=sys.version, geometry_format=fingerprint_format,
                     dependencies={name: importlib.metadata.version(name) for name in
                                   ("cadquery", "cadquery-ocp", "ocp-tessellate")}, producers=codes)
        return hashlib.sha256(json.dumps(value, sort_keys=True, allow_nan=False).encode()).hexdigest()
    except (AttributeError, TypeError, ValueError):
        # Unknown/custom producers still render; they simply cannot reuse data.
        return None


def _valid_hash(value):
    return isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value)


def take(cache, fingerprint, current_policy, offered):
    if not _valid_hash(current_policy) or not isinstance(offered, dict):
        return None
    mesh = offered.get(fingerprint)
    if not isinstance(mesh, dict) or mesh.get("format") != "cadpad-mesh-v2" or mesh.get("cache_policy") != current_policy:
        return None
    buffers = mesh.get("buffers")
    if not isinstance(buffers, dict) or not buffers or not set(buffers).issubset(BUFFER_NAMES):
        return None
    total = 0
    try:
        for descriptor in buffers.values():
            digest = descriptor["sha256"]
            size = descriptor["bytes"]
            if not _valid_hash(digest) or descriptor["handle"] != digest + ".bin" or type(size) is not int or not 0 <= size <= MAX_BUFFER_BYTES:
                return None
            total += size
            if total > MAX_BYTES:
                return None
            fd = os.open(cache / descriptor["handle"], os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
            with os.fdopen(fd, "rb") as stream:
                info = os.fstat(stream.fileno())
                if not stat.S_ISREG(info.st_mode) or info.st_size != size:
                    return None
                hasher = hashlib.sha256()
                read = 0
                for block in iter(lambda: stream.read(65536), b""):
                    read += len(block)
                    if read > size:
                        return None
                    hasher.update(block)
                if read != size or hasher.hexdigest() != digest:
                    return None
        return copy.deepcopy(mesh)
    except (OSError, KeyError, TypeError, ValueError):
        return None


def offers(args):
    value = args.get("reuse_meshes", {})
    if not isinstance(value, dict) or len(value) > 128:
        return {}
    try:
        if len(json.dumps(value, separators=(",", ":")).encode()) > MAX_MANIFEST_BYTES:
            return {}
    except (TypeError, ValueError):
        return {}
    return value
