"""Cabinet with a hinged door (requirement fixture, case E3).

The door closes flat against the frame's front face (face contact, no
overlap) and swings about a vertical hinge on the frame's +x front edge.
"""
import cadquery as cq

DEPTH = 18
DOOR_THICKNESS = 2


def cabinet(context):
    angle = context.parameter("door_angle", 0.0, unit="deg", minimum=0.0, maximum=130.0)
    frame = cq.Workplane().box(40, DEPTH, 60).faces(">Y").workplane().rect(34, 54).cutBlind(-DEPTH + 2)
    door = cq.Workplane().box(36, DOOR_THICKNESS, 56).translate((0, DEPTH / 2 + DOOR_THICKNESS / 2, 0))
    # Hinge on the +x edge of the frame front, so the door swings out and away.
    door = door.rotate((18, DEPTH / 2, 0), (18, DEPTH / 2, 1), -angle)
    handle = cq.Workplane().box(3, 3, 12).translate((-12, DEPTH / 2 + DOOR_THICKNESS + 1.5, 0))
    handle = handle.rotate((18, DEPTH / 2, 0), (18, DEPTH / 2, 1), -angle)
    context.show(frame, name="frame", features={"opening": frame.faces(">Y[-2]")})
    context.show(door, name="door")
    context.show(handle, name="handle")
