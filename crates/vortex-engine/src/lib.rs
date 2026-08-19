//! The Vortex transfer core.
//!
//! Given a `RequestEnvelope`, produce a correct file on disk as fast as the network allows,
//! and never enter a state a restart cannot recover from. No UI, no IPC, no globals — the
//! daemon drives it, and the tests drive it just as easily.
//!
//! Reading order: [`probe`] decides what the server can do, [`scheduler`] decides who
//! fetches what, [`writer`] hides the disk, [`meta`] makes it survivable, and [`error`]
//! decides what any of it is allowed to do about a failure.

pub mod blocks;
pub mod concurrency;
pub mod error;
pub mod ewma;
pub mod fsalloc;
pub mod integrity;
pub mod job;
pub mod meta;
pub mod naming;
pub mod policy;
pub mod probe;
pub mod ratelimit;
pub mod scheduler;
pub mod transport;
pub mod writer;

pub use error::{Class, EngineError, Result};
pub use probe::Probe;
pub use transport::{Transport, TransportConfig};
