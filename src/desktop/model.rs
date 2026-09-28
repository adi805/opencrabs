//! Cross-platform shapes and selection rules for [`super`].
//!
//! The rules live here, away from the syscalls, for one reason: they decide
//! what an agent gets to see and therefore what it can act on, and they are
//! testable without a desktop. The backend files stay thin marshalling layers.

use std::fmt;

/// Window bounds in physical screen pixels, exactly as Win32 reports them:
/// the origin is the top-left of the primary monitor and coordinates can be
/// negative, which is normal for a monitor placed to the left or above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right.saturating_sub(self.left)
    }

    pub fn height(&self) -> i32 {
        self.bottom.saturating_sub(self.top)
    }

    /// Signed, saturating area. Negative widths (a rect reported inverted) are
    /// kept negative rather than absolved, so a nonsense rect cannot pass the
    /// `area > 0` candidate test by arithmetic accident.
    pub fn area(&self) -> i64 {
        i64::from(self.width()).saturating_mul(i64::from(self.height()))
    }
}

/// One top-level window as the OS sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// The Win32 `HWND`, held as a pointer-sized integer so this type compiles
    /// on every target. It is never `0`: `EnumWindows` only hands out real
    /// handles, and `0` is the value `GetForegroundWindow` uses for "nothing",
    /// so the two are not confusable.
    pub hwnd: isize,
    /// Owning process id, `0` when the query failed. Kept as a real field
    /// rather than an `Option` because the Windows API reports failure that
    /// way already, and callers filter on it.
    pub pid: u32,
    pub title: String,
    /// Window class name (the `RegisterClass` atom, not the title). Used by
    /// [`is_shell_backdrop`] and, more importantly, as the stable identifier
    /// for automated driving: titles get localised and renamed.
    pub class: String,
    pub rect: Rect,
    /// True for the one window currently holding keyboard focus.
    pub foreground: bool,
}

impl WindowInfo {
    pub fn is_zero_area(&self) -> bool {
        self.rect.area() <= 0
    }
}

/// Hard ceiling for one snapshot. Deliberate, not a tuning knob: a desktop
/// with 2,000 windows (a browser with many tabs plus a few Electron apps is
/// enough) would otherwise dump the whole window list into the model's context
/// every turn, and callers cannot tell a real list from a truncated one unless
/// the type says so. See [`WindowList::truncated`].
pub const MAX_WINDOWS: usize = 256;

/// Window classes that paint the desktop itself rather than being an app an
/// agent could usefully drive: the wallpaper layer, its sibling windows, and
/// the taskbar. They are visible, they have area, and they are noise: offered
/// as click targets, they invite an agent to "click the desktop" and call it
/// progress. Windows keeps the class names stable across localisations even
/// when the titles are localised, so the filter survives a non-English host.
const SHELL_BACKDROPS: [&str; 3] = ["Progman", "WorkerW", "Shell_TrayWnd"];

/// Whether a class name belongs to the desktop furniture rather than an app.
pub fn is_shell_backdrop(class: &str) -> bool {
    SHELL_BACKDROPS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(class))
}

/// The candidate rule: visible, real area, not desktop furniture.
///
/// Deliberately *not* `!title.is_empty()`: untitled windows are often exactly
/// the ones worth driving (a dialog that has not set its caption yet, a game,
/// a Chrome render surface), and dropping them would hide interactive targets
/// behind a cosmetic property.
pub fn keep_candidate(visible: bool, area: i64, class: &str) -> bool {
    visible && area > 0 && !is_shell_backdrop(class)
}

/// A window snapshot plus whether it was cut short.
///
/// `truncated` is part of the type rather than a log line because a truncated
/// list that looks complete is the failure mode that matters: an agent that
/// cannot find its target window concludes "the app is not running" and starts
/// doing something worse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowList {
    pub windows: Vec<WindowInfo>,
    pub truncated: bool,
}

impl WindowList {
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// The window holding keyboard focus, if it survived filtering.
    pub fn foreground(&self) -> Option<&WindowInfo> {
        self.windows.iter().find(|w| w.foreground)
    }
}

impl fmt::Display for WindowList {
    /// One line per window, in the order the OS enumerated them (z-order,
    /// front first). Bounded by [`MAX_WINDOWS`] by construction, so this can
    /// never dump an unbounded string into a prompt or a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for w in &self.windows {
            writeln!(
                f,
                "hwnd={} pid={} {}x{} {:+},{} [{}] {}",
                w.hwnd,
                w.pid,
                w.rect.width(),
                w.rect.height(),
                w.rect.left,
                w.rect.top,
                w.class,
                w.title
            )?;
        }
        if self.truncated {
            writeln!(f, "(truncated at {MAX_WINDOWS} windows)")?;
        }
        Ok(())
    }
}
