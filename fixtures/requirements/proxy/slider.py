import cadquery as cq

def slider(context):
    context.show(cq.Workplane().box(20, 20, 20), name="slider")
