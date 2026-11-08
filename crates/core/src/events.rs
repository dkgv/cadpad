//! Ordered small notifications; geometry remains on the scoped artifact transport.
use crate::{Core, Job};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Activity {
    pub workspace_id: String,
    pub kind: String,
    pub job: Option<Job>,
    pub actor: Option<String>,
    pub tool: Option<String>,
}
/// Balance activity even when a read-only dispatcher future is cancelled.
/// This owns only a broadcast sender, never a Core/application-lock reference.
pub(crate) struct ToolActivity {
    sender: tokio::sync::broadcast::Sender<Activity>,
    unfinished: Option<Activity>,
}
impl ToolActivity {
    pub(crate) fn start(
        sender: tokio::sync::broadcast::Sender<Activity>,
        workspace: String,
        actor: &str,
        tool: &str,
    ) -> Self {
        let mut event = Activity {
            workspace_id: workspace,
            kind: "tool_started".into(),
            job: None,
            actor: Some(actor.into()),
            tool: Some(tool.into()),
        };
        let _ = sender.send(event.clone());
        event.kind = "tool_failed".into();
        Self {
            sender,
            unfinished: Some(event),
        }
    }
    pub(crate) fn finish(mut self, succeeded: bool) {
        if let Some(mut event) = self.unfinished.take() {
            event.kind = if succeeded {
                "tool_succeeded"
            } else {
                "tool_failed"
            }
            .into();
            let _ = self.sender.send(event);
        }
    }
}
impl Drop for ToolActivity {
    fn drop(&mut self) {
        if let Some(event) = self.unfinished.take() {
            let _ = self.sender.send(event);
        }
    }
}
impl Core {
    pub fn subscribe_activity(&self) -> tokio::sync::broadcast::Receiver<Activity> {
        self.activity.subscribe()
    }
    pub(crate) fn job_activity(&self, workspace: &str, job: &Job) {
        let _ = self.activity.send(Activity {
            workspace_id: workspace.into(),
            kind: "job".into(),
            job: Some(job.clone()),
            actor: Some(job.actor.clone()),
            tool: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{tools::ToolContext, worker::WorkerConfig};
    use serde_json::json;
    use std::time::Duration;
    #[tokio::test]
    async fn dropping_a_held_read_only_dispatch_finishes_its_activity_without_executing_work() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::open(
            temp.path().join("app"),
            WorkerConfig::bundled("unused-worker".into()),
        )
        .await
        .unwrap();
        let workspace = core.create_workspace("user").await.unwrap();
        let mut events = core.subscribe_activity();
        let held = core.workspaces.lock().await;
        let actor = ToolContext {
            actor: "cancelled-reader".into(),
        };
        {
            let request = core.dispatch(&actor, "get_project", json!({"workspace_id":workspace}));
            tokio::pin!(request);
            let started = tokio::select! {
                _=&mut request=>panic!("The workspace lock should keep this read pending"),
                event=events.recv()=>event.unwrap(),
                _=tokio::time::sleep(Duration::from_secs(1))=>panic!("Missing start activity"),
            };
            assert_eq!(started.kind, "tool_started");
        }
        let finished = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(finished.kind, "tool_failed");
        assert_eq!(finished.workspace_id, workspace);
        assert_eq!(finished.actor.as_deref(), Some("cancelled-reader"));
        assert_eq!(finished.tool.as_deref(), Some("get_project"));
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        drop(held);
        assert!(!temp.path().join("app/runs").exists());
        assert_eq!(core.get_project(&workspace).await.unwrap()["total_jobs"], 0);
    }
    #[tokio::test]
    async fn successful_and_failed_dispatches_each_publish_one_terminal_activity() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::open(
            temp.path().join("app"),
            WorkerConfig::bundled("unused-worker".into()),
        )
        .await
        .unwrap();
        let workspace = core.create_workspace("user").await.unwrap();
        let mut events = core.subscribe_activity();
        let actor = ToolContext {
            actor: "user".into(),
        };
        for (name, terminal) in [
            ("get_project", "tool_succeeded"),
            ("unknown_tool", "tool_failed"),
        ] {
            let result = core
                .dispatch(&actor, name, json!({"workspace_id":workspace}))
                .await;
            assert_eq!(result.is_ok(), terminal == "tool_succeeded");
            assert_eq!(events.recv().await.unwrap().kind, "tool_started");
            assert_eq!(events.recv().await.unwrap().kind, terminal);
            assert!(matches!(
                events.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
        }
    }
}
