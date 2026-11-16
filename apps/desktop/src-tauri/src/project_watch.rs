//! Filesystem notifications invalidate views; directory reads remain authoritative.
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tauri::{State, ipc::Channel};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

const BATCH_DELAY: Duration = Duration::from_millis(200);

#[derive(Clone, Default)]
pub struct ProjectWatchers {
    roots: Arc<Mutex<HashMap<PathBuf, Weak<RootWatch>>>>,
}
struct RootWatch {
    changes: watch::Receiver<()>,
    task: JoinHandle<()>,
}
impl Drop for RootWatch {
    fn drop(&mut self) {
        // Aborting drops the native watcher even if the directory stays idle.
        self.task.abort();
    }
}
fn relevant(root: &Path, event: &Event) -> bool {
    if event.need_rescan() {
        return true;
    }
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.is_empty()
        || event.paths.iter().any(|path| {
            path.strip_prefix(root).is_ok_and(|relative| {
                !relative.components().any(|part| {
                    matches!(
                        part.as_os_str().to_str(),
                        Some("node_modules" | "__pycache__" | ".venv" | "venv")
                    )
                })
            })
        })
}
impl ProjectWatchers {
    fn subscribe(&self, directory: PathBuf) -> notify::Result<Arc<RootWatch>> {
        let directory = directory.canonicalize().unwrap_or(directory);
        let mut roots = self.roots.lock().unwrap();
        roots.retain(|_, root| root.strong_count() > 0);
        if let Some(root) = roots.get(&directory).and_then(Weak::upgrade) {
            return Ok(root);
        }
        let (dirty, mut events) = mpsc::channel(1);
        let rearm = Arc::new(AtomicBool::new(false));
        let callback_rearm = rearm.clone();
        let root = directory.clone();
        let mut watcher = RecommendedWatcher::new(
            move |result: notify::Result<Event>| {
                let invalidate = match result {
                    Ok(event) => {
                        if event.need_rescan()
                            || (matches!(
                                event.kind,
                                EventKind::Create(_)
                                    | EventKind::Remove(_)
                                    | EventKind::Modify(notify::event::ModifyKind::Name(_))
                            ) && event.paths.iter().any(|path| path == &root))
                        {
                            callback_rearm.store(true, Ordering::Relaxed);
                        }
                        relevant(&root, &event)
                    }
                    Err(_) => {
                        // Lost/overflowed events require a fresh disk read, never replay.
                        callback_rearm.store(true, Ordering::Relaxed);
                        true
                    }
                };
                if invalidate {
                    let _ = dirty.try_send(());
                }
            },
            Config::default().with_follow_symlinks(false),
        )?;
        // Keep watching the parent so deleting and recreating a project root
        // reattaches the recursive watch without a polling timer.
        if let Some(parent) = directory.parent() {
            watcher.watch(parent, RecursiveMode::NonRecursive)?;
        }
        if directory.is_dir() {
            watcher.watch(&directory, RecursiveMode::Recursive)?;
        }
        let (changes, receiver) = watch::channel(());
        let watch_key = directory.clone();
        let task = tokio::spawn(async move {
            let mut watcher = watcher;
            while events.recv().await.is_some() {
                // A bounded queue and fixed batch window handle event storms
                // without unbounded memory or starving refresh during long writes.
                tokio::time::sleep(BATCH_DELAY).await;
                while events.try_recv().is_ok() {}
                if rearm.swap(false, Ordering::Relaxed) && directory.is_dir() {
                    let _ = watcher.watch(&directory, RecursiveMode::Recursive);
                }
                if changes.send(()).is_err() {
                    break;
                }
            }
        });
        let root = Arc::new(RootWatch {
            changes: receiver,
            task,
        });
        roots.insert(watch_key, Arc::downgrade(&root));
        Ok(root)
    }
}

#[tauri::command]
pub async fn connect_project_files(
    core: State<'_, cadpad_core::Core>,
    watchers: State<'_, ProjectWatchers>,
    subscriptions: State<'_, crate::subscriptions::Subscriptions>,
    workspace: String,
    channel: Channel<Value>,
) -> Result<(), Value> {
    let directory = match core.project_directory(&workspace).await {
        Ok(directory) => directory,
        Err(error) if error.code() == "INVALID_REQUEST" => return Ok(()),
        Err(error) => return Err(cadpad_core::error_value(&error)),
    };
    let watchers = watchers.inner().clone();
    let root = tokio::task::spawn_blocking(move || watchers.subscribe(directory))
        .await
        .map_err(watch_error)?
        .map_err(watch_error)?;
    let mut changes = root.changes.clone();
    changes.borrow_and_update();
    subscriptions.spawn(channel.id(), async move {
        let _root = root;
        // Refresh after registration closes the initial read/watch race.
        if channel.send(json!({"kind":"ready"})).is_err() {
            return;
        }
        while changes.changed().await.is_ok() {
            if channel.send(json!({"kind":"changed"})).is_err() {
                break;
            }
        }
    });
    Ok(())
}
fn watch_error(error: impl std::fmt::Display) -> Value {
    json!({"code":"IO_ERROR","message":format!("Could not watch project folder: {error}")})
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, Flag};

    async fn changed(changes: &mut watch::Receiver<()>) {
        tokio::time::timeout(Duration::from_secs(5), changes.changed())
            .await
            .expect("native filesystem change was not delivered")
            .unwrap();
    }
    async fn settle(changes: &mut watch::Receiver<()>) {
        while tokio::time::timeout(Duration::from_millis(600), changes.changed())
            .await
            .is_ok_and(|result| result.is_ok())
        {}
    }
    #[test]
    fn filters_read_events_and_cache_noise_but_keeps_rescan_and_directory_changes() {
        let root = Path::new("/project");
        let created =
            |path| Event::new(EventKind::Create(CreateKind::Any)).add_path(PathBuf::from(path));
        assert!(relevant(root, &created("/project/new/nested")));
        assert!(relevant(root, &created("/project/.git/HEAD")));
        assert!(!relevant(root, &created("/sibling/new.py")));
        assert!(!relevant(root, &created("/project/__pycache__/model.pyc")));
        assert!(!relevant(
            root,
            &created("/project/node_modules/package/index.js")
        ));
        assert!(!relevant(
            root,
            &Event::new(EventKind::Access(AccessKind::Read)).add_path(root.join("model.py"))
        ));
        assert!(relevant(
            root,
            &Event::new(EventKind::Other).set_flag(Flag::Rescan)
        ));
    }
    #[tokio::test]
    async fn native_events_cover_nested_changes_atomic_saves_and_recreated_roots() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("project");
        std::fs::create_dir(&directory).unwrap();
        let watchers = ProjectWatchers::default();
        let root = watchers.subscribe(directory.clone()).unwrap();
        let shared = watchers.subscribe(directory.clone()).unwrap();
        assert!(
            Arc::ptr_eq(&root, &shared),
            "one native watch per directory"
        );
        let mut changes = root.changes.clone();
        settle(&mut changes).await;
        std::fs::create_dir(directory.join("nested")).unwrap();
        std::fs::write(directory.join("nested/model.py"), "# new").unwrap();
        changed(&mut changes).await;
        settle(&mut changes).await;
        std::fs::write(directory.join("replacement.tmp"), "# atomic save").unwrap();
        std::fs::rename(
            directory.join("replacement.tmp"),
            directory.join("nested/model.py"),
        )
        .unwrap();
        changed(&mut changes).await;
        settle(&mut changes).await;
        std::fs::rename(directory.join("nested"), directory.join("moved")).unwrap();
        changed(&mut changes).await;
        settle(&mut changes).await;
        std::fs::remove_dir_all(&directory).unwrap();
        changed(&mut changes).await;
        settle(&mut changes).await;
        std::fs::create_dir(&directory).unwrap();
        changed(&mut changes).await;
        settle(&mut changes).await;
        std::fs::create_dir(directory.join("again")).unwrap();
        std::fs::write(directory.join("again/new.py"), "# after recreation").unwrap();
        changed(&mut changes).await;
    }
    #[tokio::test]
    async fn idle_watches_do_not_emit_and_disconnect_releases_the_native_watcher() {
        let temp = tempfile::tempdir().unwrap();
        let watchers = ProjectWatchers::default();
        let root = watchers.subscribe(temp.path().to_path_buf()).unwrap();
        let weak = Arc::downgrade(&root);
        let task = root.task.abort_handle();
        let mut changes = root.changes.clone();
        assert!(
            tokio::time::timeout(Duration::from_millis(700), changes.changed())
                .await
                .is_err()
        );
        let subscriptions = crate::subscriptions::Subscriptions::default();
        subscriptions.spawn(123, async move {
            let _root = root;
            std::future::pending::<()>().await;
        });
        subscriptions.disconnect(123);
        tokio::time::timeout(Duration::from_secs(2), async {
            while weak.upgrade().is_some() || !task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("disconnect must release an idle native watcher");
    }
}
