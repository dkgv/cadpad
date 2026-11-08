import cadquery as cq

show(cq.Workplane().sphere(200), name="sphere")
show(cq.Solid.makeTorus(20, 3), name="torus")
