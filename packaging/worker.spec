# Build with .venv/bin/python -m PyInstaller --clean --noconfirm packaging/worker.spec
# One-folder layout keeps shared libraries accessible for diagnosis/replacement.
from PyInstaller.utils.hooks import collect_all
from pathlib import Path
import importlib.util
import tempfile
from PyInstaller.config import CONF

repository = Path(SPECPATH).parent
all_data, all_binaries, hidden = [], [], []
# The headless CadQuery/OCP runtime does not include VTK.
for package in ("cadquery", "OCP", "ocp_tessellate"):
    data, binaries, imports = collect_all(
        package,
        # Bytecode is already included in PYZ; don't duplicate package sources.
        include_py_files=False,
        # Cadpad has its own viewer. Don't force in CadQuery's optional browser
        # figure server (Trame) or its Sphinx documentation extension.
        filter_submodules=lambda name: name not in {"cadquery.fig", "cadquery.vis", "cadquery.cq_directive"},
    )
    all_data.extend(data)
    all_binaries.extend(binaries)
    hidden.extend(imports)
a = Analysis([str(repository / "scripts/worker_entry.py")],
             pathex=[str(repository / "python")], binaries=all_binaries,
             datas=all_data, hiddenimports=hidden, hookspath=[],
             excludes=["pytest", "tkinter", "IPython", "jupyter", "matplotlib", "vtk", "vtkmodules", "trame", "trame_vtk"])
# Fail closed if an upstream packaging change reintroduces VTK binaries.
if any("vtk" in Path(destination).name.lower() for destination, *_ in a.binaries):
    raise RuntimeError("VTK binary unexpectedly included in the headless worker")
# collect_all(import_name) misses notices installed outside that import root
# (notably cadquery_ocp_novtk/LICENSE). Resolve ownership from final Analysis inputs.
notice_spec = importlib.util.spec_from_file_location("worker_notices", repository / "scripts/worker_notices.py")
notice_module = importlib.util.module_from_spec(notice_spec)
notice_spec.loader.exec_module(notice_module)
notice_parent = Path(tempfile.mkdtemp(prefix="notices-", dir=CONF["workpath"]))
notice_root = notice_parent / "third-party"
notice_module.collect([*a.scripts, *a.pure, *a.binaries, *a.datas], notice_root, repository)
a.datas.extend(("third-party/" + path.relative_to(notice_root).as_posix(), str(path), "DATA")
               for path in sorted(notice_root.rglob("*")) if path.is_file())
pyz = PYZ(a.pure)
exe = EXE(pyz, a.scripts, [], exclude_binaries=True, name="cadpad-worker",
          debug=False, bootloader_ignore_signals=False, strip=False, upx=False,
          console=True, target_arch=None, codesign_identity=None, entitlements_file=None)
coll = COLLECT(exe, a.binaries, a.datas, strip=False, upx=False, name="cadpad-worker")
