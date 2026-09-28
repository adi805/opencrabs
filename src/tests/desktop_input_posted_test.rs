//! Tests for the posted-input half of the desktop lane: message packing, the
//! text encoding, the key table, and what a delivery is allowed to claim.
//!
//! Split from `desktop_input_test` (which covers the ABI layout and the
//! coordinate arithmetic) because the two halves fail differently: a wrong
//! layout is rejected by `SendInput` on every call, while a wrong *message* is
//! accepted, delivered, and quietly does nothing. Both are pure arithmetic over
//! bytes, so both belong in the Linux job that always runs.

use crate::desktop::{
    Delivery, INPUT_KEYBOARD, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    KEYEVENTF_UNICODE, Key, MouseButton, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WinPoint, char_units, key_lparam, keyboard_input, mouse_lparam,
    posted_char_messages, posted_click_messages, posted_key_messages, unicode_events,
};

// The fail-closed test is the only thing here that aims at a screen point, and that
// test does not exist on Windows, so the import must not either.
#[cfg(not(windows))]
use crate::desktop::ScreenPoint;

#[test]
fn a_client_point_is_packed_low_word_first() {
    assert_eq!(mouse_lparam(0, 0), 0);
    assert_eq!(mouse_lparam(10, 20) as u32, 0x0014_000A);
}

#[test]
fn negative_client_coordinates_keep_their_bits() {
    // A window dragged partly off the top of the screen legitimately has client
    // coordinates above the origin that are negative. `MAKELPARAM` masks each
    // half to 16 bits; sign-extending instead puts the click 65536 pixels away.
    assert_eq!(mouse_lparam(-1, -1) as u32, 0xFFFF_FFFF);
    assert_eq!(mouse_lparam(-2, -1) as u32, 0xFFFF_FFFE);
    assert_eq!(mouse_lparam(5, -1) as u32, 0xFFFF_0005);
}

#[test]
fn a_posted_click_is_three_messages_in_order_at_one_point() {
    let messages = posted_click_messages(MouseButton::Left, WinPoint { x: 12, y: 34 });
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].message, WM_MOUSEMOVE);
    assert_eq!(messages[1].message, WM_LBUTTONDOWN);
    assert_eq!(messages[2].message, WM_LBUTTONUP);
    let packed = mouse_lparam(12, 34);
    for message in &messages {
        assert_eq!(message.lparam, packed, "all three aim at one point");
    }
}

#[test]
fn a_release_does_not_claim_the_button_is_still_held() {
    let messages = posted_click_messages(MouseButton::Left, WinPoint { x: 1, y: 1 });
    assert_eq!(messages[0].wparam, 0, "the move carries no button state");
    assert_eq!(messages[1].wparam, 0x0001, "MK_LBUTTON on the press");
    assert_eq!(messages[2].wparam, 0, "no button state on the release");
}

#[test]
fn posted_text_is_one_character_message_per_utf16_unit() {
    let messages = posted_char_messages("aé\u{1F642}");
    assert_eq!(messages.len(), 4, "1 + 1 + two surrogates");
    assert!(
        messages.iter().all(|m| m.message == WM_CHAR),
        "WM_CHAR only: this route never synthesises a shortcut"
    );
    let units: Vec<u16> = messages.iter().map(|m| m.wparam as u16).collect();
    assert_eq!(units, vec![0x61, 0xE9, 0xD83D, 0xDE42]);
    assert!(
        posted_char_messages("").is_empty(),
        "an empty string must be an empty batch, which the sender refuses to post"
    );
}

// ------------------------------------------------------------------- text

#[test]
fn text_becomes_utf16_units() {
    assert_eq!(char_units("a"), vec![0x61]);
    assert_eq!(char_units("é"), vec![0xE9]);
    // A 4-byte UTF-8 character is two UTF-16 units, high surrogate first.
    assert_eq!(char_units("\u{1F642}"), vec![0xD83D, 0xDE42]);
    assert!(char_units("").is_empty());
}

#[test]
fn unicode_events_pair_each_unit_with_its_release() {
    let events = unicode_events("a\u{1F642}");
    assert_eq!(events.len(), 6, "three units, two events each");
    let scans: Vec<u16> = events
        .iter()
        .map(|e| e.as_keyboard().expect("keyboard").scan)
        .collect();
    assert_eq!(scans, vec![0x61, 0x61, 0xD83D, 0xD83D, 0xDE42, 0xDE42]);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.kind, INPUT_KEYBOARD);
        let key = event.as_keyboard().expect("keyboard");
        assert_eq!(
            key.virtual_key, 0,
            "the unicode path puts everything in wScan"
        );
        let expected = if index % 2 == 0 {
            KEYEVENTF_UNICODE
        } else {
            KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
        };
        assert_eq!(
            key.flags, expected,
            "event {index} carries the wrong route tag"
        );
    }
}

#[test]
fn a_key_event_declares_its_route_only_in_its_flags() {
    // A unicode character and a scan code occupy identical bytes, so the flags
    // are the entire difference between "type this" and "press this".
    let text = keyboard_input(0x41, KEYEVENTF_UNICODE);
    let scan = keyboard_input(0x41, KEYEVENTF_SCANCODE);
    assert_eq!(
        text.as_keyboard().expect("k").scan,
        scan.as_keyboard().expect("k").scan
    );
    assert_ne!(
        text.as_keyboard().expect("k").flags,
        scan.as_keyboard().expect("k").flags,
        "identical bytes must not mean two different intents"
    );
}

// ------------------------------------------------------------------- keys

const ALL_KEYS: [Key; 14] = [
    Key::Enter,
    Key::Tab,
    Key::Escape,
    Key::Backspace,
    Key::Delete,
    Key::Insert,
    Key::Home,
    Key::End,
    Key::PageUp,
    Key::PageDown,
    Key::Up,
    Key::Down,
    Key::Left,
    Key::Right,
];

#[test]
fn no_two_named_keys_share_a_virtual_key_code() {
    let codes: Vec<u16> = ALL_KEYS.iter().map(|key| key.vk()).collect();
    let unique: std::collections::HashSet<u16> = codes.iter().copied().collect();
    assert_eq!(
        codes.len(),
        ALL_KEYS.len(),
        "the table grew or shrank quietly"
    );
    assert_eq!(
        unique.len(),
        codes.len(),
        "two names map to one key code, so one of them cannot be pressed: {codes:?}"
    );
}

#[test]
fn the_navigation_cluster_needs_the_extended_bit() {
    // Arrows, Home/End/PgUp/PgDn and Insert/Delete are extended keys. Sent
    // without the flag they read as the numeric keypad's digits, so "press
    // Down" types 2 and nothing reports that it happened.
    for key in ALL_KEYS {
        let expected = !matches!(key, Key::Enter | Key::Tab | Key::Escape | Key::Backspace);
        assert_eq!(key.extended(), expected, "{key:?} extended flag");
    }
}

#[test]
fn key_events_are_a_press_and_a_release_on_the_scan_code_route() {
    let events = Key::Down.events(0x50);
    assert_eq!(events.len(), 2);
    let down = events[0].as_keyboard().expect("keyboard");
    let up = events[1].as_keyboard().expect("keyboard");
    assert_eq!(down.virtual_key, 0);
    assert_eq!(down.scan, 0x50);
    assert_eq!(down.flags, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY);
    assert_eq!(up.flags, down.flags | KEYEVENTF_KEYUP);
    assert_eq!(up.scan, down.scan, "the release names the same key");

    let enter = Key::Enter.events(0x1C);
    assert_eq!(
        enter[0].as_keyboard().expect("keyboard").flags,
        KEYEVENTF_SCANCODE,
        "main Enter is not extended; claiming it is makes some applications submit twice"
    );
}

#[test]
fn the_key_lparam_carries_scan_code_and_state_bits() {
    // repeat count 1 in bits 0..15, scan code in 16..23, extended in 24.
    assert_eq!(key_lparam(0x50, true, false) as u32, 0x0150_0001);
    assert_eq!(key_lparam(0x50, false, false) as u32, 0x0050_0001);
    // A release additionally reports the key as previously down (29) and
    // transitioning up (30).
    assert_eq!(key_lparam(0x50, false, true) as u32, 0x6050_0001);
    assert_eq!(key_lparam(0x1C, true, true) as u32, 0x611C_0001);
}

#[test]
fn posted_key_pair_distinguishes_press_from_release() {
    let messages = posted_key_messages(Key::Enter, 0x1C);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].message, WM_KEYDOWN);
    assert_eq!(messages[1].message, WM_KEYUP);
    assert_eq!(messages[0].wparam, 0x0D, "wParam is the virtual key code");
    assert_eq!(messages[1].wparam, 0x0D);
    assert_eq!(messages[0].lparam as u32, 0x001C_0001);
    assert_eq!(messages[1].lparam as u32, 0x601C_0001);
    assert_ne!(messages[0].lparam, messages[1].lparam);
}

// --------------------------------------------------------------- receipts

#[test]
fn a_delivery_counts_the_units_it_is_a_claim_about() {
    assert_eq!(Delivery::Injected { events: 3 }.delivered(), 3);
    assert_eq!(Delivery::Queued { messages: 2 }.delivered(), 2);
    // The two routes are not interchangeable in a report: three events the OS
    // took is not the same statement as three messages a queue accepted.
    assert_ne!(
        Delivery::Injected { events: 3 },
        Delivery::Queued { messages: 3 }
    );
    assert_eq!(Delivery::Injected { events: 0 }.delivered(), 0);
}

#[cfg(not(windows))]
#[test]
fn the_input_actions_refuse_on_a_host_with_no_backend() {
    let point = ScreenPoint { x: 10, y: 10 };
    let refusals = [
        crate::desktop::click_window(1, MouseButton::Left, point)
            .err()
            .map(|e| e.kind()),
        crate::desktop::type_into_window(1, "hello")
            .err()
            .map(|e| e.kind()),
        crate::desktop::press_key_in_window(1, Key::Enter)
            .err()
            .map(|e| e.kind()),
        crate::desktop::inject_click(MouseButton::Left, point)
            .err()
            .map(|e| e.kind()),
        crate::desktop::inject_text("hello").err().map(|e| e.kind()),
        crate::desktop::inject_key(Key::Enter)
            .err()
            .map(|e| e.kind()),
        crate::desktop::cursor_position().err().map(|e| e.kind()),
        crate::desktop::virtual_desktop().err().map(|e| e.kind()),
    ];
    for (index, kind) in refusals.iter().enumerate() {
        assert_eq!(
            kind,
            &Some(std::io::ErrorKind::Unsupported),
            "action {index} did not fail closed: an input stub that returned Ok \
             would let a caller on Linux believe it had clicked something"
        );
    }
}
