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
#[cfg(target_os = "macos")]
pub fn sample(pid: u32, limits: Limits) -> std::io::Result<Option<Report>> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v0>::zeroed();
    // SAFETY: the V0 flavor writes exactly rusage_info_v0. The pointer cast is
    // libproc's rusage_info_t ABI, not an indirect pointer allocation.
    let result = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V0,
            usage.as_mut_ptr().cast(),
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful libproc call initialized the V0 structure.
    let usage = unsafe { usage.assume_init() };
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
            cpu_millis(
                usage.ri_user_time,
                usage.ri_system_time,
                timebase.numer,
                timebase.denom,
            )
        }
    }
    .ok_or_else(|| std::io::Error::other("Unable to read the Mach CPU timebase"))?;
    let memory = usage.ri_resident_size.max(usage.ri_phys_footprint);
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
}
