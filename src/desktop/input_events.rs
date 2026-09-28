//! The event sequences that turn an intent into the shapes in [`super::input`].
//!
//! Split from that file because the two kinds of mistake live here and there
//! respectively. Getting a struct's layout wrong is caught by the size
//! assertions; getting a *sequence* wrong is not caught by anything on the
//! machine that made the call. A click without its preceding move, a release
//! that still claims the button is down, a surrogate pair sent low-first: each
//! of those is accepted by Windows, delivered, and does something other than
//! what the caller asked for. So they are built by these pure functions, where
//! the order of a sequence is a value a test can assert on.
//!
//! Nothing here makes a syscall. That is what lets the whole of it be tested on
//! Linux, which is where the CI test job runs.

use super::input::{
    Input, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MouseButton, ScreenPoint,
    WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_MOUSEMOVE, WinPoint, char_units, keyboard_input, mouse_input,
    mouse_lparam, normalize_axis,
};
use super::model::Rect;
/// Key-press and key-release for one UTF-16 unit, on the unicode path.
pub fn unicode_pair(unit: u16) -> [Input; 2] {
    [
        keyboard_input(unit, KEYEVENTF_UNICODE),
        keyboard_input(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
    ]
}

/// The whole string as injected events: press then release, per UTF-16 unit.
pub fn unicode_events(text: &str) -> Vec<Input> {
    let units = char_units(text);
    let mut events = Vec::with_capacity(units.len() * 2);
    for unit in units {
        events.extend(unicode_pair(unit));
    }
    events
}

/// A named key that has no character of its own.
///
/// Bounded on purpose. Everything here is a key a dialog or a console actually
/// needs (confirm, complete, cancel, delete, navigate); an editor driving
/// arbitrary chords belongs to the UIA layer, not to this table. Adding a key
/// is one variant plus one row in the test that checks the extended-key flags,
/// because `extended()` is the part nobody can guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
}

impl Key {
    /// The `VK_*` code. Also the `wParam` of a posted `WM_KEYDOWN`, so the
    /// posted and injected routes agree on what a key is.
    pub fn vk(self) -> u16 {
        match self {
            Self::Enter => 0x0D,
            Self::Tab => 0x09,
            Self::Escape => 0x1B,
            Self::Backspace => 0x08,
            Self::Delete => 0x2E,
            Self::Insert => 0x2D,
            Self::Home => 0x24,
            Self::End => 0x23,
            Self::PageUp => 0x21,
            Self::PageDown => 0x22,
            Self::Up => 0x26,
            Self::Down => 0x28,
            Self::Left => 0x25,
            Self::Right => 0x27,
        }
    }

    /// Whether this key needs `KEYEVENTF_EXTENDEDKEY`.
    ///
    /// Without the bit, the navigation cluster is *not* delivered: an arrow key
    /// sent as a plain scan code reads as the numeric keypad's digit, so "press
    /// Down" becomes "type 2". Every key in the 0x21..=0x29 range here needs it.
    ///
    /// `Enter` is the deliberate exception. The main Enter is not extended;
    /// only the keypad Enter (same `VK_RETURN`, different scan code 0x1C with
    /// the extended prefix) is. Sending the flag for both is the classic way to
    /// make a form submit twice in applications that distinguish them.
    pub fn extended(self) -> bool {
        !matches!(
            self,
            Self::Enter | Self::Tab | Self::Escape | Self::Backspace
        )
    }

    /// Press and release on the scan-code path, using the scan code resolved
    /// from `vk()`.
    pub fn events(self, scan: u16) -> [Input; 2] {
        let base = KEYEVENTF_SCANCODE
            | if self.extended() {
                KEYEVENTF_EXTENDEDKEY
            } else {
                0
            };
        [
            keyboard_input(scan, base),
            keyboard_input(scan, base | KEYEVENTF_KEYUP),
        ]
    }
}

/// One posted window message: the triple `PostMessageW` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostedMessage {
    pub message: u32,
    pub wparam: usize,
    pub lparam: isize,
}

/// Move, press, release, in client coordinates.
///
/// The move comes first because it is what a hand does: many custom title bars
/// and menu implementations arm a control on `WM_MOUSEMOVE` and ignore a button
/// message that arrives at a location they have never seen hovered. Skipping
/// it is the difference between "the click worked" and "the click worked on my
/// machine".
///
/// The release carries no button state in `wParam`. That is not tidiness: `MK_*`
/// on `WM_LBUTTONUP` claims the button is still down while reporting that it
/// was released, and applications that read the state rather than the message
/// (file explorers, drag handles) end up in a mode they cannot leave.
pub fn posted_click_messages(button: MouseButton, client: WinPoint) -> Vec<PostedMessage> {
    let point = mouse_lparam(client.x, client.y);
    vec![
        PostedMessage {
            message: WM_MOUSEMOVE,
            wparam: 0,
            lparam: point,
        },
        PostedMessage {
            message: button.down_message(),
            wparam: button.mk_flag(),
            lparam: point,
        },
        PostedMessage {
            message: button.up_message(),
            wparam: 0,
            lparam: point,
        },
    ]
}

/// A string as posted `WM_CHAR` messages, one per UTF-16 unit.
pub fn posted_char_messages(text: &str) -> Vec<PostedMessage> {
    char_units(text)
        .into_iter()
        .map(|unit| PostedMessage {
            message: WM_CHAR,
            wparam: usize::from(unit),
            lparam: 0,
        })
        .collect()
}

/// The three injected events for one absolute click: hover, press, release.
///
/// `None` when the point is not inside `desktop`, or the desktop is too narrow
/// to scale into. A click outside the virtual desktop is refused rather than
/// clamped: clamping sends it to a screen edge where another application's
/// control lives, and the caller cannot tell that the click it aimed at its own
/// window landed somewhere else entirely.
pub fn click_events(desktop: &Rect, button: MouseButton, at: ScreenPoint) -> Option<Vec<Input>> {
    let dx = normalize_axis(at.x, desktop.left, desktop.width())?;
    let dy = normalize_axis(at.y, desktop.top, desktop.height())?;
    let absolute = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
    Some(vec![
        mouse_input(dx, dy, absolute),
        mouse_input(dx, dy, absolute | button.down_flag()),
        mouse_input(dx, dy, absolute | button.up_flag()),
    ])
}

/// The `lParam` of a key message: repeat count, scan code, and the two state
/// bits, packed the way `winuser.h` documents them.
///
/// `wParam` alone is enough for well-behaved dialogs and not enough for an
/// application that decodes the scan code out of `lParam`, which is how a
/// posted key ends up looking received while doing nothing. Packing it costs
/// one function; the alternative is a class of silent no-ops.
///
/// Bit 24 is the extended flag, bit 29 the previous key state and bit 30 the
/// transition state. Both of the last two are set only on a release, which is
/// what distinguishes the two otherwise-identical messages.
pub fn key_lparam(scan: u16, extended: bool, released: bool) -> isize {
    let mut value: u32 = 1 | (u32::from(scan) << 16);
    if extended {
        value |= 1 << 24;
    }
    if released {
        value |= 1 << 29 | 1 << 30;
    }
    value as i32 as isize
}

/// Press and release for one named key, addressed at a window.
///
/// `scan` comes from `MapVirtualKeyW` on the caller's side: it is per-keyboard
/// layout, so it cannot be a constant here, and a hard-coded one would press
/// the wrong key on a non-US layout while still appearing to work on the
/// machine it was written on.
pub fn posted_key_messages(key: Key, scan: u16) -> Vec<PostedMessage> {
    let vk = usize::from(key.vk());
    let extended = key.extended();
    vec![
        PostedMessage {
            message: WM_KEYDOWN,
            wparam: vk,
            lparam: key_lparam(scan, extended, false),
        },
        PostedMessage {
            message: WM_KEYUP,
            wparam: vk,
            lparam: key_lparam(scan, extended, true),
        },
    ]
}
