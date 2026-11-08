"""Compare exact interference queries with the original exhaustive implementation.

Run: PYTHONPATH=python .venv/bin/python scripts/benchmark_interference.py --rounds 7
Reports kernel/query time only, excluding model construction and worker IPC.
"""
import argparse
import itertools
import json
import platform
import statistics
import tempfile
import time
from pathlib import Path

import cadquery as cq

from cadpad_worker.model import Model, Part


def summarize(samples):
    return dict(median_ms=statistics.median(samples),
                p95_ms=sorted(samples)[min(len(samples) - 1, int(len(samples) * .95))])


def benchmark(root, kind, rounds):
    model = Model(root / kind)
    model.revision = 'benchmark'
    if kind == 'sparse_boxes':
        shape = cq.Workplane().box(10, 10, 10).val()
        spacing = 30
    elif kind == 'sparse_compounds':
        box = cq.Workplane().box(10, 10, 10).val()
        shape = cq.Compound.makeCompound([box, box.translate((0, 15, 0))])
        spacing = 30
    else:
        shape = cq.Workplane().box(20, 20, 5).faces('>Z').workplane().hole(6).val()
        spacing = 1
    model.parts = {f'part{i:02}': Part(f'part{i:02}', shape, (i * spacing, 0, 0), {}, None)
                   for i in range(16)}
    pairs = list(itertools.combinations(model.parts, 2))[:64]
    args = dict(query='interference', pairs=pairs)
    def original():
        results = []
        for first, second in pairs:
            a = model.parts[first]
            b = model.parts[second]
            a = a.shape.moved(model.location(dict(position=a.position)))
            b = b.shape.moved(model.location(dict(position=b.position)))
            assert a.Solids() and b.Solids()
            overlap = a.intersect(b)
            assert overlap.isValid()
            volume = overlap.Volume() if overlap.Solids() else 0.
            results.append((a.distance(b), volume, bool(overlap.Solids())))
        return results
    def cold():
        model._interference_results.clear()
        return model.inspect(args)
    expected = original()
    actual = cold()['pairs']
    for old, new in zip(expected, actual):
        assert abs(old[0] - new['distance_mm']) < 1e-8
        assert abs(old[1] - new['intersection_volume_mm3']) < 1e-6
        assert old[2] == new['interfering']
    # Warm all paths before measuring; cold() clears only measurement results.
    original()
    samples = dict(original=[], cold=[], cached=[])
    # Interleave baseline/optimized samples and reverse their order each round
    # to reduce thermal and load drift. A cold run also primes the cached run.
    for round_index in range(rounds):
        operations = [('original', original), ('cold', cold)]
        if round_index % 2:
            operations.reverse()
        operations.append(('cached', lambda: model.inspect(args)))
        for name, operation in operations:
            started = time.perf_counter()
            operation()
            samples[name].append((time.perf_counter() - started) * 1000)
    baseline = summarize(samples['original'])
    optimized = summarize(samples['cold'])
    warm = summarize(samples['cached'])
    return dict(scene=kind, components=16, pairs=len(pairs), original=baseline,
                optimized_cold=optimized, optimized_cached=warm,
                cold_speedup=baseline['median_ms'] / optimized['median_ms'],
                cached_speedup=baseline['median_ms'] / warm['median_ms'])


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rounds', type=int, default=7)
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error('--rounds must be positive')
    with tempfile.TemporaryDirectory() as directory:
        print(json.dumps(dict(platform=platform.platform(), rounds=args.rounds)), flush=True)
        for kind in ['sparse_boxes', 'sparse_compounds', 'overlapping_plates']:
            print(json.dumps(benchmark(Path(directory), kind, args.rounds)), flush=True)
