import cadquery as cq

length = parameter("length", 10, unit="mm", minimum=1, maximum=100)
show(cq.Workplane().box(length, 10, 10), name="box")
show(cq.Workplane().sphere(5), name="round", position=(length + 20, 0, 0))

# Test-only observation of actual tessellator calls, without changing its code
# or the producer policy used to decide reuse.
import sys
from pathlib import Path
from ocp_tessellate.tessellator import tessellate

cache = Path(sys.argv[sys.argv.index("--cache") + 1])
(cache / "source-ran").write_text("source executed")
observed_code = tessellate.__code__
observed_cells = dict(zip(observed_code.co_freevars, tessellate.__closure__ or ()))

def profile(frame, event, arg):
    if event == "call" and frame.f_code is observed_code and all(
        frame.f_locals.get(name) is cell.cell_contents for name, cell in observed_cells.items()
    ):
        with (cache / "tessellator-calls").open("a") as stream:
            stream.write("called\n")

sys.setprofile(profile)
