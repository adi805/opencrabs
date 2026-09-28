//! The raw Win32 surface this module tree needs, in one place.
//!
//! Bindings are declared by hand, following `config::winlock`: this needs about
//! twenty signatures, and adding `windows-sys` as a direct dependency would
//! churn `Cargo.lock`, which CI pins with `--locked`. Same trade-off, same
//! reasoning, so there is one convention for raw Win32 in this crate.
//!
//! One file rather than one `extern` block per backend, because the enumeration
//! and the capture both need `GetWindowRect`: a second declaration of the same
//! function in a sibling file is the kind of duplicate that drifts (one gets a
//! parameter added, the other does not) and nobody notices until a capture
//! reads the wrong rectangle.
//!
//! Types are ABI-shaped, not the crate's: handles arrive as pointer-sized
//! integers, so they are `isize`; `BOOL` is a 32-bit int where only zero means
//! false; `DWORD`/`UINT` are `u32`. Nothing above this file sees a Win32 type.
//!
//! Two exceptions, both because the input ABI is a *type* rather than a
//! signature and its layout is asserted on every host: [`Input`] and
//! [`WinPoint`] are declared in `super::input`, where the coordinate and
//! UTF-16 arithmetic that fills them can be tested on Linux.

use super::input::{Input, WinPoint};

/// `WTSGetActiveConsoleSessionId` returns this when no session is attached to
/// the physical console: RDP disconnected, or a headless host. It is a valid
/// `u32` session id nowhere else, which is why the API can use it as "none".
pub const NO_ACTIVE_CONSOLE_SESSION: u32 = 0xFFFF_FFFF;

/// Cap for a title or class we read back. Windows truncates at the requested
/// length and reports the characters copied, so this bounds memory instead of
/// requiring a length-then-allocate round trip.
pub const TEXT_BUF_CHARS: usize = 512;

/// `PrintWindow` flag: ask the window to paint itself in full, including
/// surfaces the default path skips (DirectComposition / hardware-composited
/// content). Without it a browser or Electron window captures as an empty
/// frame on modern Windows, which is exactly the blank capture the ink check
/// exists to catch.
pub const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;

/// `GetDIBits` usage: fill in the colour table (unused for 32-bpp).
pub const DIB_RGB_COLORS: u32 = 0;
/// `BITMAPINFOHEADER::biCompression`: uncompressed.
pub const BI_RGB: u32 = 0;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct WinRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct BitmapInfoHeader {
    pub size: u32,
    pub width: i32,
    pub height: i32,
    pub planes: u16,
    pub bit_count: u16,
    pub compression: u32,
    pub size_image: u32,
    pub x_pels_per_meter: i32,
    pub y_pels_per_meter: i32,
    pub clr_used: u32,
    pub clr_important: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct RgbQuad {
    pub blue: u8,
    pub green: u8,
    pub red: u8,
    pub reserved: u8,
}

/// `BITMAPINFO`: a header followed by a colour table that `GetDIBits` fills in.
/// The one-element array is the API's own shape, not a bug: the struct is
/// variable-length in C, and `BI_RGB` 32-bpp never uses more than zero entries.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct BitmapInfo {
    pub header: BitmapInfoHeader,
    pub colors: [RgbQuad; 1],
}

unsafe extern "system" {
    // ---- window enumeration (see `super::windows`) ----

    /// Walks top-level windows in z-order, front first. Returns FALSE either
    /// on failure *or* because the callback returned 0, so callers must not
    /// read `0` as "error" without checking why.
    pub fn EnumWindows(
        callback: Option<unsafe extern "system" fn(hwnd: isize, param: isize) -> i32>,
        param: isize,
    ) -> i32;
    pub fn GetForegroundWindow() -> isize;
    pub fn GetWindowTextW(hwnd: isize, buffer: *mut u16, max_chars: i32) -> i32;
    pub fn GetClassNameW(hwnd: isize, buffer: *mut u16, max_chars: i32) -> i32;
    pub fn GetWindowThreadProcessId(hwnd: isize, process_id: *mut u32) -> u32;
    pub fn IsWindowVisible(hwnd: isize) -> i32;
    /// The window station this process is attached to. `NULL` means there is
    /// no interactive station: a service, or a process spawned from a
    /// non-interactive logon.
    pub fn GetProcessWindowStation() -> isize;
    pub fn GetCurrentProcessId() -> u32;
    pub fn ProcessIdToSessionId(process_id: u32, session_id: *mut u32) -> i32;
    pub fn WTSGetActiveConsoleSessionId() -> u32;

    // ---- window capture (see `super::capture`) ----

    /// Bounds of a window in screen coordinates, including its non-client
    /// area. Shared with enumeration deliberately: the capture must photograph
    /// the same rectangle the caller was shown, or a click aimed at a reported
    /// rect would land somewhere the agent never saw.
    pub fn GetWindowRect(hwnd: isize, rect: *mut WinRect) -> i32;
    /// The screen DC when `hwnd` is null. Used only as a format template.
    pub fn GetDC(hwnd: isize) -> isize;
    pub fn ReleaseDC(hwnd: isize, dc: isize) -> i32;
    pub fn CreateCompatibleDC(dc: isize) -> isize;
    pub fn DeleteDC(dc: isize) -> i32;
    pub fn CreateCompatibleBitmap(dc: isize, width: i32, height: i32) -> isize;
    /// Returns the object that was previously selected, which the caller must
    /// put back before deleting the DC.
    pub fn SelectObject(dc: isize, object: isize) -> isize;
    pub fn DeleteObject(object: isize) -> i32;
    pub fn PrintWindow(hwnd: isize, dc: isize, flags: u32) -> i32;
    /// Reads the bitmap back out as a device-independent pixel buffer. Returns
    /// the number of scan lines copied, or 0 on failure.
    pub fn GetDIBits(
        dc: isize,
        bitmap: isize,
        start: u32,
        lines: u32,
        bits: *mut u8,
        info: *mut BitmapInfo,
        usage: u32,
    ) -> i32;
}

// The input lane's signatures. A second block rather than one long one, so
// that the section comments above each group stay next to the functions they
// describe instead of pointing into a list.
unsafe extern "system" {
    // ---- input synthesis (see `super::input_windows`) ----

    /// Injects an array of events into the OS input stream in order. Returns
    /// the number *inserted*, so both zero and a short count are distinct
    /// failures: the first means nothing was delivered, the second means the
    /// stream is now mid-gesture.
    ///
    /// `cbSize` must equal `size_of::<Input>()` exactly or the call fails
    /// outright, which is the only reason a wrong struct layout cannot be
    /// shipped silently: it is checked by the OS, at the worst possible moment.
    pub fn SendInput(inputs: u32, events: *const Input, size: i32) -> u32;
    /// Queues a message at a window's thread. Returns zero on failure, which
    /// for a *valid* window means the queue refused it (UIPI, or a destroyed
    /// handle); it does not report whether anything acted on the message.
    pub fn PostMessageW(hwnd: isize, message: u32, wparam: usize, lparam: isize) -> i32;
    /// Screen point to client point, in place. Fails when the point is outside
    /// the window's clipping region on windows that clip, so the error is a
    /// normal outcome for a stale snapshot, not an anomaly.
    pub fn ScreenToClient(hwnd: isize, point: *mut WinPoint) -> i32;
    /// The only way to ask where the desktop is: metrics are per-system and a
    /// hosted runner's answer is not guessable from anything in the repo.
    pub fn GetSystemMetrics(index: i32) -> i32;
    /// Virtual-key to scan code for the installed layout. Zero means no
    /// mapping, which is different from scan code zero.
    pub fn MapVirtualKeyW(code: u32, map_type: u32) -> u32;
    /// Current cursor position, in screen coordinates. Read-only, and the
    /// receipt that a posted click left the human's pointer where it was.
    pub fn GetCursorPos(point: *mut WinPoint) -> i32;
}
