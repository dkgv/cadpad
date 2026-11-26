"""Housing fixture for the ISO 286 H7 requirements integration case."""
import cadquery as cq


def housing(context):
    bore_diameter = 8.007
    shape = cq.Workplane().box(16, 16, 12).faces(">Z").workplane().hole(bore_diameter)
    bore = shape.faces("%CYLINDER")
    context.show(shape, name="housing", features={"shaft_bore": bore})
