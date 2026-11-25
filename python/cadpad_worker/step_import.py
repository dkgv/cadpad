"""Named STEP assembly import using OCCT XCAF labels and occurrences."""
from __future__ import annotations

import cadquery as cq


def _label_name(label, fallback):
    from OCP.TDataStd import TDataStd_Name

    attr = TDataStd_Name()
    if label.FindAttribute(TDataStd_Name.GetID_s(), attr):
        value = attr.Get().ToExtString().strip()
        if value:
            return value
    return fallback


def _unique_name(name, used):
    base = name or "part"
    candidate = base
    suffix = 2
    while candidate in used:
        candidate = f"{base}_{suffix}"
        suffix += 1
    used.add(candidate)
    return candidate


def import_named_step(path, root_name="imported"):
    """Return a CadQuery Assembly retaining STEP product names and occurrences."""
    from OCP.IFSelect import IFSelect_RetDone
    from OCP.STEPCAFControl import STEPCAFControl_Reader
    from OCP.TCollection import TCollection_ExtendedString
    from OCP.TDF import TDF_Label, TDF_LabelSequence
    from OCP.TDocStd import TDocStd_Document
    from OCP.XCAFDoc import XCAFDoc_DocumentTool, XCAFDoc_ShapeTool

    document = TDocStd_Document(TCollection_ExtendedString("CadpadSTEP"))
    reader = STEPCAFControl_Reader()
    reader.SetNameMode(True)
    if reader.ReadFile(str(path)) != IFSelect_RetDone or not reader.Transfer(document):
        raise ValueError(f"STEP file could not be loaded: {path.name}")
    shape_tool = XCAFDoc_DocumentTool.ShapeTool_s(document.Main())
    roots = TDF_LabelSequence()
    shape_tool.GetFreeShapes(roots)
    if roots.Length() == 0:
        raise ValueError(f"STEP file contains no named shapes: {path.name}")

    def children(label):
        referred = label
        if XCAFDoc_ShapeTool.IsComponent_s(label):
            referred = TDF_Label()
            XCAFDoc_ShapeTool.GetReferredShape_s(label, referred)
        sequence = TDF_LabelSequence()
        if XCAFDoc_ShapeTool.GetComponents_s(referred, sequence):
            return [(sequence.Value(i), referred) for i in range(1, sequence.Length() + 1)]
        return []

    def build(label, display_name, used):
        refs = children(label)
        if refs:
            assembly = cq.Assembly(name=display_name)
            child_names = set()
            for index, (component, _definition) in enumerate(refs, 1):
                definition = component
                if XCAFDoc_ShapeTool.IsComponent_s(component):
                    definition = TDF_Label()
                    XCAFDoc_ShapeTool.GetReferredShape_s(component, definition)
                child_name = _unique_name(_label_name(component, _label_name(definition, f"part_{index}")), child_names)
                child = build(definition, child_name, child_names)
                loc = cq.Location(XCAFDoc_ShapeTool.GetLocation_s(component)) if XCAFDoc_ShapeTool.IsComponent_s(component) else cq.Location()
                assembly.add(child, name=child_name)
                assembly.children[-1].loc = loc
            return assembly
        shape_label = label
        if XCAFDoc_ShapeTool.IsComponent_s(label):
            shape_label = TDF_Label()
            XCAFDoc_ShapeTool.GetReferredShape_s(label, shape_label)
        shape = XCAFDoc_ShapeTool.GetShape_s(shape_label)
        if shape.IsNull():
            raise ValueError(f"STEP component has no geometry: {display_name}")
        return cq.Shape.cast(shape)

    if roots.Length() == 1:
        label = roots.Value(1)
        return build(label, _label_name(label, root_name or "part_1"), set())
    result = cq.Assembly(name="__step_root__")
    used = set()
    for index in range(1, roots.Length() + 1):
        label = roots.Value(index)
        name = _unique_name(_label_name(label, root_name if index == 1 else f"part_{index}"), used)
        result.add(build(label, name, set()), name=name)
    return result
