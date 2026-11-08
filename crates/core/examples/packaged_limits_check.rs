//! CLI checks of reduced native budgets against the actual macOS bundle.
#[cfg(target_os = "macos")]
mod macos {
    use cadpad_core::{Core, Job, JobState, runtime::PackagedRuntime, worker_limits::Limits};
    use serde_json::{Value, json};
    use std::{
        collections::BTreeMap,
        path::Path,
        time::{Duration, Instant},
    };

    const GOOD: &str = "import cadquery as cq\nshow(cq.Workplane().box(10,20,30),name='box')";
    const ENTERED: &str = r#"
import json, os, sys, time
from pathlib import Path
cache = Path(sys.argv[sys.argv.index('--cache') + 1])
(cache / 'budget-entered.json').write_text(json.dumps(dict(worker=os.getpid(), guardian=os.getppid())))
"#;

    async fn build(core: &Core, ws: &str, revision: &str, source: &str) -> Job {
        let draft = core
            .update_source(
                ws,
                revision,
                None,
                BTreeMap::from([("model.py".into(), source.into())]),
                "packaged-budget-check",
            )
            .await
            .unwrap();
        core.build(ws, revision, &draft.id, "packaged-budget-check")
            .await
            .unwrap()
    }
    async fn terminal(core: &Core, ws: &str, job: Job) -> Job {
        tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                let result = core.get_job(ws, &job.id).await.unwrap();
                if !matches!(result.state, JobState::Queued | JobState::Running) {
                    return result;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn entered(root: &Path) -> (std::path::PathBuf, Value) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                for directory in std::fs::read_dir(root).unwrap() {
                    let cache = directory.unwrap().path();
                    if let Ok(bytes) = std::fs::read(cache.join("budget-entered.json"))
                        && let Ok(mut pids) = serde_json::from_slice::<Value>(&bytes)
                    {
                        let mut children = [0i32; 8];
                        // The already-exec'd anchor must be armed before source runs.
                        let count = unsafe {
                            libc::proc_listchildpids(
                                pids["worker"].as_i64().unwrap() as i32,
                                children.as_mut_ptr().cast(),
                                std::mem::size_of_val(&children) as i32,
                            )
                        };
                        assert_eq!(count, 1);
                        pids["anchor"] = json!(children[0]);
                        return (cache, pids);
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn gone_and_unleased(cache: &Path, pids: &Value) {
        use std::os::fd::AsRawFd;
        tokio::time::timeout(Duration::from_secs(10), async {
            for key in ["worker", "guardian", "anchor"] {
                let pid = pids[key].as_i64().unwrap() as i32;
                loop {
                    if unsafe { libc::kill(pid, 0) } < 0 {
                        assert_eq!(
                            std::io::Error::last_os_error().raw_os_error(),
                            Some(libc::ESRCH)
                        );
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            let lease = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(cache.join(".lease"))
                .unwrap();
            loop {
                if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    break;
                }
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn preserved(core: &Core, ws: &str, revision: &str) {
        assert_eq!(core.get_project(ws).await.unwrap()["revision"], revision);
        assert_eq!(
            core.read_source(ws, revision, "model.py").await.unwrap()["source"],
            GOOD
        );
        let exact = core
        .query(
            ws,
            revision,
            "inspect",
            json!({"entities":[{"revision":revision,"instance_id":"box","entity_id":"part:0"}]}),
        )
        .await
        .unwrap();
        assert_eq!(exact["entities"][0]["volume"], 6000.0);
        assert_eq!(
            exact["entities"][0]["bounds"]["size"],
            json!([10.0, 20.0, 30.0])
        );
    }

    #[tokio::main]
    pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            std::env::var("PATH").as_deref(),
            Ok(""),
            "Run with an empty PATH"
        );
        let resources = std::path::PathBuf::from(
            std::env::args_os()
                .nth(1)
                .ok_or("Resources directory required")?,
        );
        let runtime = PackagedRuntime::open(&resources)?;
        let temporary = tempfile::tempdir()?;
        let mut results = Vec::new();
        for (resource, limit, source) in [
            ("cpu_time_ms", 8000u64, "while True: pass"),
            // Touch bounded chunks slowly enough for the sampled watchdog to run.
            (
                "memory_bytes",
                1024 * 1024 * 1024,
                "blocks=[]\nfor _ in range(128):\n blocks.append(bytearray(b'x'*(4*1024*1024)))\n time.sleep(0.01)\ntime.sleep(60)",
            ),
            ("operation_time_ms", 8000, "time.sleep(60)"),
        ] {
            let mut config = runtime.worker_config();
            config.limits = Limits {
                operation_timeout_ms: 60_000,
                cpu_time_ms: 60_000,
                ..Default::default()
            };
            match resource {
                "cpu_time_ms" => config.limits.cpu_time_ms = limit,
                "memory_bytes" => config.limits.memory_bytes = limit,
                "operation_time_ms" => config.limits.operation_timeout_ms = limit,
                _ => unreachable!(),
            }
            let root = temporary.path().join(resource);
            let core = Core::open(root.clone(), config).await?;
            let ws = core.create_workspace("packaged-budget-check").await?;
            let base = core.get_project(&ws).await?["revision"]
                .as_str()
                .unwrap()
                .to_owned();
            let good = terminal(&core, &ws, build(&core, &ws, &base, GOOD).await).await;
            assert_eq!(good.state, JobState::Succeeded, "{}", good.diagnostics);
            let revision = good.resulting_revision.unwrap();
            let start = Instant::now();
            let job = build(&core, &ws, &revision, &format!("{ENTERED}\n{source}")).await;
            let (cache, pids) = entered(&root.join("artifacts").join(&ws)).await;
            let failed = terminal(&core, &ws, job).await;
            assert_eq!(failed.state, JobState::Failed, "{}", failed.diagnostics);
            assert!(failed.resulting_revision.is_none());
            assert_eq!(failed.diagnostics["code"], "RESOURCE_LIMIT");
            let details = &failed.diagnostics["details"];
            assert_eq!(details["resource"], resource);
            assert_eq!(details["limit"], limit);
            if resource != "operation_time_ms" {
                assert_eq!(details["kind"], "limit");
                assert_eq!(details["pid"], pids["worker"]);
                assert!(details["observed"].as_u64().unwrap() >= limit);
            }
            gone_and_unleased(&cache, &pids).await;
            preserved(&core, &ws, &revision).await;
            let recovered = terminal(&core, &ws, build(&core, &ws, &revision, GOOD).await).await;
            assert_eq!(
                recovered.state,
                JobState::Succeeded,
                "{}",
                recovered.diagnostics
            );
            let recovered_revision = recovered.resulting_revision.unwrap();
            assert_ne!(recovered_revision, revision);
            preserved(&core, &ws, &recovered_revision).await;
            results.push(json!({"resource":resource,"report":details,"elapsed_through_recovery_ms":start.elapsed().as_millis(),"source_entered":true,"worker_guardian_anchor_gone":true,"cache_lease_released":true,"committed_source_and_revision_preserved":true,"exact_volume_mm3":6000,"rebuild_after_limit_succeeded":true}));
        }
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"status":"passed","resources":resources.canonicalize()?,"empty_path":true,"contained_bundled_worker":true,"reduced_host_budgets":true,"max_retained_fixture_allocation_bytes":512*1024*1024u64,"allocation_chunk_bytes":4*1024*1024u64,"probes":results,"ui_launched":false})
            )?
        );
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        macos::run()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Packaged resource-limit probes require macOS".into())
    }
}
