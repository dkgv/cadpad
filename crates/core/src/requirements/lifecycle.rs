//! Final requirement status (spec §6.2, P1.59): the one place that turns
//! what is known about a requirement (compile state, approval, relink,
//! run state, result) into one of the eight statuses, with the reasons.
//!
//! Precedence, first match wins:
//!
//! 1. `draft`: has errors, or never approved;
//! 2. `error`: the subject fails to build;
//! 3. `needs_review`: relink says so (reference gone or ambiguous, kind or
//!    count changed, builder remapped, library or settings changed), or the
//!    prose was edited (the approved version keeps running);
//! 4. `error`: a binding cannot resolve and relink did not explain it;
//! 5. `pending`: queued or running, or not run yet;
//! 6. the result's own status (`inconclusive`, `not_met`, `met_partial`,
//!    `met`), only when it belongs to the current plan.
//!
//! A result shown under `draft`, `needs_review`, `error` or `pending` is the
//! previous one ([`StatusReport::show_previous`]).
use super::evaluate::ResultBody;
use super::relink::{Change, Effect, RelinkReport};
use super::status::RequirementStatus;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default)]
pub struct StatusInputs<'a> {
    /// The IR has compile errors.
    pub has_errors: bool,
    /// An approved version exists.
    pub approved: bool,
    /// The prose or IR was edited after approval (a new draft exists).
    pub edited_since_approval: bool,
    /// A subject model failed to build.
    pub build_failed: bool,
    /// Some binding failed to resolve on the current build.
    pub bind_failed: bool,
    /// Queued or running now.
    pub running: bool,
    /// Relink against the approved manifest, when done.
    pub relink: Option<&'a RelinkReport>,
    /// Hash of the plan the run is for (the approved one, relinked).
    pub current_plan_hash: Option<&'a str>,
    /// The latest result, whatever plan it was for.
    pub result: Option<&'a ResultBody>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StatusReport {
    pub status: RequirementStatus,
    /// Why, one line per reason.
    pub reasons: Vec<String>,
    /// The result in hand is not for the current state: show it as
    /// "previous" (or "last approved").
    pub show_previous: bool,
    /// "Changed since approval": differences that still ran.
    pub changed_since_approval: Vec<Change>,
}

/// Compute the status (P1.59).
pub fn compute_status(i: &StatusInputs) -> StatusReport {
    let has_result = i.result.is_some();
    let mk = |status, reasons: Vec<String>| StatusReport {
        status,
        reasons,
        show_previous: has_result
            && !status.has_verdict()
            && status != RequirementStatus::Inconclusive,
        changed_since_approval: vec![],
    };
    if i.has_errors || !i.approved {
        let why = if i.has_errors {
            "the requirement has errors"
        } else {
            "the requirement is not approved yet"
        };
        return mk(RequirementStatus::Draft, vec![why.into()]);
    }
    if i.build_failed {
        return mk(
            RequirementStatus::Error,
            vec!["a subject model fails to build".into()],
        );
    }
    let mut review: Vec<String> = Vec::new();
    if let Some(r) = i.relink {
        review.extend(
            r.changes
                .iter()
                .filter(|c| c.effect == Effect::NeedsReview)
                .map(|c| c.message.clone()),
        );
    }
    if i.edited_since_approval || i.relink.is_some_and(|r| r.requirement_changed) {
        review.push(
            "the requirement was edited; the approved version is still the one checked".into(),
        );
    }
    if !review.is_empty() {
        return mk(RequirementStatus::NeedsReview, review);
    }
    if i.bind_failed {
        return mk(
            RequirementStatus::Error,
            vec!["a name in the requirement no longer resolves".into()],
        );
    }
    let changed: Vec<Change> = i
        .relink
        .map(|r| r.changed_since_approval().into_iter().cloned().collect())
        .unwrap_or_default();
    let current = match (i.result, i.current_plan_hash) {
        (Some(r), Some(h)) => (r.plan_hash == h).then_some(r),
        (Some(r), None) => Some(r),
        _ => None,
    };
    if i.running {
        return StatusReport {
            changed_since_approval: changed,
            ..mk(RequirementStatus::Pending, vec!["queued or running".into()])
        };
    }
    match current {
        Some(r) => StatusReport {
            status: r.status,
            reasons: vec![],
            show_previous: false,
            changed_since_approval: changed,
        },
        None => StatusReport {
            changed_since_approval: changed,
            ..mk(
                RequirementStatus::Pending,
                vec!["not run on this plan yet".into()],
            )
        },
    }
}
