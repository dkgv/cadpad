import cadquery as cq

# A uniform box with its local center at the body origin. CAD lengths are mm.
# The grounded hinge attaches at the local top-face center, 100 mm above it.
SCALE = 1.0
show(cq.Workplane().box(10 * SCALE, 10 * SCALE, 200 * SCALE),
     name="pendulum", position=(0, 0, -100 * SCALE))
