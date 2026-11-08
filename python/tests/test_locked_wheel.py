import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import zipfile

import pytest


spec = importlib.util.spec_from_file_location(
    "verify_locked_wheel", Path(__file__).resolve().parents[2] / "scripts/verify_locked_wheel.py"
)
wheel_audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wheel_audit)


def fixture(tmp_path, *, defective_record=False, unsafe_member=False):
    files = {"example.py": b"pass\n", "example-1.0.dist-info/METADATA": b"Name: example\nVersion: 1.0\n",
             "example-1.0.dist-info/WHEEL": b"Wheel-Version: 1.0\n", "example-1.0.dist-info/LICENSE": b"license\n"}
    if unsafe_member:
        files["../outside"] = b"escape"
    record = "example-1.0.dist-info/RECORD"
    rows = []
    for path, content in files.items():
        digest = base64.urlsafe_b64encode(hashlib.sha256(content).digest()).decode().rstrip("=")
        if defective_record and path.endswith("/WHEEL"):
            digest = base64.urlsafe_b64encode(b"0" * 32).decode().rstrip("=")
        rows.append(f"{path},sha256={digest},{len(content)}")
    files[record] = ("\n".join(rows) + f"\n{record},,\n").encode()
    wheel = tmp_path / "example.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        for path, content in files.items():
            archive.writestr(path, content)
    lock = tmp_path / "uv.lock"
    lock.write_text(f'[[package]]\nname="example"\nversion="1.0"\nwheels=[{{url="https://example.invalid/example.whl",hash="sha256:{wheel_audit.sha256(wheel)}"}}]\n')
    inventory = tmp_path / "inventory.json"
    inventory.write_text(json.dumps(dict(format="cadpad-worker-notices-v1", uv_lock_sha256=wheel_audit.sha256(lock), components=[
        dict(id="example", version="1.0", files=[dict(installed_path="example.py", destination="example", source_sha256=hashlib.sha256(files["example.py"]).hexdigest())],
             notices=[dict(source="example-1.0.dist-info/LICENSE", file="notice", sha256=hashlib.sha256(files["example-1.0.dist-info/LICENSE"]).hexdigest())], provenance=[])
    ])))
    return wheel, inventory, lock


def test_exact_archive_matches_selected_code_and_supplied_notice(tmp_path):
    wheel, inventory, lock = fixture(tmp_path)
    result = wheel_audit.verify(wheel, inventory, lock, "example")
    assert result["status"] == "inputs-match"
    assert result["archive_sha256_verified"]
    assert len(result["comparisons"]) == 2
    assert not result["legal_clearance"]
    assert not result["exact_embedded_native_sources_verified"]


def test_upstream_record_defect_is_distinct_from_changed_selected_input(tmp_path):
    wheel, inventory, lock = fixture(tmp_path, defective_record=True)
    result = wheel_audit.verify(wheel, inventory, lock, "example")
    assert result["status"] == "payload-matches-with-review-items"
    assert result["archive_record_issues"][0]["issue"] == "archive-record-hash-mismatch"
    data = json.loads(inventory.read_text())
    data["components"][0]["files"][0]["source_sha256"] = "f" * 64
    inventory.write_text(json.dumps(data))
    result = wheel_audit.verify(wheel, inventory, lock, "example")
    assert result["status"] == "input-mismatch"
    assert not result["selected_inputs_match"]


def test_modified_archive_fails_locked_hash_verification(tmp_path):
    wheel, inventory, lock = fixture(tmp_path)
    with wheel.open("ab") as stream:
        stream.write(b"changed")
    with pytest.raises(ValueError, match="SHA-256"):
        wheel_audit.verify(wheel, inventory, lock, "example")


def test_archive_path_traversal_is_rejected_without_extraction(tmp_path):
    wheel, inventory, lock = fixture(tmp_path, unsafe_member=True)
    with pytest.raises(ValueError, match="Unsafe wheel member"):
        wheel_audit.verify(wheel, inventory, lock, "example")
    assert not (tmp_path.parent / "outside").exists()


def test_installation_bookkeeping_differences_do_not_hide_changed_publisher_metadata(tmp_path):
    wheel, inventory, lock = fixture(tmp_path)
    data = json.loads(inventory.read_text())
    files = data["components"][0]["files"]
    files.extend([dict(installed_path="example-1.0.dist-info/INSTALLER", destination="installer", source_sha256="f" * 64),
                  dict(installed_path="example-1.0.dist-info/RECORD", destination="record", source_sha256="f" * 64)])
    inventory.write_text(json.dumps(data))
    result = wheel_audit.verify(wheel, inventory, lock, "example")
    assert result["selected_payload_matches"]
    assert not result["selected_inputs_match"]
    assert len(result["installation_metadata_differences"]) == 2
    files.append(dict(installed_path="example-1.0.dist-info/WHEEL", destination="wheel", source_sha256="f" * 64))
    inventory.write_text(json.dumps(data))
    result = wheel_audit.verify(wheel, inventory, lock, "example")
    assert not result["selected_payload_matches"]
