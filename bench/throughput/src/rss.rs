//! Resident memory, sampled.
//!
//! The slow-disk fixture in 06 asserts a memory ceiling, and the writer's budget is a
//! semaphore — so the interesting question is not "does the semaphore hold permits" (it
//! does, by construction) but "does the process stay small when the network outruns the
//! disk". Only an end-to-end measurement answers that, because the leak this guards
//! against would be somewhere else entirely: a reqwest buffer, a channel, a `Vec` that
//! grows per block.
//!
//! Sampled rather than read from the kernel's own high-water mark, because that mark is
//! monotonic over the life of the process and this harness runs a dozen transfers back to
//! back. A peak that can never come down would report the largest fixture's number for
//! every fixture after it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Frequent enough to catch a spike inside a one-second transfer, rare enough to cost
/// nothing.
const INTERVAL: Duration = Duration::from_millis(20);

pub struct Watch {
    peak: Arc<AtomicU64>,
    base: u64,
    task: tokio::task::JoinHandle<()>,
}

impl Watch {
    /// Starts sampling, with the current footprint as zero. The origin's fixture body is
    /// already resident by the time a transfer starts, and it is not what is being
    /// measured — growth during the transfer is.
    pub fn start() -> Self {
        let base = current().unwrap_or(0);
        let peak = Arc::new(AtomicU64::new(base));
        let shared = peak.clone();
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(INTERVAL);
            loop {
                tick.tick().await;
                if let Some(now) = current() {
                    shared.fetch_max(now, Ordering::Relaxed);
                }
            }
        });
        Self { peak, base, task }
    }

    /// Bytes of growth over the baseline, or `None` on a platform this harness cannot
    /// measure — which is reported as "not measured" rather than as zero.
    pub fn stop(self) -> Option<u64> {
        self.task.abort();
        current()?;
        Some(self.peak.load(Ordering::Relaxed).saturating_sub(self.base))
    }
}

#[cfg(windows)]
pub fn current() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: `counters` is a correctly sized, zeroed `PROCESS_MEMORY_COUNTERS`, and the
    // pseudo-handle from `GetCurrentProcess` needs no closing.
    unsafe {
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) == 0 {
            return None;
        }
        Some(counters.WorkingSetSize as u64)
    }
}

#[cfg(target_os = "linux")]
pub fn current() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn current() -> Option<u64> {
    None
}
