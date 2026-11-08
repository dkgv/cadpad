//! Ephemeral admission and cancellation for isolated simulation tests. A lease
//! remains live until both the owned operation and any entered CPU work finish.
use crate::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

pub const MAX_RUNS: usize = 8;
pub const MAX_ACTOR_RUNS: usize = 2;
pub const DEADLINE_S: u64 = 60;
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Running,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}
impl State {
    pub fn active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Cancelling)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Failure {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Status {
    pub id: String,
    pub workspace_id: String,
    pub revision: String,
    pub test_id: String,
    pub actor: String,
    pub state: State,
    pub error: Option<Failure>,
}
struct Entry {
    status: Status,
    cancel: watch::Sender<bool>,
    timed_out: bool,
}
#[derive(Default)]
pub(crate) struct Runs(Mutex<BTreeMap<String, Entry>>);
struct Inner {
    runs: Arc<Runs>,
    workspace: String,
    id: String,
    cancel: watch::Sender<bool>,
}
#[derive(Clone)]
pub(crate) struct Lease(Arc<Inner>);
pub(crate) struct CancelOnDrop(pub Lease);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
impl Runs {
    pub(crate) fn cancel_actor(&self, actor: &str) -> usize {
        let mut entries = self.0.lock().unwrap();
        let mut count = 0;
        for entry in entries
            .values_mut()
            .filter(|entry| entry.status.actor == actor && entry.status.state.active())
        {
            entry.cancel.send_replace(true);
            entry.status.state = State::Cancelling;
            count += 1;
        }
        count
    }
    pub(crate) fn status(&self, workspace: &str) -> Option<Status> {
        self.0
            .lock()
            .unwrap()
            .get(workspace)
            .map(|entry| entry.status.clone())
    }
    pub(crate) fn start(
        self: &Arc<Self>,
        workspace: &str,
        revision: &str,
        test: &str,
        actor: &str,
    ) -> Result<Lease> {
        let mut entries = self.0.lock().unwrap();
        let active = entries
            .values()
            .filter(|entry| entry.status.state.active())
            .collect::<Vec<_>>();
        for (scope, observed, limit) in [
            (
                "workspace",
                usize::from(
                    entries
                        .get(workspace)
                        .is_some_and(|e| e.status.state.active()),
                ),
                1,
            ),
            (
                "actor",
                active
                    .iter()
                    .filter(|entry| entry.status.actor == actor)
                    .count(),
                MAX_ACTOR_RUNS,
            ),
            ("application", active.len(), MAX_RUNS),
        ] {
            if observed >= limit {
                return Err(Error::Domain { code:"RESOURCE_LIMIT".into(),message:"A simulation test is already active or test capacity is full; stop a run or wait for completion".into(),details:serde_json::json!({"resource":"motion_test_runs","scope":scope,"limit":limit,"observed":observed,"retryable":true}) });
            }
        }
        let id = crate::id();
        let (cancel, _) = watch::channel(false);
        entries.insert(
            workspace.into(),
            Entry {
                status: Status {
                    id: id.clone(),
                    workspace_id: workspace.into(),
                    revision: revision.into(),
                    test_id: test.into(),
                    actor: actor.into(),
                    state: State::Queued,
                    error: None,
                },
                cancel: cancel.clone(),
                timed_out: false,
            },
        );
        Ok(Lease(Arc::new(Inner {
            runs: self.clone(),
            workspace: workspace.into(),
            id,
            cancel,
        })))
    }
    pub(crate) fn stop(&self, workspace: &str, id: &str, actor: &str) -> Result<()> {
        let mut entries = self.0.lock().unwrap();
        let entry = entries
            .get_mut(workspace)
            .filter(|entry| entry.status.id == id)
            .ok_or_else(|| {
                Error::domain("STALE_ENTITY", "This simulation run is no longer current")
            })?;
        if entry.status.actor != actor && actor != "local-user" {
            return Err(Error::domain(
                "UNSUPPORTED_OPERATION",
                "Only the run's actor or the local user can stop it",
            ));
        }
        if entry.status.state.active() {
            entry.cancel.send_replace(true);
            entry.status.state = State::Cancelling;
        }
        Ok(())
    }
    pub(crate) fn forget(&self, workspace: &str) {
        let mut entries = self.0.lock().unwrap();
        if let Some(entry) = entries.remove(workspace) {
            if entry.status.state.active() {
                entry.cancel.send_replace(true);
            }
        }
    }
}
impl Lease {
    pub(crate) fn signal(&self) -> watch::Receiver<bool> {
        self.0.cancel.subscribe()
    }
    pub(crate) fn status(&self) -> Status {
        self.0.runs.status(&self.0.workspace).unwrap()
    }
    fn update(&self, change: impl FnOnce(&mut Entry)) {
        let mut entries = self.0.runs.0.lock().unwrap();
        if let Some(entry) = entries
            .get_mut(&self.0.workspace)
            .filter(|entry| entry.status.id == self.0.id)
        {
            change(entry);
        }
    }
    pub(crate) fn cancel(&self) {
        self.update(|entry| {
            if entry.status.state.active() {
                entry.cancel.send_replace(true);
                entry.status.state = State::Cancelling;
            }
        });
    }
    pub(crate) fn timeout(&self) {
        self.update(|entry| {
            if entry.status.state.active() {
                entry.timed_out = true;
                entry.cancel.send_replace(true);
                entry.status.state = State::Cancelling;
            }
        });
    }
    pub(crate) fn running(&self) {
        self.update(|entry| {
            if entry.status.state == State::Queued {
                entry.status.state = State::Running;
            }
        });
    }
    pub(crate) fn check(&self) -> Result<()> {
        if *self.signal().borrow() {
            Err(Error::domain(
                "CANCELLED",
                "Simulation test cancelled; previous result is preserved",
            ))
        } else {
            Ok(())
        }
    }
    pub(crate) fn finish<T>(&self, result: &mut Result<T>) -> Option<Status> {
        let mut status = None;
        self.update(|entry| {
            if entry.timed_out && result.as_ref().is_err_and(|error| error.code()=="CANCELLED") {
                *result=Err(Error::domain("RESOURCE_LIMIT",format!("Simulation test exceeded {DEADLINE_S} seconds; shorten the test or simplify the mechanism")));
            }
            match result {
                Ok(_) => { entry.status.state=State::Completed; entry.status.error=None; }
                Err(error) => { entry.status.state=if error.code()=="CANCELLED" { State::Cancelled } else { State::Failed }; entry.status.error=Some(Failure { code:error.code().into(),message:error.to_string().chars().take(4096).collect() }); }
            }
            status=Some(entry.status.clone());
        });
        status
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        let mut entries = self.runs.0.lock().unwrap();
        if let Some(entry) = entries
            .get_mut(&self.workspace)
            .filter(|entry| entry.status.id == self.id && entry.status.state.active())
        {
            let cancelled = *self.cancel.borrow();
            entry.status.state = if cancelled && !entry.timed_out {
                State::Cancelled
            } else {
                State::Failed
            };
            entry.status.error=Some(Failure { code:if entry.timed_out { "RESOURCE_LIMIT" } else if cancelled { "CANCELLED" } else { "RUN_INTERRUPTED" }.into(),message:"Simulation test ended without committing a result; previous result is preserved".into() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_is_bounded_per_workspace_actor_and_application_and_old_leases_cannot_stop_new_runs()
     {
        let runs = Arc::new(Runs::default());
        let first = runs.start("a", "r", "test", "actor").unwrap();
        let old_id = first.status().id;
        assert_eq!(
            runs.start("a", "r", "test", "other").err().unwrap().code(),
            "RESOURCE_LIMIT"
        );
        let second = runs.start("b", "r", "test", "actor").unwrap();
        assert_eq!(
            runs.start("c", "r", "test", "actor").err().unwrap().code(),
            "RESOURCE_LIMIT"
        );
        let mut held = vec![second];
        for i in 0..6 {
            held.push(
                runs.start(&format!("global-{i}"), "r", "test", &format!("actor-{i}"))
                    .unwrap(),
            );
        }
        assert_eq!(
            runs.start("full", "r", "test", "fresh")
                .err()
                .unwrap()
                .code(),
            "RESOURCE_LIMIT"
        );
        let mut result: Result<()> = Ok(());
        first.finish(&mut result);
        let replacement = runs.start("a", "r", "test", "other").unwrap();
        first.cancel();
        drop(first);
        assert_eq!(replacement.status().state, State::Queued);
        assert_eq!(
            runs.stop("a", &old_id, "local-user").unwrap_err().code(),
            "STALE_ENTITY"
        );
        assert!(runs.stop("a", &replacement.status().id, "actor").is_err());
        runs.stop("a", &replacement.status().id, "local-user")
            .unwrap();
        assert_eq!(replacement.status().state, State::Cancelling);
    }
    #[tokio::test]
    async fn dropped_request_keeps_capacity_and_cancelling_state_until_entered_cpu_work_exits() {
        let runs = Arc::new(Runs::default());
        let lease = runs.start("a", "r", "test", "actor").unwrap();
        let cancel = lease.signal();
        let worker = lease.clone();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, waiting) = std::sync::mpsc::channel();
        let task = tokio::task::spawn_blocking(move || {
            let _worker = worker;
            let _ = entered.send(());
            waiting.recv().unwrap();
        });
        ready.await.unwrap();
        drop(CancelOnDrop(lease));
        assert!(*cancel.borrow());
        assert_eq!(runs.status("a").unwrap().state, State::Cancelling);
        assert!(runs.start("a", "r", "test", "actor").is_err());
        release.send(()).unwrap();
        task.await.unwrap();
        assert_eq!(runs.status("a").unwrap().state, State::Cancelled);
        assert!(runs.start("a", "r", "test", "actor").is_ok());
    }
    #[test]
    fn deadline_failure_is_distinct_from_stop_and_actor_revocation_targets_only_owned_runs() {
        let runs = Arc::new(Runs::default());
        let a = runs.start("a", "r", "test", "actor-a").unwrap();
        let b = runs.start("b", "r", "test", "actor-b").unwrap();
        assert_eq!(runs.cancel_actor("actor-a"), 1);
        assert!(*a.signal().borrow());
        assert!(!*b.signal().borrow());
        b.timeout();
        assert_eq!(b.status().state, State::Cancelling);
        let mut result: Result<()> = Err(Error::domain("CANCELLED", "tick cancelled"));
        let status = b.finish(&mut result).unwrap();
        assert_eq!(result.unwrap_err().code(), "RESOURCE_LIMIT");
        assert_eq!(status.state, State::Failed);
    }
}
