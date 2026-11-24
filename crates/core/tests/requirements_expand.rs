use cadpad_core::requirements::{
    DiagnosticCode, ProjectSettings, Requirement, expand_requirement, library_version,
};
use serde_json::Value;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("fixtures/requirements/", $name, ".json"))
    };
}

/// Appendix A cases that use only v1 rules and primitives, with the golden
/// expansion beside each in `fixtures/requirements/expansion/`.
const GOLDEN: [(&str, &str, &str); 5] = [
    (
        "R1",
        fixture!("R1"),
        include_str!("fixtures/requirements/expansion/R1.json"),
    ),
    (
        "R2",
        fixture!("R2"),
        include_str!("fixtures/requirements/expansion/R2.json"),
    ),
    (
        "R4",
        fixture!("R4"),
        include_str!("fixtures/requirements/expansion/R4.json"),
    ),
    (
        "E3",
        fixture!("E3"),
        include_str!("fixtures/requirements/expansion/E3.json"),
    ),
    (
        "E5",
        fixture!("E5"),
        include_str!("fixtures/requirements/expansion/E5.json"),
    ),
];

fn expansion(requirement: &str) -> Value {
    let req: Requirement = serde_json::from_str(requirement).unwrap();
    let out = expand_requirement(&req, &ProjectSettings::default()).unwrap();
    serde_json::to_value(out).unwrap()
}

#[test]
fn v1_reference_cases_expand_to_the_golden_primitive_claims() {
    for (name, requirement, golden) in GOLDEN {
        let expected: Value = serde_json::from_str(golden).unwrap();
        assert_eq!(
            expansion(requirement),
            expected,
            "{name}: expansion changed; if intended, regenerate with \
             cargo test -p cadpad-core --test requirements_expand -- --ignored regenerate_golden"
        );
    }
}

#[test]
#[ignore = "writes golden expansions"]
fn regenerate_golden() {
    for (name, requirement, _) in GOLDEN {
        let path = format!(
            "{}/tests/fixtures/requirements/expansion/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut text = serde_json::to_string_pretty(&expansion(requirement)).unwrap();
        text.push('\n');
        std::fs::write(path, text).unwrap();
    }
}

#[test]
fn expansion_is_tagged_deterministic_and_ordered() {
    for (name, requirement, _) in GOLDEN {
        let first = expansion(requirement);
        assert_eq!(first, expansion(requirement), "{name} deterministic");
        for (i, claim) in first.as_array().unwrap().iter().enumerate() {
            assert_eq!(
                claim["origin"]["library_version"],
                library_version(),
                "{name}[{i}]"
            );
            let id = claim["id"].as_str().unwrap();
            assert!(
                id.starts_with(claim["origin"]["claim"].as_str().unwrap()),
                "{name} {id}"
            );
        }
    }
}

#[test]
fn counts_match_the_spec_expansions() {
    let count = |req: &str| expansion(req).as_array().unwrap().len();
    assert_eq!(count(fixture!("R1")), 1); // interference per pair
    assert_eq!(count(fixture!("R2")), 1);
    assert_eq!(count(fixture!("R4")), 2); // moves_freely + clear_of
    assert_eq!(count(fixture!("E3")), 1);
    assert_eq!(count(fixture!("E5")), 4); // 3 extents + 1 solid count
}

#[test]
fn cases_needing_later_library_entries_fail_cleanly() {
    for (name, requirement) in [
        ("R3", fixture!("R3")),
        ("E1", fixture!("E1")),
        ("E2", fixture!("E2")),
        ("E4", fixture!("E4")),
        ("E6", fixture!("E6")),
        ("X1", fixture!("X1")),
    ] {
        let req: Requirement = serde_json::from_str(requirement).unwrap();
        let errors = expand_requirement(&req, &ProjectSettings::default()).unwrap_err();
        assert!(!errors.is_empty(), "{name}");
        assert!(
            errors.iter().all(|d| d.code == DiagnosticCode::ETypeKind),
            "{name}: {errors:?}"
        );
        assert!(errors.iter().all(|d| d.at.starts_with("claims[")), "{name}");
    }
}
