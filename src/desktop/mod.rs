//! Desktop control: see the windows the OS is showing, and act on them.
//!
//! Everything else in this crate talks to a *service* (a channel, a database,
//! a browser over CDP). This module talks to the window manager instead, and
//! that difference is load-bearing: an agent that can click anywhere can drive
//! any application on the machine, including ones holding credentials the
//! agent was never granted. So the surface is deliberately split by risk.
//! Reading (`list_windows`, capture) is free, while synthesising input is not,
//! and the input half stays behind the caller's approval gate rather than
//! trusting a model to decide when it is allowed to move a cursor.
//!
//! Layout, per `CONTRIBUTING.md` (`mod.rs` is declarations only):
//! * [`model`] holds the cross-platform shapes and the *selection rules*,
//!   because the rules are the part that is easy to get wrong and cheap to test
//!   on any host.
//! * [`windows`] is the Windows backend for enumeration; [`capture`] is the
//!   Windows backend for photographing one window. `win32` holds the raw
//!   bindings both share, declared by hand following the precedent
//!   `config::winlock` sets: the handful of signatures needed here do not
//!   justify a new direct dependency, and `Cargo.lock` is pinned with
//!   `--locked` in CI.
//! * [`disabled`] keeps the same API compiling on Linux and macOS, reporting
//!   the missing backend instead of pretending the desktop is empty.
//!
//! The input lane is split three ways rather than two, because the two kinds of
//! mistake in it need different hosts to be caught: [`input`] holds the Win32
//! input ABI and the arithmetic that fills it (coordinate scaling, message
//! packing, UTF-16 splitting), [`input_events`] builds the *sequences* an intent
//! turns into, and [`input_windows`] is the only place a syscall happens. The
//! first two compile on every target so their assertions run in the Linux test
//! job, which is the only job that runs tests; a wrong sequence is invisible to
//! Windows itself, so there is nothing else to catch it.
//!
//! Capture is on the read side of the split, with `list_windows`: it produces
//! pixels rather than input events, and pixels are what let a caller *decide*
//! where to act instead of guessing. The one thing it must never do is report a
//! blank frame as a successful one, so [`capture::capture_window_to_png`]
//! measures the image and fails on emptiness.
//!
//! Seat safety, which is the thing that will confuse anyone debugging this:
//! Windows only lets a process see the desktop of the session it belongs to.
//! A service in Session 0, or a process spawned over SSH, has no window
//! station at all, so enumeration returns an empty list rather than an error.
//! [`interactive_session`] is how callers tell "nothing is open" apart from
//! "you are not sitting at a desktop", and an empty list without that check is
//! an unactionable report.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
mod capture;

#[cfg(windows)]
mod win32;

#[cfg(not(windows))]
mod disabled;

mod input;
mod input_events;

#[cfg(windows)]
mod input_windows;

mod model;
pub mod policy;

pub use model::{
    Capture, INK_CHANNEL_FLOOR, MAX_WINDOWS, MIN_INK_RATIO, Rect, WindowInfo, WindowList,
    bgra_to_rgba, ink_ratio, is_shell_backdrop, keep_candidate,
};

#[cfg(windows)]
pub use capture::{capture_window, capture_window_to_png};
#[cfg(windows)]
pub use windows::{interactive_session, list_windows};

#[cfg(not(windows))]
pub use disabled::{capture_window, capture_window_to_png, interactive_session, list_windows};

/// The input lane's vocabulary. Re-exported because a caller that receives a
/// [`Delivery`] has to be able to read what it was built from: these are the
/// constants and shapes that make an event sequence inspectable in a log
/// instead of a blob of bytes.
pub use input::{
    Delivery, INPUT_BODY_BYTES, INPUT_KEYBOARD, INPUT_MOUSE, Input, InputBody,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, KeyboardInput,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MouseButton, MouseInput, ScreenPoint, VK_CONTROL,
    VK_MENU, VK_SHIFT, VKS_ALT, VKS_CONTROL, VKS_SHIFT, WM_CHAR, WM_CLOSE, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WinPoint, char_units, keyboard_input, mouse_input, mouse_lparam, normalize_axis,
};
pub use input_events::{
    Key, PostedMessage, click_events, decode_vk_scan, key_events_with_modifiers, key_lparam,
    posted_char_messages, posted_click_messages, posted_key_messages, unicode_events, unicode_pair,
};

/// The actions. Everything here changes what a machine does, not what this
/// process knows, so the caller is expected to have settled the approval
/// question before reaching for one; the tool surface that does that is
/// documented in `brain::tools`.
#[cfg(windows)]
pub use input_windows::{
    click_window, cursor_position, inject_click, inject_key, inject_text, inject_text_as_keys,
    post_close, press_key_in_window, type_into_window, virtual_desktop,
};

#[cfg(not(windows))]
pub use disabled::{
    click_window, cursor_position, inject_click, inject_key, inject_text, inject_text_as_keys,
    post_close, press_key_in_window, type_into_window, virtual_desktop,
};
