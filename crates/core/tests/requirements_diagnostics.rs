//! Emitted diagnostics at the compiler boundary. Runtime-only diagnostics
//! (`W_SLOW`) are covered by requirements_evaluate.rs. The enum-only codes
//! `E_BIND_FASTENER_MISSING`, `E_BIND_CHIP_MISMATCH`,
//! `W_PLACEMENT_UNVERIFIED`, and `W_POSITIONAL_NAME` have no emission site yet.
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

    // Listed but not emitted by the P1 compiler: no stable name-relation
    // validation currently emits E_BIND_CHIP_MISMATCH; it remains assigned to
    // the build/snapshot integration phase when that check is implemented.
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
