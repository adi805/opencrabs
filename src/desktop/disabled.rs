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

use super::model::WindowList;
use std::io;

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
