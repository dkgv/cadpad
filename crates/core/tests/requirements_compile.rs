//! The compiler end to end on representative requirements, including the
//! newer press_fit and seated rules, against fixture build snapshots.
use cadpad_core::requirements::{
    ChunkOptions, Compiled, DiagnosticCode, Dimension, Pairing, ProjectSettings, Requirement,
    Source, Unit, WorkspaceSnapshot, build_requests, compile, evaluate, library, run_plan,
};
use serde_json::{Value, json};

macro_rules! case {
    ($name:literal) => {
        case!($name, $name)
    };
    ($name:literal, $snapshot:literal) => {
        (
            $name,
            include_str!(concat!("fixtures/requirements/", $name, ".json")),
            include_str!(concat!(
                "fixtures/requirements/snapshots/",
                $snapshot,
                ".json"
            )),
            include_str!(concat!("fixtures/requirements/print/", $name, ".txt")),
        )
    };
}

const CASES: [(&str, &str, &str, &str); 9] = [
    case!("R1"),
    case!("R2"),
    case!("R3"),
    case!("R4"),
    case!("E2"),
    case!("E3"),
    case!("E5"),
    case!("PF1", "fit-and-seating"),
    case!("SE1", "fit-and-seating"),
];

fn run(requirement: &str, snapshot: &str) -> Compiled {
    let req: Requirement = serde_json::from_str(requirement).unwrap();
    let snap: WorkspaceSnapshot = serde_json::from_str(snapshot).unwrap();
    compile(&req, &snap, &ProjectSettings::default())
}

fn case(name: &str) -> (&'static str, &'static str) {
    let c = CASES.iter().find(|c| c.0 == name).unwrap();
    (c.1, c.2)
}

#[test]
fn reference_cases_compile_without_diagnostics_or_downgrades() {
    for (name, req, snap, _) in CASES {
        let c = run(req, snap);
        assert!(c.diagnostics.is_empty(), "{name}: {:?}", c.diagnostics);
        assert!(
            c.plan.is_some() && c.manifest.is_some() && c.print.is_some() && c.hashes.is_some(),
            "{name}"
        );
        assert!(c.downgrades.is_empty(), "{name}: {:?}", c.downgrades);
    }
}

#[test]
fn canonical_print_matches_golden() {
    for (name, req, snap, golden) in CASES {
        let printed = run(req, snap).print.unwrap().render();
        assert_eq!(
            printed, golden,
            "{name}: print changed; if intended, regenerate with \
             cargo test -p cadpad-core --test requirements_compile -- --ignored regenerate_golden"
        );
    }
}

#[test]
#[ignore = "writes golden prints"]
fn regenerate_golden() {
    for (name, req, snap, _) in CASES {
        let path = format!(
            "{}/tests/fixtures/requirements/print/{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        );
        let compiled = run(req, snap);
        let print = compiled
            .print
            .unwrap_or_else(|| panic!("{name}: {:?}", compiled.diagnostics));
        std::fs::write(path, print.render()).unwrap();
    }
}

#[test]
fn plans_have_the_expected_size_and_canonical_order() {
    let jobs = |name: &str| -> Vec<usize> {
        let (r, s) = case(name);
        run(r, s)
            .plan
            .unwrap()
            .claims
            .iter()
            .map(|c| c.jobs.len())
            .collect()
    };
    assert_eq!(jobs("R1"), [5 * 73]); // 5 turntable parts x base, 73 angles
    assert_eq!(jobs("R2"), [3 * 3 * 129]); // 3 clamps x 3 others, 43 x 3 states
    assert_eq!(jobs("R4"), [2 * 2 * 11, 2]);
    assert_eq!(jobs("E3"), [56]);
    assert_eq!(jobs("E5"), [1, 1, 1, 1]);
    // E2: no_overlap pair, clearance against the four-face wall selection
    // (one argument, the worker reduces it), aligned centre + size.
    assert_eq!(jobs("E2"), [1, 1, 1, 1]);
    for (name, req, snap, _) in CASES {
        let plan = run(req, snap).plan.unwrap();
        for claim in &plan.claims {
            let keys: Vec<_> = claim
                .jobs
                .iter()
                .map(|j| (j.state, j.a.clone(), j.b.clone()))
                .collect();
            let mut sorted = keys.clone();
            sorted.sort();
            assert_eq!(keys, sorted, "{name} {} not in canonical order", claim.id);
            assert!(claim.jobs.iter().all(|j| j.state < claim.state_count));
        }
    }
}

#[test]
fn contact_area_is_published_typed_planned_printed_and_evaluated_in_mm2() {
    let text = "The rotor has at least 50 mm2 of face contact with the base.";
    let req: Requirement = serde_json::from_value(json!({
        "ir_version": 1,
        "id": "bearing-contact",
        "name": "Rotor bearing contact",
        "text": text,
        "claims": [{
            "id": "contact",
            "rule": "measure",
            "span": [0, text.chars().count()],
            "args": {
                "fn": {"text": "contact_area"},
                "a": {"part": {"model": {"entry": "machine.py", "builder": "machine"}, "part": {"name": "rotor"}}},
                "b": {"part": {"model": {"entry": "machine.py", "builder": "machine"}, "part": {"name": "base"}}},
                "cmp": {"compare": {"op": ">=", "value": {"value": 50.0, "unit": "mm2", "source": "text"}}}
            }
        }]
    })).unwrap();
    let (_, snapshot) = case("R1");
    let snap: WorkspaceSnapshot = serde_json::from_str(snapshot).unwrap();
    let compiled = compile(&req, &snap, &ProjectSettings::default());
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    assert_eq!(
        library().primitive("contact_area").unwrap().result,
        Dimension::Area
    );
    let public_library = serde_json::to_value(library()).unwrap();
    let published = public_library["primitives"]
        .as_array()
        .unwrap()
        .iter()
        .find(|primitive| primitive["name"] == "contact_area")
        .unwrap();
    assert_eq!(published["result"], "area");
    assert_eq!(published["accuracy"], "exact");
    let plan = compiled.plan.as_ref().unwrap();
    assert_eq!(plan.claims[0].primitive, "contact_area");
    assert_eq!(plan.claims[0].pairing, Pairing::EachPair);
    let chunks = build_requests(
        plan,
        compiled.manifest.as_ref().unwrap(),
        &ChunkOptions::default(),
    )
    .unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(
        chunks[0].params["states"][0]["measurements"][0]["primitive"],
        "contact_area"
    );
    assert_eq!(
        chunks[0].params["states"][0]["measurements"][0]["a"],
        json!(["base"])
    );
    assert_eq!(
        chunks[0].params["states"][0]["measurements"][0]["b"],
        json!(["rotor"])
    );
    let printed = compiled.print.as_ref().unwrap().render();
    assert!(
        printed.contains("Area of common faces between rotor and base"),
        "{printed}"
    );
    assert!(printed.contains("at least 50 mm²"), "{printed}");

    let mut exec = cadpad_core::requirements::FakeExecutor::new(|_, measurement| {
        let (a, b) = (measurement["a"][0].clone(), measurement["b"][0].clone());
        json!({"status":"ok", "value":75.0, "witness":[a,b],
               "items":[{"a":a,"b":b,"status":"ok","value":75.0}]})
    });
    let manifest = compiled.manifest.as_ref().unwrap();
    let raw = run_plan(plan, manifest, &mut exec, &ChunkOptions::default()).unwrap();
    let evaluated = evaluate(
        plan,
        manifest,
        &raw,
        &ProjectSettings::default(),
        "bearing-contact",
        0,
    );
    let measured = evaluated.body.primitives[0].worst.as_ref().unwrap();
    assert_eq!(
        measured.value,
        Some(cadpad_core::requirements::Quantity::new(75.0, Unit::Mm2))
    );
    assert_eq!(
        measured.margin,
        Some(cadpad_core::requirements::Quantity::new(25.0, Unit::Mm2))
    );
}

#[test]
fn others_and_globs_resolve_to_sorted_member_keys() {
    let (r, s) = case("R2");
    let plan = run(r, s).plan.unwrap();
    let first = &plan.claims[0].jobs[0];
    assert_eq!(first.a, ["machine.py#machine/base"]);
    assert_eq!(first.b, ["machine.py#machine/clamp_1"]);
    assert_eq!(
        plan.requires.parts["machine.py#machine/clamp_3"],
        "h-clamp_3"
    );
}

#[test]
fn allowances_apply_per_pair_and_state() {
    let (r, s) = case("E3");
    let c = run(r, s);
    let jobs = &c.plan.unwrap().claims[0].jobs;
    let allowed: Vec<usize> = jobs
        .iter()
        .filter(|j| j.allowed.is_some())
        .map(|j| j.state)
        .collect();
    assert_eq!(allowed, [0], "touch is allowed at door_angle 0 only");
    assert_eq!(
        jobs[0].allowed.as_ref().unwrap().reason,
        "closed door rests on frame"
    );
    // An allowance for a pair the plan never measures is reported.
    let mut v: Value = serde_json::from_str(r).unwrap();
    v["allow"][0]["pair"][0] = json!({"part": {"model": {"entry": "cabinet.py", "builder": "cabinet"}, "part": {"name": "handle"}}});
    let c = run(&v.to_string(), s);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::WAllowUnused && d.at == "allow[0]"),
        "{:?}",
        c.diagnostics
    );
    let unused = c
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::WAllowUnused)
        .unwrap();
    assert_eq!(
        unused.severity,
        cadpad_core::requirements::Severity::Warning
    );
    assert!(unused.span.is_some());
    assert!(unused.fix.is_none());
    assert!(!c.has_errors());
}

#[test]
fn hashes_are_deterministic_and_track_what_changed() {
    let (r, s) = case("E3");
    let a = run(r, s).hashes.unwrap();
    assert_eq!(a, run(r, s).hashes.unwrap());
    // Geometry changed, same names: manifest and plan change, the IR doesn't.
    let g = s.replace("h-door", "h-door-v2");
    let b = run(r, &g).hashes.unwrap();
    assert_eq!(a.requirement, b.requirement);
    assert_ne!(a.manifest, b.manifest);
    assert_ne!(a.plan, b.plan);
    // IR changed: everything chains.
    let edited = r.replace("closed door rests on frame", "door rests on frame");
    let c = run(&edited, s).hashes.unwrap();
    assert_ne!(a.requirement, c.requirement);
    assert_ne!(a.manifest, c.manifest);
    // Settings are part of the manifest.
    let req: Requirement = serde_json::from_str(r).unwrap();
    let snap: WorkspaceSnapshot = serde_json::from_str(s).unwrap();
    let slower = ProjectSettings {
        auto_run_budget_secs: 99,
        ..Default::default()
    };
    let d = compile(&req, &snap, &slower).hashes.unwrap();
    assert_eq!(a.requirement, d.requirement);
    assert_ne!(a.manifest, d.manifest);
    // Over the state limit: an error, so nothing to hash or approve.
    let tight = ProjectSettings {
        max_states: 40,
        ..Default::default()
    };
    let c = compile(&req, &snap, &tight);
    assert!(
        c.hashes.is_none()
            && c.diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::ETypeStates)
    );
}

#[test]
fn binding_errors_block_the_plan_and_suggest_fixes() {
    let (r, s) = case("E3");
    let mut snap: Value = serde_json::from_str(s).unwrap();
    snap["models"][0]["built"]["parts"]
        .as_array_mut()
        .unwrap()
        .retain(|p| p["name"] != "frame");
    snap["models"][0]["built"]["parts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "frames", "geometry_hash": "x"}));
    let c = run(r, &snap.to_string());
    assert!(c.has_errors());
    assert!(c.plan.is_none() && c.manifest.is_none() && c.print.is_none());
    let d = c
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::EBindPartMissing)
        .unwrap();
    assert_eq!(d.at, "text");
    assert!(
        d.span.is_some(),
        "binding diagnostic should point into prose: {d:?}"
    );
    assert!(matches!(
        d.fix,
        Some(cadpad_core::requirements::Fix::AskUser { .. })
    ));
    assert!(format!("{:?}", d.fix).contains("frames"));
}

#[test]
fn text_values_not_in_the_prose_are_downgraded_not_rejected() {
    let (r, s) = case("E5");
    let mut v: Value = serde_json::from_str(r).unwrap();
    v["claims"][0]["args"]["bed"]["list"][0]["quantity"]["value"] = 300.0.into();
    let c = run(&v.to_string(), s);
    assert!(!c.has_errors(), "{:?}", c.diagnostics);
    let downgrade = c
        .downgrades
        .iter()
        .find(|d| d.at == "claims[0].args.bed[0]")
        .expect("unsourced LLM value should be downgraded");
    assert_eq!(downgrade.was, Source::Text);
    let printed = c.print.unwrap().render();
    assert!(
        printed.contains("Numbers I chose: bed x 300 mm, bed y 256 mm, bed z 256 mm"),
        "{printed}"
    );
    let Some(cadpad_core::requirements::Arg::List(items)) = c.requirement.claims[0].args.get("bed")
    else {
        panic!()
    };
    assert!(
        matches!(&items[0], cadpad_core::requirements::Arg::Quantity(l) if l.source == Source::Inferred)
    );
    // The compiled, reviewable value is what an approval hashes; it records
    // the inferred value instead of treating the assistant's `text` label as
    // user-confirmed provenance.
    assert!(c.hashes.is_some());
    assert_eq!(
        c.requirement.claims[0].args["bed"],
        cadpad_core::requirements::Arg::List(items.clone())
    );
}

#[test]
fn library_rules_check_their_signature() {
    // `bolted` is available since P3.6: rewriting a claim to it must surface
    // signature diagnostics on the arguments, not a "not available" rule error.
    let (r, _) = case("E3");
    let mut v: Value = serde_json::from_str(r).unwrap();
    v["claims"][0]["rule"] = "bolted".into();
    let (_, s) = case("E3");
    let c = run(&v.to_string(), s);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::ETypeKind && d.at.starts_with("claims[0].args")),
        "{:?}",
        c.diagnostics
    );
    assert!(
        !c.diagnostics.iter().any(|d| d.at == "claims[0].rule"),
        "{:?}",
        c.diagnostics
    );
}

#[test]
fn unanswered_vague_words_and_unused_chips_block_compilation() {
    let (r, s) = case("E3");
    let mut v: Value = serde_json::from_str(r).unwrap();
    v.as_object_mut().unwrap().remove("answers");
    let c = run(&v.to_string(), s);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::EVague),
        "{:?}",
        c.diagnostics
    );
    let mut v: Value = serde_json::from_str(r).unwrap();
    v["text"] = format!(
        "{} and @[x](cabinet.py#cabinet/handle)",
        v["text"].as_str().unwrap()
    )
    .into();
    let c = run(&v.to_string(), s);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::ECoverMention),
        "{:?}",
        c.diagnostics
    );
}
