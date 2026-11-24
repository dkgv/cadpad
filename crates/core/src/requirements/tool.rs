//! The `control_requirements` agent tool (spec §8): `list`, `compile`,
//! `save`, `library`, `run`, `start_run`, `cancel_run`. There is deliberately
//! no `approve` command: approval is a user action
//! ([`Core::approve_requirement`]), which `tools::dispatch` cannot reach.
use super::compile::{Compiled, compile};
use super::diagnostics::Diagnostic;
use super::ir::{ModelRef, Requirement};
use super::library::{library, library_version, vague_table};
use super::print::Print;
use super::provenance::Downgrade;
use super::snapshot::{WorkspaceSnapshot, snapshot_from_values};
use super::storage::{RequirementInfo, StoredHashes, project_settings, reply as catalog};
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
    Compile { ir: Box<Requirement> },
    /// Compile and store as a draft (needs expected_catalog from list). The
    /// user approves drafts; an approved requirement is never changed.
    Save { ir: Box<Requirement> },
    /// Rules, primitives, defaults, print templates and the vague-word table.
    Library,
    /// Evaluate an approved requirement and wait for the result.
    Run { id: String },
    /// Start an evaluation and return immediately.
    StartRun { id: String },
    CancelRun { run_id: String },
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

/// Evaluate an approved requirement. The evaluation layer is built
/// separately; this is the single seam it plugs into.
// TODO: replace the body with the evaluator (requirements::dispatch) once it
// lands: relink against a fresh build, diff, run the plan, store `last_result`.
pub(crate) async fn run_requirement(_core: &Core, _project: &str, id: &str) -> Result<Value> {
    Err(Error::domain(
        "REQUIREMENT_EVALUATION_UNAVAILABLE",
        format!("Cannot run requirement '{id}': evaluation is not wired yet in this build"),
    ))
}

fn cancel_unavailable(run_id: &str) -> Error {
    Error::domain(
        "REQUIREMENT_EVALUATION_UNAVAILABLE",
        format!("Cannot cancel run '{run_id}': evaluation is not wired yet in this build"),
    )
}

/// A workspace snapshot of the project's models for binding: the project's
/// source catalog plus every open model workspace that has built. Models with
/// no open, built workspace are listed as not built (and bind as such).
// TODO: build models that have no open workspace (needs the run lifecycle's
// worker admission) instead of reporting them not built.
pub(crate) async fn snapshot_for(core: &Core, workspace_id: &str) -> Result<WorkspaceSnapshot> {
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
    for id in candidates.into_iter().filter(|id| seen.insert(id.clone())).take(32) {
        let Ok(project) = core.get_project(&id).await else {
            continue;
        };
        let entry = &project["model_entry"];
        let (Some(entry_path), Some(builder)) = (entry["entry"].as_str(), entry["builder"].as_str())
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
    Ok(snapshot_from_values(&catalog, &builds))
}

fn library_value() -> Value {
    json!({
        "library_version": library_version(),
        "rules": library().rules,
        "primitives": library().primitives,
        "settings_defaults": project_settings(),
        "vague_words": vague_table(),
    })
}

impl Core {
    pub async fn control_requirements(&self, actor: &str, args: Control) -> Result<Reply> {
        self.ensure_actor_active(actor)?;
        let workspace_id = args.workspace_id.as_str();
        let _memory = self.pin_workspace(workspace_id).await?;
        let (owner, current) = self.requirement_catalog(actor, workspace_id).await?;
        let done = |compiled: Option<CompileReport>, library: Option<Value>, run: Option<Value>| {
            let base = catalog(owner.clone(), &current)?;
            Ok(Reply {
                catalog: base.catalog,
                project_workspace: base.project_workspace,
                requirements: base.requirements,
                compiled,
                library,
                run,
            })
        };
        match args.command {
            Command::List => done(None, None, None),
            Command::Library => done(None, Some(library_value()), None),
            Command::Compile { ir } => {
                let snapshot = snapshot_for(self, workspace_id).await?;
                let compiled = compile(&ir, &snapshot, &project_settings());
                done(Some(CompileReport::new(compiled)), None, None)
            }
            Command::Save { ir } => {
                if args.expected_catalog.is_none() {
                    return Err(Error::domain(
                        "REVISION_CONFLICT",
                        "Save needs expected_catalog from list",
                    ));
                }
                let snapshot = snapshot_for(self, workspace_id).await?;
                let compiled = compile(&ir, &snapshot, &project_settings());
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
                    project_workspace: stored.project_workspace,
                    requirements: stored.requirements,
                    compiled: Some(CompileReport::new(compiled)),
                    library: None,
                    run: None,
                })
            }
            Command::Run { id } | Command::StartRun { id } => {
                let entry = current
                    .get(&id)
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
                let run = run_requirement(self, &owner, &id).await?;
                done(None, None, Some(run))
            }
            Command::CancelRun { run_id } => Err(cancel_unavailable(&run_id)),
        }
    }
}
