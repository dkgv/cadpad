# CadQuery headless build recipe

This directory contains a build recipe and a small patch, not a copy of CadQuery.
`uv sync --locked` fetches the upstream 2.6.1 wheel specified in UPSTREAM.json,
verifies its SHA-256, applies no-vtk.patch, and builds 2.6.1+cadpad.1. Builds need
network access and Git; uv caches the resulting wheel. The backend fails if the
checksum or patch does not match. LICENSE is upstream's Apache-2.0 license.

The patch defers annotations and moves VTK imports inside the VTK-specific
functions in four files. Geometry and STEP/STL implementations are unchanged.
Metadata selects upstream cadquery-ocp-novtk 7.8.1.1.post1 and omits optional
Trame dependencies. Cadpad's STEP/STL exports remain supported. CadQuery's VTK
viewers and VTP/VTK scene exports are not supplied by this runtime.

## Updating upstream

1. Choose an upstream release and update the URL, version, and SHA-256 in
   UPSTREAM.json from its published PyPI metadata.
2. Update this project's version and dependency constraints, and the CadQuery
   pin in the root pyproject.toml.
3. Run `uv lock` and `uv sync --reinstall-package cadquery`. If the patch fails,
   review upstream changes and regenerate the patch against that exact wheel;
   do not bypass the checksum or silently skip failed hunks.
4. Run the Python suite, build the packaged worker, and run
   `scripts/check_bundle.py` against it to validate STEP/STL and geometry.
5. Review the resulting native inventory for VTK and compare bundle size.

Once upstream supports optional VTK, remove this recipe and use its standard
release. Until then the patch still requires review on upgrades.
