//! Unix: `$XDG_RUNTIME_DIR/vortex.sock`, mode `0600`.
//!
//! `XDG_RUNTIME_DIR` is already user-owned and `0700`, and is cleaned up at logout, which
//! is exactly the lifetime of the daemon. When it is absent (a bare `ssh` session, some
//! macOS setups) the socket falls back to the temp directory with the uid in the name, and
//! the `0600` mode is what carries the boundary.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tokio::net::{UnixListener, UnixStream};

pub type ClientStream = UnixStream;
pub type ServerStream = UnixStream;

pub fn endpoint() -> io::Result<String> {
    let path = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("vortex.sock"),
        // SAFETY-adjacent: the uid is in the name so two users never collide in /tmp.
        None => std::env::temp_dir().join(format!("vortex-{}.sock", uid())),
    };
    path.into_os_string()
        .into_string()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the endpoint path is not UTF-8"))
}

fn uid() -> u32 {
    // SAFETY: `getuid` is always safe; it reads a process attribute and cannot fail.
    unsafe { libc::getuid() }
}

pub async fn connect(addr: &str) -> io::Result<ClientStream> {
    UnixStream::connect(addr).await
}

pub struct Listener(UnixListener);

impl Listener {
    pub fn bind(addr: &str) -> io::Result<Self> {
        // A socket file left behind by a killed daemon is not a running daemon. Probing it
        // is the only way to tell the difference, so probe before unlinking.
        if std::path::Path::new(addr).exists() {
            match std::os::unix::net::UnixStream::connect(addr) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("{addr} is already in use"),
                    ))
                }
                Err(_) => std::fs::remove_file(addr)?,
            }
        }
        let listener = UnixListener::bind(addr)?;
        std::fs::set_permissions(addr, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self(listener))
    }

    pub async fn accept(&mut self) -> io::Result<ServerStream> {
        let (stream, _) = self.0.accept().await?;
        Ok(stream)
    }
}
