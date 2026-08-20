//! Stage 5: moving a few thousand segments onto disk (04 §4-5).
//!
//! A segment set is a job with a different shape, not a different program. It uses the same
//! transport, the same envelope, the same adaptive-concurrency controller and the same
//! rate limiter as a file download; what changes is the unit (one segment instead of one
//! 1 MiB block) and the ordering (in order, so the muxer has a contiguous file).
//!
//! Three rules earn their place here:
//!
//! * **Fetch out of order, write in order.** A track file is built by strict appends, which
//!   is what makes resume a truncate and a counter rather than a bitmap.
//! * **A missing segment is not a job failure.** After its retries are spent it is recorded
//!   as a gap and the run carries on. Throwing away forty minutes of successful work over
//!   three bad segments out of eighteen hundred is the wrong trade (04 §5).
//! * **Data first, then the record of it.** The sidecar that says "n segments, m bytes" is
//!   only written after the bytes it describes are on the platter.

use crate::plan::{Payload, Segment, Track};
use crate::{crypto, net, Resource};
use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vortex_engine::concurrency::{Action, Controller};
use vortex_engine::error::{Class, EngineError};
use vortex_engine::ewma::Ewma;
use vortex_engine::job::{ControlRx, Engine};
use vortex_proto::RequestEnvelope;

use futures::stream::{FuturesUnordered, StreamExt};

/// Segment concurrency (04 §4-5). Higher than a file download's floor because segments are
/// small and request latency, not bandwidth, is the limit early on.
pub const START_CONNECTIONS: u8 = 8;
pub const MAX_CONNECTIONS: u8 = 24;
/// How many attempts a single segment gets before it is recorded as a gap.
const SEGMENT_ATTEMPTS: u32 = 5;
/// Out-of-order segments held in memory while the writer catches up. This is the whole of
/// the fetcher's memory footprint.
const REORDER_BUDGET: u64 = 64 << 20;
/// Progress and the durability record are both cheap; doing either per segment is not.
const CHECKPOINT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Segments appended to the file, including the init segment.
    pub done: u32,
    pub total: u32,
    pub missing: u32,
    pub bytes: u64,
}

/// Why a track stopped before it was finished.
#[derive(Debug)]
pub enum Interruption {
    /// The user paused, or the daemon is shutting down. Everything durable is on disk.
    Stopped,
    /// The link died. The caller asks the extension to re-mint it and starts again from
    /// the same counter — no segment is fetched twice.
    UrlExpired(String),
    /// Disk full, permission denied, path too long.
    Local(EngineError),
}

/// Fetches one track into `path`, resuming whatever is already there.
pub async fn track(
    engine: &Engine,
    envelope: &RequestEnvelope,
    track: &Track,
    path: &Path,
    max_connections: u8,
    control: &ControlRx,
    report: &mut (dyn FnMut(Stats) + Send),
) -> Result<Stats, Interruption> {
    let total = (track.segments.len() as u32).saturating_sub(track.speculative)
        + u32::from(track.init.is_some());
    let mut state = State::open(path, total).map_err(Interruption::Local)?;
    let mut stats = state.stats;
    report(stats);

    // Every distinct key up front. A key server that refuses is a fact worth learning
    // before a thousand segments have been fetched and cannot be decrypted.
    let keys = match load_keys(engine, envelope, track).await {
        Ok(keys) => keys,
        Err(e) if e.class == Class::Renewable => {
            return Err(Interruption::UrlExpired(e.context))
        }
        Err(e) => return Err(Interruption::Local(e)),
    };

    // The init segment is index 0 and is not optional: without `EXT-X-MAP` an fMP4 track is
    // a pile of fragments no player will open.
    if let (Some(init), 0) = (&track.init, stats.done) {
        if control.is_stopped() {
            return Err(Interruption::Stopped);
        }
        let init = fetch(engine, envelope, init, None, control)
            .await
            .map_err(|e| classify(e, "the init segment"))?;
        stats.bytes += state.append(&init.bytes).map_err(Interruption::Local)?;
        stats.done = 1;
        state.checkpoint(stats, true).map_err(Interruption::Local)?;
        report(stats);
    }

    let offset = u32::from(track.init.is_some());
    // Where the declared segments stop and the guesses begin.
    let firm = track.segments.len().saturating_sub(track.speculative as usize);
    let mut end = track.segments.len();
    let mut cursor = stats.done.saturating_sub(offset) as usize;
    let mut dispatch = cursor;
    let mut ready: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut buffered = 0u64;
    let mut inflight = FuturesUnordered::new();

    let mut controller = Controller::new(max_connections.min(MAX_CONNECTIONS), START_CONNECTIONS);
    let mut rate = Ewma::three_second();
    let mut pushback = false;
    let mut last_checkpoint = Instant::now();

    while cursor < end {
        while inflight.len() < controller.target() as usize
            && dispatch < end
            && buffered < REORDER_BUDGET
        {
            let index = dispatch;
            let segment = &track.segments[index];
            let key = segment.key.as_ref().and_then(|k| keys.get(&k.url).copied());
            inflight.push(async move {
                let result = fetch_segment(engine, envelope, segment, key, control).await;
                (index, result)
            });
            dispatch += 1;
        }

        if inflight.is_empty() {
            break;
        }

        let (index, result) = tokio::select! {
            Some(next) = inflight.next() => next,
            _ = control.stopped() => {
                state.checkpoint(stats, true).map_err(Interruption::Local)?;
                return Err(Interruption::Stopped);
            }
        };

        match result {
            Ok(arrived) => {
                rate.observe(arrived.bytes.len() as u64, Instant::now());
                buffered += arrived.bytes.len() as u64;
                pushback |= arrived.pushback;
                ready.insert(index, arrived.bytes);
            }
            Err(Failed::Renew(reason)) => {
                state.checkpoint(stats, true).map_err(Interruption::Local)?;
                return Err(Interruption::UrlExpired(reason));
            }
            Err(Failed::Gone { reason, pushback: pushed }) => {
                pushback |= pushed;
                if index >= firm {
                    // A segment the manifest never promised. Its absence is the end of the
                    // track, not a hole in it.
                    tracing::debug!(index, "the speculative tail ends here");
                    end = end.min(index);
                    ready.insert(index, Vec::new());
                } else if track.contiguous {
                    // One file cut into byte ranges. Skipping a block does not make the
                    // file shorter, it makes it unopenable — so everything contiguous
                    // already written stays on disk and the run stops for a retry, which
                    // picks up from exactly this block.
                    tracing::warn!(index, "a block is unavailable: {reason}");
                    state.checkpoint(stats, true).map_err(Interruption::Local)?;
                    return Err(Interruption::Stopped);
                } else {
                    // The gap is recorded, not fatal. The count is reported at the end:
                    // "3 of 1,800 segments unavailable" beats a discarded download.
                    tracing::warn!(index, "a segment is unavailable: {reason}");
                    ready.insert(index, Vec::new());
                    stats.missing += 1;
                }
            }
            Err(Failed::Local(e)) => return Err(Interruption::Local(e)),
        }

        // Drain everything that is now contiguous.
        while cursor < end {
            let Some(bytes) = ready.remove(&cursor) else { break };
            buffered = buffered.saturating_sub(bytes.len() as u64);
            let joined = join(track.payload, bytes, stats.done == 0);
            stats.bytes += state.append(&joined).map_err(Interruption::Local)?;
            stats.done = (cursor as u32 + offset + 1).max(stats.done);
            cursor += 1;
        }
        stats.total = stats.total.max(stats.done);

        if let Action::Remove(by) = controller.tick(rate.rate(Instant::now()), pushback, Instant::now())
        {
            tracing::debug!("segment concurrency down by {by}");
        }
        pushback = false;

        if last_checkpoint.elapsed() >= CHECKPOINT {
            state.checkpoint(stats, true).map_err(Interruption::Local)?;
            report(stats);
            last_checkpoint = Instant::now();
        }
    }

    state.checkpoint(stats, true).map_err(Interruption::Local)?;
    report(stats);
    Ok(stats)
}

/// Turns a fetched segment into the bytes that belong in the track file.
fn join(payload: Payload, bytes: Vec<u8>, first: bool) -> Vec<u8> {
    match payload {
        Payload::Append => bytes,
        Payload::WebVtt => crate::vtt::stitch(&bytes, first),
    }
}

fn classify(e: Failed, what: &str) -> Interruption {
    match e {
        Failed::Renew(reason) => Interruption::UrlExpired(reason),
        Failed::Local(e) => Interruption::Local(e),
        // The init segment is the one segment that cannot be a gap: without it the track
        // is unplayable, so a lost init stops the run rather than shrinking the file.
        Failed::Gone { reason, .. } => Interruption::Local(EngineError::new(
            Class::Transient,
            format!("{what} could not be fetched: {reason}"),
        )),
    }
}

/// What went wrong with one segment, and therefore what the run should do about it.
#[derive(Debug)]
enum Failed {
    /// The signature died. Everything stops until the extension re-mints the URL.
    Renew(String),
    /// Retries are spent. Recorded as a gap, not a failure.
    Gone {
        reason: String,
        /// The origin was rate-limiting us. True whether or not the segment arrived in
        /// the end, because the controller wants to know either way.
        pushback: bool,
    },
    Local(EngineError),
}

/// One segment, and what fetching it cost.
struct Arrived {
    bytes: Vec<u8>,
    pushback: bool,
}

async fn fetch_segment(
    engine: &Engine,
    envelope: &RequestEnvelope,
    segment: &Segment,
    key: Option<[u8; 16]>,
    control: &ControlRx,
) -> Result<Arrived, Failed> {
    let arrived = fetch(engine, envelope, &segment.resource, None, control).await?;
    let (Some(key), Some(spec)) = (key, segment.key.as_ref()) else {
        return Ok(arrived);
    };
    let bytes = crypto::decrypt(&key, &spec.iv, arrived.bytes)
        .map_err(|e| Failed::Local(EngineError::integrity(e)))?;
    Ok(Arrived {
        bytes,
        pushback: arrived.pushback,
    })
}

/// Fetches one resource, retrying transient failures with backoff. `limit` narrows the
/// size cap for things that are not segments.
async fn fetch(
    engine: &Engine,
    envelope: &RequestEnvelope,
    resource: &Resource,
    limit: Option<usize>,
    control: &ControlRx,
) -> Result<Arrived, Failed> {
    let limit = limit.unwrap_or(net::MAX_SEGMENT);
    let mut last = String::new();
    let mut pushback = false;
    for attempt in 0..SEGMENT_ATTEMPTS {
        if control.is_stopped() {
            return Err(Failed::Gone {
                reason: "stopped".to_owned(),
                pushback,
            });
        }
        match net::get(engine, envelope, resource, limit).await {
            Ok(fetched) => {
                return Ok(Arrived {
                    bytes: fetched.body,
                    pushback,
                })
            }
            Err(e) => {
                match e.class {
                    Class::Renewable => return Err(Failed::Renew(e.context)),
                    Class::Fatal | Class::Integrity => {
                        return Err(Failed::Gone {
                            reason: e.context,
                            pushback,
                        })
                    }
                    Class::Local => return Err(Failed::Local(e)),
                    Class::Transient => {}
                }
                // 429 and 503 are still transient — they mean "later", not "never". What
                // they change is the connection count, not whether the segment is fetched.
                pushback |= e.context.contains("429") || e.context.contains("503");
                last = e.context;
                let wait = Duration::from_millis(200u64 << attempt.min(5));
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = control.stopped() => {
                        return Err(Failed::Gone { reason: "stopped".to_owned(), pushback })
                    }
                }
            }
        }
    }
    Err(Failed::Gone {
        reason: last,
        pushback,
    })
}

/// Every distinct AES-128 key the track needs, fetched once.
async fn load_keys(
    engine: &Engine,
    envelope: &RequestEnvelope,
    track: &Track,
) -> Result<std::collections::HashMap<String, [u8; 16]>, EngineError> {
    let mut urls: Vec<&str> = track
        .segments
        .iter()
        .filter_map(|s| s.key.as_ref().map(|k| k.url.as_str()))
        .collect();
    urls.sort_unstable();
    urls.dedup();

    let mut keys = std::collections::HashMap::new();
    for url in urls {
        let fetched = net::get(engine, envelope, &Resource::whole(url), 4096).await?;
        let key = crypto::key_from_bytes(&fetched.body).map_err(EngineError::integrity)?;
        keys.insert(url.to_owned(), key);
    }
    Ok(keys)
}

// ─────────────────────────────────────────────────────────────────────────────
// Durability
// ─────────────────────────────────────────────────────────────────────────────

/// A track file plus the sidecar that says how much of it is real.
struct State {
    file: std::fs::File,
    sidecar: PathBuf,
    stats: Stats,
}

impl State {
    fn open(path: &Path, total: u32) -> Result<Self, EngineError> {
        let sidecar = sidecar_path(path);
        let mut stats = read_sidecar(&sidecar).unwrap_or_default();
        stats.total = total;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(local)?;
        }
        let mut file = std::fs::File::options()
            .create(true)
            .write(true)
            .read(true)
            .truncate(false)
            .open(path)
            .map_err(local)?;

        // Anything past the recorded length was in flight when the process stopped and is
        // not vouched for by anything. Truncating is what makes the append-only invariant
        // survive a kill.
        let length = file.metadata().map_err(local)?.len();
        if length != stats.bytes {
            file.set_len(stats.bytes).map_err(local)?;
        }
        file.seek(SeekFrom::Start(stats.bytes)).map_err(local)?;

        Ok(Self {
            file,
            sidecar,
            stats,
        })
    }

    fn append(&mut self, bytes: &[u8]) -> Result<u64, EngineError> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.file.write_all(bytes).map_err(local)?;
        Ok(bytes.len() as u64)
    }

    /// Data first, then the record of it.
    fn checkpoint(&mut self, stats: Stats, durable: bool) -> Result<(), EngineError> {
        self.file.flush().map_err(local)?;
        if durable {
            self.file.sync_data().map_err(local)?;
        }
        self.stats = stats;
        let body = format!(
            "{{\"version\":1,\"done\":{},\"bytes\":{},\"missing\":{}}}",
            stats.done, stats.bytes, stats.missing
        );
        std::fs::write(&self.sidecar, body).map_err(local)?;
        Ok(())
    }
}

fn sidecar_path(path: &Path) -> PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".vxseg");
    PathBuf::from(sidecar)
}

fn read_sidecar(path: &Path) -> Option<Stats> {
    let text = std::fs::read_to_string(path).ok()?;
    let number = |key: &str| -> Option<u64> {
        let at = text.find(&format!("\"{key}\":"))? + key.len() + 3;
        text[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok()
    };
    Some(Stats {
        done: number("done")? as u32,
        total: 0,
        missing: number("missing").unwrap_or(0) as u32,
        bytes: number("bytes")?,
    })
}

fn local(e: std::io::Error) -> EngineError {
    if vortex_engine::error::is_disk_full(&e) {
        return EngineError::with_source(Class::Local, "the disk is full", e);
    }
    EngineError::with_source(Class::Local, "the segment file could not be written", e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_resumes_from_the_recorded_length_not_the_file_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("video.ts");

        let mut state = State::open(&path, 10).unwrap();
        state.append(b"aaaa").unwrap();
        state
            .checkpoint(
                Stats {
                    done: 1,
                    total: 10,
                    missing: 0,
                    bytes: 4,
                },
                true,
            )
            .unwrap();
        // Bytes written after the checkpoint are exactly what a kill would leave behind.
        state.append(b"bbbbbb").unwrap();
        state.file.flush().unwrap();
        drop(state);

        let reopened = State::open(&path, 10).unwrap();
        assert_eq!(reopened.stats.done, 1);
        assert_eq!(reopened.stats.bytes, 4);
        assert_eq!(std::fs::read(&path).unwrap(), b"aaaa");
    }

    #[test]
    fn a_fresh_track_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open(&dir.path().join("a").join("video.mp4"), 7).unwrap();
        assert_eq!(
            state.stats,
            Stats {
                done: 0,
                total: 7,
                missing: 0,
                bytes: 0
            }
        );
    }

    #[test]
    fn the_sidecar_survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio.mp4");
        let mut state = State::open(&path, 3).unwrap();
        let stats = Stats {
            done: 2,
            total: 3,
            missing: 1,
            bytes: 4096,
        };
        state.checkpoint(stats, true).unwrap();
        let back = read_sidecar(&sidecar_path(&path)).unwrap();
        assert_eq!(back.done, 2);
        assert_eq!(back.bytes, 4096);
        assert_eq!(back.missing, 1);
    }
}
