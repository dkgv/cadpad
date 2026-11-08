"""BRep verification of conservative proxy coverage; no display mesh is trusted."""
import math

import cadquery as cq
from OCP.Precision import Precision

from .model import DomainError, Model, bounds


def validate_proxies(model, definition):
    if definition is None:
        return None
    bodies = definition.get("bodies", {})
    if set(bodies) != set(model.parts) or not 1 <= len(bodies) <= 64:
        raise DomainError("INVALID_COLLISION_PROXY", "Every published instance needs an explicit collision proxy")
    if sum(len(pieces) for pieces in bodies.values()) > 128:
        raise DomainError("INVALID_COLLISION_PROXY", "At most 128 primitive pieces are supported")
    for name, pieces in bodies.items():
        if not pieces or not model.parts[name].shape.Solids():
            raise DomainError("INVALID_COLLISION_PROXY", f"{name} needs a nonempty proxy for solid geometry")
        proxies = []
        for piece in pieces:
            shape = piece["shape"]
            kind = shape["type"]
            frame = piece.get("frame", {})
            if kind == "bounds":
                box = bounds(model.parts[name].shape)
                half = [size / 2 + 1e-6 for size in box["size"]]
                center = [(a + b) / 2 for a, b in zip(box["min"], box["max"])]
                proxy = cq.Workplane().box(*(2 * v for v in half)).val().translate(tuple(center))
            else:
                if kind == "box":
                    half = shape["half_extents_m"]
                    if len(half) != 3 or any(not math.isfinite(v) or v <= 0 or v > 1e6 for v in half):
                        raise DomainError("INVALID_COLLISION_PROXY", "Invalid box dimensions")
                    proxy = cq.Workplane().box(*(v * 2000 for v in half)).val()
                elif kind == "ball":
                    radius = shape["radius_m"]
                    if not math.isfinite(radius) or not 0 < radius <= 1e6:
                        raise DomainError("INVALID_COLLISION_PROXY", "Invalid sphere radius")
                    proxy = cq.Workplane().sphere(radius * 1000).val()
                else:
                    raise DomainError("INVALID_COLLISION_PROXY", "Unsupported proxy primitive")
                proxy = proxy.moved(Model.location(dict(position=[v * 1000 for v in frame.get("position_m", [0, 0, 0])],
                                                       quaternion=frame.get("quaternion", [0, 0, 0, 1]))))
            proxies.append(proxy)
        try:
            remaining = model.parts[name].shape.cut(*proxies)
            if not remaining.isValid() or remaining.Solids():
                raise DomainError("INVALID_COLLISION_PROXY", f"Proxy pieces do not conservatively cover {name}; residual BRep solids remain")
        except DomainError:
            raise
        except Exception as error:
            raise DomainError("INVALID_COLLISION_PROXY", f"BRep coverage verification failed for {name}") from error
    return dict(method="brep_boolean_coverage", covered_instances=sorted(bodies),
                kernel_tolerance_mm=Precision.Confusion_s(), bounds_padding_mm=1e-6)
