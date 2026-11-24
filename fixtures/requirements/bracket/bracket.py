"""Printable L bracket (requirement fixture, case E5): one solid, 120 mm long."""
import cadquery as cq


def bracket(context):
    plate = cq.Workplane().box(120, 40, 6).translate((0, 0, 3))
    wall = cq.Workplane().box(120, 6, 34).translate((0, -17, 17))
    shape = plate.union(wall).faces(">Z").workplane().pushPoints([(-40, 5), (40, 5)]).hole(5)
    context.show(shape, name="bracket")
