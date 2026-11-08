"""Verify frontend inventory bytes against package-lock tarballs, without installing.

By default this is offline. --download permits fetching missing locked HTTPS
archives into a separate cache. No archive member is extracted or executed.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import tarfile
import tempfile
import urllib.parse
import urllib.request


def file_hash(path, algorithm="sha256"):
    value = hashlib.new(algorithm)
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.digest()


def integrity_matches(path, integrity):
    candidates = []
    for token in integrity.split():
        algorithm, encoded = token.split("-", 1)
        if algorithm in {"sha256", "sha384", "sha512"}:
            candidates.append((algorithm, base64.b64decode(encoded, validate=True)))
    if not candidates:
        raise ValueError("No supported strong archive integrity hash")
    strongest = max(algorithm for algorithm, _ in candidates)
    actual = file_hash(path, strongest)
    return any(actual == expected for algorithm, expected in candidates if algorithm == strongest)


def archive_path(cache, locked, download=False):
    integrity = locked["integrity"]
    path = Path(cache) / (hashlib.sha256(integrity.encode()).hexdigest() + ".tgz")
    if not path.exists():
        if not download:
            raise FileNotFoundError(f"Locked archive missing from cache: {path}")
        url = locked["resolved"]
        if urllib.parse.urlparse(url).scheme != "https":
            raise ValueError("Only locked HTTPS archive downloads are supported")
        path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(prefix="download-", dir=path.parent, delete=False) as stream:
            temporary = Path(stream.name)
            try:
                with urllib.request.urlopen(url, timeout=60) as response:
                    if urllib.parse.urlparse(response.geturl()).scheme != "https":
                        raise ValueError("Archive download redirected outside HTTPS")
                    total = 0
                    for block in iter(lambda: response.read(1024 * 1024), b""):
                        total += len(block)
                        if total > 512 * 1024**2:
                            raise ValueError("Compressed archive exceeds 512 MiB inspection limit")
                        stream.write(block)
                stream.flush()
                if not integrity_matches(temporary, integrity):
                    raise ValueError("Downloaded archive does not match locked integrity")
                temporary.replace(path)
            finally:
                temporary.unlink(missing_ok=True)
    if not integrity_matches(path, integrity):
        raise ValueError(f"Cached archive does not match locked integrity: {path.name}")
    return path


def read_archive(path):
    hashes, texts = {}, {}
    with tarfile.open(path, "r:gz") as archive:
        total = 0
        for item in archive:
            parts = PurePosixPath(item.name).parts
            if not parts or parts[0] != "package" or ".." in parts or "\\" in item.name:
                raise ValueError(f"Unsafe npm archive member: {item.name}")
            if item.isdir():
                continue
            if not item.isfile():
                raise ValueError(f"Non-regular npm archive member: {item.name}")
            relative = PurePosixPath(*parts[1:]).as_posix()
            if relative in hashes:
                raise ValueError(f"Duplicate npm archive member: {relative}")
            total += item.size
            if total > 2 * 1024**3:
                raise ValueError("Expanded archive exceeds 2 GiB inspection limit")
            value = hashlib.sha256()
            keep = relative == "package.json" or relative.endswith((".js", ".mjs", ".cjs", ".css"))
            chunks = []
            with archive.extractfile(item) as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    value.update(block)
                    if keep and item.size <= 64 * 1024**2:
                        chunks.append(block)
            hashes[relative] = value.hexdigest()
            if keep and item.size <= 64 * 1024**2:
                texts[relative] = b"".join(chunks)
    return hashes, texts


def verify(inventory, lock_path, cache, *, download=False):
    inventory, lock_path = Path(inventory), Path(lock_path)
    snapshot = json.loads(inventory.read_text())
    lock = json.loads(lock_path.read_text())
    if snapshot.get("format") != "cadpad-frontend-notices-v1":
        raise ValueError("Unsupported frontend inventory format")
    if snapshot["package_lock_sha256"] != file_hash(lock_path).hex():
        raise ValueError("Frontend inventory was collected against a different package-lock.json")
    if not snapshot["components"]:
        raise ValueError("Frontend inventory contains no components")
    components = {component["id"]: component for component in snapshot["components"]}
    if len(components) != len(snapshot["components"]):
        raise ValueError("Duplicate component identity")
    loaded, checks = {}, []
    for component in snapshot["components"]:
        locked = lock["packages"][component["installed_root"]]
        if locked != component["locked_package"] or locked["version"] != component["version"]:
            raise ValueError("Component differs from the locked package")
        integrity = locked["integrity"]
        if integrity not in loaded:
            archive = archive_path(cache, locked, download)
            hashes, texts = read_archive(archive)
            loaded[integrity] = (archive, hashes, texts)
        archive, hashes, texts = loaded[integrity]
        identity = json.loads(texts["package.json"])
        if identity["name"] != component["name"] or identity["version"] != component["version"]:
            raise ValueError("Archive package identity differs from the selected component")
        comparisons = []

        def compare(category, name, expected):
            valid = isinstance(name, str) and isinstance(expected, str) and re.fullmatch(r"[0-9a-f]{64}", expected) is not None
            comparisons.append(dict(category=category, path=name, matches=valid and hashes.get(name) == expected,
                                    expected_sha256=expected, archive_sha256=hashes.get(name)))

        compare("package-json", "package.json", component["package_json_sha256"])
        for module in component["modules"]:
            if "virtual_id" not in module:
                compare("module", module["path"], module["source_sha256"])
        for notice in component["notices"]:
            compare("notice", notice["source"], notice["sha256"])
        for publisher in component["embedded_sources"]:
            module = publisher["module"]
            references = re.findall(rb"[#@]\s*sourceMappingURL=([^\s*]+)", texts.get(module, b""))
            map_name = None
            if references:
                reference = references[-1].decode()
                # Resolve relative paths inside the archive; never access disk.
                pieces = list(PurePosixPath(module).parent.parts)
                if not PurePosixPath(reference).is_absolute() and "\\" not in reference:
                    for part in PurePosixPath(reference).parts:
                        if part == "..":
                            if not pieces:
                                break
                            pieces.pop()
                        else:
                            pieces.append(part)
                    else:
                        map_name = "/".join(pieces)
            compare("publisher-source-map", map_name, publisher["publisher_map_sha256"])
        # Independently compare source-content claims from every wrapper map
        # referring to this installed candidate against its locked tarball.
        for wrapper in snapshot["components"]:
            for publisher in wrapper["embedded_sources"]:
                for dependency in publisher["dependencies"]:
                    if dependency.get("candidate_component") != component["id"]:
                        continue
                    marker = "node_modules/" + component["name"] + "/"
                    for source in dependency["sources"]:
                        name = source["publisher_map_source"].rsplit(marker, 1)[-1]
                        compare("embedded-source", name, source["source_content_sha256"])
        checks.append(dict(component=component["id"], name=component["name"], version=component["version"],
                           archive_cache_path=str(archive), archive_sha256=file_hash(archive).hex(),
                           locked_integrity=integrity, locked_url=locked["resolved"], archive_integrity_verified=True,
                           comparisons=comparisons, payload_matches=all(item["matches"] for item in comparisons)))
    matching = all(component["payload_matches"] for component in checks)
    return dict(format="cadpad-frontend-archive-verification-v1", status="payload-matches" if matching else "input-mismatch",
                scope="Selected installed package files, supplied notices and publisher map candidates; generated virtual modules require separate build provenance",
                inventory_sha256=file_hash(inventory).hex(), package_lock_sha256=file_hash(lock_path).hex(),
                unique_archives=len(loaded), components=checks, selected_payload_matches=matching,
                unverified_generated_virtual_modules=snapshot["virtual_modules"],
                legal_clearance=False, publisher_prebundle_reproduction_verified=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inventory", type=Path, required=True)
    parser.add_argument("--lock", type=Path, default=Path(__file__).resolve().parents[1] / "package-lock.json")
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--download", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = verify(args.inventory, args.lock, args.cache, download=args.download)
    with args.output.open("x") as stream:
        json.dump(result, stream, indent=2)
        stream.write("\n")
    print(json.dumps({key: result[key] for key in ("status", "unique_archives", "selected_payload_matches")}))
    return 0 if result["selected_payload_matches"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
