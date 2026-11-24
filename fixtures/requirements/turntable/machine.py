"""Turntable (requirement fixture, case R1): a rigid rotary sweep.

Everything above the base rotates about the z axis through the origin. The
rotor rests on the base (face contact); towers stand on the rotor and clamps
sit on the towers, all rotationally clear of the base.
"""
import cadquery as cq


def machine(context):
    base = cq.Workplane().circle(80).extrude(10).translate((0, 0, -10))
    rotor = cq.Workplane().circle(50).extrude(6)
    towers = [
        cq.Workplane().box(10, 10, 60).translate((35, 0, 36)),
        cq.Workplane().box(10, 10, 60).translate((-35, 0, 36)),
    ]
    clamps = [
        cq.Workplane().box(6, 6, 6).translate((47, 0, 50)),
        cq.Workplane().box(6, 6, 6).translate((-47, 0, 50)),
    ]
    context.show(base, name="base")
    context.show(rotor, name="rotor")
    for i, tower in enumerate(towers, 1):
        context.show(tower, name=f"tower_{i}")
    for i, clamp in enumerate(clamps, 1):
        context.show(clamp, name=f"clamp_{i}")
