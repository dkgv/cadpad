"""Collect supplied notices for the distributions actually selected by PyInstaller.

Run with .venv/bin/python. This is an inventory, not a license-clearance tool:
wheel ownership does not establish the provenance of embedded native libraries.
"""
from __future__ import annotations

import argparse
import ast
import base64
import hashlib
import importlib.metadata as metadata
import json
from pathlib import Path
import re
import sys
import tomllib


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def normalized(name):
    result = re.sub(r"[-_.]+", "-", name).lower()
    if not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", result):
        raise ValueError(f"Unsafe distribution name: {name!r}")
    return result


def is_notice(path):
    # Include vendor trees such as casadi/include/licenses, but not source code
    # merely mentioning a license in its header.
    parts = Path(str(path)).parts
    return bool(re.match(r"^(licen[cs]e|copying|copyright|notice)([._-].*|$)", parts[-1], re.I)) or any(
        part.lower() in {"licenses", "licences"} for part in parts[:-1]
    )


def collect(entries, output, repository, *, distributions=None, base_prefix=None, python_version=None):
    """Write a new directory; never merge potentially stale notices into a build."""
    output, repository = Path(output), Path(repository).resolve()
    output.mkdir(parents=True, exist_ok=False)
    base_prefix = Path(base_prefix or sys.base_prefix).resolve()
    runtime_root = base_prefix / "lib" / f"python{sys.version_info.major}.{sys.version_info.minor}"
    distributions = list(metadata.distributions() if distributions is None else distributions)
    owners, records, components, issues = {}, {}, {}, []
    lock = tomllib.loads((repository / "uv.lock").read_text())
    locked = {(normalized(p["name"]), p["version"]): p for p in lock["package"]}

    for dist in distributions:
        key = normalized(dist.metadata["Name"])
        if key in records:
            raise ValueError(f"Duplicate installed distribution: {key}")
        files = list(dist.files or [])
        root = Path(dist.locate_file("")).resolve()
        records[key] = (dist, files, root)
        for item in files:
            source = Path(dist.locate_file(item)).resolve()
            owners.setdefault(source, []).append((key, item))

    def copy_material(component, source, label, category):
        data_hash = digest(source)
        filename = re.sub(r"[^a-zA-Z0-9._-]", "_", Path(label).name)
        relative = Path(component["id"]) / category / f"{data_hash}-{filename}"
        target = output / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(source.read_bytes())
        return {"source": label, "file": relative.as_posix(), "sha256": data_hash}

    def distribution_component(key):
        if key in components:
            return components[key]
        dist, files, root = records[key]
        version = dist.version
        # Wheel metadata is reported verbatim; do not promote it to an SPDX
        # conclusion or apply it to every library vendored inside that wheel.
        component = dict(id=key, name=dist.metadata["Name"], version=version,
                         kind="python-distribution", files=[], notices=[], provenance=[],
                         declared_license=dist.metadata.get("License"),
                         declared_license_expression=dist.metadata.get("License-Expression"),
                         project_urls=dist.metadata.get_all("Project-URL", []),
                         home_page=dist.metadata.get("Home-page"))
        components[key] = component
        package = locked.get((key, version))
        if package:
            component["locked_archives"] = {field: package[field] for field in ("sdist", "wheels", "source") if field in package}
            component["archive_selection_verified"] = False
        else:
            issues.append(dict(component=key, issue="installed-version-not-in-uv-lock"))
        declared = set(dist.metadata.get_all("License-File", []))
        for item in files:
            label = str(item)
            source = Path(dist.locate_file(item)).resolve()
            is_metadata = any(part.endswith(".dist-info") for part in item.parts[:-1])
            selected_notice = is_notice(item) or (is_metadata and any(label.endswith("/" + name) for name in declared))
            selected_provenance = is_metadata and item.name in {"METADATA", "RECORD", "WHEEL"}
            if not (selected_notice or selected_provenance):
                continue
            if not source.is_relative_to(root) or not source.is_file():
                issues.append(dict(component=key, issue="unsafe-or-missing-publisher-material", source=label))
                continue
            category = "notices" if selected_notice else "provenance"
            component[category].append(copy_material(component, source, label, category))
        if not component["notices"]:
            issues.append(dict(component=key, issue="no-publisher-notice-found"))
        return component

    def runtime_component():
        if "cpython" not in components:
            component = dict(id="cpython", kind="python-runtime", version=python_version or sys.version,
                             files=[], notices=[], provenance=[], exact_runtime_archive_verified=False)
            components["cpython"] = component
            license_file = runtime_root / "LICENSE.txt"
            if license_file.is_file():
                component["notices"].append(copy_material(component, license_file, "lib/python/LICENSE.txt", "notices"))
            else:
                issues.append(dict(component="cpython", issue="runtime-license-missing"))
            issues.append(dict(component="cpython", issue="runtime-build-and-embedded-native-source-review-required"))
        return components["cpython"]

    unknown, first_party = [], []
    native_components = set()
    # The bootloader ships even when no PyInstaller module appears in a.pure.
    if "pyinstaller" not in records:
        raise ValueError("PyInstaller distribution metadata is required")
    distribution_component("pyinstaller")
    for destination, original, kind in sorted(set(tuple(e) for e in entries)):
        source = Path(original).resolve()
        if kind == "SYMLINK":
            continue  # original is a link target, not an installed source file
        if not source.is_file():
            unknown.append(dict(destination=destination, kind=kind, issue="source-file-missing"))
            continue
        item = dict(destination=destination, kind=kind, source_sha256=digest(source))
        matches = owners.get(source, [])
        if len(matches) == 1:
            key, record = matches[0]
            # The editable first-party distribution has no third-party notices.
            if key == "cadpad-worker":
                first_party.append(item)
                continue
            component = distribution_component(key)
            item["installed_path"] = str(record)
            recorded_hash = getattr(record, "hash", None)
            if recorded_hash and recorded_hash.mode == "sha256":
                expected = base64.urlsafe_b64decode(recorded_hash.value + "===").hex()
                item["record_sha256_matches"] = expected == item["source_sha256"]
                if not item["record_sha256_matches"]:
                    issues.append(dict(component=key, issue="installed-record-hash-mismatch", source=str(record)))
            if kind in {"BINARY", "EXTENSION"}:
                native_components.add(key)
        elif len(matches) > 1:
            unknown.append(dict(**item, issue="multiple-distribution-owners", owners=sorted(key for key, _ in matches)))
            continue
        elif source.is_relative_to(repository / "python") or source.is_relative_to(repository / "scripts"):
            first_party.append(item)
            continue
        elif (source.is_relative_to(runtime_root) and not source.is_relative_to(runtime_root / "site-packages")) or source == base_prefix / "lib/libpython3.12.dylib":
            component = runtime_component()
            item["installed_path"] = source.relative_to(base_prefix).as_posix()
        else:
            unknown.append(dict(**item, issue="unowned-source-file"))
            continue
        component["files"].append(item)

    for key in sorted(native_components):
        issues.append(dict(component=key, issue="vendored-native-libraries-source-and-license-review-required"))
    if unknown:
        issues.append(dict(issue="unowned-pyinstaller-inputs", count=len(unknown)))
    result = dict(format="cadpad-worker-notices-v1", legal_clearance=False,
                  scope="PyInstaller input ownership and publisher-supplied materials; not final transformed binary provenance",
                  uv_lock_sha256=digest(repository / "uv.lock"),
                  components=sorted(components.values(), key=lambda component: component["id"]),
                  first_party_files=first_party, unowned_inputs=unknown, review_required=issues,
                  release_gates=["Exact installed archive provenance", "Embedded native component inventory and source availability",
                                 "Modified-library execution and replacement", "Frontend and Rust notices are collected separately"])
    (output / "inventory.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


def analysis_entries(path):
    analysis = ast.literal_eval(Path(path).read_text())
    # Pinned PyInstaller 6.16 Analysis TOC. Reject other schemas, rather than
    # accidentally interpreting a different tuple index as shipped inputs.
    if len(analysis) != 20 or not isinstance(analysis[12], str):
        raise ValueError("Unsupported PyInstaller Analysis TOC schema")
    entries = []
    for index in (13, 14, 15, 18, 19):
        for entry in analysis[index]:
            if len(entry) != 3 or not all(isinstance(value, str) for value in entry):
                raise ValueError("Unsupported PyInstaller Analysis entry")
            entries.append(entry)
    return entries, analysis[12]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--analysis", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    entries, version = analysis_entries(args.analysis)
    if version != sys.version:
        raise ValueError("Analysis Python version differs from the collecting interpreter")
    result = collect(entries, args.output, Path(__file__).resolve().parents[1], python_version=version)
    print(json.dumps(dict(components=len(result["components"]), unowned_inputs=len(result["unowned_inputs"]),
                          review_items=len(result["review_required"]), output=str(args.output))))


if __name__ == "__main__":
    main()
