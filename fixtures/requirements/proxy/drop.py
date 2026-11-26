"""Falling body (requirement fixture, proxy simulation contact impulse).

The ball is a dynamic body on a vertical prismatic joint; the plate is static
geometry underneath it. Gravity makes the ball hit the plate.
"""
import cadquery as cq


def drop(context):
    context.show(cq.Workplane().box(40, 40, 2).translate((0, 0, -1)), name="plate")
    context.show(cq.Workplane().box(10, 10, 10).translate((0, 0, 7)), name="ball")
