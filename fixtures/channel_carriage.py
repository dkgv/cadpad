import cadquery as cq

stop_x = parameter("stop_x", 70, unit="mm", minimum=30, maximum=95,
                   description="Stop position along the rail")
base = cq.Workplane().box(200, 20, 4).translate((0, 0, -5))
left = cq.Workplane().box(200, 4, 12).translate((0, 8, 3))
right = cq.Workplane().box(200, 4, 12).translate((0, -8, 3))
show(base.union(left).union(right), name="rail")
show(cq.Workplane().box(20, 8, 4), name="carriage")
show(cq.Workplane().box(2, 12, 12), name="stop", position=(stop_x, 0, 3))
dimension("channel_width", "rail", (0, -6, 3), (0, 6, 3), label="Channel width")
