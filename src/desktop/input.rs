//! Synthesised mouse and keyboard input, on Windows.
//!
//! Enumeration says which windows exist and capture says what one looks like.
//! This file is the point where the agent stops reading and starts *acting*, and
//! it is the only part of the module that can affect something the human is
//! currently using. So the surface is built around one question: how much of the
//! machine does this call touch?
//!
//! Two delivery routes, deliberately separate rather than one function with a
//! mode flag:
//!
//! * **Injected** (`SendInput`) puts events into the OS input stream. Whatever
//!   the focused window is, that is what receives them, and a mouse event moves
//!   the human's real cursor. It is the only route that works for applications
//!   that read raw input (consoles in quick-edit mode, games, anything using its
//!   own message pump for input), and it is the intrusive one.
//! * **Posted** (`PostMessageW`) addresses one window handle. The cursor does
//!   not move and keyboard focus is not taken, so a person can keep typing
//!   while the agent drives something else. It is the safe default and the
//!   weaker guarantee: it asks, it does not force.
//!
//! Why the weaker one is the default is worth being blunt about, because the
//! trade-off is not obvious from the caller's seat. An injected click goes to
//! the *focused* window, which is not necessarily the window the caller aimed
//! at: a notification, a modal that appeared a frame earlier, or the terminal
//! the human is typing in absorbs it instead. The agent believes it clicked its
//! target and the human's keystrokes end up in the wrong application. A posted
//! click cannot be misdirected, and when it does not work the caller finds out
//! in the same call (`Err`, or a window that visibly did not change) rather
//! than never. So: post first, inject only when the caller has decided that
//! moving the human's cursor is acceptable, and say so in the function name.
//!
//! What posting cannot do, since it is the limit callers hit: it delivers
//! messages, not input. A hung window's queue accepts them and nothing
//! happens; UIPI refuses them outright when the target runs elevated and the
//! caller does not; DirectComposition and other out-of-process surfaces never
//! see the thread queue at all; and an application that polls device state
//! (`GetAsyncKeyState`, raw input, XInput) ignores a posted message however
//! correctly it was delivered. Those are the cases the injected route exists
//! for, not a reason to prefer it.
//!
//! The shapes and the arithmetic are here rather than in `win32.rs` so that
//! they compile and are tested on every host: a coordinate-mapping or
//! UTF-16-splitting mistake does not announce itself, it just makes the click
//! land somewhere the agent never saw.

/// Bytes in an [`Input`] body: the largest of its members, `MOUSEINPUT`, on
/// x64.
///
/// The body is *pinned* to this size on every target rather than following the
/// largest member per-architecture, which is what makes the x64 layout exact
/// and a 32-bit build diverge: there `INPUT` becomes 36 bytes where
/// `winuser.h` says 28. That divergence cannot turn into a misread gesture,
/// because `SendInput` compares the `cbSize` argument against its own notion of
/// the size and fails the call outright on a mismatch. `a_32bit_build_diverges_
/// from_the_os_and_says_so` asserts the arithmetic so the x64 numbers elsewhere
/// in this file are never mistaken for a claim about that target.
pub const INPUT_BODY_BYTES: usize = 32;

// `MOUSEEVENTF_*`, `winuser.h`.
pub const MOUSEEVENTF_MOVE: u32 = 0x0001;
pub const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
pub const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
pub const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
pub const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;
pub const MOUSEEVENTF_MIDDLEDOWN: u32 = 0x0020;
pub const MOUSEEVENTF_MIDDLEUP: u32 = 0x0040;
pub const MOUSEEVENTF_WHEEL: u32 = 0x0800;
/// Required with `ABSOLUTE`: without it the 0..=65535 range spans the *primary
/// monitor* only, so every coordinate is relative to one screen regardless of
/// where the point actually is.
pub const MOUSEEVENTF_VIRTUALDESK: u32 = 0x4000;
pub const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;

/// `SendInput` event classes (`INPUT_ *`). The tag decides which union member
/// the OS reads, so a wrong value here does not corrupt data: it makes the
/// mouse fields be interpreted as keyboard fields.
pub const INPUT_MOUSE: u32 = 0;
pub const INPUT_KEYBOARD: u32 = 1;

/// Which button. Order is the order `MK_*` flags use, so [`MouseButton::mk_flag`]
/// can be a table rather than a match that has to be kept in sync by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    /// The button-down half of a `SendInput` mouse event.
    pub fn down_flag(self) -> u32 {
        match self {
            Self::Left => MOUSEEVENTF_LEFTDOWN,
            Self::Right => MOUSEEVENTF_RIGHTDOWN,
            Self::Middle => MOUSEEVENTF_MIDDLEDOWN,
        }
    }

    /// The button-up half.
    ///
    /// A down without its up is a drag that never ends: the next window the
    /// human touches receives a mouse-up it did not ask for, which reads as a
    /// spurious click on whatever happens to be under the pointer when the
    /// button is released. So these are paired, and the senders build
    /// down-and-up in one batch for exactly that reason.
    pub fn up_flag(self) -> u32 {
        match self {
            Self::Left => MOUSEEVENTF_LEFTUP,
            Self::Right => MOUSEEVENTF_RIGHTUP,
            Self::Middle => MOUSEEVENTF_MIDDLEUP,
        }
    }

    /// The `MK_*` state bit for `wParam` of a posted mouse message.
    pub fn mk_flag(self) -> usize {
        match self {
            Self::Left => 0x0001,
            Self::Right => 0x0002,
            Self::Middle => 0x0010,
        }
    }

    /// The posted-message id for this button's press.
    pub fn down_message(self) -> u32 {
        match self {
            Self::Left => WM_LBUTTONDOWN,
            Self::Right => WM_RBUTTONDOWN,
            Self::Middle => WM_MBUTTONDOWN,
        }
    }

    /// The posted-message id for this button's release.
    pub fn up_message(self) -> u32 {
        match self {
            Self::Left => WM_LBUTTONUP,
            Self::Right => WM_RBUTTONUP,
            Self::Middle => WM_MBUTTONUP,
        }
    }
}

// Window messages (`WM_*`) and the character message. Posted input is these,
// not the `SendInput` flag bits: two different encoding schemes for the same
// intent, which is the reason the two routes cannot share one code path.
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;
pub const WM_MBUTTONDOWN: u32 = 0x0207;
pub const WM_MBUTTONUP: u32 = 0x0208;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_CHAR: u32 = 0x0102;

/// A point in physical screen pixels: the same coordinate space
/// [`super::Rect`] and `GetWindowRect` use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

/// `POINT`, marshalled by reference into `ScreenToClient` and `GetCursorPos`.
///
/// Distinct from [`ScreenPoint`] rather than one type reused: this one's layout
/// is fixed by the ABI and must not be changed to match convenience, and the
/// field types are `LONG` where the public type is deliberately `i32` on a
/// path the caller controls.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct WinPoint {
    pub x: i32,
    pub y: i32,
}

/// `MOUSEINPUT`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MouseInput {
    pub dx: i32,
    pub dy: i32,
    /// Wheel delta for `MOUSEEVENTF_WHEEL`, otherwise zero.
    pub mouse_data: u32,
    pub flags: u32,
    /// Zero: let the OS stamp the event. A caller-supplied time is a timestamp
    /// the rest of the system has no way to trust.
    pub time: u32,
    pub extra_info: usize,
}

/// `KEYBDINPUT`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyboardInput {
    pub virtual_key: u16,
    /// A scan code with `KEYEVENTF_SCANCODE`, or a UTF-16 unit with
    /// `KEYEVENTF_UNICODE`. Which one it is, the caller cannot tell from the
    /// bytes: only `flags` says. That is the whole reason `flags` is asserted
    /// rather than inferred anywhere these events are checked.
    pub scan: u16,
    pub flags: u32,
    pub time: u32,
    pub extra_info: usize,
}

/// `INPUT`. The `type` tag followed by a member selected by it.
#[repr(C)]
pub struct Input {
    /// `INPUT_ *`. Named `kind` because `type` is a Rust keyword and `r#type`
    /// reads like a mistake in every call site.
    pub kind: u32,
    pub body: InputBody,
}

/// `INPUTUNION`. The four members are laid out so the union is 32 bytes:
/// a pointer-sized integer on x64 pushes `MouseInput` to 32 bytes.
#[repr(C)]
pub union InputBody {
    pub mouse: MouseInput,
    pub keyboard: KeyboardInput,
    /// The 8-byte `HARDWAREINPUT` fits inside the padded body, so it is not a
    /// variant that changes the size and is not declared: `SendInput` rejects
    /// `INPUT_HARDWARE` outright on modern Windows, and nothing here offers it.
    ///
    /// Zero-filled bytes, which every other member can be read out of: all of
    /// them are plain integers with no invalid bit pattern, so an all-zero body
    /// is a valid `MouseInput` and a valid `KeyboardInput` at once. That is what
    /// lets the layout tests inspect offsets without reading uninitialised
    /// memory.
    pub zeroed: [u8; INPUT_BODY_BYTES],
}

impl Input {
    /// An event whose tag is `kind` and whose body is all zeroes.
    pub fn zeroed(kind: u32) -> Self {
        Self {
            kind,
            body: InputBody {
                zeroed: [0; INPUT_BODY_BYTES],
            },
        }
    }

    /// The mouse member, or `None` when the tag does not name one.
    ///
    /// Rust requires `unsafe` to read *any* union field, whether or not the
    /// member is the one that was written, which means the interesting bug is
    /// not the unsafe block, it is reading `mouse` out of a keyboard event and
    /// getting plausible-looking bytes. Both accessors check the tag first, so
    /// that mistake is a `None` instead of a number nobody questions.
    pub fn as_mouse(&self) -> Option<MouseInput> {
        if self.kind == INPUT_MOUSE {
            // SAFETY: `kind` names the active member, so this reads the field
            // that was written. Guarded with `if` rather than `bool::then_some`
            // because `then_some` would evaluate its argument either way, and
            // reading a member the tag does not name is exactly the mistake this
            // accessor exists to make impossible.
            Some(unsafe { self.body.mouse })
        } else {
            None
        }
    }

    /// The keyboard member, or `None` when the tag does not name one.
    pub fn as_keyboard(&self) -> Option<KeyboardInput> {
        if self.kind == INPUT_KEYBOARD {
            // SAFETY: as above, with the other tag.
            Some(unsafe { self.body.keyboard })
        } else {
            None
        }
    }
}

/// The scaled axis `SendInput` expects for an absolute pointer event.
///
/// Absolute coordinates are *normalised*, not pixels: 0 to 65535 across the
/// virtual desktop, which is what the `ABSOLUTE | VIRTUALDESK` flag pair means.
/// Three ways this goes wrong, all silent:
///
/// * dividing by `span` instead of `span - 1` leaves the final pixel of the
///   screen unreachable, so clicking the right edge or the corner of a taskbar
///   stops working on exactly the monitors where it matters;
/// * forgetting the origin breaks every coordinate on a multi-monitor setup
///   where the secondary screen sits to the *left* (negative `x`), and works
///   fine on the one machine the developer tested on;
/// * clamping an out-of-range point sends the click to the screen edge, where
///   some other application's button lives. That is why out-of-range is
///   `None`: a refused click is a bug report, an edge click is a wrong action.
///
/// A span of 1 or 0 has no interior to scale into and returns `None` for every
/// position rather than dividing by zero. A 1-pixel desktop is not a desktop
/// anyone is clicking on.
pub fn normalize_axis(position: i32, origin: i32, span: i32) -> Option<i32> {
    if span < 2 {
        return None;
    }
    let offset = position.checked_sub(origin)?;
    if offset < 0 || offset >= span {
        return None;
    }
    let scaled = i64::from(offset) * 65535 / (i64::from(span) - 1);
    Some(scaled as i32)
}

/// Packs a client coordinate pair into a mouse message's `lParam`: `x` in the
/// low 16 bits, `y` in the high 16, both as *unsigned* halves.
///
/// Masking is what the `MAKELPARAM` macro does, and the negative cases matter:
/// a window dragged partly off the top of the screen legitimately has client
/// coordinates above the origin that are negative, and sign-extending them
/// instead of masking puts the click 65536 pixels away.
pub fn mouse_lparam(x: i32, y: i32) -> isize {
    let low = u32::from(x as u16);
    let high = u32::from(y as u16) << 16;
    (low | high) as i32 as isize
}

// `KEYEVENTF_*`. `wVk` is zero on both the unicode and scan-code paths: the tag
// is entirely in these bits, which is why a `KeyboardInput` cannot be
// interpreted without them.
pub const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
pub const KEYEVENTF_KEYUP: u32 = 0x0002;
pub const KEYEVENTF_UNICODE: u32 = 0x0004;
pub const KEYEVENTF_SCANCODE: u32 = 0x0008;

/// Modifier mask bits reported by `VkKeyScanW`, not event flags.
///
/// They are deliberately separate from `KEYEVENTF_*`: these describe the state
/// the *keyboard* has to be in for a character, and the only way to satisfy one
/// is to press that modifier key as an ordinary event. Reading `VKS_SHIFT` as if
/// it were a flag on the event produces an event that says nothing of the sort
/// and a lowercase letter anyway.
pub const VKS_SHIFT: u8 = 0x01;
pub const VKS_CONTROL: u8 = 0x02;
pub const VKS_ALT: u8 = 0x04;

/// Virtual-key codes of the modifiers themselves, left hand.
///
/// `VkKeyScanW` does not distinguish left from right, and the right Alt (AltGr)
/// needs `KEYEVENTF_EXTENDEDKEY` on top of its scan code, which is a layout
/// detail this module leaves to whatever the OS mapping returns rather than
/// inventing here.
pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12;

/// The UTF-16 units a string becomes on the wire, in order.
///
/// Windows takes UTF-16 code units, not characters, and Rust strings are
/// UTF-8, so a character outside the BMP is *two* units. `encode_utf16` yields
/// them in surrogate order (high first), which is the order the text services
/// expect: sending the low surrogate first produces mojibake rather than an
/// error, and the mojibake is what gets typed into the application.
pub fn char_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// One `KEYBDINPUT` with the tag in `flags` and the payload in `scan`.
pub fn keyboard_input(scan: u16, flags: u32) -> Input {
    Input {
        kind: INPUT_KEYBOARD,
        body: InputBody {
            keyboard: KeyboardInput {
                virtual_key: 0,
                scan,
                flags,
                time: 0,
                extra_info: 0,
            },
        },
    }
}

/// One `MOUSEINPUT`.
pub fn mouse_input(dx: i32, dy: i32, flags: u32) -> Input {
    Input {
        kind: INPUT_MOUSE,
        body: InputBody {
            mouse: MouseInput {
                dx,
                dy,
                mouse_data: 0,
                flags,
                time: 0,
                extra_info: 0,
            },
        },
    }
}

/// What a delivery call can honestly claim.
///
/// The two variants are not "success" and "less success": they are two
/// different statements, and conflating them is how an agent ends up believing
/// it clicked something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// `SendInput` accepted this many events into the input stream. The count
    /// is checked by the sender, so this means the OS took them, not that an
    /// application acted on them.
    Injected { events: usize },
    /// `PostMessageW` queued this many messages. It says nothing at all about
    /// whether the target processed them: a hung window's queue accepts
    /// messages forever, and a window with no handler for `WM_CHAR` discards
    /// them without a trace. Only a later capture can tell, which is why the
    /// CI job photographs the window rather than trusting this value.
    Queued { messages: usize },
}

impl Delivery {
    /// How many events or messages were handed over, whichever route ran.
    pub fn delivered(self) -> usize {
        match self {
            Self::Injected { events } => events,
            Self::Queued { messages } => messages,
        }
    }
}
