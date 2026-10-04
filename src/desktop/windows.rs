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
    DWMWA_CLOAKED, DwmGetWindowAttribute, EnumWindows, GetClassNameW, GetCurrentProcessId,
    GetForegroundWindow, GetProcessWindowStation, GetUserObjectInformationW, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, NO_ACTIVE_CONSOLE_SESSION,
    ProcessIdToSessionId, TEXT_BUF_CHARS, UOI_NAME, WTSGetActiveConsoleSessionId, WinRect,
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

/// Whether the Desktop Window Manager is currently showing a window.
///
/// `IsWindowVisible` answers a different question: a window on another virtual
/// desktop, or a suspended UWP app, keeps that bit set and a positive rectangle
/// while the compositor is not showing it at all, so offering it as a click
/// target invites an action aimed at something nobody can see. A query that
/// fails is read as "not cloaked": this is a refinement, and dropping every
/// window because one attribute is unavailable would be worse than the rare
/// stale hit it prevents.
fn is_cloaked(hwnd: isize) -> bool {
    let mut cloaked = 0i32;
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut i32,
            std::mem::size_of::<i32>() as u32,
        )
    };
    ok == 0 && cloaked != 0
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
    if !keep_candidate(visible, &rect, &class) || is_cloaked(hwnd) {
        return 1;
    }

    // A kept window is the only thing that can push past the cap, so this is
    // where the flag belongs: setting it at the top of the walk would mark a
    // list truncated when the 257th window is one the filter would have
    // dropped, and the boundary case (the walk ending exactly at the cap with
    // no further window) would never set it at all.
    if sink.windows.len() >= MAX_WINDOWS {
        sink.truncated = true;
        return 0;
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
    // The handle being non-null is not the answer: a non-interactive logon
    // still holds a window-station handle. The interactive station is the one
    // named WinSta0; a service's station (Service-0x0-...) has no desktop a
    // person is looking at, so treating any handle as proof would report a
    // seat that cannot show anything.
    let station = unsafe { GetProcessWindowStation() };
    if station == 0 {
        return false;
    }
    let mut name = [0u16; TEXT_BUF_CHARS];
    let read = unsafe {
        GetUserObjectInformationW(
            station,
            UOI_NAME,
            name.as_mut_ptr(),
            (name.len() * std::mem::size_of::<u16>()) as u32,
            std::ptr::null_mut(),
        )
    };
    if read == 0 {
        return false;
    }
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("WinSta0")
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
