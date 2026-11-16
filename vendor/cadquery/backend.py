"""Build a pinned upstream CadQuery wheel with the reviewed lazy-VTK patch."""
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import urllib.request
import zipfile

from hatchling import build

ROOT = Path(__file__).resolve().parent


def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    destination = str(Path(wheel_directory).resolve())
    upstream = json.loads((ROOT / "UPSTREAM.json").read_text())
    with urllib.request.urlopen(upstream["url"], timeout=60) as response:
        archive = response.read()
    if hashlib.sha256(archive).hexdigest() != upstream["sha256"]:
        raise RuntimeError("Upstream CadQuery wheel checksum mismatch")
    with tempfile.TemporaryDirectory(prefix="cadpad-cadquery-") as temporary:
        stage = Path(temporary)
        with zipfile.ZipFile(io.BytesIO(archive)) as wheel:
            for name in wheel.namelist():
                path = Path(name)
                if path.parts[0] != "cadquery" or ".." in path.parts or name.endswith("/"):
                    continue
                target = stage / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(wheel.read(name))
        patch = str(ROOT / "no-vtk.patch")
        subprocess.run(["git", "apply", "--check", patch], cwd=stage, check=True)
        subprocess.run(["git", "apply", patch], cwd=stage, check=True)
        for name in ("pyproject.toml", "LICENSE"):
            shutil.copy2(ROOT / name, stage / name)
        previous = Path.cwd()
        try:
            os.chdir(stage)
            return build.build_wheel(destination, config_settings)
        finally:
            os.chdir(previous)


def build_sdist(sdist_directory, config_settings=None):
    return build.build_sdist(sdist_directory, config_settings)


def get_requires_for_build_wheel(config_settings=None):
    return []


def get_requires_for_build_sdist(config_settings=None):
    return []
