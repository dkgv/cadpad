//! Native resource policy, shared with the small guardian without linking CAD.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub operation_timeout_ms: u64,
    /// Lifetime CPU time of one worker, across all requests and idle threads.
    pub cpu_time_ms: u64,
    /// Sampled max(resident size, physical footprint), not virtual address space.
    pub memory_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            operation_timeout_ms: 300_000,
            cpu_time_ms: 900_000,
            memory_bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> std::io::Result<()> {
        if self.operation_timeout_ms == 0
            || self.operation_timeout_ms > u32::MAX.into()
            || self.cpu_time_ms == 0
            || self.cpu_time_ms > u32::MAX.into()
            || self.memory_bytes == 0
            || self.memory_bytes > 1 << 50
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Worker resource limits must be positive and bounded",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    CpuTimeMs,
    MemoryBytes,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Report {
    Limit {
        resource: Resource,
        limit: u64,
        observed: u64,
        pid: u32,
    },
    MonitorUnavailable {
        message: String,
    },
}
/// Raw resource counters of one process, in Mach ticks and bytes.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ProcessUsage {
    /// User + system time of the process itself.
    own_ticks: u64,
    /// User + system time of its terminated, waited-for descendants.
    reaped_ticks: u64,
    resident: u64,
    footprint: u64,
}

/// Total ticks and memory of a process tree: the root first, then every live
/// descendant. The model's own forked children (the requirements evaluator's
/// pair pool and rebuilders) must count against the worker, so:
/// - CPU sums `own + reaped` over live processes; a process's reaped time
///   already contains its own finished descendants and nothing is counted twice.
/// - Memory is the root's max(resident, footprint) plus each descendant's
///   footprint only. Forked children share the parent's clean pages, which
///   resident size would count once per process; footprint is the private
///   (dirty or compressed) part and cannot over-report shared pages.
#[cfg(any(target_os = "macos", test))]
fn tree_totals(root: ProcessUsage, descendants: &[ProcessUsage]) -> (u64, u64) {
    let ticks = std::iter::once(&root)
        .chain(descendants)
        .fold(0u64, |sum, u| {
            sum.saturating_add(u.own_ticks)
                .saturating_add(u.reaped_ticks)
        });
    let memory = descendants
        .iter()
        .fold(root.resident.max(root.footprint), |sum, u| {
            sum.saturating_add(u.footprint)
        });
    (ticks, memory)
}

#[cfg(target_os = "macos")]
fn usage_of(pid: u32) -> std::io::Result<ProcessUsage> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: the V2 flavor writes exactly rusage_info_v2. The pointer cast is
    // libproc's rusage_info_t ABI, not an indirect pointer allocation.
    let result = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V2,
            usage.as_mut_ptr().cast(),
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful libproc call initialized the V2 structure.
    let usage = unsafe { usage.assume_init() };
    Ok(ProcessUsage {
        own_ticks: usage.ri_user_time.saturating_add(usage.ri_system_time),
        reaped_ticks: usage
            .ri_child_user_time
            .saturating_add(usage.ri_child_system_time),
        resident: usage.ri_resident_size,
        footprint: usage.ri_phys_footprint,
    })
}

/// Upper bound on tracked descendants; a fork bomb stops growing the walk, not
/// the accounting of what was found (the memory cap still trips on it).
#[cfg(target_os = "macos")]
const MAX_TRACKED_DESCENDANTS: usize = 1024;

#[cfg(target_os = "macos")]
fn children_of(pid: u32) -> Vec<u32> {
    let mut buffer = vec![0 as libc::c_int; 64];
    loop {
        // SAFETY: the buffer is valid for the stated number of bytes and the
        // call writes at most that many.
        let found = unsafe {
            libc::proc_listchildpids(
                pid as libc::c_int,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * std::mem::size_of::<libc::c_int>()) as libc::c_int,
            )
        };
        if found <= 0 {
            return Vec::new();
        }
        // libproc returns the number of pids, not bytes. A full buffer may
        // have truncated the list: grow and ask again.
        let found = found as usize;
        if found >= buffer.len() && buffer.len() < MAX_TRACKED_DESCENDANTS * 2 {
            let grown = buffer.len() * 4;
            buffer.resize(grown, 0);
            continue;
        }
        return buffer[..found.min(buffer.len())]
            .iter()
            .filter(|child| **child > 0)
            .map(|child| *child as u32)
            .collect();
    }
}

/// Every live descendant of `pid`, breadth first.
#[cfg(target_os = "macos")]
fn descendants(pid: u32) -> Vec<u32> {
    let mut found: Vec<u32> = Vec::new();
    let mut queue = std::collections::VecDeque::from([pid]);
    while let Some(next) = queue.pop_front() {
        for child in children_of(next) {
            if found.len() >= MAX_TRACKED_DESCENDANTS {
                return found;
            }
            if child != pid && !found.contains(&child) {
                found.push(child);
                queue.push_back(child);
            }
        }
    }
    found
}

/// Sample the worker and everything it forked. Limits apply to the sum.
#[cfg(target_os = "macos")]
pub fn sample(pid: u32, limits: Limits) -> std::io::Result<Option<Report>> {
    let root = usage_of(pid)?;
    // A child can exit between listing and reading; it no longer counts.
    let children: Vec<ProcessUsage> = descendants(pid)
        .into_iter()
        .filter_map(|child| usage_of(child).ok())
        .collect();
    let (ticks, memory) = tree_totals(root, &children);
    // libc deprecates its Mach bindings in favor of a separate crate; retain
    // the pinned stable ABI here to keep the independent guardian's dependency
    // set unchanged. The OS API itself supplies the platform's conversion ratio.
    #[allow(deprecated)]
    let cpu_ms = {
        let mut timebase = libc::mach_timebase_info_data_t { numer: 0, denom: 0 };
        // SAFETY: the kernel fills this fixed-size Mach timebase structure.
        if unsafe { libc::mach_timebase_info(&mut timebase) } != 0 {
            None
        } else {
            cpu_millis(ticks, 0, timebase.numer, timebase.denom)
        }
    }
    .ok_or_else(|| std::io::Error::other("Unable to read the Mach CPU timebase"))?;
    Ok(if cpu_ms >= limits.cpu_time_ms {
        Some(Report::Limit {
            resource: Resource::CpuTimeMs,
            limit: limits.cpu_time_ms,
            observed: cpu_ms,
            pid,
        })
    } else if memory >= limits.memory_bytes {
        Some(Report::Limit {
            resource: Resource::MemoryBytes,
            limit: limits.memory_bytes,
            observed: memory,
            pid,
        })
    } else {
        None
    })
}

#[cfg(any(target_os = "macos", test))]
fn cpu_millis(user: u64, system: u64, numerator: u32, denominator: u32) -> Option<u64> {
    if numerator == 0 || denominator == 0 {
        return None;
    }
    let ticks = u128::from(user) + u128::from(system);
    Some(
        (ticks * u128::from(numerator) / u128::from(denominator) / 1_000_000)
            .min(u128::from(u64::MAX)) as u64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mach_cpu_units_use_the_timebase_and_sum_user_and_system_without_overflow() {
        assert_eq!(cpu_millis(12_000_000, 12_000_000, 125, 3), Some(1000));
        assert_eq!(cpu_millis(500_000_000, 500_000_000, 1, 1), Some(1000));
        assert_eq!(cpu_millis(u64::MAX, u64::MAX, u32::MAX, 1), Some(u64::MAX));
        assert_eq!(cpu_millis(1, 1, 1, 0), None);
        assert_eq!(cpu_millis(1, 1, 0, 1), None);
    }

    #[test]
    fn tree_totals_sum_cpu_and_count_children_by_private_memory() {
        let root = ProcessUsage {
            own_ticks: 10,
            reaped_ticks: 5,
            resident: 900,
            footprint: 400,
        };
        let child = ProcessUsage {
            own_ticks: 7,
            reaped_ticks: 1,
            resident: 800,
            footprint: 100,
        };
        assert_eq!(tree_totals(root, &[]), (15, 900));
        assert_eq!(tree_totals(root, &[child, child]), (31, 1100));
        let huge = ProcessUsage {
            own_ticks: u64::MAX,
            reaped_ticks: u64::MAX,
            resident: u64::MAX,
            footprint: u64::MAX,
        };
        assert_eq!(tree_totals(huge, &[huge]), (u64::MAX, u64::MAX));
    }

    #[cfg(target_os = "macos")]
    mod forked_children {
        use super::*;
        use std::process::{Command, Stdio};

        const BIG: u64 = 1 << 50;
        fn limits(cpu_time_ms: u64, memory_bytes: u64) -> Limits {
            Limits {
                operation_timeout_ms: 60_000,
                cpu_time_ms,
                memory_bytes,
            }
        }
        fn kill_tree(root: &mut std::process::Child) {
            for pid in descendants(root.id()) {
                // SAFETY: signals a process this test just spawned.
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
            let _ = root.kill();
            let _ = root.wait();
        }

        #[test]
        fn descendants_are_found_through_intermediate_processes() {
            // sh -> sh -> sleep, plus a direct sleep.
            let mut root = Command::new("/bin/sh")
                .args(["-c", "sleep 30 & /bin/sh -c 'sleep 30 & wait' & wait"])
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            let mut count = 0;
            for _ in 0..100 {
                count = descendants(root.id()).len();
                if count >= 3 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            kill_tree(&mut root);
            assert!(count >= 3, "found {count} descendants");
        }

        #[test]
        fn cpu_burned_by_a_forked_child_counts_against_the_worker() {
            // The shell itself idles; the busy loop runs in a forked child.
            let mut root = Command::new("/bin/sh")
                .args(["-c", "/usr/bin/yes >/dev/null & wait"])
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            let mut tripped = None;
            for _ in 0..200 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if let Some(report) = sample(root.id(), limits(300, BIG)).unwrap() {
                    tripped = Some(report);
                    break;
                }
            }
            let own_ticks = usage_of(root.id()).unwrap().own_ticks;
            kill_tree(&mut root);
            assert!(
                matches!(
                    tripped,
                    Some(Report::Limit {
                        resource: Resource::CpuTimeMs,
                        ..
                    })
                ),
                "tree CPU limit did not trip (root's own ticks {own_ticks})"
            );
        }
    }
}
