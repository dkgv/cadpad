"""Packaging notices must follow selected inputs, including non-import licenses."""
import base64
from email.message import Message
import hashlib
import importlib.metadata
import importlib.util
from pathlib import Path
from types import SimpleNamespace

import pytest


spec = importlib.util.spec_from_file_location(
    "worker_notices", Path(__file__).resolve().parents[2] / "scripts/worker_notices.py"
)
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


class Distribution:
    def __init__(self, root, name, files, *, declared=None):
        self.root, self.version = root, "1.0"
        self.metadata = Message()
        self.metadata["Name"] = name
        if declared:
            self.metadata["License-File"] = declared
        self.files = []
        for path, data in files.items():
            target = root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            item = importlib.metadata.PackagePath(path)
            item.hash = SimpleNamespace(mode="sha256", value=base64.urlsafe_b64encode(hashlib.sha256(data).digest()).decode().rstrip("="))
            self.files.append(item)

    def locate_file(self, item):
        return self.root / item


@pytest.fixture
def setup(tmp_path):
    repository = tmp_path / "repo"
    repository.mkdir()
    (repository / "uv.lock").write_text('[[package]]\nname="example-wheel"\nversion="1.0"\n[[package]]\nname="pyinstaller"\nversion="1.0"\n')
    installed = tmp_path / "installed"
    installed.mkdir()
    bootloader = Distribution(installed, "PyInstaller", {"pyinstaller-1.0.dist-info/licenses/COPYING.txt": b"bootloader exception"})
    return repository, installed, bootloader


def test_selected_package_retains_external_notice_and_arbitrary_declared_license(setup, tmp_path):
    repository, installed, bootloader = setup
    selected = Distribution(installed, "example_wheel", {
        "Example/module.py": b"pass\n",
        "example_wheel/LICENSE": b"publisher license\r\n",
        "example_wheel-1.0.dist-info/licenses/TERMS": b"declared terms",
        "example_wheel-1.0.dist-info/METADATA": b"Name: example_wheel\n",
    }, declared="TERMS")
    unused = Distribution(installed, "unused", {"unused.py": b"pass", "unused.dist-info/LICENSE": b"unused"})
    output = tmp_path / "output"
    result = notices.collect([("Example.module", str(installed / "Example/module.py"), "PYMODULE")], output, repository, distributions=[bootloader, selected, unused])
    assert [component["id"] for component in result["components"]] == ["example-wheel", "pyinstaller"]
    component = result["components"][0]
    assert len(component["notices"]) == 2
    assert { (output / item["file"]).read_bytes() for item in component["notices"] } == {b"publisher license\r\n", b"declared terms"}
    assert component["files"][0]["record_sha256_matches"] is True
    assert component["declared_license"] is None
    assert result["legal_clearance"] is False
    with pytest.raises(FileExistsError):
        notices.collect([], output, repository, distributions=[bootloader])


def test_native_hash_mismatch_and_unowned_inputs_remain_review_items(setup, tmp_path):
    repository, installed, bootloader = setup
    selected = Distribution(installed, "example-wheel", {"libexample.dylib": b"original", "example.dist-info/LICENSE": b"license"})
    (installed / "libexample.dylib").write_bytes(b"modified")
    unknown = tmp_path / "unowned.dylib"
    unknown.write_bytes(b"unknown")
    result = notices.collect([("libexample.dylib", str(installed / "libexample.dylib"), "BINARY"), ("unknown", str(unknown), "BINARY")], tmp_path / "output", repository, distributions=[bootloader, selected])
    assert result["components"][0]["files"][0]["record_sha256_matches"] is False
    assert {item["issue"] for item in result["review_required"]} >= {"installed-record-hash-mismatch", "vendored-native-libraries-source-and-license-review-required", "unowned-pyinstaller-inputs"}
    assert len(result["unowned_inputs"]) == 1


def test_notice_symlink_cannot_copy_files_outside_installation(setup, tmp_path):
    repository, installed, bootloader = setup
    selected = Distribution(installed, "example-wheel", {"example.py": b"pass", "example.dist-info/LICENSE": b"license"})
    outside = tmp_path / "private"
    outside.write_bytes(b"do not copy")
    license_path = installed / "example.dist-info/LICENSE"
    license_path.unlink()
    license_path.symlink_to(outside)
    result = notices.collect([("example", str(installed / "example.py"), "PYMODULE")], tmp_path / "output", repository, distributions=[bootloader, selected])
    assert result["components"][0]["notices"] == []
    assert any(item["issue"] == "unsafe-or-missing-publisher-material" for item in result["review_required"])
    assert all(path.read_bytes() != b"do not copy" for path in (tmp_path / "output").rglob("*") if path.is_file())


def test_unknown_analysis_schema_is_rejected(tmp_path):
    path = tmp_path / "Analysis-00.toc"
    path.write_text(repr(([], [], "not the pinned schema")))
    with pytest.raises(ValueError, match="schema"):
        notices.analysis_entries(path)


def test_distribution_name_cannot_escape_output_directory():
    with pytest.raises(ValueError, match="Unsafe distribution name"):
        notices.normalized("../../outside")
