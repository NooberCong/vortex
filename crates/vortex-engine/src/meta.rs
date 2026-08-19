//! The `.vxpart.meta` sidecar (02 §5).
//!
//! ```text
//! magic "VXPT" | version | flags
//! header:      final_url, origin_url, method, header set, validators, block size
//! checkpoint:  roaring bitmap of completed blocks
//! journal:     append-only run records since the last checkpoint
//! ```
//!
//! **Write ordering is the whole game:** data written → fsync(data) → append journal
//! record → fsync(meta). The reverse order produces a `.meta` claiming blocks that never
//! reached the platter, which is silent corruption that surfaces months later.
//!
//! It lives next to the destination file, not in the app directory, so a half-finished
//! download can be moved to another drive and still resume, and so a corrupted `vortex.db`
//! costs history rather than bytes.

use crate::error::Result;
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"VXPT";
const VERSION: u16 = 1;

/// Everything needed to rebuild a job from nothing but this file — which is exactly what
/// daemon rehydration does after a crash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaHeader {
    pub origin_url: String,
    pub final_url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub page_url: Option<String>,
    pub filename: String,
    pub total: u64,
    pub block_size: u32,
    /// Strong ETag preferred. A weak tag is recorded but never relied on alone for
    /// `If-Range` — across CDN nodes it is unusable for resume (02 §2).
    pub etag: Option<String>,
    pub weak_etag: bool,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    /// `(algorithm, expected)` from `Repr-Digest`, `x-amz-checksum-*`, `x-goog-hash`.
    pub digest: Option<(String, String)>,
    pub created_at: u64,
}

/// An open `.meta` file. Appends are cheap; checkpoints rewrite the file and truncate the
/// journal.
pub struct MetaFile {
    path: PathBuf,
    file: File,
    header: MetaHeader,
    /// Offset where the journal begins — everything after this is replayable runs.
    journal_start: u64,
    journal_bytes: u64,
}

impl MetaFile {
    pub fn meta_path(part: &Path) -> PathBuf {
        let mut s = part.as_os_str().to_os_string();
        s.push(".meta");
        PathBuf::from(s)
    }

    /// Creates (or truncates) the sidecar and writes header + empty checkpoint.
    pub fn create(path: &Path, header: MetaHeader) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        let mut meta = Self {
            path: path.to_owned(),
            file,
            header,
            journal_start: 0,
            journal_bytes: 0,
        };
        meta.rewrite(&RoaringBitmap::new())?;
        Ok(meta)
    }

    /// Opens an existing sidecar and replays its journal onto the checkpoint. A torn
    /// record at the tail is discarded; it costs at most a few re-fetched blocks.
    pub fn open(path: &Path) -> Result<(Self, RoaringBitmap)> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut buf = Vec::new();
        BufReader::new(&mut file).read_to_end(&mut buf)?;

        let mut cursor = 0usize;
        let need = |cursor: usize, n: usize, buf: &[u8]| -> Result<()> {
            if cursor + n > buf.len() {
                Err(crate::error::EngineError::integrity(
                    "the .meta sidecar is truncated",
                ))
            } else {
                Ok(())
            }
        };

        need(cursor, 12, &buf)?;
        if &buf[0..4] != MAGIC {
            return Err(crate::error::EngineError::integrity(
                "not a Vortex .meta file",
            ));
        }
        let version = u16::from_le_bytes([buf[4], buf[5]]);
        if version != VERSION {
            return Err(crate::error::EngineError::integrity(format!(
                "unsupported .meta version {version}"
            )));
        }
        let header_len = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as usize;
        cursor = 12;
        need(cursor, header_len, &buf)?;
        let header: MetaHeader = rmp_serde::from_slice(&buf[cursor..cursor + header_len])
            .map_err(|e| {
                crate::error::EngineError::integrity(format!("unreadable .meta header: {e}"))
            })?;
        cursor += header_len;

        need(cursor, 4, &buf)?;
        let cp_len = u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;
        need(cursor, cp_len, &buf)?;
        let mut bitmap = RoaringBitmap::deserialize_from(&buf[cursor..cursor + cp_len])
            .map_err(|e| {
                crate::error::EngineError::integrity(format!("unreadable .meta checkpoint: {e}"))
            })?;
        cursor += cp_len;

        let journal_start = cursor as u64;
        let mut journal_bytes = 0u64;
        // Replay. Stop at the first record that does not verify — the tail of a journal
        // written when the power went out is exactly this shape.
        while cursor + RECORD_LEN <= buf.len() {
            let rec = &buf[cursor..cursor + RECORD_LEN];
            let crc = u32::from_le_bytes(rec[8..12].try_into().unwrap());
            if crc32fast::hash(&rec[0..8]) != crc {
                break;
            }
            let start = u32::from_le_bytes(rec[0..4].try_into().unwrap());
            let count = u32::from_le_bytes(rec[4..8].try_into().unwrap());
            bitmap.insert_range(start..start.saturating_add(count));
            cursor += RECORD_LEN;
            journal_bytes += RECORD_LEN as u64;
        }

        // Drop anything after the last good record so the next append cannot land behind
        // garbage.
        let good_end = journal_start + journal_bytes;
        if (buf.len() as u64) > good_end {
            file.set_len(good_end)?;
        }
        file.seek(SeekFrom::End(0))?;

        Ok((
            Self {
                path: path.to_owned(),
                file,
                header,
                journal_start,
                journal_bytes,
            },
            bitmap,
        ))
    }

    pub fn header(&self) -> &MetaHeader {
        &self.header
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one run of newly durable blocks. The caller must already have fsynced the
    /// data file — this function is the second half of the ordering rule, never the first.
    pub fn append(&mut self, start: u32, count: u32) -> Result<()> {
        let mut rec = [0u8; RECORD_LEN];
        rec[0..4].copy_from_slice(&start.to_le_bytes());
        rec[4..8].copy_from_slice(&count.to_le_bytes());
        let crc = crc32fast::hash(&rec[0..8]);
        rec[8..12].copy_from_slice(&crc.to_le_bytes());
        self.file.write_all(&rec)?;
        self.journal_bytes += RECORD_LEN as u64;
        Ok(())
    }

    pub fn sync(&mut self) -> Result<()> {
        self.file.sync_data()?;
        Ok(())
    }

    /// Full bitmap checkpoint; truncates the journal. Every 30 s or 256 MiB, whichever
    /// comes first.
    pub fn checkpoint(&mut self, bitmap: &RoaringBitmap) -> Result<()> {
        self.rewrite(bitmap)?;
        self.sync()
    }

    /// Journal length in bytes — the job uses it to decide when a checkpoint is due.
    pub fn journal_len(&self) -> u64 {
        self.journal_bytes
    }

    fn rewrite(&mut self, bitmap: &RoaringBitmap) -> Result<()> {
        let header = rmp_serde::to_vec_named(&self.header).map_err(|e| {
            crate::error::EngineError::integrity(format!("cannot encode .meta header: {e}"))
        })?;
        let mut cp = Vec::new();
        bitmap.serialize_into(&mut cp).map_err(|e| {
            crate::error::EngineError::integrity(format!("cannot encode .meta checkpoint: {e}"))
        })?;

        let mut out = Vec::with_capacity(16 + header.len() + cp.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&(header.len() as u32).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(&(cp.len() as u32).to_le_bytes());
        out.extend_from_slice(&cp);

        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&out)?;
        self.file.set_len(out.len() as u64)?;
        self.file.seek(SeekFrom::End(0))?;
        self.journal_start = out.len() as u64;
        self.journal_bytes = 0;
        Ok(())
    }
}

const RECORD_LEN: usize = 12;

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> MetaHeader {
        MetaHeader {
            origin_url: "https://example.com/a.iso".into(),
            final_url: "https://cdn.example.com/a.iso".into(),
            method: "GET".into(),
            headers: vec![("User-Agent".into(), "Mozilla".into())],
            page_url: None,
            filename: "a.iso".into(),
            total: 10_000,
            block_size: 1_000,
            etag: Some("\"abc\"".into()),
            weak_etag: false,
            last_modified: None,
            content_type: None,
            digest: None,
            created_at: 0,
        }
    }

    #[test]
    fn journal_replays_onto_the_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.iso.vxpart.meta");

        let mut meta = MetaFile::create(&path, header()).unwrap();
        meta.append(0, 3).unwrap();
        meta.append(5, 2).unwrap();
        meta.sync().unwrap();
        drop(meta);

        let (meta, bits) = MetaFile::open(&path).unwrap();
        assert_eq!(bits.iter().collect::<Vec<_>>(), vec![0, 1, 2, 5, 6]);
        assert_eq!(meta.header().final_url, "https://cdn.example.com/a.iso");
    }

    #[test]
    fn a_torn_tail_record_is_discarded_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.iso.vxpart.meta");

        let mut meta = MetaFile::create(&path, header()).unwrap();
        meta.append(0, 4).unwrap();
        meta.sync().unwrap();
        drop(meta);

        // Half a record, as a power cut would leave it.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[9, 9, 9, 9, 1]).unwrap();
        drop(f);

        let (_, bits) = MetaFile::open(&path).unwrap();
        assert_eq!(bits.iter().collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn a_corrupt_record_stops_the_replay_rather_than_poisoning_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.iso.vxpart.meta");

        let mut meta = MetaFile::create(&path, header()).unwrap();
        meta.append(0, 2).unwrap();
        meta.sync().unwrap();
        drop(meta);

        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[7u8; 12]).unwrap(); // wrong CRC
        drop(f);

        let (_, bits) = MetaFile::open(&path).unwrap();
        assert_eq!(bits.iter().collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn checkpoint_truncates_the_journal_and_keeps_the_bits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.iso.vxpart.meta");

        let mut meta = MetaFile::create(&path, header()).unwrap();
        meta.append(0, 3).unwrap();
        let mut bits = RoaringBitmap::new();
        bits.insert_range(0..3);
        meta.checkpoint(&bits).unwrap();
        assert_eq!(meta.journal_len(), 0);
        meta.append(8, 1).unwrap();
        meta.sync().unwrap();
        drop(meta);

        let (_, replayed) = MetaFile::open(&path).unwrap();
        assert_eq!(replayed.iter().collect::<Vec<_>>(), vec![0, 1, 2, 8]);
    }
}
