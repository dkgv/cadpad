//! Diagnostics (spec §8): what binding, type checking and coverage report, with
//! a machine-readable fix so the agent or the user can act on it.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// Stable diagnostic codes. `E_*` block compilation and approval; `W_*` warn
/// .
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum DiagnosticCode {
    /// IR doesn't match the schema.
    #[serde(rename = "E_SCHEMA")]
    ESchema,
    /// Model not found in the project source catalog.
    #[serde(rename = "E_BIND_MODEL_MISSING")]
    EBindModelMissing,
    /// Published part not found in the build result.
    #[serde(rename = "E_BIND_PART_MISSING")]
    EBindPartMissing,
    /// Semantic feature not found on a fresh build.
    #[serde(rename = "E_BIND_FEATURE_MISSING")]
    EBindFeatureMissing,
    /// Parameter not found in the parameter registry.
    #[serde(rename = "E_BIND_PARAM_MISSING")]
    EBindParamMissing,
    /// Fastener metadata missing on a published part.
    #[serde(rename = "E_BIND_FASTENER_MISSING")]
    EBindFastenerMissing,
    /// Mechanism joint not found (extension of the §8 table for `JointRef`).
    #[serde(rename = "E_BIND_JOINT_MISSING")]
    EBindJointMissing,
    /// Feature selects more or fewer entities than the measure needs.
    #[serde(rename = "E_BIND_FEATURE_AMBIGUOUS")]
    EBindFeatureAmbiguous,
    /// Glob matches no parts.
    #[serde(rename = "E_BIND_GLOB_EMPTY")]
    EBindGlobEmpty,
    /// Model failed to build (at a named state).
    #[serde(rename = "E_BUILD")]
    EBuild,
    /// Argument of the wrong kind for a rule or primitive signature.
    #[serde(rename = "E_TYPE_KIND")]
    ETypeKind,
    /// Units of the wrong dimension.
    #[serde(rename = "E_TYPE_UNIT")]
    ETypeUnit,
    /// Value out of range: lo > hi, tol <= 0, step <= 0, outside parameter bounds.
    #[serde(rename = "E_TYPE_RANGE")]
    ETypeRange,
    /// State space exceeds `MAX_STATES`.
    #[serde(rename = "E_TYPE_STATES")]
    ETypeStates,
    /// An `@` chip's mapping to an assembly part no longer matches (spec §3.2).
    #[serde(rename = "E_BIND_CHIP_MISMATCH")]
    EBindChipMismatch,
    /// A vague word maps to more than one rule or setting; ask the user.
    #[serde(rename = "E_VAGUE")]
    EVague,
    /// An `@` chip used by no claim and not in `unsupported`.
    #[serde(rename = "E_COVER_MENTION")]
    ECoverMention,
    /// Text outside every span and `unsupported` entry.
    #[serde(rename = "W_COVER_TEXT")]
    WCoverText,
    /// Parts placed by raw transforms with no seated/contact claim.
    #[serde(rename = "W_PLACEMENT_UNVERIFIED")]
    WPlacementUnverified,
    /// Over the auto-run time budget.
    #[serde(rename = "W_SLOW")]
    WSlow,
    /// An allowance matches no pair at any state.
    #[serde(rename = "W_ALLOW_UNUSED")]
    WAllowUnused,
    /// A history name depends on an occurrence or op index; prefer an
    /// explicit `features=` name.
    #[serde(rename = "W_POSITIONAL_NAME")]
    WPositionalName,
}

impl DiagnosticCode {
    pub const ALL: [DiagnosticCode; 22] = [
        DiagnosticCode::ESchema,
        DiagnosticCode::EBindModelMissing,
        DiagnosticCode::EBindPartMissing,
        DiagnosticCode::EBindFeatureMissing,
        DiagnosticCode::EBindParamMissing,
        DiagnosticCode::EBindFastenerMissing,
        DiagnosticCode::EBindJointMissing,
        DiagnosticCode::EBindFeatureAmbiguous,
        DiagnosticCode::EBindGlobEmpty,
        DiagnosticCode::EBuild,
        DiagnosticCode::ETypeKind,
        DiagnosticCode::ETypeUnit,
        DiagnosticCode::ETypeRange,
        DiagnosticCode::ETypeStates,
        DiagnosticCode::EBindChipMismatch,
        DiagnosticCode::EVague,
        DiagnosticCode::ECoverMention,
        DiagnosticCode::WCoverText,
        DiagnosticCode::WPlacementUnverified,
        DiagnosticCode::WSlow,
        DiagnosticCode::WAllowUnused,
        DiagnosticCode::WPositionalName,
    ];
    /// The code as it appears in JSON and in the spec.
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticCode::ESchema => "E_SCHEMA",
            DiagnosticCode::EBindModelMissing => "E_BIND_MODEL_MISSING",
            DiagnosticCode::EBindPartMissing => "E_BIND_PART_MISSING",
            DiagnosticCode::EBindFeatureMissing => "E_BIND_FEATURE_MISSING",
            DiagnosticCode::EBindParamMissing => "E_BIND_PARAM_MISSING",
            DiagnosticCode::EBindFastenerMissing => "E_BIND_FASTENER_MISSING",
            DiagnosticCode::EBindJointMissing => "E_BIND_JOINT_MISSING",
            DiagnosticCode::EBindFeatureAmbiguous => "E_BIND_FEATURE_AMBIGUOUS",
            DiagnosticCode::EBindGlobEmpty => "E_BIND_GLOB_EMPTY",
            DiagnosticCode::EBuild => "E_BUILD",
            DiagnosticCode::ETypeKind => "E_TYPE_KIND",
            DiagnosticCode::ETypeUnit => "E_TYPE_UNIT",
            DiagnosticCode::ETypeRange => "E_TYPE_RANGE",
            DiagnosticCode::ETypeStates => "E_TYPE_STATES",
            DiagnosticCode::EBindChipMismatch => "E_BIND_CHIP_MISMATCH",
            DiagnosticCode::EVague => "E_VAGUE",
            DiagnosticCode::ECoverMention => "E_COVER_MENTION",
            DiagnosticCode::WCoverText => "W_COVER_TEXT",
            DiagnosticCode::WPlacementUnverified => "W_PLACEMENT_UNVERIFIED",
            DiagnosticCode::WSlow => "W_SLOW",
            DiagnosticCode::WAllowUnused => "W_ALLOW_UNUSED",
            DiagnosticCode::WPositionalName => "W_POSITIONAL_NAME",
        }
    }
    /// `W_*` codes warn; everything else is an error.
    pub fn severity(self) -> Severity {
        if self.as_str().starts_with("W_") {
            Severity::Warning
        } else {
            Severity::Error
        }
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One mechanical edit to the IR, addressed by JSON path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IrEdit {
    /// JSON path of the node to change, e.g. `claims[0].args.over`.
    pub at: String,
    /// Replacement value; omit to remove the node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

/// What to do about a diagnostic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Fix {
    /// Missing or ambiguous information: ask the user, optionally offering choices.
    AskUser {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        question: Option<String>,
    },
    /// The model source must change (publish a name, fastener metadata, an assembly).
    EditSource {
        description: String,
        /// Project-relative file the change applies to, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Suggested source text, when the compiler can produce it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proposed: Option<String>,
    },
    /// Mechanical correction of the IR that the LLM may apply.
    EditIr {
        description: String,
        edits: Vec<IrEdit>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    /// JSON path into the IR, e.g. `claims[0].args.against`.
    pub at: String,
    /// Character range of the requirement text this refers to, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<[usize; 2]>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

impl Diagnostic {
    /// Diagnostic with the code's default severity and no span or fix.
    pub fn new(code: DiagnosticCode, at: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: code.severity(),
            at: at.into(),
            span: None,
            message: message.into(),
            fix: None,
        }
    }
    pub fn with_span(mut self, span: Option<[usize; 2]>) -> Self {
        self.span = span;
        self
    }
    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.fix = Some(fix);
        self
    }
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}: {}", self.code, self.at, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codes_serialize_as_spec_strings_and_cover_section_8() {
        for code in DiagnosticCode::ALL {
            assert_eq!(serde_json::to_value(code).unwrap(), json!(code.as_str()));
            let back: DiagnosticCode = serde_json::from_value(json!(code.as_str())).unwrap();
            assert_eq!(back, code);
        }
        let unique: std::collections::HashSet<_> = DiagnosticCode::ALL.iter().collect();
        assert_eq!(unique.len(), DiagnosticCode::ALL.len());
        for required in [
            "E_SCHEMA",
            "E_BIND_MODEL_MISSING",
            "E_BIND_PART_MISSING",
            "E_BIND_FEATURE_MISSING",
            "E_BIND_PARAM_MISSING",
            "E_BIND_FASTENER_MISSING",
            "E_BIND_FEATURE_AMBIGUOUS",
            "E_BIND_GLOB_EMPTY",
            "E_BUILD",
            "E_TYPE_KIND",
            "E_TYPE_UNIT",
            "E_TYPE_RANGE",
            "E_TYPE_STATES",
            "E_VAGUE",
            "E_COVER_MENTION",
            "W_COVER_TEXT",
            "W_PLACEMENT_UNVERIFIED",
            "W_SLOW",
            "W_ALLOW_UNUSED",
        ] {
            assert!(
                DiagnosticCode::ALL.iter().any(|c| c.as_str() == required),
                "{required}"
            );
        }
    }

    #[test]
    fn severity_follows_prefix() {
        assert_eq!(DiagnosticCode::ESchema.severity(), Severity::Error);
        assert_eq!(DiagnosticCode::WSlow.severity(), Severity::Warning);
        assert!(!Diagnostic::new(DiagnosticCode::WAllowUnused, "allow[0]", "x").is_error());
    }

    #[test]
    fn spec_example_round_trips() {
        let value = json!({
            "code": "E_BIND_PART_MISSING", "severity": "error",
            "at": "claims[0].args.against", "span": [27, 32],
            "message": "@beam is not a part of Frame v4",
            "fix": { "kind": "ask_user", "options": ["upright_near", "rail"] }
        });
        let diagnostic: Diagnostic = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&diagnostic).unwrap(), value);
    }

    #[test]
    fn every_fix_kind_round_trips_and_rejects_unknown_fields() {
        let fixes = [
            json!({"kind": "ask_user", "question": "Retractable to what?"}),
            json!({"kind": "edit_source", "description": "publish a name", "path": "a.py", "proposed": "show(x, name=\"d\")"}),
            json!({"kind": "edit_ir", "description": "swap", "edits": [{"at": "claims[0].span", "value": [0, 3]}, {"at": "claims[1]"}]}),
        ];
        for value in fixes {
            let fix: Fix = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&fix).unwrap(), value);
        }
        assert!(serde_json::from_value::<Fix>(json!({"kind": "ask_user", "bogus": 1})).is_err());
        assert!(serde_json::from_value::<Fix>(json!({"kind": "run_script"})).is_err());
    }
}
