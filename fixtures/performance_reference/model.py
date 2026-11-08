import cadquery as cq

# Default: 100 bearing balls, approximately 1M triangles at the pinned display profile.
side = parameter("grid_side", 10, unit="count", minimum=10, maximum=40,
                 description="10 gives 100 instances; 20 gives a larger 400-instance reference")
if side != int(side):
    raise ValueError("grid_side must be an integer")
side = int(side)
ball = cq.Workplane().sphere(100)
for index in range(side * side):
    name = f"bearing_{index:03}"
    show(ball, name=name, position=((index % side - (side-1)/2)*240,
                                   (index // side - (side-1)/2)*240, 0))
    if index in range(0, 90, 3):
        dimension(f"diameter_{index}", name, (-100, 0, 0), (100, 0, 0), label=f"Ball {index}")
