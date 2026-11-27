//! The `control_requirements` agent tool (spec §8): `list`, `compile`,
//! `save`, `library`, `run`, `start_run`, `cancel_run`. There is deliberately
//! no `approve` command: approval is a user action
//! ([`Core::approve_requirement`]), which `tools::dispatch` cannot reach.
use super::compile::{Compiled, compile};
use super::diagnostics::{Diagnostic, DiagnosticCode};
use super::dispatch::LiveSnapshot;
use super::ir::{FeatureRef, ModelRef, PartName, Requirement};
use super::library::{library, library_version, vague_table};
use super::print::Print;
use super::provenance::Downgrade;
use super::run::RunInfo;
use super::settings::ProjectSettings;
use super::snapshot::{WorkspaceSnapshot, snapshot_from_values};
use super::storage::{Approval, RequirementInfo, StoredHashes, project_settings, reply as catalog};
use super::suggestions::ShapeSignature;
use crate::motion_tests::Saved;
use crate::{Core, Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// Catalog with statuses, prints, diagnostics and results.
    List,
    /// Full compile of one IR against the current builds. Saves nothing.
    Compile {
        ir: Box<Requirement>,
    },
    /// Compile and store as a draft (needs expected_catalog from list). The
    /// user approves drafts; an approved requirement is never changed.
    Save {
        ir: Box<Requirement>,
    },
    /// Rules, primitives, defaults, print templates and the vague-word table.
    Library,
    /// The IR JSON schema and command actions, for clients that hide this
    /// tool's input schema.
    Schema,
    /// Evaluate an approved requirement and wait for the result.
    Run {
        id: String,
    },
    /// Start an evaluation and return immediately.
    StartRun {
        id: String,
    },
    CancelRun {
        run_id: String,
    },
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Control {
    /// Any workspace of the project: a model's workspace or the project's.
    /// Requirements belong to the project.
    pub workspace_id: String,
    /// Returned by list. Required by save to prevent lost updates.
    #[serde(default)]
    pub expected_catalog: Option<String>,
    pub command: Command,
}

/// Result of one compile (`compile` and `save`).
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CompileReport {
    /// No error diagnostics: the requirement can be reviewed and approved.
    pub ok: bool,
    /// The IR after provenance checks; this is what gets hashed and printed.
    pub requirement: Requirement,
    pub diagnostics: Vec<Diagnostic>,
    /// Values whose claimed source did not check out and are now `inferred`.
    pub downgrades: Vec<Downgrade>,
    /// The canonical English the user reviews. Explain a requirement only from
    /// this and from results.
    pub print: Option<Print>,
    pub rendered: Option<String>,
    pub hashes: Option<StoredHashes>,
    /// Number of checks the plan will run.
    pub planned_jobs: Option<usize>,
}

impl CompileReport {
    fn new(c: Compiled) -> Self {
        Self {
            ok: !c.has_errors(),
            rendered: c.print.as_ref().map(Print::render),
            planned_jobs: c.plan.as_ref().map(|p| p.job_count()),
            hashes: c.hashes.as_ref().map(StoredHashes::from),
            requirement: c.requirement,
            diagnostics: c.diagnostics,
            downgrades: c.downgrades,
            print: c.print,
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Reply {
    /// Catalog fingerprint; pass back as expected_catalog.
    pub catalog: String,
    pub settings: ProjectSettings,
    /// Workspace that stores the project's catalog.
    pub project_workspace: String,
    pub requirements: Vec<RequirementInfo>,
    /// Present for compile and save.
    pub compiled: Option<CompileReport>,
    /// Present for library.
    pub library: Option<Value>,
    /// Present for run and start_run.
    pub run: Option<Value>,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_REQUIREMENT", message)
}

/// A workspace snapshot of the project's models for binding: the project's
/// source catalog plus every open model workspace that has built. Models with
/// no open, built workspace are listed as not built (and bind as such).
pub(crate) async fn snapshot_for(
    core: &Core,
    workspace_id: &str,
    actor: &str,
    requirement: &Requirement,
) -> Result<WorkspaceSnapshot> {
    // Bind against what the models build to now, from the project's sources.
    // A workspace that is not a project (or has no models) falls back to the
    // open model workspaces below.
    let referenced = requirement_models(requirement);
    if let Ok(mut live) = core
        .live_snapshot(workspace_id, actor, Some(&referenced))
        .await
        && !live.snapshot.models.is_empty()
    {
        hydrate_history_features(core, actor, &mut live, requirement).await?;
        attach_reference_signatures(core, workspace_id, requirement, &mut live.snapshot).await;
        return Ok(live.snapshot);
    }
    let owner = core.requirement_owner(workspace_id).await?;
    let revision = {
        let all = core.workspace(&owner).await?;
        all.get(&owner)
            .ok_or_else(|| invalid("Unknown workspace"))?
            .stored
            .current
            .clone()
    };
    let catalog = core
        .project_source_catalog(&owner, &revision, None)
        .await
        .unwrap_or_else(|_| json!({"files": []}));
    let mut candidates = vec![workspace_id.to_owned(), owner.clone()];
    for id in core.list_workspaces().await {
        if core.model_parent(&id).await.as_deref() == Some(owner.as_str()) {
            candidates.push(id);
        }
    }
    candidates.dedup();
    let mut seen = std::collections::BTreeSet::new();
    let mut builds: BTreeMap<ModelRef, std::result::Result<Value, String>> = BTreeMap::new();
    for id in candidates
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .take(32)
    {
        let Ok(project) = core.get_project(&id).await else {
            continue;
        };
        let entry = &project["model_entry"];
        let (Some(entry_path), Some(builder)) =
            (entry["entry"].as_str(), entry["builder"].as_str())
        else {
            continue;
        };
        let key = ModelRef {
            entry: entry_path.to_owned(),
            builder: builder.to_owned(),
        };
        let built = if project["model"]["rebuild_required"] == true {
            Err("The model has changes that are not built yet".to_owned())
        } else {
            Ok(project)
        };
        builds.entry(key).or_insert(built);
    }
    let mut snap = snapshot_from_values(&catalog, &builds);
    attach_reference_signatures(core, workspace_id, requirement, &mut snap).await;
    Ok(snap)
}

/// Give the snapshot the shape signatures this requirement's approved
/// manifest recorded, so `bind` can rank signature candidates for broken
/// references (spec §3.1). Drafts and requirements never approved get none.
/// Reads the stored catalog directly: `requirement_catalog` migrates motion
/// tests, and migration itself compiles through `snapshot_for`.
async fn attach_reference_signatures(
    core: &Core,
    workspace_id: &str,
    requirement: &Requirement,
    snap: &mut WorkspaceSnapshot,
) {
    let Ok(owner) = core.requirement_owner(workspace_id).await else {
        return;
    };
    let Ok(all) = core.workspace(&owner).await else {
        return;
    };
    let signatures = all
        .get(&owner)
        .and_then(|ws| ws.stored.motion_tests.get(&requirement.id))
        .and_then(Saved::requirement)
        .and_then(|entry| entry.approved.as_ref())
        .map(|approved| approved.manifest.signature_map())
        .unwrap_or_default();
    snap.attach_reference_signatures(&signatures);
}

pub(super) async fn hydrate_history_features(
    core: &Core,
    actor: &str,
    live: &mut LiveSnapshot,
    requirement: &Requirement,
) -> Result<()> {
    let mut refs: BTreeMap<ModelRef, Vec<(String, Value)>> = BTreeMap::new();
    super::refs::walk(requirement, &mut |site| {
        let super::refs::Ref::Feature(feature) = site.reference else {
            return;
        };
        let (part, query) = match feature {
            FeatureRef::History { part, name, .. } => {
                (part, json!({"kind":"history", "name":name}))
            }
            FeatureRef::Geometric { part, entity, hash } => (
                part,
                json!({"kind":"geometric", "entity":entity, "hash":hash}),
            ),
            FeatureRef::Explicit { .. } => return,
        };
        let PartName::Name(part_name) = &part.part else {
            return;
        };
        let mut query = query;
        query["part"] = json!(part_name);
        refs.entry(part.model.clone())
            .or_default()
            .push((part_name.clone(), query));
    });
    for (model, references) in refs {
        let Some(view) = live.views.get(&model) else {
            continue;
        };
        let response = core
            .query_as_actor(
                actor,
                &view.workspace,
                &view.revision,
                "resolve_features",
                json!({"references": references.iter().map(|(_, r)| r).collect::<Vec<_>>()}),
            )
            .await?;
        let Some(rows) = response["references"].as_array() else {
            continue;
        };
        let Some(built) = live
            .snapshot
            .models
            .iter_mut()
            .find(|m| m.model == model)
            .and_then(|m| m.built.as_mut())
        else {
            continue;
        };
        for ((part, request), resolved) in references.iter().zip(rows) {
            if resolved["status"] != "resolved" && resolved["status"] != "ambiguous" {
                continue;
            }
            let kind = match resolved["entity"].as_str() {
                Some("edge") => super::ir::EntityKind::Edge,
                Some("face") => super::ir::EntityKind::Face,
                _ => continue,
            };
            let history = request["kind"] == "history";
            let name = if history {
                request["name"].as_str().unwrap_or_default().to_owned()
            } else {
                format!(
                    "geometric:{}:{}",
                    request["entity"].as_str().unwrap_or(""),
                    request["hash"].as_str().unwrap_or("")
                )
            };
            let positional = resolved["positional"].as_bool().unwrap_or(false);
            let source = if history {
                super::snapshot::FeatureSource::History
            } else {
                super::snapshot::FeatureSource::Geometric
            };
            let count = resolved["count"].as_u64().unwrap_or(0);
            let info = super::snapshot::FeatureInfo {
                part: part.clone(),
                name,
                entity: kind,
                count,
                hash: resolved["hash"].as_str().unwrap_or_default().to_owned(),
                source,
                positional,
            };
            if built.features.iter().any(|f| {
                f.part == info.part
                    && f.name == info.name
                    && f.source == info.source
                    && f.hash == info.hash
            }) {
                continue;
            }
            built.feature_signatures.insert(
                format!("{part}#{}", info.link_suffix()),
                ShapeSignature::entity_count(kind, count),
            );
            built.features.push(info);
        }
        for (part, entries) in response["history_index"].as_object().into_iter().flatten() {
            for item in entries.as_array().into_iter().flatten() {
                let entity = match item["entity"].as_str() {
                    Some("edge") => super::ir::EntityKind::Edge,
                    Some("face") => super::ir::EntityKind::Face,
                    _ => continue,
                };
                let Some(name) = item["name"].as_str() else {
                    continue;
                };
                let hash = item["hash"].as_str().unwrap_or_default().to_owned();
                if built.features.iter().any(|f| {
                    f.part == *part
                        && f.name == name
                        && f.source == super::snapshot::FeatureSource::History
                }) {
                    continue;
                }
                let count = item["count"].as_u64().unwrap_or(0);
                let info = super::snapshot::FeatureInfo {
                    part: part.to_owned(),
                    name: name.to_owned(),
                    entity,
                    count,
                    hash,
                    source: super::snapshot::FeatureSource::History,
                    positional: item["positional"].as_bool().unwrap_or(false),
                };
                built.feature_signatures.insert(
                    format!("{part}#{}", info.link_suffix()),
                    ShapeSignature::entity_count(entity, count),
                );
                built.features.push(info);
            }
        }
    }
    Ok(())
}

fn library_value(settings: &ProjectSettings) -> Value {
    json!({
        "library_version": library_version(),
        "ir_schema": super::ir_schema(),
        "rules": library().rules,
        "primitives": library().primitives,
        "settings_defaults": project_settings(),
        "settings": settings,
        "vague_words": vague_table(),
    })
}

/// Models an IR can bind against: its text chips and every reference in its
/// sets, covers, claims and allowances. Building only these keeps compile
/// responsive in projects with many models.
fn requirement_models(requirement: &Requirement) -> Vec<ModelRef> {
    let mut models = std::collections::BTreeSet::new();
    for chip in super::bind::parse_chips(&requirement.text) {
        models.insert(chip.model);
    }
    super::refs::walk(requirement, &mut |site| match site.reference {
        super::refs::Ref::Model(model) => {
            models.insert(model.clone());
        }
        super::refs::Ref::Part(part) => {
            models.insert(part.model.clone());
        }
        super::refs::Ref::Param(param) => {
            if let Some(model) = &param.model {
                models.insert(model.clone());
            }
        }
        super::refs::Ref::Joint(joint) => {
            models.insert(joint.model.clone());
        }
        super::refs::Ref::Feature(feature) => {
            let part = match feature {
                FeatureRef::Explicit { part, .. }
                | FeatureRef::History { part, .. }
                | FeatureRef::Geometric { part, .. } => part,
            };
            models.insert(part.model.clone());
        }
        // Set members and covers are visited separately by `walk`.
        super::refs::Ref::Set(_) | super::refs::Ref::Others => {}
    });
    models.into_iter().collect()
}

/// Put the project's run state on the requirement it belongs to.
fn with_runs(mut requirements: Vec<RequirementInfo>, run: Option<RunInfo>) -> Vec<RequirementInfo> {
    if let Some(run) = run
        && let Some(r) = requirements.iter_mut().find(|r| r.id == run.requirement_id)
    {
        r.run = Some(run);
    }
    requirements
}

impl Core {
    fn requirement_reply(
        &self,
        owner: &str,
        catalog_now: &crate::motion_tests::Catalog,
        compiled: Option<CompileReport>,
        library: Option<Value>,
        run: Option<Value>,
    ) -> Result<Reply> {
        let base = catalog(owner.to_owned(), catalog_now)?;
        Ok(Reply {
            catalog: base.catalog,
            settings: super::storage::settings_in(catalog_now),
            project_workspace: base.project_workspace,
            requirements: with_runs(base.requirements, self.requirement_run_status(owner)),
            compiled,
            library,
            run,
        })
    }

    async fn run_command(
        &self,
        actor: &str,
        workspace_id: &str,
        current: &crate::motion_tests::Catalog,
        id: &str,
        background: bool,
    ) -> Result<Reply> {
        let entry = current
            .get(id)
            .and_then(Saved::requirement)
            .ok_or_else(|| invalid("Unknown requirement"))?;
        if entry.approved.is_none() {
            return Err(Error::domain(
                "REQUIREMENT_NOT_APPROVED",
                format!(
                    "Requirement '{id}' is a draft. Only the user can approve it; \
                     ask them to review and approve it, then run it."
                ),
            ));
        }
        let run = self
            .requirement_run(actor, workspace_id, id, background)
            .await?;
        // Reply with the catalog as it is after the run.
        let (owner, after) = self.requirement_catalog(actor, workspace_id).await?;
        self.requirement_reply(&owner, &after, None, None, Some(run))
    }

    pub async fn control_requirements(&self, actor: &str, args: Control) -> Result<Reply> {
        self.ensure_actor_active(actor)?;
        let workspace_id = args.workspace_id.as_str();
        let _memory = self.pin_workspace(workspace_id).await?;
        let (owner, current) = self.requirement_catalog(actor, workspace_id).await?;
        match args.command {
            Command::List => {
                let settings = super::storage::settings_in(&current);
                let mut expected = crate::motion_tests::fingerprint(&current)?;
                let mut refreshed = false;
                for entry in current.values().filter_map(Saved::requirement) {
                    if entry.approval() == Approval::Approved
                        || !entry
                            .draft_diagnostics
                            .iter()
                            .any(|d| d.code == DiagnosticCode::EBuild)
                    {
                        continue;
                    }
                    if let Ok(snapshot) =
                        snapshot_for(self, workspace_id, actor, &entry.draft).await
                    {
                        let compiled = compile(&entry.draft, &snapshot, &settings);
                        let next_hashes = compiled.hashes.as_ref().map(StoredHashes::from);
                        if compiled.diagnostics != entry.draft_diagnostics
                            || next_hashes != entry.draft_hashes
                        {
                            let stored = self
                                .store_requirement_draft(
                                    actor,
                                    workspace_id,
                                    Some(&expected),
                                    &compiled,
                                )
                                .await?;
                            expected = stored.catalog;
                            refreshed = true;
                        }
                    }
                }
                let (owner, latest) = if refreshed {
                    self.requirement_catalog(actor, workspace_id).await?
                } else {
                    (owner, current)
                };
                let run = self
                    .requirement_run_status(&owner)
                    .filter(|r| r.state.active())
                    .map(|r| json!(r));
                self.requirement_reply(&owner, &latest, None, None, run)
            }
            Command::Library => self.requirement_reply(
                &owner,
                &current,
                None,
                Some(library_value(&super::storage::settings_in(&current))),
                None,
            ),
            Command::Schema => self.requirement_reply(
                &owner,
                &current,
                None,
                Some(json!({
                    "ir_version": super::IR_VERSION,
                    "ir_schema": super::ir_schema(),
                    "actions": [
                        "list",
                        "schema",
                        "compile",
                        "save",
                        "library",
                        "run",
                        "start_run",
                        "cancel_run",
                    ],
                })),
                None,
            ),
            Command::Compile { ir } => {
                let snapshot = snapshot_for(self, workspace_id, actor, &ir).await?;
                let compiled = compile(&ir, &snapshot, &super::storage::settings_in(&current));
                let report = CompileReport::new(compiled);
                self.requirement_reply(&owner, &current, Some(report), None, None)
            }
            Command::Save { ir } => {
                if args.expected_catalog.is_none() {
                    return Err(Error::domain(
                        "REVISION_CONFLICT",
                        "Save needs expected_catalog from list",
                    ));
                }
                let snapshot = snapshot_for(self, workspace_id, actor, &ir).await?;
                let compiled = compile(&ir, &snapshot, &super::storage::settings_in(&current));
                let stored = self
                    .store_requirement_draft(
                        actor,
                        workspace_id,
                        args.expected_catalog.as_deref(),
                        &compiled,
                    )
                    .await?;
                Ok(Reply {
                    catalog: stored.catalog,
                    settings: super::storage::settings_in(&current),
                    project_workspace: stored.project_workspace,
                    requirements: with_runs(
                        stored.requirements,
                        self.requirement_run_status(&owner),
                    ),
                    compiled: Some(CompileReport::new(compiled)),
                    library: None,
                    run: None,
                })
            }
            Command::Run { id } => {
                self.run_command(actor, workspace_id, &current, &id, false)
                    .await
            }
            Command::StartRun { id } => {
                self.run_command(actor, workspace_id, &current, &id, true)
                    .await
            }
            Command::CancelRun { run_id } => {
                let status = self
                    .requirement_cancel(actor, workspace_id, &run_id)
                    .await?;
                self.requirement_reply(&owner, &current, None, None, status.map(|r| json!(r)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> ModelRef {
        ModelRef {
            entry: "machine.py".into(),
            builder: "machine".into(),
        }
    }

    #[test]
    fn requirement_models_covers_sets_and_claims() {
        let requirement: Requirement =
            serde_json::from_str(include_str!("../../tests/fixtures/requirements/R1.json"))
                .unwrap();
        assert_eq!(requirement_models(&requirement), vec![machine()]);
    }

    #[test]
    fn requirement_models_keeps_text_chips_without_claims() {
        let mut requirement: Requirement =
            serde_json::from_str(include_str!("../../tests/fixtures/requirements/R1.json"))
                .unwrap();
        requirement.sets.clear();
        requirement.claims.clear();
        requirement.text = "@[Frame](parts/frame.py#frame) attaches".into();
        assert_eq!(
            requirement_models(&requirement),
            vec![ModelRef {
                entry: "parts/frame.py".into(),
                builder: "frame".into(),
            }]
        );
    }
}
