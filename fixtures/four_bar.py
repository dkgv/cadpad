import cadquery as cq
import math

a, b, c, d = 40, 90, 70, 100

def rod(length, z=0, pins=False):
    shape = cq.Workplane().box(length+8, 8, 3).edges("|Z").fillet(3.9).translate((length/2, 0, z+1.5)).val()
    bores = [cq.Solid.makeCylinder(2, 30, cq.Vector(x, 0, -10)) for x in (0, length)]
    shape = shape.cut(*bores)
    if pins:
        for x in (0, length):
            pin = cq.Solid.makeCylinder(1.8, 11, cq.Vector(x, 0, -4))
            collar = cq.Solid.makeCylinder(3, 1, cq.Vector(x, 0, -1))
            shape = shape.fuse(pin, collar)
    end = [face for face in shape.Faces() if face.geomType() == "CYLINDER" and abs(face.Area()-12*math.pi) < 1e-5 and face.Center().x > length/2]
    return shape, {"end_bore":cq.Workplane().newObject(end)}

ground = cq.Workplane().box(d+20, 20, 3).translate((d/2, 0, -4)).val()
ground = ground.cut(*[cq.Solid.makeCylinder(2, 30, cq.Vector(x, 0, -10)) for x in (0, d)])
show(ground, name="ground", density=7800)
for name, length, z, pins in [("crank",a,0,True),("coupler",b,4,False),("rocker",c,0,True)]:
    shape, features = rod(length,z,pins)
    show(shape,name=name,features=features,density=7800)
show(cq.Workplane().box(4,4,10),name="stop",position=(88,33,1))
