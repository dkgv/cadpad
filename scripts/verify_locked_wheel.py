"""Compare a worker input inventory with an exact uv.lock wheel, without extracting it.

Uses local archives only; download the named locked artifact separately. Matching
inputs prove archive provenance, not embedded-library source or license clearance.
"""
from __future__ import annotations

import argparse
import base64
import csv
from email.parser import BytesParser
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import tomllib
import zipfile


def sha256(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def normalize(name):
    return re.sub(r"[-_.]+", "-", name).lower()


def installed_path(name):
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or "\\" in name:
        raise ValueError(f"Unsafe wheel member: {name}")
    if path.parts[0].endswith(".data"):
        if len(path.parts) > 2 and path.parts[1] in {"purelib", "platlib"}:
            return "/".join(path.parts[2:])
        return None  # scripts, headers and data have installation-specific roots
    return name


def verify(wheel, inventory, lock, distribution):
    wheel, inventory, lock = Path(wheel), Path(inventory), Path(lock)
    archive_hash = sha256(wheel)
    snapshot = json.loads(inventory.read_text())
    if snapshot.get("format") != "cadpad-worker-notices-v1":
        raise ValueError("Unsupported worker inventory format")
    if snapshot["uv_lock_sha256"] != sha256(lock):
        raise ValueError("Worker inventory was collected against a different uv.lock")
    component = next(c for c in snapshot["components"] if c["id"] == normalize(distribution))
    package = next(p for p in tomllib.loads(lock.read_text())["package"]
                   if normalize(p["name"]) == component["id"] and p["version"] == component["version"])
    archives = [item for item in package.get("wheels", []) if item["hash"] == "sha256:" + archive_hash]
    if not archives:
        raise ValueError("Wheel SHA-256 does not match a locked archive for this component")
    archive_records, hashes, installed = [], {}, {}
    with zipfile.ZipFile(wheel) as archive:
        members = [info for info in archive.infolist() if not info.is_dir()]
        if len({info.filename for info in members}) != len(members):
            raise ValueError("Duplicate wheel member")
        if sum(info.file_size for info in members) > 2 * 1024**3:
            raise ValueError("Wheel exceeds 2 GiB inspection limit")
        for info in members:
            name = info.filename
            relative = installed_path(name)
            if relative is not None:
                if relative in installed:
                    raise ValueError("Multiple wheel members map to the same installed path")
                installed[relative] = name
            value = hashlib.sha256()
            with archive.open(info) as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    value.update(block)
            hashes[name] = (value.hexdigest(), info.file_size)
        metadata_names = [name for name in hashes if name.endswith(".dist-info/METADATA")]
        if len(metadata_names) != 1:
            raise ValueError("Wheel must contain one METADATA file")
        metadata_name = metadata_names[0]
        if hashes[metadata_name][1] > 4 * 1024**2:
            raise ValueError("Wheel metadata exceeds inspection limit")
        published = BytesParser().parsebytes(archive.read(metadata_name))
        if normalize(published["Name"]) != component["id"] or published["Version"] != component["version"]:
            raise ValueError("Wheel metadata differs from the selected component")
        record_name = metadata_name.rsplit("/", 1)[0] + "/RECORD"
        if record_name not in hashes or hashes[record_name][1] > 16 * 1024**2:
            raise ValueError("Wheel RECORD missing or exceeds inspection limit")
        listed = set()
        for row in csv.reader(io.StringIO(archive.read(record_name).decode())):
            if len(row) != 3 or row[0] in listed:
                raise ValueError("Malformed or duplicate RECORD row")
            name, recorded_hash, size = row
            listed.add(name)
            if name not in hashes:
                archive_records.append(dict(path=name, issue="record-member-missing"))
                continue
            actual_hash, actual_size = hashes[name]
            if recorded_hash:
                algorithm, encoded = recorded_hash.split("=", 1)
                if algorithm != "sha256":
                    archive_records.append(dict(path=name, issue="unsupported-record-hash", algorithm=algorithm))
                elif base64.urlsafe_b64decode(encoded + "===").hex() != actual_hash:
                    archive_records.append(dict(path=name, issue="archive-record-hash-mismatch", actual_sha256=actual_hash))
            elif name != record_name:
                archive_records.append(dict(path=name, issue="archive-record-hash-absent"))
            if size and int(size) != actual_size:
                archive_records.append(dict(path=name, issue="archive-record-size-mismatch"))
        for name in sorted(hashes.keys() - listed):
            # Wheel signatures are deliberately excluded from RECORD.
            if name not in {record_name + ".jws", record_name + ".p7s"}:
                archive_records.append(dict(path=name, issue="archive-member-not-in-record"))

    comparisons = []
    # Installation bookkeeping is distinct from publisher payload. Keep every
    # difference visible; never waive changed WHEEL, METADATA, code or notices.
    dist_info = metadata_name.rsplit("/", 1)[0]
    installation_metadata = {dist_info + "/" + name for name in ("RECORD", "INSTALLER", "REQUESTED", "direct_url.json")}
    for category in ("files", "notices", "provenance"):
        for item in component[category]:
            path = item.get("installed_path") if category == "files" else item["source"]
            expected_hash = item.get("source_sha256") if category == "files" else item["sha256"]
            name = installed.get(path)
            comparisons.append(dict(category=category, path=path,
                                    destination=item.get("destination", item.get("file")),
                                    archive_path=name, matches=name is not None and hashes[name][0] == expected_hash,
                                    installation_metadata=path in installation_metadata))
    matching = all(item["matches"] for item in comparisons)
    payload = [item for item in comparisons if not item["installation_metadata"]]
    if not payload:
        raise ValueError("Inventory has no selected publisher payload for this component")
    payload_matches = all(item["matches"] for item in payload)
    metadata_differences = [item for item in comparisons if item["installation_metadata"] and not item["matches"]]
    if not payload_matches:
        status = "input-mismatch"
    elif archive_records or metadata_differences:
        status = "payload-matches-with-review-items"
    else:
        status = "inputs-match"
    return dict(format="cadpad-locked-wheel-verification-v1", component=component["id"], version=component["version"],
                inventory_sha256=sha256(inventory), uv_lock_sha256=sha256(lock),
                archive_sha256=archive_hash, locked_archives=archives, archive_sha256_verified=True,
                selected_inputs_match=matching, selected_payload_matches=payload_matches, comparisons=comparisons,
                installation_metadata_differences=metadata_differences,
                archive_record_issues=archive_records, archive_files=len(hashes),
                status=status,
                legal_clearance=False, exact_embedded_native_sources_verified=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wheel", type=Path, required=True)
    parser.add_argument("--inventory", type=Path, required=True)
    parser.add_argument("--lock", type=Path, default=Path(__file__).resolve().parents[1] / "uv.lock")
    parser.add_argument("--distribution", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = verify(args.wheel, args.inventory, args.lock, args.distribution)
    # A separate receipt never edits or clears review items in the build inventory.
    with args.output.open("x") as stream:
        json.dump(result, stream, indent=2)
        stream.write("\n")
    print(json.dumps({key: result[key] for key in ("component", "status", "selected_inputs_match", "selected_payload_matches")}))
    return 0 if result["selected_payload_matches"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
