//! Windows desktop backend: which seat are we on, and what windows does it
//! have.
//!
//! Bindings live in [`super::win32`] and are shared with the capture backend,
//! which needs `GetWindowRect` for the same reason this file does. Types are the
//! ABI-shaped ones, not the crate's: `HWND`/`HWINSTA` arrive as pointer-sized
//! values, so they are `isize`, and `BOOL` is a 32-bit int where only zero means
//! false. `Rect`/`WindowInfo` are built from the raw shapes at the boundary, so
//! nothing above this file sees a Win32 type.

use super::model::{MAX_WINDOWS, Rect, WindowInfo, WindowList, keep_candidate};
use super::win32::{
    EnumWindows, GetClassNameW, GetCurrentProcessId, GetForegroundWindow, GetProcessWindowStation,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    NO_ACTIVE_CONSOLE_SESSION, ProcessIdToSessionId, TEXT_BUF_CHARS, WTSGetActiveConsoleSessionId,
    WinRect,
};
use std::io;

/// Decode a UTF-16 buffer written by one of the `...W` calls, which return the
/// number of characters copied (excluding the terminating NUL).
fn read_wide_text(read: impl Fn(*mut u16, i32) -> i32) -> String {
    let mut buffer = [0u16; TEXT_BUF_CHARS];
    let copied = read(buffer.as_mut_ptr(), (TEXT_BUF_CHARS - 1) as i32);
    if copied <= 0 {
        return String::new();
    }
    let len = (copied as usize).min(TEXT_BUF_CHARS - 1);
    String::from_utf16_lossy(&buffer[..len])
}

/// Accumulator handed to the callback through a pointer-sized `LPARAM`.
struct Collector {
    windows: Vec<WindowInfo>,
    truncated: bool,
    foreground: isize,
}

unsafe extern "system" fn collect_window(hwnd: isize, param: isize) -> i32 {
    // Safety: `param` is the address of the live `Collector` that
    // `list_windows` passed to `EnumWindows`, which outlives the walk, and
    // nothing else holds a reference to it during the callback.
    let sink = unsafe { &mut *(param as *mut Collector) };

    if sink.windows.len() >= MAX_WINDOWS {
        // Stop the walk: a snapshot that keeps growing past the cap is worse
        // than one that admits it stopped.
        sink.truncated = true;
        return 0;
    }

    let mut raw = WinRect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetWindowRect(hwnd, &mut raw) } == 0 {
        return 1;
    }
    let rect = Rect {
        left: raw.left,
        top: raw.top,
        right: raw.right,
        bottom: raw.bottom,
    };
    let visible = unsafe { IsWindowVisible(hwnd) } != 0;
    let class = read_wide_text(|buffer, max| unsafe { GetClassNameW(hwnd, buffer, max) });
    if !keep_candidate(visible, &rect, &class) {
        return 1;
    }

    let title = read_wide_text(|buffer, max| unsafe { GetWindowTextW(hwnd, buffer, max) });
    let mut pid = 0u32;
    // The return value is the owning thread id; the process id is the one
    // callers actually need, and a failed query leaves `pid` at 0 rather than
    // inventing a number.
    let _thread_id = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    sink.windows.push(WindowInfo {
        hwnd,
        pid,
        title,
        class,
        rect,
        foreground: hwnd == sink.foreground,
    });
    1
}

/// Whether this process can reach a desktop a person is looking at.
///
/// Three independent readings, in the order Microsoft's own guidance implies:
/// * our own session id, because a Session 0 service has a window station but
///   no visible desktop;
/// * the console session id, because the process that owns the screen is the
///   one whose input will land where the user can see it;
/// * the window station handle, as the fallback for the case where nobody is
///   logged at the physical console (`NO_ACTIVE_CONSOLE_SESSION`) but this
///   process still has an interactive station, which is what an RDP or a
///   detached headless session looks like.
///
/// `false` is a *reason*, not a failure: it tells the caller that an empty
/// window list means "no desktop here", not "nothing is open".
pub fn interactive_session() -> bool {
    let pid = unsafe { GetCurrentProcessId() };
    let mut session = 0u32;
    if unsafe { ProcessIdToSessionId(pid, &mut session) } == 0 {
        return false;
    }
    if session == 0 {
        return false;
    }
    let active = unsafe { WTSGetActiveConsoleSessionId() };
    if active != NO_ACTIVE_CONSOLE_SESSION && active == session {
        return true;
    }
    let station = unsafe { GetProcessWindowStation() };
    station != 0
}

/// Top-level windows an agent could act on, front of z-order first.
pub fn list_windows() -> io::Result<WindowList> {
    let mut sink = Collector {
        windows: Vec::new(),
        truncated: false,
        foreground: unsafe { GetForegroundWindow() },
    };
    let walk = unsafe { EnumWindows(Some(collect_window), &mut sink as *mut Collector as isize) };
    if walk == 0 && !sink.truncated {
        // Only now is FALSE an error: the alternative explanation, our own
        // callback stopping the walk, was already ruled out.
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!("EnumWindows failed: {error}"),
        ));
    }
    Ok(WindowList {
        windows: sink.windows,
        truncated: sink.truncated,
    })
}
