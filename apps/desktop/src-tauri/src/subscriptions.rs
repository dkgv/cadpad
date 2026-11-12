//! Window/session-owned IPC streams must stop even when no further event arrives.
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::task::AbortHandle;

type Tasks = Mutex<HashMap<u32, (u64, AbortHandle)>>;
#[derive(Default)]
pub struct Subscriptions {
    tasks: Arc<Tasks>,
    next: AtomicU64,
}
struct Cleanup {
    tasks: Weak<Tasks>,
    id: u32,
    generation: u64,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(tasks) = self.tasks.upgrade() {
            let mut tasks = tasks.lock().unwrap();
            if tasks
                .get(&self.id)
                .is_some_and(|(generation, _)| *generation == self.generation)
            {
                tasks.remove(&self.id);
            }
        }
    }
}
impl Subscriptions {
    pub fn spawn(&self, id: u32, future: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.tasks.lock().unwrap();
        let generation = self.next.fetch_add(1, Ordering::Relaxed);
        let cleanup = Cleanup {
            tasks: Arc::downgrade(&self.tasks),
            id,
            generation,
        };
        let handle = tokio::spawn(async move {
            let _cleanup = cleanup;
            future.await;
        });
        if let Some((_, previous)) = tasks.insert(id, (generation, handle.abort_handle())) {
            previous.abort();
        }
    }
    pub fn disconnect(&self, id: u32) {
        let handle = self.tasks.lock().unwrap().remove(&id);
        if let Some((_, handle)) = handle {
            handle.abort();
        }
    }
}
impl Drop for Subscriptions {
    fn drop(&mut self) {
        for (_, (_, handle)) in self.tasks.lock().unwrap().drain() {
            handle.abort();
        }
    }
}
#[tauri::command]
pub fn disconnect_stream(subscriptions: tauri::State<'_, Subscriptions>, subscription_id: u32) {
    subscriptions.disconnect(subscription_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn closing_idle_streams_drops_captured_session_owners() {
        let subscriptions = Subscriptions::default();
        for id in 0..100 {
            let owner = Arc::new(vec![0u8; 1024 * 1024]);
            let weak = Arc::downgrade(&owner);
            subscriptions.spawn(id, async move {
                let _owner = owner;
                std::future::pending::<()>().await;
            });
            tokio::task::yield_now().await;
            subscriptions.disconnect(id);
            for _ in 0..10 {
                if weak.upgrade().is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                weak.upgrade().is_none(),
                "disconnection must release the session without waiting for another event"
            );
            assert!(subscriptions.tasks.lock().unwrap().is_empty());
        }
    }
}
