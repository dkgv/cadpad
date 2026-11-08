#!/usr/bin/env python3
"""Fail unless package versions match the release tag (vX.Y.Z)."""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPOSITORY = Path(__file__).resolve().parents[1]


def cargo_workspace_version() -> str:
    text = (REPOSITORY / "Cargo.toml").read_text()
    match = re.search(r'(?m)^version\s*=\s*"([^"]+)"\s*$', text)
    if not match:
        raise SystemExit("Cargo.toml workspace version not found")
    return match.group(1)


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: check_release_version.py vX.Y.Z")
    tag = sys.argv[1]
    if not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
        raise SystemExit(f"tag must look like v0.1.0, got {tag!r}")
    expected = tag[1:]
    versions = {
        "Cargo.toml": cargo_workspace_version(),
        "package.json": json.loads((REPOSITORY / "package.json").read_text())["version"],
        "apps/desktop/package.json": json.loads((REPOSITORY / "apps/desktop/package.json").read_text())["version"],
        "apps/desktop/src-tauri/tauri.conf.json": json.loads((REPOSITORY / "apps/desktop/src-tauri/tauri.conf.json").read_text())["version"],
        "pyproject.toml": re.search(
            r'(?m)^version\s*=\s*"([^"]+)"\s*$',
            (REPOSITORY / "pyproject.toml").read_text(),
        ).group(1),
    }
    mismatched = {path: version for path, version in versions.items() if version != expected}
    if mismatched:
        detail = ", ".join(f"{path}={version}" for path, version in mismatched.items())
        raise SystemExit(f"version mismatch for {tag}: expected {expected}; {detail}")
    print(f"release version {expected} matches package manifests")


if __name__ == "__main__":
    main()
