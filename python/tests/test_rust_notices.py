import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile

import pytest


spec = importlib.util.spec_from_file_location(
    "rust_notices", Path(__file__).resolve().parents[2] / "scripts/rust_notices.py"
)
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


def package(name, *, source="registry+test", macro=False):
    return dict(id=name, name=name, version="1.0.0", source=source,
                targets=[dict(kind=["proc-macro" if macro else "lib"])], license="MIT", license_file=None, repository=None)


def node(name, dependencies=()):
    return dict(id=name, features=[], deps=[dict(pkg=dependency, dep_kinds=[dict(kind=kind)]) for dependency, kind in dependencies])


def test_runtime_build_macro_and_dev_edges_have_distinct_roles():
    metadata = dict(packages=[package("app", source=None), package("runtime"), package("macro", macro=True),
                              package("build"), package("shared"), package("syn"), package("test-only")],
                    resolve=dict(nodes=[node("app", [("runtime", None), ("macro", None), ("build", "build"), ("test-only", "dev")]),
                                        node("runtime", [("shared", None)]), node("macro", [("syn", None)]),
                                        node("build", [("shared", None)]), node("shared"), node("syn"), node("test-only")]))
    _, _, roles = notices.dependency_roles(metadata, ["app"])
    assert roles["runtime"] == {"runtime"}
    assert roles["macro"] == roles["syn"] == roles["build"] == {"build-tool"}
    assert roles["shared"] == {"runtime", "build-tool"}
    assert "test-only" not in roles


def fixture(tmp_path, *, escaping_notice=False):
    registry = tmp_path / "registry"
    root = registry / "src/index/example-1.0.0"
    root.mkdir(parents=True)
    files = {"Cargo.toml": b'[package]\nname="example"\nversion="1.0.0"\n', "src/lib.rs": b"pub fn example() {}", "LICENSE": b"publisher license\r\n"}
    for relative, content in files.items():
        file = root / relative
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_bytes(content)
    archive = registry / "cache/index/example-1.0.0.crate"
    archive.parent.mkdir(parents=True)
    with tarfile.open(archive, "w:gz") as stream:
        for relative, content in files.items():
            member = tarfile.TarInfo("example-1.0.0/" + relative)
            member.size = len(content)
            stream.addfile(member, io.BytesIO(content))
    if escaping_notice:
        outside = tmp_path / "private"
        outside.write_bytes(b"private material")
        (root / "LICENSE").unlink()
        (root / "LICENSE").symlink_to(outside)
    metadata = dict(packages=[package("app", source=None), {**package("example"), "manifest_path": str(root / "Cargo.toml")}],
                    resolve=dict(nodes=[node("app", [("example", None)]), node("example")]))
    lock = tmp_path / "Cargo.lock"
    lock.write_text(f'[[package]]\nname="example"\nversion="1.0.0"\nsource="registry+test"\nchecksum="{hashlib.sha256(archive.read_bytes()).hexdigest()}"\n')
    sysroot = tmp_path / "sysroot"
    licenses = sysroot / "share/doc/rust/licenses"
    licenses.mkdir(parents=True)
    (licenses / "MIT.txt").write_bytes(b"toolchain license")
    return metadata, lock, root, sysroot


def test_collected_notices_retain_bytes_and_verify_all_installed_archive_members(tmp_path):
    metadata, lock, _, sysroot = fixture(tmp_path)
    output = tmp_path / "output"
    result = notices.collect(metadata, lock, output, ["app"], rustc_version="fixture compiler", sysroot=sysroot)
    component = result["components"][0]
    assert component["provenance"]["archive_sha256_verified"]
    assert component["provenance"]["installed_source_matches_archive"]
    assert component["provenance"]["archive_files"] == 3
    assert (output / component["notices"][0]["file"]).read_bytes() == b"publisher license\r\n"
    assert not component["binary_reachability_verified"]
    assert not result["legal_clearance"]
    assert result["toolchain"]["notices"]
    with pytest.raises(FileExistsError):
        notices.collect(metadata, lock, output, ["app"], rustc_version="fixture compiler", sysroot=sysroot)


def test_modified_installed_source_is_flagged_separately_from_verified_archive(tmp_path):
    metadata, lock, root, sysroot = fixture(tmp_path)
    (root / "src/lib.rs").write_bytes(b"changed source")
    result = notices.collect(metadata, lock, tmp_path / "output", ["app"], rustc_version="fixture compiler", sysroot=sysroot)
    provenance = result["components"][0]["provenance"]
    assert provenance["archive_sha256_verified"]
    assert not provenance["installed_source_matches_archive"]
    assert provenance["differences"] == ["src/lib.rs"]


def test_escaping_notice_symlink_cannot_copy_private_content(tmp_path):
    metadata, lock, _, sysroot = fixture(tmp_path, escaping_notice=True)
    output = tmp_path / "output"
    result = notices.collect(metadata, lock, output, ["app"], rustc_version="fixture compiler", sysroot=sysroot)
    assert result["components"][0]["notices"] == []
    assert any(item["issue"] == "unsafe-or-missing-license-file" for item in result["review_required"])
    assert all(file.read_bytes() != b"private material" for file in output.rglob("*") if file.is_file())


def test_requested_root_cannot_be_silently_omitted():
    with pytest.raises(ValueError, match="production root"):
        notices.dependency_roles(dict(packages=[], resolve=dict(nodes=[])), ["missing"])


def test_unarchived_source_files_are_flagged_but_cargo_cache_markers_are_allowed(tmp_path):
    metadata, lock, root, sysroot = fixture(tmp_path)
    (root / ".cargo-ok").write_bytes(b"cache marker")
    (root / "src/extra.rs").write_bytes(b"unexpected source")
    result = notices.collect(metadata, lock, tmp_path / "output", ["app"], rustc_version="fixture compiler", sysroot=sysroot)
    provenance = result["components"][0]["provenance"]
    assert provenance["archive_sha256_verified"]
    assert not provenance["installed_source_matches_archive"]
    assert provenance["installed_extra_files"] == ["src/extra.rs"]
