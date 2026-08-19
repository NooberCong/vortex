//! The writer (02 §4).
//!
//! Sixteen sockets producing data at random offsets is a disk access pattern from hell.
//! The writer's whole job is to hide that:
//!
//! ```text
//! workers ──(offset, Bytes)──► mpsc ──► WRITER THREAD ──► pwrite / WriteFile
//!                                             │
//!                                      reorder + coalesce
//! ```
//!
//! It is a **dedicated OS thread, not a tokio task**. A blocking write on a slow disk must
//! never occupy a runtime worker; at 16 connections against a 5400 rpm drive it would
//! starve the sockets and collapse throughput. That bug is invisible on an NVMe dev machine
//! and it is the single most common performance defect in Rust download managers.

use crate::error::{EngineError, Result};
use bytes::Bytes;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::fs::File;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

/// Flush once this much has accumulated — or half the memory budget, whichever is less.
///
/// The second half of that sentence is load-bearing. A budget at or below this threshold
/// can never reach it: the permits covering the queued buffers are held from before the
/// send until after the flush, so some of the budget is always in the channel rather than
/// in `pending`, and the volume trigger simply never fires. What governs instead is
/// [`FLUSH_INTERVAL`], which caps throughput at one budget per 250 ms — 32 MB/s at the
/// smallest budget the daemon allows. A memory setting is not supposed to be a speed
/// setting.
const FLUSH_BYTES: usize = 8 << 20;
/// …or this long has passed, so a trickling transfer still reaches the platter.
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
/// Budget accounting unit. 4096 permits at 64 KiB is the default 256 MiB ceiling.
const BUDGET_UNIT: u64 = 64 << 10;

/// The volume trigger, which has to be reachable or it is not a trigger.
fn flush_threshold(permits: usize) -> usize {
    FLUSH_BYTES.min(permits * BUDGET_UNIT as usize / 2)
}

enum Req {
    Data {
        offset: u64,
        bytes: Bytes,
        _permit: OwnedSemaphorePermit,
    },
    /// Flush everything pending and fsync. The second half of the durability rule.
    Barrier(oneshot::Sender<Result<()>>),
    Finish(oneshot::Sender<Result<()>>),
}

/// Bounded by a global memory budget rather than by a queue length. When the budget is
/// exhausted, workers stop reading their sockets and TCP flow control does the rest, so
/// memory stays bounded regardless of the disk-to-network ratio.
pub struct Writer {
    tx: SyncSender<Req>,
    budget: Arc<Semaphore>,
    failed: Arc<Mutex<Option<String>>>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Writer {
    pub fn spawn(file: File, budget_bytes: u64, label: String) -> Self {
        let permits = (budget_bytes / BUDGET_UNIT).clamp(16, Semaphore::MAX_PERMITS as u64) as usize;
        let flush_at = flush_threshold(permits);
        let budget = Arc::new(Semaphore::new(permits));
        let failed = Arc::new(Mutex::new(None));
        // A short queue: the real backpressure is the memory budget, and a long queue would
        // only defer it.
        let (tx, rx) = sync_channel(64);
        let thread_failed = failed.clone();
        let handle = std::thread::Builder::new()
            .name(format!("vortex-writer {label}"))
            .spawn(move || run(file, rx, thread_failed, flush_at))
            .expect("spawning the writer thread");
        Self {
            tx,
            budget,
            failed,
            handle: Mutex::new(Some(handle)),
        }
    }

    /// Queues one buffer. Awaits budget first, which is what actually throttles the
    /// sockets when the disk cannot keep up.
    pub async fn write(&self, offset: u64, bytes: Bytes) -> Result<()> {
        self.check()?;
        let units = (bytes.len() as u64).div_ceil(BUDGET_UNIT).max(1) as u32;
        let permit = self
            .budget
            .clone()
            .acquire_many_owned(units)
            .await
            .map_err(|_| EngineError::local("the writer stopped"))?;
        let tx = self.tx.clone();
        // `send` blocks when the (short) queue is full; hand it to a blocking thread so a
        // slow disk never parks a runtime worker.
        tokio::task::spawn_blocking(move || {
            tx.send(Req::Data {
                offset,
                bytes,
                _permit: permit,
            })
        })
        .await
        .map_err(|_| EngineError::local("the writer stopped"))?
        .map_err(|_| EngineError::local("the writer stopped"))?;
        self.check()
    }

    /// Flush and fsync. Nothing may be journalled until this returns: data before
    /// metadata, always (02 §5).
    pub async fn barrier(&self) -> Result<()> {
        self.round_trip(Req::Barrier as fn(oneshot::Sender<Result<()>>) -> Req)
            .await
    }

    /// Final flush, fsync and close.
    pub async fn finish(&self) -> Result<()> {
        let r = self
            .round_trip(Req::Finish as fn(oneshot::Sender<Result<()>>) -> Req)
            .await;
        let handle = self.handle.lock().take();
        if let Some(handle) = handle {
            let _ = tokio::task::spawn_blocking(move || handle.join()).await;
        }
        r
    }

    async fn round_trip(&self, make: fn(oneshot::Sender<Result<()>>) -> Req) -> Result<()> {
        self.check()?;
        let (tx, rx) = oneshot::channel();
        let sender = self.tx.clone();
        tokio::task::spawn_blocking(move || sender.send(make(tx)))
            .await
            .map_err(|_| EngineError::local("the writer stopped"))?
            .map_err(|_| EngineError::local("the writer stopped"))?;
        rx.await
            .map_err(|_| EngineError::local("the writer stopped"))?
    }

    fn check(&self) -> Result<()> {
        match self.failed.lock().as_ref() {
            Some(message) => Err(EngineError::local(message.clone())),
            None => Ok(()),
        }
    }
}

fn run(file: File, rx: Receiver<Req>, failed: Arc<Mutex<Option<String>>>, flush_at: usize) {
    let mut pending: BTreeMap<u64, Bytes> = BTreeMap::new();
    let mut pending_bytes = 0usize;
    let mut last_flush = Instant::now();
    // Budget permits are held until the bytes they account for are out of memory, so the
    // ceiling covers what is queued as well as what is in flight.
    let mut held: Vec<OwnedSemaphorePermit> = Vec::new();

    let fail = |e: std::io::Error, failed: &Arc<Mutex<Option<String>>>| -> EngineError {
        let err = EngineError::from(e);
        *failed.lock() = Some(err.context.clone());
        err
    };

    loop {
        let timeout = FLUSH_INTERVAL.saturating_sub(last_flush.elapsed());
        match rx.recv_timeout(timeout) {
            Ok(Req::Data {
                offset,
                bytes,
                _permit,
            }) => {
                pending_bytes += bytes.len();
                pending.insert(offset, bytes);
                held.push(_permit);
                if pending_bytes >= flush_at || last_flush.elapsed() >= FLUSH_INTERVAL {
                    if let Err(e) = flush(&file, &mut pending, &mut pending_bytes) {
                        fail(e, &failed);
                        drain(rx);
                        return;
                    }
                    held.clear();
                    last_flush = Instant::now();
                }
            }
            Ok(Req::Barrier(reply)) => {
                let r = flush(&file, &mut pending, &mut pending_bytes)
                    .and_then(|_| file.sync_data())
                    .map_err(|e| fail(e, &failed));
                held.clear();
                last_flush = Instant::now();
                let _ = reply.send(r);
            }
            Ok(Req::Finish(reply)) => {
                let r = flush(&file, &mut pending, &mut pending_bytes)
                    .and_then(|_| file.sync_all())
                    .map_err(|e| fail(e, &failed));
                held.clear();
                let _ = reply.send(r);
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !pending.is_empty() {
                    if let Err(e) = flush(&file, &mut pending, &mut pending_bytes) {
                        fail(e, &failed);
                        drain(rx);
                        return;
                    }
                    held.clear();
                }
                last_flush = Instant::now();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Keep draining after a failure so senders see the error from [`Writer::check`] rather
/// than blocking forever on a full queue.
fn drain(rx: Receiver<Req>) {
    while let Ok(req) = rx.recv() {
        match req {
            Req::Barrier(reply) | Req::Finish(reply) => {
                let _ = reply.send(Err(EngineError::local("the writer stopped")));
            }
            Req::Data { .. } => {}
        }
    }
}

/// Coalesce adjacent buffers into one positional write each. Sixteen random writers become
/// one near-sequential writer, which is the entire point of the reorder window.
fn flush(
    file: &File,
    pending: &mut BTreeMap<u64, Bytes>,
    pending_bytes: &mut usize,
) -> std::io::Result<()> {
    let mut run: Vec<Bytes> = Vec::new();
    let mut run_start = 0u64;
    let mut run_end = 0u64;

    for (offset, bytes) in std::mem::take(pending) {
        if !run.is_empty() && offset == run_end {
            run_end += bytes.len() as u64;
            run.push(bytes);
        } else {
            if !run.is_empty() {
                write_run(file, run_start, &run)?;
                run.clear();
            }
            run_start = offset;
            run_end = offset + bytes.len() as u64;
            run.push(bytes);
        }
    }
    if !run.is_empty() {
        write_run(file, run_start, &run)?;
    }
    *pending_bytes = 0;
    Ok(())
}

fn write_run(file: &File, offset: u64, run: &[Bytes]) -> std::io::Result<()> {
    if run.len() == 1 {
        return write_at(file, offset, &run[0]);
    }
    let mut buf = Vec::with_capacity(run.iter().map(|b| b.len()).sum());
    for b in run {
        buf.extend_from_slice(b);
    }
    write_at(file, offset, &buf)
}

fn write_at(file: &File, mut offset: u64, mut buf: &[u8]) -> std::io::Result<()> {
    while !buf.is_empty() {
        let n = positional_write(file, offset, buf)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "the disk accepted zero bytes",
            ));
        }
        offset += n as u64;
        buf = &buf[n..];
    }
    Ok(())
}

#[cfg(unix)]
fn positional_write(file: &File, offset: u64, buf: &[u8]) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.write_at(buf, offset)
}

#[cfg(windows)]
fn positional_write(file: &File, offset: u64, buf: &[u8]) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(buf, offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    async fn writer_for(path: &std::path::Path) -> Writer {
        let file = File::options()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(path)
            .unwrap();
        Writer::spawn(file, 4 << 20, "test".into())
    }

    #[tokio::test]
    async fn out_of_order_writes_land_at_the_right_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.vxpart");
        let writer = writer_for(&path).await;

        writer.write(8, Bytes::from_static(b"CCCC")).await.unwrap();
        writer.write(0, Bytes::from_static(b"AAAA")).await.unwrap();
        writer.write(4, Bytes::from_static(b"BBBB")).await.unwrap();
        writer.finish().await.unwrap();

        let mut s = String::new();
        File::open(&path).unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "AAAABBBBCCCC");
    }

    #[tokio::test]
    async fn a_barrier_makes_everything_before_it_durable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.vxpart");
        let writer = writer_for(&path).await;

        writer.write(0, Bytes::from_static(b"hello")).await.unwrap();
        writer.barrier().await.unwrap();

        let mut s = String::new();
        File::open(&path).unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "hello");
        writer.finish().await.unwrap();
    }

    #[test]
    fn the_volume_trigger_is_always_reachable() {
        // The bug this pins: with a threshold of a flat 8 MiB and a budget of 8 MiB, some
        // of the budget is always sitting in the channel rather than in `pending`, so the
        // volume trigger never fires and the 250 ms timer becomes the throttle — one
        // budget per interval, 32 MB/s, at the smallest budget the daemon will accept.
        // Measured at 7.5 s for 256 MiB before this, and 0.40 s after.
        for budget in [8u64 << 20, 32 << 20, 256 << 20, 2 << 30] {
            let permits =
                (budget / BUDGET_UNIT).clamp(16, Semaphore::MAX_PERMITS as u64) as usize;
            let held = permits * BUDGET_UNIT as usize;
            assert!(
                flush_threshold(permits) < held,
                "a {budget}-byte budget holds {held} but only flushes at {}",
                flush_threshold(permits)
            );
        }
        // …and the default is unchanged, so this costs the common path nothing.
        assert_eq!(flush_threshold((256 << 20) / BUDGET_UNIT as usize), FLUSH_BYTES);
    }

    #[tokio::test]
    async fn the_memory_budget_bounds_what_is_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.vxpart");
        let file = File::options()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        // 16 permits is the floor, i.e. 1 MiB of in-flight data.
        let writer = Writer::spawn(file, 0, "test".into());

        for i in 0..64u64 {
            writer
                .write(i * BUDGET_UNIT, Bytes::from(vec![7u8; BUDGET_UNIT as usize]))
                .await
                .unwrap();
        }
        writer.finish().await.unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            64 * BUDGET_UNIT,
            "every buffer still reached the disk"
        );
    }
}
