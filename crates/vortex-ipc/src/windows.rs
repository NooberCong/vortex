//! Windows: `\\.\pipe\vortex.SID`, with a DACL that names exactly one account.
//!
//! The pipe is created with a *protected* DACL (`D:P`) carrying a single ACE for the
//! owning user's SID. Protected means no inherited ACEs sneak in, so "only this user" is
//! a property of the object rather than a property of the default token — which is what
//! makes the "page JS → daemon: impossible" row in 01 §Security boundaries true.

use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::ptr;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, HANDLE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
#[cfg(test)]
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub type ClientStream = NamedPipeClient;
pub type ServerStream = NamedPipeServer;

pub fn endpoint() -> io::Result<String> {
    static SID: OnceLock<io::Result<String>> = OnceLock::new();
    match SID.get_or_init(current_user_sid) {
        Ok(sid) => Ok(format!(r"\\.\pipe\vortex.{sid}")),
        Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
    }
}

pub async fn connect(addr: &str) -> io::Result<ClientStream> {
    // A busy pipe means every instance is momentarily taken, not that the daemon is gone.
    // The listener always keeps one spare, so this only ever waits through a burst.
    for _ in 0..20 {
        match ClientOptions::new().open(addr) {
            Ok(client) => return Ok(client),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => return Err(e),
        }
    }
    ClientOptions::new().open(addr)
}

pub struct Listener {
    addr: String,
    descriptor: SecurityDescriptor,
    /// The instance currently waiting for a client. A named pipe server has to exist
    /// before a client can connect, so there is always exactly one spare.
    idle: NamedPipeServer,
}

impl Listener {
    pub fn bind(addr: &str) -> io::Result<Self> {
        let descriptor = SecurityDescriptor::for_current_user()?;
        let idle = create(addr, &descriptor, true).map_err(|e| {
            // `first_pipe_instance` against a name someone else already owns comes back as
            // access-denied. For our purposes that is "a daemon is already running".
            if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                io::Error::new(io::ErrorKind::AddrInUse, format!("{addr} is already in use"))
            } else {
                e
            }
        })?;
        Ok(Self {
            addr: addr.to_owned(),
            descriptor,
            idle,
        })
    }

    pub async fn accept(&mut self) -> io::Result<ServerStream> {
        self.idle.connect().await?;
        let spare = create(&self.addr, &self.descriptor, false)?;
        Ok(std::mem::replace(&mut self.idle, spare))
    }
}

fn create(addr: &str, sd: &SecurityDescriptor, first: bool) -> io::Result<NamedPipeServer> {
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    // SAFETY: `attributes` is valid for the duration of the call, and the descriptor it
    // points at outlives it — the `Listener` owns it.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                addr,
                &mut attributes as *mut SECURITY_ATTRIBUTES as *mut c_void,
            )
    }
}

/// A `LocalAlloc`ed security descriptor, freed on drop.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the pointer is only read by the kernel during `CreateNamedPipe`, and is freed
// exactly once, on drop.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    fn for_current_user() -> io::Result<Self> {
        let sid = endpoint_sid()?;
        // D:P     protected DACL — inherited ACEs are not merged in
        // (A;;GA;;;SID)   allow generic-all, to this user and nobody else
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut psd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `sddl` is a NUL-terminated UTF-16 string; `psd` receives an allocation
        // we take ownership of.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(psd))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

fn endpoint_sid() -> io::Result<String> {
    static SID: OnceLock<io::Result<String>> = OnceLock::new();
    match SID.get_or_init(current_user_sid) {
        Ok(s) => Ok(s.clone()),
        Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
    }
}

/// The SID of the account this process runs as, in `S-1-5-21-…` form.
fn current_user_sid() -> io::Result<String> {
    // SAFETY: every out-parameter is a live local, every handle is closed on the way out,
    // and the token buffer is sized by the first (deliberately failing) query.
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle(token);

        let mut needed = 0u32;
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        // u64-aligned: TOKEN_USER contains a pointer, and a Vec<u8> guarantees nothing.
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        if GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr() as *mut c_void,
            needed,
            &mut needed,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }

        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut raw: *mut u16 = ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut raw) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0usize;
        while *raw.add(len) != 0 {
            len += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(raw, len));
        LocalFree(raw as *mut c_void);
        Ok(sid)
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: opened by OpenProcessToken, closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;

    /// The security boundary, read back from the kernel rather than assumed.
    ///
    /// "A page cannot open a named pipe" is only half the claim in 01 §Security boundaries;
    /// the other half is that no *other account* on the machine can either. That is a
    /// property of the DACL, so this asserts on the DACL the object actually ended up with.
    #[tokio::test]
    async fn the_pipe_admits_this_user_and_nobody_else() {
        let addr = format!(r"\\.\pipe\vortex-dacl.{}", std::process::id());
        let listener = Listener::bind(&addr).expect("bind");
        let sid = current_user_sid().expect("sid");

        // SAFETY: the handle is owned by `listener` and outlives the call; both allocations
        // returned are freed below.
        let sddl = unsafe {
            let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
            let mut dacl = ptr::null_mut();
            let status = GetSecurityInfo(
                listener.idle.as_raw_handle(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            );
            assert_eq!(status, 0, "GetSecurityInfo failed");

            let mut text: *mut u16 = ptr::null_mut();
            let mut len = 0u32;
            let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                &mut len,
            );
            assert_ne!(ok, 0, "ConvertSecurityDescriptorToStringSecurityDescriptorW failed");
            let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(text, len as usize));
            LocalFree(text as *mut c_void);
            LocalFree(descriptor);
            sddl
        };

        assert!(sddl.starts_with("D:P"), "the DACL is not protected: {sddl}");
        assert!(sddl.contains(&sid), "this user is not on the DACL: {sddl}");
        assert_eq!(
            sddl.matches("(A;").count(),
            1,
            "the pipe grants access to more than one account: {sddl}"
        );
    }
}
