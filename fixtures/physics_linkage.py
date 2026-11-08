import cadquery as cq

# Uniform rectangular bars; each moving part's local origin is its first pin.
SCALE = 1.0
show(cq.Workplane().box(120*SCALE, 16*SCALE, 4*SCALE)
     .translate((50*SCALE, 0, -8*SCALE)), name="ground")
for name, length in [("crank", 40), ("coupler", 90), ("rocker", 70)]:
    show(cq.Workplane().box(length*SCALE, 6*SCALE, 4*SCALE)
         .translate((length*SCALE/2, 0, 0)), name=name)
