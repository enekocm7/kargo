//! How many packages, targets and Native platforms build at the same time: `-j/--jobs`,
//! `KARGO_JOBS`, `[build] jobs` in `kargo.toml` and in `config.toml`, and the default picked from
//! the CPUs and the free memory of the machine.

use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};

/// The environment variable that sets the number of jobs when `--jobs` is not given.
pub const ENV: &str = "KARGO_JOBS";

/// The memory one job is assumed to use: a compiler (or test) JVM, or a Kotlin/Native link.
pub const MEMORY_PER_JOB: u64 = 1536 * 1024 * 1024;

/// A number of jobs as written: a count, or `auto` (also written `0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jobs {
    Auto,
    Count(usize),
}

impl Jobs {
    /// Parses `auto`, `0` or a positive number.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("auto") {
            return Ok(Jobs::Auto);
        }
        match text.parse::<usize>() {
            Ok(0) => Ok(Jobs::Auto),
            Ok(n) => Ok(Jobs::Count(n)),
            Err(_) => bail!("expected a number of jobs or `auto`, got `{text}`"),
        }
    }

    /// The number of jobs this stands for on this machine.
    pub fn count(self) -> usize {
        match self {
            Jobs::Auto => auto(),
            Jobs::Count(n) => n.max(1),
        }
    }
}

impl std::str::FromStr for Jobs {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        Jobs::parse(text)
    }
}

/// A `jobs` value of a TOML file: a number (`0` for `auto`) or `"auto"`.
pub fn from_toml(key: &str, value: &toml::Value) -> Result<Jobs> {
    let parsed = match value {
        toml::Value::Integer(n) if *n >= 0 => Jobs::parse(&n.to_string()),
        toml::Value::String(s) => Jobs::parse(s),
        _ => bail!("{key} must be a number of jobs or \"auto\", got `{value}`"),
    };
    parsed.with_context(|| format!("invalid {key}"))
}

/// `auto`: the number of CPUs, lowered so every job gets [`MEMORY_PER_JOB`] of the memory that is
/// free now (when it can be read).
pub fn auto() -> usize {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    auto_from(cpus, available_memory())
}

fn auto_from(cpus: usize, memory: Option<u64>) -> usize {
    let by_memory = memory.map_or(usize::MAX, |bytes| usize::try_from(bytes / MEMORY_PER_JOB).unwrap_or(usize::MAX));
    cpus.min(by_memory).max(1)
}

/// The limit set with [`set`]; 0 until then, which means [`auto`].
static LIMIT: AtomicUsize = AtomicUsize::new(0);

/// Sets how many targets (and Native platforms, and generation steps) of a package build at the same
/// time, for the rest of the process.
pub fn set(jobs: Jobs) {
    LIMIT.store(jobs.count(), Ordering::Relaxed);
}

/// The limit of [`set`], or [`auto`] when none was set.
pub fn limit() -> usize {
    match LIMIT.load(Ordering::Relaxed) {
        0 => auto(),
        n => n,
    }
}

/// The memory, in bytes, that new processes can use now: free memory, within the limit of the
/// cgroup on Linux. macOS reports half the physical memory, since it keeps little memory free.
pub fn available_memory() -> Option<u64> {
    imp::available_memory()
}

#[cfg(target_os = "linux")]
mod imp {
    pub(super) fn available_memory() -> Option<u64> {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let free = super::meminfo_available(&meminfo)?;
        // A container's limit (cgroup v2) is often far below the host's free memory.
        let read = |file: &str| std::fs::read_to_string(format!("/sys/fs/cgroup/{file}")).ok()?.trim().parse::<u64>().ok();
        match (read("memory.max"), read("memory.current")) {
            (Some(max), Some(current)) => Some(free.min(max.saturating_sub(current))),
            _ => Some(free),
        }
    }
}

#[cfg(windows)]
mod imp {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }

    pub(super) fn available_memory() -> Option<u64> {
        let mut status = MemoryStatusEx {
            length: std::mem::size_of::<MemoryStatusEx>() as u32,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        // SAFETY: `status` is a MEMORYSTATUSEX with its length set, as the call requires; it only writes to it.
        (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.avail_phys)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_char, c_int, c_void};

    unsafe extern "C" {
        fn sysctlbyname(name: *const c_char, old: *mut c_void, old_len: *mut usize, new: *mut c_void, new_len: usize) -> c_int;
    }

    pub(super) fn available_memory() -> Option<u64> {
        let mut total: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: `hw.memsize` is a 64-bit integer, written into `total`, whose size `len` holds.
        let ok = unsafe { sysctlbyname(c"hw.memsize".as_ptr(), (&raw mut total).cast(), &mut len, std::ptr::null_mut(), 0) } == 0;
        (ok && total > 0).then_some(total / 2)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    pub(super) fn available_memory() -> Option<u64> {
        None
    }
}

/// `MemAvailable` of `/proc/meminfo`, in bytes.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn meminfo_available(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find_map(|l| l.strip_prefix("MemAvailable:"))?;
    let kb: u64 = line.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_counts_and_auto() {
        assert_eq!(Jobs::parse("4").unwrap(), Jobs::Count(4));
        assert_eq!(Jobs::parse(" auto ").unwrap(), Jobs::Auto);
        assert_eq!(Jobs::parse("0").unwrap(), Jobs::Auto);
        assert!(Jobs::parse("-1").is_err());
        assert!(Jobs::parse("many").unwrap_err().to_string().contains("`auto`"));
        assert_eq!(Jobs::Count(3).count(), 3);
        assert!(Jobs::Auto.count() >= 1);
    }

    #[test]
    fn auto_leaves_each_job_enough_memory() {
        let gib = 1024 * 1024 * 1024;
        assert_eq!(auto_from(8, None), 8);
        assert_eq!(auto_from(8, Some(64 * gib)), 8);
        assert_eq!(auto_from(16, Some(6 * gib)), 4);
        // Never below one job, even when memory is short.
        assert_eq!(auto_from(4, Some(gib / 2)), 1);
    }

    #[test]
    fn reads_meminfo() {
        let meminfo = "MemTotal:       16303456 kB\nMemFree:          512000 kB\nMemAvailable:    8151728 kB\n";
        assert_eq!(meminfo_available(meminfo), Some(8151728 * 1024));
        assert_eq!(meminfo_available("MemTotal: 1 kB\n"), None);
    }
}
