//! Emitted diagnostics at the compiler boundary. Runtime-only diagnostics
//! (`W_SLOW`) are covered by requirements_evaluate.rs. `E_BIND_FASTENER_MISSING`
//! and `W_POSITIONAL_NAME` are emitted from binding (`bind.rs` tests);
//! `E_BIND_CHIP_MISMATCH` and `W_PLACEMENT_UNVERIFIED` from the assembly checks
//! below and in `requirements::assembly`.
use cadpad_core::requirements::{
    DiagnosticCode, Fix, ProjectSettings, Requirement, WorkspaceSnapshot, compile,
};
use serde_json::{Value, json};

const R1: &str = include_str!("fixtures/requirements/R1.json");
const R1_SNAPSHOT: &str = include_str!("fixtures/requirements/snapshots/R1.json");
const E3: &str = include_str!("fixtures/requirements/E3.json");
const E3_SNAPSHOT: &str = include_str!("fixtures/requirements/snapshots/E3.json");
const E5: &str = include_str!("fixtures/requirements/E5.json");
const E5_SNAPSHOT: &str = include_str!("fixtures/requirements/snapshots/E5.json");
const E2: &str = include_str!("fixtures/requirements/E2.json");
const E2_SNAPSHOT: &str = include_str!("fixtures/requirements/snapshots/E2.json");

fn compile_json(
    req: &Value,
    snap: &Value,
    settings: &ProjectSettings,
) -> cadpad_core::requirements::Compiled {
    compile(
        &serde_json::from_value::<Requirement>(req.clone()).unwrap(),
        &serde_json::from_value::<WorkspaceSnapshot>(snap.clone()).unwrap(),
        settings,
    )
}

fn assert_diagnostic(
    compiled: &cadpad_core::requirements::Compiled,
    code: DiagnosticCode,
    at: &str,
    fix_kind: Option<&str>,
) {
    let d = compiled
        .diagnostics
        .iter()
        .find(|d| d.code == code && d.at == at)
        .unwrap_or_else(|| panic!("expected {code} at {at}; got {:?}", compiled.diagnostics));
    assert_eq!(d.severity, code.severity());
    let actual = match &d.fix {
        None => None,
        Some(Fix::AskUser { .. }) => Some("ask_user"),
        Some(Fix::EditSource { .. }) => Some("edit_source"),
        Some(Fix::EditIr { .. }) => Some("edit_ir"),
    };
    assert_eq!(actual, fix_kind, "{d:?}");
}

#[test]
fn binding_schema_and_coverage_diagnostics_have_stable_locations_and_fixes() {
    let r1: Value = serde_json::from_str(R1).unwrap();
    let s1: Value = serde_json::from_str(R1_SNAPSHOT).unwrap();

    let mut req = r1.clone();
    req["ir_version"] = json!(99);
    assert_diagnostic(
        &compile_json(&req, &s1, &ProjectSettings::default()),
        DiagnosticCode::ESchema,
        "ir_version",
        None,
    );

    let mut req = r1.clone();
    req["claims"][0]["args"]["against"]["part"]["model"]["entry"] = json!("missing.py");
    assert_diagnostic(
        &compile_json(&req, &s1, &ProjectSettings::default()),
        DiagnosticCode::EBindModelMissing,
        "claims[0].args.against",
        Some("ask_user"),
    );

    let mut req = r1.clone();
    req["claims"][0]["args"]["against"]["part"]["part"]["name"] = json!("bas");
    let c = compile_json(&req, &s1, &ProjectSettings::default());
    let d = c
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::EBindPartMissing)
        .unwrap();
    assert_eq!(d.at, "claims[0].args.against");
    assert!(matches!(d.fix, Some(Fix::AskUser { .. })));

    let mut req = r1.clone();
    req["claims"][0]["args"]["against"]["part"]["part"] = json!({"glob":"nothing_*"});
    assert_diagnostic(
        &compile_json(&req, &s1, &ProjectSettings::default()),
        DiagnosticCode::EBindGlobEmpty,
        "claims[0].args.against",
        None,
    );

    let mut snap = s1.clone();
    snap["models"][0]["build_error"] = json!("syntax error");
    assert_diagnostic(
        &compile_json(&r1, &snap, &ProjectSettings::default()),
        DiagnosticCode::EBuild,
        "claims[0].args.against",
        None,
    );

    let mut req = r1.clone();
    req["claims"][0]["args"]["extra_feature"] = json!({"feature": {
        "kind":"explicit",
        "part":{"model":{"entry":"machine.py","builder":"machine"},"part":{"name":"base"}},
        "name":"missing_feature"
    }});
    assert_diagnostic(
        &compile_json(&req, &s1, &ProjectSettings::default()),
        DiagnosticCode::EBindFeatureMissing,
        "claims[0].args.extra_feature",
        None,
    );

    let mut req = r1.clone();
    req["claims"][0]["args"]["extra_feature"] = json!({"feature": {
        "kind":"explicit",
        "part":{"model":{"entry":"machine.py","builder":"machine"},"part":{"glob":"base*"}},
        "name":"missing_feature"
    }});
    assert_diagnostic(
        &compile_json(&req, &s1, &ProjectSettings::default()),
        DiagnosticCode::EBindFeatureAmbiguous,
        "claims[0].args.extra_feature",
        None,
    );

    let mut req = r1.clone();
    req["text"] =
        json!("ensure turntable can rotate freely without clashing with base and mystery");
    assert_diagnostic(
        &compile_json(&req, &s1, &ProjectSettings::default()),
        DiagnosticCode::WCoverText,
        "text",
        None,
    );

    let mut req = r1;
    req["text"] = json!(
        "ensure turntable can rotate freely without clashing with base and @[unused](machine.py#machine/unused_part)"
    );
    assert_diagnostic(
        &compile_json(&req, &s1, &ProjectSettings::default()),
        DiagnosticCode::ECoverMention,
        "text",
        None,
    );

    let mut req: Value = serde_json::from_str(E3).unwrap();
    req.as_object_mut().unwrap().remove("answers");
    assert_diagnostic(
        &compile_json(&req, &snap_for(E3_SNAPSHOT), &ProjectSettings::default()),
        DiagnosticCode::EVague,
        "text",
        Some("ask_user"),
    );
}

#[test]
fn assembled_models_verify_chip_mappings_and_placement_coverage() {
    let req: Value = serde_json::from_str(E2).unwrap();
    let snap: Value = serde_json::from_str(E2_SNAPSHOT).unwrap();
    // The shipped E2 fixture is a verified assembly: no diagnostics.
    let clean = compile_json(&req, &snap, &ProjectSettings::default());
    assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

    // The covered part must record the tagged model as its origin.
    let mut wrong_origin = snap.clone();
    wrong_origin["models"][2]["built"]["placements"]["pcb"]["origin"]["builder"] = json!("other");
    let c = compile_json(&req, &wrong_origin, &ProjectSettings::default());
    assert_diagnostic(
        &c,
        DiagnosticCode::EBindChipMismatch,
        "covers[0].part",
        Some("edit_source"),
    );
    let d = c
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::EBindChipMismatch)
        .unwrap();
    assert!(d.message.contains("pcb.py#other"), "{}", d.message);

    // A covered part with no recorded origin cannot be verified either.
    let mut no_origin = snap.clone();
    no_origin["models"][2]["built"]["placements"]["pcb"]
        .as_object_mut()
        .unwrap()
        .remove("origin");
    assert_diagnostic(
        &compile_json(&req, &no_origin, &ProjectSettings::default()),
        DiagnosticCode::EBindChipMismatch,
        "covers[0].part",
        Some("edit_source"),
    );

    // A referenced part positioned by a raw transform with no contact claim
    // warns instead of being trusted.
    let mut raw = snap;
    raw["models"][2]["built"]["placements"]["pcb"] = json!({
        "method": "position",
        "origin": {"entry": "pcb.py", "builder": "pcb"}
    });
    let mut req = req;
    // Drop the no_overlap(pcb, case) claim and check `pcb` without asserting
    // contact, so the placement really is unverified.
    req["claims"].as_array_mut().unwrap().remove(0);
    req["claims"][0]["args"]["a"] = json!({"part": {
        "model": {"entry": "device.py", "builder": "device"},
        "part": {"name": "pcb"}
    }});
    let c = compile_json(&req, &raw, &ProjectSettings::default());
    assert_diagnostic(
        &c,
        DiagnosticCode::WPlacementUnverified,
        "claims[0].args.a",
        None,
    );
}

#[test]
fn parameter_and_type_diagnostics_have_stable_locations() {
    let mut req: Value = serde_json::from_str(E3).unwrap();
    let snap: Value = serde_json::from_str(E3_SNAPSHOT).unwrap();
    req["claims"][0]["args"]["over"]["states"]["param"]["name"] = json!("door_angl");
    let c = compile_json(&req, &snap, &ProjectSettings::default());
    assert_diagnostic(
        &c,
        DiagnosticCode::EBindParamMissing,
        "claims[0].args.over.param",
        Some("ask_user"),
    );

    let mut req: Value = serde_json::from_str(R1).unwrap();
    let snap = snap_for(R1_SNAPSHOT);
    req["claims"][0]["args"]["over"]["states"]["range"]["step"]["value"] = json!(0.0);
    assert_diagnostic(
        &compile_json(&req, &snap, &ProjectSettings::default()),
        DiagnosticCode::ETypeRange,
        "claims[0].args.over.range.step",
        None,
    );

    let mut req: Value = serde_json::from_str(R1).unwrap();
    req["claims"][0]["args"]["over"]["states"] = json!({
        "kind":"joint",
        "joint":{"model":{"entry":"machine.py","builder":"machine"},"name":"hinge"},
        "range": req["claims"][0]["args"]["over"]["states"]["range"].clone()
    });
    assert_diagnostic(
        &compile_json(&req, &snap, &ProjectSettings::default()),
        DiagnosticCode::EBindJointMissing,
        "claims[0].args.over.joint",
        None,
    );

    let mut req: Value = serde_json::from_str(R1).unwrap();
    let snap: Value = serde_json::from_str(R1_SNAPSHOT).unwrap();
    req["claims"][0]["rule"] = json!("unknown_rule");
    assert_diagnostic(
        &compile_json(&req, &snap, &ProjectSettings::default()),
        DiagnosticCode::ETypeKind,
        "claims[0].rule",
        None,
    );

    let mut req: Value = serde_json::from_str(E5).unwrap();
    let snap: Value = serde_json::from_str(E5_SNAPSHOT).unwrap();
    req["claims"][0]["args"]["bed"]["list"][0]["quantity"]["unit"] = json!("deg");
    assert_diagnostic(
        &compile_json(&req, &snap, &ProjectSettings::default()),
        DiagnosticCode::ETypeUnit,
        "claims[0].args.bed[0]",
        None,
    );

    let req: Value = serde_json::from_str(R1).unwrap();
    let c = compile_json(
        &req,
        &snap_for(R1_SNAPSHOT),
        &ProjectSettings {
            max_states: 1,
            ..Default::default()
        },
    );
    assert_diagnostic(&c, DiagnosticCode::ETypeStates, "claims[0].args.over", None);
}

fn snap_for(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap()
}
