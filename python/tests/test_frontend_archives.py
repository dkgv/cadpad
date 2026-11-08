import base64
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile

import pytest


spec = importlib.util.spec_from_file_location(
    "verify_frontend_archives", Path(__file__).resolve().parents[2] / "scripts/verify_frontend_archives.py"
)
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


def fixture(tmp_path, *, unsafe=False, duplicate=False, symlink=False):
    files = {"package.json": b'{"name":"example","version":"1.0.0"}', "index.js": b"export const value=42;", "LICENSE": b"license\r\n"}
    archive = tmp_path / "archive.tgz"
    with tarfile.open(archive, "w:gz") as stream:
        for name, content in files.items():
            member = tarfile.TarInfo("package/" + name)
            member.size = len(content)
            stream.addfile(member, io.BytesIO(content))
        if unsafe:
            stream.addfile(tarfile.TarInfo("package/../../outside"), io.BytesIO())
        if duplicate:
            stream.addfile(tarfile.TarInfo("package/index.js"), io.BytesIO())
        if symlink:
            member = tarfile.TarInfo("package/link")
            member.type, member.linkname = tarfile.SYMTYPE, "/private"
            stream.addfile(member)
    integrity = "sha512-" + base64.b64encode(hashlib.sha512(archive.read_bytes()).digest()).decode()
    locked = {"version": "1.0.0", "integrity": integrity, "resolved": "https://example.invalid/example.tgz"}
    lock = tmp_path / "package-lock.json"
    lock.write_text(json.dumps(dict(packages={"node_modules/example": locked})))
    component = dict(id="example", name="example", version="1.0.0", installed_root="node_modules/example",
                     locked_package=locked, package_json_sha256=hashlib.sha256(files["package.json"]).hexdigest(),
                     modules=[dict(path="index.js", source_sha256=hashlib.sha256(files["index.js"]).hexdigest())],
                     notices=[dict(source="LICENSE", sha256=hashlib.sha256(files["LICENSE"]).hexdigest())], embedded_sources=[])
    snapshot = dict(format="cadpad-frontend-notices-v1", package_lock_sha256=verifier.file_hash(lock).hex(), components=[component], virtual_modules=[])
    inventory = tmp_path / "inventory.json"
    inventory.write_text(json.dumps(snapshot))
    cache = tmp_path / "cache"
    cache.mkdir()
    cached = cache / (hashlib.sha256(integrity.encode()).hexdigest() + ".tgz")
    cached.write_bytes(archive.read_bytes())
    return inventory, lock, cache, cached


def test_locked_archive_matches_module_metadata_and_exact_notice_bytes(tmp_path):
    inventory, lock, cache, _ = fixture(tmp_path)
    result = verifier.verify(inventory, lock, cache)
    assert result["selected_payload_matches"]
    assert result["unique_archives"] == 1
    assert len(result["components"][0]["comparisons"]) == 3
    assert not result["legal_clearance"]
    assert not result["publisher_prebundle_reproduction_verified"]


def test_changed_module_and_notice_hashes_fail_without_rewriting_inventory(tmp_path):
    inventory, lock, cache, _ = fixture(tmp_path)
    snapshot = json.loads(inventory.read_text())
    snapshot["components"][0]["modules"][0]["source_sha256"] = "f" * 64
    snapshot["components"][0]["notices"][0]["sha256"] = "e" * 64
    inventory.write_text(json.dumps(snapshot))
    original = inventory.read_bytes()
    result = verifier.verify(inventory, lock, cache)
    assert not result["selected_payload_matches"]
    assert sum(not item["matches"] for item in result["components"][0]["comparisons"]) == 2
    assert inventory.read_bytes() == original


def test_cached_archive_integrity_is_checked_before_inspection(tmp_path):
    inventory, lock, cache, cached = fixture(tmp_path)
    cached.write_bytes(b"changed archive")
    with pytest.raises(ValueError, match="locked integrity"):
        verifier.verify(inventory, lock, cache)


@pytest.mark.parametrize("variant", ["unsafe", "duplicate", "symlink"])
def test_unsafe_archive_members_are_not_extracted(tmp_path, variant):
    inventory, lock, cache, _ = fixture(tmp_path, **{variant: True})
    with pytest.raises(ValueError, match="Unsafe|Duplicate|Non-regular"):
        verifier.verify(inventory, lock, cache)
    assert not (tmp_path.parent / "outside").exists()


def test_missing_publisher_map_cannot_match_missing_hash(tmp_path):
    inventory, lock, cache, _ = fixture(tmp_path)
    snapshot = json.loads(inventory.read_text())
    snapshot["components"][0]["embedded_sources"] = [dict(module="index.js", publisher_map_sha256=None, dependencies=[])]
    inventory.write_text(json.dumps(snapshot))
    result = verifier.verify(inventory, lock, cache)
    assert not result["selected_payload_matches"]
    assert not result["components"][0]["comparisons"][-1]["matches"]


def test_offline_mode_does_not_fetch_missing_archive(tmp_path):
    inventory, lock, cache, cached = fixture(tmp_path)
    cached.unlink()
    with pytest.raises(FileNotFoundError, match="missing from cache"):
        verifier.verify(inventory, lock, cache)
