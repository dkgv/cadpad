"""Requirements evaluator: raw measurements, caches, broad phase, pool, limits."""
import json
import math

import cadquery as cq
import pytest

from cadpad_worker.model import DomainError, Model, Part
from cadpad_worker.requirements_eval import evaluate_plan_chunk


def box(x, y, z, at=(0, 0, 0)):
    return cq.Workplane().box(x, y, z).val().translate(at)


def model_with(tmp_path, shapes):
    model = Model(tmp_path / "cache")
    model.revision = "r1"
    model.parts = {name: Part(name, shape, (0, 0, 0), {}, None) for name, shape in shapes.items()}
    return model


def evaluate(model, states, **limits):
    limits.setdefault("workers", 2)
    return evaluate_plan_chunk(model, dict(states=states, limits=limits))


def measurement(result, state=0, index=0):
    return result["results"]["states"][state]["measurements"][index]


def interference(a="a", b="b", id="m"):
    return dict(id=id, primitive="interference", a=[a], b=[b])


def canonical(result):
    return json.dumps(result["results"], sort_keys=True, allow_nan=False)


@pytest.mark.parametrize("offset,expected", [(10, 0.0), (5, 500.0), (10 - 1e-6, 100 * 1e-6), (30, 0.0)])
def test_known_overlaps_touching_and_micro_overlap(tmp_path, offset, expected):
    model = model_with(tmp_path, dict(a=box(10, 10, 10), b=box(10, 10, 10, (offset, 0, 0))))
    m = measurement(evaluate(model, [dict(id="s", measurements=[interference()])]))
    assert m["status"] == "ok"
    assert m["value"] == pytest.approx(expected, abs=1e-8)
    assert (m["witness"] is not None) == (expected > 0)


def test_other_primitives_on_known_geometry(tmp_path):
    cyl = cq.Workplane().circle(2).extrude(10).val().translate((20, 0, 0))
    model = model_with(tmp_path, dict(a=box(10, 10, 10), b=box(10, 10, 10, (15, 0, 0)), c=cyl,
                                      env=box(20, 20, 20, (5, 0, 0))))
    r = evaluate(model, [dict(id="s", measurements=[
        dict(id="d", primitive="min_clearance", a=["a"], b=["b"]),
        dict(id="e", primitive="extent", a=["a", "b"], axis="x"),
        dict(id="o", primitive="outside_volume", a=["a"], env=["env"]),
        dict(id="o2", primitive="outside_volume", a=["c"], env=["a"]),
        dict(id="v", primitive="valid_solid_count", a=["a", "b", "c"]),
    ])])
    d, e, o, o2, v = r["results"]["states"][0]["measurements"]
    assert d["value"] == pytest.approx(5.0) and d["witness"] == ["a", "b"]
    assert (e["min"], e["max"], e["value"]) == pytest.approx((-5.0, 20.0, 25.0))
    assert o["value"] == pytest.approx(0.0, abs=1e-6)       # a lies inside env
    assert o2["value"] == pytest.approx(math.pi * 4 * 10)   # cylinder far from a: pruned, full volume
    assert o2["pairs_pruned"] == 1
    assert (v["value"], v["solids"]) == (3, 3)


def test_set_operands_sum_member_pairs(tmp_path):
    model = model_with(tmp_path, dict(a=box(10, 10, 10), b=box(10, 10, 10, (5, 0, 0)),
                                      c=box(10, 10, 10, (-5, 0, 0)), far=box(1, 1, 1, (100, 0, 0))))
    m = measurement(evaluate(model, [dict(id="s", measurements=[
        dict(id="m", primitive="interference", a=["a"], b=["b", "c", "far"])])]))
    assert m["value"] == pytest.approx(1000.0)
    assert (m["pairs_total"], m["pairs_pruned"]) == (3, 1)
    assert m["witness"] == ["a", "b"]  # equal volumes: lexicographic tiebreak


def slow_grid():
    # 256 disjoint cylinders: ~0.8 s for a boolean against a shifted copy, built in milliseconds.
    cylinder = cq.Workplane().circle(1).extrude(5).val()
    return cq.Compound.makeCompound([cylinder.translate((3 * i, 3 * j, 0)) for i in range(16) for j in range(16)])


def rotation_sweep(angles):
    return [dict(id=f"r{a}", moves=[dict(op="rotate", parts=["arm"], axis="z", angle_deg=a)],
                 measurements=[interference("arm", "post")]) for a in angles]


def sweep_model(tmp_path):
    # Thin arm pivoting about the origin; a post at +x only gets hit near 0 degrees.
    arm = box(40, 4, 4, (20, 0, 0))
    post = cq.Workplane().circle(3).extrude(10).val().translate((30, 0, -5))
    return model_with(tmp_path, dict(arm=arm, post=post))


def test_rigid_rotation_sweep_finds_known_clash_angle(tmp_path):
    angles = [0, 5, 10, 15, 30, 90, 180, 355]
    r = evaluate(sweep_model(tmp_path), rotation_sweep(angles))
    clash = {a: s["measurements"][0]["value"] for a, s in zip(angles, r["results"]["states"])}
    assert clash[0] > 0 and clash[355] > 0 and clash[5] > 0
    assert clash[15] == 0 and clash[30] == 0 and clash[90] == 0 and clash[180] == 0
    # Far angles never reached a boolean: the broad phase pruned them.
    assert r["results"]["states"][5]["measurements"][0]["pairs_pruned"] == 1
    assert r["cache"]["result_misses"] < len(angles)


def test_translate_move_and_unknown_part(tmp_path):
    model = model_with(tmp_path, dict(a=box(10, 10, 10), b=box(10, 10, 10, (30, 0, 0))))
    r = evaluate(model, [
        dict(id="t", moves=[dict(op="translate", parts=["b"], vector=[-25, 0, 0])], measurements=[interference()]),
        dict(id="bad", measurements=[interference("a", "zz")]),
    ])
    assert measurement(r)["value"] == pytest.approx(500.0)
    bad = measurement(r, 1)
    assert bad["status"] == "error" and bad["value"] is None and "zz" in bad["reason"]


def test_result_cache_hits_in_memory_and_across_processes(tmp_path):
    shapes = dict(a=box(10, 10, 10), b=box(10, 10, 10, (5, 0, 0)))
    states = [dict(id="s", measurements=[interference()])]
    first = evaluate(model_with(tmp_path, shapes), states)
    assert first["cache"]["result_hits"] == 0 and first["timings"]["jobs"]
    # A different Model object (a new "process") over the same cache directory.
    second = evaluate(model_with(tmp_path, shapes), states)
    assert second["cache"]["result_hits"] == 1 and second["cache"]["result_misses"] == 0
    assert second["timings"]["jobs"] == []
    assert second["results_sha256"] == first["results_sha256"]
    # The reversed pair is the same cache entry.
    reverse = evaluate(model_with(tmp_path, shapes), [dict(id="s", measurements=[interference("b", "a")])])
    assert reverse["cache"]["result_hits"] == 1
    # Cache can be bypassed.
    fresh = evaluate_plan_chunk(model_with(tmp_path, shapes), dict(states=states, cache=dict(results=False)))
    assert fresh["cache"]["result_hits"] == 0


def test_identical_jobs_in_one_chunk_run_once(tmp_path):
    model = model_with(tmp_path, dict(a=box(10, 10, 10), b=box(10, 10, 10, (5, 0, 0))))
    r = evaluate(model, [dict(id=f"s{i}", measurements=[interference(), interference(id="n")]) for i in range(3)])
    assert len(r["timings"]["jobs"]) == 1 and r["cache"]["dedup"] == 5


def test_broad_phase_prunes_without_boolean(tmp_path):
    model = model_with(tmp_path, dict(a=box(10, 10, 10), b=box(10, 10, 10, (50, 0, 0))))
    r = evaluate(model, [dict(id="s", measurements=[interference()])])
    m = measurement(r)
    assert (m["value"], m["pairs_pruned"], m["pairs_total"], m["items"]) == (0.0, 1, 1, [])
    assert r["timings"]["jobs"] == [] and r["cache"]["result_misses"] == 0


def test_pair_over_time_limit_is_inconclusive_never_a_value(tmp_path):
    holes = slow_grid()
    model = model_with(tmp_path, dict(a=holes, b=holes.translate((0.5, 0.5, 0)), c=box(1, 1, 1, (200, 0, 0))))
    r = evaluate(model, [dict(id="s", measurements=[interference("a", "b"),
                                                    dict(id="ok", primitive="extent", a=["c"], axis="x")])],
                 pair_time_limit_s=0.005)
    m, e = r["results"]["states"][0]["measurements"]
    assert m["status"] == "inconclusive" and m["value"] is None and m["reason"] == "pair_time_limit"
    assert m["items"][0]["status"] == "inconclusive"
    assert e["status"] == "ok"
    assert r["timings"]["jobs"][0]["status"] == "timeout"
    # Inconclusive outcomes are not cached: a later run with room computes the value.
    r2 = evaluate(model, [dict(id="s", measurements=[interference("a", "b")])], pair_time_limit_s=60)
    assert measurement(r2)["status"] == "ok" and measurement(r2)["value"] > 0


def test_chunk_time_limit_cancels_without_partial_results(tmp_path):
    holes = slow_grid()
    model = model_with(tmp_path, dict(a=holes, b=holes.translate((0.5, 0.5, 0))))
    r = evaluate(model, [dict(id="s", measurements=[interference("a", "b")])], chunk_time_limit_s=0.05,
                 pair_time_limit_s=60)
    assert r["status"] == "cancelled" and r["results"] is None and r["results_sha256"] is None


@pytest.mark.parametrize("workers", [0, 1, 3])
def test_serial_and_pool_results_are_byte_identical(tmp_path, workers):
    states = rotation_sweep([0, 5, 10, 355]) + [dict(id="rest", measurements=[
        interference("arm", "post"), dict(id="d", primitive="min_clearance", a=["arm"], b=["post"]),
        dict(id="x", primitive="extent", a=["arm", "post"], axis="y"),
        dict(id="o", primitive="outside_volume", a=["arm"], env=["post"]),
        dict(id="v", primitive="valid_solid_count", a=["arm", "post"])])]
    reference = evaluate_plan_chunk(sweep_model(tmp_path / "ref"), dict(states=states, limits=dict(workers=0),
                                                                        cache=dict(results=False)))
    other = evaluate_plan_chunk(sweep_model(tmp_path / f"w{workers}"),
                                dict(states=states, limits=dict(workers=workers), cache=dict(results=False)))
    assert canonical(other) == canonical(reference)
    assert other["results_sha256"] == reference["results_sha256"]
    # Warm cache gives the same bytes too.
    warm_model = sweep_model(tmp_path / "warm")
    evaluate_plan_chunk(warm_model, dict(states=states, limits=dict(workers=workers)))
    warm = evaluate_plan_chunk(warm_model, dict(states=states, limits=dict(workers=workers)))
    assert warm["cache"]["result_hits"] > 0 and canonical(warm) == canonical(reference)


def test_request_validation(tmp_path):
    model = model_with(tmp_path, dict(a=box(1, 1, 1), b=box(1, 1, 1)))
    for states in ([], [dict(id="s", measurements=[dict(id="m", primitive="nope", a=["a"])])],
                   [dict(id="s", moves=[dict(op="rotate", parts=["a"], axis=[0, 0, 0], angle_deg=1)])],
                   [dict(id="s"), dict(id="s")]):
        with pytest.raises(DomainError) as error:
            evaluate(model, states)
        assert error.value.code == "INVALID_REQUEST"


def build_project(tmp_path):
    source = tmp_path / "project"
    source.mkdir(parents=True)
    (source / "model.py").write_text(
        "import cadquery as cq\n"
        "ext = parameter('ext', 10.0, minimum=0, maximum=40)\n"
        "show(cq.Workplane().box(10, 10, 10), name='base')\n"
        "show(cq.Workplane().box(10, 10, 10).translate((5 + ext, 0, 0)), name='slider')\n")
    model = Model(tmp_path / "cache")
    model.build(dict(source_root=str(source), input_digest="a" * 64, generate_mesh=False), "r1")
    return source, model


def param_state(ext, id=None):
    return dict(id=id or f"e{ext}", params=dict(ext=ext), measurements=[
        interference("base", "slider"), dict(id="x", primitive="extent", a=["slider"], axis="x")])


def test_param_sweep_rebuilds_once_per_value_and_caches_rebuilds(tmp_path):
    _, model = build_project(tmp_path)
    # Slider spans [ext, ext+10]; base spans [-5, 5]. Rest build is ext=10.
    states = [param_state(v, f"s{i}") for i, v in enumerate((0, 10, 5, 20, 0))]
    r = evaluate(model, states)
    got = [(s["measurements"][0]["value"], s["measurements"][1]["max"]) for s in r["results"]["states"]]
    assert [g[0] for g in got] == pytest.approx([500.0, 0.0, 0.0, 0.0, 500.0])
    assert [g[1] for g in got] == pytest.approx([10.0, 20.0, 15.0, 30.0, 10.0])
    assert r["cache"]["rebuilds"] == 3  # 0, 5 and 20 once each; 10 is the rest build
    again = evaluate(model, states)
    assert again["cache"]["rebuilds"] == 0 and again["cache"]["rebuild_memory_hits"] == 3
    assert again["results_sha256"] == r["results_sha256"]
    # A new Model over the same cache and input digest reuses the rebuilt geometry from disk.
    _, other = build_project(tmp_path / "second")
    other.cache = model.cache
    disk = evaluate(other, states)
    assert disk["cache"]["rebuilds"] == 0 and disk["cache"]["rebuild_disk_hits"] == 3
    assert disk["results_sha256"] == r["results_sha256"]
    # Disabling the rebuild cache rebuilds again, with identical numbers.
    cold = evaluate_plan_chunk(other, dict(states=states, limits=dict(workers=2), cache=dict(rebuild=False)))
    assert cold["cache"]["rebuilds"] == 3 and cold["results_sha256"] == r["results_sha256"]


def test_param_state_errors_are_per_state(tmp_path):
    _, model = build_project(tmp_path)
    r = evaluate(model, [param_state(0, "clash"),
                         dict(id="bad_name", params=dict(nope=1), measurements=[interference("base", "slider")]),
                         dict(id="out_of_range", params=dict(ext=99), measurements=[interference("base", "slider")])])
    clash, bad, oob = r["results"]["states"]
    assert clash["status"] == "ok" and clash["measurements"][0]["value"] == pytest.approx(500.0)
    assert bad["status"] == "error" and bad["error"]["code"] == "INVALID_PARAMETER"
    assert oob["status"] == "error" and oob["measurements"][0]["status"] == "error"
    assert r["status"] == "complete"


def test_param_and_rigid_states_compose(tmp_path):
    _, model = build_project(tmp_path)
    state = param_state(20, "both")
    state["moves"] = [dict(op="translate", parts=["slider"], vector=[-20, 0, 0])]
    r = evaluate(model, [state])
    assert measurement(r)["value"] == pytest.approx(500.0)


def test_protocol_round_trip_after_build(tmp_path):
    from test_protocol import request, worker
    source = tmp_path / "source"
    source.mkdir()
    (source / "model.py").write_text(
        "import cadquery as cq\n"
        "ext = parameter('ext', 10.0, minimum=0, maximum=40)\n"
        "show(cq.Workplane().box(10, 10, 10), name='base')\n"
        "show(cq.Workplane().box(10, 10, 10).translate((5 + ext, 0, 0)), name='slider')\n")
    plan = dict(chunk_id="c1", limits=dict(workers=2), states=[
        param_state(0, "clash"), dict(id="rest", measurements=[interference("base", "slider")])])
    responses = worker(tmp_path, [
        request("build", dict(source_root=str(source), generate_mesh=False, parameters=dict(ext=10))),
        request("evaluate_plan_chunk", plan),
        request("evaluate_plan_chunk", plan, revision="other"),
    ])
    result = responses[1]["result"]
    assert result["chunk_id"] == "c1" and result["status"] == "complete"
    assert result["results"]["states"][0]["measurements"][0]["value"] == pytest.approx(500.0)
    assert result["results"]["states"][1]["measurements"][0]["value"] == 0.0
    assert responses[2]["error"]["data"]["code"] == "STALE_ENTITY"
