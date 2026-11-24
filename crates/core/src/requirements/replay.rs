//! Read-only preview of a recorded requirement state.
use super::dispatch::ModelView;
use super::evaluate::ResultBody;
use super::ir::ModelRef;
use super::states::ConcreteState;
use super::worker_plan::{split_part_key, worker_state};
use crate::motion_tests::Saved;
use crate::{Core, Error, JobKind, JobState, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_REQUIREMENT_REPLAY", message)
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMode {
    Rest,
    Rigid,
    Parameter,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplayRequest {
    pub requirement_id: String,
    pub primitive_id: String,
    pub state: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RequirementReplay {
    pub requirement_id: String,
    pub primitive_id: String,
    pub state_index: usize,
    pub model: ModelRef,
    pub state: ConcreteState,
    pub mode: ReplayMode,
    pub view: ReplayView,
    /// Ordered world-space moves, exactly as sent to the evaluator.
    pub moves: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReplayView {
    pub workspace: String,
    pub revision: String,
    pub temporary: bool,
}

impl Core {
    pub async fn replay_requirement_state(
        &self,
        actor: &str,
        workspace: &str,
        requirement_id: &str,
        primitive_id: &str,
        state_index: usize,
    ) -> Result<RequirementReplay> {
        self.ensure_actor_active(actor)?;
        let cancel = self.actor_signal(actor);
        let (_, prepared, result) = self
            .prepare_requirement(actor, workspace, requirement_id, &cancel)
            .await?;
        let result = result
            .filter(|result| result["ran"] == true)
            .ok_or_else(|| invalid("The requirement has no current result to replay"))?;
        let run = prepared
            .run
            .as_ref()
            .ok_or_else(|| invalid("The requirement needs review before replay"))?;
        let body: ResultBody = serde_json::from_value(result["body"].clone())
            .map_err(|_| invalid("The saved result has no replayable measurements"))?;
        if body.plan_hash != run.plan.content_hash()
            || json!(prepared.subjects) != result["subjects"]
        {
            return Err(invalid(
                "The model changed after this result. Check the requirement again before replay.",
            ));
        }
        let primitive = body
            .primitives
            .iter()
            .find(|primitive| primitive.id == primitive_id)
            .ok_or_else(|| invalid("The requested check is not in the saved result"))?;
        let pair = primitive
            .worst
            .as_ref()
            .filter(|worst| worst.state == state_index)
            .map(|worst| (&worst.pair.a, &worst.pair.b))
            .or_else(|| {
                body.findings
                    .iter()
                    .find(|finding| {
                        finding.claim == primitive_id
                            && finding
                                .states
                                .iter()
                                .any(|state| state.index == state_index)
                    })
                    .map(|finding| (&finding.a, &finding.b))
            })
            .ok_or_else(|| invalid("This state is not a saved failure or worst result"))?;
        let (key, _) = pair
            .0
            .iter()
            .chain(pair.1)
            .find_map(|key| split_part_key(key))
            .ok_or_else(|| invalid("The saved state has no model reference"))?;
        let source = prepared
            .views
            .get(key)
            .ok_or_else(|| invalid("The model for this state could not be built"))?;
        let model = run
            .manifest
            .subjects
            .iter()
            .map(|subject| &subject.model)
            .find(|model| super::bind::model_key(model) == key)
            .cloned()
            .ok_or_else(|| invalid("The model is no longer a subject of the check"))?;
        let state = run
            .manifest
            .states
            .get(primitive_id)
            .and_then(|states| states.get(state_index))
            .cloned()
            .ok_or_else(|| invalid("The state is outside the evaluated plan"))?;
        let sample = worker_state(
            primitive_id,
            &state,
            &run.manifest,
            &mut Some(key.to_owned()),
        )
        .map_err(|error| invalid(error.to_string()))?;
        let (_, catalog) = self.requirement_catalog(actor, workspace).await?;
        let current = catalog.get(requirement_id).and_then(Saved::requirement);
        if !current.is_some_and(|entry| {
            !entry.rerun_required
                && entry
                    .approved
                    .as_ref()
                    .is_some_and(|approved| approved.hashes == prepared.approved.hashes)
                && entry.last_result.as_ref().is_some_and(|now| {
                    now["ran"] == true
                        && now["result_hash"] == result["result_hash"]
                        && now["subjects"] == result["subjects"]
                })
        }) {
            return Err(invalid(
                "The requirement changed. Open its latest result to replay.",
            ));
        }
        let (mode, view) = if sample.params.is_empty() {
            let mode = if sample.moves.is_empty() {
                ReplayMode::Rest
            } else {
                ReplayMode::Rigid
            };
            (
                mode,
                ReplayView {
                    workspace: source.workspace.clone(),
                    revision: source.revision.clone(),
                    temporary: false,
                },
            )
        } else {
            (
                ReplayMode::Parameter,
                self.build_replay_view(actor, source, sample.params).await?,
            )
        };
        Ok(RequirementReplay {
            requirement_id: requirement_id.to_owned(),
            primitive_id: primitive_id.to_owned(),
            state_index,
            model,
            state,
            mode,
            view,
            moves: sample.moves,
        })
    }

    async fn build_replay_view(
        &self,
        actor: &str,
        source: &ModelView,
        overrides: BTreeMap<String, f64>,
    ) -> Result<ReplayView> {
        let _source_memory = self.pin_workspace(&source.workspace).await?;
        let _assets = self.blob_publication.read().await;
        let mut inputs = {
            let all = self.workspace(&source.workspace).await?;
            let stored = &all[&source.workspace].stored;
            if stored.current != source.revision {
                return Err(invalid("The model changed before replay could open"));
            }
            stored.current_revision().inputs.clone()
        };
        let params = inputs
            .parameters
            .as_object_mut()
            .ok_or_else(|| invalid("The model has invalid parameter overrides"))?;
        params.extend(
            overrides
                .into_iter()
                .map(|(name, value)| (name, json!(value))),
        );

        let workspace = self.create_workspace_with_lifetime(actor, true).await?;
        let result = async {
            let (base, draft_id) = {
                let mut all = self.workspace(&workspace).await?;
                let ws = all
                    .get_mut(&workspace)
                    .ok_or_else(|| invalid("Preview workspace closed"))?;
                let mut next = ws.stored.clone();
                let base = next.current.clone();
                let draft = Self::input_draft(inputs, &base, actor);
                let draft_id = draft.id.clone();
                next.drafts.insert(draft_id.clone(), draft);
                self.persist(&next).await?;
                ws.stored = next;
                (base, draft_id)
            };
            drop(_assets);
            let job = self
                .queue_job(&workspace, &base, &draft_id, actor, JobKind::Build, false)
                .await?;
            let revision = tokio::time::timeout(Duration::from_secs(120), async {
                loop {
                    let job = self.get_job(&workspace, &job.id).await?;
                    match job.state {
                        JobState::Succeeded => {
                            return job.resulting_revision.ok_or_else(|| {
                                invalid("Preview build returned no geometry revision")
                            });
                        }
                        JobState::Failed | JobState::Cancelled => {
                            return Err(invalid(
                                job.diagnostics["message"]
                                    .as_str()
                                    .unwrap_or("Preview build failed"),
                            ));
                        }
                        JobState::Queued | JobState::Running => {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    }
                }
            })
            .await
            .map_err(|_| invalid("Preview build exceeded 120 seconds"))??;
            Ok(ReplayView {
                workspace: workspace.clone(),
                revision,
                temporary: true,
            })
        }
        .await;
        if result.is_err() {
            let _ = self.forget_workspace(&workspace).await;
        }
        result
    }

    /// Release an isolated replay workspace when its viewer closes.
    pub async fn close_requirement_replay(&self, workspace: &str) -> Result<()> {
        let disposable = {
            let all = self.workspaces.lock().await;
            all.get(workspace)
                .is_some_and(|ws| ws.stored.ephemeral && ws.stored.model_parent.is_none())
        };
        if disposable {
            self.forget_workspace(workspace).await?;
        }
        Ok(())
    }
}
