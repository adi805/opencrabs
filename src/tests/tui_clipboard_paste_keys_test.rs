//! #1812: Cmd+V passed through by a kitty-protocol terminal arrives as
//! `Char('v')` + SUPER. It must paste like Ctrl+V, and no SUPER chord may
//! type its letter into the chat input.

use crate::tui::events::keys::{is_clipboard_paste, types_character};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(c: char, m: KeyModifiers) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), m)
}

#[test]
fn ctrl_v_and_cmd_v_both_paste() {
    assert!(is_clipboard_paste(&key('v', KeyModifiers::CONTROL)));
    assert!(is_clipboard_paste(&key('V', KeyModifiers::CONTROL)));
    assert!(is_clipboard_paste(&key('v', KeyModifiers::SUPER)));
    assert!(is_clipboard_paste(&key('V', KeyModifiers::SUPER)));
}

#[test]
fn plain_and_alt_v_do_not_paste() {
    assert!(!is_clipboard_paste(&key('v', KeyModifiers::NONE)));
    assert!(!is_clipboard_paste(&key('V', KeyModifiers::SHIFT)));
    assert!(!is_clipboard_paste(&key('v', KeyModifiers::ALT)));
    assert!(!is_clipboard_paste(&key(
        'v',
        KeyModifiers::CONTROL | KeyModifiers::ALT
    )));
}

#[test]
fn other_letters_with_ctrl_or_cmd_do_not_paste() {
    assert!(!is_clipboard_paste(&key('c', KeyModifiers::SUPER)));
    assert!(!is_clipboard_paste(&key('b', KeyModifiers::CONTROL)));
}

#[test]
fn super_chords_never_type_their_character() {
    assert!(!types_character(KeyModifiers::SUPER));
    assert!(!types_character(KeyModifiers::SUPER | KeyModifiers::SHIFT));
    assert!(!types_character(KeyModifiers::SUPER | KeyModifiers::ALT));
}

#[test]
fn plain_shift_and_alt_characters_still_type() {
    assert!(types_character(KeyModifiers::NONE));
    assert!(types_character(KeyModifiers::SHIFT));
    assert!(types_character(KeyModifiers::ALT));
}

#[test]
fn altgr_reported_as_ctrl_alt_still_types() {
    assert!(types_character(KeyModifiers::CONTROL | KeyModifiers::ALT));
}

#[test]
fn bare_ctrl_chords_do_not_type() {
    assert!(!types_character(KeyModifiers::CONTROL));
    assert!(!types_character(
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    ));
}
