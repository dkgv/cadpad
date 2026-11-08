import cadquery as cq
import math

stop_angle = parameter("stop_angle", 90, unit="deg", minimum=30, maximum=330,
                       description="End-stop angle around the circular guide")
angle = math.radians(stop_angle)
show(cq.Workplane().circle(54).circle(46).extrude(2).translate((0, 0, -5)), name="rail")
show(cq.Workplane().box(8, 4, 4), name="carriage", position=(50, 0, 0))
show(cq.Workplane().box(4, 4, 8), name="stop", position=(50*math.cos(angle), 50*math.sin(angle), 1))
