import cadquery as cq

show(cq.Workplane("XY").box(80, 10, 10), name="lever", position=(40, 0, 0))
show(cq.Workplane("XY").box(20, 10, 10), name="shoe", position=(75, 12, 0))
