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
    /// Left Alt, `VK_MENU`.
    ///
    /// This variant exists for one reason beyond typing: Windows lets the process
    /// that last touched the input stream move the foreground, so a lone Alt
    /// press-and-release is the standard way to become that process. Nothing
    /// chords it, which is what makes it inert in the applications this lane has
    /// been measured against; `focus_target` is its only caller in the crate.
    Alt,
}

impl Key {
    /// Read a key back from the name an agent would type.
    ///
    /// Names are the lowercase variant, because that is what reads naturally in
    /// a tool argument (`"enter"`, `"pageup"`) and what the schema lists. Case
    /// is ignored so `"PageUp"` and `"pageup"` cannot be two different things
    /// that one of them fails as.
    pub fn parse(name: &str) -> Option<Self> {
        let folded = name.to_ascii_lowercase();
        match folded.as_str() {
            "enter" | "return" => Some(Self::Enter),
            "tab" => Some(Self::Tab),
            "escape" | "esc" => Some(Self::Escape),
            "backspace" => Some(Self::Backspace),
            "delete" | "del" => Some(Self::Delete),
            "insert" | "ins" => Some(Self::Insert),
            "home" => Some(Self::Home),
            "end" => Some(Self::End),
            "pageup" | "pgup" => Some(Self::PageUp),
            "pagedown" | "pgdn" => Some(Self::PageDown),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "alt" | "menu" => Some(Self::Alt),
            _ => None,
        }
    }

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
            Self::Alt => 0x12,
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
            Self::Enter | Self::Tab | Self::Escape | Self::Backspace | Self::Alt
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
/// Bit 24 is the extended flag. On a release the previous-key-state bit (30)
/// and the transition bit (31) are set; bit 29 is the context code (ALT held),
/// which a synthetic key press is not, so setting it would misdescribe the
/// message rather than merely being ignored.
pub fn key_lparam(scan: u16, extended: bool, released: bool) -> isize {
    let mut value: u32 = 1 | (u32::from(scan) << 16);
    if extended {
        value |= 1 << 24;
    }
    if released {
        value |= 1 << 30 | 1 << 31;
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

/// Split the `SHORT` from `VkKeyScanW` into a key code and a modifier mask.
///
/// This is the entry to the scan-code route. `KEYEVENTF_UNICODE` reaches an
/// application as a `WM_CHAR` born from a `VK_PACKET` event, and programs that
/// poll keys instead of reading characters (games, launchers, terminals with
/// their own key handling, anything on DirectInput) never see a packet, so a
/// string sent that way is silently swallowed. The fix is not a flag: it is the
/// key that makes the character on the installed layout, plus the modifiers it
/// needs, which is what these two numbers describe.
///
/// `None` for -1, meaning the character is not producible on this keyboard. That
/// is a property of the layout, not a transient failure, so it is reported per
/// character and the caller decides whether to drop it or refuse the string. A
/// caller that masked the low byte without checking the negative case would read
/// `0xFF` and press a key that does not exist.
pub fn decode_vk_scan(raw: i16) -> Option<(u16, u8)> {
    if raw < 0 {
        return None;
    }
    let bits = raw as u16;
    Some((bits & 0x00FF, (bits >> 8) as u8))
}

/// Press and release for one key, with its modifiers held around it.
///
/// `modifiers` pairs each `VKS_*` bit with that modifier's scan code, resolved by
/// the caller from the layout. The ordering rule is the same whatever that
/// answer is, and the ordering is the part worth testing here.
///
/// A modifier released before the key stops being a modifier, so releasing Shift
/// early turns `A` into `a`. The key's own release comes before the modifiers'
/// for the same reason, and the modifiers come back up in reverse so a two-key
/// chord never briefly presents a different chord on the way out.
pub fn key_events_with_modifiers(scan: u16, state: u8, modifiers: &[(u8, u16)]) -> Vec<Input> {
    let mut events = Vec::new();
    let mut held: Vec<u16> = Vec::new();
    for (bit, modifier_scan) in modifiers {
        if state & *bit != 0 {
            events.push(keyboard_input(*modifier_scan, KEYEVENTF_SCANCODE));
            held.push(*modifier_scan);
        }
    }
    events.push(keyboard_input(scan, KEYEVENTF_SCANCODE));
    events.push(keyboard_input(scan, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP));
    for modifier_scan in held.into_iter().rev() {
        events.push(keyboard_input(
            modifier_scan,
            KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP,
        ));
    }
    events
}
