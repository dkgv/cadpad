Pure protocol/control contracts live in `tests/unit`, using published primitive
metadata and an intentionally missing worker executable. They exercise the real
Core handlers without making a CAD build a prerequisite. Native integration
coverage remains in `motion`, `dynamics_integration`, `geometry_contracts`, the
agent loop, and the dedicated worker suites.

`published_fixture::open` supplies an independent app root containing a committed
model produced by the real worker. The first consumer builds normally and
publishes a snapshot; later consumers copy that snapshot. Copies never share
files, process locks, admission leases, run directories, or live workers.

Use this for tests of metadata, project persistence, playback and control logic
whose initial CAD build is only setup. Keep explicit builds for construction,
sandboxing, warm-cache behavior, recovery, and native lifecycle tests. Exact
queries on a copied fixture still recover through the real worker.

Seeds are scoped to `NEXTEST_RUN_ID` (or the current Cargo test process), and
keyed by source, mechanism and worker configuration. They are never reused by
another run. Packaged-runtime QA always builds fresh. Set
`CADPAD_TEST_FRESH_FIXTURES=1` to compare the same tests with fresh setup, and
`CADPAD_TEST_FIXTURE_TRACE=1` plus nextest's `--success-output immediate` to see
build/reuse counts.
