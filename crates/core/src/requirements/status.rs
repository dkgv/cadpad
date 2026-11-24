//! Requirement status (spec §6.2). Computed by the run lifecycle (P1.59);
//! the vocabulary lives here so every layer shares it.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
    /// Not yet approved, or has errors.
    Draft,
    /// Prose changed, or relink changed meaning; the last approved result is
    /// still shown.
    NeedsReview,
    /// The subject fails to build or a binding can't resolve.
    Error,
    /// Queued or running; any previous result is shown as "previous".
    Pending,
    /// A check hit its per-pair time limit: nothing is claimed for it, and the
    /// slow pair is named.
    Inconclusive,
    NotMet,
    /// All claims met; `unsupported` is non-empty.
    MetPartial,
    Met,
}

impl RequirementStatus {
    pub const ALL: [RequirementStatus; 8] = [
        RequirementStatus::Draft,
        RequirementStatus::NeedsReview,
        RequirementStatus::Error,
        RequirementStatus::Pending,
        RequirementStatus::Inconclusive,
        RequirementStatus::NotMet,
        RequirementStatus::MetPartial,
        RequirementStatus::Met,
    ];
    /// Statuses that carry a result for the current plan.
    pub fn has_verdict(self) -> bool {
        matches!(
            self,
            RequirementStatus::NotMet | RequirementStatus::MetPartial | RequirementStatus::Met
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_serialize_as_spec_names() {
        let names: Vec<_> = RequirementStatus::ALL
            .iter()
            .map(|s| serde_json::to_value(s).unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "draft",
                "needs_review",
                "error",
                "pending",
                "inconclusive",
                "not_met",
                "met_partial",
                "met"
            ]
        );
        assert!(!RequirementStatus::Inconclusive.has_verdict());
        assert!(RequirementStatus::MetPartial.has_verdict());
    }
}
