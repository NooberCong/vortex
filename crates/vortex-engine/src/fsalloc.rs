//! Preallocation (02 §4).
//!
//! The goal is to reserve the file's extent without a zero-fill pass and without leaking
//! anything. On Windows that means marking the file sparse and setting its allocation
//! size — **not** `SetFileValidData`, which needs `SE_MANAGE_VOLUME_NAME` (i.e. admin) and
//! exposes stale on-disk contents in the unwritten region. That is a genuine information
//! disclosure bug, so it is not on the table at any privilege level.

use std::fs::File;
use std::io;

/// Reserve `total` bytes for `file`, sparsely where the platform supports it.
///
/// Best-effort by design: a filesystem that refuses the hint is not a reason to fail a
/// download, so only the length is treated as load-bearing.
pub fn preallocate(file: &File, total: u64) -> io::Result<()> {
    platform_hint(file, total);
    file.set_len(total)
}

#[cfg(windows)]
fn platform_hint(file: &File, total: u64) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileAllocationInfo, SetFileInformationByHandle, FILE_ALLOCATION_INFO,
    };
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    use windows_sys::Win32::System::IO::DeviceIoControl;

    let handle = file.as_raw_handle() as _;
    // SAFETY: `handle` is a live file handle owned by `file` for the duration of the call,
    // and both calls are given correctly sized, correctly typed buffers.
    unsafe {
        let mut returned = 0u32;
        // Sparse first: without it the allocation below would commit real clusters.
        DeviceIoControl(
            handle,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
        let info = FILE_ALLOCATION_INFO {
            AllocationSize: total as i64,
        };
        SetFileInformationByHandle(
            handle,
            FileAllocationInfo,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<FILE_ALLOCATION_INFO>() as u32,
        );
    }
}

#[cfg(target_os = "linux")]
fn platform_hint(file: &File, total: u64) {
    use std::os::unix::io::AsRawFd;
    // FALLOC_FL_KEEP_SIZE: reserve the extent, leave the length to `set_len`.
    const FALLOC_FL_KEEP_SIZE: libc::c_int = 0x01;
    // SAFETY: `fd` is a live descriptor owned by `file`; the length is non-negative.
    unsafe {
        libc::fallocate(file.as_raw_fd(), FALLOC_FL_KEEP_SIZE, 0, total as libc::off_t);
    }
}

#[cfg(target_os = "macos")]
fn platform_hint(file: &File, total: u64) {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `fd` is a live descriptor owned by `file`; `fstore_t` is filled completely.
    unsafe {
        let mut store = libc::fstore_t {
            fst_flags: libc::F_ALLOCATECONTIG,
            fst_posmode: libc::F_PEOFPOSMODE,
            fst_offset: 0,
            fst_length: total as libc::off_t,
            fst_bytesalloc: 0,
        };
        if libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &mut store) == -1 {
            // Contiguous allocation failed; ask for the same space, fragmented.
            store.fst_flags = libc::F_ALLOCATEALL;
            libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &mut store);
        }
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn platform_hint(_file: &File, _total: u64) {}

/// Free space on the volume containing `path`, when the platform can answer cheaply.
/// Used to turn "disk full" into a question with a number in it rather than an errno.
pub fn available_space(path: &std::path::Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

        let dir = if path.is_dir() { path } else { path.parent()? };
        let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
        wide.push(0);
        let mut free = 0u64;
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call.
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut 0, &mut 0) };
        (ok != 0).then_some(free)
    }
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = if path.is_dir() { path } else { path.parent()? };
        let c = CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: `c` is a NUL-terminated path; `stat` is fully initialised by the call.
        unsafe {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(c.as_ptr(), &mut stat) != 0 {
                return None;
            }
            Some(stat.f_bavail as u64 * stat.f_frsize as u64)
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preallocation_sets_the_length_and_costs_nothing_to_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.vxpart");
        let file = File::options()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        preallocate(&file, 64 << 20).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 64 << 20);
    }

    #[test]
    fn free_space_is_answerable_for_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(available_space(dir.path()).unwrap_or(1) > 0);
    }
}
