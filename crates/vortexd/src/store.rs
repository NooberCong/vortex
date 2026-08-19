//! `vortex.db` — SQLite in WAL mode (01 §State ownership).
//!
//! What lives here is *history and convenience*: the job list, and what the engine has
//! learned about each origin. What does **not** live here is anything a transfer depends
//! on. Every byte of resumable state is in the `.vxpart.meta` sidecar next to the file, so
//! losing this database costs the user their list, never their download.
//!
//! Credentials never reach it. Cookies and `Authorization` headers are stripped on the way
//! in ([`redact`]); a job that resumes after a restart re-acquires them from the browser
//! through URL renewal, which is exactly the path that already exists for expiry.
//!
//! Every statement here runs on the daemon's command loop. That is deliberate: these are
//! sub-millisecond local writes, and moving them to a blocking pool would only buy a race
//! between the loop's idea of the world and the disk's.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use vortex_engine::policy::OriginPolicy;
use vortex_proto::{JobId, JobSpec, JobView};

const SCHEMA_VERSION: i64 = 1;

/// A job as it is written down. The runtime job holds more (a control handle, the live
/// progress frame); none of that outlives the process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredJob {
    pub view: JobView,
    pub spec: JobSpec,
    pub dest_dir: String,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        Self::prepare(conn)
    }

    /// Used by tests, and by `--ephemeral` runs that must not touch the user's real list.
    pub fn in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self> {
        // WAL so a reader (a future `vortex-cli history`) never blocks the daemon, and
        // NORMAL because losing the last few list updates to a power cut is survivable —
        // the sidecars are the durable record, not this.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            "create table if not exists meta (
                 key   text primary key,
                 value text not null
             );
             create table if not exists jobs (
                 id         integer primary key,
                 record     text    not null,
                 updated_at integer not null
             );
             create table if not exists origin_policy (
                 origin     text primary key,
                 record     text    not null,
                 updated_at integer not null
             );",
        )?;
        let store = Self { conn };
        match store.meta("schema_version")? {
            None => store.set_meta("schema_version", &SCHEMA_VERSION.to_string())?,
            Some(found) if found != SCHEMA_VERSION.to_string() => {
                anyhow::bail!(
                    "vortex.db was written by schema {found}, this build speaks {SCHEMA_VERSION}"
                )
            }
            Some(_) => {}
        }
        Ok(store)
    }

    fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("select value from meta where key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .optional()?)
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "insert into meta(key, value) values(?1, ?2)
             on conflict(key) do update set value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    /// Ids are never recycled, even after every job is removed: a stale client holding an
    /// id must not be able to address a different job with it.
    pub fn next_job_id(&self) -> Result<JobId> {
        let next: u64 = self
            .meta("next_job_id")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        self.set_meta("next_job_id", &(next + 1).to_string())?;
        Ok(JobId(next))
    }

    pub fn save_job(&self, job: &StoredJob) -> Result<()> {
        let record = serde_json::to_string(&StoredJob {
            view: job.view.clone(),
            spec: redact(&job.spec),
            dest_dir: job.dest_dir.clone(),
        })?;
        self.conn.execute(
            "insert into jobs(id, record, updated_at) values(?1, ?2, ?3)
             on conflict(id) do update set record = excluded.record, updated_at = excluded.updated_at",
            rusqlite::params![job.view.id.0, record, now()],
        )?;
        Ok(())
    }

    pub fn delete_job(&self, id: JobId) -> Result<()> {
        self.conn
            .execute("delete from jobs where id = ?1", [id.0])?;
        Ok(())
    }

    /// Oldest first, so the list rehydrates in the order the user created it.
    pub fn jobs(&self) -> Result<Vec<StoredJob>> {
        let mut stmt = self
            .conn
            .prepare("select id, record from jobs order by id asc")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut jobs = Vec::new();
        for row in rows {
            let (id, record) = row?;
            match serde_json::from_str::<StoredJob>(&record) {
                Ok(job) => jobs.push(job),
                // One unreadable row (an older build, a truncated write) must not cost the
                // whole list. Drop it and carry on; the sidecar scan may find it anyway.
                Err(e) => tracing::warn!(job = id, "skipping an unreadable job record: {e}"),
            }
        }
        Ok(jobs)
    }

    pub fn save_policy(&self, entries: &[(String, OriginPolicy)]) -> Result<()> {
        let stamp = now();
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "insert into origin_policy(origin, record, updated_at) values(?1, ?2, ?3)
                 on conflict(origin) do update set record = excluded.record, updated_at = excluded.updated_at",
            )?;
            for (origin, policy) in entries {
                stmt.execute(rusqlite::params![
                    origin,
                    serde_json::to_string(policy)?,
                    stamp
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn policy(&self) -> Result<Vec<(String, OriginPolicy)>> {
        let mut stmt = self
            .conn
            .prepare("select origin, record from origin_policy")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (origin, record) = row?;
            if let Ok(policy) = serde_json::from_str(&record) {
                out.push((origin, policy));
            }
        }
        Ok(out)
    }
}

/// Headers that are credentials. They live in memory for the life of the job and are
/// never written to `vortex.db`, to a log, or to a `.vxpart.meta`.
const SECRETS: [&str; 3] = ["cookie", "authorization", "proxy-authorization"];

pub fn redact(spec: &JobSpec) -> JobSpec {
    let mut spec = spec.clone();
    spec.envelope.cookies = None;
    spec.envelope
        .headers
        .retain(|(name, _)| !SECRETS.iter().any(|s| name.eq_ignore_ascii_case(s)));
    spec
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vortex_proto::{Category, JobState, Priority, RequestEnvelope, TransferMode};

    fn sample(id: u64) -> StoredJob {
        let mut envelope = RequestEnvelope::new("https://example.com/a.iso");
        envelope.cookies = Some("session=secret".into());
        envelope.set_header("Cookie", "session=secret");
        envelope.set_header("Authorization", "Bearer secret");
        envelope.set_header("User-Agent", "Mozilla/5.0");
        StoredJob {
            view: JobView {
                id: JobId(id),
                filename: "a.iso".into(),
                dest_path: "/tmp/a.iso".into(),
                url: "https://example.com/a.iso".into(),
                host: "example.com".into(),
                category: Category::Archives,
                state: JobState::Queued,
                priority: Priority::Normal,
                total: Some(100),
                completed: 0,
                mode: TransferMode::Single,
                protocol: None,
                addresses: 0,
                created_at: 0,
                finished_at: None,
                verified: None,
                retries: Default::default(),
                media: None,
            },
            spec: JobSpec::file(envelope),
            dest_dir: "/tmp".into(),
        }
    }

    #[test]
    fn credentials_never_reach_the_database() {
        let store = Store::in_memory().unwrap();
        store.save_job(&sample(1)).unwrap();

        let back = store.jobs().unwrap();
        assert_eq!(back.len(), 1);
        let envelope = &back[0].spec.envelope;
        assert!(envelope.cookies.is_none());
        assert!(envelope.header("cookie").is_none());
        assert!(envelope.header("authorization").is_none());
        assert_eq!(envelope.header("user-agent"), Some("Mozilla/5.0"));
    }

    #[test]
    fn job_ids_are_never_recycled() {
        let store = Store::in_memory().unwrap();
        let first = store.next_job_id().unwrap();
        let second = store.next_job_id().unwrap();
        assert_eq!(second.0, first.0 + 1);

        store.save_job(&sample(second.0)).unwrap();
        store.delete_job(second).unwrap();
        assert_eq!(store.next_job_id().unwrap().0, second.0 + 1);
    }

    #[test]
    fn what_the_engine_learned_about_an_origin_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vortex.db");
        {
            let store = Store::open(&path).unwrap();
            store
                .save_policy(&[(
                    "https://example.com".into(),
                    OriginPolicy {
                        ranges: Some(true),
                        plateau: Some(9),
                        ..Default::default()
                    },
                )])
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let policy = store.policy().unwrap();
        assert_eq!(policy.len(), 1);
        assert_eq!(policy[0].1.plateau, Some(9));
    }
}
