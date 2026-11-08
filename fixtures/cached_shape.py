import cadquery as cq
import sys
from pathlib import Path

print("__source_executed__")
cache = Path(sys.argv[sys.argv.index("--cache") + 1])
with (cache / "source-executions").open("a") as stream:
    stream.write("executed\n")
width = parameter("width", 40, unit="mm", minimum=20, maximum=100)


def make_plate():
    return cq.Workplane().box(width, 20, 6).faces(">Z").workplane().hole(4).val()


# Test-only observation; the geometry builder itself has no I/O or state reads.
def observe(frame, event, argument):
    if event == "call" and frame.f_code is make_plate.__code__:
        print("__native_builder_executed__")
        with (cache / "builder-executions").open("a") as stream:
            stream.write("executed\n")


sys.setprofile(observe)
try:
    plate = cached_shape("plate", make_plate)
finally:
    sys.setprofile(None)
show(plate, name="plate", features={"top": cq.Workplane(obj=plate).faces(">Z")}, density=2700)
