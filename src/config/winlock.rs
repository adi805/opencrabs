//! Windows backend for the advisory locks that [`flock`](super::flock)
//! provides on Unix, plus the one process-stop primitive the preempt path
//! needs there.
//!
//! The kernel32 bindings are declared by hand, matching the existing
//! precedent in this module tree (`is_pid_alive` in `profile.rs` declares
//! `OpenProcess`/`CloseHandle` the same way). That is deliberate: the lock
//! needs exactly three functions, and pulling `windows-sys` in as a direct
//! dependency would churn `Cargo.lock` — which CI pins with `--locked` — for
//! a couple dozen lines of signatures the codebase already writes this way.
//!
//! Semantics mirrored from Unix:
//! * Whole-file advisory lock (flock has no byte ranges) = `LockFileEx`
//!   spanning the entire file with `LOCKFILE_EXCLUSIVE_LOCK`.
//! * `LOCK_NB` = `LOCKFILE_FAIL_IMMEDIATELY`; contention reports `Held`,
//!   exactly like `FlockOutcome::Held`, so callers keep identical arms.
//! * Release on handle drop: the OS clears range locks when the file handle
//!   closes. [`unlock`] exists for explicit release with a loggable error.
//!
//! Cross-platform caveat, by design: a Unix flock and a Windows range lock
//! on the same file do not see each other. Both families are only ever
//! contended by two OpenCrabs instances on one machine, and a Windows build
//! never executes the Unix code path, so the split is safe as long as no
//! caller mixes them.

use std::io;
use std::os::windows::io::RawHandle;
use std::ptr;

/// Outcome of a lock request, mirroring `flock::FlockOutcome`.
#[derive(Debug)]
pub enum LockOutcome {
    Acquired,
    /// Non-blocking request and a live process holds the range right now.
    Held,
    Failed(io::Error),
}

const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
/// ERROR_LOCK_VIOLATION: the range is already held by another handle.
const ERROR_LOCK_VIOLATION: i32 = 33;

/// Win32 `OVERLAPPED`, laid out by hand:
/// `ULONG_PTR Internal; ULONG_PTR InternalHigh;`
/// `union { struct { DWORD Offset; DWORD OffsetHigh; }; PVOID Pointer; };`
/// `HANDLE hEvent;`
/// We always lock from offset 0 with a NULL event (synchronous), so the
/// union is a zeroed u64 and the struct is valid as all-zeros.
#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset_union: u64,
    event: RawHandle,
}

impl Overlapped {
    fn whole_file() -> Self {
        Self {
            internal: 0,
            internal_high: 0,
            offset_union: 0,
            event: ptr::null_mut(),
        }
    }
}

unsafe extern "system" {
    fn LockFileEx(
        file: RawHandle,
        flags: u32,
        reserved: u32,
        bytes_low: u32,
        bytes_high: u32,
        overlapped: *const Overlapped,
    ) -> i32;
    fn UnlockFileEx(
        file: RawHandle,
        reserved: u32,
        bytes_low: u32,
        bytes_high: u32,
        overlapped: *const Overlapped,
    ) -> i32;
    fn OpenProcess(desired_access: u32, inherit: i32, pid: u32) -> RawHandle;
    fn TerminateProcess(process: RawHandle, exit_code: u32) -> i32;
    fn CloseHandle(object: RawHandle) -> i32;
    fn QueryFullProcessImageNameW(
        process: RawHandle,
        flags: u32,
        exe_name: *mut u16,
        size: *mut u32,
    ) -> i32;
}

/// Exclusive whole-file lock on `handle`, mirroring `flock::exclusive`
/// argument-for-argument: `nb == true` behaves like `LOCK_NB` — a contended
/// lock returns [`LockOutcome::Held`] immediately instead of waiting.
pub fn exclusive(handle: RawHandle, nb: bool) -> LockOutcome {
    let ov = Overlapped::whole_file();
    let flags = LOCKFILE_EXCLUSIVE_LOCK | if nb { LOCKFILE_FAIL_IMMEDIATELY } else { 0 };
    // low = high = 0xFFFFFFFF locks "to EOF", covering every byte, the
    // byte-range analogue of flock(fd, LOCK_EX) on the whole file.
    let ok = unsafe { LockFileEx(handle, flags, 0, u32::MAX, u32::MAX, &ov) };
    if ok != 0 {
        return LockOutcome::Acquired;
    }
    let err = io::Error::last_os_error();
    if nb && err.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
        LockOutcome::Held
    } else {
        LockOutcome::Failed(err)
    }
}

/// Explicitly release a lock acquired via [`exclusive`]. Dropping the
/// underlying `File` also releases (the OS cleans up on handle close); this
/// exists so a caller can see the error when the release itself fails.
pub fn unlock(handle: RawHandle) -> io::Result<()> {
    let ov = Overlapped::whole_file();
    if unsafe { UnlockFileEx(handle, 0, u32::MAX, u32::MAX, &ov) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Hard stop for the Windows preempt path — FAIL-CLOSED on target identity.
///
/// Unix has a polite rung (SIGTERM) and a rude one (SIGKILL); a headless
/// Windows console process has no signal channel at all, so this is the rude
/// rung only. That is exactly why a PID alone must not pull the trigger:
/// Windows recycles PIDs, and a stale lock stamp can name an unrelated
/// process holding the number now. So we open the process, ask kernel32 for
/// its FULL IMAGE PATH, and compare it (case-insensitively) against this
/// process's own `current_exe()`. Not us — or unreadable — we do NOT kill.
/// "Leaves a stubborn instance running" always beats "kills something
/// unrelated the user is doing".
pub fn terminate(pid: u32) -> io::Result<()> {
    const PROCESS_TERMINATE: u32 = 0x0001;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    let self_exe = match normalize_exe_path(&std::env::current_exe()?) {
        Some(p) => p,
        None => return Err(io::Error::other("cannot resolve own exe path")),
    };

    // One handle, both rights: query the image to verify identity, and
    // terminate only once verified.
    let rights = PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION;
    let h = unsafe { OpenProcess(rights, 0, pid) };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }

    let mut buf = [0u16; 32_768];
    let mut size = buf.len() as u32;
    let queried = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size) };
    let outcome = if queried == 0 {
        // Cannot read the image path of the thing we were about to kill:
        // that IS the fail-closed condition, not a nuisance.
        Err(io::Error::last_os_error())
    } else {
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        match normalize_exe_path(std::path::Path::new(&path)) {
            Some(target) if target == self_exe => {
                if unsafe { TerminateProcess(h, 1) } != 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            }
            Some(_) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("refusing to terminate PID {pid}: image {path:?} is not this binary"),
            )),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing to terminate PID {pid}: unreadable image path {path:?}"),
            )),
        }
    };
    unsafe { CloseHandle(h) };
    outcome
}

/// Lowercase + trim trailing separators so two `\\?\`-normalized Win32 paths
/// compare case-insensitively (short paths and the current one can differ in
/// case and separator style while naming the same file).
fn normalize_exe_path(p: &std::path::Path) -> Option<String> {
    Some(p.to_string_lossy().to_lowercase().trim_end_matches('\\').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;

    /// Range-lock contention is per-HANDLE, so a second handle to the same
    /// file in the SAME process contends exactly like a second process.
    /// That makes the Held/Acquired/unlock cycle testable without spawning
    /// anything.
    #[test]
    fn non_blocking_second_handle_reports_held() {
        let dir = std::env::temp_dir().join(format!("winlock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.lock");
        let open = || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&path)
                .unwrap()
        };
        let a = open();
        let b = open();
        assert!(matches!(
            exclusive(a.as_raw_handle(), true),
            LockOutcome::Acquired
        ));
        assert!(matches!(
            exclusive(b.as_raw_handle(), true),
            LockOutcome::Held
        ));
        assert!(unlock(a.as_raw_handle()).is_ok());
        assert!(matches!(
            exclusive(b.as_raw_handle(), true),
            LockOutcome::Acquired
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
