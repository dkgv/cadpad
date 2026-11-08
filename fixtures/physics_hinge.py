import cadquery as cq

# Reference rotor centered on its grounded Z hinge. Lengths are millimetres.
SCALE = 1.0
show(cq.Workplane().box(100 * SCALE, 20 * SCALE, 10 * SCALE), name="rotor")
