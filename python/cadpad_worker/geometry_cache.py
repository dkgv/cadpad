"""Explicit deterministic shape builders; source execution is never memoized."""
import hashlib
import importlib.metadata
import io
import json
import os
import stat
import sys

import cadquery as cq

FORMAT = "cadpad-native-shape-v1"
MAX_BYTES = 64 * 1024 * 1024
MAX_ENTRY_BYTES = 16 * 1024 * 1024
MAX_ENTRIES = 128
MAX_MANIFEST_BYTES = 512 * 1024


def valid_hash(value):
    return isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value)


def encode(shape):
    from OCP.BRepBuilderAPI import BRepBuilderAPI_Copy
    from OCP.BRepTools import BRepTools
    from OCP.TopTools import TopTools_FormatVersion
    copy = BRepBuilderAPI_Copy(shape.wrapped, False, False).Shape()
    stream = io.BytesIO()
    BRepTools.Write_s(copy, stream, False, False, TopTools_FormatVersion.TopTools_FormatVersion_VERSION_3)
    return stream.getvalue()


def decode(data):
    return cq.Shape.importBrep(io.BytesIO(data))


def policy(fingerprint):
    from .display_cache import _constant
    from .model import FINGERPRINT_FORMAT
    try:
        producers = [encode, decode, valid_hash, take, Cache.__init__, Cache.shape, Cache.manifest, fingerprint,
                     cq.Shape.importBrep.__func__, cq.Shape.cast.__func__]
        value = dict(format=FORMAT, geometry_format=FINGERPRINT_FORMAT, python=sys.version,
                     dependencies={name: importlib.metadata.version(name) for name in
                                   ("cadquery", "cadquery-ocp", "ocp-tessellate")},
                     producers=[_constant(producer.__code__) for producer in producers])
        return hashlib.sha256(json.dumps(value, sort_keys=True, allow_nan=False).encode()).hexdigest()
    except (AttributeError, TypeError, ValueError):
        return None


def take(directory, descriptor, fingerprint):
    from .model import intrinsic_reuse_allowed
    try:
        digest, size = descriptor["sha256"], descriptor["bytes"]
        if not valid_hash(digest) or descriptor["handle"] != digest+".brep" or type(size) is not int or not 0 < size <= MAX_ENTRY_BYTES:
            return None
        fd = os.open(directory / descriptor["handle"], os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_size != size:
                return None
            data = stream.read(size+1)
        if len(data) != size or hashlib.sha256(data).hexdigest() != digest:
            return None
        shape = decode(data)
        if (not intrinsic_reuse_allowed(shape) or shape.isNull() or not shape.isValid()
                or shape.ShapeType() != descriptor["shape_type"]
                or fingerprint(shape) != descriptor["geometry_sha256"]):
            return None
        return shape
    except (OSError, KeyError, TypeError, ValueError, RuntimeError):
        return None


class Cache:
    def __init__(self, model, input_digest, offered, fingerprint):
        self.model, self.fingerprint = model, fingerprint
        self.input_digest = input_digest if valid_hash(input_digest) else None
        self.policy = policy(fingerprint)
        self.entries, self.names, self.bytes = {}, set(), 0
        self.offered = {}
        if (isinstance(offered, dict) and offered.get("format") == FORMAT
                and self.input_digest is not None and offered.get("input_digest") == self.input_digest
                and self.policy is not None and offered.get("policy") == self.policy):
            entries = offered.get("entries")
            try:
                if isinstance(entries, dict) and len(entries) <= MAX_ENTRIES and len(json.dumps(offered).encode()) <= MAX_MANIFEST_BYTES:
                    self.offered = entries
            except (TypeError, ValueError):
                pass

    def shape(self, name, builder):
        from .model import DomainError, intrinsic_reuse_allowed
        if not isinstance(name, str) or not 0 < len(name) <= 128 or name in self.names or not callable(builder):
            raise DomainError("INVALID_GEOMETRY", "cached_shape needs a unique name (1..128 characters) and a deterministic shape builder")
        self.names.add(name)
        eligible = self.input_digest is not None and self.policy is not None and len(self.entries) < MAX_ENTRIES
        descriptor = self.offered.get(name) if eligible else None
        # Every call imports an independent native shape, never a shared mutable
        # Python wrapper. Failed validation is a miss, not a publication failure.
        shape = take(self.model.cache, descriptor, self.fingerprint) if isinstance(descriptor, dict) else None
        if shape is not None and self.bytes + descriptor["bytes"] <= MAX_BYTES:
            self.entries[name] = descriptor.copy()
            self.bytes += descriptor["bytes"]
            return shape
        shape = builder()
        if not isinstance(shape, cq.Shape) or shape.isNull() or not shape.isValid():
            raise DomainError("INVALID_GEOMETRY", "cached_shape builder must return a valid CadQuery Shape; use .val() for a Workplane")
        if intrinsic_reuse_allowed(shape):
            # Publish the same canonical native representation on a miss and a
            # hit. OCCT serialization can change the last bits of integrals;
            # recovery must not weaken its exact publication comparison.
            try:
                data = encode(shape)
                restored = decode(data)
                digest = self.fingerprint(shape)
                if (restored.isNull() or not restored.isValid() or restored.ShapeType() != shape.ShapeType()
                        or not intrinsic_reuse_allowed(restored) or self.fingerprint(restored) != digest):
                    return shape
                shape = restored
                if eligible and len(data) <= MAX_ENTRY_BYTES and self.bytes + len(data) <= MAX_BYTES:
                    descriptor = dict(self.model.artifact(data, ".brep"), geometry_sha256=digest, shape_type=shape.ShapeType())
                    # Bound metadata too, including multibyte names.
                    candidate = {**self.entries, name: descriptor}
                    if len(json.dumps(candidate).encode()) <= MAX_MANIFEST_BYTES-1024:
                        self.entries[name] = descriptor
                        self.bytes += len(data)
            except (OSError, ValueError, RuntimeError):
                pass
            except DomainError as error:
                if error.code != "INVALID_ARTIFACT":
                    raise
        return shape

    def manifest(self):
        if self.input_digest is None or self.policy is None or not self.entries:
            return None
        return dict(format=FORMAT, input_digest=self.input_digest, policy=self.policy, entries=self.entries)
