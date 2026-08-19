//! Rebuilding the list from what is on disk (01 §Failure isolation).
//!
//! `vortex.db` is a convenience. The authority is the `.vxpart.meta` sitting next to the
//! destination file: it names the URL, the validators, the block size and every block that
//! made it to the platter. If the database is lost, corrupted, or simply older than the
//! files, this scan finds the downloads anyway.
//!
//! Found downloads come back **paused**, not queued. Without the database there is no way
//! to know whether the user had paused them, and quietly resuming a transfer nobody asked
//! for is the worse of the two mistakes — the bytes are safe either way, and one click
//! continues.

use crate::jobs::{self, Intent, Job};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use vortex_engine::blocks::BlockMap;
use vortex_engine::meta::{MetaFile, MetaHeader};
use vortex_proto::{
    Category, JobId, JobSpec, JobState, JobView, Priority, ProgressFrame, RequestEnvelope,
    TransferMode,
};

/// How deep under a download root to look. A download folder with a few levels of
/// organisation is normal; a full recursive walk of a home directory is not, and startup
/// has to stay under 400 ms.
const MAX_DEPTH: usize = 3;

pub struct Found {
    header: MetaHeader,
    part: PathBuf,
    dest_dir: PathBuf,
    completed: u64,
}

/// Every directory a job could have landed in: the download root and any category
/// directory pointed somewhere else.
pub fn roots(settings: &vortex_proto::Settings) -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from(&settings.download_dir)];
    for category in Category::ALL {
        let dir = crate::settings::dest_dir(settings, category);
        if !roots.contains(&dir) {
            roots.push(dir);
        }
    }
    roots
}

/// Sidecars under `roots` that no known job already owns.
pub fn scan(roots: &[PathBuf], known: &[PathBuf]) -> Vec<Found> {
    let known: HashSet<&Path> = known.iter().map(PathBuf::as_path).collect();
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for root in roots {
        visit(root, 0, &mut |part: PathBuf| {
            if !known.contains(part.as_path()) && seen.insert(part.clone()) {
                if let Some(job) = read(&part) {
                    found.push(job);
                }
            }
        });
    }
    found
}

fn visit(dir: &Path, depth: usize, on_part: &mut impl FnMut(PathBuf)) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            visit(&path, depth + 1, on_part);
        } else if path.extension().is_some_and(|e| e == "meta") {
            let part = path.with_extension("");
            if part.extension().is_some_and(|e| e == "vxpart") {
                on_part(part);
            }
        }
    }
}

/// What a `.vxpart` is actually worth: the bytes in whole, journalled blocks.
///
/// The number in `vortex.db` is the last frame the UI was shown, which counts blocks still
/// in flight. Those blocks are re-fetched on resume, never trusted, so showing their bytes
/// as progress would be a small lie that corrects itself downward a moment later.
pub fn durable_progress(part: &Path) -> Option<u64> {
    read(part).map(|found| found.completed)
}

fn read(part: &Path) -> Option<Found> {
    // The data file is what holds the bytes. A sidecar without one describes a download
    // that never started, and is not worth showing anyone.
    if !part.exists() {
        return None;
    }
    let (meta, bitmap) = MetaFile::open(&MetaFile::meta_path(part)).ok()?;
    let header = meta.header().clone();
    let completed = if header.total > 0 {
        BlockMap::from_bitmap(header.total, header.block_size, bitmap).completed_bytes()
    } else {
        std::fs::metadata(part).map(|m| m.len()).unwrap_or(0)
    };
    Some(Found {
        dest_dir: part.parent().unwrap_or(Path::new(".")).to_path_buf(),
        part: part.to_path_buf(),
        header,
        completed,
    })
}

impl Found {
    pub fn into_job(self, id: JobId) -> Job {
        let mut envelope = RequestEnvelope::new(self.header.origin_url.clone());
        envelope.final_url = Some(self.header.final_url.clone());
        envelope.method = self.header.method.clone();
        // Whatever was safe to write down: `User-Agent`, `Referer`, and friends. The
        // credentials are gone, which is exactly why a resume that 403s asks the browser
        // for a fresh link rather than giving up (02 §6).
        envelope.headers = self.header.headers.clone();
        envelope.page_url = self.header.page_url.clone();
        envelope.content_length = (self.header.total > 0).then_some(self.header.total);
        envelope.mime_type = self.header.content_type.clone();
        envelope.filename_hint = Some(self.header.filename.clone());

        let filename = self.header.filename.clone();
        let dest = self.dest_dir.join(&filename);
        let spec = JobSpec {
            dest_dir: Some(self.dest_dir.to_string_lossy().into_owned()),
            filename: Some(filename.clone()),
            ..JobSpec::file(envelope)
        };
        let view = JobView {
            id,
            host: jobs::host_of(&self.header.final_url),
            url: self.header.final_url.clone(),
            filename,
            dest_path: dest.to_string_lossy().into_owned(),
            category: vortex_engine::naming::categorize(
                &self.header.filename,
                self.header.content_type.as_deref(),
            ),
            state: JobState::Paused,
            priority: Priority::Normal,
            total: (self.header.total > 0).then_some(self.header.total),
            completed: self.completed,
            mode: TransferMode::Single,
            protocol: None,
            addresses: 0,
            created_at: self.header.created_at,
            finished_at: None,
            verified: None,
            retries: Default::default(),
            media: None,
        };
        Job {
            view,
            spec,
            dest_dir: self.dest_dir,
            part: self.part,
            run: None,
            intent: Intent::Run,
            attempts: 0,
            retry_at: None,
            latest: ProgressFrame::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidecar(dir: &Path, name: &str, total: u64) -> PathBuf {
        let part = dir.join(format!("{name}.vxpart"));
        std::fs::write(&part, vec![0u8; 16]).unwrap();
        let header = MetaHeader {
            origin_url: format!("https://example.com/{name}"),
            final_url: format!("https://cdn.example.com/{name}"),
            method: "GET".into(),
            headers: vec![("Referer".into(), "https://example.com/".into())],
            page_url: Some("https://example.com/".into()),
            filename: name.to_owned(),
            total,
            block_size: 1 << 20,
            etag: Some("\"abc\"".into()),
            weak_etag: false,
            last_modified: None,
            content_type: Some("application/x-iso9660-image".into()),
            digest: None,
            created_at: 1_700_000_000,
        };
        MetaFile::create(&MetaFile::meta_path(&part), header).unwrap();
        part
    }

    #[test]
    fn a_lost_database_costs_history_and_not_bytes() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("ISOs");
        std::fs::create_dir_all(&nested).unwrap();
        let part = sidecar(&nested, "ubuntu.iso", 4 << 20);

        let found = scan(&[root.path().to_path_buf()], &[]);
        assert_eq!(found.len(), 1);
        let job = found.into_iter().next().unwrap().into_job(JobId(7));
        assert_eq!(job.view.filename, "ubuntu.iso");
        assert_eq!(job.view.total, Some(4 << 20));
        assert_eq!(job.view.host, "cdn.example.com");
        assert_eq!(job.part, part);
        assert_eq!(job.view.state, JobState::Paused);
        assert_eq!(
            job.spec.envelope.header("referer"),
            Some("https://example.com/")
        );
        assert!(job.spec.envelope.header("cookie").is_none());
    }

    #[test]
    fn a_job_the_daemon_already_knows_is_not_found_twice() {
        let root = tempfile::tempdir().unwrap();
        let part = sidecar(root.path(), "a.bin", 1 << 20);
        assert!(scan(&[root.path().to_path_buf()], &[part]).is_empty());
    }

    #[test]
    fn a_sidecar_with_no_data_file_describes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let part = sidecar(root.path(), "b.bin", 1 << 20);
        std::fs::remove_file(&part).unwrap();
        assert!(scan(&[root.path().to_path_buf()], &[]).is_empty());
    }
}
