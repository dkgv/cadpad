import cadquery as cq

# CAD lengths are mm. SCALE is replaced by the native reference for the two-size matrix.
SCALE = 1.0
# The floor top is z=0; the block starts 0.1*SCALE mm above it.
show(
    cq.Workplane().box(2000 * SCALE, 200 * SCALE, 10 * SCALE),
    name="floor",
    position=(0, 0, -5 * SCALE),
)
show(
    cq.Workplane().box(40 * SCALE, 40 * SCALE, 20 * SCALE),
    name="block",
    position=(0, 0, 10.1 * SCALE),
)
