//! Stand-ins for platforms with no desktop backend compiled in.
//!
//! The temptation is to return an empty window list, and that is exactly the
//! wrong answer: "no windows" and "this build cannot see windows" print
//! identically, and an agent that cannot tell them apart concludes the app it
//! is looking for is not running, then does something worse than report a
//! miss. So the stub fails loudly with [`std::io::ErrorKind::Unsupported`],
//! the same shape `src/rtk/disabled.rs` uses for a feature that is off.
//!
//! Linux (X11/Wayland) and macOS (CoreGraphics) backends are real future work;
//! when one lands it replaces this file's `#[cfg]` arm, not the API.

use super::model::{Capture, WindowList};
use std::io;
use std::path::Path;

/// Fail-closed: no backend, so no claim about the seat can be verified.
pub fn interactive_session() -> bool {
    false
}

pub fn list_windows() -> io::Result<WindowList> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop control has no backend on this platform yet",
    ))
}

/// Same fail-closed shape as [`list_windows`]: a stub that returned a black
/// `Capture` would be indistinguishable from a window that genuinely painted
/// nothing, and the caller would save an empty PNG and call it a screenshot.
pub fn capture_window(_hwnd: isize) -> io::Result<Capture> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "window capture has no backend on this platform yet",
    ))
}

pub fn capture_window_to_png(_hwnd: isize, _path: &Path) -> io::Result<Capture> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "window capture has no backend on this platform yet",
    ))
}

// The input lane's stand-ins. Same fail-closed reasoning as above, with one
// addition worth stating: these are the *dangerous* functions, and a stub that
// returned `Ok` would let a caller on Linux believe it had clicked something.

/// No desktop to report, so no claim about the pointer can be verified either.
pub fn cursor_position() -> io::Result<super::input::ScreenPoint> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop input has no backend on this platform yet",
    ))
}

pub fn virtual_desktop() -> io::Result<super::model::Rect> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop input has no backend on this platform yet",
    ))
}

pub fn click_window(
    _hwnd: isize,
    _button: super::input::MouseButton,
    _at: super::input::ScreenPoint,
) -> io::Result<super::input::Delivery> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop input has no backend on this platform yet",
    ))
}

pub fn type_into_window(_hwnd: isize, _text: &str) -> io::Result<super::input::Delivery> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop input has no backend on this platform yet",
    ))
}

pub fn press_key_in_window(
    _hwnd: isize,
    _key: super::input_events::Key,
) -> io::Result<super::input::Delivery> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop input has no backend on this platform yet",
    ))
}

pub fn inject_click(
    _button: super::input::MouseButton,
    _at: super::input::ScreenPoint,
) -> io::Result<super::input::Delivery> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop input has no backend on this platform yet",
    ))
}

pub fn inject_text(_text: &str) -> io::Result<super::input::Delivery> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop input has no backend on this platform yet",
    ))
}

pub fn inject_key(_key: super::input_events::Key) -> io::Result<super::input::Delivery> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop input has no backend on this platform yet",
    ))
}

/// See [`super::input_windows::inject_text_as_keys`]: refused here because there
/// is no keyboard to press, not because the question was unreasonable.
/// Fail closed: there is no window to ask.
///
/// `false` rather than an error because the Windows half of this pair answers
/// the same question ("did the message get in") with a bool, and a caller
/// written against `bool` must not be able to read a Linux run as a success.
pub fn post_close(_hwnd: isize) -> bool {
    false
}

pub fn inject_text_as_keys(_text: &str) -> io::Result<super::input::Delivery> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop input has no backend on this platform yet",
    ))
}

// The app-control lane's stand-ins. `focus_target` is why this half of the pair
// returns a `Result` at all: `FocusVerdict` has no value meaning "this platform
// cannot look", so a verdict-returning signature would force a caller to read a
// fabricated answer about a window that was never asked about. `Unsupported` is
// the only honest thing to say here, and a launch that returned a pid would be
// the same lie with more consequences.

pub fn launch_app(_program: &str, _args: &[String]) -> Result<u32, String> {
    Err("desktop control has no backend on this platform yet".to_string())
}

pub fn launch_to_window(
    _plan: &super::app::LaunchPlan,
    _settle: std::time::Duration,
) -> Result<(u32, Option<super::model::WindowInfo>), String> {
    Err("desktop control has no backend on this platform yet".to_string())
}

pub fn focus_target(_hwnd: isize) -> Result<super::app::FocusVerdict, String> {
    Err("desktop control has no backend on this platform yet".to_string())
}

pub fn close_target(
    _target: &super::model::WindowInfo,
    _our_pid: u32,
    _settle: std::time::Duration,
) -> Result<super::app::CloseVerdict, String> {
    Err("desktop control has no backend on this platform yet".to_string())
}
