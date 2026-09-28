//! Delivering synthesised input, on Windows.
//!
//! The thin half: every decision about *what* to send is already made by the
//! pure builders in [`super::input`] and [`super::input_events`]. What is here
//! is the two syscalls that move a pointer or a character, the queries needed
//! to aim them, and the error handling around the fact that both APIs report
//! success in a way that does not mean the application did anything.
//!
//! Coordinates are physical screen pixels, matching what [`super::capture`]
//! photographs and [`super::windows`] reports, so a rectangle an agent was
//! shown is a rectangle it can click. That correspondence holds while this
//! process has one DPI awareness context, which it does: nothing here calls
//! `SetProcessDpiAwarenessContext`, and the window rectangles read from
//! `GetWindowRect` are virtualised by the same rule that scales the coordinates
//! given to `SendInput`. Mixing the two (declaring awareness after reading
//! rectangles, or comparing our pixels against a screenshot taken by a
//! differently-aware process) is how clicks land offset on a laptop running
//! 150% scaling while working correctly on every CI runner.

use super::input::{Delivery, Input, MouseButton, ScreenPoint, WinPoint};
use super::input_events::{
    Key, click_events, posted_char_messages, posted_click_messages, posted_key_messages,
    unicode_events,
};
use super::model::Rect;
use super::win32::{
    GetCursorPos, GetSystemMetrics, MapVirtualKeyW, PostMessageW, ScreenToClient, SendInput,
};
use std::io;

/// `GetSystemMetrics` indices for the *virtual* desktop: the bounding box of
/// every monitor, which is the space an `ABSOLUTE | VIRTUALDESK` event is
/// scaled into.
const SM_XVIRTUALSCREEN: i32 = 76;
const SM_YVIRTUALSCREEN: i32 = 77;
const SM_CXVIRTUALSCREEN: i32 = 78;
const SM_CYVIRTUALSCREEN: i32 = 79;

/// `MapVirtualKeyW` map type: virtual-key code to scan code.
const MAPVK_VK_TO_VSC: u32 = 0;

/// The bounding box of all monitors.
///
/// Refused when it has no area: a session with no display device reports zero
/// here, and scaling a coordinate into a zero-width desktop is a division by
/// minus one, not an error message.
pub fn virtual_desktop() -> io::Result<Rect> {
    let (left, top) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        )
    };
    let (width, height) = unsafe {
        (
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let rect = Rect {
        left,
        top,
        right: left.saturating_add(width),
        bottom: top.saturating_add(height),
    };
    if !rect.has_positive_span() {
        return Err(io::Error::other(format!(
            "no display device to aim at: virtual desktop reports {width}x{height} at {left},{top} \
             (a session with no window station has no desktop to click on)"
        )));
    }
    Ok(rect)
}

/// Where the human's cursor is right now.
///
/// Read-only, and the reason the posted route can be *proved* safe rather than
/// merely claimed so: a caller can sample this before and after a posted click
/// and see that it did not move.
pub fn cursor_position() -> io::Result<ScreenPoint> {
    let mut point = WinPoint::default();
    if unsafe { GetCursorPos(&mut point) } == 0 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!("GetCursorPos failed: {error}"),
        ));
    }
    Ok(ScreenPoint {
        x: point.x,
        y: point.y,
    })
}

/// Convert a screen point into the client coordinates a mouse message wants.
///
/// This is the step that cannot be skipped or guessed. `WM_LBUTTONDOWN` takes
/// coordinates relative to the window's client area, while `SendInput` takes
/// screen coordinates: posting the screen point directly aims the click at a
/// spot offset by the title bar and the window's own position, so it lands
/// outside anything the agent photographed.
fn client_point(hwnd: isize, at: ScreenPoint) -> io::Result<WinPoint> {
    let mut point = WinPoint { x: at.x, y: at.y };
    if unsafe { ScreenToClient(hwnd, &mut point) } == 0 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!(
                "ScreenToClient refused point {},{} for window {hwnd}: {error}",
                at.x, at.y
            ),
        ));
    }
    Ok(point)
}

/// The scan code for a key on the keyboard layout actually installed.
fn scan_code(key: Key) -> io::Result<u16> {
    let scan = unsafe { MapVirtualKeyW(u32::from(key.vk()), MAPVK_VK_TO_VSC) };
    if scan == 0 {
        return Err(io::Error::other(format!(
            "no scan code maps for key {:?} (vk {:#04x}) on this keyboard layout",
            key,
            key.vk()
        )));
    }
    // `u32` in the API, but a scan code is a byte (or a byte with the extended
    // prefix in the high byte); anything wider means this is not a scan code and
    // truncating it silently would press a different key.
    if scan > u32::from(u16::MAX) {
        return Err(io::Error::other(format!(
            "MapVirtualKeyW returned {scan} for key {key:?}, which is not a scan code"
        )));
    }
    Ok(scan as u16)
}

/// Hand a batch of events to `SendInput` as one call.
///
/// One call, not one per event: `SendInput` injects the array in order, and
/// splitting a click across three calls leaves a window between them in which
/// the human's own input can interleave, turning a click into a drag.
fn inject(events: &[Input]) -> io::Result<Delivery> {
    if events.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to inject an empty batch: nothing would be delivered and the call would \
             still look like it ran",
        ));
    }
    let size =
        i32::try_from(std::mem::size_of::<Input>()).expect("INPUT is a fixed 32-bit-padded struct");
    let inserted = unsafe { SendInput(events.len() as u32, events.as_ptr(), size) };
    if inserted == 0 {
        // Failure is 0 plus a last error. The two that matter in practice:
        // `ERROR_ACCESS_DENIED` when a higher-integrity window owns the
        // foreground (UIPI will not let this process type into it), and a
        // `BlockInput` call somewhere else. Both mean nothing was delivered.
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!(
                "SendInput accepted none of {} events: {error} (a foreground window running \
                 elevated, or input blocked, rejects injection outright)",
                events.len()
            ),
        ));
    }
    if inserted as usize != events.len() {
        // The worst of the three outcomes, because it is neither success nor
        // clean failure: the batch stopped partway, so a click may have
        // delivered its press without its release and the button is logically
        // held until something sends the up.
        return Err(io::Error::other(format!(
            "SendInput accepted {inserted} of {} events: the input stream may be mid-gesture, \
             so no further input should be assumed safe",
            events.len()
        )));
    }
    Ok(Delivery::Injected {
        events: inserted as usize,
    })
}

/// Queue messages at one window.
///
/// Stops at the first refusal and reports how many landed, for the same reason
/// [`inject`] does: a partially delivered press/release pair is a held button.
fn post(hwnd: isize, messages: &[super::input_events::PostedMessage]) -> io::Result<Delivery> {
    if hwnd == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "0 is not a window handle: it is the value GetForegroundWindow uses for 'none'",
        ));
    }
    if messages.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to post an empty batch: nothing would be delivered and the call would \
             still look like it ran",
        ));
    }
    for (index, message) in messages.iter().enumerate() {
        let queued = unsafe { PostMessageW(hwnd, message.message, message.wparam, message.lparam) };
        if queued == 0 {
            let error = io::Error::last_os_error();
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "PostMessage refused message {:#06x} ({}/{}) for window {hwnd}: {error} \
                     (an elevated target rejects this process under UIPI)",
                    message.message,
                    index + 1,
                    messages.len()
                ),
            ));
        }
    }
    Ok(Delivery::Queued {
        messages: messages.len(),
    })
}

/// Click a button in one window, addressed at that window.
///
/// Does not move the cursor and does not take keyboard focus. See
/// [`Delivery::Queued`] for what this does not prove.
pub fn click_window(hwnd: isize, button: MouseButton, at: ScreenPoint) -> io::Result<Delivery> {
    let client = client_point(hwnd, at)?;
    post(hwnd, &posted_click_messages(button, client))
}

/// Type text into one window as `WM_CHAR`, without touching the cursor.
///
/// `WM_CHAR` is a *character* message, so this is the path for text, not for
/// shortcuts: Ctrl+V and Alt+F4 are `WM_KEYDOWN` with modifier state, and no
/// amount of posted characters will produce them. That limit is deliberate,
/// because a synthetic Ctrl held across a window's own handler can trigger
/// anything that application binds to that chord.
pub fn type_into_window(hwnd: isize, text: &str) -> io::Result<Delivery> {
    post(hwnd, &posted_char_messages(text))
}

/// Press and release one named key in a window.
pub fn press_key_in_window(hwnd: isize, key: Key) -> io::Result<Delivery> {
    post(hwnd, &posted_key_messages(key, scan_code(key)?))
}

/// Move the pointer and click, through the OS input stream.
///
/// This goes to whatever holds focus and moves the human's cursor. The
/// [`super`] docs explain why the addressed route is the default; use this only
/// when the target has shown it ignores posted messages.
pub fn inject_click(button: MouseButton, at: ScreenPoint) -> io::Result<Delivery> {
    let desktop = virtual_desktop()?;
    let events = click_events(&desktop, button, at).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "point ({},{}) is not inside the virtual desktop ({}x{} at {},{}): an out-of-range \
                 click is refused rather than clamped, because the edge of the screen belongs to \
                 some other application",
                at.x,
                at.y,
                desktop.width(),
                desktop.height(),
                desktop.left,
                desktop.top
            ),
        )
    })?;
    inject(&events)
}

/// Type text through the OS input stream, into the focused window.
pub fn inject_text(text: &str) -> io::Result<Delivery> {
    inject(&unicode_events(text))
}

/// Press and release one named key through the OS input stream.
///
/// Uses scan codes rather than unicode: navigation keys have no character, and
/// the scan-code path is what an application polling raw input will see.
pub fn inject_key(key: Key) -> io::Result<Delivery> {
    let scan = scan_code(key)?;
    inject(&key.events(scan))
}
