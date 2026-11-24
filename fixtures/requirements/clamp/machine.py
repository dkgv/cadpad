"""Sliding clamp and retractable arms (requirement fixtures, cases R2 and R4).

* `clamp_theta` swings three clamps around tower_1 (the string-clamp sweep).
* `comb_travel` extends a comb tooth on tower_1; at 8 mm it reaches the top
  clamp's path. It is the planted fault of case R2.
* `extension` slides the tower arms; 5 is fully retracted. The arms stay clear
  of the racket envelope, a loading region left of the rotor.
"""
import math

import cadquery as cq

TOWER_COMB_TRAVEL = [0, 4, 8]
CLAMP_RADIUS = 12
TOWER_X = 50


def machine(context):
    theta = context.parameter("clamp_theta", 90.0, unit="deg", minimum=48.0, maximum=132.0)
    comb = context.parameter("comb_travel", 0.0, unit="mm", minimum=0.0, maximum=8.0)
    extension = context.parameter("extension", 5.0, unit="one", minimum=5.0, maximum=15.0)

    base = cq.Workplane().box(220, 120, 10).translate((0, 0, -5))
    rotor = cq.Workplane().circle(30).extrude(6)
    tower = cq.Workplane().box(10, 10, 60).translate((TOWER_X, 0, 30))
    if comb > 0:
        tooth = cq.Workplane().box(4, comb, 4).translate((TOWER_X, 5 + comb / 2, 50))
        tower = tower.union(tooth)
    context.show(base, name="base")
    context.show(rotor, name="rotor")
    context.show(tower, name="tower_1")
    for i in range(3):
        z = 30 + 10 * i
        a = math.radians(theta)
        x = TOWER_X + CLAMP_RADIUS * math.cos(a)
        y = CLAMP_RADIUS * math.sin(a)
        clamp = cq.Workplane().box(4, 4, 4).translate((x, y, z))
        context.show(clamp, name=f"clamp_{i + 1}")
    for i, y in enumerate((-12, 12), 1):
        arm = cq.Workplane().box(4, 4, 30).translate((-50 - extension, y, 15))
        context.show(arm, name=f"tower_arm_{i}")
    envelope = cq.Workplane().box(25, 40, 40).translate((-82.5, 0, 20))
    context.show(envelope, name="racket_envelope")
