//! Bound candidate/recovery construction and display repair across one core.
//! Warm committed interpreters and exact queries have separate lifetimes.
use crate::{Core, Error, Result};
use tokio::sync::{OwnedSemaphorePermit, watch};

pub(crate) const MAX_CONSTRUCTIONS: usize = 2;
pub(crate) const MAX_OUTSTANDING_JOBS: usize = 32;
pub(crate) const MAX_ACTOR_JOBS: usize = 8;
pub(crate) const MAX_WORKSPACE_JOBS: usize = 8;

/// Called under the same workspace lock that publishes the accepted job. The
/// cancellation registry contains exactly accepted, unfinished jobs; completed
/// history never needs scanning and cannot consume admission capacity.
pub(crate) fn check_job_capacity(
    workspaces: &std::collections::HashMap<String, crate::Workspace>,
    workspace: &str,
    actor: &str,
) -> Result<()> {
    let mut total = 0;
    let mut per_workspace = 0;
    let mut per_actor = 0;
    for (id, ws) in workspaces {
        for job_id in ws.cancellation.keys() {
            total += 1;
            per_workspace += usize::from(id == workspace);
            per_actor += usize::from(ws.stored.jobs[job_id].actor == actor);
        }
    }
    for (scope, observed, limit) in [
        ("workspace", per_workspace, MAX_WORKSPACE_JOBS),
        ("actor", per_actor, MAX_ACTOR_JOBS),
        ("application", total, MAX_OUTSTANDING_JOBS),
    ] {
        if observed >= limit {
            return Err(Error::Domain {
                code: "RESOURCE_LIMIT".into(),
                message: "Too many unfinished geometry jobs; wait for a job to finish or cancel one before retrying".into(),
                details: serde_json::json!({"resource":"outstanding_jobs","scope":scope,"limit":limit,"observed":observed,"retryable":true}),
            });
        }
    }
    Ok(())
}

impl Core {
    pub(crate) async fn construction_slot(
        &self,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<OwnedSemaphorePermit> {
        if *cancel.borrow() {
            return Err(Error::domain(
                "CANCELLED",
                "Geometry construction cancelled",
            ));
        }
        // Semaphore acquisition is FIFO and cancellation-safe. No workspace or
        // worker mutex is held here, so queueing cannot block context/navigation.
        tokio::select! {
            biased;
            _ = cancel.changed() => Err(Error::domain("CANCELLED", "Cancelled while waiting for geometry capacity")),
            permit = self.construction_slots.clone().acquire_owned() => {
                let permit = permit.map_err(|_| Error::domain("WORKER_UNAVAILABLE", "Geometry construction capacity is unavailable"))?;
                if *cancel.borrow() {
                    return Err(Error::domain("CANCELLED", "Geometry construction cancelled"));
                }
                Ok(permit)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Draft, Job, JobState};
    use std::collections::BTreeMap;
    use std::time::Duration;

    async fn workspace(core: &Core) -> (String, String) {
        let ws = core.create_workspace("user").await.unwrap();
        let revision = core.get_project(&ws).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        (ws, revision)
    }
    async fn draft(core: &Core, ws: &str, revision: &str, actor: &str) -> Draft {
        core.update_source(ws, revision, None, BTreeMap::new(), actor)
            .await
            .unwrap()
    }
    async fn cancelled(core: &Core, ws: &str, job: &Job) {
        core.cancel_job(ws, &job.id).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let job = core.get_job(ws, &job.id).await.unwrap();
                if job.state == JobState::Cancelled {
                    break;
                }
                assert_eq!(job.state, JobState::Queued);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn bounded_job_admission(scope: &str, limit: usize) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let core = Core::open(
            root.clone(),
            crate::worker::WorkerConfig::bundled("unused".into()),
        )
        .await
        .unwrap();
        // Real durable jobs queue behind capacity, without launching a fixture
        // interpreter. This isolates admission, persistence and cancellation.
        let held = core
            .construction_slots
            .clone()
            .acquire_many_owned(MAX_CONSTRUCTIONS as u32)
            .await
            .unwrap();
        let common = workspace(&core).await;
        let mut accepted = Vec::new();
        for index in 0..limit {
            let (ws, revision) = if scope == "workspace" {
                common.clone()
            } else {
                workspace(&core).await
            };
            let actor = if scope == "actor" {
                "single-actor".into()
            } else {
                format!("actor-{index}")
            };
            let draft = draft(&core, &ws, &revision, &actor).await;
            let job = core.build(&ws, &revision, &draft.id, &actor).await.unwrap();
            accepted.push((ws, job));
        }
        let (ws, revision) = if scope == "workspace" {
            common
        } else {
            workspace(&core).await
        };
        let actor = if scope == "actor" {
            "single-actor"
        } else {
            "rejected-actor"
        };
        let draft = draft(&core, &ws, &revision, actor).await;
        let persisted = root.join("workspaces").join(&ws).join("workspace.json");
        let before = std::fs::read(&persisted).unwrap();
        let source_before = core.read_source(&ws, &draft.id, "model.py").await.unwrap();
        let error = core
            .build(&ws, &revision, &draft.id, actor)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "RESOURCE_LIMIT");
        let details = crate::error_value(&error)["details"].clone();
        assert_eq!(
            details,
            serde_json::json!({"resource":"outstanding_jobs","scope":scope,"limit":limit,"observed":limit,"retryable":true})
        );
        assert_eq!(
            std::fs::read(&persisted).unwrap(),
            before,
            "rejected admission must not create a job or immutable snapshot"
        );
        assert_eq!(core.get_project(&ws).await.unwrap()["revision"], revision);
        assert_eq!(
            core.read_source(&ws, &draft.id, "model.py").await.unwrap(),
            source_before
        );
        {
            let workspaces = core.workspaces.lock().await;
            assert_eq!(
                workspaces
                    .values()
                    .map(|ws| ws.cancellation.len())
                    .sum::<usize>(),
                limit
            );
            assert_eq!(
                workspaces
                    .values()
                    .map(|ws| ws.stored.job_snapshots.len())
                    .sum::<usize>(),
                limit
            );
        }
        cancelled(&core, &accepted[0].0, &accepted[0].1).await;
        let replacement = core.build(&ws, &revision, &draft.id, actor).await.unwrap();
        accepted.push((ws, replacement));
        for (ws, job) in accepted {
            cancelled(&core, &ws, &job).await;
        }
        assert!(!root.join("runs").exists());
        assert!(
            core.workspaces
                .lock()
                .await
                .values()
                .all(|ws| ws.cancellation.is_empty())
        );
        drop(held);
    }

    #[tokio::test]
    async fn workspace_job_limit_applies_across_actors_before_snapshot_persistence() {
        bounded_job_admission("workspace", MAX_WORKSPACE_JOBS).await;
    }
    #[tokio::test]
    async fn actor_job_limit_applies_across_workspaces_before_snapshot_persistence() {
        bounded_job_admission("actor", MAX_ACTOR_JOBS).await;
    }
    #[tokio::test]
    async fn application_job_limit_applies_across_actors_and_workspaces() {
        bounded_job_admission("application", MAX_OUTSTANDING_JOBS).await;
    }
    #[tokio::test]
    async fn concurrent_job_submissions_cannot_race_past_application_capacity() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::open(
            temp.path().join("app"),
            crate::worker::WorkerConfig::bundled("unused".into()),
        )
        .await
        .unwrap();
        let held = core
            .construction_slots
            .clone()
            .acquire_many_owned(MAX_CONSTRUCTIONS as u32)
            .await
            .unwrap();
        let mut accepted = Vec::new();
        for index in 0..MAX_OUTSTANDING_JOBS - 1 {
            let (ws, revision) = workspace(&core).await;
            let actor = format!("actor-{index}");
            let draft = draft(&core, &ws, &revision, &actor).await;
            let job = core.build(&ws, &revision, &draft.id, &actor).await.unwrap();
            accepted.push((ws, job));
        }
        let (a, ar) = workspace(&core).await;
        let (b, br) = workspace(&core).await;
        let ad = draft(&core, &a, &ar, "racing-a").await;
        let bd = draft(&core, &b, &br, "racing-b").await;
        let (left, right) = tokio::join!(
            core.build(&a, &ar, &ad.id, "racing-a"),
            core.build(&b, &br, &bd.id, "racing-b")
        );
        let mut rejected = 0;
        for (ws, result) in [(a, left), (b, right)] {
            match result {
                Ok(job) => accepted.push((ws, job)),
                Err(error) => {
                    assert_eq!(error.code(), "RESOURCE_LIMIT");
                    rejected += 1;
                }
            }
        }
        assert_eq!(rejected, 1);
        assert_eq!(accepted.len(), MAX_OUTSTANDING_JOBS);
        for (ws, job) in accepted {
            cancelled(&core, &ws, &job).await;
        }
        assert!(!temp.path().join("app/runs").exists());
        drop(held);
    }

    #[tokio::test]
    async fn recovery_shares_construction_capacity_and_cancels_without_materialization() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let core = Core::open(
            root.clone(),
            crate::worker::WorkerConfig::bundled("unused".into()),
        )
        .await
        .unwrap();
        let ws = core.create_workspace("user").await.unwrap();
        let revision = core.get_project(&ws).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let (_send, cancel) = watch::channel(false);
        let first = core.construction_slot(cancel.clone()).await.unwrap();
        let second = core.construction_slot(cancel).await.unwrap();
        let (send, cancel) = watch::channel(false);
        let waiting = core.verified_worker(&ws, &revision, cancel);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut waiting)
                .await
                .is_err()
        );
        send.send_replace(true);
        let result = tokio::time::timeout(Duration::from_secs(1), &mut waiting)
            .await
            .unwrap();
        assert!(matches!(result, Err(error) if error.code() == "CANCELLED"));
        assert!(!root.join("runs").exists());
        drop(first);
        drop(second);
        assert_eq!(
            core.construction_slots.available_permits(),
            MAX_CONSTRUCTIONS
        );
    }

    #[tokio::test]
    async fn queued_recovery_rechecks_revision_before_executing_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        let core = Core::open(
            root.clone(),
            crate::worker::WorkerConfig::bundled("unused".into()),
        )
        .await
        .unwrap();
        let ws = core.create_workspace("user").await.unwrap();
        let revision = core.get_project(&ws).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let held = core
            .construction_slots
            .clone()
            .acquire_many_owned(MAX_CONSTRUCTIONS as u32)
            .await
            .unwrap();
        let (_send, cancel) = watch::channel(false);
        let waiting = core.verified_worker(&ws, &revision, cancel);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut waiting)
                .await
                .is_err()
        );
        // Synthetic revision transition isolates the admission/validation race;
        // full native revision transactions are covered by integration tests.
        core.workspaces
            .lock()
            .await
            .get_mut(&ws)
            .unwrap()
            .stored
            .current = "later".into();
        drop(held);
        let result = tokio::time::timeout(Duration::from_secs(1), &mut waiting)
            .await
            .unwrap();
        assert!(matches!(result, Err(error) if error.code() == "STALE_ENTITY"));
        assert!(!root.join("runs").exists());
        assert_eq!(
            core.construction_slots.available_permits(),
            MAX_CONSTRUCTIONS
        );
    }
}
