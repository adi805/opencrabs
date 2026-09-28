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
//! * [`windows`] is the Windows backend. kernel32/user32 bindings are declared
//!   by hand, following the precedent `config::winlock` sets: the handful of
//!   signatures needed here do not justify a new direct dependency, and
//!   `Cargo.lock` is pinned with `--locked` in CI.
//! * [`disabled`] keeps the same API compiling on Linux and macOS, reporting
//!   the missing backend instead of pretending the desktop is empty.
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

#[cfg(not(windows))]
mod disabled;

mod model;

pub use model::{MAX_WINDOWS, Rect, WindowInfo, WindowList, is_shell_backdrop, keep_candidate};

#[cfg(windows)]
pub use windows::{interactive_session, list_windows};

#[cfg(not(windows))]
pub use disabled::{interactive_session, list_windows};
