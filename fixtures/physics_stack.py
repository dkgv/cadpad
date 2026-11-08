import cadquery as cq

# CAD lengths are mm. SCALE is replaced by the native reference for the two-size matrix.
SCALE = 1.0
# Floor top is z=0. Long blocks keep the sliding upper body on the lower face
# through the slide and stop distances (~160 mm at scale 1).
# Lower starts 0.1*SCALE mm above the floor; upper starts 0.1*SCALE mm above lower.
show(
    cq.Workplane().box(2000 * SCALE, 200 * SCALE, 10 * SCALE),
    name="floor",
    position=(0, 0, -5 * SCALE),
)
show(
    cq.Workplane().box(400 * SCALE, 40 * SCALE, 20 * SCALE),
    name="lower",
    position=(0, 0, 10.1 * SCALE),
)
show(
    cq.Workplane().box(400 * SCALE, 40 * SCALE, 20 * SCALE),
    name="upper",
    position=(0, 0, 30.3 * SCALE),
)
