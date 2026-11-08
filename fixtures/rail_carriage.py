import cadquery as cq

length = parameter("rail_length", 200, unit="mm", minimum=140, maximum=1000,
                   description="Length of the rail")
rail = cq.Workplane("XY").box(length, 30, 10).edges("|Z").fillet(2)
rail = rail.faces(">Z").workplane().pushPoints([(-60, 0), (60, 0)]).hole(6)
show(rail, name="rail", features={"mounting_holes": cq.Workplane(obj=rail.val()).newObject([f for f in rail.val().Faces() if f.geomType() == "CYLINDER" and abs(f.Area() - 60 * 3.141592653589793) < 1e-6])})
carriage = cq.Workplane("XY").box(40, 40, 16).faces(">Z").workplane().hole(8)
show(carriage, name="carriage", position=(0, 0, 13.02))
dimension("mounting_spacing", "rail", ( -60, 0, 5), (60, 0, 5), label="Mount spacing")
