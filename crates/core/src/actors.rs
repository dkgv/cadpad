//! Transport-owned session cancellation. A revoked actor cannot create new builds.
use crate::{Core, Error, Result};
use tokio::sync::watch;
impl Core {
    pub fn actor_signal(&self, actor: &str) -> watch::Receiver<bool> {
        self.actor_tokens
            .lock()
            .unwrap()
            .entry(actor.into())
            .or_insert_with(|| watch::channel(false).0)
            .subscribe()
    }
    pub fn ensure_actor_active(&self, actor: &str) -> Result<()> {
        if *self.actor_signal(actor).borrow() {
            Err(Error::domain(
                "CANCELLED",
                "Actor session was disconnected; reconnect to Cadpad with a new session",
            ))
        } else {
            Ok(())
        }
    }
    pub async fn revoke_actor(&self, actor: &str) -> usize {
        let workspaces = self.workspaces.lock().await;
        self.actor_tokens
            .lock()
            .unwrap()
            .entry(actor.into())
            .or_insert_with(|| watch::channel(false).0)
            .send_replace(true);
        let mut count = self.motion_test_runs.cancel_actor(actor);
        for workspace in workspaces.values() {
            for (job_id, cancel) in &workspace.cancellation {
                if workspace
                    .stored
                    .jobs
                    .get(job_id)
                    .is_some_and(|job| job.actor == actor)
                {
                    let _ = cancel.send(true);
                    count += 1;
                }
            }
        }
        count
    }
}

impl Core {
    pub async fn actor_jobs(&self, actor: &str) -> Vec<(String, crate::Job)> {
        let workspaces = self.workspaces.lock().await;
        workspaces
            .iter()
            .flat_map(|(workspace, state)| {
                state
                    .stored
                    .jobs
                    .values()
                    .filter(|job| job.actor == actor)
                    .cloned()
                    .map(|job| (workspace.clone(), job))
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}
