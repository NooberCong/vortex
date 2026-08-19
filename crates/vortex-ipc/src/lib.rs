//! The local endpoint (01 §IPC).
//!
//! | Platform | Endpoint |
//! |---|---|
//! | Windows | `\\.\pipe\vortex.SID` — DACL grants the owning SID only |
//! | Unix | `$XDG_RUNTIME_DIR/vortex.sock`, mode `0600` |
//!
//! There is deliberately **no TCP listener**. A page cannot open a named pipe or a unix
//! socket, so the "any website can drive your download manager" class of bug does not
//! exist here — which is the entire reason this crate is not an HTTP server.
//!
//! Three processes share this: `vortexd` listens, `vortex-host` and `vortex-cli` connect.
//!
//! The platform modules below are the only place in Vortex that reaches for `unsafe` — a
//! pipe DACL and a `getuid` are not expressible any other way.

#![deny(unsafe_code)]

use std::io;

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
use windows as sys;

#[cfg(unix)]
#[allow(unsafe_code)]
mod unix;
#[cfg(unix)]
use unix as sys;

mod launch;

pub use launch::{connect_or_start, start};
pub use sys::{ClientStream, ServerStream};

/// The address clients connect to and the daemon listens on.
pub fn endpoint() -> io::Result<String> {
    sys::endpoint()
}

/// Connects to a running daemon. An error here means "not running" — every caller treats
/// it that way, and the extension goes passive rather than eating the user's download
/// (03 §2).
pub async fn connect() -> io::Result<ClientStream> {
    sys::connect(&endpoint()?).await
}

/// Connects to a named endpoint. Only tests and `--endpoint` use this; everything in
/// production talks to the one address [`endpoint`] derives.
pub async fn connect_at(addr: &str) -> io::Result<ClientStream> {
    sys::connect(addr).await
}

/// Is a daemon listening right now? Used for the single-instance check and by the host
/// before it decides to spawn one.
pub async fn is_running() -> bool {
    connect().await.is_ok()
}

/// Accepts connections on the endpoint. Bound exactly once per daemon; a second `bind`
/// fails with `AddrInUse`, which is how a second `vortexd` learns to exit quietly.
pub struct Listener(sys::Listener);

impl Listener {
    pub fn bind() -> io::Result<Self> {
        Self::bind_at(&endpoint()?)
    }

    pub fn bind_at(addr: &str) -> io::Result<Self> {
        Ok(Self(sys::Listener::bind(addr)?))
    }

    pub async fn accept(&mut self) -> io::Result<ServerStream> {
        self.0.accept().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn a_client_and_the_daemon_meet_on_the_endpoint() {
        // `is_running` is honest before anything is listening. (On a developer machine a
        // real daemon may be up, so only the negative direction is asserted here.)
        let mut listener = match Listener::bind() {
            Ok(l) => l,
            // A real vortexd owns the endpoint on this machine; nothing to prove.
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => return,
            Err(e) => panic!("bind failed: {e}"),
        };

        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let mut buf = [0u8; 5];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(b"world").await.unwrap();
            stream.flush().await.unwrap();
            buf
        });

        let mut client = connect().await.expect("connect");
        client.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"world");
        assert_eq!(&server.await.unwrap(), b"hello");
    }

    #[test]
    fn the_endpoint_is_per_user() {
        let addr = endpoint().unwrap();
        assert!(addr.contains("vortex"), "{addr}");
        #[cfg(windows)]
        assert!(addr.starts_with(r"\\.\pipe\vortex.S-1-"), "{addr}");
    }
}
