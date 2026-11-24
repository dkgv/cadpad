use cadpad_core::requirements::{Requirement, ir_schema, parse::Parsed, parse::parse_quantities};
use serde_json::Value;

macro_rules! fixtures {
    ($($name:literal),*) => {
        [$(($name, include_str!(concat!("fixtures/requirements/", $name, ".json")))),*]
    };
}

/// Appendix A reference cases R1-R4 and E1-E6, plus X1 for the variants the
/// reference cases don't reach.
const FIXTURES: [(&str, &str); 11] = fixtures!(
    "R1", "R2", "R3", "R4", "E1", "E2", "E3", "E4", "E5", "E6", "X1"
);

#[test]
fn ir_schema_matches_snapshot() {
    let locked: Value =
        serde_json::from_str(include_str!("fixtures/requirements-ir-schema-v1.json")).unwrap();
    assert_eq!(
        locked,
        ir_schema(),
        "the requirement IR schema changed: bump ir_version if it is breaking, then regenerate \
         tests/fixtures/requirements-ir-schema-v1.json (cargo test -p cadpad-core --test requirements_ir \
         -- --ignored regenerate_schema_snapshot)"
    );
    assert_eq!(locked["additionalProperties"], false);
}

#[test]
#[ignore = "writes the schema snapshot"]
fn regenerate_schema_snapshot() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/requirements-ir-schema-v1.json"
    );
    let mut text = serde_json::to_string_pretty(&ir_schema()).unwrap();
    text.push('\n');
    std::fs::write(path, text).unwrap();
}

#[test]
fn reference_cases_round_trip_exactly() {
    for (name, text) in FIXTURES {
        let original: Value = serde_json::from_str(text).unwrap();
        let requirement: Requirement =
            serde_json::from_value(original.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            serde_json::to_value(&requirement).unwrap(),
            original,
            "{name} round trip"
        );
        let again: Requirement = serde_json::from_str(&requirement.canonical_json()).unwrap();
        assert_eq!(again, requirement, "{name} canonical round trip");
    }
}

#[test]
fn reference_cases_validate_against_the_schema_and_structurally() {
    let validator = jsonschema::validator_for(&ir_schema()).unwrap();
    for (name, text) in FIXTURES {
        let value: Value = serde_json::from_str(text).unwrap();
        let errors: Vec<_> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
        let requirement: Requirement = serde_json::from_value(value).unwrap();
        let diagnostics = requirement.validate();
        assert!(diagnostics.is_empty(), "{name}: {diagnostics:?}");
        assert_eq!(requirement.content_hash().len(), 64);
    }
}

#[test]
fn schema_rejects_what_serde_rejects() {
    let validator = jsonschema::validator_for(&ir_schema()).unwrap();
    let mut value: Value = serde_json::from_str(FIXTURES[0].1).unwrap();
    value["surprise"] = Value::Bool(true);
    assert!(!validator.is_valid(&value));
    assert!(serde_json::from_value::<Requirement>(value).is_err());
    let mut value: Value = serde_json::from_str(FIXTURES[0].1).unwrap();
    value["claims"][0]["args"]["over"]["states"]["range"]["step"]["unit"] = "furlong".into();
    assert!(!validator.is_valid(&value));
    assert!(serde_json::from_value::<Requirement>(value).is_err());
    let mut value: Value = serde_json::from_str(FIXTURES[0].1).unwrap();
    value["claims"][0]["args"]["over"]["states"]["bogus"] = 1.into();
    assert!(!validator.is_valid(&value));
}

#[test]
fn spans_cover_text_and_numbers_in_text_claims_parse() {
    // Spans are character offsets into `text`; E3's span contains its 110°.
    for (name, text) in FIXTURES {
        let r: Requirement = serde_json::from_str(text).unwrap();
        for claim in &r.claims {
            let span = claim.span.unwrap();
            let slice: String = r
                .text
                .chars()
                .skip(span[0])
                .take(span[1] - span[0])
                .collect();
            assert!(!slice.is_empty(), "{name} {}", claim.id);
            let _ = parse_quantities(&slice);
        }
    }
    let r: Requirement = serde_json::from_str(FIXTURES[6].1).unwrap();
    let span = r.claims[0].span.unwrap();
    let slice: String = r
        .text
        .chars()
        .skip(span[0])
        .take(span[1] - span[0])
        .collect();
    assert!(
        parse_quantities(&slice)
            .iter()
            .any(|m| matches!(m.value, Parsed::Quantity(q) if q.value == 110.0))
    );
}
