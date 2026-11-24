//! Requirement entries in the project catalog (spec §2.1). They live beside
//! the motion tests in the one per-project catalog (see
//! [`crate::motion_tests::Saved`]), so they share its compare-and-swap
//! fingerprint, size bound and Save/Open archive.
//!
//! An entry holds the latest **draft** IR and, once the user approved it, the
//! **approved** `{IR, manifest, plan, hashes}`. The manifest and plan are
//! generated, never edited, like a lockfile. Editing the prose after approval
//! only replaces the draft: the approved version is kept and the status is
//! `needs_review` until the user approves again.
//!
//! Approval is user-only. The methods that approve or delete take no actor
//! (they act as the local user) and are not reachable from `tools::dispatch`;
//! agents have `list`, `compile`, `save` and the run commands only.
use super::compile::{Compiled, Hashes, compile};
use super::diagnostics::Diagnostic;
use super::ir::Requirement;
use super::manifest::Manifest;
use super::plan::Plan;
use super::print::Print;
use super::settings::ProjectSettings;
use super::status::RequirementStatus;
use super::tool::snapshot_for;
use crate::motion_tests::{Catalog, Saved, fingerprint};
use crate::{Core, Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Most requirements one project catalog holds (motion tests are limited
/// separately).
pub const MAX_REQUIREMENTS: usize = 64;
/// Largest serialized entry: IR, manifest, plan and print together.
pub const MAX_ENTRY_BYTES: usize = 1024 * 1024;
const USER: &str = "local-user";

fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_REQUIREMENT", message)
}

/// Settings every requirement result depends on. Project-stored settings are
/// not wired yet, so this is the default set.
// TODO: read the project's stored settings once they have a home.
pub fn project_settings() -> ProjectSettings {
    ProjectSettings::default()
}

/// Discriminator that keeps requirement entries distinguishable from motion
/// tests in the shared catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Requirement,
}

/// Content hashes of what the user approved (mirrors [`Hashes`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StoredHashes {
    pub requirement: String,
    pub manifest: String,
    pub plan: String,
}

impl From<&Hashes> for StoredHashes {
    fn from(h: &Hashes) -> Self {
        Self {
            requirement: h.requirement.clone(),
            manifest: h.manifest.clone(),
            plan: h.plan.clone(),
        }
    }
}

/// What the user approved: the IR, the manifest it linked to, and the plan.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Approved {
    pub requirement: Requirement,
    pub manifest: Manifest,
    pub plan: Plan,
    /// The print the user reviewed.
    pub print: Print,
    pub hashes: StoredHashes,
    /// Library version the plan was compiled with; a change marks the
    /// requirement `needs_review`.
    pub library_version: String,
    pub settings_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementEntry {
    pub kind: EntryKind,
    /// The latest saved IR; equals the approved IR until it is edited.
    pub draft: Requirement,
    /// Diagnostics of the draft's last compile.
    #[serde(default)]
    pub draft_diagnostics: Vec<Diagnostic>,
    /// Canonical print of the draft; absent while it has errors.
    #[serde(default)]
    pub draft_print: Option<Print>,
    /// Hashes of the draft's last compile; absent while it has errors. What
    /// a review of the draft would approve.
    #[serde(default)]
    pub draft_hashes: Option<StoredHashes>,
    #[serde(default)]
    pub approved: Option<Approved>,
    /// Last evaluation result of the approved plan (placeholder JSON until
    /// the run lifecycle fills it).
    #[serde(default)]
    pub last_result: Option<Value>,
    /// The stored result is historical, following import or a new approval.
    #[serde(default, skip_serializing_if = "is_false")]
    pub rerun_required: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// How a requirement stands with respect to approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    /// Never approved.
    Draft,
    /// Approved, and the draft is the approved IR.
    Approved,
    /// Approved earlier; the draft has since been edited. The approved
    /// version is the one that runs.
    EditedSinceApproval,
}

impl RequirementEntry {
    pub fn new_draft(compiled: &Compiled) -> Self {
        let mut entry = Self {
            kind: EntryKind::Requirement,
            draft: compiled.requirement.clone(),
            draft_diagnostics: vec![],
            draft_print: None,
            draft_hashes: None,
            approved: None,
            last_result: None,
            rerun_required: false,
        };
        entry.set_draft(compiled);
        entry
    }

    /// Replace the draft with a fresh compile. The approved version, its
    /// result and re-run mark are untouched.
    pub fn set_draft(&mut self, compiled: &Compiled) {
        self.draft = compiled.requirement.clone();
        self.draft_diagnostics = compiled.diagnostics.clone();
        self.draft_print = compiled.print.clone();
        self.draft_hashes = compiled.hashes.as_ref().map(StoredHashes::from);
    }

    pub fn approval(&self) -> Approval {
        match &self.approved {
            None => Approval::Draft,
            Some(a) if a.hashes.requirement == self.draft.content_hash() => Approval::Approved,
            Some(_) => Approval::EditedSinceApproval,
        }
    }

    /// Status (spec §6.2) as far as storage can tell. `None`: approved and
    /// current but not run yet; the run lifecycle supplies the rest.
    pub fn status(&self) -> Option<RequirementStatus> {
        let Some(approved) = &self.approved else {
            return Some(RequirementStatus::Draft);
        };
        if self.approval() == Approval::EditedSinceApproval
            || approved.library_version != super::library_version()
        {
            return Some(RequirementStatus::NeedsReview);
        }
        if self.rerun_required {
            return Some(RequirementStatus::Pending);
        }
        self.last_result
            .as_ref()
            .and_then(|r| serde_json::from_value(r.get("status")?.clone()).ok())
    }

    /// Import from a project file: the stored result is only history.
    pub fn mark_imported(&mut self) {
        self.rerun_required = self.approved.is_some();
    }

    /// Size and identity limits, with clear errors.
    pub fn validate(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        if self.draft.id != id
            || self
                .approved
                .as_ref()
                .is_some_and(|a| a.requirement.id != id)
        {
            return Err(invalid("Requirement ID does not match its catalog key"));
        }
        let size = serde_json::to_vec(self)?.len();
        if size > MAX_ENTRY_BYTES {
            return Err(invalid(format!(
                "Requirement '{id}' is {} KiB; one requirement is limited to {} KiB. \
                 Split it into several requirements.",
                size / 1024,
                MAX_ENTRY_BYTES / 1024
            )));
        }
        Ok(())
    }
}

fn validate_id(id: &str) -> Result<()> {
    if id.trim().is_empty() || id.len() > 120 || id.chars().any(char::is_control) {
        return Err(invalid(
            "Requirement ID must be 1..120 characters without control characters",
        ));
    }
    Ok(())
}

/// Count limit across the catalog (the motion-test limit is separate).
pub fn check_count(catalog: &Catalog) -> Result<()> {
    let n = catalog
        .values()
        .filter(|saved| saved.requirement().is_some())
        .count();
    if n > MAX_REQUIREMENTS {
        return Err(invalid(format!(
            "At most {MAX_REQUIREMENTS} requirements per project; this would make {n}. \
             Delete one first."
        )));
    }
    Ok(())
}

// ----------------------------------------------------------------- listing

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ApprovedInfo {
    pub hashes: StoredHashes,
    /// The reviewed print, rendered as the user read it.
    pub rendered: String,
    pub print: Print,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RequirementInfo {
    pub id: String,
    pub name: String,
    /// `draft`, `needs_review`, or the last result's status; absent when the
    /// approved requirement has not been run.
    pub status: Option<RequirementStatus>,
    pub approval: Approval,
    /// The user's prose with `@[Label](entry#builder)` chips.
    pub text: String,
    /// Hash of the draft IR (what a review of the draft would approve).
    pub draft_hash: String,
    /// Requirement, manifest and plan hashes of the compiled draft, to pass to
    /// `approve_requirement`; null while the draft has errors.
    #[serde(default)]
    pub draft_hashes: Option<StoredHashes>,
    /// Canonical print of the draft; absent while it has errors.
    pub print: Option<Print>,
    pub rendered: Option<String>,
    /// Diagnostics of the draft's last compile (errors block approval).
    pub diagnostics: Vec<Diagnostic>,
    /// The approved version, which is what runs.
    pub approved: Option<ApprovedInfo>,
    pub last_result: Option<Value>,
    /// The stored result must be rechecked.
    pub rerun_required: bool,
    /// The project's latest requirement run when it was of this requirement
    /// (queued, running, cancelling or finished). Set by the tool; absent in
    /// catalog-only replies.
    #[serde(default)]
    pub run: Option<super::run::RunInfo>,
}

impl RequirementInfo {
    pub fn from_entry(id: &str, entry: &RequirementEntry) -> Self {
        Self {
            id: id.to_owned(),
            name: entry.draft.name.clone(),
            status: entry.status(),
            approval: entry.approval(),
            text: entry.draft.text.clone(),
            draft_hash: entry.draft.content_hash(),
            draft_hashes: entry.draft_hashes.clone(),
            print: entry.draft_print.clone(),
            rendered: entry.draft_print.as_ref().map(Print::render),
            diagnostics: entry.draft_diagnostics.clone(),
            approved: entry.approved.as_ref().map(|a| ApprovedInfo {
                hashes: a.hashes.clone(),
                rendered: a.print.render(),
                print: a.print.clone(),
            }),
            last_result: entry.last_result.clone(),
            rerun_required: entry.rerun_required,
            run: None,
        }
    }
}

pub fn infos(catalog: &Catalog) -> Vec<RequirementInfo> {
    catalog
        .iter()
        .filter_map(|(id, saved)| Some(RequirementInfo::from_entry(id, saved.requirement()?)))
        .collect()
}

// ---------------------------------------------------------- user-only commands

/// What the user reviewed. Approval refuses unless a fresh compile still
/// produces exactly these hashes, so the user approves what they saw.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveRequest {
    pub workspace_id: String,
    pub expected_catalog: Option<String>,
    pub id: String,
    /// `approved`-shaped hashes of the reviewed print: requirement, manifest
    /// and plan.
    pub hashes: StoredHashes,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteRequest {
    pub workspace_id: String,
    pub expected_catalog: Option<String>,
    pub id: String,
}

/// Reply of the user-only commands: the new catalog fingerprint and list.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CatalogReply {
    pub catalog: String,
    pub project_workspace: String,
    pub requirements: Vec<RequirementInfo>,
}

impl Core {
    async fn load_requirements(
        &self,
        actor: &str,
        workspace_id: &str,
        expected: Option<&str>,
    ) -> Result<(String, Catalog, String)> {
        let (owner, catalog) = self.requirement_catalog(actor, workspace_id).await?;
        let current = fingerprint(&catalog)?;
        if expected != Some(current.as_str()) {
            return Err(Error::domain(
                "REVISION_CONFLICT",
                "Requirements changed; list and review before retrying",
            ));
        }
        Ok((owner, catalog, current))
    }

    /// Replace or add the draft of one requirement (agents and the user).
    /// An approved version is never touched: a changed draft makes the
    /// requirement `needs_review`.
    pub(crate) async fn store_requirement_draft(
        &self,
        actor: &str,
        workspace_id: &str,
        expected: Option<&str>,
        compiled: &Compiled,
    ) -> Result<CatalogReply> {
        let _memory = self.pin_workspace(workspace_id).await?;
        self.ensure_actor_active(actor)?;
        let (owner, mut catalog, current) = self
            .load_requirements(actor, workspace_id, expected)
            .await?;
        let _owner_memory = self.pin_workspace(&owner).await?;
        let id = compiled.requirement.id.clone();
        validate_id(&id)?;
        match catalog.get_mut(&id) {
            Some(Saved::Requirement(entry)) => entry.set_draft(compiled),
            Some(Saved::Test(_)) => {
                return Err(invalid("ID belongs to a motion test, not a requirement"));
            }
            None => {
                catalog.insert(
                    id,
                    Saved::Requirement(Box::new(RequirementEntry::new_draft(compiled))),
                );
            }
        }
        self.commit_requirement_catalog(actor, &owner, workspace_id, &current, &catalog)
            .await?;
        reply(owner, &catalog)
    }

    /// User-only: approve the draft of `request.id`. Compiles against the
    /// current build, refuses on any error diagnostic, refuses if what would
    /// be approved differs from what the user reviewed, and stores the
    /// approved `{IR, manifest, plan, hashes}`. Not reachable from
    /// `tools::dispatch`.
    pub async fn approve_requirement(&self, request: ApproveRequest) -> Result<CatalogReply> {
        let _memory = self.pin_workspace(&request.workspace_id).await?;
        let (owner, mut catalog, current) = self
            .load_requirements(
                USER,
                &request.workspace_id,
                request.expected_catalog.as_deref(),
            )
            .await?;
        let _owner_memory = self.pin_workspace(&owner).await?;
        let draft = catalog
            .get(&request.id)
            .and_then(Saved::requirement)
            .ok_or_else(|| invalid("Unknown requirement"))?
            .draft
            .clone();
        let settings = project_settings();
        let snapshot = snapshot_for(self, &request.workspace_id, USER).await?;
        let compiled = compile(&draft, &snapshot, &settings);
        if compiled.has_errors() {
            let errors: Vec<&Diagnostic> = compiled
                .diagnostics
                .iter()
                .filter(|d| d.is_error())
                .collect();
            let summary = errors
                .iter()
                .take(3)
                .map(|d| format!("{} at {}: {}", d.code, d.at, d.message))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(Error::Domain {
                code: "REQUIREMENT_HAS_ERRORS".into(),
                message: format!("Cannot approve: {} error(s). {summary}", errors.len()),
                details: serde_json::to_value(&compiled.diagnostics)?,
            });
        }
        let (Some(manifest), Some(plan), Some(print), Some(hashes)) = (
            compiled.manifest.clone(),
            compiled.plan.clone(),
            compiled.print.clone(),
            compiled.hashes.as_ref(),
        ) else {
            return Err(invalid("Requirement did not compile to a plan"));
        };
        let hashes = StoredHashes::from(hashes);
        if hashes != request.hashes {
            return Err(Error::domain(
                "REQUIREMENT_CHANGED",
                "The requirement, the model it binds to, or its plan changed since you \
                 reviewed it. Review the updated print and approve again.",
            ));
        }
        let Some(Saved::Requirement(entry)) = catalog.get_mut(&request.id) else {
            return Err(invalid("Unknown requirement"));
        };
        entry.approved = Some(Approved {
            requirement: compiled.requirement.clone(),
            manifest,
            plan,
            print,
            hashes,
            library_version: super::library_version().to_owned(),
            settings_hash: settings.content_hash(),
        });
        entry.set_draft(&compiled);
        // Keep the earlier verdict visible as history until this plan runs.
        entry.rerun_required = entry.last_result.is_some();
        self.commit_requirement_catalog(USER, &owner, &request.workspace_id, &current, &catalog)
            .await?;
        reply(owner, &catalog)
    }

    /// User-only: remove a requirement and its approved version.
    pub async fn delete_requirement(&self, request: DeleteRequest) -> Result<CatalogReply> {
        let _memory = self.pin_workspace(&request.workspace_id).await?;
        let (owner, mut catalog, current) = self
            .load_requirements(
                USER,
                &request.workspace_id,
                request.expected_catalog.as_deref(),
            )
            .await?;
        let _owner_memory = self.pin_workspace(&owner).await?;
        if catalog
            .get(&request.id)
            .and_then(Saved::requirement)
            .is_none()
        {
            return Err(invalid("Unknown requirement"));
        }
        catalog.remove(&request.id);
        self.commit_requirement_catalog(USER, &owner, &request.workspace_id, &current, &catalog)
            .await?;
        reply(owner, &catalog)
    }
}

pub(crate) fn reply(owner: String, catalog: &Catalog) -> Result<CatalogReply> {
    Ok(CatalogReply {
        catalog: fingerprint(catalog)?,
        project_workspace: owner,
        requirements: infos(catalog),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion_tests::{encode, load};
    use crate::requirements::WorkspaceSnapshot;
    use std::collections::BTreeMap;

    fn compiled() -> Compiled {
        let req: Requirement =
            serde_json::from_str(include_str!("../../tests/fixtures/requirements/R1.json"))
                .unwrap();
        let snap: WorkspaceSnapshot = serde_json::from_str(include_str!(
            "../../tests/fixtures/requirements/snapshots/R1.json"
        ))
        .unwrap();
        let c = compile(&req, &snap, &project_settings());
        assert!(!c.has_errors(), "{:?}", c.diagnostics);
        c
    }

    fn approve(entry: &mut RequirementEntry, c: &Compiled) {
        entry.approved = Some(Approved {
            requirement: c.requirement.clone(),
            manifest: c.manifest.clone().unwrap(),
            plan: c.plan.clone().unwrap(),
            print: c.print.clone().unwrap(),
            hashes: c.hashes.as_ref().unwrap().into(),
            library_version: super::super::library_version().to_owned(),
            settings_hash: project_settings().content_hash(),
        });
    }

    #[test]
    fn prose_edit_after_approval_keeps_the_approved_version_and_needs_review() {
        let c = compiled();
        let mut entry = RequirementEntry::new_draft(&c);
        assert_eq!(entry.status(), Some(RequirementStatus::Draft));
        assert_eq!(entry.approval(), Approval::Draft);
        approve(&mut entry, &c);
        assert_eq!(entry.approval(), Approval::Approved);
        assert_eq!(entry.status(), None);
        entry.last_result = Some(serde_json::json!({"status": "met"}));
        assert_eq!(entry.status(), Some(RequirementStatus::Met));

        let mut edited = c.clone();
        edited.requirement.text.push_str(" and more");
        entry.set_draft(&edited);
        assert_eq!(entry.approval(), Approval::EditedSinceApproval);
        assert_eq!(entry.status(), Some(RequirementStatus::NeedsReview));
        // The approved version and its result are untouched.
        let approved = entry.approved.as_ref().unwrap();
        assert_eq!(approved.requirement, c.requirement);
        assert_eq!(
            entry.last_result,
            Some(serde_json::json!({"status": "met"}))
        );
        // Restoring the approved text makes it current again.
        entry.set_draft(&c);
        assert_eq!(entry.approval(), Approval::Approved);
    }

    #[test]
    fn catalog_mixes_tests_and_requirements_and_imports_for_rerun() {
        let c = compiled();
        let id = c.requirement.id.clone();
        let mut entry = RequirementEntry::new_draft(&c);
        approve(&mut entry, &c);
        entry.last_result = Some(serde_json::json!({"status": "met"}));
        let test: crate::motion_tests::SavedTest = serde_json::from_value(serde_json::json!({
            "definition": {"name":"Slider","intent":"x","initial_coordinates":{"slide":0},
              "simulation":{"gravity_m_s2":[0,0,0],"bodies":{"block":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":1},"friction":0,"restitution":0}}},
              "actions":[{"ticks":30,"motors":[],"loads":{}}],
              "checks":[{"type":"displacement","body":"block","direction":[1,0,0],"minimum_mm":0.1}]},
            "last_run": null})).unwrap();
        let mut catalog = BTreeMap::new();
        catalog.insert("t".to_owned(), Saved::Test(test));
        catalog.insert(id.clone(), Saved::Requirement(Box::new(entry)));
        let bytes = encode(&catalog).unwrap();
        // The archive is the shared catalog file; load marks results for re-run.
        let dir = tempfile::tempdir().unwrap();
        let digest = crate::portable::bytes_hash(&bytes);
        std::fs::create_dir(dir.path().join("motion-tests")).unwrap();
        std::fs::write(
            dir.path().join(format!("motion-tests/{digest}.json")),
            &bytes,
        )
        .unwrap();
        let loaded = load(dir.path(), Some(&digest)).unwrap();
        assert!(loaded["t"].test().is_some());
        let entry = loaded[&id].requirement().unwrap();
        assert!(entry.rerun_required);
        assert!(entry.last_result.is_some());
        assert!(entry.approved.is_some());
        assert_eq!(infos(&loaded).len(), 1);
        // Test entries still serialize without any requirement fields.
        let value = serde_json::to_value(&catalog).unwrap();
        assert!(value["t"].get("kind").is_none());
        assert_eq!(value[&id]["kind"], "requirement");
    }

    #[test]
    fn limits_give_clear_errors() {
        let c = compiled();
        let mut catalog = BTreeMap::new();
        for i in 0..=MAX_REQUIREMENTS {
            let mut one = c.clone();
            one.requirement.id = format!("r{i}");
            catalog.insert(
                format!("r{i}"),
                Saved::Requirement(Box::new(RequirementEntry::new_draft(&one))),
            );
        }
        let err = encode(&catalog).unwrap_err().to_string();
        assert!(err.contains("At most 64 requirements"), "{err}");
        catalog.remove("r0");
        encode(&catalog).unwrap();

        let mut big = c.clone();
        big.requirement.text = "x".repeat(MAX_ENTRY_BYTES);
        let err = RequirementEntry::new_draft(&big)
            .validate(&big.requirement.id)
            .unwrap_err()
            .to_string();
        assert!(err.contains("limited to 1024 KiB"), "{err}");
        // A key that does not match the IR id is refused.
        let err = RequirementEntry::new_draft(&c)
            .validate("other")
            .unwrap_err();
        assert_eq!(err.code(), "INVALID_REQUIREMENT");
    }
}
