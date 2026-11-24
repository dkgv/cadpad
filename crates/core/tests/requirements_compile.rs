//! The compiler end to end on Appendix A cases that use only v1 rules
//! (R1, R2, R4, E3, E5), against fixture build snapshots.
use cadpad_core::requirements::{
    Compiled, DiagnosticCode, ProjectSettings, Requirement, Source, WorkspaceSnapshot, compile,
};
use serde_json::{Value, json};

macro_rules! case {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!("fixtures/requirements/", $name, ".json")),
            include_str!(concat!("fixtures/requirements/snapshots/", $name, ".json")),
            include_str!(concat!("fixtures/requirements/print/", $name, ".txt")),
        )
    };
}

const CASES: [(&str, &str, &str, &str); 5] = [
    case!("R1"),
    case!("R2"),
    case!("R4"),
    case!("E3"),
    case!("E5"),
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
fn v1_reference_cases_compile_without_diagnostics() {
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
        std::fs::write(path, run(req, snap).print.unwrap().render()).unwrap();
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
    assert!(format!("{:?}", d.fix).contains("frames"));
}

#[test]
fn text_values_not_in_the_prose_are_downgraded_not_rejected() {
    let (r, s) = case("E5");
    let mut v: Value = serde_json::from_str(r).unwrap();
    v["claims"][0]["args"]["bed"]["list"][0]["quantity"]["value"] = 300.0.into();
    let c = run(&v.to_string(), s);
    assert!(!c.has_errors(), "{:?}", c.diagnostics);
    assert!(
        c.downgrades.iter().any(|d| d.at == "claims[0].args.bed[0]"),
        "{:?}",
        c.downgrades
    );
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
}

#[test]
fn library_rules_that_need_later_phases_say_so() {
    let (r, _) = case("E3");
    let mut v: Value = serde_json::from_str(r).unwrap();
    v["claims"][0]["rule"] = "bolted".into();
    let (_, s) = case("E3");
    let c = run(&v.to_string(), s);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::ETypeKind && d.at == "claims[0].rule"),
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
