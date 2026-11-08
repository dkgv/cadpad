import cadquery as cq

# CAD lengths are mm. The vertical mechanism coordinate uses metres.
show(cq.Workplane().box(20, 20, 20), name="slider", position=(0, 0, 50))
show(cq.Workplane().box(80, 80, 10), name="stop", position=(0, 0, 125))
