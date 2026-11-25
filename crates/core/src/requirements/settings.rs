//! Project settings that requirement results depend on (spec §4.2, §6.4).
//! Only *computational* settings live here: design values (clearances,
//! tolerances, bed sizes) are never settings, they are claim arguments with a
//! source (spec §4 "No design defaults"). The defaults are in [`defaults`], in
//! one place. Settings are stored with the project and are part of every
//! result's inputs, so [`ProjectSettings::content_hash`] goes into the cache
//! key.
use super::diagnostics::{Diagnostic, DiagnosticCode};
use super::ir::canonical;
use super::quantity::{Dimension, Quantity, Unit};
use crate::portable;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Reserved catalog record makes settings project-local, portable, and part of
/// the same compare-and-swap as requirements and motion tests.
pub const CATALOG_SETTINGS_PREFIX: &str = "__cadpad_project_settings_v1__";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSettingsRecord {
    pub project_settings: ProjectSettings,
}

/// Named defaults. Change values here only.
pub mod defaults {
    /// Overlap tolerance ε in mm³: interference at or below this is "no
    /// overlap". Exact contact measures 0, so there is no noise band; 1e-3 mm³
    /// is about 1e-5 mm deep on a 10×10 mm face (phase 0).
    pub const OVERLAP_TOLERANCE_MM3: f64 = 1e-3;
    /// Most states one claim may expand to.
    pub const MAX_STATES: u32 = 512;
    /// Requirements estimated to cost more than this run on demand, not
    /// automatically after a build (spec §6.4).
    pub const AUTO_RUN_BUDGET_SECS: u32 = 10;
    /// Per-pair time limit; a pair over it makes its claim inconclusive.
    pub const PAIR_TIME_LIMIT_SECS: u32 = 20;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ProjectSettings {
    /// Overlap tolerance ε; a volume.
    pub overlap_tolerance: Quantity,
    /// Most states per claim (`E_TYPE_STATES` above this).
    pub max_states: u32,
    /// Auto-run time budget in seconds.
    pub auto_run_budget_secs: u32,
    /// Per-pair time limit in seconds.
    pub pair_time_limit_secs: u32,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            overlap_tolerance: Quantity::new(defaults::OVERLAP_TOLERANCE_MM3, Unit::Mm3),
            max_states: defaults::MAX_STATES,
            auto_run_budget_secs: defaults::AUTO_RUN_BUDGET_SECS,
            pair_time_limit_secs: defaults::PAIR_TIME_LIMIT_SECS,
        }
    }
}

impl ProjectSettings {
    pub fn validate(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        match self.overlap_tolerance.expect(Dimension::Volume) {
            Err(e) => out.push(e.diagnostic("overlap_tolerance")),
            Ok(v) if !(v.is_finite() && v > 0.0) => out.push(Diagnostic::new(
                DiagnosticCode::ETypeRange,
                "overlap_tolerance",
                "overlap tolerance must be greater than zero",
            )),
            Ok(_) => {}
        }
        if self.max_states == 0 || self.max_states > defaults::MAX_STATES {
            out.push(Diagnostic::new(
                DiagnosticCode::ETypeRange,
                "max_states",
                "max_states must be between 1 and 512",
            ));
        }
        if self.auto_run_budget_secs > 120
            || self.pair_time_limit_secs == 0
            || self.pair_time_limit_secs > 120
        {
            out.push(Diagnostic::new(
                DiagnosticCode::ETypeRange,
                "time_budget",
                "auto-run budget must be 0–120 seconds and pair time limit must be 1–120 seconds",
            ));
        }
        out
    }

    /// Hash of the canonical JSON: an input to every result's cache key.
    pub fn content_hash(&self) -> String {
        let value = serde_json::to_value(self).expect("settings serialize");
        let mut text = String::new();
        canonical(&value, &mut text);
        portable::bytes_hash(text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec_and_validate() {
        let s = ProjectSettings::default();
        assert_eq!(s.overlap_tolerance, Quantity::new(1e-3, Unit::Mm3));
        assert_eq!(s.max_states, 512);
        assert_eq!(s.pair_time_limit_secs, 20);
        assert!(s.validate().is_empty());
    }

    #[test]
    fn invalid_settings_are_reported() {
        let s = ProjectSettings {
            overlap_tolerance: Quantity::new(1.0, Unit::Mm),
            max_states: 0,
            ..Default::default()
        };
        let at: Vec<_> = s.validate().into_iter().map(|d| d.at).collect();
        assert_eq!(at, ["overlap_tolerance", "max_states"]);
    }

    #[test]
    fn hash_tracks_every_field_and_empty_json_means_defaults() {
        let base = ProjectSettings::default();
        let parsed: ProjectSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, base);
        assert_eq!(parsed.content_hash(), base.content_hash());
        let changed = ProjectSettings {
            max_states: 256,
            ..base.clone()
        };
        assert_ne!(changed.content_hash(), base.content_hash());
        assert!(serde_json::from_str::<ProjectSettings>(r#"{"epsilon": 1}"#).is_err());
        assert!(serde_json::from_str::<ProjectSettings>(r#"{"printer_presets": {}}"#).is_err());
    }
}
