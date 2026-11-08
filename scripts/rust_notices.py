"""Collect Cargo dependency notices for runtime and build-tool closures separately.

Uses locked offline Cargo metadata and local registry archives. This is an
overinclusive dependency inventory, not proof of binary/linker reachability.
"""
from __future__ import annotations

import argparse
from collections import deque
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tomllib


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def dependency_roles(metadata, root_names):
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    roots = [package["id"] for package in packages.values() if package["name"] in root_names and package["source"] is None]
    if {packages[root]["name"] for root in roots} != set(root_names):
        raise ValueError("Requested production root is absent from Cargo metadata")
    pending, roles = deque((root, "runtime") for root in roots), {}
    while pending:
        package_id, role = pending.popleft()
        if any("proc-macro" in target["kind"] for target in packages[package_id]["targets"]):
            role = "build-tool"
        if role in roles.get(package_id, set()):
            continue
        roles.setdefault(package_id, set()).add(role)
        for dependency in nodes[package_id]["deps"]:
            kinds = {kind["kind"] for kind in dependency["dep_kinds"]}
            if None in kinds:
                pending.append((dependency["pkg"], role))
            if "build" in kinds:
                pending.append((dependency["pkg"], "build-tool"))
            # Dev edges never make a production dependency by themselves.
    return packages, nodes, roles


def is_notice(path):
    return bool(re.match(r"^(licen[cs]e|copying|copyright|notice)([._-].*|$)", path.name, re.I)) or any(
        part.lower() in {"licenses", "licences"} for part in path.parts[:-1])


def inspect_registry_archive(root, name, version, checksum):
    # Cargo registry src/<index>/<crate> and cache/<index>/<crate>.crate.
    archive = root.parents[2] / "cache" / root.parent.name / (root.name + ".crate")
    result = dict(archive_sha256_verified=False, installed_source_matches_archive=False, archive_files=0, differences=[], installed_extra_files=[])
    if not archive.is_file():
        result["issue"] = "local-registry-archive-missing"
        return result
    result["archive_sha256"] = digest(archive)
    if not checksum or result["archive_sha256"] != checksum:
        result["issue"] = "registry-archive-does-not-match-Cargo-lock"
        return result
    result["archive_sha256_verified"] = True
    prefix, seen, total = f"{name}-{version}", set(), 0
    with tarfile.open(archive, "r:gz") as stream:
        for item in stream:
            path = PurePosixPath(item.name)
            if not path.parts or path.parts[0] != prefix or path.is_absolute() or ".." in path.parts or "\\" in item.name:
                raise ValueError("Unsafe registry archive member")
            if item.isdir():
                continue
            if not item.isfile():
                raise ValueError("Non-regular registry archive member")
            relative = PurePosixPath(*path.parts[1:]).as_posix()
            if relative in seen:
                raise ValueError("Duplicate registry archive member")
            seen.add(relative)
            total += item.size
            if total > 2 * 1024**3:
                raise ValueError("Registry archive exceeds inspection limit")
            value = hashlib.sha256()
            with stream.extractfile(item) as member:
                for block in iter(lambda: member.read(1024 * 1024), b""):
                    value.update(block)
            installed = root / relative
            if not installed.resolve().is_relative_to(root) or not installed.is_file() or digest(installed) != value.hexdigest():
                result["differences"].append(relative)
    result["archive_files"] = len(seen)
    # Cargo itself adds these two cache markers. Additional source files could
    # otherwise influence compilation even when every archived file matches.
    generated_metadata = {".cargo-ok", ".cargo-checksum.json"}
    result["installed_extra_files"] = sorted(path.relative_to(root).as_posix() for path in root.rglob("*")
                                             if (path.is_file() or path.is_symlink())
                                             and path.relative_to(root).as_posix() not in seen | generated_metadata)
    result["installed_source_matches_archive"] = not result["differences"] and not result["installed_extra_files"]
    return result


def collect(metadata, lock_path, output, root_names, *, rustc_version, sysroot):
    lock_path, output, sysroot = Path(lock_path), Path(output), Path(sysroot)
    packages, nodes, roles = dependency_roles(metadata, root_names)
    lock = tomllib.loads(lock_path.read_text())
    checksums = {(p["name"], p["version"], p.get("source")): p.get("checksum") for p in lock["package"]}
    output.mkdir(parents=True, exist_ok=False)
    components, first_party, issues = [], [], []

    def copy(component, source, relative):
        checksum = digest(source)
        filename = re.sub(r"[^a-zA-Z0-9._-]", "_", source.name)
        destination = f"{component['id']}/{checksum}-{filename}"
        target = output / destination
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(source.read_bytes())
        component["notices"].append(dict(source=relative, file=destination, sha256=checksum))

    for package_id, selected_roles in sorted(roles.items()):
        package = packages[package_id]
        if package["source"] is None:
            first_party.append(dict(name=package["name"], version=package["version"], roles=sorted(selected_roles)))
            continue
        name, version, source = package["name"], package["version"], package["source"]
        # Hash source identity too, so different registries cannot collide.
        component_id = re.sub(r"[^a-zA-Z0-9._-]", "_", f"{name}-{version}") + "-" + hashlib.sha256(source.encode()).hexdigest()[:8]
        root = Path(package["manifest_path"]).parent.resolve()
        checksum = checksums.get((name, version, source))
        component = dict(id=component_id, name=name, version=version, source=source,
                         roles=sorted(selected_roles), declared_license=package["license"],
                         declared_license_file=package["license_file"], repository=package["repository"],
                         enabled_features=nodes[package_id]["features"], locked_archive_sha256=checksum,
                         notices=[], binary_reachability_verified=False)
        if source.startswith("registry+"):
            component["provenance"] = inspect_registry_archive(root, name, version, checksum)
            if not component["provenance"]["installed_source_matches_archive"]:
                issues.append(dict(component=component_id, issue="installed-registry-source-not-verified"))
        else:
            issues.append(dict(component=component_id, issue="nonregistry-source-provenance-review-required"))
        candidates = [path for path in root.rglob("*") if path.is_file() and is_notice(path.relative_to(root))]
        if package["license_file"]:
            candidates.append(root / package["license_file"])
        for path in sorted(set(candidates)):
            if not path.resolve().is_relative_to(root) or not path.is_file():
                issues.append(dict(component=component_id, issue="unsafe-or-missing-license-file"))
                continue
            copy(component, path, path.relative_to(root).as_posix())
        if not component["notices"]:
            issues.append(dict(component=component_id, issue="publisher-notice-missing"))
        components.append(component)
    runtime = dict(id="rust-toolchain", kind="toolchain-supplied-notices", rustc_version=rustc_version,
                   exact_toolchain_archive_verified=False, notices=[])
    document_root = sysroot / "share/doc/rust"
    candidates = [document_root / "COPYRIGHT.html", document_root / "COPYRIGHT-library.html"]
    licenses = document_root / "licenses"
    if licenses.is_dir():
        candidates.extend(path for path in licenses.rglob("*") if path.is_file())
    for path in sorted(candidates):
        if path.is_file() and path.resolve().is_relative_to(sysroot.resolve()):
            copy(runtime, path, path.relative_to(sysroot).as_posix())
    if not runtime["notices"]:
        issues.append(dict(component=runtime["id"], issue="toolchain-notices-unavailable"))
    result = dict(format="cadpad-rust-notices-v1", legal_clearance=False,
                  scope="Target-filtered Cargo production dependency closures; build tools separated; binary reachability and generated-code provenance unverified",
                  cargo_lock_sha256=digest(lock_path), production_roots=root_names,
                  components=components, first_party=first_party, toolchain=runtime, review_required=issues,
                  release_gates=["Actual binary/linker and embedded-native dependency inventory", "Build-script and generated-code provenance",
                                 "Exact Rust standard-library/toolchain archive and source provenance", "Full application source/replacement compliance"])
    (output / "inventory.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    repository = Path(__file__).resolve().parents[1]
    version = subprocess.check_output(["rustc", "-vV"], text=True)
    host = next(line.split(": ", 1)[1] for line in version.splitlines() if line.startswith("host:"))
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--offline", "--format-version", "1", "--filter-platform", host], cwd=repository, text=True))
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    result = collect(metadata, repository / "Cargo.lock", args.output,
                     ["cadpad-desktop", "cadpad-mcp", "cadpad-core"], rustc_version=version, sysroot=sysroot)
    print(json.dumps(dict(components=len(result["components"]), review_items=len(result["review_required"]), output=str(args.output))))


if __name__ == "__main__":
    main()
