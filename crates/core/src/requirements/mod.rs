//! Requirements: compiled, checkable design intent. This module holds the
//! requirement IR (the typed language an LLM front end emits), quantities with
//! units, the prose quantity parser, and diagnostics. Binding, type checking,
//! expansion, printing and evaluation build on these types.
//! See `docs/requirements-spec.md`.
// Diagnostic carries a message, a path and an optional fix; it is cheap to
// build and rare, so returning it by value in `Result` is fine here.
#![allow(clippy::result_large_err)]

pub mod assembly;
pub mod auto_edits;
pub mod bind;
pub mod compile;
pub mod coverage;
pub mod diagnostics;
pub mod dispatch;
pub mod evaluate;
pub mod expand;
pub mod fasteners;
pub mod findings;
pub mod ir;
pub mod iso286;
pub mod library;
pub mod lifecycle;
pub mod manifest;
pub mod parse;
pub mod plan;
pub mod print;
pub mod provenance;
pub mod quantity;
pub mod refs;
pub mod relink;
pub mod replay;
pub mod run;
pub mod settings;
pub mod snapshot;
pub mod states;
pub mod status;
pub mod storage;
pub mod suggestions;
pub mod tool;
pub mod typecheck;
pub mod worker_plan;

pub use compile::{Compiled, Hashes, compile};
pub use diagnostics::{Diagnostic, DiagnosticCode, Fix, IrEdit, Severity};
pub use evaluate::{CheckStatus, EvalResult, ResultBody, RunTimings, evaluate};
pub use expand::{Origin, Pairing, PrimitiveClaim, expand_claim, expand_requirement};
pub use findings::{AllowanceDraft, Finding, FindingLabel, draft_allowances, label_findings};
pub use ir::*;
pub use library::{Library, library, library_version};
pub use lifecycle::{StatusInputs, StatusReport, compute_status};
pub use quantity::{Dimension, Quantity, Unit};
pub use relink::{BuildIndex, Effect, RelinkReport, relink};
pub use settings::ProjectSettings;
pub use snapshot::WorkspaceSnapshot;
pub use status::RequirementStatus;
pub use worker_plan::{
    ChunkOptions, EvalError, FakeExecutor, PlanExecutor, RawResults, build_requests, run_plan,
};

/// Version of the IR shape. Bumped on any breaking schema change.
pub const IR_VERSION: u32 = 1;

/// JSON schema of [`Requirement`]; snapshotted in
/// `tests/fixtures/requirements-ir-schema-v1.json`.
pub fn ir_schema() -> serde_json::Value {
    let schema = schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<Requirement>();
    serde_json::to_value(schema).expect("schema serialization")
}
